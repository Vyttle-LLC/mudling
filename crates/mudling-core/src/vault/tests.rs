use super::crypto::{self, KdfParams};
use super::file;
use super::*;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;
use tempfile::TempDir;
use tokio::time::{sleep, timeout};
use unicode_normalization::UnicodeNormalization as _;

const MASTER: &str = "violet anchor mosaic drifting under quiet lanterns";
const OTHER_MASTER: &str = "seventeen copper herons argue about the tide";
/// Argon2's floor: fast enough for a test to unlock freely in a debug build.
const CHEAP_KDF: KdfParams = KdfParams {
    memory_kib: 8,
    iterations: 1,
    parallelism: 1,
};
/// Long enough to show that something is waiting, not racing.
const STILL_WAITING: Duration = Duration::from_millis(50);

struct Fixture {
    _folder: TempDir,
    path: PathBuf,
    key_store: Arc<MemoryKeyStore>,
}

impl Fixture {
    fn new() -> Self {
        let folder = TempDir::new().unwrap();
        let path = folder.path().join(FILE_NAME);
        Self {
            _folder: folder,
            path,
            key_store: Arc::default(),
        }
    }

    fn vault(&self) -> Vault {
        self.vault_with(AutoLock::NEVER)
    }

    fn vault_with(&self, auto_lock: AutoLock) -> Vault {
        Vault::with_kdf(
            self.path.clone(),
            self.key_store.clone(),
            auto_lock,
            CHEAP_KDF,
        )
    }

    /// A new vault holding one saved password, with its recovery key.
    async fn created(&self) -> (Vault, RecoveryKey) {
        let vault = self.vault();
        let created = vault.create(MASTER.into()).await.unwrap();
        vault
            .set_password(fikus(), "swordfish-42".into())
            .await
            .unwrap();
        (vault, created.recovery_key)
    }

    fn text(&self) -> String {
        fs::read_to_string(&self.path).unwrap()
    }

    /// Rewrites one JSON value in the file, keeping the rest as it was.
    fn edit(&self, change: impl FnOnce(&mut Value)) {
        let mut json: Value = serde_json::from_str(&self.text()).unwrap();
        change(&mut json);
        fs::write(&self.path, serde_json::to_string_pretty(&json).unwrap()).unwrap();
    }

    /// Flips one base64 character of the value at `pointer`.
    fn flip(&self, pointer: &str) {
        self.edit(|json| {
            let value = json.pointer_mut(pointer).unwrap();
            let mut text = value.as_str().unwrap().to_owned();
            let replacement = if text.as_bytes()[8] == b'A' { "B" } else { "A" };
            text.replace_range(8..9, replacement);
            *value = Value::String(text);
        });
    }

    /// The pointer to the file's only entry.
    fn entry_pointer(&self) -> String {
        let json: Value = serde_json::from_str(&self.text()).unwrap();
        let id = json["entries"].as_object().unwrap().keys().next().unwrap();
        format!("/entries/{id}")
    }

    async fn unlock(&self) -> Result<DeviceCache, Error> {
        self.vault().unlock_with_password(MASTER.into()).await
    }
}

fn fikus() -> EntryKey {
    EntryKey::new("mud.example.org", "Fikus")
}

async fn saved(vault: &Vault) -> Option<String> {
    let password = vault.password(&fikus()).await.unwrap();
    password.map(|password| password.expose().to_owned())
}

#[tokio::test]
async fn a_password_survives_reopening_the_vault() {
    let fixture = Fixture::new();
    fixture.created().await;

    let vault = fixture.vault();
    assert_eq!(vault.state(), VaultState::Locked);
    assert!(matches!(
        vault.unlock_with_password(MASTER.into()).await,
        Ok(DeviceCache::Saved)
    ));

    assert_eq!(vault.state(), VaultState::Unlocked);
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
    assert_eq!(vault.entries().await.unwrap(), [fikus()]);
    assert!(vault.remove_password(fikus()).await.unwrap());
    assert_eq!(saved(&vault).await, None);
}

#[tokio::test]
async fn the_recovery_key_opens_the_vault_however_it_was_typed() {
    let fixture = Fixture::new();
    let (_, recovery_key) = fixture.created().await;
    let typed = recovery_key.reveal().to_lowercase().replace('-', " ");

    let vault = fixture.vault();
    let parsed = RecoveryKey::parse(&typed).unwrap();
    assert!(vault.unlock_with_recovery_key(parsed).await.is_ok());
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn the_device_cache_unlocks_without_the_master_password() {
    let fixture = Fixture::new();
    fixture.created().await;

    let vault = fixture.vault();
    vault.unlock_from_device().await.unwrap();
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));

    vault.forget_device().await.unwrap();
    assert_eq!(vault.state(), VaultState::Locked);
    assert!(matches!(
        vault.unlock_from_device().await,
        Err(Error::NotCached)
    ));
}

