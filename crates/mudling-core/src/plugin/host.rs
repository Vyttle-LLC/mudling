//! The plugin host: runs the enabled plugins over each line in one pass.
//!
//! It lives on the core's line thread, next to the line pipeline. For each
//! finished [`Line`], call [`PluginHost::read_line`]; for text the server
//! hasn't ended yet ([`crate::line::LinePipeline::pending`]), call
//! [`PluginHost::read_pending`] to learn whether it's a prompt or a pager
//! prompt.
//!
//! **Tables.** A plugin reads one [`TableReader`] table at a time. A start
//! pattern opens it; row patterns run only then, in a matcher of their own,
//! so they cost lines outside a table nothing. It ends at its `end` pattern,
//! the plugin's next table or any prompt: the Text point's shapes end it
//! here, and the caller reports the rest, with [`PluginHost::end_tables`] for
//! a prompt it recognized and [`PluginHost::end_tables_at_signal`] for a line
//! the server ended with GA or EOR. Report a pager the client answers with
//! [`PluginHost::pager_answered`]: a table that ends with its pager
//! unanswered ends in `<reader>.cut`, not `<reader>.end`.
//!
//! **Run order** is layer order, defaults first, then game plugins, then
//! overrides; within a layer, the order plugins were added. Where many
//! values apply (patterns, coloring, readers), all run in that order, so a
//! higher layer's event comes later and its coloring paints over a lower
//! one's. Where only one value can apply ([`Settings`]), the last plugin in
//! run order that sets it wins: the highest layer.
//!
//! The matchers are compiled when the set of enabled plugins changes, and
//! only then.
//!
//! **The player's triggers** join the line rules, last in run order, so they
//! cost no pass of their own. Setting them rebuilds the line matcher alone,
//! which leaves open tables as they are. A line's matches are read back from
//! [`PluginHost::triggered`].

use super::manifest::{Plugin, PluginError, PluginId, Problem};
use super::matcher::{DfaCache, Matcher};
use super::points::{
    Capture, HookName, LinkRule, MapSignal, Pager, Pattern, Point, Points, RoomIdentity,
    TableReader,
};
use crate::ansi::{Span, Style};
use crate::line::{Encoding, Line, Link, Pending};
use crate::stats::Summary;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// By default, every 256th line also times each plugin on its own, running
/// the rules the prefilter lets through one by one: often enough to fill
/// Settings › Plugins, too rarely to move the p99.
pub const DEFAULT_COST_SAMPLING: u64 = 256;
/// The samples each plugin keeps for its cost summary.
const COST_SAMPLES_KEPT: usize = 1024;

/// A pattern's match, delivered to a [`PluginSink`].
#[derive(Clone, Debug, PartialEq)]
pub struct PluginEvent {
    pub plugin: PluginId,
    pub point: Point,
    pub name: Arc<str>,
    pub captures: Box<[Capture]>,
    /// The line the pattern matched; captures are ranges of its plain text.
    pub line: Line,
    /// The Luau function the pattern names. Delivery to the sink is the call
    /// point: the Luau host (VYT-197) is a sink that runs it on this event,
    /// off this thread, inside its time box.
    pub hook: Option<HookName>,
    /// What the rule tells the automap, if it's one of the lists that do.
    pub map_signal: Option<MapSignal>,
}

impl PluginEvent {
    /// The text of the named capture, if it took part in the match.
    pub fn capture(&self, name: &str) -> Option<&str> {
        let capture = self
            .captures
            .iter()
            .find(|capture| &*capture.name == name)?;
        Some(capture.text(self.line.plain()))
    }
}

/// Where plugin events go. Stores, the inspector and the Luau host each
/// implement it.
pub trait PluginSink {
    fn event(&mut self, event: PluginEvent);
}

impl PluginSink for Vec<PluginEvent> {
    fn event(&mut self, event: PluginEvent) {
        self.push(event);
    }
}

/// What text the server hasn't ended turned out to be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PendingKind {
    /// A prompt: end the line with `LinePipeline::prompt_end`.
    Prompt,
    /// The pager's prompt; sending `key` continues. Whether to send it is
    /// the caller's choice (the defaults answer it while reading a table).
    Pager { key: Arc<str> },
}

