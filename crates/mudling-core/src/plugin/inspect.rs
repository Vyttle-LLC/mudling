//! The plugin inspector's record: which plugins claimed a line, and how long
//! each took (PRD §11).
//!
//! A plugin claims a line by matching it: an event, a table's row or its
//! coloring. [`Clocked`] stands between the host and the real sink, so nothing
//! is timed twice and no rule runs again: a claim's time runs from the one
//! before it, which is that rule's own work. Only a line read with the
//! inspector open goes through it. Otherwise the host never sees it, so a
//! closed inspector costs each line one branch.

use super::host::{PluginEvent, PluginSink};
use super::manifest::PluginId;
use crate::line::Line;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What one plugin did with a line.
#[derive(Clone, Debug, PartialEq)]
pub struct Claim {
    pub plugin: PluginId,
    /// The events it raised, in order; none if it only colored the line.
    pub events: Vec<Arc<str>>,
    pub took: Duration,
}

/// A line the chat showed, and what claimed it.
#[derive(Clone, Debug, PartialEq)]
pub struct InspectedLine {
    pub line: Line,
    /// Nothing, when no plugin claimed it.
    pub claims: Vec<Claim>,
    /// The whole read, with the one pass that no plugin pays for alone.
    pub took: Duration,
}

/// Passes events on to `sink`, noting who sent each and when.
pub struct Clocked<'a, S> {
    sink: &'a mut S,
    last: Instant,
    claims: Vec<Claim>,
}

impl<'a, S> Clocked<'a, S> {
    pub fn new(sink: &'a mut S) -> Clocked<'a, S> {
        Clocked {
            sink,
            last: Instant::now(),
            claims: Vec::new(),
        }
    }

    pub fn into_claims(self) -> Vec<Claim> {
        self.claims
    }

    /// The plugin's claim, charged the time since the last one.
    fn claim(&mut self, plugin: &PluginId) -> &mut Claim {
        let now = Instant::now();
        let took = now - self.last;
        self.last = now;
        let at = self.claims.iter().position(|c| c.plugin == *plugin);
        let at = at.unwrap_or_else(|| {
            self.claims.push(Claim {
                plugin: plugin.clone(),
                events: Vec::new(),
                took: Duration::ZERO,
            });
            self.claims.len() - 1
        });
        let claim = &mut self.claims[at];
        claim.took += took;
        claim
    }
}

impl<S: PluginSink> PluginSink for Clocked<'_, S> {
    fn event(&mut self, event: PluginEvent) {
        let name = event.name.clone();
        self.claim(&event.plugin.clone()).events.push(name);
        self.sink.event(event);
    }

    fn passed(&mut self) {
        self.last = Instant::now();
    }

    fn colored(&mut self, plugin: &PluginId) {
        self.claim(plugin);
    }
}
