//! The plugin directory, a page of Plugins (PRD §11), and the approval a
//! plugin that sends commands asks for before it's installed or turned on.
//!
//! The shell fetches and checks the signed index off the UI thread, and this
//! page only shows what it hands over: a list, or why there's none. Nothing
//! that didn't check out is ever listed.

use super::add_plugin::AddedPlugin;
use super::plugins::{PluginRow, Sending};
use super::{
    Page, Pane, Section, SettingsSheet, SheetEvent, chosen_ground, control, muted_line, text_button,
};
use crate::assets::UI_FONT;
use crate::input::CommandInput;
use crate::theme::Theme;
use gpui::{
    AnyElement, App, Context, Div, Entity, FocusHandle, Focusable, FontWeight, Role, SharedString,
    Stateful, Window, div, prelude::*, px,
};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::PluginId;
use mudling_core::registry::{self, Entry, Fetched, Index};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

pub(super) const DIRECTORY_TITLE: &str = "Plugin directory";
const CHECKING: &str = "Checking the community directory…";
const NOTHING_MATCHES: &str = "No plugin matches.";
const EMPTY: &str = "The directory lists no plugins yet.";
const SENDS: &str = "sends commands";
const ANY_GAME: &str = "Any game";
const CANT_REACH: &str =
    "It can't read your files or reach the network. You can take this back on the Plugins page.";
const CHIP_HEIGHT: gpui::Pixels = px(28.);

/// A plugin downloaded and checked, with what installing it means here.
pub struct Downloaded {
    pub fetched: Arc<Fetched>,
    /// It updates an approved copy from where that came from, so it may
    /// send without asking again.
    pub keeps_approval: bool,
    /// The installed plugin from elsewhere it would replace, in words.
    pub replaces: Option<String>,
}

/// What the directory has to show.
#[derive(Clone, Debug, Default)]
pub enum Listing {
    #[default]
    Checking,
    /// An index that checked out, and when.
    Listed { index: Arc<Index>, at: Timestamp },
    /// Why there's nothing to list.
    Refused(SharedString),
}

/// Which of the listed plugins show.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum Filter {
    #[default]
    All,
    Game(String),
    Updates,
}

/// The page's state, kept while the sheet lives so the search survives.
pub(super) struct DirectoryPane {
    listing: Listing,
    /// What's installed here, by id, with its hash if it came from the
    /// directory.
    installed: BTreeMap<String, Option<String>>,
    search: Entity<CommandInput>,
    filter: Filter,
    /// The plugin being fetched or installed.
    busy: Option<String>,
    problem: Option<SharedString>,
}

impl DirectoryPane {
    pub(super) fn new(cx: &mut Context<SettingsSheet>) -> DirectoryPane {
        let search = cx.new(|cx| CommandInput::field("Search plugins", "Search plugins", cx));
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        DirectoryPane {
            listing: Listing::default(),
            installed: BTreeMap::new(),
            search,
            filter: Filter::default(),
            busy: None,
            problem: None,
        }
    }
}

/// What a listed plugin's button does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Offer {
    Install,
    Update,
    Installed,
}

fn offer(entry: &Entry, installed: &BTreeMap<String, Option<String>>) -> Offer {
    match installed.get(&entry.id) {
        None => Offer::Install,
        Some(Some(hash)) if *hash != entry.hash => Offer::Update,
        Some(_) => Offer::Installed,
    }
}

/// "Avatar · 0.4.1 · Apache-2.0 · sends commands".
fn entry_about(entry: &Entry) -> String {
    let game = entry.game().unwrap_or(ANY_GAME);
    let mut about = format!("{game} · {} · {}", entry.version, entry.license);
    if entry.sends_commands() {
        about.push_str(&format!(" · {SENDS}"));
    }
    about
}

/// Where a row sits in its card, which decides its corners and borders. Each
/// row is a row of the pane, so Tab can bring any of them into view.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum InCard {
    First,
    Middle,
    Last,
    Only,
}

impl InCard {
    pub(super) fn of(number: usize, count: usize) -> InCard {
        match (number == 0, number + 1 == count) {
            (true, true) => InCard::Only,
            (true, false) => InCard::First,
            (false, true) => InCard::Last,
            (false, false) => InCard::Middle,
        }
    }
}