/// The values only one plugin can set, as the highest layer set them.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub encoding: Option<Encoding>,
    pub separator: Option<String>,
    pub room_identity: Option<RoomIdentity>,
    pub pager: Option<Pager>,
    pub area_name: Option<Pattern>,
}

impl Settings {
    /// Whether `text` is the pager's prompt.
    pub fn is_pager(&self, text: &str) -> bool {
        let pager = self.pager.as_ref();
        pager.is_some_and(|pager| pager.prompt.is_match(text))
    }
}

/// One plugin's cost per line, from sampled lines.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginCost {
    pub plugin: PluginId,
    pub sampled_lines: u64,
    /// Over the most recent samples; `None` before the first.
    pub per_line: Option<Summary>,
}

pub struct PluginHost {
    /// In the order added.
    installed: Vec<Installed>,
    compiled: Compiled,
    generation: u64,
    cost_sampling: u64,
    lines_read: u64,
    /// The pending text last read, and what it was.
    last_pending: Option<(Box<str>, Option<PendingKind>)>,
    /// At most one table per plugin.
    open: Vec<OpenTable>,
    /// Tables a recompile cut, for the next sink.
    cuts: Vec<PluginEvent>,
    matched: Vec<usize>,
    matched_rows: Vec<usize>,
    /// The player's patterns, as last set.
    triggers: Vec<Pattern>,
    /// Which of them the last line read matched, in order.
    triggered: Vec<usize>,
}

struct OpenTable {
    /// Into [`Compiled::tables`].
    table: usize,
    /// The line that started it.
    first_line: Line,
    has_rows: bool,
    /// The last line was no row, and the hook has heard.
    in_gap: bool,
    /// A pager prompt is waiting for an answer.
    paged: bool,
    /// The pending line of the last pager prompt, counted once.
    pager_line: Option<u64>,
}

impl OpenTable {
    /// How the table ends if it ends now.
    fn ending(&self) -> TableEvent {
        if self.paged {
            TableEvent::Cut
        } else {
            TableEvent::End
        }
    }
}

struct Installed {
    plugin: Plugin,
    enabled: bool,
    cost: CostLog,
}

impl PluginHost {
    /// A host with `plugins`, all enabled, compiled once.
    pub fn new(plugins: impl IntoIterator<Item = Plugin>) -> Result<PluginHost, PluginError> {
        let mut installed: Vec<Installed> = Vec::new();
        for plugin in plugins {
            if installed
                .iter()
                .any(|other| other.plugin.id() == plugin.id())
            {
                return Err(duplicate(plugin.id()));
            }
            installed.push(Installed {
                plugin,
                enabled: true,
                cost: CostLog::default(),
            });
        }
        let compiled = Compiled::build(&installed, &[])?;
        Ok(PluginHost {
            installed,
            compiled,
            generation: 1,
            cost_sampling: DEFAULT_COST_SAMPLING,
            lines_read: 0,
            last_pending: None,
            open: Vec::new(),
            cuts: Vec::new(),
            matched: Vec::new(),
            matched_rows: Vec::new(),
            triggers: Vec::new(),
            triggered: Vec::new(),
        })
    }

    /// Replaces the player's triggers. On an error the old ones stay.
    pub fn set_triggers(&mut self, patterns: Vec<Pattern>) -> Result<(), PluginError> {
        let lines = &self.compiled.lines;
        let plugin_rules = self.compiled.plugin_rules.last();
        let first_trigger = plugin_rules.map_or(0, |rules| rules.end);
        let rules = (lines.rules[..first_trigger].iter().cloned())
            .chain(trigger_rules(&patterns))
            .collect();
        self.compiled.lines = line_matcher(rules)?;
        self.triggers = patterns;
        Ok(())
    }

    /// Which triggers the last line read matched, as indexes into what was
    /// given to [`PluginHost::set_triggers`], in order.
    pub fn triggered(&self) -> &[usize] {
        &self.triggered
    }

    /// Times each plugin on its own every `every` lines; 0 turns it off.
    pub fn set_cost_sampling(&mut self, every: u64) {
        self.cost_sampling = every;
    }

    /// Adds `plugin`, enabled, and recompiles.
    pub fn add(&mut self, plugin: Plugin) -> Result<(), PluginError> {
        if self.position(plugin.id()).is_some() {
            return Err(duplicate(plugin.id()));
        }
        self.installed.push(Installed {
            plugin,
            enabled: true,
            cost: CostLog::default(),
        });
        self.recompile().inspect_err(|_| {
            self.installed.pop();
        })
    }

