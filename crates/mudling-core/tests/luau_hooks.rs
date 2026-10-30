//! Luau hooks: the sandbox, the budget, permissions, approval, and a
//! column-splitting hook over a synthetic table.

use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::luau::{
    DisabledReason, HookFailed, HookLimits, HookOutput, HookRunner, LuauHooks, PluginDisabled,
    SendCommand, StateView, StoreWrite, Value,
};
use mudling_core::plugin::{Plugin, PluginError, PluginEvent, PluginHost, PluginId, Problem};
use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

/// The patterns most tests use: `go` runs `on_go`; `stay` names no hook.
const PATTERNS: &str = r#"
[[pattern]]
name = "go"
pattern = '''^go$'''
hook = "on_go"

[[pattern]]
name = "stay"
pattern = '''^stay$'''
"#;

/// A plugin with `permissions`, the patterns above, and `source` as its hooks.
fn hooked(permissions: &[&str], source: &str) -> Plugin {
    try_hooked(permissions, source).unwrap_or_else(|error| panic!("{error}"))
}

fn try_hooked(permissions: &[&str], source: &str) -> Result<Plugin, PluginError> {
    named("hooked", permissions, source)
}

fn named(id: &str, permissions: &[&str], source: &str) -> Result<Plugin, PluginError> {
    let permissions = permissions
        .iter()
        .map(|permission| format!("\"{permission}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let manifest = format!(
        "id = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\napi = 1\nlicense = \"MIT\"\nlayer = \"default\"\n\
         permissions = [{permissions}]\n[points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"hooks.luau\"]\n"
    );
    Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(manifest.clone()),
        "text.toml" => Ok(PATTERNS.to_owned()),
        "hooks.luau" => Ok(source.to_owned()),
        _ => Err(io::Error::new(io::ErrorKind::NotFound, "no such file")),
    })
}

fn columns() -> Plugin {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/columns");
    Plugin::load(&dir).unwrap_or_else(|error| panic!("{error}"))
}

fn id(id: &str) -> PluginId {
    PluginId::try_from(id.to_owned()).unwrap()
}

fn lines(text: &[&str]) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut pipeline = LinePipeline::default();
    for line in text {
        pipeline.push(format!("{line}\n").as_bytes(), &mut lines);
    }
    lines
}

fn no_state() -> Arc<dyn StateView> {
    Arc::new(HashMap::<String, Value>::new())
}

fn runner(plugin: &Plugin) -> HookRunner {
    runner_with(plugin, HookLimits::default(), no_state())
}

fn runner_with(plugin: &Plugin, limits: HookLimits, state: Arc<dyn StateView>) -> HookRunner {
    let mut runner = HookRunner::new(limits, state);
    runner.add(plugin).unwrap_or_else(|error| panic!("{error}"));
    runner
}

/// Each line through a host with `plugin`, then each event through `runner`.
fn run(plugin: &Plugin, runner: &mut HookRunner, text: &[&str]) -> Vec<HookOutput> {
    let mut host = PluginHost::new([plugin.clone()]).unwrap();
    let mut outputs = Vec::new();
    for line in lines(text) {
        let mut events: Vec<PluginEvent> = Vec::new();
        host.read_line(&line, &mut events);
        for event in &events {
            runner.run(event, &mut outputs);
        }
    }
    outputs
}

fn writes(outputs: &[HookOutput]) -> Vec<(String, String, Value)> {
    outputs
        .iter()
        .filter_map(|output| match output {
            HookOutput::StoreWrite(StoreWrite {
                store, key, value, ..
            }) => Some((store.clone(), key.clone(), value.clone())),
            _ => None,
        })
        .collect()
}

fn sends(outputs: &[HookOutput]) -> Vec<&str> {
    outputs
        .iter()
        .filter_map(|output| match output {
            HookOutput::SendCommand(SendCommand { command, .. }) => Some(command.as_str()),
            _ => None,
        })
        .collect()
}

fn failures(outputs: &[HookOutput]) -> Vec<&HookFailed> {
    outputs
        .iter()
        .filter_map(|output| match output {
            HookOutput::HookFailed(failed) => Some(failed),
            _ => None,
        })
        .collect()
}

fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

// ---- Manifests ----

#[test]
fn a_manifest_declares_hook_files_and_permissions() {
    let plugin = columns();
    assert_eq!(plugin.manifest.hooks.files, ["columns.luau"]);
    assert_eq!(plugin.scripts[0].file, "columns.luau");
    assert!(plugin.scripts[0].source.contains("function read_row"));
}

#[test]
fn a_bad_hook_file_or_permission_is_rejected_at_load() {
    let error = try_hooked(&[], "function on_go(event").unwrap_err();
    assert_eq!(error.source, "hooks.luau");
    assert!(error.to_string().contains("doesn't compile"), "{error}");

    let error = try_hooked(&["write_store:"], "").unwrap_err();
    assert!(error.to_string().contains("isn't a permission"), "{error}");
    let error = try_hooked(&["network"], "").unwrap_err();
    assert!(error.to_string().contains("isn't a permission"), "{error}");
}

// ---- Sandbox ----

#[test]
fn the_sandbox_has_no_io_os_require_load_or_debug() {
    let plugin = hooked(
        &["write_store:probe"],
        r#"
        function on_go(event)
            for _, name in ipairs({"io", "os", "require", "load", "loadstring", "dofile", "loadfile", "debug",
                                   "print", "getfenv", "setfenv", "collectgarbage"}) do
                mud.store("probe", name, type(_G[name]))
            end
            mud.store("probe", "string library", type(string.format))
        end
        "#,
    );
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    let found: HashMap<String, Value> = writes(&outputs)
        .into_iter()
        .map(|(_, key, value)| (key, value))
        .collect();
    for name in [
        "io",
        "os",
        "require",
        "load",
        "loadstring",
        "dofile",
        "loadfile",
        "debug",
        "print",
        "getfenv",
        "setfenv",
    ] {
        assert_eq!(found.get(name), Some(&text("nil")), "{name} is reachable");
    }
    assert_eq!(found.get("string library"), Some(&text("function")));
}

#[test]
fn globals_and_libraries_are_read_only() {
    for source in [
        "function on_go(event) string.upper = nil end",
        "function on_go(event) mud.send = nil end",
    ] {
        let plugin = hooked(&[], source);
        let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
        assert!(
            failures(&outputs)[0].message.contains("readonly"),
            "{source}: {outputs:?}"
        );
    }
}

#[test]
fn each_plugin_has_its_own_vm() {
    let source = |value: &str| {
        format!("shared = '{value}'\nfunction on_go(event) mud.store('seen', 'shared', shared) end")
    };
    let plugins = [
        named("first", &["write_store:seen"], &source("first")).unwrap(),
        named("second", &["write_store:seen"], &source("second")).unwrap(),
    ];
    let mut runner = HookRunner::new(HookLimits::default(), no_state());
    let mut host = PluginHost::new(plugins.clone()).unwrap();
    let mut events: Vec<PluginEvent> = Vec::new();
    host.read_line(&lines(&["go"])[0], &mut events);
    let mut outputs = Vec::new();
    for plugin in &plugins {
        runner.add(plugin).unwrap();
    }
    for event in &events {
        runner.run(event, &mut outputs);
    }
    let seen: Vec<Value> = writes(&outputs)
        .into_iter()
        .map(|(_, _, value)| value)
        .collect();
    assert_eq!(seen, [text("first"), text("second")]);
}

// ---- Budget ----

/// Limits where only the given one can stop a hook.
const NO_TIME_LIMIT: Duration = Duration::from_secs(60);

#[test]
fn an_infinite_loop_is_stopped_by_the_budget() {
    let plugin = hooked(&[], "function on_go(event) while true do end end");
    let limits = HookLimits {
        time: NO_TIME_LIMIT,
        ..HookLimits::default()
    };
    let outputs = run(
        &plugin,
        &mut runner_with(&plugin, limits, no_state()),
        &["go"],
    );
    let failed = failures(&outputs);
    assert_eq!(failed.len(), 1);
    assert!(failed[0].overran, "{failed:?}");
    assert!(
        failed[0].message.contains("4000 interrupt ticks"),
        "{failed:?}"
    );
}

