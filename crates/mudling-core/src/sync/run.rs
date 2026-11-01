//! The sync thread: when to run a pass, and what to do when one fails.

use super::pass::{Failure, Pass, Repository};
use super::{Command, RepoSettings, Sessions, SyncConfig, SyncState, vault};
use crate::git::{GitError, Limit};
use crate::merge;
use crate::persist::Timestamp;
use crate::store::{StoreHub, Versioned};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

/// How many tries a lock gets before the player is told who may hold it.
const QUIET_LOCK_TRIES: u32 = 3;
/// The longest gap between tries while the remote can't be reached.
const LONGEST_RETRY: Duration = Duration::from_secs(15 * 60);
/// How many times one pass goes around for what the next try clears up: a
/// push that lost a race, or a vault saved mid-sync.
const QUICK_TRIES: usize = 3;
/// How long a disconnect waits for the session's last rows. Rows later than
/// this go with the next sync.
const LAST_ROWS_WAIT: Duration = Duration::from_secs(5);
/// How libssh2 words an SSH server missing from `known_hosts`.
const UNKNOWN_HOST: &str = "hostkey";

pub(super) struct Thread {
    config: SyncConfig,
    hub: Arc<StoreHub>,
    quitting: Arc<AtomicBool>,
    repo: Option<Repository>,
    sessions: Sessions,
    /// Connections open now.
    playing: usize,
    /// When the session being played began, for its squashed commit.
    session_began: Option<Instant>,
    next_commit: Option<Instant>,
    wanted: Option<Pass>,
    retry: Option<(Instant, Pass)>,
    /// The furthest pass that failed and hasn't worked since. A shorter pass
    /// working says nothing about it, so its problem stays on show.
    unfinished: Option<Pass>,
    /// Passes failed in a row, for spacing out tries.
    failures: u32,
}

impl Thread {
    pub(super) fn new(
        config: SyncConfig,
        hub: Arc<StoreHub>,
        sessions: Sessions,
        quitting: Arc<AtomicBool>,
    ) -> Thread {
        Thread {
            config,
            hub,
            quitting,
            repo: None,
            sessions,
            playing: 0,
            session_began: None,
            next_commit: None,
            wanted: None,
            retry: None,
            unfinished: None,
            failures: 0,
        }
    }