    /// Removes the plugin and recompiles. Returns `None` for an unknown id.
    pub fn remove(&mut self, id: &PluginId) -> Result<Option<Plugin>, PluginError> {
        let Some(index) = self.position(id) else {
            return Ok(None);
        };
        let removed = self.installed.remove(index);
        match self.recompile() {
            Ok(()) => Ok(Some(removed.plugin)),
            Err(error) => {
                self.installed.insert(index, removed);
                Err(error)
            }
        }
    }

    /// Turns a plugin on or off. Returns whether anything changed: the
    /// matchers are recompiled only then. An unknown id changes nothing.
    pub fn set_enabled(&mut self, id: &PluginId, enabled: bool) -> Result<bool, PluginError> {
        let Some(index) = self.position(id) else {
            return Ok(false);
        };
        if self.installed[index].enabled == enabled {
            return Ok(false);
        }
        self.installed[index].enabled = enabled;
        self.recompile().map(|()| true).inspect_err(|_| {
            self.installed[index].enabled = !enabled;
        })
    }

    pub fn is_enabled(&self, id: &PluginId) -> Option<bool> {
        self.position(id).map(|index| self.installed[index].enabled)
    }

    /// Every installed plugin, in the order added, and whether it's on.
    pub fn plugins(&self) -> impl Iterator<Item = (&Plugin, bool)> {
        self.installed
            .iter()
            .map(|entry| (&entry.plugin, entry.enabled))
    }

    /// The enabled plugins, in run order.
    pub fn enabled(&self) -> impl Iterator<Item = &Plugin> {
        self.compiled
            .order
            .iter()
            .map(|&index| &self.installed[index].plugin)
    }

    /// Each enabled plugin's declaration for one extension point, in run
    /// order: `host.declared(|points| points.gear.as_ref())`.
    pub fn declared<'a, T: 'a>(
        &'a self,
        point: impl Fn(&'a Points) -> Option<&'a T> + 'a,
    ) -> impl Iterator<Item = (&'a PluginId, &'a T)> + 'a {
        self.enabled().filter_map(move |plugin| {
            point(&plugin.points).map(|declaration| (plugin.id(), declaration))
        })
    }

    pub fn settings(&self) -> &Settings {
        &self.compiled.settings
    }

    /// Counts compilations, so callers can tell when the matchers changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Runs every enabled plugin's line rules over `line` in one pass, and
    /// with a table open, one more for its rows, sending each match to
    /// `sink` in run order. Returns the coloring for a line the server sent
    /// without styles (empty otherwise, or when no rule matched), as
    /// non-overlapping spans of its plain text.
    pub fn read_line(&mut self, line: &Line, sink: &mut impl PluginSink) -> Vec<Span> {
        self.read_line_from(line, 0, sink)
    }

    /// [`PluginHost::read_line`], over the plain text from byte `start` on:
    /// what follows a prompt the caller recognized. Captures and coloring
    /// are still ranges of the whole line.
    pub fn read_line_from(
        &mut self,
        line: &Line,
        start: usize,
        sink: &mut impl PluginSink,
    ) -> Vec<Span> {
        self.cuts.drain(..).for_each(|cut| sink.event(cut));
        self.triggered.clear();
        let text = &line.plain()[start..];
        self.compiled
            .lines
            .matcher
            .matching(text, &mut self.matched);
        let lines = &self.compiled.lines;
        let at_prompt = !self.open.is_empty()
            && self
                .matched
                .iter()
                .any(|&id| lines.rules[id].starts_with_prompt(text));
        if at_prompt {
            self.end_tables(line, sink);
        }
        self.read_tables(line, start, sink);

        let lines = &self.compiled.lines;
        let mut coloring = Vec::new();
        for &id in &self.matched {
            let rule = &lines.rules[id];
            match &rule.action {
                Action::Event {
                    plugin,
                    point,
                    name,
                    hook,
                    map_signal,
                } => {
                    let Some(captures) = rule.pattern.captures(text) else {
                        continue;
                    };
                    sink.event(PluginEvent {
                        plugin: plugin.clone(),
                        point: *point,
                        name: name.clone(),
                        captures: shifted(captures, start),
                        line: line.clone(),
                        hook: hook.clone(),
                        map_signal: *map_signal,
                    });
                }
                Action::Color { style, group } if line.styled().is_empty() => {
                    for (from, to) in rule
                        .pattern
                        .spans(text, *group)
                        .filter(|(from, to)| from < to)
                    {
                        paint(
                            &mut coloring,
                            Span {
                                start: (start + from) as u32,
                                end: (start + to) as u32,
                                style: *style,
                            },
                        );
                    }
                }
                Action::Trigger { index } => self.triggered.push(*index),
                Action::Color { .. } | Action::Prompt | Action::TableStart { .. } => {}
            }
        }
        self.lines_read += 1;
        if self.cost_sampling != 0 && self.lines_read.is_multiple_of(self.cost_sampling) {
            self.sample_costs(text);
        }
        coloring
    }

