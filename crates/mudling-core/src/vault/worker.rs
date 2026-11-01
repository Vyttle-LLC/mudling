//! The vault's thread. Everything slow happens here, one job at a time: Argon2,
//! file reads and writes, and credential store calls. It also holds the vault
//! key, and drops it when the idle timer runs out.
//!
//! Every job re-reads the file, because a sync can replace it at any moment.
//! Keeping a copy would mean writing over whatever a pull just brought in.

use super::crypto::{self, KdfParams, WrappingKey};
use super::error::Error;
use super::file::{self, Sealed, VaultFile, Wrapping};
use super::key_store::KeyStore;
use super::secret::{RecoveryKey, Secret, VaultKey};
use super::{
    AutoLock, DeviceCache, EntryKey, KeepPassword, RecoveryKeyIssued, VaultState, strength,
};
use data_encoding::HEXLOWER;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, SystemTime};
use tokio::sync::watch;

pub(super) type Job = Box<dyn FnOnce(&mut Worker) + Send>;

/// How often the idle timer re-reads the clock. `recv_timeout` alone would
/// sleep on a clock that stops with the machine, so a laptop waking after a
/// night would keep its vault open for the rest of the timer.
const CLOCK_CHECK: Duration = Duration::from_secs(15);

pub(super) struct Worker {
    pub path: PathBuf,
    pub key_store: Arc<dyn KeyStore>,
    pub kdf: KdfParams,
    pub auto_lock: AutoLock,
    pub key: Option<VaultKey>,
    /// Where this device last cached a key for this vault, which `forget_device`
    /// still needs if the file has since become unreadable.
    pub account: Option<String>,
    pub idle_deadline: Option<SystemTime>,
    pub state: watch::Sender<VaultState>,
    pub now: fn() -> SystemTime,
}

impl Worker {
    pub fn run(mut self, jobs: Receiver<Job>) {
        self.publish_file_state();
        while let Some(job) = self.next_job(&jobs) {
            job(&mut self);
        }
    }

