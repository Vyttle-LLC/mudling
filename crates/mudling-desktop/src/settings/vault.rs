//! The Vault section: creating or unlocking the vault, the saved passwords
//! with Reveal and Change, auto-lock, a new master password and what's left
//! over from an old one.
//!
//! The sheet holds the only [`Vault`] handle in the app, so one thread and one
//! key serve everything. Every call is a task on the vault's own thread, and
//! nothing here does cryptography or waits on the disk.
//!
//! A password reaches this file as a [`Secret`] and leaves the screen again
//! on Hide, after [`REVEAL_FOR`], or when the player moves on or the vault
//! locks. Drawing one takes a plain copy, as gpui's text does, and that copy
//! can't be wiped. A typed password stays in a masked field until
//! `take_secret` wipes it.

use super::appearance::toggled;
use super::logs::switch_row;
use super::saved::LockAfter;
use super::{
    Pane, Section, SettingsSheet, SheetEvent, chosen_ground, control, muted_line, question_line,
    section_heading, text_button,
};
use crate::assets::UI_FONT;
use crate::input::{CommandInput, InputEvent};
use crate::theme::{CHAT_FONTS, Theme};
use gpui::{
    ClipboardItem, Context, Div, Entity, FontWeight, Role, SharedString, Stateful, Task, Window,
    div, prelude::*, px,
};
use mudling_core::vault::{
    DEVICE_KEY_GUARDED, DeviceCache, EntryKey, Error, KeepPassword, RecoveryKey, RecoveryKeyIssued,
    Secret, Vault, VaultState,
};
use std::collections::VecDeque;
use std::future::Future;
use std::time::Duration;

/// How long a revealed password stays on the screen.
const REVEAL_FOR: Duration = Duration::from_secs(15);
const NO_VAULT: &str = "The vault isn't running in this window.";
const NOT_CREATED: &str = "Set a master password to keep your server passwords in an encrypted \
                           vault. It's never stored, so choose one you'll remember.";
const LOCKED: &str = "The vault is locked.";
const NOTHING_SAVED: &str = "No saved passwords yet.";
const WORKING: &str = "Working…";
/// What auto-lock does today, said exactly.
const AUTO_LOCK_TODAY: &str = "Locking takes the vault's key out of memory. A password already \
                               used to log in to a game stays in this app's memory as plain text \
                               after a lock, for as long as that game's session is open.";
const KEEP_ON_DEVICE: &str = "Keep a copy of the key on this device";
/// Only a macOS release build guards the device's copy of the key; anywhere
/// else that copy opens the vault with no prompt.
const DEVICE_COPY: &str = if DEVICE_KEY_GUARDED {
    GUARDED_COPY
} else {
    UNGUARDED_COPY
};
const GUARDED_COPY: &str = "Only Touch ID, or your login password, opens this device's copy, so \
                            Unlock on this device and auto-login ask first. Cancel, and the vault \
                            stays locked until you use the master password.";
#[cfg(target_os = "macos")]
const UNGUARDED_COPY: &str = "This development build doesn't guard this device's copy with Touch \
                              ID, so Unlock on this device and auto-login open the vault again \
                              without asking, and locking doesn't stop someone using this \
                              computer. Turn the copy off to have every unlock take the master \
                              password.";
#[cfg(not(target_os = "macos"))]
const UNGUARDED_COPY: &str = "MUDling on Windows and Linux doesn't guard this device's copy, so \
                              Unlock on this device and auto-login open the vault again without \
                              asking, and locking doesn't stop someone using this computer. Turn \
                              the copy off to have every unlock take the master password.";
const NO_DEVICE_COPY: &str = "This device keeps no copy, so every unlock, auto-login's too, takes \
                              the master password or the recovery key.";
const NEW_MASTER_NOTE: &str = "A new master password protects future commits only. Old copies of \
                               the vault in git history still open with the old one, so if it \
                               may have leaked, change your passwords on the servers too. You'll \
                               get a new recovery key, and the old key stops working.";
const RECOVERY_NOTE: &str = "This is your recovery key. It opens the vault if you lose your \
                             master password. Print it or keep it in a password manager: it \
                             won't be shown again.";
const NOT_CACHED_NOTE: &str = "This device couldn't keep the vault's key, so the next unlock \
                               will ask for the master password.";
const MASTER: &str = "Master password";
const REPEAT: &str = "Repeat it";
const RECOVERY: &str = "Recovery key";
const OLD_MASTER: &str = "Old master password";
const PASSWORD: &str = "Password";
/// Stands for any password, whatever its length.
const HIDDEN: &str = "••••••••";

fn words(error: &Error) -> String {
    match error {
        Error::WrongPassword => "That isn't the master password.".to_owned(),
        Error::WrongRecoveryKey => "That isn't this vault's recovery key.".to_owned(),
        Error::WeakPassword { warning: None } => {
            "That master password is too weak. Try a longer one.".to_owned()
        }
        Error::WeakPassword {
            warning: Some(warning),
        } => format!("That master password is too weak: {warning}"),
        Error::NotCached => "This device has no key saved. Use the master password.".to_owned(),
        Error::Cancelled => {
            "Unlocking on this device was cancelled. Use the master password, or try again."
                .to_owned()
        }
        Error::ChangedDuringSave => {
            "The vault changed while saving, usually from a sync. Nothing was lost, so try again."
                .to_owned()
        }
        Error::NoCurrentPassword => {
            "No readable password is saved for that character, so the other one stays.".to_owned()
        }
        error => format!("The vault couldn't do that: {error}."),
    }
}

/// Which form is open under the section's other rows.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Form {
    RecoveryUnlock,
    Add,
    Change(EntryKey),
    NewMaster,
    Rescue,
}

/// A password that's on the screen.
struct Revealed {
    entry: EntryKey,
    password: Secret,
}

/// Two passwords for one character, which only the player can choose between.
struct Conflicts {
    /// Settling each one takes it again.
    old_master: Secret,
    waiting: VecDeque<(EntryKey, Secret)>,
    /// The stranded password is on the screen, beside the current one.
    comparing: bool,
}

struct Fields {
    master: Entity<CommandInput>,
    repeat: Entity<CommandInput>,
    recovery: Entity<CommandInput>,
    old_master: Entity<CommandInput>,
    password: Entity<CommandInput>,
    server: Entity<CommandInput>,
    character: Entity<CommandInput>,
}

impl Fields {
    fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> Fields {
        let mut watched = |field: fn(&mut Context<CommandInput>) -> CommandInput| {
            let field = cx.new(field);
            cx.subscribe_in(&field, window, |sheet, _, event, window, cx| {
                if let InputEvent::Submitted(_) = event {
                    sheet.submit_vault(window, cx);
                }
            })
            .detach();
            field
        };
        Fields {
            master: watched(|cx| CommandInput::secret_field(MASTER, MASTER, cx)),
            repeat: watched(|cx| CommandInput::secret_field(REPEAT, REPEAT, cx)),
            recovery: watched(|cx| CommandInput::secret_field(RECOVERY, RECOVERY, cx)),
            old_master: watched(|cx| CommandInput::secret_field(OLD_MASTER, OLD_MASTER, cx)),
            password: watched(|cx| CommandInput::secret_field(PASSWORD, PASSWORD, cx)),
            server: watched(|cx| CommandInput::field("Server", "mud.example.org", cx)),
            character: watched(|cx| CommandInput::field("Character", "Character", cx)),
        }
    }

    fn secret_fields(&self) -> [&Entity<CommandInput>; 5] {
        [
            &self.master,
            &self.repeat,
            &self.recovery,
            &self.old_master,
            &self.password,
        ]
    }
}

