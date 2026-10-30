//! Runs the Avatar plugin the way the client will, through the public
//! plugin API only: server text through the line pipeline, each line
//! through the plugin host, and each event naming a hook through the Luau
//! hooks. The fixture tests and the log replay share it.

use mudling_core::line::{Line, LinePipeline};
use mudling_core::plugin::luau::{
    HookLimits, HookOutput, HookRunner, StateView, StoreWrite, Value,
};
use mudling_core::plugin::{PendingKind, Plugin, PluginEvent, PluginHost};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// The plugin folder: this crate's own.
pub fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn load() -> Plugin {
    Plugin::load(plugin_dir()).unwrap_or_else(|error| panic!("{error}"))
}

/// One connection's text through the plugin.
pub struct Session {
    pipeline: LinePipeline,
    host: PluginHost,
    hooks: HookRunner,
    /// Pager prompts seen, each one the client would answer.
    pub pager_prompts: usize,
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

impl Session {
    pub fn new() -> Session {
        let plugin = load();
        let state: Arc<dyn StateView> = Arc::new(HashMap::<String, Value>::new());
        let mut hooks = HookRunner::new(HookLimits::default(), state);
        hooks.add(&plugin).unwrap_or_else(|error| panic!("{error}"));
        let host = PluginHost::new([plugin]).unwrap_or_else(|error| panic!("{error}"));
        Session {
            pipeline: LinePipeline::default(),
            host,
            hooks,
            pager_prompts: 0,
        }
    }

    /// Reads one line the server sent. As Avatar sends no GA, a line that
    /// the plugin's prompt hint recognizes before its line end is ended
    /// there, as a prompt; one with output glued after it, as session logs
    /// have, is read whole. `observe` sees each event and its hook's
    /// outputs, in order.
    pub fn read_line(&mut self, text: &str, observe: &mut impl FnMut(&PluginEvent, &[HookOutput])) {
        let mut lines = Vec::new();
        self.pipeline.push(text.as_bytes(), &mut lines);
        match self
            .pipeline
            .pending()
            .and_then(|pending| self.host.read_pending(&pending))
        {
            Some(PendingKind::Prompt) => lines.extend(self.pipeline.prompt_end()),
            Some(PendingKind::Pager { .. }) => {
                self.pager_prompts += 1;
                self.pipeline.push(b"\n", &mut lines);
            }
            None => self.pipeline.push(b"\n", &mut lines),
        }
        for line in &lines {
            self.read(line, observe);
        }
    }

    fn read(&mut self, line: &Line, observe: &mut impl FnMut(&PluginEvent, &[HookOutput])) {
        let mut events: Vec<PluginEvent> = Vec::new();
        self.host.read_line(line, &mut events);
        let mut outputs = Vec::new();
        for event in &events {
            outputs.clear();
            self.hooks.run(event, &mut outputs);
            observe(event, &outputs);
        }
    }
}

/// Everything a run of lines produced.
#[derive(Debug, Default)]
pub struct Transcript {
    pub events: Vec<PluginEvent>,
    pub outputs: Vec<HookOutput>,
    pub pager_prompts: usize,
}

impl Transcript {
    /// Reads `text`, one server line per line, through a new session.
    pub fn of(text: &str) -> Transcript {
        let mut session = Session::new();
        let mut transcript = Transcript::default();
        for line in text.lines() {
            session.read_line(line, &mut |event, outputs| {
                transcript.events.push(event.clone());
                transcript.outputs.extend_from_slice(outputs);
            });
        }
        transcript.pager_prompts = session.pager_prompts;
        transcript
    }

    /// The store writes, in order.
    pub fn writes(&self) -> impl Iterator<Item = &StoreWrite> {
        self.outputs.iter().filter_map(|output| match output {
            HookOutput::StoreWrite(write) => Some(write),
            _ => None,
        })
    }

    /// Each key of `store` and the value it was last written.
    pub fn store(&self, store: &str) -> HashMap<String, Value> {
        self.writes()
            .filter(|write| write.store == store)
            .map(|write| (write.key.clone(), write.value.clone()))
            .collect()
    }

    /// Every output that isn't a store write, such as a failed hook.
    pub fn problems(&self) -> Vec<&HookOutput> {
        self.outputs
            .iter()
            .filter(|output| !matches!(output, HookOutput::StoreWrite(_)))
            .collect()
    }

    /// The events named `name`, as the text of each of their `captures`.
    pub fn events(&self, name: &str, captures: &[&str]) -> Vec<Vec<Option<String>>> {
        self.events
            .iter()
            .filter(|event| &*event.name == name)
            .map(|event| {
                captures
                    .iter()
                    .map(|capture| event.capture(capture).map(str::to_owned))
                    .collect()
            })
            .collect()
    }
}