    pub(super) fn run(mut self, inbox: &Receiver<Command>) {
        let settings = self.config.settings.clone();
        self.publish(|state| state.settings = settings);
        self.publish_vault_file();
        if let Some(repo) = self.config.repo.clone() {
            self.open_repo(repo);
        }
        loop {
            let mut quit = false;
            if self.wanted.is_none() {
                match inbox.recv_timeout(self.until_next_wake()) {
                    Ok(command) => quit |= self.obey(command),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            while let Ok(command) = inbox.try_recv() {
                quit |= self.obey(command);
            }
            if quit {
                self.run_pass(Pass::Full, Limit::after(self.config.quit_wait));
                return;
            }
            self.notice_timers();
            if let Some(pass) = self.wanted.take() {
                let limit =
                    Limit::stopped_by(self.config.network_limit, Arc::clone(&self.quitting));
                self.run_pass(pass, limit);
            }
        }
    }

    /// Acts on `command`. Returns whether it's time to stop.
    fn obey(&mut self, command: Command) -> bool {
        match command {
            Command::Connected => {
                self.playing += 1;
                self.session_began.get_or_insert_with(Instant::now);
                self.next_commit
                    .get_or_insert_with(|| Instant::now() + self.config.commit_every);
                self.want(Pass::Pull);
            }
            Command::Disconnected(last_rows_in) => {
                let _ = last_rows_in.recv_timeout(LAST_ROWS_WAIT);
                self.playing = self.playing.saturating_sub(1);
                if self.playing == 0 {
                    self.next_commit = None;
                }
                self.want(Pass::Full);
            }
            Command::Foreground => self.want(Pass::Pull),
            Command::SyncNow => {
                self.failures = 0;
                self.want(Pass::Full);
            }
            Command::SetRepo(repo) => {
                if self.bring_vault_home() {
                    self.forget_failures();
                    self.config.repo = Some(repo.clone());
                    self.open_repo(repo);
                }
            }
            Command::ClearRepo => self.clear_repo(),
            Command::Settings(settings) => self.config.settings = settings,
            Command::Quit => return true,
        }
        false
    }

    fn want(&mut self, pass: Pass) {
        self.wanted = self.wanted.max(Some(pass));
    }

    fn until_next_wake(&self) -> Duration {
        let retry = self.retry.map(|(at, _)| at);
        [self.next_commit, retry]
            .into_iter()
            .flatten()
            .min()
            .map_or(LONGEST_RETRY, |at| {
                at.saturating_duration_since(Instant::now())
            })
    }

    fn notice_timers(&mut self) {
        let now = Instant::now();
        if self.next_commit.is_some_and(|at| at <= now) {
            self.next_commit = Some(now + self.config.commit_every);
            self.want(Pass::Commit);
        }
        if let Some((at, pass)) = self.retry
            && at <= now
        {
            self.retry = None;
            self.want(pass);
        }
    }

    fn open_repo(&mut self, settings: RepoSettings) {
        self.repo = None;
        match Repository::open(settings, &self.config.data_folder) {
            Ok(repo) => {
                self.repo = Some(repo);
                self.want(Pass::Full);
            }
            Err(error) => self.failed(&Failure::Git(error), Pass::Full),
        }
        self.publish_vault_file();
    }

    fn clear_repo(&mut self) {
        if !self.bring_vault_home() {
            return;
        }
        self.repo = None;
        self.config.repo = None;
        self.forget_failures();
        self.wanted = None;
        self.publish(|state| {
            state.problem = None;
            state.syncing = false;
            state.unsynced = 0;
        });
        self.publish_vault_file();
    }

    /// Copies the vault out of the repo being left, whether or not it opened,
    /// so the next place can take it in. Returns whether the player can go on:
    /// if not, the store says why and the repo stays set.
    fn bring_vault_home(&self) -> bool {
        let Some(repo) = &self.config.repo else {
            return true;
        };
        let moved = vault::bring_home(&repo.folder, &self.config.data_folder);
        let failed = moved.as_ref().err().map(|error| {
            format!("The vault couldn't be moved out of the repo, so it stays set: {error}.")
        });
        let stayed = failed.is_some();
        if stayed {
            self.publish(|state| state.problem = failed);
        }
        !stayed
    }

    fn forget_failures(&mut self) {
        self.retry = None;
        self.unfinished = None;
        self.failures = 0;
    }

    fn run_pass(&mut self, pass: Pass, limit: Limit) {
        if self.repo.is_none() {
            let Some(settings) = self.config.repo.clone() else {
                return;
            };
            self.open_repo(settings);
            self.wanted = None;
        }
        let Some(mut repo) = self.repo.take() else {
            return;
        };
        self.publish(|state| state.syncing = true);
        let mut outcome = Err(Failure::Git(GitError::Failed("no try ran".to_owned())));
        for _ in 0..QUICK_TRIES {
            outcome = repo.run(
                pass,
                &mut self.config.settings,
                &self.sessions,
                self.session_began,
                &limit,
            );
            let again = matches!(
                &outcome,
                Err(Failure::Merge(merge::Error::VaultNotCommitted))
                    | Err(Failure::Git(GitError::NonFastForward(_)))
            );
            if !again {
                break;
            }
        }
        let unsynced = repo.unsynced();
        self.repo = Some(repo);
        match outcome {
            Ok(done) => {
                let finished = self.unfinished.is_none_or(|failed| pass >= failed);
                if finished {
                    self.forget_failures();
                } else if done.reached_remote
                    && let Some((_, waiting)) = self.retry.take()
                {
                    // The remote answered, so what's waiting needn't.
                    self.want(waiting);
                }
                if pass == Pass::Full && self.playing == 0 {
                    self.session_began = None;
                }
                let settings = self.config.settings.clone();
                self.publish(|state| {
                    state.syncing = false;
                    if finished {
                        state.problem = None;
                    }
                    state.last_sync = done.reached_remote.then(Timestamp::now).or(state.last_sync);
                    state.changed = done.changed.or(state.changed.take());
                    state.unsynced = unsynced;
                    state.settings = settings;
                });
            }
            Err(failure) => {
                self.publish(|state| state.unsynced = unsynced);
                self.failed(&failure, pass);
            }
        }
    }

    /// Tells the player, and sets up the next try if one can help.
    fn failed(&mut self, failure: &Failure, pass: Pass) {
        self.failures += 1;
        let wait = self.wait_before_retry(failure);
        let pass = self.unfinished.map_or(pass, |earlier| earlier.max(pass));
        self.unfinished = Some(pass);
        // A shorter pass failing must not cancel a further one still waiting.
        let waiting = self.retry.map(|(at, _)| at);
        self.retry = wait
            .map(|wait| Instant::now() + wait)
            .or(waiting)
            .map(|at| (at, pass));
        let settings = self.config.settings.clone();
        let locks = self
            .repo
            .as_ref()
            .map(Repository::locks)
            .unwrap_or_default();
        let problem = plain_words(failure, wait, self.failures, &locks);
        self.publish(|state| {
            state.syncing = false;
            state.problem = Some(problem);
            state.settings = settings;
        });
    }

    fn wait_before_retry(&self, failure: &Failure) -> Option<Duration> {
        let first = self.config.first_retry;
        let growing = first
            .saturating_mul(2u32.saturating_pow(self.failures.saturating_sub(1)))
            .min(LONGEST_RETRY);
        match failure {
            Failure::Git(GitError::Locked(_)) => Some(first / 10),
            Failure::Merge(merge::Error::VaultNotCommitted) => Some(first / 10),
            Failure::Git(
                GitError::NoNetwork(_)
                | GitError::Unavailable(_)
                | GitError::TimedOut(_)
                | GitError::NonFastForward(_),
            ) => Some(growing),
            _ => None,
        }
    }

    fn publish(&self, change: impl FnOnce(&mut SyncState)) {
        let mut state = self
            .hub
            .read(|state: &Versioned<SyncState>| state.value().clone());
        change(&mut state);
        self.hub.write::<SyncState>(state);
    }

    fn publish_vault_file(&self) {
        let folder = self.repo.as_ref().map(Repository::folder);
        let vault_file = vault::file_in(folder.unwrap_or(&self.config.data_folder));
        let repo = folder.map(Path::to_owned);
        self.publish(|state| {
            state.vault_file = vault_file;
            state.repo = repo;
        });
    }
}

/// A failure as the player reads it. Git's own words come after ours: a
/// refused push names the field or line, and never holds the value.
fn plain_words(failure: &Failure, wait: Option<Duration>, tries: u32, locks: &[PathBuf]) -> String {
    let retry = match wait {
        Some(wait) => format!(" Trying again in {}.", in_words(wait)),
        None => String::new(),
    };
    match failure {
        Failure::Git(GitError::Locked(detail)) if tries > QUIET_LOCK_TRIES => match locks {
            [lock, ..] => format!(
                "Another git program left a lock on this repo. If none is running, delete {} and sync will carry on.{retry}",
                lock.display()
            ),
            [] => format!("The repo is locked: {detail}.{retry}"),
        },
        Failure::Git(GitError::Locked(_)) => format!("The repo is busy.{retry}"),
        Failure::Git(GitError::NoNetwork(detail)) => {
            format!("Can't reach the remote ({detail}). Your data is safe here.{retry}")
        }
        Failure::Git(GitError::Unavailable(detail)) => {
            format!("The remote is having trouble ({detail}). Your data is safe here.{retry}")
        }
        Failure::Git(GitError::TimedOut(_)) => {
            format!(
                "The remote or its sign-in didn't answer in time. Your data is safe here.{retry}"
            )
        }
        Failure::Git(GitError::AuthRefused(detail)) if detail.contains(UNKNOWN_HOST) => {
            "This device doesn't know the remote's server yet. Connect to it once with ssh, then sync again. Sync waits until that's done."
                .to_owned()
        }
        Failure::Git(GitError::AuthRefused(detail)) => format!(
            "The remote refused this device's sign-in: {detail}. Sync waits until that's fixed."
        ),
        Failure::Git(GitError::RemoteRefused(detail)) => {
            format!("The remote refused: {detail}. Sync waits until that's fixed.")
        }
        Failure::Git(error @ GitError::HoldsTheDataFolder) => {
            format!("{error} Sync waits until it's changed.")
        }
        Failure::Git(GitError::PasswordInFile { path, field }) => format!(
            "Nothing was pushed: {path} seems to hold a password in {field}. Take it out and sync again."
        ),
        Failure::Merge(merge::Error::Persist(error)) => {
            format!("Sync stopped and the branch didn't move, because a save failed: {error}.")
        }
        Failure::Merge(error) => format!("Sync stopped: {error}.{retry}"),
        Failure::Git(error) => format!("Sync stopped: {error}.{retry}"),
    }
}

fn in_words(wait: Duration) -> String {
    match wait.as_secs() {
        seconds @ 0..=89 => format!("{seconds} seconds"),
        seconds => format!("{} minutes", seconds.div_ceil(60)),
    }
}
