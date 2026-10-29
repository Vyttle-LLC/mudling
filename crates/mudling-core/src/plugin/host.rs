//! The plugin host: runs the enabled plugins over each line in one pass.
//!
//! It lives on the core's line thread, next to the line pipeline. For each
//! finished [`Line`], call [`PluginHost::read_line`]; for text the server
//! hasn't ended yet ([`crate::line::LinePipeline::pending`]), call
//! [`PluginHost::read_pending`] to learn whether it's a prompt or a pager
//! prompt.
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

use super::manifest::{Plugin, PluginError, PluginId, Problem};
use super::matcher::{DfaCache, Matcher};
use super::points::{Capture, HookName, Pager, Pattern, Point, Points, RoomIdentity};
use crate::ansi::{Span, Style};
use crate::line::{Encoding, Line, Pending};
use crate::stats::Summary;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// By default, every 256th line also times each plugin on its own, running
/// its rules one by one: often enough to fill Settings › Plugins, too rarely
/// to move the p99.
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
}

impl PluginEvent {
    /// The text of the named capture, if it took part in the match.
    pub fn capture(&self, name: &str) -> Option<&str> {
        let capture = self.captures.iter().find(|capture| &*capture.name == name)?;
        Some(&self.line.plain()[capture.start as usize..capture.end as usize])
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
    matched: Vec<usize>,
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
            if installed.iter().any(|other| other.plugin.id() == plugin.id()) {
                return Err(duplicate(plugin.id()));
            }
            installed.push(Installed { plugin, enabled: true, cost: CostLog::default() });
        }
        let compiled = Compiled::build(&installed)?;
        Ok(PluginHost {
            installed,
            compiled,
            generation: 1,
            cost_sampling: DEFAULT_COST_SAMPLING,
            lines_read: 0,
            last_pending: None,
            matched: Vec::new(),
        })
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
        self.installed.push(Installed { plugin, enabled: true, cost: CostLog::default() });
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
        self.installed.iter().map(|entry| (&entry.plugin, entry.enabled))
    }

    /// The enabled plugins, in run order.
    pub fn enabled(&self) -> impl Iterator<Item = &Plugin> {
        self.compiled.order.iter().map(|&index| &self.installed[index].plugin)
    }

    /// Each enabled plugin's declaration for one extension point, in run
    /// order: `host.declared(|points| points.gear.as_ref())`.
    pub fn declared<'a, T: 'a>(
        &'a self,
        point: impl Fn(&'a Points) -> Option<&'a T> + 'a,
    ) -> impl Iterator<Item = (&'a PluginId, &'a T)> + 'a {
        self.enabled().filter_map(move |plugin| point(&plugin.points).map(|declaration| (plugin.id(), declaration)))
    }

    pub fn settings(&self) -> &Settings {
        &self.compiled.settings
    }

    /// Counts compilations, so callers can tell when the matchers changed.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Runs every enabled plugin's Text patterns over `line` in one pass,
    /// sending each match to `sink` in run order. Returns the coloring for a
    /// line the server sent without styles (empty otherwise, or when no rule
    /// matched), as non-overlapping spans of its plain text.
    pub fn read_line(&mut self, line: &Line, sink: &mut impl PluginSink) -> Vec<Span> {
        let text = line.plain();
        let lines = &self.compiled.lines;
        lines.matcher.matching(text, &mut self.matched);
        let mut coloring = Vec::new();
        for &id in &self.matched {
            let rule = &lines.rules[id];
            match &rule.action {
                Action::Event { name, hook } => {
                    let Some(captures) = rule.pattern.captures(text) else { continue };
                    sink.event(PluginEvent {
                        plugin: rule.plugin.clone(),
                        point: Point::Text,
                        name: name.clone(),
                        captures,
                        line: line.clone(),
                        hook: hook.clone(),
                    });
                }
                Action::Color { style, group } if line.styled().is_empty() => {
                    for (start, end) in rule.pattern.spans(text, *group).filter(|(start, end)| start < end) {
                        paint(&mut coloring, Span { start: start as u32, end: end as u32, style: *style });
                    }
                }
                Action::Color { .. } => {}
            }
        }
        self.lines_read += 1;
        if self.cost_sampling != 0 && self.lines_read.is_multiple_of(self.cost_sampling) {
            self.sample_costs(text);
        }
        coloring
    }

    /// Says whether the text the server hasn't ended is the pager's prompt
    /// or a prompt. The pager wins when both match. Asking again about the
    /// same text doesn't match again: the answer depends on the text alone,
    /// so it holds across pipelines, as after a reconnect.
    pub fn read_pending(&mut self, pending: &Pending<'_>) -> Option<PendingKind> {
        if let Some((last, kind)) = &self.last_pending
            && **last == *pending.plain
        {
            return kind.clone();
        }
        let compiled = &self.compiled;
        compiled.pending.matching(pending.plain, &mut self.matched);
        let kind = self.matched.first().map(|&id| compiled.pending_kinds[id].clone());
        self.last_pending = Some((pending.plain.into(), kind.clone()));
        kind
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

    /// Times each enabled plugin's own rules over `text`, one by one. That
    /// keeps no second matcher per plugin, and runs no slower than the
    /// plugin would on its own: an upper bound on its share of the pass.
    fn sample_costs(&mut self, text: &str) {
        let rules = &self.compiled.lines.rules;
        for (range, &index) in self.compiled.plugin_rules.iter().zip(&self.compiled.order) {
            let started = Instant::now();
            for rule in &rules[range.clone()] {
                match rule.action {
                    Action::Event { .. } => {
                        std::hint::black_box(rule.pattern.captures(text));
                    }
                    Action::Color { group, .. } => {
                        std::hint::black_box(rule.pattern.spans(text, group).count());
                    }
                }
            }
            self.installed[index].cost.record(started.elapsed());
        }
    }

    fn position(&self, id: &PluginId) -> Option<usize> {
        self.installed.iter().position(|entry| entry.plugin.id() == id)
    }

    fn recompile(&mut self) -> Result<(), PluginError> {
        self.compiled = Compiled::build(&self.installed)?;
        self.generation += 1;
        self.last_pending = None;
        Ok(())
    }
}

