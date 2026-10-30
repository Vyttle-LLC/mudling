//! What the player types, kept only to help them type it again: the last 500
//! commands for ↑ recall, the words the game has shown, and Tab completion
//! over both. Nothing typed while echo is off is ever kept (PRD §9).

mod history;
mod known_words;
mod store;
mod tab;

pub use history::{HISTORY_LIMIT, History};
pub use known_words::KnownWords;
pub use store::{HISTORY_FILE_NAME, HistoryWriter, default_history_path, load_history};
pub use tab::{Completer, Completion, words_of};