/// Shows the vault's state as it changes.
#[cfg(not(test))]
fn follow_states(
    vault: &Vault,
    window: &mut Window,
    cx: &mut Context<SettingsSheet>,
) -> Option<Task<()>> {
    let mut states = vault.subscribe();
    Some(cx.spawn_in(window, async move |sheet, cx| {
        while states.changed().await.is_ok() {
            let state = *states.borrow_and_update();
            let shown = sheet.update_in(cx, |sheet, window, cx| {
                sheet.vault_state_changed(state, window, cx);
            });
            if shown.is_err() {
                return;
            }
        }
    }))
}

/// gpui's test scheduler refuses a wake from another thread, so a test asks
/// after the state itself.
#[cfg(test)]
fn follow_states(_: &Vault, _: &mut Window, _: &mut Context<SettingsSheet>) -> Option<Task<()>> {
    None
}

/// The vault's answer to `call`.
#[cfg(not(test))]
async fn answer<T>(call: impl Future<Output = T>) -> T {
    call.await
}

/// Waits right here, for the reason [`follow_states`] gives.
#[cfg(test)]
async fn answer<T>(call: impl Future<Output = T>) -> T {
    futures::executor::block_on(call)
}

fn take(field: &Entity<CommandInput>, cx: &mut Context<SettingsSheet>) -> Secret {
    field.update(cx, |field, cx| field.take_secret(cx))
}

fn typed(field: &Entity<CommandInput>, cx: &Context<SettingsSheet>) -> String {
    field.read(cx).content().trim().to_owned()
}

pub(super) struct VaultPane {
    vault: Option<Vault>,
    state: VaultState,
    entries: Vec<EntryKey>,
    stranded: usize,
    lock_after: LockAfter,
    keep_on_device: bool,
    form: Option<Form>,
    fields: Fields,
    revealed: Option<Revealed>,
    /// Shown once, and kept through a lock: nothing can show it again.
    recovery: Option<RecoveryKey>,
    conflicts: Option<Conflicts>,
    /// The entry whose removal is being asked about.
    removing: Option<EntryKey>,
    problem: Option<SharedString>,
    note: Option<SharedString>,
    /// A server that's waiting for the vault so it can log in.
    waiting_host: Option<SharedString>,
    busy: bool,
    job: Option<Task<()>>,
    /// A job that hands back a recovery key, which exists nowhere else, so
    /// nothing cancels it.
    issuing: Option<Task<()>>,
    listing: Option<Task<()>>,
    revealing: Option<Task<()>>,
    hiding: Option<Task<()>>,
    _watching: Option<Task<()>>,
}

impl VaultPane {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> VaultPane {
        VaultPane {
            vault: None,
            state: VaultState::Locked,
            entries: Vec::new(),
            stranded: 0,
            lock_after: LockAfter::default(),
            keep_on_device: true,
            form: None,
            fields: Fields::new(window, cx),
            revealed: None,
            recovery: None,
            conflicts: None,
            removing: None,
            problem: None,
            note: None,
            waiting_host: None,
            busy: false,
            job: None,
            issuing: None,
            listing: None,
            revealing: None,
            hiding: None,
            _watching: None,
        }
    }
}

impl SettingsSheet {
    /// The vault to show, in place of the one before: its thread stops with
    /// this, the last handle, and wipes the key. What's on the screen goes too.
    pub fn use_vault(
        &mut self,
        vault: Vault,
        lock_after: LockAfter,
        keep_on_device: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault._watching = follow_states(&vault, window, cx);
        let state = vault.state();
        self.vault.state = state;
        self.vault.vault = Some(vault);
        self.vault.lock_after = lock_after;
        self.vault.keep_on_device = keep_on_device;
        self.vault_state_changed(state, window, cx);
    }

    /// The handle the shell logs in with.
    pub fn vault(&self) -> Option<Vault> {
        self.vault.vault.clone()
    }

    /// Whether a server is waiting for the player to unlock the vault.
    pub(super) fn is_waited_on(&self) -> bool {
        self.vault.waiting_host.is_some()
    }

    /// Says which server is waiting for an unlock, and shows the section.
    pub fn wait_for_unlock(&mut self, host: &str, cx: &mut Context<SettingsSheet>) {
        self.vault.waiting_host = Some(host.to_owned().into());
        self.select(Section::Vault, cx);
    }

    pub fn stop_waiting_for_unlock(&mut self, cx: &mut Context<SettingsSheet>) {
        self.vault.waiting_host = None;
        cx.notify();
    }

    /// Takes the passwords off the screen. A typed one stays, because the
    /// player may have left only to look something up.
    pub fn hide_passwords(&mut self, cx: &mut Context<SettingsSheet>) {
        let pane = &mut self.vault;
        pane.revealing = None;
        pane.hiding = None;
        pane.revealed = None;
        if let Some(conflicts) = &mut pane.conflicts {
            conflicts.comparing = false;
        }
        cx.notify();
    }

    /// Hides what's on the screen and wipes what's typed.
    pub fn forget_typed(&mut self, cx: &mut Context<SettingsSheet>) {
        self.hide_passwords(cx);
        let fields: Vec<_> = (self.vault.fields.secret_fields().into_iter().cloned()).collect();
        for field in &fields {
            drop(take(field, cx));
        }
    }

    /// Everything a locked vault must not leave behind, except a recovery key
    /// the player hasn't seen to.
    fn forget_vault_secrets(&mut self, cx: &mut Context<SettingsSheet>) {
        self.forget_typed(cx);
        let pane = &mut self.vault;
        pane.conflicts = None;
        pane.removing = None;
        pane.form = None;
        pane.entries.clear();
        pane.stranded = 0;
        pane.job = None;
        pane.listing = None;
        pane.busy = false;
    }

    fn vault_state_changed(
        &mut self,
        state: VaultState,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault.state = state;
        self.keep_focus_in_sheet(window, cx);
        if state == VaultState::Unlocked {
            self.vault.waiting_host = None;
            self.list_vault(cx);
            cx.emit(SheetEvent::VaultUnlocked);
        } else {
            self.forget_vault_secrets(cx);
        }
        cx.notify();
    }

    /// Moves focus off a stop that's about to go with its form, so the sheet
    /// keeps the keyboard. A focused stop means the sheet is on the screen.
    fn keep_focus_in_sheet(&self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        if self.a_stop_of_the_section_has_focus(window) {
            self.focus_tab(Section::Vault, window, cx);
        }
    }

    fn a_stop_of_the_section_has_focus(&self, window: &Window) -> bool {
        self.section == Section::Vault
            && self
                .stops
                .in_use()
                .iter()
                .any(|stop| stop.is_focused(window))
    }

    /// Reads the list again: it can change under a sync.
    pub(super) fn list_vault(&mut self, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        if self.vault.state != VaultState::Unlocked {
            return;
        }
        self.vault.listing = Some(cx.spawn(async move |sheet, cx| {
            let listed =
                async { Ok::<_, Error>((vault.entries().await?, vault.stranded().await?)) };
            let listed = answer(listed).await;
            let _ = sheet.update(cx, |sheet, cx| {
                match listed {
                    Ok((entries, stranded)) => {
                        sheet.vault.entries = entries;
                        sheet.vault.stranded = stranded;
                    }
                    Err(error) => sheet.vault.problem = Some(words(&error).into()),
                }
                cx.notify();
            });
        }));
    }

