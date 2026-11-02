//! The shell's side of the plugin directory and approvals: everything that
//! touches the network or the disk runs off the UI thread, and the sheet is
//! told what came of it.

use super::Shell;
use crate::launcher::Launcher;
use crate::settings::{Listing, PluginRow};
use gpui::{AppContext, Context, SharedString, Window};
use mudling_core::git::Limit;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::PluginId;
use mudling_core::registry::{Entry, Fetched, Index};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// How long a fetch from the registry may take before the page says so.
const REGISTRY_WAIT: Duration = Duration::from_secs(60);
const ARRIVED_NOTE: &str = "It arrived from another device in a copy this device didn't install.";
const NOT_SIGNED: &str = "The directory doesn't list this copy.";

/// What the installed plugins are now, read off the UI thread.
struct PluginsNow {
    launcher: Launcher,
    installed: BTreeMap<String, Option<String>>,
    /// The rows of the server being played, if there's one.
    rows: Option<Vec<PluginRow>>,
}

impl PluginsNow {
    fn read(launcher: Launcher, host: Option<&str>) -> Result<PluginsNow, String> {
        let installed = launcher.installed_versions();
        let rows = host.map(|host| launcher.plugin_rows(host)).transpose()?;
        Ok(PluginsNow {
            launcher,
            installed,
            rows,
        })
    }
}