fn duplicate(id: &PluginId) -> PluginError {
    PluginError { source: id.to_string(), problem: Problem::DuplicateId(id.clone()) }
}

/// Everything derived from the set of enabled plugins.
struct Compiled {
    /// Indexes into the installed plugins, in run order.
    order: Vec<usize>,
    lines: LineRules,
    pending: Matcher,
    pending_kinds: Vec<PendingKind>,
    /// Each enabled plugin's rules in `lines`, in run order, for costs.
    plugin_rules: Vec<Range<usize>>,
    settings: Settings,
}

struct LineRules {
    matcher: Matcher,
    rules: Vec<LineRule>,
}

struct LineRule {
    plugin: PluginId,
    pattern: Pattern,
    action: Action,
}

enum Action {
    Event { name: Arc<str>, hook: Option<HookName> },
    /// Colors `group` of each match; 0 is the whole match.
    Color { style: Style, group: usize },
}

impl Compiled {
    fn build(installed: &[Installed]) -> Result<Compiled, PluginError> {
        let mut order: Vec<usize> = (0..installed.len()).filter(|&index| installed[index].enabled).collect();
        order.sort_by_key(|&index| installed[index].plugin.layer());
        let plugins: Vec<&Plugin> = order.iter().map(|&index| &installed[index].plugin).collect();
        let settings = settings(&plugins);

        let mut rules = Vec::new();
        let mut plugin_rules = Vec::with_capacity(plugins.len());
        for plugin in &plugins {
            let first = rules.len();
            rules.extend(line_rules(plugin));
            plugin_rules.push(first..rules.len());
        }
        let sources: Vec<&str> = rules.iter().map(|rule| rule.pattern.as_str()).collect();
        let lines = matcher(&sources, DfaCache::Large)?;

        let mut pending_sources = Vec::new();
        let mut pending_kinds = Vec::new();
        if let Some(pager) = &settings.pager {
            pending_sources.push(pager.prompt.as_str());
            pending_kinds.push(PendingKind::Pager { key: pager.key.as_str().into() });
        }
        for text in plugins.iter().filter_map(|plugin| plugin.points.text.as_ref()) {
            for hint in &text.prompts {
                pending_sources.push(hint.pattern.as_str());
                pending_kinds.push(PendingKind::Prompt);
            }
        }
        let pending = matcher(&pending_sources, DfaCache::Default)?;

        Ok(Compiled {
            order,
            lines: LineRules { matcher: lines, rules },
            pending,
            pending_kinds,
            plugin_rules,
            settings,
        })
    }
}

fn matcher(sources: &[&str], cache: DfaCache) -> Result<Matcher, PluginError> {
    Matcher::new(sources, cache)
        .map_err(|message| PluginError { source: "plugin host".into(), problem: Problem::Matcher(message) })
}

/// A plugin's Text patterns, then its color rules.
fn line_rules(plugin: &Plugin) -> Vec<LineRule> {
    let Some(text) = &plugin.points.text else {
        return Vec::new();
    };
    let rule = |pattern: &Pattern, action| LineRule { plugin: plugin.id().clone(), pattern: pattern.clone(), action };
    let events = text.patterns.iter().map(|event| {
        rule(&event.pattern, Action::Event { name: event.name.shared(), hook: event.hook.clone() })
    });
    let colors = text.colors.iter().map(|color| {
        let group = color.group.as_deref().and_then(|name| color.pattern.group(name)).unwrap_or(0);
        rule(&color.pattern, Action::Color { style: color.style(), group })
    });
    events.chain(colors).collect()
}

/// The single values, each from the last plugin in run order that sets it.
fn settings(plugins: &[&Plugin]) -> Settings {
    fn last<'a, T>(plugins: &[&'a Plugin], value: impl Fn(&'a Plugin) -> Option<T>) -> Option<T> {
        plugins.iter().rev().find_map(|plugin| value(plugin))
    }
    Settings {
        encoding: last(plugins, |plugin| plugin.manifest.encoding()),
        separator: last(plugins, |plugin| plugin.points.commands.as_ref()?.separator.clone()),
        room_identity: last(plugins, |plugin| plugin.points.map.as_ref()?.room_identity),
        pager: last(plugins, |plugin| plugin.points.text.as_ref()?.pager.clone()),
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
            painted.push(Span { end: span.start, ..old });
        }
        if old.end > span.end {
            painted.push(Span { start: span.end, ..old });
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
        Span { start, end, style: Style { fg: Some(Color(color)), ..Style::default() } }
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