/// A row of a card that's drawn row by row.
pub(super) fn card_row(at: InCard, theme: &Theme) -> Div {
    let top = matches!(at, InCard::First | InCard::Only);
    let bottom = matches!(at, InCard::Last | InCard::Only);
    div()
        .border_l_1()
        .border_r_1()
        .border_color(theme.border)
        .when(top, |row| row.mt_3().border_t_1().rounded_t_lg())
        .when(!top, |row| row.border_t_1().border_color(theme.divider))
        .when(bottom, |row| row.border_b_1().rounded_b_lg())
}

/// What the sheet asks before a plugin changes: approval for one that sends
/// commands, trust for a copy that arrived, or a removal.
pub(super) struct Approval {
    plugin: PluginId,
    name: SharedString,
    asks: Vec<SharedString>,
    purpose: Purpose,
    /// Where the keyboard goes back to.
    return_to: Option<FocusHandle>,
}

#[derive(Clone)]
pub(super) enum Purpose {
    /// Install `fetched`, saying first what it replaces, if anything.
    Install {
        fetched: Arc<Fetched>,
        replaces: Option<SharedString>,
    },
    TurnOn(SharedString),
    Review,
    /// Load a copy this device didn't install, which the directory doesn't
    /// vouch for: the words say why. It's the one in `folder`, shown with
    /// `hash`, and nothing else.
    Trust {
        why: SharedString,
        folder: SharedString,
        hash: SharedString,
    },
    /// Take out the plugin in this folder of the data folder.
    Remove(SharedString),
    /// Stop loading the folder the player added, leaving it be.
    Forget(PathBuf),
}

/// What the dialog says and offers for its purpose.
struct Words {
    title: String,
    /// Said first, before what it asks to do.
    lead: Option<SharedString>,
    shows_asks: bool,
    footer: &'static str,
    without: Option<&'static str>,
    with: &'static str,
}

const TRUST_FOOTER: &str = "Trusting this copy loads it from your next connect. It can send \
     commands only if you approve that too.";
const REMOVE_FOOTER: &str = "It comes off this device, and off your other devices when they \
     sync. Its approval goes with it, and it stops at your next connect.";
const FORGET_FOOTER: &str = "MUDling stops loading it at your next connect, and its approval \
     goes with it. Your folder and its files stay where they are.";

impl Approval {
    fn words(&self) -> Words {
        let name = &self.name;
        let approval = |without, with| Words {
            title: format!("Let {name} send commands?"),
            lead: None,
            shows_asks: true,
            footer: CANT_REACH,
            without,
            with,
        };
        match &self.purpose {
            Purpose::Install { replaces, .. } => Words {
                lead: replaces.clone(),
                ..approval(Some("Install without sending"), "Allow and install")
            },
            Purpose::TurnOn(_) => approval(Some("Turn on without sending"), "Allow and turn on"),
            Purpose::Review => approval(None, "Allow"),
            Purpose::Trust { why, .. } => Words {
                title: format!("Load this copy of {name}?"),
                lead: Some(why.clone()),
                shows_asks: !self.asks.is_empty(),
                footer: TRUST_FOOTER,
                without: None,
                with: "Trust this copy",
            },
            Purpose::Remove(_) => Words {
                title: format!("Remove {name}?"),
                lead: None,
                shows_asks: false,
                footer: REMOVE_FOOTER,
                without: None,
                with: "Remove",
            },
            Purpose::Forget(_) => Words {
                title: format!("Remove {name}?"),
                lead: None,
                shows_asks: false,
                footer: FORGET_FOOTER,
                without: None,
                with: "Remove",
            },
        }
    }
}

impl Approval {
    pub(super) fn for_row(
        row: &PluginRow,
        purpose: Purpose,
        window: &Window,
        cx: &App,
    ) -> Approval {
        Approval {
            plugin: row.id.clone(),
            name: row.name.clone(),
            asks: row.asks.clone(),
            purpose,
            return_to: window.focused(cx),
        }
    }