impl Shell {
    /// Fetches and checks the directory's index.
    pub(super) fn refresh_directory(&mut self, cx: &mut Context<Shell>) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let read = cx.background_spawn(async move {
            let listing = match launcher.registry().refresh(&Limit::after(REGISTRY_WAIT)) {
                Ok(index) => Listing::Listed {
                    index: Arc::new(index),
                    at: Timestamp::now(),
                },
                Err(refused) => Listing::Refused(refused.to_string().into()),
            };
            (listing, launcher.installed_versions())
        });
        self.directory_read = Some(cx.spawn(async move |shell, cx| {
            let (listing, installed) = read.await;
            let _ = shell.update(cx, |shell, cx| {
                shell.settings.update(cx, |sheet, cx| {
                    sheet.show_directory(listing, installed, cx);
                });
            });
        }));
    }

    /// Downloads `entry` and checks it, then hands it to the sheet, which
    /// asks for approval if it needs one.
    pub(super) fn fetch_plugin(
        &mut self,
        index: Arc<Index>,
        entry: Entry,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let fetch = cx.background_spawn(async move {
            let fetched = (launcher.registry())
                .fetch(&index, &entry, &Limit::after(REGISTRY_WAIT))
                .map_err(|error| format!("{} wasn't installed: {error}.", entry.name))?;
            let approved = launcher.approved().contains(fetched.plugin.id());
            Ok((Arc::new(fetched), approved))
        });
        self.plugin_work = Some(cx.spawn_in(window, async move |shell, cx| {
            let fetched = fetch.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                shell
                    .settings
                    .update(cx, |sheet, cx| sheet.fetched(fetched, window, cx));
            });
        }));
    }

    pub(super) fn install_plugin(
        &mut self,
        fetched: Arc<Fetched>,
        approve: bool,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let install = cx.background_spawn(async move {
            let launcher = launcher.install(&fetched, approve)?;
            PluginsNow::read(launcher, host.as_deref())
        });
        self.plugin_work = Some(cx.spawn(async move |shell, cx| {
            let now = install.await;
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, true, cx));
        }));
    }

    /// Reads the installed plugins again, as a sync may have brought some.
    pub(super) fn reload_plugins(&mut self, cx: &mut Context<Shell>) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let reload = cx.background_spawn(async move {
            PluginsNow::read(launcher.with_installed(), host.as_deref())
        });
        self.plugin_work = Some(cx.spawn(async move |shell, cx| {
            let now = reload.await;
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, false, cx));
        }));
    }

    /// Takes `plugin` out of the data folder, from `folder`, the one its row
    /// came from. A session running it stops letting it send at once; the
    /// rest waits for the next connect.
    pub(super) fn remove_plugin(
        &mut self,
        plugin: PluginId,
        folder: SharedString,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        if let Some(game) = &self.game {
            game.session.set_approved(plugin.clone(), false);
        }
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let remove = cx.background_spawn(async move {
            let launcher = launcher.uninstall(&plugin, &folder)?;
            PluginsNow::read(launcher, host.as_deref())
        });
        self.plugin_work = Some(cx.spawn(async move |shell, cx| {
            let now = remove.await;
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, true, cx));
        }));
    }

    /// Checks the copy in `folder`, shown with `hash`, against the
    /// directory's verified index, which trusts it if the registry signed for
    /// it. If not, and it's still that copy, asks the player, saying why.
    pub(super) fn review_arrival(
        &mut self,
        folder: SharedString,
        hash: SharedString,
        window: &mut Window,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let (checking, shown) = (folder.clone(), hash.clone());
        let check = cx.background_spawn(async move {
            let checked = launcher.registry().refresh(&Limit::after(REGISTRY_WAIT));
            let launcher = launcher.with_installed();
            let still_held = launcher.is_held_back(&checking, &shown);
            let why = still_held.then(|| match checked {
                Ok(_) => format!("{ARRIVED_NOTE} {NOT_SIGNED}"),
                Err(refused) => {
                    format!("{ARRIVED_NOTE} The directory couldn't vouch for it: {refused}")
                }
            });
            PluginsNow::read(launcher, host.as_deref()).map(|now| (now, why))
        });
        self.plugin_work = Some(cx.spawn_in(window, async move |shell, cx| {
            let checked = check.await;
            let _ = shell.update_in(cx, |shell, window, cx| match checked {
                Ok((now, why)) => {
                    shell.show_plugins_now(Ok(now), why.is_none(), cx);
                    if let Some(why) = why {
                        shell.settings.update(cx, |sheet, cx| {
                            sheet.ask_to_trust(&folder, &hash, why, window, cx);
                        });
                    }
                }
                Err(problem) => shell.show_plugins_now(Err(problem), false, cx),
            });
        }));
    }

    /// Loads the copy in `folder` the player chose, as long as it's still
    /// the one shown with `hash`.
    pub(super) fn trust_plugin(
        &mut self,
        folder: SharedString,
        hash: SharedString,
        cx: &mut Context<Shell>,
    ) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let trust = cx.background_spawn(async move {
            PluginsNow::read(launcher.trust(&folder, &hash)?, host.as_deref())
        });
        self.plugin_work = Some(cx.spawn(async move |shell, cx| {
            let now = trust.await;
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, true, cx));
        }));
    }

    /// Shows what's installed now. `by_player` says the player changed it,
    /// so the session running needs a new connect to follow.
    fn show_plugins_now(
        &mut self,
        now: Result<PluginsNow, String>,
        by_player: bool,
        cx: &mut Context<Shell>,
    ) {
        match now {
            Ok(now) => {
                let refused = now.launcher.refused().to_vec();
                self.launcher = Some(now.launcher);
                self.settings.update(cx, |sheet, cx| {
                    sheet.installed(None, now.installed, cx);
                    sheet.show_refused(refused, cx);
                    if let Some(rows) = now.rows {
                        sheet.refresh_plugins(rows, by_player, cx);
                    }
                });
            }
            Err(problem) => self.settings.update(cx, |sheet, cx| {
                sheet.install_failed(problem, cx);
            }),
        }
    }

    /// Saves whether `plugin` may send commands, and applies it to the
    /// session at once. Taking it back applies before it's saved; letting it
    /// send waits until it is.
    pub(super) fn set_approved(
        &mut self,
        plugin: PluginId,
        approved: bool,
        cx: &mut Context<Shell>,
    ) {
        if !approved && let Some(game) = &self.game {
            game.session.set_approved(plugin.clone(), false);
        }
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let saving = plugin.clone();
        let save = cx.background_spawn(async move { launcher.set_approved(&saving, approved) });
        cx.spawn(async move |shell, cx| {
            let saved = save.await;
            let _ = shell.update(cx, |shell, cx| match saved {
                Ok(()) if approved => {
                    if let Some(game) = &shell.game {
                        game.session.set_approved(plugin, true);
                    }
                }
                Ok(()) => {}
                Err(problem) => shell
                    .settings
                    .update(cx, |sheet, cx| sheet.show_switch_problem(problem, cx)),
            });
        })
        .detach();
    }
}
