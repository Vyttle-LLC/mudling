//! Sync: SQLite stays the working store, and one thread keeps a git repo up to
//! date with it (PRD §10). It never blocks play and never loses data.
//!
//! [`Syncer`] is a cheap handle a shell or session holds. Every call only
//! queues work, so nothing here touches a shell's thread. The sync thread owns
//! the [`git::Repo`](crate::git::Repo) for as long as a repo is set, and
//! publishes [`SyncState`] through its own hub, which a shell reads as it does
//! a session's stores.
//!
//! ```no_run
//! use mudling_core::sync::{RepoSettings, SyncConfig, Syncer};
//! use mudling_core::session::{Session, SessionConfig};
//! # fn demo() -> std::io::Result<()> {
//! mudling_core::git::prepare(); // first thing in main
//! let mut config = SyncConfig::new("/data");
//! config.repo = Some(RepoSettings::new("/data/repo"));
//! let sync = Syncer::start(config, || {})?;
//! let session = Session::open(
//!     mudling_core::session::SessionConfig {
//!         sync: Some(sync.clone()),
//!         ..SessionConfig::new("/data", "mud.example.org", 4000)
//!     },
//!     || {},
//! )?;
//! sync.sync_now();
//! session.quit().wait().ok();
//! sync.quit().wait();
//! # Ok(())
//! # }
//! ```
//!
//! **When.** A [`Pass::Commit`] runs every 10 minutes of play. A [`Pass::Pull`]
//! runs on connecting and on coming to the front. A [`Pass::Full`] runs at
//! launch, at the end of a session, on Sync now and at quit. Quit waits about
//! 2 seconds, then lets go: everything is in SQLite, so the next launch has it.
//!
//! **Order.** Each pass goes as far as it needs along one line:
//!
//! 1. Take in a vault left in the data folder.
//! 2. [`merge::join`](crate::merge::join) any database that hasn't seen the
//!    branch, since an export would delete what it hasn't read.
//! 3. Export, and `commit_all(merge::CHECKPOINT)`.
//! 4. Fetch, then merge with a live session's rows held.
//! 5. Squash the session, and push.
//!
//! **A kill at any point loses nothing.** SQLite commits each batch whole, a
//! file reaches the disk whole, and a branch moves in one step, so each kill
//! leaves one of these:
//!
//! | Killed | Left behind | Next sync |
//! | --- | --- | --- |
//! | during export | some files new, none half | writes the rest |
//! | before or during a commit | uncommitted files, maybe a lock | clears the lock, commits |
//! | during a fetch | a tracking ref old or new | fetches again |
//! | after a database forgot what it saw | nothing else | joins, which changes nothing |
//! | during the merge's files | a folder ahead of SQLite | joins, so export deletes none of it; merges again |
//! | after the import | SQLite ahead of the branch | joins, merges again, which changes nothing |
//! | after the branch moved | the commit unmarked as seen | joins, which changes nothing |
//! | during a squash | a commit no branch holds | squashes again |
//! | during or after a push | the remote old or new, whole | fetches, which shows which |
//! | moving the vault | it in both places | merges them, removes the old copy once committed |
//!
//! **Waiting.** Only fetch and push reach the network, each under a
//! [`Limit`](crate::git::Limit), and sync goes on when one gives up. What can
//! pass is retried with growing gaps, and the player is told; what needs the
//! player waits for the next sync.

mod pass;
mod run;
mod summary;
mod vault;

#[cfg(test)]
mod tests;

pub use crate::store::SyncState;
pub use pass::Pass;

use crate::repo::AppSettings;
use crate::session::SessionLink;
use crate::store::StoreHub;
use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

/// How often a session's changes are committed while playing (PRD §10).
const COMMIT_EVERY: Duration = Duration::from_secs(10 * 60);
/// How long one fetch or push may take.
const NETWORK_LIMIT: Duration = Duration::from_secs(60);
/// How long quit waits for the last push (PRD §10).
const QUIT_WAIT: Duration = Duration::from_secs(2);
const FIRST_RETRY: Duration = Duration::from_secs(30);
const DEFAULT_REMOTE: &str = "origin";

/// The repo a player chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepoSettings {
    /// A git repo, or an empty folder to start one in.
    pub folder: PathBuf,
    /// The remote to sync with. A repo without it still gets commits.
    pub remote: String,
}

impl RepoSettings {
    pub fn new(folder: impl Into<PathBuf>) -> RepoSettings {
        RepoSettings {
            folder: folder.into(),
            remote: DEFAULT_REMOTE.to_owned(),
        }
    }
}

/// What sync works on. The caller keeps these and passes them in each launch.
#[derive(Clone, Debug)]
pub struct SyncConfig {
    /// The folder sessions use: databases under `servers`, and the vault
    /// while there's no repo.
    pub data_folder: PathBuf,
    /// `None` keeps everything local.
    pub repo: Option<RepoSettings>,
    pub settings: AppSettings,
    pub commit_every: Duration,
    pub network_limit: Duration,
    pub quit_wait: Duration,
    /// The first gap before trying again; later gaps double. A lock gets a
    /// tenth of it each time, since it's usually gone in a moment.
    pub first_retry: Duration,
}

impl SyncConfig {
    pub fn new(data_folder: impl Into<PathBuf>) -> SyncConfig {
        SyncConfig {
            data_folder: data_folder.into(),
            repo: None,
            settings: AppSettings::default(),
            commit_every: COMMIT_EVERY,
            network_limit: NETWORK_LIMIT,
            quit_wait: QUIT_WAIT,
            first_retry: FIRST_RETRY,
        }
    }
}

