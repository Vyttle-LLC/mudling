//! Updates in the window: a check at launch and then daily, off the UI
//! thread, and what the player asks of an update that's ready. Only a window
//! that keeps the player's settings checks; measured runs and checks never do.

use super::Shell;
use crate::launcher::Target;
use crate::settings::UpdateAsk;
use crate::update::{self, Eligibility, Failure, GitHub, Staged, System, Updates};
use gpui::{AppContext, Context, SharedString, Task, Window};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{CHARACTER_NAME, ConnectionStatus};
use std::path::PathBuf;
use std::time::Duration;

/// How often the window asks whether the daily check is due. A day is too
/// long for one timer: the computer may sleep through it.
const DUE_EVERY: Duration = Duration::from_secs(60 * 60);

/// What `main` reads from the disk before the app runs.
pub struct UpdateStart {
    pub eligibility: Eligibility,
    /// The OS's cache folder, where downloads wait.
    pub cache: PathBuf,
    /// What the last exit's swap ran into, if it failed.
    pub failed: Option<Failure>,
    /// What a restart passes the new copy before `--reopen`, like `--data`.
    pub arguments: Vec<String>,
}

pub(super) struct UpdateLink {
    updates: Updates,
    /// `None` for a sample, which never checks or goes in.
    cache: Option<PathBuf>,
    arguments: Vec<String>,
    /// Restart to update was chosen, to reopen this server, if any.
    restart: Option<Option<Target>>,
    _daily: Task<()>,
    _checking: Option<Task<()>>,
}

/// An update to put in as the app exits, and whether to open MUDling again.
pub struct ExitUpdate {
    staged: Staged,
    relaunch: Option<Vec<String>>,
    cache: Option<PathBuf>,
}

impl ExitUpdate {
    #[cfg(test)]
    pub fn relaunch(&self) -> Option<&[String]> {
        self.relaunch.as_deref()
    }

    /// Call it once everything is saved and sync's last push is done.
    pub fn install(self) {
        let Some(cache) = self.cache else {
            return;
        };
        if let Err(problem) = self
            .staged
            .install_on_exit(self.relaunch.as_deref(), &cache)
        {
            eprintln!("the update didn't go in: {problem}");
        }
    }
}

impl Shell {
    /// Checks for updates from now on, and at once, unless the player
    /// turned checking off.
    pub fn use_updates(&mut self, start: UpdateStart, cx: &mut Context<Shell>) {
        let choices = (self.saved_settings.as_ref())
            .map(|saved| saved.update_choices())
            .unwrap_or_default();
        let swap_failed = start.failed.is_some();
        let checking_on = choices.check;
        let updates = Updates::new(start.eligibility, choices, start.failed);
        let daily = cx.spawn(async move |shell, cx| {
            loop {
                cx.background_executor().timer(DUE_EVERY).await;
                let checked = shell.update(cx, |shell, cx| {
                    if shell.updates_due() {
                        shell.check_for_updates(false, cx);
                    }
                });
                if checked.is_err() {
                    return;
                }
            }
        });
        self.updates = Some(UpdateLink {
            updates,
            cache: Some(start.cache),
            arguments: start.arguments,
            restart: None,
            _daily: daily,
            _checking: None,
        });
        if swap_failed {
            self.save_update_choices();
        }
        // Every launch checks; the daily timer waits for a day since the last.
        if checking_on {
            self.check_for_updates(false, cx);
        }
        self.show_update(cx);
    }

    /// Shows `staged` as ready, with no checking: for measured runs, checks
    /// and images.
    pub fn show_sample_update(&mut self, staged: Staged, cx: &mut Context<Shell>) {
        let installed = staged.installed().clone();
        let mut updates = Updates::new(
            Eligibility::Updates(installed),
            update::UpdateChoices::default(),
            None,
        );
        updates.begin_check(true);
        let checked_at = Timestamp(Timestamp::now().0 - 3 * 60 * 60 * 1000);
        updates.finish_check(Ok(update::Checked::Ready(staged)), checked_at);
        self.updates = Some(UpdateLink {
            updates,
            cache: None,
            arguments: Vec::new(),
            restart: None,
            _daily: Task::ready(()),
            _checking: None,
        });
        self.show_update(cx);
    }

    /// Opens the server a restart to update left, logging in as at any
    /// launch: a login waits for the vault.
    pub fn reopen(&mut self, target: &Target, window: &mut Window, cx: &mut Context<Shell>) {
        self.ask_to_open(target, window, cx);
    }

    fn updates_due(&self) -> bool {
        (self.updates.as_ref()).is_some_and(|link| link.updates.due(Timestamp::now()))
    }

