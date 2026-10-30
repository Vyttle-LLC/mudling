//! The password vault: saved server passwords, encrypted in `secrets.vault`
//! and safe to sync through a git repo (PRD §10). `file.rs` documents the format.
//!
//! A random vault key encrypts each password with XChaCha20-Poly1305. The key
//! is stored twice in the file: under a key Argon2id derives from the master
//! password, and under a recovery key shown once. Each device also caches it
//! in its OS credential store, behind [`KeyStore`].
//!
//! [`Vault`] is a cheap, cloneable handle to a thread of its own, and every
//! call is an `async fn` that waits on that thread, so no caller ever does
//! cryptography or file I/O itself. Nothing needs a tokio runtime, so the UI
//! can await these from its own executor. Dropping the last handle stops the
//! thread and wipes the key.
//!
//! **Cost.** One unlock with the master password runs Argon2id at 64 MiB and
//! three passes on one core: a few hundred milliseconds at most on an Apple
//! Silicon Mac in a release build, depending on load, and creating a vault or
//! changing its password costs the same. It runs on the vault's thread, so it
//! stalls neither the UI nor a core thread; other vault calls queue behind it.
//! Peak memory is the 64 MiB. Unlocking from the device cache (around a
//! millisecond or less) or a recovery key skips Argon2. To measure it here:
//! `cargo run --release -p mudling-core --example vault_kdf_cost`.
//!
//! **Locked vaults.** [`Vault::password`] and the other entry calls wait until
//! the vault is unlocked, however long that takes, and return once it is.
//! Auto-login awaits [`Vault::password`]; dropping the future stops the wait.
//!
//! ```no_run
//! use mudling_core::vault::{AutoLock, EntryKey, OsKeyStore, Vault};
//! use std::sync::Arc;
//!
//! # async fn example() -> Result<(), mudling_core::vault::Error> {
//! let path = Vault::default_path().expect("the OS has a data folder");
//! let vault = Vault::open(path, Arc::new(OsKeyStore), AutoLock::default());
//! vault.unlock_with_password("a long and unusual passphrase".into()).await?;
//! let entry = EntryKey::new("mud.example.org", "Fikus");
//! let password = vault.password(&entry).await?;
//! # Ok(())
//! # }
//! ```

mod crypto;
mod error;
mod file;
mod key_store;
mod merge;
mod secret;
mod strength;
mod worker;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use key_store::{KeyStore, KeyStoreError, MemoryKeyStore, OsKeyStore};
pub(crate) use merge::merge_files;
pub use secret::{RecoveryKey, Secret, VaultKey};

use crypto::KdfParams;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, SystemTime};
use tokio::sync::{oneshot, watch};
use worker::{Job, Worker};

/// The vault's file name, in the data folder or at the root of a synced repo.
pub const FILE_NAME: &str = "secrets.vault";

/// Which saved password: a character on a server.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntryKey {
    pub server: String,
    pub character: String,
}

impl EntryKey {
    pub fn new(server: impl Into<String>, character: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            character: character.into(),
        }
    }
}

/// Which password survives a conflict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepPassword {
    Current,
    Stranded,
}

/// Whether a caller's request could go through right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultState {
    NotCreated,
    Locked,
    Unlocked,
}

/// How long the vault stays unlocked without being used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutoLock(Option<Duration>);

impl AutoLock {
    pub const FIVE_MINUTES: Self = Self(Some(Duration::from_secs(5 * 60)));
    pub const FIFTEEN_MINUTES: Self = Self(Some(Duration::from_secs(15 * 60)));
    pub const ONE_HOUR: Self = Self(Some(Duration::from_secs(60 * 60)));
    pub const NEVER: Self = Self(None);

    pub fn duration(self) -> Option<Duration> {
        self.0
    }
}

impl Default for AutoLock {
    fn default() -> Self {
        Self::FIFTEEN_MINUTES
    }
}

/// Whether unlocking also cached the vault key on this device. A failure here
/// doesn't stop the vault opening; it means the next unlock asks again.
#[derive(Debug)]
#[must_use]
pub enum DeviceCache {
    Saved,
    NotSaved(KeyStoreError),
}

