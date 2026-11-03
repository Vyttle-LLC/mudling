//! The shell's side of the plugin directory and approvals: everything that
//! touches the network or the disk runs off the UI thread, and the sheet is
//! told what came of it.
//!
//! Every change to the plugins runs in turn, each on the launcher the one
//! before left: two at once would each start from a copy without the
//! other's, and the second to finish would undo the first.

use super::Shell;
use crate::launcher::{InstalledVersions, Launcher};
use crate::settings::{AddedPlugin, Listing, PluginRow};
use gpui::{AppContext, Context, SharedString, Window};
use mudling_core::git::Limit;
use mudling_core::persist::Timestamp;
use mudling_core::plugin::PluginId;
use mudling_core::registry::{Entry, Fetched, Index};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// How long a fetch from the registry may take before the page says so.
const REGISTRY_WAIT: Duration = Duration::from_secs(60);
const ARRIVED_NOTE: &str = "It arrived from another device in a copy this device didn't install.";
const NOT_SIGNED: &str = "The directory doesn't list this copy.";

/// A change to the plugins, waiting its turn.
pub(super) struct PluginChange {
    change: Box<dyn FnOnce(Launcher) -> Result<Launcher, String> + Send>,
    shown: Shown,
}

/// Who made a change, which decides how it shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shown {
    /// An install the player chose, which the directory shows as busy
    /// until it ends.
    Install,
    /// Another change the player made: the session running needs a new
    /// connect to follow, and a problem shows on the page.
    ByPlayer,
    /// A save in a folder the player added: the session needs a new connect
    /// too, but the sheet's other work is left alone.
    FolderSaved,
    /// What a sync brought, or the copies trusted now.
    Synced,
}

impl Shown {
    fn needs_connect(self) -> bool {
        self != Shown::Synced
    }
}

/// What the installed plugins are now, read off the UI thread.
struct PluginsNow {
    launcher: Launcher,
    installed: InstalledVersions,
    added: Vec<AddedPlugin>,
    /// The rows of the server being played, if there's one.
    rows: Option<Vec<PluginRow>>,
}

