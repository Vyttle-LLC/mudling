//! Watches the plugin folders the player added, so that saving a file in
//! one reloads it. The OS reports each change to a thread `notify` runs,
//! and nothing here runs while nothing changes, so a watched folder adds
//! nothing to the cost of reading a line.

use super::Shell;
use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui::{Context, Task};
use mudling_core::registry::AddedFolder;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long changes settle before a reload: an editor's save is often
/// several writes.
const SETTLE: Duration = Duration::from_millis(200);

pub(super) struct FolderWatch {
    watcher: RecommendedWatcher,
    /// Each folder watched, with the path the OS may report it by.
    watched: Vec<(PathBuf, PathBuf)>,
    _changes: Task<()>,
}

impl FolderWatch {
    fn start(cx: &mut Context<Shell>) -> notify::Result<FolderWatch> {
        let (changed, mut changes) = unbounded::<PathBuf>();
        let watcher = notify::recommended_watcher(report_to(changed))?;
        let reloads = cx.spawn(async move |shell, cx| {
            while let Some(first) = changes.next().await {
                cx.background_executor().timer(SETTLE).await;
                let mut paths = vec![first];
                while let Ok(path) = changes.try_recv() {
                    paths.push(path);
                }
                let reloaded = shell.update(cx, |shell, cx| shell.reload_changed(&paths, cx));
                if reloaded.is_err() {
                    return;
                }
            }
        });
        Ok(FolderWatch {
            watcher,
            watched: Vec::new(),
            _changes: reloads,
        })
    }

    /// Watches exactly `folders`.
    fn watch(&mut self, folders: &[PathBuf]) {
        let gone: Vec<PathBuf> = (self.watched.iter())
            .map(|(folder, _)| folder.clone())
            .filter(|folder| !folders.contains(folder))
            .collect();
        for folder in &gone {
            let _ = self.watcher.unwatch(folder);
        }
        self.watched.retain(|(folder, _)| !gone.contains(folder));
        for folder in folders {
            if self.watched.iter().any(|(watched, _)| watched == folder) {
                continue;
            }
            match self.watcher.watch(folder, RecursiveMode::Recursive) {
                Ok(()) => {
                    let real = folder.canonicalize().unwrap_or_else(|_| folder.clone());
                    self.watched.push((folder.clone(), real));
                }
                Err(error) => eprintln!("couldn't watch {}: {error}", folder.display()),
            }
        }
    }

    /// The watched folders `path` is in.
    fn folders_of(&self, path: &Path) -> impl Iterator<Item = &PathBuf> {
        (self.watched.iter())
            .filter(move |(folder, real)| path.starts_with(folder) || path.starts_with(real))
            .map(|(folder, _)| folder)
    }
}

/// Sends each changed path from `notify`'s thread.
fn report_to(changed: UnboundedSender<PathBuf>) -> impl notify::EventHandler {
    move |event: notify::Result<notify::Event>| {
        for path in event.map(|event| event.paths).unwrap_or_default() {
            let _ = changed.unbounded_send(path);
        }
    }
}

impl Shell {
    /// Watches the folders in `added`, and nothing else.
    pub(super) fn watch_added_folders(&mut self, added: &[AddedFolder], cx: &mut Context<Shell>) {
        let folders: Vec<PathBuf> = added.iter().map(|added| added.path.clone()).collect();
        if folders.is_empty() {
            self.folder_watch = None;
            return;
        }
        if self.folder_watch.is_none() {
            match FolderWatch::start(cx) {
                Ok(watch) => self.folder_watch = Some(watch),
                Err(error) => {
                    eprintln!("couldn't watch plugin folders: {error}");
                    return;
                }
            }
        }
        if let Some(watch) = &mut self.folder_watch {
            watch.watch(&folders);
        }
    }

    /// Reloads each added folder a path in `changed` is in.
    fn reload_changed(&mut self, changed: &[PathBuf], cx: &mut Context<Shell>) {
        let Some(watch) = &self.folder_watch else {
            return;
        };
        let mut folders: Vec<PathBuf> = Vec::new();
        for folder in changed.iter().flat_map(|path| watch.folders_of(path)) {
            if !folders.contains(folder) {
                folders.push(folder.clone());
            }
        }
        if !folders.is_empty() {
            self.reload_folders(folders, cx);
        }
    }
}