    /// Asks before taking out `plugin`, named `name`, which has no row on
    /// the Plugins page to ask from.
    pub(super) fn to_remove(
        plugin: PluginId,
        name: SharedString,
        purpose: Purpose,
        window: &Window,
        cx: &App,
    ) -> Approval {
        Approval {
            plugin,
            name,
            asks: Vec::new(),
            purpose,
            return_to: window.focused(cx),
        }
    }
}

impl SettingsSheet {
    pub fn open_directory(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.directory.problem = None;
        self.go_to_page(Page::Directory, window, cx);
        self._ago_ticks = Some(self.tick_ago(cx));
        cx.emit(SheetEvent::OpenDirectory);
    }

    /// What the shell found: the index, or why there's none, and what's
    /// installed here.
    pub fn show_directory(
        &mut self,
        listing: Listing,
        installed: BTreeMap<String, Option<String>>,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.directory.listing = listing;
        self.directory.installed = installed;
        cx.notify();
    }

    /// A plugin downloaded and checked. One that sends commands asks first,
    /// unless it's an update of an approved copy from where that came from.
    pub fn fetched(
        &mut self,
        downloaded: Result<Downloaded, String>,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        match downloaded {
            Err(problem) => self.plugin_work_ended(Some(problem.into())),
            Ok(Downloaded {
                fetched,
                keeps_approval: false,
                replaces,
            }) if registry::sends_commands(&fetched.plugin) => {
                let row = PluginRow::of(&fetched.plugin, false);
                let purpose = Purpose::Install {
                    fetched,
                    replaces: replaces.map(Into::into),
                };
                let approval = Approval::for_row(&row, purpose, window, cx);
                self.ask(approval, window, cx);
            }
            Ok(Downloaded { fetched, .. }) => cx.emit(SheetEvent::Install {
                fetched,
                approve: false,
            }),
        }
        cx.notify();
    }

    /// What's installed now, after a change to it, and what was added from
    /// links and folders.
    pub fn installed(
        &mut self,
        installed: BTreeMap<String, Option<String>>,
        added: Vec<AddedPlugin>,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.plugin_work_ended(None);
        self.directory.installed = installed;
        self.show_added(added, cx);
        cx.notify();
    }

    pub fn install_failed(&mut self, problem: String, cx: &mut Context<SettingsSheet>) {
        self.plugin_work_ended(Some(problem.into()));
        cx.notify();
    }

    /// The work of adding a plugin ended, with `problem` if it failed, which
    /// shows on the page the player is on.
    fn plugin_work_ended(&mut self, problem: Option<SharedString>) {
        self.directory.busy = None;
        self.add.busy = false;
        match self.page {
            Page::AddPlugin => self.add.problem = problem,
            _ => self.directory.problem = problem,
        }
    }

    fn get(&mut self, entry: Entry, index: Arc<Index>, cx: &mut Context<SettingsSheet>) {
        self.directory.busy = Some(entry.id.clone());
        self.directory.problem = None;
        cx.emit(SheetEvent::Fetch { index, entry });
        cx.notify();
    }

    fn filter(&mut self, filter: Filter, cx: &mut Context<SettingsSheet>) {
        self.directory.filter = filter;
        cx.notify();
    }