impl PluginsNow {
    fn read(launcher: Launcher, host: Option<&str>) -> Result<PluginsNow, String> {
        let installed = launcher.installed_versions();
        let added = launcher.added_plugins();
        let rows = host.map(|host| launcher.plugin_rows(host)).transpose()?;
        Ok(PluginsNow {
            launcher,
            installed,
            added,
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
            Ok(launcher.downloaded(fetched))
        });
        self.plugin_fetch = Some(cx.spawn_in(window, async move |shell, cx| {
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
        self.change_plugins(
            Shown::Install,
            move |launcher| launcher.install(&fetched, approve),
            cx,
        );
    }

    /// Reads the installed plugins again, as a sync may have brought some.
    pub(super) fn reload_plugins(&mut self, cx: &mut Context<Shell>) {
        self.change_plugins(Shown::Synced, |launcher| Ok(launcher.with_installed()), cx);
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
        // A copy held back beside the one that runs never ran.
        if let (Some(game), Some(launcher)) = (&self.game, &self.launcher)
            && launcher.runs_from_folder(&plugin, &folder)
        {
            game.session.set_approved(plugin.clone(), false);
        }
        self.change_plugins(
            Shown::ByPlayer,
            move |launcher| launcher.uninstall(&plugin, &folder),
            cx,
        );
    }

    /// Hands the sheet what was added from links and folders.
    pub(super) fn show_added(&mut self, cx: &mut Context<Shell>) {
        let Some(launcher) = &self.launcher else {
            return;
        };
        let added = launcher.added_plugins();
        self.settings
            .update(cx, |sheet, cx| sheet.show_added(added, cx));
    }

    /// Fetches the plugin at `url` and checks it, then hands it to the
    /// sheet, which asks for approval if it needs one, as the directory's.
    pub(super) fn add_link(&mut self, url: String, window: &mut Window, cx: &mut Context<Shell>) {
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let fetch = cx.background_spawn(async move {
            let fetched = launcher.fetch_link(&url, &Limit::after(REGISTRY_WAIT))?;
            Ok(launcher.downloaded(fetched))
        });
        self.link_check = Some(cx.spawn_in(window, async move |shell, cx| {
            let checked = fetch.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                shell
                    .settings
                    .update(cx, |sheet, cx| sheet.link_checked(checked, window, cx));
            });
        }));
    }

    /// Loads the plugin in `folder` from now on, and watches it.
    pub(super) fn add_folder(&mut self, folder: PathBuf, cx: &mut Context<Shell>) {
        let repo = self
            .sync
            .as_ref()
            .and_then(|link| link.choices.repo.clone());
        self.change_plugins(
            Shown::ByPlayer,
            move |launcher| launcher.add_folder(&folder, repo.as_deref()),
            cx,
        );
    }

    /// Reads `folders` again, as a file in each changed.
    pub(super) fn reload_folders(&mut self, folders: Vec<PathBuf>, cx: &mut Context<Shell>) {
        self.change_plugins(
            Shown::FolderSaved,
            move |launcher| {
                let reload = |launcher: Launcher, folder: &PathBuf| launcher.reload_folder(folder);
                Ok(folders.iter().fold(launcher, reload))
            },
            cx,
        );
    }

    /// Stops loading the folder at `path`, which held `plugin` if it ever
    /// loaded. A session running it stops letting it send at once.
    pub(super) fn forget_folder(
        &mut self,
        plugin: Option<PluginId>,
        path: PathBuf,
        cx: &mut Context<Shell>,
    ) {
        if let (Some(game), Some(launcher), Some(plugin)) = (&self.game, &self.launcher, plugin)
            && launcher.runs_from_added(&plugin, &path)
        {
            game.session.set_approved(plugin, false);
        }
        self.change_plugins(
            Shown::ByPlayer,
            move |launcher| launcher.forget_folder(&path),
            cx,
        );
    }

    /// Runs `change` on the launcher off the UI thread once the changes
    /// before it are done, and then shows what's installed after it.
    fn change_plugins(
        &mut self,
        shown: Shown,
        change: impl FnOnce(Launcher) -> Result<Launcher, String> + Send + 'static,
        cx: &mut Context<Shell>,
    ) {
        self.plugin_changes.push_back(PluginChange {
            change: Box::new(change),
            shown,
        });
        self.next_plugin_change(cx);
    }

    /// Starts the next change waiting, unless one is running.
    fn next_plugin_change(&mut self, cx: &mut Context<Shell>) {
        if self.plugin_work.is_some() {
            return;
        }
        let Some(launcher) = self.launcher.clone() else {
            return;
        };
        let Some(PluginChange { change, shown }) = self.plugin_changes.pop_front() else {
            return;
        };
        let host = self.game.as_ref().map(|game| game.target.host.clone());
        let changed = cx
            .background_spawn(async move { PluginsNow::read(change(launcher)?, host.as_deref()) });
        self.plugin_work = Some(cx.spawn(async move |shell, cx| {
            let now = changed.await;
            let _ = shell.update(cx, |shell, cx| {
                shell.plugin_work = None;
                shell.show_plugins_now(now, shown, cx);
                shell.next_plugin_change(cx);
            });
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
        let (checking, shown) = (folder.clone(), hash.clone());
        let check = cx.background_spawn(async move {
            let checked = launcher.registry().refresh(&Limit::after(REGISTRY_WAIT));
            let still_held = launcher.with_installed().is_held_back(&checking, &shown);
            still_held.then(|| match checked {
                Ok(_) => format!("{ARRIVED_NOTE} {NOT_SIGNED}"),
                Err(refused) => {
                    format!("{ARRIVED_NOTE} The directory couldn't vouch for it: {refused}")
                }
            })
        });
        self.arrival_check = Some(cx.spawn_in(window, async move |shell, cx| {
            let why = check.await;
            let _ = shell.update_in(cx, |shell, window, cx| {
                // The index may have trusted it: load whatever is trusted now.
                let shown = match why {
                    None => Shown::ByPlayer,
                    Some(_) => Shown::Synced,
                };
                shell.change_plugins(shown, |launcher| Ok(launcher.with_installed()), cx);
                if let Some(why) = why {
                    shell.settings.update(cx, |sheet, cx| {
                        sheet.ask_to_trust(&folder, &hash, why, window, cx);
                    });
                }
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
        self.change_plugins(
            Shown::ByPlayer,
            move |launcher| launcher.trust(&folder, &hash),
            cx,
        );
    }

    /// Shows what's installed now, as `shown` says.
    fn show_plugins_now(
        &mut self,
        now: Result<PluginsNow, String>,
        shown: Shown,
        cx: &mut Context<Shell>,
    ) {
        let now = match now {
            Ok(now) => now,
            Err(problem) => {
                return match shown {
                    Shown::Install => self.settings.update(cx, |sheet, cx| {
                        sheet.install_ended(Some(problem), cx);
                    }),
                    Shown::ByPlayer => self.settings.update(cx, |sheet, cx| {
                        sheet.show_plugin_problem(problem, cx);
                    }),
                    Shown::FolderSaved | Shown::Synced => {
                        eprintln!("couldn't show the plugins: {problem}");
                    }
                };
            }
        };
        let refused = now.launcher.refused().to_vec();
        self.watch_added_folders(now.launcher.added_folders(), cx);
        self.launcher = Some(now.launcher);
        self.settings.update(cx, |sheet, cx| {
            sheet.show_installed(now.installed, now.added, cx);
            sheet.show_refused(refused, cx);
            if let Some(rows) = now.rows {
                sheet.refresh_plugins(rows, shown.needs_connect(), cx);
            }
            if shown == Shown::Install {
                sheet.install_ended(None, cx);
            }
        });
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
