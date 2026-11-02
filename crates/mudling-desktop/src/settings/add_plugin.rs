//! Adding a plugin from a git link or from a folder on this device, a page
//! of the directory (PRD §11). The shell fetches and checks a link, and
//! reads and watches a folder, off the UI thread; this page shows what it
//! hands over, and lists what the player added, with Remove.

use super::directory::{Approval, InCard, Purpose, card_row};
use super::plugins::broken_reload;
use super::{Page, Pane, SettingsSheet, SheetEvent, muted_line, section_heading, text_button};
use crate::assets::UI_FONT;
use crate::input::{CommandInput, InputEvent};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, Div, Entity, Focusable, FontWeight, PathPromptOptions, Role, SharedString,
    Stateful, Window, div, prelude::*, px,
};
use mudling_core::persist::Timestamp;
use mudling_core::plugin::PluginId;
use std::path::{Path, PathBuf};

pub(super) const ADD_TITLE: &str = "Add a plugin";
const FROM_LINK: &str = "From a git link";
const FROM_FOLDER: &str = "From a folder on this device";
const LINK_LABEL: &str = "Git link";
const LINK_PLACEHOLDER: &str = "https://github.com/someone/my-plugin.git";
const LINK_NOTE: &str = "MUDling checks it the way the directory checks plugins: its manifest, \
     its patterns, its tests and its cost.";
const CHOOSE: &str = "Choose a folder…";
const CHOOSE_NOTE: &str = "For your own plugin. It reloads when a file in it changes.";
const SYNC_NOTE: &str = "A plugin from a link or the directory syncs to your other devices with \
     your data. A folder plugin stays on this device.";
const CHECKING: &str = "Checking…";
const WAITS_FOR_CHECK: &str = "Waits until the link being checked is done.";

/// A plugin the player added from a link or a folder.
#[derive(Clone, Debug, PartialEq)]
pub struct AddedPlugin {
    /// Its id, unless its folder has never loaded.
    pub plugin: Option<PluginId>,
    pub name: SharedString,
    pub from: Added,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Added {
    /// Installed from `url` in the data folder's `folder`.
    Link {
        url: SharedString,
        folder: SharedString,
    },
    /// Loaded from where it is on this device.
    Folder {
        path: PathBuf,
        /// When the copy that runs loaded.
        loaded_at: Option<Timestamp>,
        /// Why the files there now don't load.
        problem: Option<SharedString>,
    },
}

/// `path` as the player knows it, with their home folder as `~`.
pub fn shown_path(path: &Path) -> String {
    let home = std::env::home_dir();
    match home
        .as_deref()
        .and_then(|home| path.strip_prefix(home).ok())
    {
        Some(inside) => format!("~/{}", inside.display()),
        None => path.display().to_string(),
    }
}

/// The page's state, kept while the sheet lives so a typed link survives.
pub(super) struct AddPane {
    link: Entity<CommandInput>,
    /// A link is being checked, or a folder read.
    pub(super) busy: bool,
    pub(super) problem: Option<SharedString>,
    added: Vec<AddedPlugin>,
}

impl AddPane {
    pub(super) fn new(window: &mut Window, cx: &mut Context<SettingsSheet>) -> AddPane {
        let link = cx.new(|cx| CommandInput::field(LINK_LABEL, LINK_PLACEHOLDER, cx));
        cx.subscribe_in(&link, window, |sheet, _, event, _, cx| {
            if let InputEvent::Submitted(_) = event {
                sheet.add_link(cx);
            }
        })
        .detach();
        cx.observe(&link, |_, _, cx| cx.notify()).detach();
        AddPane {
            link,
            busy: false,
            problem: None,
            added: Vec::new(),
        }
    }
}

impl SettingsSheet {
    pub fn open_add_plugin(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        self.add.problem = None;
        self.go_to_page(Page::AddPlugin, window, cx);
        self._ago_ticks = Some(self.tick_ago(cx));
        cx.emit(SheetEvent::OpenAddPlugin);
    }

    /// What the player added from links and folders, as it is now.
    pub fn show_added(&mut self, added: Vec<AddedPlugin>, cx: &mut Context<SettingsSheet>) {
        if self.add.added != added {
            self.add.added = added;
            cx.notify();
        }
    }

    fn add_link(&mut self, cx: &mut Context<SettingsSheet>) {
        let url = self.add.link.read(cx).content().trim().to_owned();
        if url.is_empty() || self.add.busy {
            return;
        }
        self.add.busy = true;
        self.add.problem = None;
        cx.emit(SheetEvent::AddLink(url));
        cx.notify();
    }