    /// Waits for a job, locking the vault first if the idle timer runs out.
    /// A job can end the wait before the clock check does, so an expired
    /// vault is locked again here, or the job would renew its timer.
    pub(super) fn next_job(&mut self, jobs: &Receiver<Job>) -> Option<Job> {
        loop {
            self.lock_if_expired();
            let Some(deadline) = self.idle_deadline else {
                return jobs.recv().ok();
            };
            let remaining = deadline
                .duration_since((self.now)())
                .unwrap_or(Duration::ZERO);
            match jobs.recv_timeout(remaining.min(CLOCK_CHECK)) {
                Ok(job) => {
                    self.lock_if_expired();
                    return Some(job);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    fn lock_if_expired(&mut self) {
        if self
            .idle_deadline
            .is_some_and(|deadline| (self.now)() >= deadline)
        {
            self.lock();
        }
    }

    fn publish(&self, state: VaultState) {
        self.state.send_if_modified(|current| {
            let changed = *current != state;
            *current = state;
            changed
        });
    }

    fn publish_file_state(&self) {
        self.publish(if self.path.exists() {
            VaultState::Locked
        } else {
            VaultState::NotCreated
        });
    }

    pub fn lock(&mut self) {
        self.key = None;
        self.idle_deadline = None;
        self.publish_file_state();
    }

    pub fn set_auto_lock(&mut self, auto_lock: AutoLock) {
        self.auto_lock = auto_lock;
        if self.key.is_some() {
            self.restart_idle_timer();
        }
    }

    fn restart_idle_timer(&mut self) {
        self.idle_deadline = self.auto_lock.duration().map(|idle| (self.now)() + idle);
    }

    fn read(&self) -> Result<VaultFile, Error> {
        match fs::read(&self.path) {
            Ok(bytes) => file::parse(&bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                // A checkout can take the file away for a moment. While the
                // key is held the vault is still unlocked, and says so.
                if self.key.is_none() {
                    self.publish(VaultState::NotCreated);
                }
                Err(Error::NotCreated)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The file and the key to read it with. A key the file no longer matches
    /// (the master password changed on another device) is dropped, so callers
    /// wait for a fresh unlock.
    fn unlocked(&mut self) -> Result<(VaultFile, &VaultKey), Error> {
        let file = self.read()?;
        let current = self
            .key
            .as_ref()
            .is_some_and(|key| crypto::fingerprint(key) == file.key_fingerprint);
        if !current {
            self.lock();
            return Err(Error::Locked);
        }
        self.restart_idle_timer();
        let key = self.key.as_ref().ok_or(Error::Locked)?;
        Ok((file, key))
    }

    pub fn create(&mut self, password: &Secret) -> Result<RecoveryKeyIssued, Error> {
        if self.path.exists() {
            return Err(Error::AlreadyExists);
        }
        strength::check(password)?;
        let (file, key, recovery_key) = self.new_vault(crypto::random_array(), password)?;
        file::save_new(&self.path, &file)?;
        let device_cache = self.adopt(key, &file);
        Ok(RecoveryKeyIssued {
            recovery_key,
            device_cache,
        })
    }

    /// A fresh vault key and salt, wrapped under `password` and a new recovery
    /// key, with no entries.
    fn new_vault(
        &self,
        vault_id: [u8; crypto::VAULT_ID_LEN],
        password: &Secret,
    ) -> Result<(VaultFile, VaultKey, RecoveryKey), Error> {
        let key = VaultKey::generate();
        let recovery_key = RecoveryKey::generate();
        let salt = crypto::random_array();
        let master = WrappingKey::from_password(password, &salt, self.kdf)?;
        let recovery = WrappingKey::from_recovery_key(&recovery_key);
        let file = VaultFile::new(vault_id, &key, self.kdf, salt, &master, &recovery);
        Ok((file, key, recovery_key))
    }

    pub fn unlock_with_password(&mut self, password: &Secret) -> Result<DeviceCache, Error> {
        let file = self.read()?;
        let wrapping = WrappingKey::from_password(password, &file.salt, file.kdf)?;
        let key = file.unwrap_key(Wrapping::Master, &wrapping)?;
        Ok(self.adopt(key, &file))
    }

    pub fn unlock_with_recovery_key(
        &mut self,
        recovery_key: &RecoveryKey,
    ) -> Result<DeviceCache, Error> {
        let file = self.read()?;
        let wrapping = WrappingKey::from_recovery_key(recovery_key);
        let key = file.unwrap_key(Wrapping::Recovery, &wrapping)?;
        Ok(self.adopt(key, &file))
    }

    /// Unlocks with this device's cached key. A stale key is removed, so the
    /// next unlock asks for the master password, unless it still opens
    /// stranded passwords: the next unlock uses it for those.
    pub fn unlock_from_device(&mut self) -> Result<(), Error> {
        let file = self.read()?;
        let account = account(&file);
        let key = self.key_store.load(&account)?.ok_or(Error::NotCached)?;
        let fingerprint = crypto::fingerprint(&key);
        if fingerprint != file.key_fingerprint {
            if !file.stranded.contains_key(&file::stranded_id(&fingerprint)) {
                self.key_store.delete(&account)?;
            }
            return Err(Error::NotCached);
        }
        self.install(key, account);
        Ok(())
    }

    /// Unlocks with `key` and caches it on this device. The key it replaces
    /// here may open stranded passwords; if that fails, they stay stranded
    /// for the old master password to open.
    fn adopt(&mut self, key: VaultKey, file: &VaultFile) -> DeviceCache {
        let account = account(file);
        let replaced = self.key_store.load(&account).ok().flatten();
        let cached = self.key_store.save(&account, &key);
        self.install(key, account);
        if let Some(replaced) = replaced {
            let _ = self.rescue(&replaced);
        }
        match cached {
            Ok(()) => DeviceCache::Saved,
            Err(error) => DeviceCache::NotSaved(error),
        }
    }

    /// Passwords the vault holds but can't read with the current key: those
    /// stranded under a replaced key, and any entry that fails to open.
    pub fn stranded(&mut self) -> Result<usize, Error> {
        let (file, key) = self.unlocked()?;
        let unreadable = file
            .entries
            .iter()
            .filter(|(id, sealed)| decrypt_entry(&file, key, id, sealed).is_err())
            .count();
        let older: usize = file.stranded.values().map(|old| old.entries.len()).sum();
        Ok(unreadable + older)
    }

    /// Moves what `password`, a replaced master password, can open back into
    /// the vault, returning how many, except any that differ from the password
    /// saved now, which stay for [`Worker::conflicts`]. Fails with
    /// [`Error::WrongPassword`] if it opens nothing stranded.
    pub fn rescue_with_password(&mut self, password: &Secret) -> Result<usize, Error> {
        let old = self.stranded_key(password)?;
        self.rescue(&old)
    }

    /// The replaced key `password` opens among the stranded vaults.
    fn stranded_key(&mut self, password: &Secret) -> Result<VaultKey, Error> {
        let (file, _) = self.unlocked()?;
        for old in file.stranded.values() {
            let wrapping = WrappingKey::from_password(password, &old.salt, old.kdf)?;
            if let Ok(key) = old.unwrap_key(Wrapping::Master, &wrapping) {
                return Ok(key);
            }
        }
        Err(Error::WrongPassword)
    }

    /// Re-seals what `old` opens under the current key. Entries carry no time,
    /// so one that differs from the password saved now for that character
    /// stays stranded for the player to settle, as does one that won't open.
    fn rescue(&mut self, old: &VaultKey) -> Result<usize, Error> {
        let stranded_id = file::stranded_id(&crypto::fingerprint(old));
        let (mut file, key) = self.unlocked()?;
        let Some(mut block) = file.stranded.remove(&stranded_id) else {
            return Ok(0);
        };
        let mut rescued = 0;
        for (id, sealed) in std::mem::take(&mut block.entries) {
            let Ok((entry, password)) = decrypt_entry(&block, old, &id, &sealed) else {
                block.entries.insert(id, sealed);
                continue;
            };
            let current_id = crypto::entry_id(key, &entry);
            match file.entries.get(&current_id) {
                None => {
                    let resealed = encrypt_entry(&file, key, &current_id, &entry, &password);
                    file.entries.insert(current_id, resealed);
                    rescued += 1;
                }
                Some(current) => {
                    let opened = decrypt_entry(&file, key, &current_id, current);
                    if opened.is_ok_and(|(_, held)| held.expose() == password.expose()) {
                        rescued += 1;
                    } else {
                        block.entries.insert(id, sealed);
                    }
                }
            }
        }
        if !block.entries.is_empty() {
            file.stranded.insert(stranded_id, block);
        }
        file::save(&self.path, &file)?;
        Ok(rescued)
    }

    /// What `password`, a replaced master password, holds that differs from
    /// what's saved now, after rescuing everything else it opens.
    pub fn conflicts(&mut self, password: &Secret) -> Result<Vec<(EntryKey, Secret)>, Error> {
        let old = self.stranded_key(password)?;
        self.rescue(&old)?;
        let (file, _) = self.unlocked()?;
        let Some(block) = file
            .stranded
            .get(&file::stranded_id(&crypto::fingerprint(&old)))
        else {
            return Ok(Vec::new());
        };
        let mut conflicts: Vec<(EntryKey, Secret)> = block
            .entries
            .iter()
            .filter_map(|(id, sealed)| decrypt_entry(block, &old, id, sealed).ok())
            .collect();
        conflicts.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(conflicts)
    }

    /// Ends a conflict over `entry` with the player's choice. Either way the
    /// stranded copy goes, so the other password survives only in git history.
    /// Keeping the current password fails with [`Error::NoCurrentPassword`] if
    /// no readable one is saved, because that would drop the only copy.
    pub fn settle_conflict(
        &mut self,
        password: &Secret,
        entry: &EntryKey,
        keep: KeepPassword,
    ) -> Result<(), Error> {
        let old = self.stranded_key(password)?;
        let stranded_id = file::stranded_id(&crypto::fingerprint(&old));
        let (mut file, key) = self.unlocked()?;
        let Some(block) = file.stranded.get(&stranded_id) else {
            return Ok(());
        };
        let found = block.entries.iter().find_map(|(id, sealed)| {
            let (held, password) = decrypt_entry(block, &old, id, sealed).ok()?;
            (held == *entry).then(|| (id.clone(), password))
        });
        let Some((old_id, stranded)) = found else {
            return Ok(());
        };
        let id = crypto::entry_id(key, entry);
        match keep {
            KeepPassword::Current => {
                let current = file.entries.get(&id);
                if !current.is_some_and(|sealed| decrypt_entry(&file, key, &id, sealed).is_ok()) {
                    return Err(Error::NoCurrentPassword);
                }
            }
            KeepPassword::Stranded => {
                let sealed = encrypt_entry(&file, key, &id, entry, &stranded);
                file.entries.insert(id, sealed);
            }
        }
        if let Some(block) = file.stranded.get_mut(&stranded_id) {
            block.entries.remove(&old_id);
        }
        file.stranded.retain(|_, block| !block.entries.is_empty());
        file::save(&self.path, &file)?;
        Ok(())
    }

    fn install(&mut self, key: VaultKey, account: String) {
        self.key = Some(key);
        self.account = Some(account);
        self.restart_idle_timer();
        self.publish(VaultState::Unlocked);
    }

    pub fn password(&mut self, entry: &EntryKey) -> Result<Option<Secret>, Error> {
        let (file, key) = self.unlocked()?;
        let id = crypto::entry_id(key, entry);
        let Some(sealed) = file.entries.get(&id) else {
            return Ok(None);
        };
        let (found, password) = decrypt_entry(&file, key, &id, sealed)?;
        if found != *entry {
            return Err(Error::Tampered);
        }
        Ok(Some(password))
    }

    /// One entry that won't open doesn't cost the player the list of the
    /// rest: [`Worker::stranded`] counts it instead.
    pub fn entries(&mut self) -> Result<Vec<EntryKey>, Error> {
        let (file, key) = self.unlocked()?;
        let mut entries: Vec<EntryKey> = file
            .entries
            .iter()
            .filter_map(|(id, sealed)| Some(decrypt_entry(&file, key, id, sealed).ok()?.0))
            .collect();
        entries.sort();
        Ok(entries)
    }

    pub fn set_password(&mut self, entry: &EntryKey, password: &Secret) -> Result<(), Error> {
        let (mut file, key) = self.unlocked()?;
        let id = crypto::entry_id(key, entry);
        let sealed = encrypt_entry(&file, key, &id, entry, password);
        file.entries.insert(id, sealed);
        file::save(&self.path, &file)?;
        Ok(())
    }

    pub fn remove_password(&mut self, entry: &EntryKey) -> Result<bool, Error> {
        let (mut file, key) = self.unlocked()?;
        let id = crypto::entry_id(key, entry);
        let removed = file.entries.remove(&id).is_some();
        if removed {
            file::save(&self.path, &file)?;
        }
        Ok(removed)
    }

    /// Replaces the vault key, not just the password that wraps it: git keeps
    /// the old file, and with it the old key to anyone holding the old
    /// password. The recovery key has to change too, because the vault can't
    /// wrap a new key for an old recovery key it never stores.
    pub fn change_master_password(
        &mut self,
        password: &Secret,
    ) -> Result<RecoveryKeyIssued, Error> {
        strength::check(password)?;
        let (old_file, old_key) = self.unlocked()?;
        let plain = old_file
            .entries
            .iter()
            .map(|(id, sealed)| decrypt_entry(&old_file, old_key, id, sealed))
            .collect::<Result<Vec<_>, Error>>()?;
        let vault_id = old_file.vault_id();
        let (mut file, key, recovery_key) = self.new_vault(vault_id, password)?;
        file.stranded = old_file.stranded.clone();
        for (entry, password) in &plain {
            let id = crypto::entry_id(&key, entry);
            let sealed = encrypt_entry(&file, &key, &id, entry, password);
            file.entries.insert(id, sealed);
        }
        self.save_if_unchanged(&old_file, &file)?;
        let device_cache = self.adopt(key, &file);
        Ok(RecoveryKeyIssued {
            recovery_key,
            device_cache,
        })
    }

    /// Saves `file` unless the vault on disk has moved on from `expected`.
    /// Argon2 leaves time for a pull to land, and saving over it would lose
    /// whatever it brought. The file system has no compare-and-swap, so this
    /// narrows the window to the instant between the check and the rename.
    pub(super) fn save_if_unchanged(
        &self,
        expected: &VaultFile,
        file: &VaultFile,
    ) -> Result<(), Error> {
        if self.read()? != *expected {
            return Err(Error::ChangedDuringSave);
        }
        file::save(&self.path, file)?;
        Ok(())
    }

    /// Locks and removes this device's cached key, so the next unlock takes
    /// the master password or recovery key. It locks whatever state the file
    /// is in; an unreadable file only fails this if nothing here has unlocked
    /// the vault, so there's no telling which key to remove.
    pub fn forget_device(&mut self) -> Result<(), Error> {
        self.lock();
        let account = match (self.read(), self.account.take()) {
            (Ok(file), _) => account(&file),
            (Err(_), Some(account)) => account,
            (Err(error), None) => return Err(error),
        };
        self.key_store.delete(&account)?;
        Ok(())
    }
}

/// Each vault gets its own account, so two vaults on one device don't collide.
fn account(file: &VaultFile) -> String {
    format!("vault-{}", HEXLOWER.encode(&file.vault_id()))
}

fn encrypt_entry(
    file: &VaultFile,
    key: &VaultKey,
    id: &str,
    entry: &EntryKey,
    password: &Secret,
) -> Sealed {
    let plaintext = crypto::encode_entry(entry, password);
    Sealed(crypto::seal(key.expose(), &plaintext, &file.entry_aad(id)))
}

fn decrypt_entry(
    file: &VaultFile,
    key: &VaultKey,
    id: &str,
    sealed: &Sealed,
) -> Result<(EntryKey, Secret), Error> {
    let plaintext =
        crypto::open(key.expose(), &sealed.0, &file.entry_aad(id)).ok_or(Error::Tampered)?;
    crypto::decode_entry(&plaintext).ok_or(Error::Corrupt("an entry has an unknown layout"))
}
