//! The history file: this device only, never synced, logged or exported.
//!
//! It is one small file of its own in the OS data folder, apart from every
//! other store, so a repo export or sync can leave it out by name or by
//! folder. Writes happen on a worker thread; the caller only enqueues a copy
//! of the commands.

use super::History;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

pub const HISTORY_FILE_NAME: &str = "command-history.json";

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    commands: Vec<String>,
}

/// Where the history file lives in the OS data folder, or `None` when the
/// OS names no home folder.
pub fn default_history_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "Vyttle", "MUDling").map(|dirs| dirs.data_dir().join(HISTORY_FILE_NAME))
}

/// Reads the history file, or an empty history if there is none yet.
pub fn load_history(path: &Path) -> io::Result<History> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(History::new()),
        Err(error) => return Err(error),
    };
    let file: HistoryFile = serde_json::from_str(&text).map_err(io::Error::other)?;
    Ok(History::from_entries(file.commands))
}

/// Saves the history file from a worker thread.
#[derive(Debug)]
pub struct HistoryWriter {
    queue: Sender<Vec<String>>,
    worker: JoinHandle<io::Result<()>>,
}

impl HistoryWriter {
    pub fn new(path: PathBuf) -> Self {
        let (queue, snapshots) = mpsc::channel::<Vec<String>>();
        let worker = thread::spawn(move || {
            let mut outcome = Ok(());
            while let Ok(mut commands) = snapshots.recv() {
                // Only the newest snapshot is worth writing.
                while let Ok(newer) = snapshots.try_recv() {
                    commands = newer;
                }
                outcome = write_atomically(&path, &commands);
            }
            outcome
        });
        HistoryWriter { queue, worker }
    }

    /// Queues the history to be saved, and returns at once.
    pub fn save(&self, history: &History) {
        // The worker only stops once this writer is dropped.
        let _ = self.queue.send(history.entries().to_vec());
    }

    /// Waits for the queued saves to finish. Returns the last save's error,
    /// if it failed.
    pub fn finish(self) -> io::Result<()> {
        drop(self.queue);
        self.worker.join().unwrap_or_else(|_| Err(io::Error::other("the history writer panicked")))
    }
}

fn write_atomically(path: &Path, commands: &[String]) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let json = serde_json::to_string(&HistoryFile { commands: commands.to_vec() }).map_err(io::Error::other)?;
    let partial = path.with_extension("json.tmp");
    std::fs::write(&partial, json)?;
    std::fs::rename(&partial, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A folder of its own under the OS temp folder, removed afterwards.
    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let name = format!("mudling-history-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            TempFolder(std::env::temp_dir().join(name))
        }

        fn file(&self) -> PathBuf {
            self.0.join(HISTORY_FILE_NAME)
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_file_round_trips() {
        let folder = TempFolder::new();
        let mut history = History::new();
        history.record("look", false);
        history.record("cast 'bless' me", false);
        let writer = HistoryWriter::new(folder.file());
        writer.save(&history);
        writer.finish().unwrap();
        assert_eq!(load_history(&folder.file()).unwrap().entries(), history.entries());
    }

    #[test]
    fn a_missing_file_is_an_empty_history() {
        let folder = TempFolder::new();
        assert!(load_history(&folder.file()).unwrap().entries().is_empty());
    }

    #[test]
    fn a_damaged_file_is_an_error() {
        let folder = TempFolder::new();
        std::fs::create_dir_all(&folder.0).unwrap();
        std::fs::write(folder.file(), "not json").unwrap();
        assert!(load_history(&folder.file()).is_err());
    }

    #[test]
    fn echo_off_input_never_reaches_the_disk() {
        let folder = TempFolder::new();
        let mut history = History::new();
        history.record("connect mike", false);
        history.record("hunter2-the-password", true);
        history.record("look", false);
        let writer = HistoryWriter::new(folder.file());
        writer.save(&history);
        writer.finish().unwrap();
        let on_disk = std::fs::read_to_string(folder.file()).unwrap();
        assert!(!on_disk.contains("hunter2"));
        assert!(on_disk.contains("connect mike"));
    }

    #[test]
    fn the_newest_save_wins() {
        let folder = TempFolder::new();
        let writer = HistoryWriter::new(folder.file());
        let mut history = History::new();
        for command in ["a", "b", "c"] {
            history.record(command, false);
            writer.save(&history);
        }
        writer.finish().unwrap();
        assert_eq!(load_history(&folder.file()).unwrap().entries(), ["a", "b", "c"]);
    }

    #[test]
    fn a_failed_save_is_reported_on_finish() {
        let folder = TempFolder::new();
        std::fs::create_dir_all(&folder.0).unwrap();
        // A file where the history's folder should be.
        let blocker = folder.0.join("blocker");
        std::fs::write(&blocker, "").unwrap();
        let writer = HistoryWriter::new(blocker.join(HISTORY_FILE_NAME));
        writer.save(&History::new());
        assert!(writer.finish().is_err());
    }
}