    /// Runs one call on the vault, then `done`. One runs at a time, since
    /// Argon2 queues everything behind it anyway.
    fn vault_job<T: 'static>(
        &mut self,
        work: impl Future<Output = Result<T, Error>> + 'static,
        done: impl FnOnce(&mut SettingsSheet, T, &mut Window, &mut Context<SettingsSheet>) + 'static,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault.busy = true;
        self.vault.problem = None;
        self.vault.note = None;
        self.vault.job = Some(cx.spawn_in(window, async move |sheet, cx| {
            let outcome = answer(work).await;
            let _ = sheet.update_in(cx, |sheet, window, cx| {
                sheet.vault.busy = false;
                match outcome {
                    Ok(value) => done(sheet, value, window, cx),
                    Err(error) => sheet.vault.problem = Some(words(&error).into()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn issued(
        &mut self,
        issued: RecoveryKeyIssued,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault.recovery = Some(issued.recovery_key);
        self.cached(issued.device_cache);
        self.finish_vault_form(window, cx);
    }

    /// The form did its job, so it goes, and the keyboard stays in the sheet.
    fn finish_vault_form(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.keep_focus_in_sheet(window, cx);
        self.close_vault_form(cx);
    }

    fn cached(&mut self, device_cache: DeviceCache) {
        if let DeviceCache::NotSaved(_) = device_cache {
            self.vault.note = Some(NOT_CACHED_NOTE.into());
        }
    }

    fn close_vault_form(&mut self, cx: &mut Context<SettingsSheet>) {
        self.vault.form = None;
        let fields = &self.vault.fields;
        let plain = [fields.server.clone(), fields.character.clone()];
        for field in plain {
            field.update(cx, |field, cx| drop(field.take_secret(cx)));
        }
        let fields: Vec<_> = (self.vault.fields.secret_fields().into_iter().cloned()).collect();
        for field in &fields {
            drop(take(field, cx));
        }
    }

    fn open_vault_form(
        &mut self,
        form: Form,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.close_vault_form(cx);
        let first = match &form {
            Form::RecoveryUnlock => self.vault.fields.recovery.clone(),
            Form::Add => {
                self.fill_new_entry(window, cx);
                self.vault.fields.password.clone()
            }
            Form::Change(_) => self.vault.fields.password.clone(),
            Form::NewMaster => self.vault.fields.master.clone(),
            Form::Rescue => self.vault.fields.old_master.clone(),
        };
        self.vault.form = Some(form);
        self.vault.problem = None;
        let handle = gpui::Focusable::focus_handle(first.read(cx), cx);
        window.focus(&handle, cx);
        self.reveal = Some(handle);
        cx.notify();
    }

    /// Starts Add from what's most likely: the game in play, or the first
    /// saved server, and that server's first character.
    fn fill_new_entry(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let host = self.playing.clone();
        let server = match &host {
            Some(host) => self.servers.iter().find(|server| server.host == *host),
            None => self.servers.first(),
        };
        let (host, character) = (
            server.map(|server| server.host.to_string()),
            server.and_then(|server| server.characters.first().map(ToString::to_string)),
        );
        let fields = &self.vault.fields;
        for (field, text) in [(&fields.server, host), (&fields.character, character)] {
            let text = text.unwrap_or_default();
            let end = text.len();
            field.update(cx, |field, cx| field.show(text, end, window, cx));
        }
    }

    /// Return in a field: does what the open form is for.
    fn submit_vault(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        if self.vault.busy {
            return;
        }
        match (self.vault.state, self.vault.form.clone()) {
            (VaultState::NotCreated, _) => self.create_vault(window, cx),
            (VaultState::Locked, Some(Form::RecoveryUnlock)) => self.unlock_recovery(window, cx),
            (VaultState::Locked, _) => self.unlock_master(window, cx),
            (VaultState::Unlocked, Some(Form::Add)) => self.save_new_entry(window, cx),
            (VaultState::Unlocked, Some(Form::Change(entry))) => {
                self.save_password(entry, window, cx);
            }
            (VaultState::Unlocked, Some(Form::NewMaster)) => self.change_master(window, cx),
            (VaultState::Unlocked, Some(Form::Rescue)) => self.rescue(window, cx),
            (VaultState::Unlocked, _) => {}
        }
    }

    /// The two fields of a new master password, if they agree.
    fn new_master(&mut self, cx: &mut Context<SettingsSheet>) -> Option<Secret> {
        let (master, repeat) = (
            self.vault.fields.master.clone(),
            self.vault.fields.repeat.clone(),
        );
        let (master, repeat) = (take(&master, cx), take(&repeat, cx));
        if master.expose() != repeat.expose() {
            self.vault.problem = Some("Those two don't match.".into());
            cx.notify();
            return None;
        }
        Some(master)
    }

    fn create_vault(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        if let Some(master) = self.new_master(cx) {
            self.vault_job(
                async move { vault.create(master).await },
                |sheet, issued, window, cx| sheet.issued(issued, window, cx),
                window,
                cx,
            );
            self.vault.issuing = self.vault.job.take();
        }
    }

    fn change_master(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        if let Some(master) = self.new_master(cx) {
            self.vault_job(
                async move { vault.change_master_password(master).await },
                |sheet, issued, window, cx| sheet.issued(issued, window, cx),
                window,
                cx,
            );
            self.vault.issuing = self.vault.job.take();
        }
    }

    fn unlock_master(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        let master = take(&self.vault.fields.master.clone(), cx);
        self.vault_job(
            async move { vault.unlock_with_password(master).await },
            |sheet, cached, _, _| sheet.cached(cached),
            window,
            cx,
        );
    }

    fn unlock_recovery(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        let typed = take(&self.vault.fields.recovery.clone(), cx);
        self.vault_job(
            async move {
                let key = RecoveryKey::parse(typed.expose())?;
                vault.unlock_with_recovery_key(key).await
            },
            |sheet, cached, _, _| sheet.cached(cached),
            window,
            cx,
        );
    }

    fn unlock_here(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        self.vault_job(
            async move { vault.unlock_from_device().await },
            |_, (), _, _| {},
            window,
            cx,
        );
    }

    fn lock_vault(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        self.vault_job(
            async move { vault.lock().await },
            |_, (), _, _| {},
            window,
            cx,
        );
    }

    fn choose_lock_after(
        &mut self,
        lock_after: LockAfter,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault.lock_after = lock_after;
        cx.emit(SheetEvent::LockAfter(lock_after));
        cx.notify();
        if let Some(vault) = self.vault.vault.clone() {
            cx.spawn_in(window, async move |_, _| {
                let chosen = answer(vault.set_auto_lock(lock_after.auto_lock())).await;
                if let Err(error) = chosen {
                    eprintln!("couldn't set auto-lock: {error}");
                }
            })
            .detach();
        }
    }

    fn toggle_keep_on_device(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone().filter(|_| !self.vault.busy) else {
            return;
        };
        let keep = !self.vault.keep_on_device;
        self.vault.keep_on_device = keep;
        cx.emit(SheetEvent::KeepKeyOnDevice(keep));
        let chosen = vault.set_keep_on_device(keep);
        self.vault_job(
            chosen,
            |sheet, saved, _, _| {
                if let Some(saved) = saved {
                    sheet.cached(saved);
                }
            },
            window,
            cx,
        );
    }

    /// Asks the vault to look at its file again if it had none: the file
    /// lands after sync names its new place, or arrives with a pull. Locking
    /// a vault with no key only reads the file.
    pub(super) fn look_for_vault_file(&mut self, cx: &mut Context<SettingsSheet>) {
        if self.vault.state != VaultState::NotCreated || self.vault.busy {
            return;
        }
        if let Some(vault) = self.vault.vault.clone() {
            cx.spawn(async move |_, _| drop(answer(vault.lock()).await))
                .detach();
        }
    }

    fn save_new_entry(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let fields = &self.vault.fields;
        let (server, character) = (typed(&fields.server, cx), typed(&fields.character, cx));
        if server.is_empty() || character.is_empty() {
            self.vault.problem = Some("A saved password needs a server and a character.".into());
            cx.notify();
            return;
        }
        self.save_password(EntryKey::new(server, character), window, cx);
    }

    fn save_password(
        &mut self,
        entry: EntryKey,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        let password = take(&self.vault.fields.password.clone(), cx);
        if password.expose().is_empty() {
            self.vault.problem = Some("Type the password to save.".into());
            cx.notify();
            return;
        }
        self.vault_job(
            async move { vault.set_password(entry, password).await },
            |sheet, (), window, cx| {
                sheet.finish_vault_form(window, cx);
                sheet.list_vault(cx);
            },
            window,
            cx,
        );
    }

    fn remove_entry(
        &mut self,
        entry: EntryKey,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(vault) = self.vault.vault.clone().filter(|_| !self.vault.busy) else {
            return;
        };
        self.vault.removing = None;
        // This stop goes with the question.
        self.focus_tab(Section::Vault, window, cx);
        self.vault_job(
            async move { vault.remove_password(entry).await },
            |sheet, _, _, cx| sheet.list_vault(cx),
            window,
            cx,
        );
    }

    /// Puts a saved password on the screen for [`REVEAL_FOR`]. The call is
    /// dropped if the vault locks first, so nothing appears on a later unlock.
    fn reveal_password(
        &mut self,
        entry: EntryKey,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.hide_passwords(cx);
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        self.vault.revealing = Some(cx.spawn_in(window, async move |sheet, cx| {
            let password = answer(vault.password(&entry)).await;
            let _ = sheet.update(cx, |sheet, cx| {
                match password {
                    Ok(Some(password)) => {
                        sheet.vault.revealed = Some(Revealed { entry, password });
                        sheet.hide_after_a_while(cx);
                    }
                    Ok(None) => sheet.vault.problem = Some("That password is gone.".into()),
                    Err(error) => sheet.vault.problem = Some(words(&error).into()),
                }
                cx.notify();
            });
        }));
    }

    fn hide_after_a_while(&mut self, cx: &mut Context<SettingsSheet>) {
        let timer = cx.background_executor().timer(REVEAL_FOR);
        self.vault.hiding = Some(cx.spawn(async move |sheet, cx| {
            timer.await;
            let _ = sheet.update(cx, |sheet, cx| sheet.hide_passwords(cx));
        }));
    }

    /// Brings back what the old master password opens, then asks about what
    /// it can't decide alone.
    fn rescue(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        let old_master = take(&self.vault.fields.old_master.clone(), cx);
        self.vault_job(
            async move {
                let again = || Secret::from(old_master.expose());
                let brought_back = vault.rescue_with_password(again()).await?;
                let conflicts = vault.conflicts(again()).await?;
                Ok((old_master, brought_back, conflicts))
            },
            |sheet, (old_master, brought_back, conflicts), window, cx| {
                sheet.finish_vault_form(window, cx);
                sheet.vault.note = Some(brought_back_words(brought_back).into());
                if !conflicts.is_empty() {
                    sheet.vault.conflicts = Some(Conflicts {
                        old_master,
                        waiting: conflicts.into(),
                        comparing: false,
                    });
                }
                sheet.list_vault(cx);
            },
            window,
            cx,
        );
    }

    fn settle(&mut self, keep: KeepPassword, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(vault) = self.vault.vault.clone() else {
            return;
        };
        let Some(conflicts) = &self.vault.conflicts else {
            return;
        };
        let Some((entry, _)) = conflicts.waiting.front() else {
            return;
        };
        let (old_master, entry) = (Secret::from(conflicts.old_master.expose()), entry.clone());
        self.hide_passwords(cx);
        self.vault_job(
            async move { vault.settle_conflict(old_master, entry, keep).await },
            |sheet, (), window, cx| {
                let conflicts = sheet.vault.conflicts.as_mut();
                let left = conflicts.map_or(0, |conflicts| {
                    conflicts.waiting.pop_front();
                    conflicts.waiting.len()
                });
                if left == 0 {
                    sheet.keep_focus_in_sheet(window, cx);
                    sheet.vault.conflicts = None;
                }
                sheet.list_vault(cx);
            },
            window,
            cx,
        );
    }

    fn compare(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let Some(conflicts) = &self.vault.conflicts else {
            return;
        };
        let Some((entry, _)) = conflicts.waiting.front() else {
            return;
        };
        let entry = entry.clone();
        self.reveal_password(entry, window, cx);
        if let Some(conflicts) = &mut self.vault.conflicts {
            conflicts.comparing = true;
        }
    }

    fn vault_button(
        id: &'static str,
        label: &'static str,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
        act: impl Fn(&mut SettingsSheet, &mut Window, &mut Context<SettingsSheet>) + 'static,
    ) -> Stateful<Div> {
        text_button(id, label, &pane.stop(cx), theme).on_click(cx.listener(
            move |sheet, _, window, cx| {
                if !sheet.vault.busy {
                    act(sheet, window, cx);
                }
            },
        ))
    }

    pub(super) fn fill_vault(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        if self.vault.vault.is_none() && self.vault.state != VaultState::Unlocked {
            pane.row(muted_line(NO_VAULT, theme));
            return;
        }
        if let Some(host) = &self.vault.waiting_host {
            let waiting = format!(
                "{host} is waiting to log in. Unlock the vault, or close settings to connect \
                 without logging in."
            );
            pane.row(question_line(waiting, theme));
        }
        if let Some(problem) = self.vault.problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "vault-problem".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        if let Some(note) = self.vault.note.clone() {
            pane.row(muted_line(note, theme).role(Role::Status));
        }
        if self.vault.busy {
            pane.row(muted_line(WORKING, theme).role(Role::Status));
        }
        self.fill_recovery(pane, theme, cx);
        match self.vault.state {
            VaultState::NotCreated => self.fill_create(pane, theme, cx),
            VaultState::Locked => self.fill_locked(pane, theme, cx),
            VaultState::Unlocked => self.fill_unlocked(pane, theme, cx),
        }
    }

    /// Plainly, and readable by a screen reader: this isn't a masked field,
    /// and the player has to be able to hear it to keep it.
    fn fill_recovery(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        let Some(key) = &self.vault.recovery else {
            return;
        };
        let key: SharedString = key.reveal().to_string().into();
        pane.row(section_heading("Recovery key", theme));
        pane.row(
            muted_line(RECOVERY_NOTE, theme)
                .role(Role::Alert)
                .text_color(theme.text),
        );
        pane.row(
            div()
                .id("recovery-key")
                .debug_selector(|| "recovery-key".to_owned())
                .role(Role::Label)
                .aria_label("Recovery key")
                .aria_value(key.clone())
                .pb_2()
                .font_family(CHAT_FONTS[0])
                .font_weight(FontWeight::MEDIUM)
                .text_size(px(15.))
                .text_color(theme.text)
                .child(key.clone()),
        );
        let copy = text_button("copy-recovery-key", "Copy", &pane.stop(cx), theme)
            .aria_label("Copy the recovery key")
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(key.to_string()));
            }));
        let saved = Self::vault_button(
            "recovery-key-saved",
            "I've saved it",
            pane,
            theme,
            cx,
            |sheet, window, cx| {
                sheet.vault.recovery = None;
                // This stop goes with the key.
                sheet.focus_tab(Section::Vault, window, cx);
                cx.notify();
            },
        );
        pane.row(div().pb_2().flex().gap_2().child(copy).child(saved));
    }

    fn fill_create(&mut self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(muted_line(NOT_CREATED, theme));
        let fields = (
            self.vault.fields.master.clone(),
            self.vault.fields.repeat.clone(),
        );
        self.fill_field(pane, &fields.0, cx);
        self.fill_field(pane, &fields.1, cx);
        let create = Self::vault_button(
            "vault-create",
            "Create vault",
            pane,
            theme,
            cx,
            Self::create_vault,
        );
        pane.row(div().pb_2().flex().child(create));
    }

    fn fill_locked(&mut self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(muted_line(LOCKED, theme));
        if self.vault.form == Some(Form::RecoveryUnlock) {
            let recovery = self.vault.fields.recovery.clone();
            self.fill_field(pane, &recovery, cx);
            let unlock = Self::vault_button(
                "vault-unlock-recovery",
                "Unlock with the recovery key",
                pane,
                theme,
                cx,
                Self::unlock_recovery,
            );
            pane.row(div().pb_2().flex().child(unlock));
            return;
        }
        let master = self.vault.fields.master.clone();
        self.fill_field(pane, &master, cx);
        let unlock = Self::vault_button(
            "vault-unlock",
            "Unlock",
            pane,
            theme,
            cx,
            Self::unlock_master,
        );
        let here = self.vault.keep_on_device.then(|| {
            Self::vault_button(
                "vault-unlock-here",
                "Unlock on this device",
                pane,
                theme,
                cx,
                Self::unlock_here,
            )
        });
        let recovery = Self::vault_button(
            "vault-use-recovery",
            "Use the recovery key",
            pane,
            theme,
            cx,
            |sheet, window, cx| sheet.open_vault_form(Form::RecoveryUnlock, window, cx),
        );
        pane.row(
            div()
                .pb_2()
                .flex()
                .gap_2()
                .child(unlock)
                .children(here)
                .child(recovery),
        );
    }

    fn fill_unlocked(&mut self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        self.fill_conflict(pane, theme, cx);
        self.fill_stranded(pane, theme, cx);
        pane.row(section_heading("Saved passwords", theme));
        if self.vault.entries.is_empty() {
            pane.row(muted_line(NOTHING_SAVED, theme));
        }
        for (number, entry) in self.vault.entries.clone().into_iter().enumerate() {
            let row = self.entry_row(number, &entry, pane, theme, cx);
            pane.row(row);
            if self.vault.form == Some(Form::Change(entry.clone())) {
                self.fill_password_form(pane, theme, cx);
            }
        }
        if self.vault.form == Some(Form::Add) {
            let fields = (
                self.vault.fields.server.clone(),
                self.vault.fields.character.clone(),
            );
            self.fill_field(pane, &fields.0, cx);
            self.fill_field(pane, &fields.1, cx);
            self.fill_password_form(pane, theme, cx);
        } else {
            let add = Self::vault_button(
                "vault-add",
                "Add a password",
                pane,
                theme,
                cx,
                |sheet, window, cx| sheet.open_vault_form(Form::Add, window, cx),
            );
            pane.row(div().pt_3().pb_2().flex().child(add));
        }
        self.fill_auto_lock(pane, theme, cx);
        pane.row(section_heading("Master password", theme));
        if self.vault.form == Some(Form::NewMaster) {
            pane.row(muted_line(NEW_MASTER_NOTE, theme));
            let fields = (
                self.vault.fields.master.clone(),
                self.vault.fields.repeat.clone(),
            );
            self.fill_field(pane, &fields.0, cx);
            self.fill_field(pane, &fields.1, cx);
            let change = Self::vault_button(
                "vault-master-save",
                "Change it",
                pane,
                theme,
                cx,
                Self::change_master,
            );
            let cancel = Self::cancel_button(pane, theme, cx);
            pane.row(div().pb_2().flex().gap_2().child(change).child(cancel));
        } else {
            let change = Self::vault_button(
                "vault-master",
                "Change master password",
                pane,
                theme,
                cx,
                |sheet, window, cx| sheet.open_vault_form(Form::NewMaster, window, cx),
            );
            pane.row(div().pb_2().flex().child(change));
        }
    }

    fn cancel_button(
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Stateful<Div> {
        Self::vault_button(
            "vault-form-cancel",
            "Cancel",
            pane,
            theme,
            cx,
            |sheet, window, cx| {
                sheet.close_vault_form(cx);
                // This stop goes with the form.
                sheet.focus_tab(Section::Vault, window, cx);
                cx.notify();
            },
        )
    }

    fn fill_password_form(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let password = self.vault.fields.password.clone();
        self.fill_field(pane, &password, cx);
        let save = Self::vault_button(
            "vault-form-save",
            "Save",
            pane,
            theme,
            cx,
            Self::submit_vault,
        );
        let cancel = Self::cancel_button(pane, theme, cx);
        pane.row(div().pb_2().flex().gap_2().child(save).child(cancel));
    }

    fn entry_row(
        &self,
        number: usize,
        entry: &EntryKey,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let name = format!("{} on {}", entry.character, entry.server);
        let shown = self
            .vault
            .revealed
            .as_ref()
            .filter(|shown| shown.entry == *entry);
        // A plain copy for the screen: see the module's note.
        let password: SharedString = shown.map_or(HIDDEN.into(), |shown| {
            shown.password.expose().to_owned().into()
        });
        let spoken = match shown {
            Some(_) => format!("{name}: {password}"),
            None => format!("{name}: password hidden"),
        };
        let about = div()
            .id(("vault-entry", number))
            .debug_selector(move || format!("vault-entry-{number}"))
            .role(Role::Label)
            .aria_value(spoken)
            .flex_1()
            .min_w_0()
            .font_family(UI_FONT)
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(name.clone()),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child(password),
            );
        let id = |what: &str| SharedString::from(format!("vault-{what}-{number}"));
        let (revealing, asked) = (shown.is_some(), entry.clone());
        let reveal = text_button(
            id("reveal"),
            if revealing { "Hide" } else { "Reveal" },
            &pane.stop(cx),
            theme,
        )
        .aria_label(format!(
            "{} the password for {name}",
            if revealing { "Hide" } else { "Reveal" }
        ))
        .on_click(cx.listener(move |sheet, _, window, cx| match revealing {
            true => sheet.hide_passwords(cx),
            false => sheet.reveal_password(asked.clone(), window, cx),
        }));
        let asked = entry.clone();
        let change = text_button(id("change"), "Change", &pane.stop(cx), theme)
            .aria_label(format!("Change the password for {name}"))
            .on_click(cx.listener(move |sheet, _, window, cx| {
                sheet.open_vault_form(Form::Change(asked.clone()), window, cx);
            }));
        let asking = self.vault.removing.as_ref() == Some(entry);
        let asked = entry.clone();
        let remove = (!asking).then(|| {
            text_button(id("remove"), "Remove", &pane.stop(cx), theme)
                .aria_label(format!("Remove the password for {name}"))
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.vault.removing = Some(asked.clone());
                    cx.notify();
                }))
        });
        let question = asking.then(|| {
            let asked = entry.clone();
            let keep = text_button(id("keep"), "Keep", &pane.stop(cx), theme)
                .aria_label(format!("Keep the password for {name}"))
                .on_click(cx.listener(|sheet, _, _, cx| {
                    sheet.vault.removing = None;
                    cx.notify();
                }));
            let confirm = text_button(id("confirm-remove"), "Remove", &pane.stop(cx), theme)
                .aria_label(format!("Remove the password for {name}"))
                .on_click(cx.listener(move |sheet, _, window, cx| {
                    sheet.remove_entry(asked.clone(), window, cx);
                }));
            div()
                .pt_2()
                .child(
                    muted_line(format!("Remove the password for {name}?"), theme)
                        .role(Role::Alert)
                        .text_color(theme.text),
                )
                .child(div().flex().gap_2().child(keep).child(confirm))
        });
        div()
            .py_3()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(about)
                    .child(reveal)
                    .child(change)
                    .children(remove),
            )
            .children(question)
    }

    fn fill_auto_lock(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        pane.row(section_heading("Auto-lock", theme));
        let chosen = self.vault.lock_after;
        let segments: Vec<_> = LockAfter::ALL
            .into_iter()
            .map(|lock_after| {
                let id = SharedString::from(format!("lock-after-{}", lock_after.label()));
                control(id, lock_after.label(), &pane.stop(cx), theme)
                    .role(Role::RadioButton)
                    .aria_toggled(toggled(lock_after == chosen))
                    .flex_1()
                    .h(px(34.))
                    .bg(chosen_ground(lock_after == chosen, theme))
                    .text_size(px(14.))
                    .text_color(theme.text)
                    .child(lock_after.label())
                    .on_click(cx.listener(move |sheet, _, window, cx| {
                        sheet.choose_lock_after(lock_after, window, cx);
                    }))
            })
            .collect();
        pane.row(
            div()
                .id("auto-lock-choice")
                .role(Role::RadioGroup)
                .aria_label("Lock the vault after")
                .w(px(440.))
                .max_w_full()
                .p_0p5()
                .mb_2()
                .flex()
                .gap_0p5()
                .rounded_lg()
                .bg(theme.segment)
                .children(segments),
        );
        pane.row(muted_line(AUTO_LOCK_TODAY, theme));
        let keep = self.vault.keep_on_device;
        let row = switch_row(
            "vault-keep-on-device".to_owned(),
            KEEP_ON_DEVICE,
            keep,
            Self::toggle_keep_on_device,
            pane,
            theme,
            cx,
        );
        pane.row(row);
        pane.row(muted_line(
            if keep { DEVICE_COPY } else { NO_DEVICE_COPY },
            theme,
        ));
        let lock = Self::vault_button("vault-lock", "Lock now", pane, theme, cx, Self::lock_vault);
        pane.row(div().pb_2().flex().child(lock));
    }

    fn fill_stranded(&mut self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        if self.vault.stranded == 0 || self.vault.conflicts.is_some() {
            return;
        }
        pane.row(section_heading(
            "Passwords from an old master password",
            theme,
        ));
        pane.row(muted_line(stranded_words(self.vault.stranded), theme));
        if self.vault.form == Some(Form::Rescue) {
            let old_master = self.vault.fields.old_master.clone();
            self.fill_field(pane, &old_master, cx);
            let open =
                Self::vault_button("vault-rescue", "Open them", pane, theme, cx, Self::rescue);
            let cancel = Self::cancel_button(pane, theme, cx);
            pane.row(div().pb_2().flex().gap_2().child(open).child(cancel));
        } else {
            let open = Self::vault_button(
                "vault-open-rescue",
                "Enter the old master password",
                pane,
                theme,
                cx,
                |sheet, window, cx| sheet.open_vault_form(Form::Rescue, window, cx),
            );
            pane.row(div().pb_2().flex().child(open));
        }
    }

    /// One conflict at a time, in the prompt the player must answer.
    fn fill_conflict(&self, pane: &mut Pane, theme: &Theme, cx: &mut Context<SettingsSheet>) {
        let Some(conflicts) = &self.vault.conflicts else {
            return;
        };
        let Some((entry, stranded)) = conflicts.waiting.front() else {
            return;
        };
        pane.row(section_heading("Two passwords", theme));
        pane.row(
            muted_line(conflict_question(entry), theme)
                .role(Role::Alert)
                .text_color(theme.text),
        );
        let current = self.vault.revealed.as_ref().filter(|_| conflicts.comparing);
        if let Some(current) = current {
            pane.row(muted_line(
                format!("Saved now: {}", current.password.expose()),
                theme,
            ));
            pane.row(muted_line(
                format!("From before: {}", stranded.expose()),
                theme,
            ));
        }
        let compare = Self::vault_button(
            "conflict-compare",
            "Compare them",
            pane,
            theme,
            cx,
            Self::compare,
        );
        let current = Self::vault_button(
            "conflict-keep-current",
            "Keep the one saved now",
            pane,
            theme,
            cx,
            |sheet, window, cx| sheet.settle(KeepPassword::Current, window, cx),
        );
        let stranded = Self::vault_button(
            "conflict-keep-stranded",
            "Keep the one from before",
            pane,
            theme,
            cx,
            |sheet, window, cx| sheet.settle(KeepPassword::Stranded, window, cx),
        );
        pane.row(
            div()
                .pb_2()
                .flex()
                .gap_2()
                .child(compare)
                .child(current)
                .child(stranded),
        );
    }

    /// Opens Add with `password` typed, for the accessibility check to look
    /// for it.
    #[cfg(target_os = "macos")]
    pub fn type_sample_password(
        &mut self,
        password: &str,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.open_vault_form(Form::Add, window, cx);
        self.vault.fields.password.update(cx, |field, cx| {
            field.show(password.to_owned(), password.len(), window, cx);
        });
    }

    /// Fills the section with made-up entries, on no vault: for measuring and
    /// looking at it.
    pub fn show_sample_vault(
        &mut self,
        entries: Vec<EntryKey>,
        stranded: usize,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.vault.state = VaultState::Unlocked;
        self.vault.entries = entries;
        self.vault.stranded = stranded;
        cx.notify();
    }

    /// Asks whether to remove the first password, as its Remove does, for
    /// `--ax-check`.
    #[cfg(target_os = "macos")]
    pub fn ask_to_remove_sample_password(&mut self, cx: &mut Context<SettingsSheet>) {
        self.vault.removing = self.vault.entries.first().cloned();
        cx.notify();
    }
}