#[test]
fn a_hook_with_slow_ticks_is_stopped_by_the_time_limit() {
    let plugin = hooked(
        &[],
        "function on_go(event) for i = 1, 2000 do local s = string.rep('x', 100000) s = s:upper() end end",
    );
    // 2,000 iterations is within the tick budget, so only the time limit
    // can stop it; the message says which limit did.
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    let failed = failures(&outputs);
    assert!(failed[0].overran, "{outputs:?}");
    assert!(failed[0].message.contains("ms of CPU time"), "{failed:?}");
}

/// A state whose first read spins the CPU past the time limit, as a jump in
/// the thread's CPU clock would look; later reads are instant.
#[derive(Default)]
struct OneSpike(Mutex<bool>);

impl StateView for OneSpike {
    fn get(&self, _key: &str) -> Option<Value> {
        let mut spiked = self.0.lock().unwrap();
        if !*spiked {
            *spiked = true;
            let started = Instant::now();
            while started.elapsed() < 2 * HookLimits::default().time {
                std::hint::spin_loop();
            }
        }
        None
    }
}

#[test]
fn one_jump_past_the_time_limit_isnt_an_overrun() {
    let plugin = hooked(
        &["read_state", "write_store:s"],
        "function on_go(event) mud.state('k') for i = 1, 50 do end mud.store('s', 'done', true) end",
    );
    let mut runner = runner_with(
        &plugin,
        HookLimits::default(),
        Arc::new(OneSpike::default()),
    );
    let outputs = run(&plugin, &mut runner, &["go"]);
    assert_eq!(
        writes(&outputs),
        [("s".to_owned(), "done".to_owned(), Value::Bool(true))],
        "{outputs:?}"
    );
}

/// A state whose every read sleeps, as a blocked or preempted thread would.
struct Slow;

impl StateView for Slow {
    fn get(&self, _key: &str) -> Option<Value> {
        thread::sleep(Duration::from_millis(20));
        Some(Value::Bool(true))
    }
}

#[test]
fn time_a_hook_spends_off_the_cpu_isnt_charged() {
    let plugin = hooked(
        &["read_state", "write_store:s"],
        "function on_go(event) for i = 1, 3 do mud.state('k') end mud.store('s', 'done', true) end",
    );
    let started = Instant::now();
    let outputs = run(
        &plugin,
        &mut runner_with(&plugin, HookLimits::default(), Arc::new(Slow)),
        &["go"],
    );
    assert!(
        started.elapsed() >= 30 * HookLimits::default().time,
        "the hook took {:?}",
        started.elapsed()
    );
    assert_eq!(
        writes(&outputs),
        [("s".to_owned(), "done".to_owned(), Value::Bool(true))],
        "{outputs:?}"
    );
}

#[test]
fn a_pcall_cant_catch_the_budget() {
    let plugin = hooked(
        &["write_store:probe"],
        "function on_go(event)\n\
         while true do pcall(function() while true do end end) end\n\
         end",
    );
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    assert!(failures(&outputs)[0].overran, "{outputs:?}");
}

#[test]
fn the_memory_cap_counts_as_an_overrun() {
    let plugin = hooked(
        &[],
        "function on_go(event) local t = {} for i = 1, 1e7 do t[i] = string.rep('x', 64) .. i end end",
    );
    let limits = HookLimits {
        budget: u32::MAX,
        time: NO_TIME_LIMIT,
        memory: 1024 * 1024,
        ..HookLimits::default()
    };
    let outputs = run(
        &plugin,
        &mut runner_with(&plugin, limits, no_state()),
        &["go"],
    );
    let failed = failures(&outputs);
    assert!(
        failed[0].overran && failed[0].message.contains("memory"),
        "{failed:?}"
    );
}