    /// Whether a table is being read, so the pager should be answered.
    pub fn table_open(&self) -> bool {
        !self.open.is_empty()
    }

    /// `line` is a prompt: ends every open table on it. For a prompt only
    /// the caller knows, such as a learned one; the ones the plugins declare
    /// end tables by themselves.
    pub fn end_tables(&mut self, line: &Line, sink: &mut impl PluginSink) {
        for open in std::mem::take(&mut self.open) {
            self.compiled.tables[open.table].send(open.ending(), line, Box::default(), sink);
        }
    }

    /// The server ended `line` with GA or EOR, which ends every open table,
    /// unless it was the pager's prompt: that table has more to come.
    pub fn end_tables_at_signal(&mut self, line: &Line, sink: &mut impl PluginSink) {
        if !self.settings().is_pager(line.plain()) {
            self.end_tables(line, sink);
        }
    }

    /// The connection ended: no open table will get the rest of its rows.
    pub fn cut_tables(&mut self, sink: &mut impl PluginSink) {
        self.cuts.drain(..).for_each(|cut| sink.event(cut));
        for open in std::mem::take(&mut self.open) {
            let table = &self.compiled.tables[open.table];
            table.send(TableEvent::Cut, &open.first_line, Box::default(), sink);
        }
    }

    /// The client answered the pager prompt last read.
    pub fn pager_answered(&mut self) {
        for open in &mut self.open {
            open.paged = false;
        }
    }

    /// Says whether the text the server hasn't ended is the pager's prompt
    /// or a prompt. The pager wins when both match. Asking again about the
    /// same text doesn't match again: the answer depends on the text alone,
    /// so it holds across pipelines, as after a reconnect.
    pub fn read_pending(&mut self, pending: &Pending<'_>) -> Option<PendingKind> {
        let cached = self
            .last_pending
            .as_ref()
            .filter(|(last, _)| **last == *pending.plain)
            .map(|(_, kind)| kind.clone());
        let kind = cached.unwrap_or_else(|| {
            let kind = self.classify_pending(pending.plain);
            self.last_pending = Some((pending.plain.into(), kind.clone()));
            kind
        });
        if matches!(kind, Some(PendingKind::Pager { .. })) {
            for open in &mut self.open {
                if open.pager_line != Some(pending.number) {
                    open.pager_line = Some(pending.number);
                    open.paged = true;
                }
            }
        }
        kind
    }

    /// The links and actions the enabled plugins' Text rules find in `line`,
    /// in run order, for a shell to offer. Asked for per line shown, not
    /// per line read, so they cost the line thread nothing.
    pub fn links(&self, line: &Line) -> Vec<Link> {
        let mut links = Vec::new();
        for rule in self.link_rules() {
            rule.find(line.plain(), &mut links);
        }
        links
    }

    /// The enabled plugins' link rules, in run order. Copies share their
    /// compiled patterns, so a shell's thread can run them.
    pub fn link_rules(&self) -> impl Iterator<Item = &LinkRule> {
        self.declared(|points| points.text.as_ref())
            .flat_map(|(_, text)| &text.links)
    }

    /// Each installed plugin's sampled cost per line, in the order added.
    pub fn costs(&self) -> Vec<PluginCost> {
        self.installed
            .iter()
            .map(|entry| PluginCost {
                plugin: entry.plugin.id().clone(),
                sampled_lines: entry.cost.sampled,
                per_line: Summary::of(&entry.cost.recent),
            })
            .collect()
    }

