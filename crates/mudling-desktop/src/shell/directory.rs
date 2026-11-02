//! The shell's side of the plugin directory and approvals: everything that
//! touches the network or the disk runs off the UI thread, and the sheet is
//! told what came of it.

use super::Shell;
use crate::launcher::Launcher;
use crate::settings::{Listing, PluginRow, installed_versions};
use gpui::{AppContext, Context, Window};
use mudling_core::git::Limit;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::PluginId;
use mudling_core::registry::{Entry, Fetched, Index};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// How long a fetch from the registry may take before the page says so.
const REGISTRY_WAIT: Duration = Duration::from_secs(60);

/// What the installed plugins are now, read off the UI thread.
struct PluginsNow {
    launcher: Launcher,
    installed: BTreeMap<String, Option<String>>,
    /// The rows of the server being played, if there's one.
    rows: Option<Vec<PluginRow>>,
}

impl PluginsNow {
    fn read(launcher: Launcher, host: Option<&str>) -> Result<PluginsNow, String> {
        let installed = installed_versions(launcher.plugins(), &launcher.records());
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
            (
                listing,
                installed_versions(launcher.plugins(), &launcher.records()),
            )
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
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, cx));
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
            let _ = shell.update(cx, |shell, cx| shell.show_plugins_now(now, cx));
        }));
    }

    fn show_plugins_now(&mut self, now: Result<PluginsNow, String>, cx: &mut Context<Shell>) {
        match now {
            Ok(now) => {
                self.launcher = Some(now.launcher);
                self.settings.update(cx, |sheet, cx| {
                    sheet.installed(None, now.installed, cx);
                    if let Some(rows) = now.rows {
                        sheet.refresh_plugins(rows, cx);
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