    pub(super) fn fill_directory(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let back = self.back_button(Section::Plugins, pane, theme, cx);
        pane.leading = Some(back.into_any_element());
        // Whatever the index says, a link or a folder can be added.
        let add = text_button(
            "directory-add",
            "Add from a link or folder",
            &pane.stop(cx),
            theme,
        )
        .on_click(cx.listener(|sheet, _, window, cx| sheet.open_add_plugin(window, cx)));
        pane.trailing = Some(add.into_any_element());
        let index = match self.directory.listing.clone() {
            Listing::Checking => {
                pane.row(muted_line(CHECKING, theme).role(Role::Status));
                return;
            }
            Listing::Refused(why) => {
                pane.row(
                    muted_line(why, theme)
                        .debug_selector(|| "directory-refused".to_owned())
                        .role(Role::Alert)
                        .text_color(theme.error),
                );
                return;
            }
            Listing::Listed { index, at } => (index, at),
        };
        let (index, at) = index;
        let search = self.directory.search.clone();
        pane.adopt(search.focus_handle(cx));
        pane.row(div().pb_2().child(search.clone()));
        self.fill_filters(&index, pane, theme, cx);
        if let Some(problem) = self.directory.problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "directory-problem".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        // An install or update reaches a session only when it opens.
        if self.installs_changed && self.server.is_some() {
            pane.row(self.reconnect_note(theme));
        }

        let typed = search.read(cx).content().trim().to_lowercase();
        let installed = &self.directory.installed;
        let shown: Vec<Entry> = (index.plugins.iter())
            .filter(|entry| match &self.directory.filter {
                Filter::All => true,
                Filter::Game(game) => entry.game() == Some(game.as_str()),
                Filter::Updates => offer(entry, installed) == Offer::Update,
            })
            .filter(|entry| {
                typed.is_empty()
                    || entry.name.to_lowercase().contains(&typed)
                    || entry_about(entry).to_lowercase().contains(&typed)
            })
            .cloned()
            .collect();
        if shown.is_empty() {
            let said = if index.plugins.is_empty() {
                EMPTY
            } else {
                NOTHING_MATCHES
            };
            pane.row(muted_line(said, theme).mt_3());
        }
        for (number, entry) in shown.iter().enumerate() {
            let at = InCard::of(number, shown.len());
            let row = self.directory_row(entry, &index, at, pane, theme, cx);
            pane.row(row);
        }
        let updated = format!(
            "Updated {} from the community registry.",
            super::data::ago(at, Timestamp::now())
        );
        pane.row(muted_line(updated, theme).mt_3());
    }

    fn fill_filters(
        &self,
        index: &Index,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let mut games: Vec<&str> = index.plugins.iter().filter_map(Entry::game).collect();
        games.sort_unstable();
        games.dedup();
        let installed = &self.directory.installed;
        let updates = (index.plugins.iter())
            .filter(|entry| offer(entry, installed) == Offer::Update)
            .count();
        let filters = std::iter::once(("All games".to_owned(), Filter::All))
            .chain(
                games
                    .into_iter()
                    .map(|g| (g.to_owned(), Filter::Game(g.to_owned()))),
            )
            .chain(std::iter::once((
                format!("Updates ({updates})"),
                Filter::Updates,
            )));
        let chips: Vec<AnyElement> = filters
            .enumerate()
            .map(|(number, (name, filter))| {
                let chosen = self.directory.filter == filter;
                control(
                    ("directory-filter", number),
                    name.clone(),
                    &pane.stop(cx),
                    theme,
                )
                .role(Role::RadioButton)
                .aria_toggled(chosen.into())
                .h(CHIP_HEIGHT)
                .px_3()
                .rounded_xl()
                .border_color(if chosen { theme.accent } else { theme.border })
                .bg(chosen_ground(chosen, theme))
                .text_size(px(13.))
                .text_color(if chosen {
                    theme.accent
                } else {
                    theme.text_muted
                })
                .child(name)
                .on_click(cx.listener(move |sheet, _, _, cx| sheet.filter(filter.clone(), cx)))
                .into_any_element()
            })
            .collect();
        pane.row(
            div()
                .id("directory-filters")
                .role(Role::RadioGroup)
                .aria_label("Show")
                .pb_1()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(chips),
        );
    }

    fn directory_row(
        &self,
        entry: &Entry,
        index: &Arc<Index>,
        at: InCard,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Div {
        let about = entry_about(entry);
        let offered = offer(entry, &self.directory.installed);
        let busy = self.directory.busy.as_deref() == Some(entry.id.as_str());
        let words = div()
            .id(SharedString::from(format!("directory-{}", entry.id)))
            .role(Role::Label)
            .aria_value(format!("{}, {about}", entry.name))
            .flex_1()
            .min_w_0()
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(entry.name.clone()),
            )
            .child(about_line(entry, theme));
        let action: AnyElement = match (busy, offered) {
            (true, _) => muted_line("Installing…", theme)
                .pb_0()
                .role(Role::Status)
                .into_any_element(),
            (false, Offer::Installed) => muted_line("Installed", theme).pb_0().into_any_element(),
            (false, offered) => {
                let label = match offered {
                    Offer::Update => format!("Update to {}", entry.version),
                    _ => "Install".to_owned(),
                };
                let (entry, index) = (entry.clone(), index.clone());
                text_button(
                    SharedString::from(format!("directory-get-{}", entry.id)),
                    label,
                    &pane.stop(cx),
                    theme,
                )
                .aria_label(format!("{} {}", offer_verb(&offered), entry.name))
                .text_color(theme.accent)
                .border_color(theme.accent)
                .on_click(cx.listener(move |sheet, _, _, cx| {
                    sheet.get(entry.clone(), index.clone(), cx);
                }))
                .into_any_element()
            }
        };
        card_row(at, theme)
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .font_family(UI_FONT)
            .child(words)
            .child(action)
    }

