//! Luau hooks: the reading patterns alone can't do, such as a table cut into
//! columns by width (PRD §11, "How plugins are written" item 2).
//!
//! A plugin lists its Luau files under `[hooks]` and what they may do under
//! `permissions`. A Text pattern or table reader names a hook; when it
//! matches, the event goes to that global function, and to nothing else:
//!
//! ```lua
//! function read_row(event)
//!     -- event.name, event.captures.<name>, event.plain, event.raw
//!     mud.store("skills", event.captures.skill, tonumber(event.captures.percent))
//! end
//! ```
//!
//! The `mud` table is the whole API:
//!
//! - `mud.state(key)` reads the caller's [`StateView`] (`read_state`);
//! - `mud.store(store, key, value)` writes a boolean, number or string to a
//!   named store (`write_store:<store>`), as a [`StoreWrite`];
//! - `mud.send(command)` sends a command (`send_commands`), as a
//!   [`SendCommand`] the shell shows in chat like the player's own. It's
//!   rate-limited, and returns `false` when over the limit.
//!
//! Without its permission, each one raises an error. A hook's outputs go out
//! only when it returns: one that fails or overruns sends nothing.
//!
//! **Sandbox.** Each plugin has its own VM in Luau's sandbox mode, with
//! read-only globals and only the `string`, `table`, `math`, `utf8` and
//! `bit32` libraries: no `io`, `os`, `debug`, `require`, `loadstring` or
//! `print`, so no file or network access. Its memory is capped.
//!
//! **Time boxes.** Every hook call gets an instruction budget, counted by
//! Luau's interrupt (about one tick per loop iteration or function call). A
//! hook past its budget, or its memory cap, is stopped; a plugin whose hooks
//! overrun [`OVERRUNS_TO_TURN_OFF`] times in a session is turned off, with a
//! [`PluginDisabled`] notice.
//!
//! **Approval.** A plugin asking for `send_commands` runs no hooks until the
//! caller approves it.
//!
//! [`HookRunner`] runs hooks on the thread that owns it. [`LuauHooks`] is the
//! [`PluginSink`] the line thread uses: it hands each hook event to its own
//! worker thread, which owns a runner.

use super::host::{PluginEvent, PluginHost, PluginSink};
use super::manifest::{Permission, Plugin, PluginError, PluginId, Problem};
use super::points::HookName;
use mlua::{Function, Lua, LuaOptions, StdLib, Table, VmState};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Interrupt ticks a hook call may use. On an M-series Mac this stops an
/// endless loop in about 80 µs (mudling-bench): 5 µs a line amortized over
/// the 20 or so lines that don't reach the hook. Real hooks use far fewer.
pub const DEFAULT_BUDGET: u32 = 10_000;
pub const DEFAULT_COMMANDS_PER_SECOND: u32 = 5;
pub const DEFAULT_MEMORY_LIMIT: usize = 8 * 1024 * 1024;
/// Overruns in one session that turn a plugin off.
pub const OVERRUNS_TO_TURN_OFF: u32 = 3;

/// One of a plugin's `[hooks]` files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Script {
    pub file: String,
    pub source: Arc<str>,
}