    /// What `text`, unended, is: `self.matched` is overwritten. A prompt
    /// hint must cover all of it, or the rest is output on its way.
    fn classify_pending(&mut self, text: &str) -> Option<PendingKind> {
        let compiled = &self.compiled;
        compiled.pending.matching(text, &mut self.matched);
        self.matched.iter().find_map(|&id| {
            let (pattern, kind) = &compiled.pending_rules[id];
            let whole = |(_, end): (usize, usize)| text[end..].trim().is_empty();
            let fits =
                matches!(kind, PendingKind::Pager { .. }) || pattern.spans(text, 0).any(whole);
            fits.then(|| kind.clone())
        })
    }

    /// Runs the open tables' row and end patterns over the line, then opens
    /// what `self.matched` starts. A line that starts one of a plugin's
    /// tables is no row of the one it has open.
    fn read_tables(&mut self, line: &Line, start: usize, sink: &mut impl PluginSink) {
        let Compiled {
            lines,
            tables,
            rows,
            row_rules,
            settings,
            ..
        } = &self.compiled;
        let text = &line.plain()[start..];
        let starts: Vec<(usize, &LineRule)> = self
            .matched
            .iter()
            .filter_map(|&id| match lines.rules[id].action {
                Action::TableStart { table } => Some((table, &lines.rules[id])),
                _ => None,
            })
            .collect();
        let started = |slot: usize| starts.iter().any(|&(table, _)| tables[table].slot == slot);
        if !self.open.is_empty() {
            rows.matching(text, &mut self.matched_rows);
            let matched_rows = &self.matched_rows;
            self.open.retain_mut(|open| {
                let table = &tables[open.table];
                if started(table.slot) {
                    return true;
                }
                let rule = matched_rows
                    .iter()
                    .map(|&id| &row_rules[id])
                    .find(|rule| rule.table == open.table);
                let Some(rule) = rule else {
                    // The pager's line is the table going on, not a gap in it.
                    if !open.in_gap && !settings.is_pager(text) {
                        open.in_gap = true;
                        table.send(TableEvent::Gap, line, Box::default(), sink);
                    }
                    return true;
                };
                let captures = rule
                    .pattern
                    .captures(text)
                    .map(|captures| shifted(captures, start))
                    .unwrap_or_default();
                if rule.ends {
                    table.send(TableEvent::End, line, captures, sink);
                    return false;
                }
                open.has_rows = true;
                open.in_gap = false;
                open.paged = false;
                table.send(TableEvent::Row, line, captures, sink);
                true
            });
        }
        for (index, rule) in starts {
            let table = &tables[index];
            let open = self
                .open
                .iter()
                .position(|open| tables[open.table].slot == table.slot);
            let continues = open.is_some_and(|at| {
                let open = &self.open[at];
                open.table == index && !open.has_rows
            });
            if !continues {
                if let Some(at) = open {
                    let open = self.open.swap_remove(at);
                    tables[open.table].send(open.ending(), line, Box::default(), sink);
                }
                self.open.push(OpenTable {
                    table: index,
                    first_line: line.clone(),
                    has_rows: false,
                    in_gap: false,
                    paged: false,
                    pager_line: None,
                });
            }
            let captures = rule
                .pattern
                .captures(text)
                .map(|captures| shifted(captures, start))
                .unwrap_or_default();
            table.send(TableEvent::Start, line, captures, sink);
        }
    }

    /// Times each enabled plugin's rules over `text`, one by one, but only
    /// those the merged matcher's prefilter lets through: the rules whose
    /// required literals `text` holds, and the plugin's unfiltered rules.
    /// Finding those rules takes one more prefilter scan of `text`, made
    /// once for all plugins and counted against none. That keeps no second
    /// matcher per plugin, and stays close to the plugin's share. Row
    /// patterns aren't timed: they run only inside a table.
    fn sample_costs(&mut self, text: &str) {
        let lines = &self.compiled.lines;
        lines.matcher.candidates(text, &mut self.matched);
        for (range, &index) in self.compiled.plugin_rules.iter().zip(&self.compiled.order) {
            let first = self.matched.partition_point(|&id| id < range.start);
            let end = self.matched.partition_point(|&id| id < range.end);
            let started = Instant::now();
            for rule in self.matched[first..end].iter().map(|&id| &lines.rules[id]) {
                if !rule.pattern.is_match(text) {
                    continue;
                }
                match rule.action {
                    Action::Event { .. } | Action::TableStart { .. } => {
                        std::hint::black_box(rule.pattern.captures(text));
                    }
                    Action::Color { group, .. } => {
                        std::hint::black_box(rule.pattern.spans(text, group).count());
                    }
                    Action::Prompt | Action::Trigger { .. } => {}
                }
            }
            self.installed[index].cost.record(started.elapsed());
        }
    }