#[tokio::test]
async fn forgetting_a_device_works_while_the_file_is_unreadable() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let readable = fixture.bytes();
    fs::write(&fixture.path, "<<<<<<< HEAD").unwrap();

    vault.forget_device().await.unwrap();

    assert_eq!(vault.state(), VaultState::Locked);
    fs::write(&fixture.path, readable).unwrap();
    assert!(matches!(
        vault.unlock_from_device().await,
        Err(Error::NotCached)
    ));
}

#[tokio::test]
async fn forgetting_a_device_with_no_way_to_name_its_key_says_so_and_keeps_it() {
    let fixture = Fixture::new();
    fixture.created().await;
    let readable = fixture.bytes();
    let vault = fixture.vault();
    fs::write(&fixture.path, "<<<<<<< HEAD").unwrap();

    assert!(matches!(
        vault.forget_device().await,
        Err(Error::Corrupt(_))
    ));

    fs::write(&fixture.path, readable).unwrap();
    vault.unlock_from_device().await.unwrap();
}

/// A folder that can be written but not opened, as some network mounts behave:
/// the rename lands, and the sync after it can't happen.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_folder_sync_still_hands_over_the_recovery_key() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    let folder = fixture.path.parent().unwrap();
    let permissions = |mode| fs::Permissions::from_mode(mode);
    fs::set_permissions(folder, permissions(0o300)).unwrap();

    let created = fixture.vault().create(MASTER.into()).await;

    fs::set_permissions(folder, permissions(0o700)).unwrap();
    let recovery_key = created.unwrap().recovery_key;
    let reader = fixture.vault();
    let _ = reader.unlock_with_recovery_key(recovery_key).await.unwrap();
}

#[tokio::test]
async fn a_broken_credential_store_is_reported_and_the_vault_still_opens() {
    struct Broken;
    impl KeyStore for Broken {
        fn load(&self, _: &str) -> Result<Option<VaultKey>, KeyStoreError> {
            Err(KeyStoreError("no keychain".into()))
        }
        fn save(&self, _: &str, _: &VaultKey) -> Result<(), KeyStoreError> {
            Err(KeyStoreError("no keychain".into()))
        }
        fn delete(&self, _: &str) -> Result<(), KeyStoreError> {
            Err(KeyStoreError("no keychain".into()))
        }
    }

    let fixture = Fixture::new();
    fixture.created().await;
    let vault = Vault::with_kdf(
        fixture.path.clone(),
        Arc::new(Broken),
        AutoLock::NEVER,
        CHEAP_KDF,
    );

    assert!(matches!(
        vault.unlock_with_password(MASTER.into()).await,
        Ok(DeviceCache::NotSaved(_))
    ));
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn weak_master_passwords_are_refused() {
    let fixture = Fixture::new();
    let vault = fixture.vault();
    for weak in [
        "",
        "password123",
        "qwertyuiop",
        "mudlingvault1",
        "aaaaaaaaaaaaaaaa",
    ] {
        assert!(
            matches!(
                vault.create(weak.into()).await,
                Err(Error::WeakPassword { .. })
            ),
            "{weak} was accepted"
        );
    }
    assert!(!fixture.path.exists());
}

#[tokio::test]
async fn creating_never_overwrites_a_vault() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let before = fixture.text();

    assert!(matches!(
        vault.create(OTHER_MASTER.into()).await,
        Err(Error::AlreadyExists)
    ));
    assert_eq!(fixture.text(), before);
}

#[tokio::test]
async fn asking_before_a_vault_exists_is_an_error_not_a_wait() {
    let vault = Fixture::new().vault();
    assert!(matches!(
        vault.password(&fikus()).await,
        Err(Error::NotCreated)
    ));
    assert_eq!(vault.state(), VaultState::NotCreated);
}

#[tokio::test]
async fn a_wrong_password_or_recovery_key_leaves_the_file_intact() {
    let fixture = Fixture::new();
    fixture.created().await;
    let before = fixture.text();
    let vault = fixture.vault();

    assert!(matches!(
        vault.unlock_with_password(OTHER_MASTER.into()).await,
        Err(Error::WrongPassword)
    ));
    assert!(matches!(
        vault
            .unlock_with_recovery_key(RecoveryKey::generate())
            .await,
        Err(Error::WrongRecoveryKey)
    ));
    assert!(matches!(
        RecoveryKey::parse("not a key"),
        Err(Error::WrongRecoveryKey)
    ));
    assert_eq!(vault.state(), VaultState::Locked);
    assert_eq!(fixture.text(), before);
}

