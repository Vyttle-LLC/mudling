//! MUDling's core: everything except drawing. It holds no UI types, so every
//! shell (gpui on the desktop, SwiftUI through UniFFI on iOS) shares it.

pub mod ansi;
pub mod completion;
pub mod defaults;
pub mod git;
pub mod line;
pub mod map;
pub mod merge;
pub mod net;
pub mod persist;
pub mod plugin;
pub mod replay;
pub mod repo;
pub mod scrollback;
pub mod session;
pub mod stats;
pub mod store;
pub mod sync;
pub mod telnet;
pub mod vault;

pub use ansi::{AnsiParser, Color, Span, Style};
pub use line::{Line, LineKind, LinePipeline};
pub use scrollback::Scrollback;

use std::path::PathBuf;

/// The OS data folder MUDling keeps its files in, or `None` when the OS names
/// no home folder.
pub fn data_folder() -> Option<PathBuf> {
    directories::ProjectDirs::from("com", "Vyttle", "MUDling")
        .map(|dirs| dirs.data_dir().to_owned())
}