    fn position(&self, id: &PluginId) -> Option<usize> {
        self.installed
            .iter()
            .position(|entry| entry.plugin.id() == id)
    }

    /// Builds the matchers again. Open tables can't carry over, as they index
    /// the old build: each is cut, and its hook hears at the next read.
    fn recompile(&mut self) -> Result<(), PluginError> {
        let compiled = Compiled::build(&self.installed, &self.triggers)?;
        for open in self.open.drain(..) {
            let table = &self.compiled.tables[open.table];
            table.send(
                TableEvent::Cut,
                &open.first_line,
                Box::default(),
                &mut self.cuts,
            );
        }
        self.compiled = compiled;
        self.generation += 1;
        self.last_pending = None;
        Ok(())
    }
}

fn duplicate(id: &PluginId) -> PluginError {
    PluginError {
        source: id.to_string(),
        problem: Problem::DuplicateId(id.clone()),
    }
}

/// Everything derived from the set of enabled plugins.
struct Compiled {
    /// Indexes into the installed plugins, in run order.
    order: Vec<usize>,
    lines: LineRules,
    pending: Matcher,
    pending_rules: Vec<(Pattern, PendingKind)>,
    /// Each enabled plugin's rules in `lines`, in run order, for costs.
    plugin_rules: Vec<Range<usize>>,
    tables: Vec<Table>,
    /// Every table's end and row patterns, run only while a table is open.
    rows: Matcher,
    /// Each table's end pattern comes before its rows, so it wins.
    row_rules: Vec<RowRule>,
    settings: Settings,
}

/// A [`TableReader`] as the host runs it.
struct Table {
    /// The plugin's place in run order.
    slot: usize,
    plugin: PluginId,
    point: Point,
    hook: Option<HookName>,
    start: Arc<str>,
    row: Arc<str>,
    gap: Arc<str>,
    end: Arc<str>,
    cut: Arc<str>,
}

#[derive(Clone, Copy)]
enum TableEvent {
    Start,
    Row,
    Gap,
    End,
    Cut,
}

impl Table {
    fn new(slot: usize, plugin: PluginId, point: Point, reader: &TableReader) -> Table {
        let event = |suffix: &str| Arc::from(format!("{}.{suffix}", reader.name));
        Table {
            slot,
            plugin,
            point,
            hook: reader.hook.clone(),
            start: event("start"),
            row: event("row"),
            gap: event("gap"),
            end: event("end"),
            cut: event("cut"),
        }
    }

    fn send(
        &self,
        event: TableEvent,
        line: &Line,
        captures: Box<[Capture]>,
        sink: &mut impl PluginSink,
    ) {
        let name = match event {
            TableEvent::Start => &self.start,
            TableEvent::Row => &self.row,
            TableEvent::Gap => &self.gap,
            TableEvent::End => &self.end,
            TableEvent::Cut => &self.cut,
        };
        sink.event(PluginEvent {
            plugin: self.plugin.clone(),
            point: self.point,
            name: name.clone(),
            captures,
            line: line.clone(),
            hook: self.hook.clone(),
            map_signal: None,
        });
    }
}

struct RowRule {
    /// Into [`Compiled::tables`].
    table: usize,
    pattern: Pattern,
    ends: bool,
}

struct LineRules {
    matcher: Matcher,
    rules: Vec<LineRule>,
}

#[derive(Clone)]
struct LineRule {
    pattern: Pattern,
    action: Action,
}

#[derive(Clone)]
enum Action {
    Event {
        plugin: PluginId,
        point: Point,
        name: Arc<str>,
        hook: Option<HookName>,
        map_signal: Option<MapSignal>,
    },
    /// Colors `group` of each match; 0 is the whole match.
    Color { style: Style, group: usize },
    /// The prompt's shape: ends open tables where it starts a line.
    Prompt,
    /// Opens [`Compiled::tables`]`[table]`.
    TableStart { table: usize },
    /// The player's trigger at `index`.
    Trigger { index: usize },
}