#[tokio::test]
async fn damaged_files_are_corrupt_and_stay_as_they_were() {
    let fixture = Fixture::new();
    fixture.created().await;
    let good = fixture.text();

    for damage in [
        "not json at all\n".to_owned(),
        "{}\n".to_owned(),
        good.replace("mudling-vault", "someone-else"),
        format!("{good}trailing"),
        good.replace("\"digest\"", "\"digset\""),
    ] {
        fs::write(&fixture.path, &damage).unwrap();
        assert!(
            matches!(fixture.unlock().await, Err(Error::Corrupt(_))),
            "{damage}"
        );
        assert_eq!(fixture.text(), damage);
    }

    // Each of these would otherwise look like a wrong password.
    for pointer in ["/salt", "/master_password/check", "/key_fingerprint"] {
        fs::write(&fixture.path, &good).unwrap();
        fixture.flip(pointer);
        assert!(
            matches!(fixture.unlock().await, Err(Error::Corrupt(_))),
            "{pointer}"
        );
    }
}

#[tokio::test]
async fn an_absurd_argon2_cost_is_refused_before_any_memory_is_taken() {
    let fixture = Fixture::new();
    fixture.created().await;
    fixture.edit(|json| json["kdf"]["memory_kib"] = u32::MAX.into());

    assert!(matches!(
        fixture.unlock().await,
        Err(Error::Corrupt("Argon2 parameters out of range"))
    ));
}

#[tokio::test]
async fn tampered_ciphertext_is_told_from_a_wrong_password() {
    let fixture = Fixture::new();
    let (_, recovery_key) = fixture.created().await;
    let good = fixture.text();

    fixture.flip("/master_password/vault_key");
    assert!(matches!(fixture.unlock().await, Err(Error::Tampered)));

    fs::write(&fixture.path, &good).unwrap();
    fixture.flip("/recovery_key/vault_key");
    let typed = RecoveryKey::parse(&recovery_key.reveal()).unwrap();
    let vault = fixture.vault();
    assert!(matches!(
        vault.unlock_with_recovery_key(typed).await,
        Err(Error::Tampered)
    ));

    fs::write(&fixture.path, &good).unwrap();
    let entry = fixture.entry_pointer();
    fixture.flip(&entry);
    let tampered = fixture.text();
    let vault = fixture.vault();
    let _ = vault.unlock_with_password(MASTER.into()).await.unwrap();
    assert!(matches!(
        vault.password(&fikus()).await,
        Err(Error::Tampered)
    ));
    assert_eq!(fixture.text(), tampered);
}

#[tokio::test]
async fn an_entry_moved_to_another_id_fails_authentication() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let other = EntryKey::new("mud.example.org", "Mira");
    vault.set_password(other, "other".into()).await.unwrap();

    fixture.edit(|json| {
        let entries = json["entries"].as_object_mut().unwrap();
        let ids: Vec<String> = entries.keys().cloned().collect();
        let first = entries[&ids[0]].clone();
        entries[&ids[0]] = entries[&ids[1]].clone();
        entries[&ids[1]] = first;
    });

    assert!(matches!(
        vault.password(&fikus()).await,
        Err(Error::Tampered)
    ));
}

#[tokio::test]
async fn a_write_cut_off_at_any_byte_reads_as_truncated() {
    let fixture = Fixture::new();
    fixture.created().await;
    let whole = fs::read(&fixture.path).unwrap();
    let last_needed = whole.len() - 1; // Only the final newline is optional.

    for length in 0..last_needed {
        assert!(
            matches!(file::parse(&whole[..length]), Err(Error::Truncated)),
            "cut at {length}"
        );
    }
    assert!(file::parse(&whole[..last_needed]).is_ok());

    let cut = &whole[..whole.len() / 2];
    fs::write(&fixture.path, cut).unwrap();
    assert!(matches!(fixture.unlock().await, Err(Error::Truncated)));
    assert_eq!(fs::read(&fixture.path).unwrap(), cut);
}

#[tokio::test]
async fn a_newer_format_is_reported_by_version() {
    let fixture = Fixture::new();
    fixture.created().await;
    fixture.edit(|json| {
        json["version"] = 2.into();
        json["from_the_future"] = true.into();
    });

    assert!(matches!(
        fixture.unlock().await,
        Err(Error::UnsupportedVersion(2))
    ));
}

#[tokio::test]
async fn the_file_holds_no_plaintext() {
    let fixture = Fixture::new();
    fixture.created().await;

    let text = fixture.text();
    for plaintext in ["mud.example", "Fikus", "swordfish", MASTER] {
        assert!(!text.contains(plaintext), "{plaintext} is in the file");
    }
    let lengths: Vec<usize> = ["short", "a much longer password than that one"]
        .iter()
        .map(|password| crypto::encode_entry(&fikus(), &(*password).into()).len())
        .collect();
    assert_eq!(lengths[0], lengths[1], "padding hides a password's length");
}