/// What creating a vault or changing its master password hands back. The
/// recovery key exists nowhere else, so the UI must show it now.
#[derive(Debug)]
#[must_use]
pub struct RecoveryKeyIssued {
    pub recovery_key: RecoveryKey,
    pub device_cache: DeviceCache,
}

/// A handle to a vault file and its thread. Clones share one thread.
#[derive(Clone)]
pub struct Vault {
    jobs: Sender<Job>,
    state: watch::Receiver<VaultState>,
}

impl Vault {
    /// Where `secrets.vault` lives when there's no repo.
    pub fn default_path() -> Option<PathBuf> {
        directories::ProjectDirs::from("com", "Vyttle", "MUDling")
            .map(|dirs| dirs.data_dir().join(FILE_NAME))
    }

    /// Starts the vault's thread, locked. The file needn't exist yet.
    pub fn open(path: PathBuf, key_store: Arc<dyn KeyStore>, auto_lock: AutoLock) -> Self {
        Self::with_kdf(path, key_store, auto_lock, KdfParams::DEFAULT)
    }

    fn with_kdf(
        path: PathBuf,
        key_store: Arc<dyn KeyStore>,
        auto_lock: AutoLock,
        kdf: KdfParams,
    ) -> Self {
        let (jobs, inbox) = mpsc::channel();
        let (state_sender, state) = watch::channel(VaultState::Locked);
        let worker = Worker {
            path,
            key_store,
            kdf,
            auto_lock,
            key: None,
            account: None,
            idle_deadline: None,
            state: state_sender,
            now: SystemTime::now,
        };
        thread::Builder::new()
            .name("mudling-vault".into())
            .spawn(move || worker.run(inbox))
            .expect("the OS can start a thread");
        Self { jobs, state }
    }

    pub fn state(&self) -> VaultState {
        *self.state.borrow()
    }

    /// Follows the state, so the UI can show when the vault locks or unlocks.
    pub fn subscribe(&self) -> watch::Receiver<VaultState> {
        self.state.clone()
    }

    pub async fn wait_until_unlocked(&self) -> Result<(), Error> {
        let mut state = self.state.clone();
        state
            .wait_for(|state| *state == VaultState::Unlocked)
            .await
            .map_err(|_| Error::Stopped)?;
        Ok(())
    }

    /// Creates a new vault under `master_password`, leaving it unlocked.
    pub async fn create(&self, master_password: Secret) -> Result<RecoveryKeyIssued, Error> {
        self.call(move |worker| worker.create(&master_password))
            .await
    }

    pub async fn unlock_with_password(
        &self,
        master_password: Secret,
    ) -> Result<DeviceCache, Error> {
        self.call(move |worker| worker.unlock_with_password(&master_password))
            .await
    }

    pub async fn unlock_with_recovery_key(
        &self,
        recovery_key: RecoveryKey,
    ) -> Result<DeviceCache, Error> {
        self.call(move |worker| worker.unlock_with_recovery_key(&recovery_key))
            .await
    }

    /// Unlocks with the key cached on this device, or returns
    /// [`Error::NotCached`]. This is where Touch ID or Face ID will gate it.
    pub async fn unlock_from_device(&self) -> Result<(), Error> {
        self.call(Worker::unlock_from_device).await
    }

    /// Lock now: drops the key from memory. The device's cached key stays.
    pub async fn lock(&self) -> Result<(), Error> {
        self.call(|worker| {
            worker.lock();
            Ok(())
        })
        .await
    }

    pub async fn set_auto_lock(&self, auto_lock: AutoLock) -> Result<(), Error> {
        self.call(move |worker| {
            worker.set_auto_lock(auto_lock);
            Ok(())
        })
        .await
    }

    /// The saved password for `entry`, waiting for an unlock if the vault is
    /// locked. `None` means nothing is saved for it.
    pub async fn password(&self, entry: &EntryKey) -> Result<Option<Secret>, Error> {
        let entry = entry.clone();
        self.call_unlocked(move |worker| worker.password(&entry))
            .await
    }