#[test]
fn a_failing_hook_emits_nothing() {
    let plugin = hooked(
        &["write_store:s"],
        "function on_go(event) mud.store('s', 'k', 1) error('boom') end",
    );
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    assert!(writes(&outputs).is_empty());
    let failed = failures(&outputs);
    assert!(
        !failed[0].overran && failed[0].message.contains("boom"),
        "{failed:?}"
    );
}

#[test]
fn three_overruns_turn_the_plugin_off_and_say_why() {
    let plugin = hooked(&[], "function on_go(event) while true do end end");
    let mut runner = runner(&plugin);
    let outputs = run(&plugin, &mut runner, &["go", "go"]);
    assert!(runner.is_active(plugin.id()));
    assert!(
        !outputs
            .iter()
            .any(|output| matches!(output, HookOutput::PluginDisabled(_)))
    );

    let outputs = run(&plugin, &mut runner, &["go"]);
    assert_eq!(
        outputs.last(),
        Some(&HookOutput::PluginDisabled(PluginDisabled {
            plugin: id("hooked"),
            reason: DisabledReason::Overran(3)
        }))
    );
    assert!(!runner.is_active(plugin.id()));
    assert!(
        run(&plugin, &mut runner, &["go"]).is_empty(),
        "a turned-off plugin's hooks don't run"
    );
}

#[test]
fn the_worker_turns_an_overrunning_plugin_off_in_the_host() {
    let plugin = hooked(&[], "function on_go(event) while true do end end");
    let mut host = PluginHost::new([plugin.clone()]).unwrap();
    let (outputs, received) = mpsc::channel();
    let mut hooks =
        LuauHooks::start([&plugin], HookLimits::default(), no_state(), outputs).unwrap();
    for line in lines(&["go", "go", "go"]) {
        host.read_line(&line, &mut hooks);
    }
    hooks.wait_until_idle();
    assert_eq!(
        hooks.turn_off_overrunning(&mut host),
        [(id("hooked"), Ok(()))]
    );
    assert_eq!(host.is_enabled(&id("hooked")), Some(false));

    let received: Vec<HookOutput> = received.try_iter().collect();
    assert_eq!(failures(&received).len(), 3);
    assert!(matches!(
        received.last(),
        Some(HookOutput::PluginDisabled(_))
    ));
}

// ---- Permissions ----

#[test]
fn without_send_commands_a_send_errors_and_nothing_is_sent() {
    let plugin = hooked(
        &["write_store:s"],
        "function on_go(event) mud.send('north') end",
    );
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    assert!(sends(&outputs).is_empty());
    assert!(
        failures(&outputs)[0]
            .message
            .contains("`send_commands` permission"),
        "{outputs:?}"
    );
}

#[test]
fn a_store_write_needs_that_stores_permission() {
    let plugin = hooked(
        &["write_store:mine"],
        "function on_go(event) mud.store('mine', 'k', true) mud.store('theirs', 'k', 1) end",
    );
    let outputs = run(&plugin, &mut runner(&plugin), &["go"]);
    assert!(writes(&outputs).is_empty());
    assert!(
        failures(&outputs)[0]
            .message
            .contains("`write_store:theirs` permission"),
        "{outputs:?}"
    );
}

#[test]
fn state_is_read_only_with_read_state() {
    let state: Arc<dyn StateView> = Arc::new(HashMap::from([(
        "vitals.hp".to_owned(),
        Value::Number(42.0),
    )]));
    let source = "function on_go(event) mud.store('s', 'hp', mud.state('vitals.hp')) end";

    let allowed = hooked(&["read_state", "write_store:s"], source);
    let outputs = run(
        &allowed,
        &mut runner_with(&allowed, HookLimits::default(), state.clone()),
        &["go"],
    );
    assert_eq!(
        writes(&outputs),
        [("s".to_owned(), "hp".to_owned(), Value::Number(42.0))]
    );

    let denied = hooked(&["write_store:s"], source);
    let outputs = run(
        &denied,
        &mut runner_with(&denied, HookLimits::default(), state),
        &["go"],
    );
    assert!(
        failures(&outputs)[0]
            .message
            .contains("`read_state` permission"),
        "{outputs:?}"
    );
}

