//! The plugin inspector's record: which plugins claimed a line, and how long
//! the line took to read (PRD §11).
//!
//! A plugin claims a line by matching it: an event, a table's row or its
//! coloring. [`Claiming`] stands between the host and the real sink and notes
//! who sent what, so no rule runs twice. Only a line read with the inspector
//! open goes through it, so a closed inspector costs each line one branch.

use super::host::{PluginEvent, PluginSink};
use super::manifest::PluginId;
use crate::line::Line;
use std::sync::Arc;
use std::time::Duration;

/// What one plugin did with a line.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub plugin: PluginId,
    /// The events it raised, in order; none if it only colored the line.
    pub events: Vec<Arc<str>>,
}

/// A line the chat showed, and what claimed it.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedLine {
    pub line: Line,
    /// Nothing, when no plugin claimed it.
    pub claims: Vec<Claim>,
    /// The whole read, every plugin in the one pass.
    pub took: Duration,
}

/// Passes events on to `sink`, noting who sent each.
pub struct Claiming<'a, S> {
    sink: &'a mut S,
    claims: Vec<Claim>,
}

impl<'a, S> Claiming<'a, S> {
    pub fn new(sink: &'a mut S) -> Claiming<'a, S> {
        Claiming {
            sink,
            claims: Vec::new(),
        }
    }

    pub fn into_claims(self) -> Vec<Claim> {
        self.claims
    }

    fn claim(&mut self, plugin: &PluginId) -> &mut Claim {
        let at = self.claims.iter().position(|c| c.plugin == *plugin);
        let at = at.unwrap_or_else(|| {
            self.claims.push(Claim {
                plugin: plugin.clone(),
                events: Vec::new(),
            });
            self.claims.len() - 1
        });
        &mut self.claims[at]
    }
}

impl<S: PluginSink> PluginSink for Claiming<'_, S> {
    fn event(&mut self, event: PluginEvent) {
        let name = event.name.clone();
        self.claim(&event.plugin).events.push(name);
        self.sink.event(event);
    }

    fn colored(&mut self, plugin: &PluginId) {
        self.claim(plugin);
    }
}