fn stranded_words(count: usize) -> String {
    let (what, them) = match count {
        1 => ("1 password was".to_owned(), "it"),
        _ => (format!("{count} passwords were"), "them"),
    };
    format!(
        "{what} saved under an older master password, so this device can't read {them}. The \
         device that saved {them} brings {them} back by itself, or enter the old master password \
         here."
    )
}

fn brought_back_words(count: usize) -> String {
    match count {
        1 => "1 password brought back.".to_owned(),
        _ => format!("{count} passwords brought back."),
    }
}

fn conflict_question(entry: &EntryKey) -> String {
    format!(
        "{} on {} has two passwords: the one saved now, and one saved under the old master \
         password. Nothing says which is newer. Which stays?",
        entry.character, entry.server
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::Shell;
    use crate::shell::tests::window_with_keys;
    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use mudling_core::persist::Timestamp;
    use mudling_core::store::SyncState;
    use mudling_core::vault::{FILE_NAME, MemoryKeyStore};
    use std::sync::Arc;
    use std::thread;
    use tempfile::TempDir;

    const FIXTURE: &str = include_str!("../../../mudling-core/src/vault/testdata/format-1.vault");
    const FIXTURE_MASTER: &str = "violet anchor mosaic drifting under quiet lanterns";
    const SAVED: &str = "swordfish-42";

    struct Open<'a> {
        shell: Entity<Shell>,
        cx: &'a mut VisualTestContext,
        vault: Vault,
        _folder: TempDir,
    }

    impl Open<'_> {
        fn sheet(&self) -> Entity<SettingsSheet> {
            self.shell
                .read_with(self.cx, |shell, _| shell.settings_sheet())
        }

        fn pane<T>(&self, read: impl FnOnce(&VaultPane) -> T) -> T {
            self.sheet()
                .read_with(self.cx, |sheet, _| read(&sheet.vault))
        }

        /// Drives the window until `done`, passing on the vault's state.
        fn until(&mut self, what: &str, done: impl Fn(&VaultPane) -> bool) {
            for _ in 0..2000 {
                let (sheet, state) = (self.sheet(), self.vault.state());
                self.cx.update(|window, cx| {
                    sheet.update(cx, |sheet, cx| {
                        if sheet.vault.state != state {
                            sheet.vault_state_changed(state, window, cx);
                        }
                    });
                });
                self.cx.run_until_parked();
                if self.pane(&done) {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("never saw {what}");
        }

        fn click(&mut self, id: &str) {
            let bounds = self.cx.debug_bounds(id.to_owned().leak());
            let bounds = bounds.unwrap_or_else(|| panic!("{id} is drawn"));
            self.cx.simulate_click(bounds.center(), Modifiers::none());
            self.cx.run_until_parked();
        }

        fn type_master(&mut self, master: &str) {
            let field = self.pane(|pane| pane.fields.master.clone());
            let cx = &mut *self.cx;
            cx.update(|window, cx| {
                let text = master.to_owned();
                let end = text.len();
                field.update(cx, |field, cx| field.show(text, end, window, cx));
            });
        }

        fn unlock(&mut self) {
            self.type_master(FIXTURE_MASTER);
            self.click("vault-unlock");
            self.until("the list", |pane| !pane.entries.is_empty());
        }
    }

    /// The sheet open on Vault, over a real vault holding one saved password.
    fn opened(cx: &mut TestAppContext) -> Open<'_> {
        let (shell, cx) = window_with_keys(cx);
        cx.simulate_resize(gpui::size(px(1300.), px(1000.)));
        let folder = TempDir::new().unwrap();
        let path = folder.path().join(FILE_NAME);
        std::fs::write(&path, FIXTURE).unwrap();
        let vault = Vault::open(
            path,
            Arc::new(MemoryKeyStore::default()),
            LockAfter::Never.auto_lock(),
        );
        let handle = vault.clone();
        shell.update_in(cx, |shell, window, cx| {
            shell.open_settings(window, cx);
            shell.settings_sheet().update(cx, |sheet, cx| {
                sheet.use_vault(handle, LockAfter::Never, true, window, cx);
                sheet.select(Section::Vault, cx);
            });
        });
        let mut open = Open {
            shell,
            cx,
            vault,
            _folder: folder,
        };
        open.until("the vault's state", |pane| pane.state == VaultState::Locked);
        open
    }

    impl Open<'_> {
        /// Puts focus in `field`, as Tab would, and types `text` there.
        fn focus_and_type(&mut self, field: &Entity<CommandInput>, text: &str) {
            let (field, text) = (field.clone(), text.to_owned());
            self.cx.update(|window, cx| {
                window.focus(&gpui::Focusable::focus_handle(field.read(cx), cx), cx);
                let end = text.len();
                field.update(cx, |field, cx| field.show(text, end, window, cx));
            });
            self.cx.run_until_parked();
        }

        /// The sheet still has the keyboard: focus sits on the Vault tab, and
        /// Esc, bound only inside the sheet, reaches it.
        fn assert_sheet_keeps_keys(&mut self) {
            let sheet = self.sheet();
            let on_tab = self.cx.update(|window, cx| {
                sheet.read(cx).tabs[Section::Vault as usize].is_focused(window)
            });
            assert!(on_tab, "focus stays in the sheet");
            self.cx.simulate_keystrokes("escape");
            self.cx.run_until_parked();
            assert!(self.cx.debug_bounds("settings-sheet").is_none());
        }
    }

    #[gpui::test]
    fn return_in_the_master_field_unlocks_and_the_sheet_keeps_the_keyboard(
        cx: &mut TestAppContext,
    ) {
        let mut open = opened(cx);
        let master = open.pane(|pane| pane.fields.master.clone());
        open.focus_and_type(&master, FIXTURE_MASTER);
        open.cx.simulate_keystrokes("enter");
        open.until("the list", |pane| !pane.entries.is_empty());
        open.cx.run_until_parked();
        open.assert_sheet_keeps_keys();
    }

    #[gpui::test]
    fn return_in_a_saved_form_leaves_the_sheet_the_keyboard(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        open.click("vault-change-0");
        let password = open.pane(|pane| pane.fields.password.clone());
        open.focus_and_type(&password, "a new one");
        open.cx.simulate_keystrokes("enter");
        open.until("the save", |pane| pane.form.is_none() && !pane.busy);
        open.cx.run_until_parked();
        open.assert_sheet_keeps_keys();
    }

    #[gpui::test]
    fn a_wrong_master_password_is_refused_in_plain_words_and_the_field_is_wiped(
        cx: &mut TestAppContext,
    ) {
        let mut open = opened(cx);
        open.type_master("not it");
        open.click("vault-unlock");
        open.until("the refusal", |pane| pane.problem.is_some());
        let problem = open.pane(|pane| pane.problem.clone().unwrap());
        assert_eq!(problem, "That isn't the master password.");
        let sheet = open.sheet();
        let left = sheet.read_with(open.cx, |sheet, cx| {
            sheet.vault.fields.master.read(cx).content().to_owned()
        });
        assert_eq!(left, "");
    }

    #[gpui::test]
    fn reveal_is_deliberate_and_hides_again_by_itself(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        let shown = |pane: &VaultPane| pane.revealed.is_some();
        assert!(!open.pane(shown), "nothing shows until it's asked for");

        open.click("vault-reveal-0");
        open.until("the password", shown);
        let password = open.pane(|pane| {
            let revealed = pane.revealed.as_ref().unwrap();
            revealed.password.expose().to_owned()
        });
        assert_eq!(password, SAVED);

        open.cx.executor().advance_clock(REVEAL_FOR);
        open.until("it hiding", |pane| pane.revealed.is_none());
    }

    #[gpui::test]
    fn hide_and_moving_on_take_the_password_off_the_screen(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        open.click("vault-reveal-0");
        open.until("the password", |pane| pane.revealed.is_some());
        open.click("vault-reveal-0");
        assert!(open.pane(|pane| pane.revealed.is_none()), "Hide");

        open.click("vault-reveal-0");
        open.until("the password", |pane| pane.revealed.is_some());
        let sheet = open.sheet();
        sheet.update(open.cx, |sheet, cx| sheet.select(Section::Data, cx));
        assert!(open.pane(|pane| pane.revealed.is_none()), "another section");
    }

    #[gpui::test]
    fn locking_forgets_what_is_on_the_screen_and_what_is_typed(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        open.click("vault-reveal-0");
        open.until("the password", |pane| pane.revealed.is_some());
        open.click("vault-change-0");
        let password = open.pane(|pane| pane.fields.password.clone());
        open.cx.update(|window, cx| {
            password.update(cx, |field, cx| {
                field.show("half typed".to_owned(), 10, window, cx);
            });
        });

        open.click("vault-lock");
        open.until("the lock", |pane| pane.state == VaultState::Locked);

        assert!(open.pane(|pane| pane.revealed.is_none() && pane.entries.is_empty()));
        assert_eq!(
            password.read_with(open.cx, |field, _| field.content().len()),
            0
        );
    }

    #[gpui::test]
    fn turning_the_device_copy_off_removes_it_and_unlock_on_this_device(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        assert!(open.cx.debug_bounds("vault-unlock-here").is_some());
        open.unlock();

        open.click("vault-keep-on-device");
        open.until("the copy removed", |pane| {
            !pane.keep_on_device && !pane.busy
        });
        open.click("vault-lock");
        open.until("the lock", |pane| pane.state == VaultState::Locked);

        assert!(open.cx.debug_bounds("vault-unlock-here").is_none());
        assert!(matches!(
            futures::executor::block_on(open.vault.unlock_from_device()),
            Err(Error::NotCached)
        ));
    }

    #[gpui::test]
    fn a_password_reveal_cut_off_by_a_lock_never_shows_after_the_next_unlock(
        cx: &mut TestAppContext,
    ) {
        let mut open = opened(cx);
        open.unlock();
        let sheet = open.sheet();
        sheet.update(open.cx, |sheet, _| {
            sheet.vault.revealing = Some(Task::ready(()));
        });
        futures::executor::block_on(open.vault.lock()).unwrap();
        open.until("the lock", |pane| pane.state == VaultState::Locked);
        assert!(
            open.pane(|pane| pane.revealing.is_none()),
            "the waiting call is dropped"
        );
    }

    #[gpui::test]
    fn change_saves_the_typed_password_and_the_field_is_wiped(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        open.click("vault-change-0");
        let password = open.pane(|pane| pane.fields.password.clone());
        open.cx.update(|window, cx| {
            password.update(cx, |field, cx| {
                field.show("a new one".to_owned(), 9, window, cx);
            });
        });
        open.click("vault-form-save");
        open.until("the save", |pane| pane.form.is_none() && !pane.busy);

        let entry = EntryKey::new("mud.example.org", "Fikus");
        let saved = futures::executor::block_on(open.vault.password(&entry));
        assert_eq!(saved.unwrap().unwrap().expose(), "a new one");
        assert_eq!(
            password.read_with(open.cx, |field, _| field.content().len()),
            0
        );
    }

    #[gpui::test]
    fn leaving_the_section_or_the_sheet_wipes_a_typed_password(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.type_master("half typed");
        let sheet = open.sheet();
        sheet.update(open.cx, |sheet, cx| sheet.select(Section::Data, cx));
        let master = open.pane(|pane| pane.fields.master.clone());
        assert_eq!(
            master.read_with(open.cx, |field, _| field.content().len()),
            0
        );

        sheet.update(open.cx, |sheet, cx| sheet.select(Section::Vault, cx));
        open.type_master("half typed");
        open.cx.simulate_keystrokes("escape");
        open.cx.run_until_parked();
        assert_eq!(
            master.read_with(open.cx, |field, _| field.content().len()),
            0
        );
    }

    /// Opens the form, fills both fields and presses Return in them. The
    /// form's button can sit below the fold of the test window.
    fn submit_new_master(open: &mut Open<'_>, first: &str, second: &str) {
        let (sheet, fields) = (
            open.sheet(),
            open.pane(|pane| (pane.fields.master.clone(), pane.fields.repeat.clone())),
        );
        open.cx.update(|window, cx| {
            sheet.update(cx, |sheet, cx| {
                sheet.open_vault_form(Form::NewMaster, window, cx);
            });
            for (field, text) in [(&fields.0, first), (&fields.1, second)] {
                let (text, end) = (text.to_owned(), text.len());
                field.update(cx, |field, cx| field.show(text, end, window, cx));
            }
            sheet.update(cx, |sheet, cx| sheet.submit_vault(window, cx));
        });
        open.cx.run_until_parked();
    }

    #[gpui::test]
    fn a_new_master_password_must_be_typed_the_same_twice(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        submit_new_master(&mut open, "one thing", "another");
        let problem = open.pane(|pane| pane.problem.clone());
        assert_eq!(problem.as_deref(), Some("Those two don't match."));
        assert!(open.pane(|pane| !pane.busy), "nothing went to the vault");
    }

    #[gpui::test]
    fn a_new_master_password_shows_the_recovery_key_until_the_player_has_it(
        cx: &mut TestAppContext,
    ) {
        let mut open = opened(cx);
        open.unlock();
        let long = "a new and unusual passphrase nobody would guess";
        submit_new_master(&mut open, long, long);
        open.until("the recovery key", |pane| pane.recovery.is_some());
        open.cx.run_until_parked();
        assert!(open.cx.debug_bounds("recovery-key").is_some());

        futures::executor::block_on(open.vault.lock()).unwrap();
        open.until("the lock", |pane| pane.state == VaultState::Locked);
        assert!(
            open.pane(|pane| pane.recovery.is_some()),
            "a lock can't lose the only copy"
        );

        open.click("recovery-key-saved");
        assert!(open.pane(|pane| pane.recovery.is_none()));
        assert!(open.cx.debug_bounds("recovery-key").is_none());
    }

    #[gpui::test]
    fn a_vault_file_that_lands_later_is_noticed_at_the_next_sync(cx: &mut TestAppContext) {
        let (shell, cx) = window_with_keys(cx);
        let folder = TempDir::new().unwrap();
        let path = folder.path().join(FILE_NAME);
        let vault = Vault::open(
            path.clone(),
            Arc::new(MemoryKeyStore::default()),
            LockAfter::Never.auto_lock(),
        );
        let handle = vault.clone();
        let mut open = Open {
            shell,
            cx,
            vault,
            _folder: folder,
        };
        let sheet = open.sheet();
        open.cx.update(|window, cx| {
            sheet.update(cx, |sheet, cx| {
                sheet.use_vault(handle, LockAfter::Never, true, window, cx);
            });
        });
        open.until("no vault", |pane| pane.state == VaultState::NotCreated);

        std::fs::write(&path, FIXTURE).unwrap();
        sheet.update(open.cx, |sheet, cx| {
            sheet.show_sync(&SyncState::default(), None, cx);
        });
        open.until("the vault", |pane| pane.state == VaultState::Locked);
    }

    #[gpui::test]
    fn two_passwords_ask_the_player_one_at_a_time(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        let sheet = open.sheet();
        sheet.update(open.cx, |sheet, cx| {
            sheet.vault.conflicts = Some(Conflicts {
                old_master: "old".into(),
                waiting: [
                    (EntryKey::new("mud.example.org", "Fikus"), "older".into()),
                    (EntryKey::new("mud.example.org", "Mira"), "other".into()),
                ]
                .into_iter()
                .collect(),
                comparing: false,
            });
            cx.notify();
        });
        open.cx.run_until_parked();
        assert!(open.cx.debug_bounds("conflict-keep-current").is_some());

        open.click("conflict-compare");
        open.until("both on the screen", |pane| pane.revealed.is_some());
        let comparing = |pane: &VaultPane| pane.conflicts.as_ref().unwrap().comparing;
        assert!(open.pane(comparing));
        open.cx.executor().advance_clock(REVEAL_FOR);
        open.until("them hiding", |pane| !comparing(pane));
    }

    #[gpui::test]
    fn a_sync_landing_refreshes_the_list_of_passwords(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        let before = open.pane(|pane| pane.entries.len());
        // What a merge does to the file: another device's password arrives.
        let arrived = EntryKey::new("mud.example.org", "Nilo");
        futures::executor::block_on(open.vault.set_password(arrived.clone(), "other".into()))
            .unwrap();
        open.cx.run_until_parked();
        assert_eq!(open.pane(|pane| pane.entries.len()), before, "not told yet");

        let landed = SyncState {
            last_sync: Some(Timestamp::now()),
            ..SyncState::default()
        };
        open.sheet().update(open.cx, |sheet, cx| {
            sheet.show_sync(&landed, None, cx);
        });
        open.until("the new password listed", |pane| {
            pane.entries.contains(&arrived)
        });
    }

    #[gpui::test]
    fn stranded_passwords_show_as_a_count_with_a_way_in(cx: &mut TestAppContext) {
        let mut open = opened(cx);
        open.unlock();
        let sheet = open.sheet();
        sheet.update(open.cx, |sheet, cx| {
            sheet.vault.stranded = 2;
            cx.notify();
        });
        open.cx.run_until_parked();
        open.click("vault-open-rescue");
        assert!(open.cx.debug_bounds("vault-rescue").is_some());
    }

    #[test]
    fn a_wrong_password_is_said_plainly() {
        assert_eq!(
            words(&Error::WrongPassword),
            "That isn't the master password."
        );
        assert!(words(&Error::ChangedDuringSave).contains("try again"));
    }

    #[test]
    fn stranded_counts_read_as_sentences() {
        assert!(stranded_words(1).starts_with("1 password was"));
        assert!(stranded_words(3).starts_with("3 passwords were"));
    }
}
