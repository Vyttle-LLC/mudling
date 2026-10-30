//! Plugins: all of MUDling's game knowledge (PRD §11).
//!
//! The core knows no game. A plugin is a folder with a `plugin.toml`
//! manifest ([`manifest`]) and one TOML file for each extension point it
//! extends ([`points`]). The [`PluginHost`] compiles every enabled plugin's
//! line rules into one merged matcher, and open tables' rows into another,
//! so a line costs one pass however many plugins are on, and one more inside
//! a table. It delivers matches to a [`PluginSink`] as [`PluginEvent`]s.

mod host;
pub mod luau;
pub mod manifest;
mod matcher;
pub mod points;

pub use host::{
    DEFAULT_COST_SAMPLING, PendingKind, PluginCost, PluginEvent, PluginHost, PluginSink, Settings,
};
pub use manifest::{Layer, Manifest, Plugin, PluginError, PluginId, Problem, ServerInfo};
pub use points::{Capture, MapSignal, Pattern, PatternError, Point, Points};
