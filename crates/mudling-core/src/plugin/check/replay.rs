//! A log's server text through a plugin, as the client reads it: the line
//! pipeline, the plugin host beside the built-in defaults, and the Luau hooks
//! on this thread. It has no connection, so hooks read an empty state, and
//! the pager is taken as answered.

use super::patterns::{self, Read, Rule};
use crate::defaults;
use crate::line::{Line, LinePipeline};
use crate::plugin::luau::{HookLimits, HookOutput, HookRunner, StateView, Value};
use crate::plugin::{PendingKind, Plugin, PluginError, PluginEvent, PluginHost};
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;
use std::sync::Arc;

/// Lines quoted for each pattern.
const EXAMPLES: usize = 3;
const QUOTE_CHARS: usize = 120;

/// What a log did.
#[derive(Default)]
pub struct Replay {
    /// Every event and hook output, in order: what `.expected` files hold.
    pub transcript: Vec<String>,
    pub caught: Vec<Caught>,
    /// Hook failures by hook, and whether they overran.
    pub failures: BTreeMap<(String, bool), HookFailures>,
}

/// What one pattern caught.
pub struct Caught {
    pub rule: Rule,
    pub lines: usize,
    pub examples: Vec<String>,
}

pub struct HookFailures {
    pub times: usize,
    pub first: String,
}

/// Replays `lines`. An error means the plugin can't run at all.
pub fn replay(plugin: &Plugin, lines: &[String]) -> Result<Replay, PluginError> {
    let mut reader = Reader::new(plugin)?;
    for line in lines {
        reader.read_server_line(line);
    }
    reader.end_of_log();
    Ok(reader.replay)
}

/// A host of the built-in defaults and `plugin`, without cost sampling. It
/// fails if their patterns won't compile together.
pub fn host_with(plugin: Option<&Plugin>) -> Result<PluginHost, PluginError> {
    let plugins = [defaults::plugin()].into_iter().chain(plugin.cloned());
    let mut host = PluginHost::new(plugins)?;
    host.set_cost_sampling(0);
    Ok(host)
}

struct Reader {
    pipeline: LinePipeline,
    host: PluginHost,
    hooks: HookRunner,
    replay: Replay,
}

impl Reader {
    fn new(plugin: &Plugin) -> Result<Reader, PluginError> {
        // The log's commands don't arrive at the pace they were sent.
        let limits = HookLimits {
            commands_per_second: u32::MAX,
            ..HookLimits::default()
        };
        let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
        let mut hooks = HookRunner::new(limits, state);
        hooks.add(plugin)?;
        hooks.set_approved(plugin.id(), true);
        let caught = patterns::declared(plugin)
            .into_iter()
            .map(|rule| Caught {
                rule,
                lines: 0,
                examples: Vec::new(),
            })
            .collect();
        Ok(Reader {
            pipeline: LinePipeline::default(),
            host: host_with(Some(plugin))?,
            hooks,
            replay: Replay {
                caught,
                ..Replay::default()
            },
        })
    }

    /// One log line. Text the plugin's prompt hint recognizes before its line
    /// end is ended there; the rest ends at the log's line end.
    fn read_server_line(&mut self, text: &str) {
        let mut lines = Vec::new();
        self.pipeline.push(text.as_bytes(), &mut lines);
        match self
            .pipeline
            .pending()
            .and_then(|pending| self.host.read_pending(&pending))
        {
            Some(PendingKind::Prompt) => lines.extend(self.pipeline.prompt_end()),
            Some(PendingKind::Pager { .. }) => {
                self.host.pager_answered();
                self.pipeline.push(b"\n", &mut lines);
            }
            None => self.pipeline.push(b"\n", &mut lines),
        }
        for line in &lines {
            let mut events = Vec::new();
            self.host.read_line(line, &mut events);
            self.note_what_caught(line, &events);
            self.deliver(&events);
        }
    }

    /// A log ends as a connection does: an open table is cut.
    fn end_of_log(&mut self) {
        let mut events = Vec::new();
        self.host.cut_tables(&mut events);
        self.deliver(&events);
    }

    fn note_what_caught(&mut self, line: &Line, events: &[PluginEvent]) {
        let text = line.plain();
        for caught in &mut self.replay.caught {
            let rule = &caught.rule;
            let matched = match &rule.read {
                Read::Event { point, name } => {
                    events
                        .iter()
                        .any(|event| event.point == *point && &*event.name == name)
                        && rule.pattern.is_match(text)
                }
                Read::Line => rule.pattern.is_match(text),
                Read::Elsewhere(_) => false,
            };
            if matched {
                caught.lines += 1;
                if caught.examples.len() < EXAMPLES {
                    caught.examples.push(example(&rule.pattern, text));
                }
            }
        }
    }

    fn deliver(&mut self, events: &[PluginEvent]) {
        let mut outputs = Vec::new();
        for event in events {
            self.replay.transcript.push(event_line(event));
            outputs.clear();
            self.hooks.run(event, &mut outputs);
            for output in &outputs {
                self.replay.transcript.push(output_line(output));
                if let HookOutput::HookFailed(failed) = output {
                    let key = (failed.hook.as_str().to_owned(), failed.overran);
                    let failures = self.replay.failures.entry(key).or_insert(HookFailures {
                        times: 0,
                        first: failed.message.clone(),
                    });
                    failures.times += 1;
                }
            }
        }
    }
}

/// The line, cut to a length, and what the pattern captured of it.
fn example(pattern: &crate::plugin::Pattern, text: &str) -> String {
    let mut example: String = text.chars().take(QUOTE_CHARS).collect();
    for capture in pattern.captures(text).unwrap_or_default() {
        let value = capture.text(text);
        write!(example, "  {}={value:?}", capture.name).expect("a String takes writes");
    }
    example
}

fn event_line(event: &PluginEvent) -> String {
    let mut line = format!("event {} {}", event.point, event.name);
    let plain = event.line.plain();
    for capture in &event.captures {
        let value = capture.text(plain);
        write!(line, " {}={value:?}", capture.name).expect("a String takes writes");
    }
    line
}

fn output_line(output: &HookOutput) -> String {
    match output {
        HookOutput::StoreWrite(write) => {
            let value = match &write.value {
                Value::Bool(value) => value.to_string(),
                Value::Number(value) => value.to_string(),
                Value::Text(value) => format!("{value:?}"),
            };
            format!("store {} {} {value}", write.store, write.key)
        }
        HookOutput::SendCommand(send) => format!("send {:?}", send.command),
        HookOutput::RefreshInventory => "refresh_inventory".to_owned(),
        HookOutput::HookFailed(failed) => format!("hook_failed {}", failed.hook.as_str()),
        HookOutput::PluginDisabled(disabled) => format!("plugin_disabled {}", disabled.plugin),
    }
}
