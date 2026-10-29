//! MUDling's core: everything except drawing. It holds no UI types, so every
//! shell (gpui on the desktop, SwiftUI through UniFFI on iOS) shares it.

pub mod ansi;
pub mod replay;
pub mod scrollback;
pub mod stats;
pub mod store;

pub use ansi::{AnsiParser, Color, Line, LineKind, Span, Style};
pub use scrollback::Scrollback;