    /// The OS's own folder chooser, which the keyboard and a screen reader
    /// can already use.
    fn choose_plugin_folder(&mut self, window: &mut Window, cx: &mut Context<SettingsSheet>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add this plugin".into()),
        });
        cx.spawn_in(window, async move |sheet, cx| {
            if let Ok(Ok(Some(folders))) = chosen.await
                && let Some(folder) = folders.into_iter().next()
            {
                let _ = sheet.update(cx, |sheet, cx| {
                    sheet.add.busy = true;
                    sheet.add.problem = None;
                    cx.emit(SheetEvent::AddFolder(folder));
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Asks before taking out a plugin added from a link, or stopping a
    /// folder's. A folder that never loaded has nothing running to lose, and
    /// its files stay, so it goes at once.
    fn remove_added(
        &mut self,
        added: &AddedPlugin,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let purpose = match &added.from {
            Added::Link { folder, .. } => Purpose::Remove(folder.clone()),
            Added::Folder { path, .. } => Purpose::Forget(path.clone()),
        };
        let Some(plugin) = added.plugin.clone() else {
            if let Purpose::Forget(path) = purpose {
                cx.emit(SheetEvent::ForgetFolder { plugin: None, path });
            }
            return;
        };
        let confirm = Approval::to_remove(plugin, added.name.clone(), purpose, window, cx);
        self.ask(confirm, window, cx);
    }

    pub(super) fn fill_add_plugin(
        &mut self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        let back = text_button("page-back", "Back", &pane.stop(cx), theme).on_click(cx.listener(
            |sheet, _, window, cx| {
                sheet.drop_approval();
                sheet.go_to_page(Page::Directory, window, cx);
            },
        ));
        pane.leading = Some(back.into_any_element());
        if let Some(problem) = self.add.problem.clone() {
            pane.row(
                muted_line(problem, theme)
                    .debug_selector(|| "add-problem".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.error),
            );
        }
        if self.installs_changed && self.server.is_some() {
            pane.row(self.reconnect_note(theme));
        }

        pane.row(section_heading(FROM_LINK, theme));
        let links: Vec<AddedPlugin> = (self.add.added.iter())
            .filter(|added| matches!(added.from, Added::Link { .. }))
            .cloned()
            .collect();
        let field = self.add.link.clone();
        pane.adopt(field.focus_handle(cx));
        let add: AnyElement = if self.add.busy {
            muted_line(CHECKING, theme)
                .pb_0()
                .role(Role::Status)
                .into_any_element()
        } else {
            text_button("add-link", "Add", &pane.stop(cx), theme)
                .aria_label("Add the plugin at this link")
                .bg(theme.accent)
                .border_color(theme.accent)
                .text_color(theme.on_accent)
                .on_click(cx.listener(|sheet, _, _, cx| sheet.add_link(cx)))
                .into_any_element()
        };
        let count = 1 + links.len();
        pane.row(
            card_row(InCard::of(0, count), theme)
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .child(div().flex_1().min_w_0().child(field))
                .child(add),
        );
        for (number, added) in links.iter().enumerate() {
            let at = InCard::of(number + 1, count);
            let row = self.added_row(added, at, pane, theme, cx);
            pane.row(row);
        }
        pane.row(muted_line(LINK_NOTE, theme).pt_2().text_size(px(13.)));

        pane.row(section_heading(FROM_FOLDER, theme));
        let folders: Vec<AddedPlugin> = (self.add.added.iter())
            .filter(|added| matches!(added.from, Added::Folder { .. }))
            .cloned()
            .collect();
        let count = 1 + folders.len();
        let choose = text_button("add-folder", "Choose", &pane.stop(cx), theme)
            .aria_label("Choose a plugin folder");
        let choose = self.unless_checking(choose, |button| {
            button.on_click(
                cx.listener(|sheet, _, window, cx| sheet.choose_plugin_folder(window, cx)),
            )
        });
        pane.row(
            card_row(InCard::of(0, count), theme)
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_3()
                .font_family(UI_FONT)
                .child(row_words(
                    "add-folder-words",
                    CHOOSE.into(),
                    CHOOSE_NOTE.into(),
                    None,
                    theme,
                ))
                .child(choose),
        );
        for (number, added) in folders.iter().enumerate() {
            let at = InCard::of(number + 1, count);
            let row = self.added_row(added, at, pane, theme, cx);
            pane.row(row);
        }
        pane.row(muted_line(SYNC_NOTE, theme).mt_3().text_size(px(13.)));
    }

    /// `button`, made to work by `working`, unless a link is being checked:
    /// changing the plugins then would drop the check, so it waits, faded
    /// and saying why.
    pub(super) fn unless_checking(
        &self,
        button: Stateful<Div>,
        working: impl FnOnce(Stateful<Div>) -> Stateful<Div>,
    ) -> Stateful<Div> {
        if self.add.busy {
            button.opacity(0.5).aria_description(WAITS_FOR_CHECK)
        } else {
            working(button)
        }
    }

    /// A plugin the player added: its name, where from, and Remove.
    fn added_row(
        &self,
        added: &AddedPlugin,
        at: InCard,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Div {
        let (key, about, problem) = match &added.from {
            Added::Link { url, folder } => (format!("link-{folder}"), url.to_string(), None),
            Added::Folder {
                path,
                loaded_at,
                problem,
            } => {
                let reloaded = loaded_at
                    .map(|at| format!(" · reloaded {}", super::data::ago(at, Timestamp::now())));
                let about = format!("{}{}", shown_path(path), reloaded.unwrap_or_default());
                let problem = problem.as_ref().map(|problem| match added.plugin {
                    Some(_) => broken_reload(problem),
                    None => format!("It doesn't load: {problem}."),
                });
                (format!("folder-{}", path.display()), about, problem)
            }
        };
        let remove = text_button(
            SharedString::from(format!("remove-{key}")),
            "Remove",
            &pane.stop(cx),
            theme,
        )
        .aria_label(format!("Remove {}", added.name));
        let remove = self.unless_checking(remove, |button| {
            let added = added.clone();
            button.on_click(cx.listener(move |sheet, _, window, cx| {
                sheet.remove_added(&added, window, cx);
            }))
        });
        let words = row_words(
            SharedString::from(format!("added-{key}")),
            added.name.clone(),
            about.into(),
            problem,
            theme,
        );
        card_row(at, theme)
            .flex()
            .items_center()
            .gap_3()
            .px_4()
            .py_3()
            .font_family(UI_FONT)
            .child(words)
            .child(remove)
    }

    /// Fills the page with made-up additions, as the design draws it: for
    /// measuring and looking at it.
    pub fn show_sample_added(&mut self, cx: &mut Context<SettingsSheet>) {
        let minutes_ago = |minutes: i64| Timestamp(Timestamp::now().0 - minutes * 60_000);
        let plugin = |id: &str| PluginId::try_from(id.to_owned()).ok();
        self.add.added = vec![
            AddedPlugin {
                plugin: plugin("tell-colors"),
                name: "Tell colors".into(),
                from: Added::Link {
                    url: "https://github.com/someone/tell-colors.git".into(),
                    folder: "tell-colors".into(),
                },
            },
            AddedPlugin {
                plugin: plugin("my-game"),
                name: "my-game".into(),
                from: Added::Folder {
                    path: "/Users/sample/plugins/my-game".into(),
                    loaded_at: Some(minutes_ago(0)),
                    problem: None,
                },
            },
            AddedPlugin {
                plugin: plugin("prompts"),
                name: "Prompts".into(),
                from: Added::Folder {
                    path: "/Users/sample/plugins/prompts".into(),
                    loaded_at: Some(minutes_ago(14)),
                    problem: Some("text.toml: pattern 2 doesn't compile: unclosed group".into()),
                },
            },
        ];
        cx.notify();
    }
}

/// A row's name, with its smaller line under it and what's wrong, if
/// anything, read as one.
fn row_words(
    id: impl Into<SharedString>,
    name: SharedString,
    about: SharedString,
    problem: Option<String>,
    theme: &Theme,
) -> gpui::Stateful<Div> {
    let problem_said = problem.as_ref().map(|problem| format!(". {problem}"));
    div()
        .id(id.into())
        .role(Role::Label)
        .aria_value(format!(
            "{name}, {about}{}",
            problem_said.unwrap_or_default()
        ))
        .flex_1()
        .min_w_0()
        .child(
            div()
                .text_size(px(15.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child(name),
        )
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme.text_muted)
                .child(about),
        )
        .children(problem.map(|problem| {
            div()
                .text_size(px(13.))
                .text_color(theme.warning)
                .child(problem)
        }))
}