#[test]
fn a_failed_hooks_sends_are_refunded_to_the_rate_limit() {
    let plugin = hooked(
        &["send_commands", "write_store:s"],
        "calls = 0\n\
         function on_go(event)\n\
         calls += 1\n\
         local sent = 0\n\
         for i = 1, 5 do if mud.send('look ' .. i) then sent += 1 end end\n\
         if calls == 1 then error('stop') end\n\
         mud.store('s', 'sent', sent)\n\
         end",
    );
    let limits = HookLimits {
        commands_per_second: 5,
        ..HookLimits::default()
    };
    let mut runner = runner_with(&plugin, limits, no_state());
    runner.set_approved(plugin.id(), true);
    let outputs = run(&plugin, &mut runner, &["go", "go"]);
    assert_eq!(sends(&outputs).len(), 5, "{outputs:?}");
    assert_eq!(
        writes(&outputs),
        [("s".to_owned(), "sent".to_owned(), Value::Number(5.0))]
    );
}

#[test]
fn a_vm_that_cant_be_set_up_says_so() {
    let plugin = hooked(&[], "function on_go(event) end");
    let limits = HookLimits {
        memory: 1,
        ..HookLimits::default()
    };
    let error = HookRunner::new(limits, no_state())
        .add(&plugin)
        .unwrap_err();
    assert!(matches!(error.problem, Problem::LuauSetup(_)), "{error:?}");
    assert_eq!(error.source, "hooked");
}

#[test]
fn sending_is_rate_limited() {
    let plugin = hooked(
        &["send_commands", "write_store:s"],
        "function on_go(event)\n\
         local refused = 0\n\
         for i = 1, 10 do if not mud.send('look ' .. i) then refused += 1 end end\n\
         mud.store('s', 'refused', refused)\n\
         end",
    );
    let limits = HookLimits {
        commands_per_second: 5,
        ..HookLimits::default()
    };
    let mut runner = runner_with(&plugin, limits, no_state());
    runner.set_approved(plugin.id(), true);
    let outputs = run(&plugin, &mut runner, &["go"]);
    assert_eq!(
        sends(&outputs),
        ["look 1", "look 2", "look 3", "look 4", "look 5"]
    );
    assert_eq!(
        writes(&outputs),
        [("s".to_owned(), "refused".to_owned(), Value::Number(5.0))]
    );
    assert!(
        sends(&run(&plugin, &mut runner, &["go"])).is_empty(),
        "still within the same second"
    );
}

#[test]
fn an_unapproved_plugins_sending_hook_doesnt_run() {
    let plugin = hooked(
        &["send_commands"],
        "function on_go(event) mud.send('north') end",
    );
    let mut runner = runner(&plugin);
    assert!(!runner.is_active(plugin.id()));
    assert!(run(&plugin, &mut runner, &["go"]).is_empty());

    runner.set_approved(plugin.id(), true);
    assert_eq!(sends(&run(&plugin, &mut runner, &["go"])), ["north"]);
}

// ---- When hooks run ----

#[test]
fn a_hook_runs_only_when_its_pattern_matched() {
    let plugin = hooked(
        &["write_store:calls"],
        "function on_go(event) mud.store('calls', event.name, event.plain) end",
    );
    let outputs = run(
        &plugin,
        &mut runner(&plugin),
        &["stay", "going", "go", "nothing"],
    );
    assert_eq!(
        writes(&outputs),
        [("calls".to_owned(), "go".to_owned(), text("go"))]
    );
}

/// Records the thread each read comes from.
#[derive(Default)]
struct Threads(Mutex<Vec<ThreadId>>);

impl StateView for Threads {
    fn get(&self, _key: &str) -> Option<Value> {
        self.0.lock().unwrap().push(thread::current().id());
        None
    }
}