impl LineRule {
    fn starts_with_prompt(&self, text: &str) -> bool {
        matches!(self.action, Action::Prompt)
            && self
                .pattern
                .spans(text, 0)
                .next()
                .is_some_and(|(at, _)| at == 0)
    }
}

/// `captures` of a text that starts at byte `start` of the line.
fn shifted(captures: Box<[Capture]>, start: usize) -> Box<[Capture]> {
    let start = start as u32;
    captures
        .into_iter()
        .map(|capture| Capture {
            start: capture.start + start,
            end: capture.end + start,
            ..capture
        })
        .collect()
}

impl Compiled {
    fn build(installed: &[Installed], triggers: &[Pattern]) -> Result<Compiled, PluginError> {
        let mut order: Vec<usize> = (0..installed.len())
            .filter(|&index| installed[index].enabled)
            .collect();
        order.sort_by_key(|&index| installed[index].plugin.layer());
        let plugins: Vec<&Plugin> = order
            .iter()
            .map(|&index| &installed[index].plugin)
            .collect();
        let settings = settings(&plugins);

        let mut rules = Vec::new();
        let mut plugin_rules = Vec::with_capacity(plugins.len());
        let mut tables = Vec::new();
        let mut row_rules = Vec::new();
        for (slot, plugin) in plugins.iter().enumerate() {
            let first = rules.len();
            rules.extend(line_rules(plugin));
            for (point, reader) in plugin.points.readers() {
                let table = tables.len();
                tables.push(Table::new(slot, plugin.id().clone(), point, reader));
                let line_rule = |pattern: &Pattern| LineRule {
                    pattern: pattern.clone(),
                    action: Action::TableStart { table },
                };
                rules.extend(reader.start.iter().map(line_rule));
                let row_rule = |pattern: &Pattern, ends| RowRule {
                    table,
                    pattern: pattern.clone(),
                    ends,
                };
                row_rules.extend(reader.end.iter().map(|end| row_rule(end, true)));
                row_rules.extend(reader.row.iter().map(|row| row_rule(row, false)));
            }
            plugin_rules.push(first..rules.len());
        }
        rules.extend(trigger_rules(triggers));
        let lines = line_matcher(rules)?;
        let sources: Vec<&str> = row_rules.iter().map(|rule| rule.pattern.as_str()).collect();
        let rows = matcher(&sources, DfaCache::Default)?;

        let mut pending_rules = Vec::new();
        if let Some(pager) = &settings.pager {
            let key = pager.key.as_str().into();
            pending_rules.push((pager.prompt.clone(), PendingKind::Pager { key }));
        }
        for text in plugins
            .iter()
            .filter_map(|plugin| plugin.points.text.as_ref())
        {
            for hint in &text.prompts {
                pending_rules.push((hint.pattern.clone(), PendingKind::Prompt));
            }
        }
        let sources: Vec<&str> = pending_rules
            .iter()
            .map(|(pattern, _)| pattern.as_str())
            .collect();
        let pending = matcher(&sources, DfaCache::Default)?;

        Ok(Compiled {
            order,
            lines,
            pending,
            pending_rules,
            plugin_rules,
            tables,
            rows,
            row_rules,
            settings,
        })
    }
}

fn line_matcher(rules: Vec<LineRule>) -> Result<LineRules, PluginError> {
    let sources: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
    let matcher = matcher(&sources, DfaCache::Large)?;
    Ok(LineRules { matcher, rules })
}

fn trigger_rules(patterns: &[Pattern]) -> impl Iterator<Item = LineRule> {
    patterns
        .iter()
        .enumerate()
        .map(|(index, pattern)| LineRule {
            pattern: pattern.clone(),
            action: Action::Trigger { index },
        })
}

fn matcher(sources: &[&str], cache: DfaCache) -> Result<Matcher, PluginError> {
    Matcher::new(sources, cache).map_err(|message| PluginError {
        source: "plugin host".into(),
        problem: Problem::Matcher(message),
    })
}