    /// Asks whether the session's `plugin` may send commands.
    pub fn review(
        &mut self,
        plugin: &PluginId,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(row) = self.plugins.iter().find(|row| row.id == *plugin) else {
            return;
        };
        let approval = Approval::for_row(row, Purpose::Review, window, cx);
        self.ask(approval, window, cx);
    }

    /// Asks before taking `plugin` out of the data folder, or forgetting the
    /// folder it was added from.
    #[cfg(all(test, target_os = "macos"))]
    pub fn confirm_remove(
        &mut self,
        plugin: &PluginId,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(row) = self.plugins.iter().find(|row| row.id == *plugin) else {
            return;
        };
        let Some(removal) = row.removal() else {
            return;
        };
        let confirm = Approval::for_row(row, removal, window, cx);
        self.ask(confirm, window, cx);
    }

    /// Asks whether to trust the copy in `folder` shown with `hash`, which
    /// the directory didn't vouch for, saying `why`. A row no longer showing
    /// that copy asks nothing.
    pub fn ask_to_trust(
        &mut self,
        folder: &str,
        hash: &str,
        why: String,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let shown = |row: &&PluginRow| {
            row.folder.as_deref() == Some(folder) && row.held_back.as_deref() == Some(hash)
        };
        let Some(row) = self.plugins.iter().find(shown) else {
            return;
        };
        let purpose = Purpose::Trust {
            why: why.into(),
            folder: folder.to_owned().into(),
            hash: hash.to_owned().into(),
        };
        let approval = Approval::for_row(row, purpose, window, cx);
        self.ask(approval, window, cx);
    }

    /// Opens the approval over the sheet, with the keyboard on its first
    /// button.
    pub(super) fn ask(
        &mut self,
        approval: Approval,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.asking = Some(approval);
        // Cancel takes the pane's first stop when the approval draws.
        if self.stops.handles.is_empty() {
            self.stops.handles.push(cx.focus_handle());
        }
        window.focus(&self.stops.handles[0], cx);
        cx.notify();
    }

    /// Closes an approval left unanswered, as Cancel would.
    pub(super) fn drop_approval(&mut self) {
        if let Some(Approval {
            purpose: Purpose::Install { .. },
            ..
        }) = self.asking.take()
        {
            self.plugin_work_ended(None);
        }
    }

    /// Closes the approval, doing what the player chose: `send` lets the
    /// plugin send commands, and `go_on` installs or turns it on.
    pub(super) fn answer(
        &mut self,
        go_on: bool,
        send: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let Some(approval) = self.asking.take() else {
            return;
        };
        let plugin = approval.plugin.clone();
        match approval.purpose {
            Purpose::Install { fetched, .. } if go_on => {
                cx.emit(SheetEvent::Install {
                    fetched,
                    approve: send,
                });
            }
            Purpose::Install { .. } => self.plugin_work_ended(None),
            Purpose::TurnOn(host) if go_on => {
                if send {
                    self.set_approved(plugin.clone(), true, cx);
                }
                self.set_plugin(host, plugin, true, cx);
            }
            Purpose::Review if go_on && send => self.set_approved(plugin, true, cx),
            Purpose::Trust { folder, hash, .. } if go_on => {
                cx.emit(SheetEvent::Trust { folder, hash });
            }
            Purpose::Remove(folder) if go_on => cx.emit(SheetEvent::Remove { plugin, folder }),
            Purpose::Forget(path) if go_on => cx.emit(SheetEvent::ForgetFolder {
                plugin: Some(plugin),
                path,
            }),
            Purpose::TurnOn(_)
            | Purpose::Review
            | Purpose::Trust { .. }
            | Purpose::Remove(_)
            | Purpose::Forget(_) => {}
        }
        if let Some(back) = approval.return_to {
            window.focus(&back, cx);
        }
        cx.notify();
    }