#[test]
fn hooks_run_off_the_calling_thread() {
    let plugin = hooked(
        &["read_state", "write_store:s"],
        "function on_go(event) mud.state('any') mud.store('s', 'raw', event.raw) mud.store('s', 'plain', event.plain) end",
    );
    let threads = Arc::new(Threads::default());
    let mut host = PluginHost::new([plugin.clone()]).unwrap();
    let (outputs, received) = mpsc::channel();
    let mut hooks =
        LuauHooks::start([&plugin], HookLimits::default(), threads.clone(), outputs).unwrap();
    for line in lines(&["\x1b[1mgo\x1b[0m", "stay"]) {
        host.read_line(&line, &mut hooks);
    }
    hooks.wait_until_idle();
    let received: Vec<HookOutput> = received.try_iter().collect();
    assert_eq!(
        writes(&received),
        [
            ("s".to_owned(), "raw".to_owned(), text("\x1b[1mgo\x1b[0m")),
            ("s".to_owned(), "plain".to_owned(), text("go"))
        ]
    );
    let threads = threads.0.lock().unwrap();
    assert_eq!(threads.len(), 1);
    assert_ne!(threads[0], thread::current().id());
}

#[test]
fn a_plugin_whose_hooks_dont_load_is_left_out_and_the_rest_run() {
    let broken = named("broken", &[], "while true do end").unwrap();
    let working = named(
        "working",
        &["write_store:s"],
        "function on_go(event) mud.store('s', 'ran', true) end",
    )
    .unwrap();
    let mut host = PluginHost::new([broken.clone(), working.clone()]).unwrap();
    let (outputs, received) = mpsc::channel();
    let mut hooks = LuauHooks::start(
        [&broken, &working],
        HookLimits::default(),
        no_state(),
        outputs,
    )
    .unwrap();

    let [(plugin, error)] = hooks.failed_to_load() else {
        panic!("{:?}", hooks.failed_to_load())
    };
    assert_eq!(plugin, &id("broken"));
    assert_eq!(error.source, "hooks.luau");
    assert!(error.to_string().contains("time box"), "{error}");

    host.read_line(&lines(&["go"])[0], &mut hooks);
    hooks.wait_until_idle();
    let received: Vec<HookOutput> = received.try_iter().collect();
    assert_eq!(
        writes(&received),
        [("s".to_owned(), "ran".to_owned(), Value::Bool(true))],
        "{received:?}"
    );
}

#[test]
fn every_hook_a_pattern_names_must_be_defined() {
    let plugin = hooked(&[], "function on_other(event) end");
    let error = HookRunner::new(HookLimits::default(), no_state())
        .add(&plugin)
        .unwrap_err();
    assert_eq!(error.source, "plugin.toml");
    assert!(
        error.to_string().contains(
            "`hook = \"on_go\"` names a function the hook files (hooks.luau) don't define"
        ),
        "{error}"
    );

    let hooks = LuauHooks::start([&plugin], HookLimits::default(), no_state(), Vec::new()).unwrap();
    assert!(
        hooks.failed_to_load()[0].1.to_string().contains("on_go"),
        "{:?}",
        hooks.failed_to_load()
    );
}

/// About 3,200 lines, the size of a real game's hooks. That compiling isn't
/// charged to the time box is proven, without timing, in `luau.rs`'s tests.
#[test]
fn a_large_hook_file_loads_under_default_limits() {
    let mut source = String::from("function on_go(event) end\n");
    for function in 0..400 {
        source.push_str(&format!(
            "function helper_{function}(row)\n\
             local cells = {{}}\n\
             for start = 1, #row, 22 do\n\
             local name, percent = string.match(string.sub(row, start, start + 21), '^%s*(.-)%s+(%d+)%%%s*$')\n\
             if percent then cells[name] = tonumber(percent) end\n\
             end\n\
             return cells\n\
             end\n"
        ));
    }
    let plugin = hooked(&[], &source);
    let mut runner = runner(&plugin);
    assert!(run(&plugin, &mut runner, &["go"]).is_empty());
}

#[test]
fn a_plugin_naming_hooks_without_hook_files_is_rejected() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/testland");
    let plugin = Plugin::load(&dir).unwrap();
    let error = HookRunner::new(HookLimits::default(), no_state())
        .add(&plugin)
        .unwrap_err();
    assert!(error.to_string().contains("names a function"), "{error}");
}