/// A plugin's prompt shapes, its event rules, then its color rules.
fn line_rules(plugin: &Plugin) -> Vec<LineRule> {
    let rule = |pattern: &Pattern, action| LineRule {
        pattern: pattern.clone(),
        action,
    };
    let text = plugin.points.text.iter();
    let prompts = text
        .clone()
        .flat_map(|text| &text.prompts)
        .map(|hint| rule(&hint.pattern, Action::Prompt));
    let events = plugin
        .points
        .event_rules()
        .into_iter()
        .map(|(point, event, map_signal)| {
            rule(
                &event.pattern,
                Action::Event {
                    plugin: plugin.id().clone(),
                    point,
                    name: event.name.shared(),
                    hook: event.hook.clone(),
                    map_signal,
                },
            )
        });
    let colors = text.flat_map(|text| &text.colors).map(|color| {
        let group = color
            .group
            .as_deref()
            .and_then(|name| color.pattern.group(name))
            .unwrap_or(0);
        rule(
            &color.pattern,
            Action::Color {
                style: color.style(),
                group,
            },
        )
    });
    prompts.chain(events).chain(colors).collect()
}

/// The single values, each from the last plugin in run order that sets it.
fn settings(plugins: &[&Plugin]) -> Settings {
    fn last<'a, T>(plugins: &[&'a Plugin], value: impl Fn(&'a Plugin) -> Option<T>) -> Option<T> {
        plugins.iter().rev().find_map(|plugin| value(plugin))
    }
    Settings {
        encoding: last(plugins, |plugin| plugin.manifest.encoding()),
        separator: last(plugins, |plugin| {
            plugin.points.commands.as_ref()?.separator.clone()
        }),
        room_identity: last(plugins, |plugin| plugin.points.map.as_ref()?.room_identity),
        pager: last(plugins, |plugin| plugin.points.text.as_ref()?.pager.clone()),
        area_name: last(plugins, |plugin| {
            plugin.points.map.as_ref()?.area_name.clone()
        }),
    }
}

/// Paints `span` over `spans` (sorted, non-overlapping), splitting what it
/// covers.
fn paint(spans: &mut Vec<Span>, span: Span) {
    let mut painted = Vec::with_capacity(spans.len() + 2);
    for old in spans.drain(..) {
        if old.end <= span.start || old.start >= span.end {
            painted.push(old);
            continue;
        }
        if old.start < span.start {
            painted.push(Span {
                end: span.start,
                ..old
            });
        }
        if old.end > span.end {
            painted.push(Span {
                start: span.end,
                ..old
            });
        }
    }
    painted.push(span);
    painted.sort_by_key(|span| span.start);
    *spans = painted;
}

#[derive(Default)]
struct CostLog {
    sampled: u64,
    recent: Vec<Duration>,
}

impl CostLog {
    fn record(&mut self, elapsed: Duration) {
        if self.recent.len() < COST_SAMPLES_KEPT {
            self.recent.push(elapsed);
        } else {
            self.recent[self.sampled as usize % COST_SAMPLES_KEPT] = elapsed;
        }
        self.sampled += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ansi::Color;

    fn span(start: u32, end: u32, color: u8) -> Span {
        Span {
            start,
            end,
            style: Style {
                fg: Some(Color(color)),
                ..Style::default()
            },
        }
    }

    #[test]
    fn a_later_span_paints_over_what_it_covers() {
        let mut spans = vec![span(0, 10, 1)];
        paint(&mut spans, span(3, 5, 2));
        assert_eq!(spans, [span(0, 3, 1), span(3, 5, 2), span(5, 10, 1)]);
        paint(&mut spans, span(4, 12, 3));
        assert_eq!(spans, [span(0, 3, 1), span(3, 4, 2), span(4, 12, 3)]);
        paint(&mut spans, span(0, 20, 4));
        assert_eq!(spans, [span(0, 20, 4)]);
    }

    #[test]
    fn the_cost_log_keeps_the_latest_samples() {
        let mut log = CostLog::default();
        for micros in 0..(COST_SAMPLES_KEPT as u64 + 10) {
            log.record(Duration::from_micros(micros));
        }
        assert_eq!(log.recent.len(), COST_SAMPLES_KEPT);
        assert_eq!(log.sampled, COST_SAMPLES_KEPT as u64 + 10);
        assert!(!log.recent.contains(&Duration::from_micros(9)));
    }
}