    /// Downloads and stages a newer release off the UI thread. A `manual`
    /// check tries again a release that failed before.
    fn check_for_updates(&mut self, manual: bool, cx: &mut Context<Shell>) {
        let Some(link) = &mut self.updates else {
            return;
        };
        let Some(staging) = link.cache.as_deref().map(update::staging_folder) else {
            return;
        };
        let Some((installed, skip)) = link.updates.begin_check(manual) else {
            return;
        };
        let checked = cx.background_spawn(async move {
            update::check(
                &installed,
                skip.as_deref(),
                &staging,
                &GitHub(&System),
                &System,
            )
        });
        link._checking = Some(cx.spawn(async move |shell, cx| {
            let checked = checked.await;
            let _ = shell.update(cx, |shell, cx| {
                if let Some(link) = &mut shell.updates {
                    link.updates.finish_check(checked, Timestamp::now());
                }
                shell.save_update_choices();
                shell.show_update(cx);
            });
        }));
        self.show_update(cx);
    }

    pub(super) fn on_update_ask(
        &mut self,
        ask: UpdateAsk,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(link) = &mut self.updates else {
            return;
        };
        match ask {
            UpdateAsk::CheckNow => return self.check_for_updates(true, cx),
            UpdateAsk::CheckingOn(on) => link.updates.set_checking_on(on),
            UpdateAsk::Later => link.updates.later(),
            UpdateAsk::DismissFailure => link.updates.dismiss_failure(),
            UpdateAsk::Restart => return self.ask_to_restart(window, cx),
            UpdateAsk::RestartNow => return self.restart_to_update(window, cx),
        }
        self.save_update_choices();
        self.show_update(cx);
    }

    /// Restarting drops the connection, so it asks first. Not connected, it
    /// just restarts.
    fn ask_to_restart(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let Some(question) = self.restart_question() else {
            return self.restart_to_update(window, cx);
        };
        self.open_settings(window, cx);
        self.settings
            .update(cx, |sheet, cx| sheet.ask_restart(question, window, cx));
    }

    /// What restarting would drop, while connected to a server.
    fn restart_question(&self) -> Option<String> {
        let game = self.game.as_ref().filter(|game| !game.is_replay())?;
        let settings = self.copies.settings.value();
        if !matches!(
            settings.connection,
            ConnectionStatus::Connected | ConnectionStatus::Connecting
        ) {
            return None;
        }
        let host = &game.target.host;
        let who = match self.copies.characters.value().0.get(CHARACTER_NAME) {
            Some(Value::Text(name)) => format!("{host} as {name}"),
            _ => host.clone(),
        };
        Some(format!(
            "You're connected to {who}. Restarting disconnects you. MUDling saves everything \
             first, and opens the same server again afterwards."
        ))
    }

    /// Quits as an ordinary quit does, then opens the new version on the same
    /// server.
    fn restart_to_update(&mut self, window: &mut Window, cx: &mut Context<Shell>) {
        let reopen = (self.game.as_ref())
            .filter(|game| !game.is_replay())
            .map(|game| game.target.clone());
        let Some(link) = &mut self.updates else {
            return;
        };
        if link.updates.staged().is_none() {
            return;
        }
        link.restart = Some(reopen);
        window.dispatch_action(Box::new(crate::Quit), cx);
    }

    /// The update that's ready, for the app to put in once it has saved
    /// everything. Taking it means it goes in only once.
    pub fn take_update_for_exit(&mut self) -> Option<ExitUpdate> {
        let link = self.updates.as_mut()?;
        let staged = link.updates.take_staged()?;
        let relaunch = link.restart.take().map(|reopen| {
            let mut arguments = link.arguments.clone();
            if let Some(target) = reopen {
                arguments.extend(["--reopen".to_owned(), target.to_string()]);
                if let Some(character) = target.character {
                    arguments.extend(["--character".to_owned(), character]);
                }
            }
            arguments
        });
        Some(ExitUpdate {
            staged,
            relaunch,
            cache: link.cache.clone(),
        })
    }

    fn save_update_choices(&self) {
        if let (Some(link), Some(saved)) = (&self.updates, &self.saved_settings) {
            saved.save_updates(link.updates.choices());
        }
    }

    /// Tells the title bar, the empty state and Settings what changed.
    fn show_update(&mut self, cx: &mut Context<Shell>) {
        let Some(link) = &self.updates else {
            return;
        };
        let status = link.updates.status();
        let ready = (status.ready.as_ref()).map(|ready| SharedString::from(ready.version.clone()));
        let card = status
            .card()
            .map(|ready| SharedString::from(ready.version.clone()));
        self.title_bar
            .update(cx, |bar, cx| bar.show_update_ready(ready, cx));
        self.empty_state
            .update(cx, |empty_state, cx| empty_state.show_update(card, cx));
        self.settings
            .update(cx, |sheet, cx| sheet.show_update(status, cx));
    }
}