#[tokio::test]
async fn saving_one_password_changes_one_line() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let lines_changed = |before: &str, after: &str| {
        let kept: Vec<&str> = before.lines().collect();
        after.lines().filter(|line| !kept.contains(line)).count()
    };

    let before = fixture.text();
    vault.set_password(fikus(), "changed".into()).await.unwrap();
    assert_eq!(lines_changed(&before, &fixture.text()), 1);

    let before = fixture.text();
    let other = EntryKey::new("mud.example.org", "Mira");
    vault.set_password(other, "new".into()).await.unwrap();
    // The new line, plus at most a comma on the line before it.
    assert!(lines_changed(&before, &fixture.text()) <= 2);
    assert!(fixture.text().ends_with("}\n"));
    assert!(!fixture.text().contains('\r'));
}

#[tokio::test]
async fn a_checkout_with_crlf_endings_still_opens() {
    let fixture = Fixture::new();
    fixture.created().await;
    fs::write(&fixture.path, fixture.text().replace('\n', "\r\n")).unwrap();

    let vault = fixture.vault();
    let _ = vault.unlock_with_password(MASTER.into()).await.unwrap();
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn saving_leaves_no_temporary_files_and_works_in_a_new_folder() {
    let fixture = Fixture::new();
    let nested = fixture.path.with_file_name("repo").join(FILE_NAME);
    let vault = Vault::with_kdf(
        nested.clone(),
        fixture.key_store.clone(),
        AutoLock::NEVER,
        CHEAP_KDF,
    );
    let _ = vault.create(MASTER.into()).await.unwrap();

    let names: Vec<_> = fs::read_dir(nested.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, [FILE_NAME]);
}

#[cfg(unix)]
#[tokio::test]
async fn only_the_owner_can_read_the_file() {
    use std::os::unix::fs::PermissionsExt as _;

    let fixture = Fixture::new();
    fixture.created().await;
    let mode = fs::metadata(&fixture.path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn a_write_from_another_handle_is_not_lost() {
    let fixture = Fixture::new();
    let (first, _) = fixture.created().await;
    let second = fixture.vault();
    let _ = second.unlock_with_password(MASTER.into()).await.unwrap();

    let mira = EntryKey::new("mud.example.org", "Mira");
    second
        .set_password(mira.clone(), "pulled".into())
        .await
        .unwrap();
    first
        .set_password(EntryKey::new("mud.example.org", "Kel"), "mine".into())
        .await
        .unwrap();

    assert_eq!(first.entries().await.unwrap().len(), 3);
    assert!(first.password(&mira).await.unwrap().is_some());
}

#[tokio::test]
async fn composed_and_decomposed_passwords_derive_the_same_key() {
    let fixture = Fixture::new();
    let composed: String = "éléphant violet anchor mosaic drifting quietly"
        .nfc()
        .collect();
    let decomposed: String = composed.nfd().collect();
    assert_ne!(composed, decomposed);

    let vault = fixture.vault();
    let _ = vault.create(composed.into()).await.unwrap();
    assert!(
        fixture
            .vault()
            .unlock_with_password(decomposed.into())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn changing_the_master_password_replaces_every_key() {
    let fixture = Fixture::new();
    let (vault, old_recovery_key) = fixture.created().await;
    let other_device = fixture.vault();
    let _ = other_device
        .unlock_with_password(MASTER.into())
        .await
        .unwrap();
    let old_text = fixture.text();

    let changed = vault
        .change_master_password(OTHER_MASTER.into())
        .await
        .unwrap();

    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
    let reader = fixture.vault();
    assert!(matches!(
        reader.unlock_with_password(MASTER.into()).await,
        Err(Error::WrongPassword)
    ));
    assert!(matches!(
        reader.unlock_with_recovery_key(old_recovery_key).await,
        Err(Error::WrongRecoveryKey)
    ));
    let _ = reader
        .unlock_with_recovery_key(changed.recovery_key)
        .await
        .unwrap();

    // The old key, which git history still holds, must open nothing new.
    let new_text = fixture.text();
    let line_with = |text: &str, field: &str| {
        let line = text.lines().find(|line| line.contains(field));
        line.unwrap().to_owned()
    };
    assert_ne!(
        line_with(&old_text, "key_fingerprint"),
        line_with(&new_text, "key_fingerprint")
    );

    // A device still holding the old key goes back to waiting for an unlock.
    let waiting = timeout(STILL_WAITING, other_device.password(&fikus())).await;
    assert!(waiting.is_err());
    assert_eq!(other_device.state(), VaultState::Locked);
}

#[tokio::test]
async fn a_stale_cached_key_is_removed_not_used() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let id = serde_json::from_str::<Value>(&fixture.text()).unwrap()["vault_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let bytes = data_encoding::BASE64.decode(id.as_bytes()).unwrap();
    let account = format!("vault-{}", data_encoding::HEXLOWER.encode(&bytes));
    fixture
        .key_store
        .save(&account, &VaultKey::generate())
        .unwrap();

    assert!(matches!(
        vault.unlock_from_device().await,
        Err(Error::NotCached)
    ));
    assert!(fixture.key_store.load(&account).unwrap().is_none());
}

#[tokio::test]
async fn a_locked_vault_makes_a_caller_wait_until_it_is_unlocked() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    vault.lock().await.unwrap();
    assert_eq!(vault.state(), VaultState::Locked);

    let waiter = vault.clone();
    let waiting = tokio::spawn(async move { saved(&waiter).await });
    sleep(STILL_WAITING).await;
    assert!(!waiting.is_finished());

    let _ = vault.unlock_with_password(MASTER.into()).await.unwrap();
    assert_eq!(waiting.await.unwrap().as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn a_cancelled_wait_leaves_the_vault_usable() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    vault.lock().await.unwrap();

    assert!(
        timeout(STILL_WAITING, vault.password(&fikus()))
            .await
            .is_err()
    );

    vault.unlock_from_device().await.unwrap();
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn the_idle_timer_locks_the_vault_and_drops_the_key() {
    let fixture = Fixture::new();
    fixture.created().await;
    let vault = fixture.vault_with(AutoLock(Some(STILL_WAITING)));
    let mut state = vault.subscribe();
    vault.unlock_from_device().await.unwrap();

    state
        .wait_for(|state| *state == VaultState::Locked)
        .await
        .unwrap();
    let key_is_gone = vault.call(|worker| Ok(worker.key.is_none())).await;
    assert!(key_is_gone.unwrap());
}

#[tokio::test]
async fn using_the_vault_restarts_the_idle_timer() {
    let fixture = Fixture::new();
    fixture.created().await;
    let vault = fixture.vault_with(AutoLock::ONE_HOUR);
    vault.unlock_from_device().await.unwrap();
    let deadline = || vault.call(|worker| Ok(worker.idle_deadline));

    let before = deadline().await.unwrap();
    sleep(Duration::from_millis(5)).await;
    vault.entries().await.unwrap();

    assert!(deadline().await.unwrap() > before);
}

#[tokio::test]
async fn time_asleep_counts_towards_the_idle_timer() {
    let fixture = Fixture::new();
    fixture.created().await;
    let vault = fixture.vault_with(AutoLock::ONE_HOUR);
    vault.unlock_from_device().await.unwrap();

    // As it looks after a machine wakes: the wall clock has jumped past the
    // deadline, though the thread only just started waiting.
    vault
        .call(|worker| {
            worker.idle_deadline = Some(UNIX_EPOCH);
            Ok(())
        })
        .await
        .unwrap();

    let waiting = timeout(STILL_WAITING, vault.password(&fikus())).await;
    assert!(waiting.is_err());
    assert_eq!(vault.state(), VaultState::Locked);
}

/// The machine sleeps while the thread waits: the deadline is still ahead when
/// the wait starts, and has passed by the time a job wakes it.
#[tokio::test]
async fn a_job_that_wakes_the_thread_after_sleep_finds_the_vault_locked() {
    static CLOCK_READS: AtomicUsize = AtomicUsize::new(0);
    fn clock_jumping_after_first_read() -> SystemTime {
        match CLOCK_READS.fetch_add(1, Ordering::Relaxed) {
            0 => UNIX_EPOCH,
            _ => UNIX_EPOCH + Duration::from_secs(8 * 60 * 60),
        }
    }

    let fixture = Fixture::new();
    fixture.created().await;
    let (state, _watching) = watch::channel(VaultState::Unlocked);
    let mut worker = Worker {
        path: fixture.path.clone(),
        key_store: fixture.key_store.clone(),
        kdf: CHEAP_KDF,
        auto_lock: AutoLock::FIVE_MINUTES,
        key: Some(VaultKey::generate()),
        account: None,
        idle_deadline: Some(UNIX_EPOCH + Duration::from_secs(5 * 60)),
        state,
        now: clock_jumping_after_first_read,
    };
    let (jobs, inbox) = mpsc::channel::<Job>();
    jobs.send(Box::new(|_| {})).unwrap();

    assert!(worker.next_job(&inbox).is_some());

    assert!(worker.key.is_none());
    assert_eq!(*worker.state.borrow(), VaultState::Locked);
}

#[test]
fn auto_lock_offers_the_four_choices() {
    let choices = [
        AutoLock::FIVE_MINUTES,
        AutoLock::FIFTEEN_MINUTES,
        AutoLock::ONE_HOUR,
        AutoLock::NEVER,
    ];
    let minutes = choices.map(|choice| choice.duration().map(|d| d.as_secs() / 60));
    assert_eq!(minutes, [Some(5), Some(15), Some(60), None]);
}

#[tokio::test]
async fn a_new_vault_never_replaces_one_that_appeared_in_the_meantime() {
    let fixture = Fixture::new();
    fixture.created().await;
    let new_vault = file::parse(fixture.text().as_bytes()).unwrap();
    fs::write(&fixture.path, "pulled while Argon2 ran").unwrap();

    assert!(matches!(
        file::save_new(&fixture.path, &new_vault),
        Err(Error::AlreadyExists)
    ));
    assert_eq!(fixture.text(), "pulled while Argon2 ran");
    let folder = fixture.path.parent().unwrap();
    assert_eq!(fs::read_dir(folder).unwrap().count(), 1);
}

#[tokio::test]
async fn a_master_password_change_never_overwrites_a_pull() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let before_the_pull = file::parse(fixture.text().as_bytes()).unwrap();
    let pulled = EntryKey::new("mud.example.org", "Mira");
    vault.set_password(pulled, "new".into()).await.unwrap();
    let after_the_pull = fixture.text();

    let outcome = vault
        .call(move |worker| worker.save_if_unchanged(&before_the_pull, &before_the_pull))
        .await;

    assert!(matches!(outcome, Err(Error::ChangedDuringSave)));
    assert_eq!(fixture.text(), after_the_pull);
    assert_eq!(vault.entries().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_briefly_missing_file_leaves_the_vault_unlocked() {
    let fixture = Fixture::new();
    let (vault, _) = fixture.created().await;
    let moved = fixture.path.with_extension("moved");
    fs::rename(&fixture.path, &moved).unwrap();

    assert!(matches!(
        vault.password(&fikus()).await,
        Err(Error::NotCreated)
    ));
    assert_eq!(vault.state(), VaultState::Unlocked);
    timeout(STILL_WAITING, vault.wait_until_unlocked())
        .await
        .expect("nothing hangs while the key is held")
        .unwrap();

    fs::rename(&moved, &fixture.path).unwrap();
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

/// Debug builds of Argon2 are slow, so this is the one round trip at full cost.
#[tokio::test]
async fn the_default_argon2_cost_opens_what_it_sealed() {
    let fixture = Fixture::new();
    let open = || {
        Vault::open(
            fixture.path.clone(),
            fixture.key_store.clone(),
            AutoLock::NEVER,
        )
    };
    let vault = open();
    let _ = vault.create(MASTER.into()).await.unwrap();
    vault
        .set_password(fikus(), "swordfish-42".into())
        .await
        .unwrap();

    let reopened = open();
    let _ = reopened.unlock_with_password(MASTER.into()).await.unwrap();
    assert_eq!(saved(&reopened).await.as_deref(), Some("swordfish-42"));
}

/// The real Argon2 cost, so it has to run on the vault's thread: a heartbeat
/// on this single-threaded runtime keeps beating all the way through.
#[tokio::test]
async fn argon2_never_stalls_the_callers_thread() {
    let fixture = Fixture::new();
    let vault = Vault::open(
        fixture.path.clone(),
        fixture.key_store.clone(),
        AutoLock::NEVER,
    );
    let beats = Arc::new(AtomicUsize::new(0));
    let counter = beats.clone();
    let heart = tokio::spawn(async move {
        loop {
            counter.fetch_add(1, Ordering::Relaxed);
            sleep(Duration::from_millis(1)).await;
        }
    });

    let _ = vault.create(MASTER.into()).await.unwrap();
    heart.abort();

    assert!(beats.load(Ordering::Relaxed) > 5);
    assert_eq!(
        serde_json::from_str::<Value>(&fixture.text()).unwrap()["kdf"],
        serde_json::json!({ "memory_kib": 65536, "iterations": 3, "parallelism": 1 })
    );
}

#[tokio::test]
async fn secrets_never_reach_debug_output_or_errors() {
    let fixture = Fixture::new();
    let (vault, recovery_key) = fixture.created().await;
    let printed_key = recovery_key.reveal().to_string();
    let password = vault.password(&fikus()).await.unwrap().unwrap();
    let new = vault
        .change_master_password(OTHER_MASTER.into())
        .await
        .unwrap();
    let vault_key = VaultKey::generate();

    let mut printed = vec![
        format!("{:?}", Secret::from(MASTER)),
        format!("{password:?}"),
        format!("{recovery_key:?}"),
        format!("{new:?}"),
        format!("{vault_key:?}"),
    ];
    assert!(printed.iter().all(|text| text.contains("[redacted]")));

    fixture.flip(&fixture.entry_pointer());
    let errors = vec![
        fixture.unlock().await.unwrap_err(),
        vault.password(&fikus()).await.unwrap_err(),
        Fixture::new()
            .vault()
            .create("swordfish".into())
            .await
            .unwrap_err(),
        file::parse(MASTER.as_bytes()).err().unwrap(),
        file::parse(format!("{{\"format\":\"{MASTER}\"}}").as_bytes())
            .err()
            .unwrap(),
    ];
    for error in errors {
        printed.push(format!("{error}"));
        printed.push(format!("{error:?}"));
    }

    let hex_key = data_encoding::HEXLOWER.encode(vault_key.expose());
    for text in &printed {
        for secret in [
            MASTER,
            OTHER_MASTER,
            "swordfish",
            printed_key.as_str(),
            hex_key.as_str(),
        ] {
            assert!(!text.contains(secret), "{text} leaks {secret}");
        }
    }
}

fn alda() -> EntryKey {
    EntryKey::new("mud.example.org", "Alda")
}

impl Fixture {
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.path).unwrap()
    }

    /// A second device holding a copy of this vault, unlocked with `MASTER`
    /// so its key is cached there.
    async fn second_device(&self) -> (Fixture, Vault) {
        let device = Fixture::new();
        fs::copy(&self.path, &device.path).unwrap();
        let vault = device.vault();
        let _ = vault.unlock_with_password(MASTER.into()).await.unwrap();
        (device, vault)
    }

    fn take_merge(&self, base: &[u8], ours: &Fixture, theirs: &Fixture, theirs_newer: bool) {
        let merged = merge_files(Some(base), &ours.bytes(), &theirs.bytes(), theirs_newer).unwrap();
        fs::write(&self.path, merged).unwrap();
    }
}

/// The mac changes its master password while the iPad, offline, saves a new
/// password under the old key; then the iPad's merge lands on both.
async fn offline_save_across_a_password_change() -> (Fixture, Vault, Fixture, Vault) {
    let mac = Fixture::new();
    let (mac_vault, _) = mac.created().await;
    let (ipad, ipad_vault) = mac.second_device().await;
    let base = mac.bytes();
    ipad_vault
        .set_password(alda(), "offline-7".into())
        .await
        .unwrap();
    let _ = mac_vault
        .change_master_password(OTHER_MASTER.into())
        .await
        .unwrap();
    let their_copy = Fixture::new();
    fs::copy(&mac.path, &their_copy.path).unwrap();
    ipad.take_merge(&base, &ipad, &their_copy, false);
    fs::copy(&ipad.path, &mac.path).unwrap();
    (mac, mac_vault, ipad, ipad_vault)
}

#[tokio::test]
async fn a_merge_across_a_password_change_breaks_nothing_and_counts_what_waits() {
    let (_mac, vault, _ipad, _) = offline_save_across_a_password_change().await;

    assert_eq!(vault.entries().await.unwrap(), [fikus()]);
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
    assert_eq!(vault.stranded().await.unwrap(), 1);
}

#[tokio::test]
async fn the_device_that_saved_it_rescues_it_by_unlocking() {
    let (_mac, _, _ipad, vault) = offline_save_across_a_password_change().await;
    assert!(matches!(
        vault.unlock_from_device().await,
        Err(Error::NotCached)
    ));

    let _ = vault
        .unlock_with_password(OTHER_MASTER.into())
        .await
        .unwrap();

    assert_eq!(vault.entries().await.unwrap(), [alda(), fikus()]);
    assert_eq!(vault.stranded().await.unwrap(), 0);
    let rescued = vault.password(&alda()).await.unwrap().unwrap();
    assert_eq!(rescued.expose(), "offline-7");
}

#[tokio::test]
async fn any_device_rescues_it_with_the_old_password() {
    let (_mac, vault, _ipad, _) = offline_save_across_a_password_change().await;

    assert!(matches!(
        vault
            .rescue_with_password("not the old password".into())
            .await,
        Err(Error::WrongPassword)
    ));
    assert_eq!(vault.stranded().await.unwrap(), 1);

    assert_eq!(vault.rescue_with_password(MASTER.into()).await.unwrap(), 1);
    assert_eq!(vault.stranded().await.unwrap(), 0);
    let rescued = vault.password(&alda()).await.unwrap().unwrap();
    assert_eq!(rescued.expose(), "offline-7");
}

#[tokio::test]
async fn another_password_change_keeps_what_waits_for_the_old_one() {
    let (mac, vault, _ipad, _) = offline_save_across_a_password_change().await;

    let _ = vault
        .change_master_password("a third long and unusual passphrase of ours".into())
        .await
        .unwrap();

    assert_eq!(vault.stranded().await.unwrap(), 1);
    assert_eq!(vault.rescue_with_password(MASTER.into()).await.unwrap(), 1);
    assert!(!mac.text().contains("stranded"));
}

#[tokio::test]
async fn a_rescue_on_one_device_holds_against_a_merge_with_another() {
    let (mac, vault, ipad, _) = offline_save_across_a_password_change().await;
    let before_the_rescue = ipad.bytes();
    assert_eq!(vault.rescue_with_password(MASTER.into()).await.unwrap(), 1);

    ipad.take_merge(&before_the_rescue, &ipad, &mac, false);

    assert!(!ipad.text().contains("stranded"));
}

#[tokio::test]
async fn nothing_stranded_leaves_no_trace_of_the_old_key() {
    let mac = Fixture::new();
    let (mac_vault, _) = mac.created().await;
    let (ipad, _ipad_vault) = mac.second_device().await;
    let base = mac.bytes();
    let _ = mac_vault
        .change_master_password(OTHER_MASTER.into())
        .await
        .unwrap();

    ipad.take_merge(&base, &ipad, &mac, false);

    assert_eq!(ipad.text(), mac.text());
}

#[tokio::test]
async fn two_devices_with_one_key_merge_entry_by_entry() {
    let mac = Fixture::new();
    let (mac_vault, _) = mac.created().await;
    let (ipad, ipad_vault) = mac.second_device().await;
    let base = mac.bytes();
    ipad_vault
        .set_password(alda(), "from-the-ipad".into())
        .await
        .unwrap();
    mac_vault.remove_password(fikus()).await.unwrap();

    ipad.take_merge(&base, &ipad, &mac, false);

    assert_eq!(ipad_vault.entries().await.unwrap(), [alda()]);
}

#[tokio::test]
async fn two_password_changes_keep_the_loser_for_its_own_password() {
    let mac = Fixture::new();
    let (mac_vault, _) = mac.created().await;
    let (ipad, ipad_vault) = mac.second_device().await;
    let base = mac.bytes();
    let third = "a third long and unusual passphrase of ours";
    let _ = mac_vault
        .change_master_password(OTHER_MASTER.into())
        .await
        .unwrap();
    let _ = ipad_vault
        .change_master_password(third.into())
        .await
        .unwrap();

    ipad.take_merge(&base, &ipad, &mac, true);
    fs::copy(&ipad.path, &mac.path).unwrap();

    assert_eq!(mac_vault.stranded().await.unwrap(), 1);
    assert_eq!(
        mac_vault.rescue_with_password(third.into()).await.unwrap(),
        1
    );
    assert_eq!(saved(&mac_vault).await.as_deref(), Some("swordfish-42"));
}

#[tokio::test]
async fn an_entry_that_wont_open_costs_only_itself() {
    let mac = Fixture::new();
    let (vault, _) = mac.created().await;
    let other = Fixture::new();
    let (other_vault, _) = other.created().await;
    drop(other_vault);
    let foreign: Value = serde_json::from_str(&other.text()).unwrap();
    let (_, sealed) = foreign["entries"]
        .as_object()
        .unwrap()
        .iter()
        .next()
        .unwrap();
    let id = "0".repeat(32);
    mac.edit(|json| json["entries"][&id] = sealed.clone());

    assert_eq!(vault.entries().await.unwrap(), [fikus()]);
    assert_eq!(vault.stranded().await.unwrap(), 1);
}

#[test]
fn a_damaged_vault_is_not_merged() {
    let error = merge_files(None, b"{ not json", b"{ not json either", false).unwrap_err();

    assert!(matches!(error, Error::Corrupt(_)));
}

/// A vault written by the first release of format 1. If this stops opening,
/// the change would strand every player's passwords. Regenerate it only with a
/// new format version and a migration.
#[tokio::test]
async fn format_one_stays_readable() {
    let fixture = Fixture::new();
    fs::write(&fixture.path, include_str!("testdata/format-1.vault")).unwrap();

    let vault = fixture.vault();
    let _ = vault.unlock_with_password(MASTER.into()).await.unwrap();
    assert_eq!(saved(&vault).await.as_deref(), Some("swordfish-42"));
}

/// Regenerates the fixture above:
/// `cargo test -p mudling-core write_format_fixture -- --ignored`
#[tokio::test]
#[ignore = "only for a deliberate format change"]
async fn write_format_fixture() {
    let fixture = Fixture::new();
    fixture.created().await;
    let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/vault/testdata");
    fs::create_dir_all(&target).unwrap();
    fs::copy(&fixture.path, target.join("format-1.vault")).unwrap();
}