    /// Saves a password, waiting for an unlock if the vault is locked.
    pub async fn set_password(&self, entry: EntryKey, password: Secret) -> Result<(), Error> {
        let password = Arc::new(password);
        self.call_unlocked(move |worker| worker.set_password(&entry, &password))
            .await
    }

    /// Removes a password, returning whether one was saved.
    pub async fn remove_password(&self, entry: EntryKey) -> Result<bool, Error> {
        self.call_unlocked(move |worker| worker.remove_password(&entry))
            .await
    }

    /// What's saved, sorted by server then character, leaving out what
    /// [`Vault::stranded`] counts.
    pub async fn entries(&self) -> Result<Vec<EntryKey>, Error> {
        self.call_unlocked(Worker::entries).await
    }

    /// How many saved passwords can't be read with the current key, usually
    /// because a device saved them offline before the master password changed
    /// on another. Unlocking on that device brings them back by itself; on
    /// any other, the UI asks for the old password and calls
    /// [`Vault::rescue_with_password`]. One that differs from the password
    /// saved now stays counted until the player settles it: see
    /// [`Vault::conflicts`].
    pub async fn stranded(&self) -> Result<usize, Error> {
        self.call_unlocked(Worker::stranded).await
    }

    /// Brings back the stranded passwords `old_master_password` opens,
    /// returning how many. A wrong one gives [`Error::WrongPassword`].
    pub async fn rescue_with_password(&self, old_master_password: Secret) -> Result<usize, Error> {
        let password = Arc::new(old_master_password);
        self.call_unlocked(move |worker| worker.rescue_with_password(&password))
            .await
    }

    /// The stranded passwords `old_master_password` opens that differ from
    /// the one saved now for that character. Nothing says which is newer, so
    /// the player compares them and calls [`Vault::settle_conflict`].
    pub async fn conflicts(
        &self,
        old_master_password: Secret,
    ) -> Result<Vec<(EntryKey, Secret)>, Error> {
        let password = Arc::new(old_master_password);
        self.call_unlocked(move |worker| worker.conflicts(&password))
            .await
    }

    /// Keeps one of the two passwords in a conflict over `entry`.
    pub async fn settle_conflict(
        &self,
        old_master_password: Secret,
        entry: EntryKey,
        keep: KeepPassword,
    ) -> Result<(), Error> {
        let password = Arc::new(old_master_password);
        self.call_unlocked(move |worker| worker.settle_conflict(&password, &entry, keep))
            .await
    }

    /// Changes the master password, re-encrypting everything under a new
    /// vault key and issuing a new recovery key; the old one stops working.
    /// Old copies in git history still open with the old password, so the UI
    /// must say so (PRD §10).
    pub async fn change_master_password(
        &self,
        master_password: Secret,
    ) -> Result<RecoveryKeyIssued, Error> {
        let master_password = Arc::new(master_password);
        self.call_unlocked(move |worker| worker.change_master_password(&master_password))
            .await
    }

    /// Locks, and removes this device's cached key.
    pub async fn forget_device(&self) -> Result<(), Error> {
        self.call(Worker::forget_device).await
    }

    /// Runs `job`, repeating it after each unlock until it finds the vault
    /// open. The worker publishes `Locked` before it answers, so this can't
    /// spin: a repeat only happens once the state says `Unlocked` again.
    async fn call_unlocked<T: Send + 'static>(
        &self,
        job: impl Fn(&mut Worker) -> Result<T, Error> + Clone + Send + 'static,
    ) -> Result<T, Error> {
        loop {
            match self.call(job.clone()).await {
                Err(Error::Locked) => self.wait_until_unlocked().await?,
                outcome => return outcome,
            }
        }
    }

    async fn call<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut Worker) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |worker| {
            // The caller may have stopped waiting; that's not an error.
            let _ = reply.send(job(worker));
        });
        self.jobs.send(job).map_err(|_| Error::Stopped)?;
        answer.await.map_err(|_| Error::Stopped)?
    }
}