/// Checks that a hook file compiles, without running it.
pub(super) fn check_syntax(file: &str, source: &str) -> Result<(), String> {
    Lua::new_with(StdLib::NONE, LuaOptions::default())
        .and_then(|lua| lua.load(source).set_name(format!("={file}")).into_function())
        .map(drop)
        .map_err(|error| format!("the Luau doesn't compile: {error}"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HookLimits {
    /// Interrupt ticks per hook call.
    pub budget: u32,
    /// Commands each plugin may send in any one second.
    pub commands_per_second: u32,
    /// Bytes each plugin's VM may hold.
    pub memory: usize,
}

impl Default for HookLimits {
    fn default() -> HookLimits {
        HookLimits {
            budget: DEFAULT_BUDGET,
            commands_per_second: DEFAULT_COMMANDS_PER_SECOND,
            memory: DEFAULT_MEMORY_LIMIT,
        }
    }
}

/// A value hooks read from state or write to a store.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Number(f64),
    Text(String),
}

/// The state hooks with `read_state` may read, supplied by the caller. Keys
/// are the caller's own, such as `"vitals.hp"`.
pub trait StateView: Send + Sync {
    fn get(&self, key: &str) -> Option<Value>;
}

impl StateView for HashMap<String, Value> {
    fn get(&self, key: &str) -> Option<Value> {
        HashMap::get(self, key).cloned()
    }
}

/// What hooks produce.
#[derive(Clone, Debug, PartialEq)]
pub enum HookOutput {
    StoreWrite(StoreWrite),
    SendCommand(SendCommand),
    /// A hook raised an error or overran; it sent nothing.
    HookFailed(HookFailed),
    PluginDisabled(PluginDisabled),
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreWrite {
    pub plugin: PluginId,
    pub store: String,
    pub key: String,
    pub value: Value,
}

/// A command to send as if the player typed it, and show in chat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SendCommand {
    pub plugin: PluginId,
    pub command: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookFailed {
    pub plugin: PluginId,
    pub hook: HookName,
    /// Stopped by its budget or memory cap, rather than an error of its own.
    pub overran: bool,
    pub message: String,
}

/// A plugin the hooks turned off, for Settings › Plugins.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginDisabled {
    pub plugin: PluginId,
    pub reason: DisabledReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisabledReason {
    /// Its hooks overran this many times this session.
    Overran(u32),
}

/// Where hook outputs go.
pub trait HookSink {
    fn output(&mut self, output: HookOutput);
}

impl HookSink for Vec<HookOutput> {
    fn output(&mut self, output: HookOutput) {
        self.push(output);
    }
}

impl HookSink for Sender<HookOutput> {
    fn output(&mut self, output: HookOutput) {
        // A receiver that has hung up wants no more outputs.
        let _ = self.send(output);
    }
}

/// Runs plugins' hooks on the thread that owns it, one VM per plugin.
pub struct HookRunner {
    limits: HookLimits,
    state: Arc<dyn StateView>,
    vms: Vec<Vm>,
}

struct Vm {
    plugin: PluginId,
    lua: Lua,
    meter: Rc<Meter>,
    outbox: Rc<RefCell<Vec<HookOutput>>>,
    needs_approval: bool,
    approved: bool,
    overruns: u32,
    disabled: bool,
}

/// The budget left in the current call, shared with the interrupt.
struct Meter {
    left: Cell<u32>,
}

impl HookRunner {
    pub fn new(limits: HookLimits, state: Arc<dyn StateView>) -> HookRunner {
        HookRunner { limits, state, vms: Vec::new() }
    }

    /// Loads `plugin`'s hook files into a fresh VM, replacing any it had.
    /// A plugin without hooks needs no VM.
    pub fn add(&mut self, plugin: &Plugin) -> Result<(), PluginError> {
        self.vms.retain(|vm| vm.plugin != *plugin.id());
        if !plugin.scripts.is_empty() {
            self.vms.push(Vm::new(plugin, self.limits, self.state.clone())?);
        }
        Ok(())
    }

    /// Approves or withdraws approval for a plugin that sends commands. It
    /// runs no hooks until approved.
    pub fn set_approved(&mut self, plugin: &PluginId, approved: bool) {
        if let Some(vm) = self.vm(plugin) {
            vm.approved = approved;
        }
    }

    /// Whether `plugin` has hooks that would run now: loaded, approved if
    /// it must be, and not turned off for overrunning.
    pub fn is_active(&self, plugin: &PluginId) -> bool {
        self.vms.iter().any(|vm| vm.plugin == *plugin && vm.is_active())
    }

    /// Runs the hook `event` names, if any, and sends its outputs to `sink`.
    pub fn run(&mut self, event: &PluginEvent, sink: &mut impl HookSink) {
        let Some(hook) = &event.hook else { return };
        let budget = self.limits.budget;
        let Some(vm) = self.vm(&event.plugin).filter(|vm| vm.is_active()) else { return };
        match vm.call(hook, event, budget) {
            Ok(()) => vm.outbox.borrow_mut().drain(..).for_each(|output| sink.output(output)),
            Err((overran, message)) => {
                vm.outbox.borrow_mut().clear();
                let plugin = vm.plugin.clone();
                sink.output(HookOutput::HookFailed(HookFailed {
                    plugin: plugin.clone(),
                    hook: hook.clone(),
                    overran,
                    message,
                }));
                if overran {
                    vm.overruns += 1;
                    if vm.overruns == OVERRUNS_TO_TURN_OFF {
                        vm.disabled = true;
                        let reason = DisabledReason::Overran(vm.overruns);
                        sink.output(HookOutput::PluginDisabled(PluginDisabled { plugin, reason }));
                    }
                }
            }
        }
    }

    fn vm(&mut self, plugin: &PluginId) -> Option<&mut Vm> {
        self.vms.iter_mut().find(|vm| vm.plugin == *plugin)
    }
}

impl Vm {
    fn new(plugin: &Plugin, limits: HookLimits, state: Arc<dyn StateView>) -> Result<Vm, PluginError> {
        let id = plugin.id().clone();
        let meter = Rc::new(Meter { left: Cell::new(0) });
        let outbox = Rc::new(RefCell::new(Vec::new()));
        let invalid = |file: &str, error: mlua::Error| PluginError {
            file: file.to_owned(),
            problem: Problem::Invalid(format!("the Luau hooks couldn't load: {error}")),
        };
        let lua = sandbox(&id, &plugin.manifest.permissions, limits, &meter, &outbox, state)
            .map_err(|error| invalid("plugin.toml", error))?;
        for script in &plugin.scripts {
            meter.left.set(limits.budget);
            lua.load(&*script.source).set_name(format!("={}", script.file)).exec().map_err(|error| invalid(&script.file, error))?;
        }
        Ok(Vm {
            plugin: id,
            lua,
            meter,
            outbox,
            needs_approval: plugin.manifest.permissions.contains(&Permission::SendCommands),
            approved: false,
            overruns: 0,
            disabled: false,
        })
    }

    fn is_active(&self) -> bool {
        !self.disabled && (self.approved || !self.needs_approval)
    }

    /// Calls `hook` on `event` inside the budget. An error says whether the
    /// hook overran, and why it stopped.
    fn call(&self, hook: &HookName, event: &PluginEvent, budget: u32) -> Result<(), (bool, String)> {
        let function: Function = match self.lua.globals().get(hook.as_str()) {
            Ok(mlua::Value::Function(function)) => function,
            _ => return Err((false, format!("the plugin has no function `{}`", hook.as_str()))),
        };
        self.meter.left.set(budget);
        let result = event_table(&self.lua, event).and_then(|table| function.call::<()>(table));
        result.map_err(|error| {
            let overran = self.meter.left.get() == 0 || is_memory_error(&error);
            (overran, error.to_string())
        })
    }
}

/// A sandboxed VM for one plugin, with the `mud` API its permissions allow.
fn sandbox(
    plugin: &PluginId,
    permissions: &[Permission],
    limits: HookLimits,
    meter: &Rc<Meter>,
    outbox: &Rc<RefCell<Vec<HookOutput>>>,
    state: Arc<dyn StateView>,
) -> mlua::Result<Lua> {
    let libs = StdLib::STRING | StdLib::TABLE | StdLib::MATH | StdLib::UTF8 | StdLib::BIT;
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    lua.set_memory_limit(limits.memory)?;
    let globals = lua.globals();
    for name in ["require", "loadstring", "getfenv", "setfenv", "collectgarbage", "print"] {
        globals.raw_set(name, mlua::Nil)?;
    }

    let mud = lua.create_table()?;
    let read_state = permissions.contains(&Permission::ReadState);
    mud.set(
        "state",
        lua.create_function(move |lua, key: String| {
            if !read_state {
                return Err(missing("read_state"));
            }
            state.get(&key).map(|value| to_lua(lua, value)).transpose()
        })?,
    )?;

    let stores: Vec<String> = permissions
        .iter()
        .filter_map(|permission| match permission {
            Permission::WriteStore(store) => Some(store.clone()),
            _ => None,
        })
        .collect();
    let (id, writes) = (plugin.clone(), outbox.clone());
    mud.set(
        "store",
        lua.create_function(move |_, (store, key, value): (String, String, mlua::Value)| {
            if !stores.contains(&store) {
                return Err(missing(&format!("write_store:{store}")));
            }
            let value = from_lua(value)?;
            writes.borrow_mut().push(HookOutput::StoreWrite(StoreWrite { plugin: id.clone(), store, key, value }));
            Ok(())
        })?,
    )?;

    let send_commands = permissions.contains(&Permission::SendCommands);
    let limit = RateLimit::new(limits.commands_per_second);
    let (id, sends) = (plugin.clone(), outbox.clone());
    mud.set(
        "send",
        lua.create_function(move |_, command: String| {
            if !send_commands {
                return Err(missing("send_commands"));
            }
            if !limit.allow(Instant::now()) {
                return Ok(false);
            }
            sends.borrow_mut().push(HookOutput::SendCommand(SendCommand { plugin: id.clone(), command }));
            Ok(true)
        })?,
    )?;
    globals.set("mud", mud)?;

    let meter = meter.clone();
    lua.set_interrupt(move |_| match meter.left.get() {
        // Stays at zero, so a `pcall` that catches this fails at its next tick.
        0 => Err(mlua::Error::runtime("the hook ran past its instruction budget")),
        left => {
            meter.left.set(left - 1);
            Ok(VmState::Continue)
        }
    });
    lua.sandbox(true)?;
    Ok(lua)
}

fn missing(permission: &str) -> mlua::Error {
    mlua::Error::runtime(format!("this plugin didn't ask for the `{permission}` permission"))
}

fn event_table(lua: &Lua, event: &PluginEvent) -> mlua::Result<Table> {
    let captures = lua.create_table_with_capacity(0, event.captures.len())?;
    let plain = event.line.plain();
    for capture in &event.captures {
        captures.raw_set(&*capture.name, &plain[capture.start as usize..capture.end as usize])?;
    }
    let table = lua.create_table_with_capacity(0, 4)?;
    table.raw_set("name", &*event.name)?;
    table.raw_set("captures", captures)?;
    table.raw_set("plain", plain)?;
    table.raw_set("raw", event.line.raw())?;
    Ok(table)
}

fn to_lua(lua: &Lua, value: Value) -> mlua::Result<mlua::Value> {
    Ok(match value {
        Value::Bool(value) => mlua::Value::Boolean(value),
        Value::Number(value) => mlua::Value::Number(value),
        Value::Text(value) => mlua::Value::String(lua.create_string(value)?),
    })
}

fn from_lua(value: mlua::Value) -> mlua::Result<Value> {
    match value {
        mlua::Value::Boolean(value) => Ok(Value::Bool(value)),
        mlua::Value::Integer(value) => Ok(Value::Number(value as f64)),
        mlua::Value::Number(value) => Ok(Value::Number(value)),
        mlua::Value::String(value) => Ok(Value::Text(value.to_str()?.to_owned())),
        other => Err(mlua::Error::runtime(format!("a store holds booleans, numbers and strings, not a {}", other.type_name()))),
    }
}

fn is_memory_error(error: &mlua::Error) -> bool {
    match error {
        mlua::Error::MemoryError(_) => true,
        mlua::Error::CallbackError { cause, .. } => is_memory_error(cause),
        _ => false,
    }
}

/// At most `per_second` in any one second.
struct RateLimit {
    per_second: usize,
    recent: RefCell<VecDeque<Instant>>,
}

impl RateLimit {
    fn new(per_second: u32) -> RateLimit {
        RateLimit { per_second: per_second as usize, recent: RefCell::default() }
    }

    fn allow(&self, now: Instant) -> bool {
        let mut recent = self.recent.borrow_mut();
        while recent.front().is_some_and(|&sent| now.duration_since(sent) >= Duration::from_secs(1)) {
            recent.pop_front();
        }
        let allowed = recent.len() < self.per_second;
        if allowed {
            recent.push_back(now);
        }
        allowed
    }
}

/// The line thread's side of the hooks: a [`PluginSink`] that hands each
/// event naming a hook to a worker thread, which runs it and sends the
/// outputs to the [`HookSink`] given at start.
pub struct LuauHooks {
    jobs: Option<Sender<Job>>,
    turned_off: Receiver<PluginId>,
    worker: Option<JoinHandle<()>>,
}

enum Job {
    Event(PluginEvent),
    Approve(PluginId, bool),
    Flush(SyncSender<()>),
}

impl LuauHooks {
    /// Starts the worker with each of `plugins` that has hooks. Fails, with
    /// the worker stopped, if any plugin's hooks don't load.
    pub fn start<'a>(
        plugins: impl IntoIterator<Item = &'a Plugin>,
        limits: HookLimits,
        state: Arc<dyn StateView>,
        sink: impl HookSink + Send + 'static,
    ) -> Result<LuauHooks, PluginError> {
        let plugins: Vec<Plugin> = plugins.into_iter().filter(|plugin| !plugin.scripts.is_empty()).cloned().collect();
        let (jobs, inbox) = mpsc::channel();
        let (turned_off_tx, turned_off) = mpsc::channel();
        let (loaded_tx, loaded) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("mudling-luau".into())
            .spawn(move || {
                let mut runner = HookRunner::new(limits, state);
                let added = plugins.iter().try_for_each(|plugin| runner.add(plugin));
                let failed = added.is_err();
                let _ = loaded_tx.send(added);
                if !failed {
                    work(runner, inbox, TurnOff { sink, turned_off: turned_off_tx });
                }
            })
            .map_err(|error| PluginError { file: "plugin host".into(), problem: Problem::Invalid(error.to_string()) })?;
        // Dropped on an error, which joins the stopped worker.
        let hooks = LuauHooks { jobs: Some(jobs), turned_off, worker: Some(worker) };
        loaded.recv().expect("the hook worker reports whether the plugins loaded")?;
        Ok(hooks)
    }

    /// See [`HookRunner::set_approved`].
    pub fn set_approved(&mut self, plugin: &PluginId, approved: bool) {
        self.send(Job::Approve(plugin.clone(), approved));
    }

    /// Turns off, in `host`, each plugin the worker turned off for
    /// overrunning since the last call, and returns them. Call it after
    /// [`PluginHost::read_line`].
    pub fn turn_off_overrunning(&mut self, host: &mut PluginHost) -> Result<Vec<PluginId>, PluginError> {
        let turned_off: Vec<PluginId> = self.turned_off.try_iter().collect();
        for plugin in &turned_off {
            host.set_enabled(plugin, false)?;
        }
        Ok(turned_off)
    }

    /// Waits until the worker has run every hook queued so far.
    pub fn flush(&mut self) {
        let (done, finished) = mpsc::sync_channel(1);
        self.send(Job::Flush(done));
        let _ = finished.recv();
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            // The worker only stops when this side drops the channel.
            let _ = jobs.send(job);
        }
    }

    fn stop(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl PluginSink for LuauHooks {
    fn event(&mut self, event: PluginEvent) {
        if event.hook.is_some() {
            self.send(Job::Event(event));
        }
    }
}

impl Drop for LuauHooks {
    fn drop(&mut self) {
        self.stop();
    }
}

fn work(mut runner: HookRunner, inbox: Receiver<Job>, mut sink: TurnOff<impl HookSink>) {
    for job in inbox {
        match job {
            Job::Event(event) => runner.run(&event, &mut sink),
            Job::Approve(plugin, approved) => runner.set_approved(&plugin, approved),
            Job::Flush(done) => {
                let _ = done.send(());
            }
        }
    }
}

/// Passes outputs on, and tells the line thread which plugins to turn off.
struct TurnOff<S> {
    sink: S,
    turned_off: Sender<PluginId>,
}

impl<S: HookSink> HookSink for TurnOff<S> {
    fn output(&mut self, output: HookOutput) {
        if let HookOutput::PluginDisabled(disabled) = &output {
            let _ = self.turned_off.send(disabled.plugin.clone());
        }
        self.sink.output(output);
    }
}