/// A state whose reads block until the test lets them go, and say when
/// one has begun.
struct Gate {
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl StateView for Gate {
    fn get(&self, _key: &str) -> Option<Value> {
        let _ = self.entered.lock().unwrap().send(());
        let _ = self.release.lock().unwrap().recv();
        None
    }
}

#[test]
fn a_full_queue_drops_new_events_without_waiting_and_counts_them() {
    let plugin = hooked(
        &["read_state", "write_store:s"],
        "function on_go(event) mud.state('k') mud.store('s', 'ran', true) end",
    );
    let (entered_tx, entered) = mpsc::channel();
    let (release, release_rx) = mpsc::channel();
    let gate = Arc::new(Gate {
        entered: Mutex::new(entered_tx),
        release: Mutex::new(release_rx),
    });
    let (outputs, received) = mpsc::channel();
    let limits = HookLimits {
        queue: 2,
        ..HookLimits::default()
    };
    let mut hooks = LuauHooks::start([&plugin], limits, gate, outputs).unwrap();
    let mut host = PluginHost::new([plugin.clone()]).unwrap();
    let go = lines(&["go"]).remove(0);

    host.read_line(&go, &mut hooks);
    entered.recv().unwrap();
    let started = Instant::now();
    for _ in 0..9 {
        host.read_line(&go, &mut hooks);
    }
    hooks.set_approved(plugin.id(), true);
    assert!(
        started.elapsed() < Duration::from_millis(100),
        "the line thread waited"
    );
    assert_eq!(hooks.dropped().get(plugin.id()), Some(&7));

    for _ in 0..3 {
        release.send(()).unwrap();
    }
    hooks.wait_until_idle();
    assert_eq!(writes(&received.try_iter().collect::<Vec<_>>()).len(), 3);
}

// ---- The manifest doc's example ----

#[test]
fn the_manifest_doc_example_loads_as_written() {
    let source = include_str!("../src/plugin/manifest.rs");
    let example: String = source
        .lines()
        .skip_while(|line| *line != "//! ```toml")
        .skip(1)
        .take_while(|line| *line != "//! ```")
        .map(|line| {
            format!(
                "{}\n",
                line.trim_start_matches("//!")
                    .strip_prefix(' ')
                    .unwrap_or("")
            )
        })
        .collect();
    let plugin = Plugin::from_files(|file| match file {
        "plugin.toml" => Ok(example.clone()),
        _ => Ok(String::new()),
    })
    .unwrap_or_else(|error| panic!("{error}\n{example}"));
    assert_eq!(plugin.manifest.permissions.len(), 3);
    assert_eq!(plugin.manifest.hooks.files, ["skills.luau"]);
}

// ---- A column-splitting hook ----

fn cell(name: &str, percent: u32) -> String {
    format!("  {name:<15} {percent:>3}%")
}

#[test]
fn a_column_splitting_hook_reads_a_wrapped_table() {
    let plugin = columns();
    let table = [
        "Your abilities:".to_owned(),
        [cell("climb", 75), cell("swim", 60), cell("fishing", 12)].concat(),
        [cell("weaving", 5), cell("cooking", 100)].concat(),
        "  rope bridge building".to_owned(),
        [cell("", 40), cell("map reading", 88)].concat(),
        "You have 3 lessons left.".to_owned(),
    ];
    let table: Vec<&str> = table.iter().map(String::as_str).collect();
    let outputs = run(&plugin, &mut runner(&plugin), &table);

    let abilities: Vec<(String, Value)> = writes(&outputs)
        .into_iter()
        .map(|(store, key, value)| {
            assert_eq!(store, "abilities");
            (key, value)
        })
        .collect();
    let expected = [
        ("climb", 75.0),
        ("swim", 60.0),
        ("fishing", 12.0),
        ("weaving", 5.0),
        ("cooking", 100.0),
        ("rope bridge building", 40.0),
        ("map reading", 88.0),
    ]
    .map(|(name, percent)| (name.to_owned(), Value::Number(percent)));
    assert_eq!(abilities, expected, "{outputs:?}");
}