    /// Saves whether `plugin` may send commands, and shows it at once.
    pub(super) fn set_approved(
        &mut self,
        plugin: PluginId,
        approved: bool,
        cx: &mut Context<SettingsSheet>,
    ) {
        let sending = if approved {
            Sending::Approved
        } else {
            Sending::Waiting
        };
        for row in self.plugins.iter_mut().filter(|row| row.id == plugin) {
            row.sending = sending;
        }
        cx.emit(SheetEvent::SetApproved { plugin, approved });
        cx.notify();
    }

    /// The approval, in the place of the sheet's sections.
    pub(super) fn render_approval(
        &mut self,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Option<impl IntoElement + use<>> {
        let approval = self.asking.as_ref()?;
        let mut stops = std::mem::take(&mut self.stops);
        stops.start_frame();
        let mut pane = Pane {
            rows: Vec::new(),
            stops,
            leading: None,
            trailing: None,
        };
        let Words {
            title,
            lead,
            shows_asks,
            footer,
            without,
            with,
        } = approval.words();
        let asks = approval.asks.iter().map(|ask| {
            div()
                .id(ask.clone())
                .role(Role::ListItem)
                .aria_label(ask.clone())
                .flex()
                .gap_2()
                .child("•")
                .child(div().flex_1().min_w_0().child(ask.clone()))
        });
        let cancel = text_button("approval-cancel", "Cancel", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer(false, false, window, cx)));
        let without = without.map(|label| {
            text_button("approval-without", label, &pane.stop(cx), theme)
                .on_click(cx.listener(|sheet, _, window, cx| sheet.answer(true, false, window, cx)))
        });
        let allow = text_button("approval-allow", with, &pane.stop(cx), theme)
            .bg(theme.accent)
            .border_color(theme.accent)
            .text_color(theme.on_accent)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.answer(true, true, window, cx)));
        self.stops = pane.stops;
        let dialog = div()
            .id("approval")
            .debug_selector(|| "approval".to_owned())
            .role(Role::AlertDialog)
            .aria_label(title.clone())
            .w(px(460.))
            .p_5()
            .rounded_xl()
            .border_1()
            .border_color(theme.border)
            .bg(theme.surface_raised)
            .font_family(UI_FONT)
            .child(
                div()
                    .id("approval-title")
                    .role(Role::Heading)
                    .aria_label(title.clone())
                    .aria_level(2)
                    .pb_1()
                    .text_size(px(16.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title),
            )
            .children(lead.map(|lead| muted_line(lead, theme).text_color(theme.text)))
            .when(shows_asks, |dialog| {
                dialog.child(muted_line("It asks to:", theme).pb_1()).child(
                    div()
                        .id("approval-asks")
                        .role(Role::List)
                        .aria_label("It asks to")
                        .pb_3()
                        .text_size(px(14.))
                        .text_color(theme.text_muted)
                        .children(asks),
                )
            })
            .child(muted_line(footer, theme).text_size(px(13.)))
            .child(
                div()
                    .pt_2()
                    .flex()
                    .justify_end()
                    .gap_2()
                    .child(cancel)
                    .children(without)
                    .child(allow),
            );
        Some(
            div()
                .flex_1()
                .h_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.scrim)
                .child(dialog),
        )
    }
}

fn offer_verb(offered: &Offer) -> &'static str {
    match offered {
        Offer::Update => "Update",
        _ => "Install",
    }
}

/// A row's smaller line, with "sends commands" in the warning color.
fn about_line(entry: &Entry, theme: &Theme) -> Stateful<Div> {
    let game = entry.game().unwrap_or(ANY_GAME);
    div()
        .id(SharedString::from(format!("directory-about-{}", entry.id)))
        .flex()
        .flex_wrap()
        .text_size(px(13.))
        .text_color(theme.text_muted)
        .child(format!("{game} · {} · {}", entry.version, entry.license))
        .when(entry.sends_commands(), |line| {
            line.child(" · ")
                .child(div().text_color(theme.warning).child(SENDS))
        })
}