enum Command {
    Attach(SessionLink),
    Connected,
    /// Carries the signal that the session's last rows are in the database.
    Disconnected(Receiver<()>),
    Foreground,
    SyncNow,
    SetRepo(RepoSettings),
    ClearRepo,
    Settings(AppSettings),
    Quit,
}

/// A handle to the sync thread. Clones share it. Every call returns at once,
/// and the store shows how it goes.
#[derive(Clone)]
pub struct Syncer {
    hub: Arc<StoreHub>,
    commands: Sender<Command>,
    /// Raised at quit, so a network step in progress stops waiting.
    quitting: Arc<AtomicBool>,
    stopped: Arc<Mutex<Receiver<()>>>,
    quit_wait: Duration,
}

impl fmt::Debug for Syncer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Syncer")
    }
}

/// Sync on its way out; see [`Syncer::quit`].
pub struct SyncQuitting {
    stopped: Arc<Mutex<Receiver<()>>>,
    wait: Duration,
}

impl SyncQuitting {
    /// Waits for the last commit and push, for about 2 seconds at most. This
    /// blocks: call it on the way out of the app, off the UI thread.
    pub fn wait(self) {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = stopped.recv_timeout(self.wait);
    }
}

impl Syncer {
    /// Starts the sync thread, which syncs at once: that's the pull at launch,
    /// and the push of whatever was left. `waker` runs on that thread when
    /// [`SyncState`] changes, so a shell schedules a frame.
    pub fn start(
        config: SyncConfig,
        waker: impl Fn() + Send + Sync + 'static,
    ) -> io::Result<Syncer> {
        let hub = Arc::new(StoreHub::new(waker));
        let (commands, inbox) = mpsc::channel();
        let quitting = Arc::new(AtomicBool::new(false));
        let (done, stopped) = mpsc::channel();
        let quit_wait = config.quit_wait;
        let thread = run::Thread::new(config, Arc::clone(&hub), Arc::clone(&quitting));
        thread::Builder::new()
            .name("mudling-sync".to_owned())
            .spawn(move || {
                thread.run(&inbox);
                let _ = done.send(());
            })?;
        Ok(Syncer {
            hub,
            commands,
            quitting,
            stopped: Arc::new(Mutex::new(stopped)),
            quit_wait,
        })
    }

    /// The store: subscribe to [`SyncState`], then drain once per frame.
    pub fn stores(&self) -> &StoreHub {
        &self.hub
    }

    /// The player pressed Sync now: commit, pull and push.
    pub fn sync_now(&self) {
        self.send(Command::SyncNow);
    }

    /// The app came to the front.
    pub fn foreground(&self) {
        self.send(Command::Foreground);
    }

    /// Uses this repo from now on, bringing this device's data and vault into
    /// it. What it may hold from another device merges by record.
    pub fn set_repo(&self, repo: RepoSettings) {
        self.send(Command::SetRepo(repo));
    }

    /// Goes back to keeping everything local. The repo stays as it is.
    pub fn clear_repo(&self) {
        self.send(Command::ClearRepo);
    }

    /// The player changed a setting, which the next sync exports.
    pub fn settings_changed(&self, settings: AppSettings) {
        self.send(Command::Settings(settings));
    }

    /// Commits and pushes one last time. Returns at once; [`SyncQuitting::wait`]
    /// waits. Quit sessions first, so their last rows are on the disk.
    pub fn quit(&self) -> SyncQuitting {
        self.quitting.store(true, Ordering::Relaxed);
        self.send(Command::Quit);
        SyncQuitting {
            stopped: Arc::clone(&self.stopped),
            wait: self.quit_wait,
        }
    }

    pub(crate) fn attach(&self, session: SessionLink) {
        self.send(Command::Attach(session));
    }

    pub(crate) fn connected(&self) {
        self.send(Command::Connected);
    }

    pub(crate) fn disconnected(&self, last_rows_in: Receiver<()>) {
        self.send(Command::Disconnected(last_rows_in));
    }

    fn send(&self, command: Command) {
        // A sync thread that has stopped has nothing left to do with it.
        let _ = self.commands.send(command);
    }
}

/// Places a test can kill the process, as `kill -9` would: no destructor runs.
/// Everything here is empty outside tests.
pub(crate) mod kill {
    #[cfg(test)]
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The step to die at, for a child process of a test.
    #[cfg(test)]
    pub(crate) const STEP: &str = "MUDLING_TEST_KILL_AT";
    /// How a killed child exits, so a test can tell it from a failure.
    #[cfg(test)]
    pub(crate) const EXIT_CODE: i32 = 77;

    #[cfg(test)]
    static ONE_FILE: AtomicBool = AtomicBool::new(false);

    #[cfg(test)]
    fn is_step(step: &str) -> bool {
        std::env::var(STEP).is_ok_and(|wanted| wanted == step)
    }

    #[cfg(test)]
    pub(crate) fn point(step: &str) {
        if is_step(step) {
            std::process::exit(EXIT_CODE);
        }
    }

    /// Dies once the next file is written, if this is the step.
    #[cfg(test)]
    pub(crate) fn arm_after_one_file(step: &str) {
        ONE_FILE.store(is_step(step), Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn file_written() {
        if ONE_FILE.load(Ordering::Relaxed) {
            std::process::exit(EXIT_CODE);
        }
    }

    #[cfg(not(test))]
    pub(crate) fn point(_step: &str) {}

    #[cfg(not(test))]
    pub(crate) fn arm_after_one_file(_step: &str) {}

    #[cfg(not(test))]
    pub(crate) fn file_written() {}
}
