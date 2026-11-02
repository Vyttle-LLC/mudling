//! The Plugins section: what's installed for this session, by layer, and the
//! session's notices.
//!
//! It shows what the core publishes today, and that's less than the canvas:
//! no description or source for a plugin. Each plugin's cost per line is
//! sampled, so it shows after a few hundred lines. A plugin the core turned
//! off has a banner that can turn it on again. The Inspector opens from here.

use super::appearance::toggled;
use super::directory::{Approval, InCard, Purpose, card_row};
use super::{
    Page, Pane, SettingsSheet, SheetEvent, control, muted_line, section_heading, text_button,
};
use crate::assets::{Icon, UI_FONT};
use crate::theme::Theme;
use gpui::{
    AnyElement, Context, Div, FontWeight, HighlightStyle, Pixels, Role, SharedString, Stateful,
    StyledText, Window, div, prelude::*, px,
};
use mudling_core::defaults;
use mudling_core::plugin::luau::{DisabledReason, OVERRUNS_TO_TURN_OFF, PluginDisabled};
use mudling_core::plugin::manifest::Permission;
use mudling_core::plugin::{Layer, Plugin, PluginCost, PluginId, ServerInfo};
use mudling_core::registry::{self, Refused};
use mudling_core::stats::Summary;

const SWITCH_WIDTH: Pixels = px(40.);
const SWITCH_HEIGHT: Pixels = px(22.);
const SWITCH_KNOB: Pixels = px(16.);
const APPLIES_NEXT: &str = "Your changes take effect the next time you connect to this server.";
const NO_SESSION: &str = "Connect to a server to see its plugins.";
const NOT_MEASURED: &str = "Costs show here once a few hundred lines have been read.";
/// PRD §6: one line with all its plugins, and what any one of them may add.
const LINE_BUDGET_US: f64 = 20.;
const PLUGIN_BUDGET_US: f64 = 5.;
/// Room for a cost like "12.3 µs".
const COST_WIDTH: Pixels = px(64.);
const LAYERS_NOTE: &str = "Your overrides win over the game plugin, which wins over the defaults.";
const OVERRAN: &str = " One of its hooks ran past its time limit three times this session.";
const SENDS_COMMANDS: &str = "Sends commands";
const NEEDS_APPROVAL: &str = "Needs your approval to send commands";
const ARRIVED: &str = "Arrived from another device";
const SENDS_ASK: &str =
    "send commands to the game, which show in the chat like yours and are rate-limited";
const READS_ASK: &str = "read your character's state";
/// Under this, a banner's buttons wrap below its text.
const BANNER_TEXT_WIDTH: Pixels = px(220.);

// The banner spells the count out, as the canvas does.
const _: () = assert!(OVERRUNS_TO_TURN_OFF == 3);

/// A plugin as this section shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    pub id: PluginId,
    pub name: SharedString,
    pub layer: Layer,
    /// "1.0.0 · MIT", until manifests carry a description.
    pub version: SharedString,
    /// The triggers and aliases it brings, as [`rule_counts`] says them.
    pub holds: SharedString,
    /// Built in, so there's no turning it off.
    pub always_on: bool,
    /// Whether the player has it on for this server. A session reads that
    /// when it opens, so a change shows here before it runs.
    pub on: bool,
    /// The player's changes to its switch since the session opened that were
    /// saved or are saving.
    pub flips: u32,
    /// Where an override meant for another server is meant to run.
    pub elsewhere: Option<SharedString>,
    pub sending: Sending,
    /// What its hooks ask to do, as the approval says it.
    pub asks: Vec<SharedString>,
    /// Its folder in the data folder, for one the player can take out. A
    /// folder needn't be named for the plugin it holds.
    pub folder: Option<SharedString>,
    /// The content hash of a copy this device doesn't trust, as shown: it
    /// isn't loaded until the player trusts exactly that copy.
    pub held_back: Option<SharedString>,
}

/// Whether a plugin's hooks send commands, and whether the player let them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sending {
    No,
    Approved,
    /// Its hooks stay off until the player approves.
    Waiting,
}

/// What `plugin`'s permissions let its hooks do, in the approval's words.
pub fn asks_of(plugin: &Plugin) -> Vec<SharedString> {
    let mut asks: Vec<SharedString> = Vec::new();
    for permission in &plugin.manifest.permissions {
        let said = match permission {
            Permission::SendCommands => SENDS_ASK.into(),
            Permission::ReadState => READS_ASK.into(),
            Permission::WriteStore(store) => format!("keep its own notes in the store \"{store}\""),
        };
        asks.push(said.into());
    }
    // Sending first, as it's what the player is asked about.
    asks.sort_by_key(|ask| ask != SENDS_ASK);
    asks
}

impl PluginRow {
    /// Marks those of `rows` in `approved` as let to send.
    pub fn approve(rows: &mut [PluginRow], approved: &[PluginId]) {
        for row in rows.iter_mut().filter(|row| approved.contains(&row.id)) {
            if row.sending == Sending::Waiting {
                row.sending = Sending::Approved;
            }
        }
    }

    pub fn of(plugin: &Plugin, on: bool) -> PluginRow {
        let manifest = &plugin.manifest;
        let triggers = plugin.points.text.as_ref().map_or(0, |t| t.patterns.len());
        let aliases = plugin
            .points
            .commands
            .as_ref()
            .map_or(0, |c| c.aliases.len());
        PluginRow {
            id: plugin.id().clone(),
            name: manifest.name.clone().into(),
            layer: plugin.layer(),
            version: format!("{} · {}", manifest.version, manifest.license).into(),
            holds: rule_counts(triggers, aliases).into(),
            always_on: false,
            on,
            flips: 0,
            elsewhere: None,
            sending: match registry::sends_commands(plugin) {
                true => Sending::Waiting,
                false => Sending::No,
            },
            asks: asks_of(plugin),
            folder: None,
            held_back: None,
        }
    }

    /// The built-in defaults, then what's installed, on as a session on `host`
    /// would run them.
    pub fn for_session(
        plugins: &[Plugin],
        enabled: &[PluginId],
        overrides_off: &[PluginId],
        host: &str,
    ) -> Vec<PluginRow> {
        let server = ServerInfo {
            host,
            mssp_name: None,
        };
        let installed = plugins.iter().map(|plugin| {
            let is_override = plugin.layer() == Layer::Override;
            let matches_host = plugin.manifest.matches_server(server);
            let turned_off = overrides_off.contains(plugin.id());
            let on = match is_override {
                true => matches_host && !turned_off,
                false => enabled.contains(plugin.id()),
            };
            // Turning it off for this server says it's meant for this server.
            let elsewhere = (is_override && !matches_host && !turned_off).then(|| {
                let servers = &plugin.manifest.servers;
                let names: Vec<&str> = (servers.hosts.iter().chain(&servers.mssp_names))
                    .map(String::as_str)
                    .collect();
                names.join(", ").into()
            });
            PluginRow {
                elsewhere,
                ..PluginRow::of(plugin, on)
            }
        });
        let built_in = PluginRow {
            always_on: true,
            ..PluginRow::of(&defaults::plugin(), true)
        };
        std::iter::once(built_in).chain(installed).collect()
    }

    /// Whether its switch shows on: the core can turn off what the player left on.
    /// An override a game's MSSP name brought on shows in the core's costs, as
    /// nothing here knows that name.
    fn switched_on(&self, costs: &[PluginCost], turned_off: &[PluginDisabled]) -> bool {
        let came_on = self.layer == Layer::Override && self.flips == 0 && self.is_running(costs);
        (self.on || came_on) && !turned_off.iter().any(|off| off.plugin == self.id)
    }

    fn is_running(&self, costs: &[PluginCost]) -> bool {
        costs.iter().any(|cost| cost.plugin == self.id)
    }

    /// Where it's meant to run instead. A switch here wouldn't stick: the next
    /// session wouldn't run it, whatever was saved. One running here isn't.
    fn meant_elsewhere(&self, costs: &[PluginCost]) -> Option<&SharedString> {
        self.elsewhere.as_ref().filter(|_| !self.is_running(costs))
    }

    /// What it costs a line in µs at p99, once sampled.
    pub(super) fn cost(&self, costs: &[PluginCost]) -> Option<f64> {
        let sampled = costs.iter().find(|cost| cost.plugin == self.id)?;
        sampled.per_line.as_ref().map(|cost| cost.p99_us)
    }
}

pub(super) fn micros(cost: f64) -> String {
    format!("{cost:.1} µs")
}

/// "2 triggers, 1 alias".
pub(super) fn rule_counts(triggers: usize, aliases: usize) -> String {
    let plural = |count: usize, one: &str, many: &str| match count {
        1 => format!("1 {one}"),
        count => format!("{count} {many}"),
    };
    format!(
        "{}, {}",
        plural(triggers, "trigger", "triggers"),
        plural(aliases, "alias", "aliases")
    )
}

/// A cost in the column it shares, in the warning color over the limit.
pub(super) fn cost_label(cost: Option<f64>, theme: &Theme) -> Div {
    let over_limit = cost.is_some_and(|cost| cost > PLUGIN_BUDGET_US);
    div()
        .flex_none()
        .w(COST_WIDTH)
        .flex()
        .justify_end()
        .font_family(UI_FONT)
        .text_size(px(14.))
        .text_color(if over_limit {
            theme.warning
        } else {
            theme.text_muted
        })
        .child(cost.map_or_else(String::new, micros))
}

/// What each layer's plugins that run cost a line, top layer first, and what
/// they cost together against the budget. The whole comes from the one pass,
/// so it's not the layers added up: each of those times its rules alone.
fn cost_lines(
    rows: &[PluginRow],
    costs: &[PluginCost],
    turned_off: &[PluginDisabled],
    whole: &Summary,
) -> (String, String) {
    let layers = [Layer::Override, Layer::Game, Layer::Default].map(|layer| {
        let running = rows.iter().filter(|row| row.layer == layer);
        let cost = running
            .filter(|row| row.switched_on(costs, turned_off))
            .filter_map(|row| row.cost(costs))
            .fold(0., |total, cost| total + cost);
        format!("{} {}", layer_name(layer), micros(cost))
    });
    (
        layers.join(" · "),
        format!(
            "All plugins {} of {LINE_BUDGET_US} µs a line. One plugin may add \
             {PLUGIN_BUDGET_US} µs.",
            micros(whole.p99_us),
        ),
    )
}

fn layer_heading(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Your overrides",
        Layer::Game => "Game",
        Layer::Default => "Defaults · work on any GMCP server",
    }
}

fn layer_name(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Overrides",
        Layer::Game => "Game",
        Layer::Default => "Defaults",
    }
}

fn layer_badge(layer: Layer) -> &'static str {
    match layer {
        Layer::Override => "Local",
        Layer::Game => "Game plugin",
        Layer::Default => "Built in",
    }
}

/// The switch's look.
fn switch(on: bool, theme: &Theme) -> Div {
    let knob =
        div()
            .size(SWITCH_KNOB)
            .rounded_sm()
            .bg(if on { theme.on_accent } else { theme.text });
    div()
        .flex_none()
        .w(SWITCH_WIDTH)
        .h(SWITCH_HEIGHT)
        .p(px(3.))
        .flex()
        .when(on, |track| track.justify_end())
        .rounded_md()
        .bg(if on { theme.accent } else { theme.segment })
        .child(knob)
}

/// A row of the Plugins card.
enum CardItem<'a> {
    /// A layer, and whether it's the first.
    Heading(Layer, bool),
    Plugin(&'a PluginRow),
    Footer,
}

/// What a row's status button does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Its copy arrived and waits to be trusted.
    Arrived,
    Approved,
    Waiting,
}

/// A plugin's status, as words and the button that changes it. A copy that
/// arrived says so first: until it's trusted, nothing else about it counts.
fn status_words(row: &PluginRow) -> Option<(Status, &'static str, &'static str)> {
    if row.held_back.is_some() {
        return Some((Status::Arrived, ARRIVED, "Review"));
    }
    match row.sending {
        Sending::No => None,
        Sending::Approved => Some((Status::Approved, SENDS_COMMANDS, "Take back")),
        Sending::Waiting => Some((Status::Waiting, NEEDS_APPROVAL, "Review")),
    }
}

/// A plugin's row. `approval` is the line saying whether it may send
/// commands, with its button.
#[allow(clippy::too_many_arguments)]
fn plugin_row(
    row: &PluginRow,
    switched_on: bool,
    overran: bool,
    cost: Option<f64>,
    elsewhere: Option<&SharedString>,
    control: AnyElement,
    approval: Option<AnyElement>,
    theme: &Theme,
) -> Div {
    let about = match elsewhere {
        Some(servers) => format!("For {servers} · Version {}", row.version),
        None => format!("Version {}", row.version),
    };
    let sending_said = status_words(row)
        .map(|(_, words, _)| format!(" {words}."))
        .unwrap_or_default();
    let state = match (switched_on, overran) {
        _ if row.held_back.is_some() => "Not loaded",
        (true, _) => "On",
        (false, true) => "Turned off · over budget",
        (false, false) => "Off",
    };
    let cost_said = match cost {
        Some(cost) if cost > PLUGIN_BUDGET_US => format!(
            " {} a line, over the {PLUGIN_BUDGET_US} µs limit.",
            micros(cost)
        ),
        Some(cost) => format!(" {} a line.", micros(cost)),
        None => String::new(),
    };
    let id = row.id.clone();
    div()
        .debug_selector(move || format!("plugin-{id}"))
        .flex()
        .items_center()
        .gap_3()
        .px_5()
        .py_3()
        .font_family(UI_FONT)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .id(SharedString::from(format!("plugin-{}", row.id)))
                        .role(Role::Label)
                        .aria_value(format!(
                            "{}, {}: {state}. {about}.{cost_said}{sending_said}",
                            row.name,
                            layer_badge(row.layer),
                        ))
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap_x_2()
                                .text_size(px(15.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(row.name.clone())
                                .when(overran, |name| {
                                    name.child(
                                        div()
                                            .text_size(px(12.))
                                            .text_color(theme.warning)
                                            .child("Turned off · over budget"),
                                    )
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(13.))
                                .text_color(theme.text_muted)
                                .child(about),
                        ),
                )
                .children(approval),
        )
        .child(
            div()
                .flex_none()
                .px_2()
                .rounded_sm()
                .border_1()
                .border_color(theme.border)
                .text_size(px(12.))
                .text_color(theme.text_muted)
                .child(layer_badge(row.layer)),
        )
        .child(cost_label(cost, theme))
        .child(control)
}

/// A layer's heading. The first has the costs' column heading too.
fn layer_heading_row(layer: Layer, first: bool, theme: &Theme) -> impl IntoElement + use<> {
    div()
        .px_5()
        .flex()
        .items_end()
        .justify_between()
        .child(section_heading(layer_heading(layer), theme))
        .when(first, |row| {
            row.child(
                div()
                    .pb_2()
                    .mr(SWITCH_WIDTH + px(12.))
                    .font_family(UI_FONT)
                    .text_size(px(13.))
                    .text_color(theme.text_muted)
                    .child("per line"),
            )
        })
}

impl SettingsSheet {
    /// A plugin's working switch. One the core turned off comes back the way
    /// its notice's button brings it; any other is the player's for this server.
    pub(super) fn plugin_switch(
        &self,
        row: &PluginRow,
        on: bool,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> impl IntoElement + use<> {
        let plugin = row.clone();
        // The core's list is of the session running, not of any other server.
        let overran = self.page_host() == self.server
            && self.turned_off.iter().any(|off| off.plugin == row.id);
        let on = on && !overran;
        let state = if on { "on" } else { "off" };
        let id = SharedString::from(format!("switch-{}", row.id));
        control(id, format!("{}, {state}", row.name), &pane.stop(cx), theme)
            .role(Role::Switch)
            .aria_toggled(toggled(on))
            .child(switch(on, theme))
            .on_click(cx.listener(move |sheet, _, window, cx| {
                if overran {
                    cx.emit(SheetEvent::TurnOn(plugin.id.clone()));
                } else if let Some(host) = sheet.page_host() {
                    sheet.flip(&plugin, host, !on, window, cx);
                }
            }))
    }

    /// Turns a plugin on or off for `host`. Turning on one whose hooks wait
    /// for approval asks first.
    fn flip(
        &mut self,
        row: &PluginRow,
        host: SharedString,
        on: bool,
        window: &mut Window,
        cx: &mut Context<SettingsSheet>,
    ) {
        let sending = self.sending_of(&row.id).unwrap_or(row.sending);
        if on && sending == Sending::Waiting {
            let approval = Approval::for_row(row, Purpose::TurnOn(host), window, cx);
            self.ask(approval, window, cx);
        } else {
            self.set_plugin(host, row.id.clone(), on, cx);
        }
    }

    /// Whether `plugin` may send, as the session's rows say now.
    fn sending_of(&self, plugin: &PluginId) -> Option<Sending> {
        let row = self.plugins.iter().find(|row| row.id == *plugin);
        row.map(|row| row.sending)
    }

    /// The line saying whether a plugin's copy is trusted and whether it may
    /// send commands, with their buttons, and Remove for one the player
    /// installed.
    fn actions_line(
        &self,
        row: &PluginRow,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Option<AnyElement> {
        let small = |button: Stateful<Div>| button.h(px(26.)).px_2().text_size(px(13.));
        let status = status_words(row).map(|(status, words, action)| {
            let plugin = row.clone();
            let id = match status {
                Status::Arrived => format!("arrival-{}", row.id),
                _ => format!("approval-{}", row.id),
            };
            let button = small(text_button(
                SharedString::from(id),
                action,
                &pane.stop(cx),
                theme,
            ))
            .aria_label(format!("{action}: {words}, {}", row.name))
            .on_click(cx.listener(move |sheet, _, window, cx| match status {
                Status::Arrived => {
                    let (Some(folder), Some(hash)) = (&plugin.folder, &plugin.held_back) else {
                        return;
                    };
                    cx.emit(SheetEvent::ReviewArrival {
                        folder: folder.clone(),
                        hash: hash.clone(),
                    });
                }
                Status::Approved => sheet.set_approved(plugin.id.clone(), false, cx),
                Status::Waiting => {
                    let approval = Approval::for_row(&plugin, Purpose::Review, window, cx);
                    sheet.ask(approval, window, cx);
                }
            }));
            div()
                .flex()
                .items_center()
                .gap_2()
                .text_color(match status {
                    Status::Approved => theme.text_muted,
                    _ => theme.warning,
                })
                .child(words)
                .child(button)
        });
        let remove = row.folder.as_ref().map(|folder| {
            let (plugin, folder) = (row.clone(), folder.clone());
            small(text_button(
                SharedString::from(format!("remove-{}", row.id)),
                "Remove",
                &pane.stop(cx),
                theme,
            ))
            .aria_label(format!("Remove {}", row.name))
            .on_click(cx.listener(move |sheet, _, window, cx| {
                let purpose = Purpose::Remove(folder.clone());
                let confirm = Approval::for_row(&plugin, purpose, window, cx);
                sheet.ask(confirm, window, cx);
            }))
        });
        let approval = status;
        if approval.is_none() && remove.is_none() {
            return None;
        }
        let line = div()
            .pt_1()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .text_size(px(13.))
            .children(approval)
            .children(remove);
        Some(line.into_any_element())
    }

    /// Rows read again, as an install, a removal or a sync changed what's
    /// installed. The player's flips since the session opened still count,
    /// and so does a change the player made, which `by_player` says this is.
    pub fn refresh_plugins(
        &mut self,
        mut rows: Vec<PluginRow>,
        by_player: bool,
        cx: &mut Context<SettingsSheet>,
    ) {
        for row in &mut rows {
            let shown = self.plugins.iter().find(|shown| shown.id == row.id);
            row.flips = shown.map_or(0, |shown| shown.flips);
        }
        self.plugins = rows;
        self.installs_changed |= by_player;
        cx.notify();
    }

    /// Installed plugins that weren't loaded, and why.
    pub fn show_refused(&mut self, refused: Vec<Refused>, cx: &mut Context<SettingsSheet>) {
        if self.refused != refused {
            self.refused = refused;
            cx.notify();
        }
    }

    pub fn show_switch_problem(&mut self, problem: String, cx: &mut Context<SettingsSheet>) {
        self.switch_problem = Some(problem.into());
        cx.notify();
    }

    /// The server a switch on this page is for.
    fn page_host(&self) -> Option<SharedString> {
        match &self.page {
            Page::Server(page) => Some(page.host.clone()),
            _ => self.server.clone(),
        }
    }

    /// Saves the player's choice, and shows it at once on what's on the screen.
    pub(super) fn set_plugin(
        &mut self,
        host: SharedString,
        plugin: PluginId,
        on: bool,
        cx: &mut Context<SettingsSheet>,
    ) {
        self.with_plugin_rows(&host, &plugin, |row| {
            row.on = on;
            row.flips += 1;
        });
        cx.emit(SheetEvent::SetPlugin { host, plugin, on });
        cx.notify();
    }

    /// How a switch's save went. One that failed goes back, and says why until
    /// the next save works.
    pub fn plugin_saved(
        &mut self,
        host: &SharedString,
        plugin: &PluginId,
        on: bool,
        problem: Option<String>,
        cx: &mut Context<SettingsSheet>,
    ) {
        if problem.is_some() {
            self.with_plugin_rows(host, plugin, |row| {
                row.on = !on;
                row.flips = row.flips.saturating_sub(1);
            });
        }
        self.switch_problem = problem.map(Into::into);
        cx.notify();
    }

    /// Runs `change` on what's on the screen of `plugin` for `host`.
    fn with_plugin_rows(
        &mut self,
        host: &SharedString,
        plugin: &PluginId,
        change: impl Fn(&mut PluginRow),
    ) {
        let page = match &mut self.page {
            Page::Server(page) if page.host == *host => Some(&mut page.plugins),
            _ => None,
        };
        let playing = self.server.as_ref() == Some(host);
        let on_session = playing.then_some(&mut self.plugins);
        let rows = [page, on_session].into_iter().flatten().flatten();
        rows.filter(|row| row.id == *plugin).for_each(change);
    }

    /// Why the last switch wasn't saved.
    pub(super) fn switch_problem_line(&self, theme: &Theme) -> Option<Stateful<Div>> {
        self.switch_problem.clone().map(|problem| {
            muted_line(problem, theme)
                .debug_selector(|| "switch-problem".to_owned())
                .role(Role::Alert)
                .mt_2()
                .text_color(theme.error)
        })
    }

    /// Says when a switch flipped on the server being played takes effect.
    pub(super) fn reconnect_note(&self, theme: &Theme) -> Stateful<Div> {
        muted_line(APPLIES_NEXT, theme)
            .debug_selector(|| "reconnect-note".to_owned())
            .role(Role::Status)
            .mt_2()
    }

    pub(super) fn display_name(&self, id: &str) -> SharedString {
        let row = self.plugins.iter().find(|row| row.id.as_str() == id);
        row.map_or_else(|| id.to_owned().into(), |row| row.name.clone())
    }

    fn toggle_details(&mut self, notice: &str, cx: &mut Context<SettingsSheet>) {
        self.details = (self.details.as_deref() != Some(notice)).then(|| notice.to_owned());
        cx.notify();
    }

    /// A notice's banner. One about a plugin turned off, in `off`, has the
    /// canvas's words and its two buttons.
    fn notice_banner(
        &self,
        number: usize,
        notice: &str,
        off: Option<&PluginDisabled>,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> Stateful<Div> {
        let open = self.details.as_deref() == Some(notice);
        let (lead, rest) = match off {
            Some(off) => (
                format!("{} was turned off.", self.display_name(off.plugin.as_str())),
                match off.reason {
                    DisabledReason::Overran(_) => OVERRAN,
                },
            ),
            None => (notice.to_owned(), ""),
        };
        let bold = HighlightStyle {
            font_weight: Some(FontWeight::MEDIUM),
            ..HighlightStyle::default()
        };
        let text = SharedString::from(format!("{lead}{rest}"));
        let summary = div()
            .flex_1()
            .min_w(BANNER_TEXT_WIDTH)
            .text_size(px(14.))
            .text_color(theme.text)
            .child(StyledText::new(text.clone()).with_highlights([(0..lead.len(), bold)]));
        let buttons = off.map(|off| {
            let details = text_button(("notice-details", number), "Details", &pane.stop(cx), theme)
                .aria_expanded(open)
                .on_click(cx.listener({
                    let notice = notice.to_owned();
                    move |sheet, _, _, cx| sheet.toggle_details(&notice, cx)
                }));
            let turn_on = text_button(
                ("notice-turn-on", number),
                "Turn on again",
                &pane.stop(cx),
                theme,
            )
            .on_click(cx.listener({
                let plugin = off.plugin.clone();
                move |_, _, _, cx| cx.emit(SheetEvent::TurnOn(plugin.clone()))
            }));
            div()
                .flex_none()
                .flex()
                .gap_2()
                .child(details)
                .child(turn_on)
        });
        div()
            .id(("notice", number))
            .debug_selector(move || format!("notice-{number}"))
            .role(Role::Label)
            .aria_value(text)
            .mb_3()
            .p_3()
            .rounded_lg()
            .border_1()
            .border_color(theme.warning)
            .bg(theme.warning_ground)
            .font_family(UI_FONT)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_3()
                    .child(
                        Icon::Warning
                            .svg()
                            .flex_none()
                            .size(px(20.))
                            .text_color(theme.warning),
                    )
                    .child(summary)
                    .children(buttons),
            )
            .when(open, |banner| {
                banner.child(
                    muted_line(format!("{notice}."), theme)
                        .mt_2()
                        .pb_0()
                        .text_size(px(13.)),
                )
            })
    }

    pub(super) fn fill_plugins(
        &self,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) {
        for (number, off) in self.turned_off.iter().enumerate() {
            let notice = off.to_string();
            let banner = self.notice_banner(number, &notice, Some(off), pane, theme, cx);
            pane.row(banner);
        }
        let first = self.turned_off.len();
        for (number, notice) in self.notices.iter().enumerate() {
            let banner = self.notice_banner(first + number, notice, None, pane, theme, cx);
            pane.row(banner);
        }
        for refused in &self.refused {
            let said = format!("{} wasn't loaded: {}.", refused.id, refused.why);
            pane.row(
                muted_line(said, theme)
                    .debug_selector(|| "plugin-refused".to_owned())
                    .role(Role::Alert)
                    .text_color(theme.warning),
            );
        }
        let browse = text_button(
            "browse-directory",
            "Browse directory",
            &pane.stop(cx),
            theme,
        )
        .on_click(cx.listener(|sheet, _, window, cx| sheet.open_directory(window, cx)));
        if self.plugins.is_empty() {
            pane.row(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(muted_line(NO_SESSION, theme).flex_1().min_w_0().pb_0())
                    .child(browse),
            );
            return;
        }
        let scope = match &self.server {
            Some(server) => format!("Plugins for {server}. {LAYERS_NOTE}"),
            None => LAYERS_NOTE.to_owned(),
        };
        let inspector = text_button("open-inspector", "Inspector", &pane.stop(cx), theme)
            .on_click(cx.listener(|sheet, _, window, cx| sheet.open_inspector(window, cx)));
        pane.row(
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(muted_line(scope, theme).flex_1().min_w_0().pb_0())
                .child(browse)
                .child(inspector),
        );
        if let Some(problem) = self.switch_problem_line(theme) {
            pane.row(problem);
        }
        if self.reconnect_to_apply() {
            pane.row(self.reconnect_note(theme));
        }
        // Each heading and plugin is a row of its own, so Tab can bring any
        // switch into view however many there are.
        let mut card: Vec<CardItem<'_>> = Vec::new();
        for layer in [Layer::Override, Layer::Game, Layer::Default] {
            let in_layer = self.plugins.iter().filter(|p| p.layer == layer);
            let mut in_layer = in_layer.map(CardItem::Plugin).peekable();
            if in_layer.peek().is_some() {
                card.push(CardItem::Heading(layer, card.is_empty()));
                card.extend(in_layer);
            }
        }
        card.push(CardItem::Footer);
        let count = card.len();
        for (number, item) in card.into_iter().enumerate() {
            let content = match item {
                CardItem::Heading(layer, first) => {
                    layer_heading_row(layer, first, theme).into_any_element()
                }
                CardItem::Plugin(row) => self.plugin_card_row(row, pane, theme, cx),
                CardItem::Footer => self.cost_footer(theme).into_any_element(),
            };
            pane.row(card_row(InCard::of(number, count), theme).child(content));
        }
    }

    fn plugin_card_row(
        &self,
        row: &PluginRow,
        pane: &mut Pane,
        theme: &Theme,
        cx: &mut Context<SettingsSheet>,
    ) -> AnyElement {
        let switched_on = row.switched_on(&self.costs, &self.turned_off);
        let elsewhere = row.meant_elsewhere(&self.costs);
        let approval = self.actions_line(row, pane, theme, cx);
        let control = match (row.always_on, elsewhere) {
            (true, _) => switch(true, theme).into_any_element(),
            // Not loaded, so there's nothing to switch until it's trusted.
            _ if row.held_back.is_some() => div().w(SWITCH_WIDTH).into_any_element(),
            (false, Some(_)) => div().w(SWITCH_WIDTH).into_any_element(),
            (false, None) => self
                .plugin_switch(row, switched_on, pane, theme, cx)
                .into_any_element(),
        };
        plugin_row(
            row,
            switched_on,
            self.turned_off.iter().any(|off| off.plugin == row.id),
            row.cost(&self.costs),
            elsewhere,
            control,
            approval,
            theme,
        )
        .into_any_element()
    }

    /// The layers' costs and the total against the budgets, closing the card.
    fn cost_footer(&self, theme: &Theme) -> Stateful<Div> {
        let (layers, total) = match &self.line_cost {
            Some(whole) => cost_lines(&self.plugins, &self.costs, &self.turned_off, whole),
            None => (String::new(), NOT_MEASURED.to_owned()),
        };
        let over_budget = (self.line_cost.as_ref()).is_some_and(|w| w.p99_us > LINE_BUDGET_US);
        div()
            .id("plugin-costs")
            .debug_selector(|| "plugin-costs".to_owned())
            .role(Role::Label)
            .aria_value(format!("{layers}. {total}"))
            .px_5()
            .py_3()
            .font_family(UI_FONT)
            .text_size(px(13.))
            .text_color(theme.text_muted)
            .when(!layers.is_empty(), |footer| footer.child(layers))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if over_budget {
                        theme.warning
                    } else {
                        theme.text
                    })
                    .child(total),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mudling_core::stats::Summary;
    use std::time::Duration;

    fn plugin(toml: &'static str) -> Plugin {
        Plugin::from_files(move |file| match file {
            "plugin.toml" => Ok(toml.to_owned()),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                file.to_owned(),
            )),
        })
        .unwrap()
    }

    const A_GAME: &str = "id = \"a-game\"\nname = \"A game\"\nversion = \"1.0.0\"\napi = 1\n\
                          license = \"MIT\"\nlayer = \"game\"\n\n[servers]\nhosts = [\"a.example\"]\n";

    const AN_OVERRIDE: &str = "id = \"mine\"\nname = \"Mine\"\nversion = \"1.0.0\"\napi = 1\n\
                               license = \"MIT\"\nlayer = \"override\"\n\n[servers]\n\
                               hosts = [\"a.example\"]\n";

    #[test]
    fn the_session_lists_the_defaults_then_what_is_installed_and_on() {
        let game = plugin(A_GAME);
        let off = PluginRow::for_session(std::slice::from_ref(&game), &[], &[], "a.example");
        assert_eq!(off.len(), 2);
        assert_eq!(off[0].layer, Layer::Default);
        assert!(off[0].on && off[0].always_on);
        assert_eq!((off[1].name.as_str(), off[1].on), ("A game", false));
        assert!(!off[1].always_on);

        let enabled = [game.id().clone()];
        let on = PluginRow::for_session(std::slice::from_ref(&game), &enabled, &[], "a.example");
        assert!(on[1].on);
    }

    #[test]
    fn an_override_runs_on_its_server_until_the_player_turns_it_off() {
        let mine = plugin(AN_OVERRIDE);
        let off = [mine.id().clone()];
        let rows = |host, off: &[PluginId]| {
            PluginRow::for_session(std::slice::from_ref(&mine), &[], off, host)[1].on
        };
        assert!(rows("a.example", &[]));
        assert!(!rows("a.example", &off));
        assert!(!rows("b.example", &[]), "it's for another server");
    }

    #[test]
    fn a_switch_is_the_players_choice_unless_the_core_turned_it_off() {
        let game = plugin(A_GAME);
        let id = game.id().clone();
        let enabled = [id.clone()];
        let row =
            &PluginRow::for_session(std::slice::from_ref(&game), &enabled, &[], "a.example")[1];
        assert!(row.switched_on(&[], &[]));
        let off = PluginDisabled {
            plugin: id.clone(),
            reason: DisabledReason::Overran(3),
        };
        assert!(!row.switched_on(&[], &[off]));

        let cost = |per_line| PluginCost {
            plugin: id.clone(),
            sampled_lines: 1,
            per_line,
        };
        assert_eq!(row.cost(&[cost(None)]), None);
        let summary = Summary::of(&[Duration::from_micros(2)]);
        assert_eq!(row.cost(&[cost(summary)]), Some(2.));
    }

    #[test]
    fn an_override_the_game_s_name_brought_on_shows_on_until_it_is_flipped() {
        let mine = plugin(AN_OVERRIDE);
        let rows = PluginRow::for_session(std::slice::from_ref(&mine), &[], &[], "b.example");
        let mut row = rows[1].clone();
        let in_the_host = [PluginCost {
            plugin: mine.id().clone(),
            sampled_lines: 1,
            per_line: Summary::of(&[Duration::from_micros(2)]),
        }];
        assert!(!row.on, "its host doesn't match");
        assert!(row.switched_on(&in_the_host, &[]));

        let whole = Summary::of(&[Duration::from_micros(2)]).unwrap();
        let (layers, _) = cost_lines(&[row.clone()], &in_the_host, &[], &whole);
        assert!(layers.contains("Overrides 2.0 µs"), "{layers}");

        row.flips = 1;
        assert!(
            !row.switched_on(&in_the_host, &[]),
            "the player's word wins"
        );
    }

    #[test]
    fn an_override_for_another_server_says_where_it_goes() {
        let mine = plugin(AN_OVERRIDE);
        let id = mine.id().clone();
        let row = |host, off: &[PluginId]| {
            PluginRow::for_session(std::slice::from_ref(&mine), &[], off, host).remove(1)
        };
        let there = row("b.example", &[]);
        assert_eq!(there.meant_elsewhere(&[]).unwrap(), "a.example");
        assert_eq!(row("a.example", &[]).meant_elsewhere(&[]), None);
        let turned_off_here = row("b.example", std::slice::from_ref(&id));
        assert_eq!(turned_off_here.meant_elsewhere(&[]), None);

        let running = PluginCost {
            plugin: id,
            sampled_lines: 1,
            per_line: None,
        };
        assert_eq!(there.meant_elsewhere(&[running]), None, "it runs here");
    }

    #[test]
    fn rule_counts_read_as_words() {
        assert_eq!(rule_counts(2, 1), "2 triggers, 1 alias");
        assert_eq!(rule_counts(1, 0), "1 trigger, 0 aliases");
    }

    #[test]
    fn layers_count_what_runs_and_the_total_is_the_whole_read() {
        let game = plugin(A_GAME);
        let id = game.id().clone();
        let rows = PluginRow::for_session(
            std::slice::from_ref(&game),
            std::slice::from_ref(&id),
            &[],
            "a.example",
        );
        let of = |plugin: &PluginId, micros| PluginCost {
            plugin: plugin.clone(),
            sampled_lines: 1,
            per_line: Summary::of(&[Duration::from_micros(micros)]),
        };
        let costs = [of(&rows[0].id, 1), of(&id, 9)];
        let whole = Summary::of(&[Duration::from_micros(7)]).unwrap();

        let (layers, total) = cost_lines(&rows, &costs, &[], &whole);
        assert_eq!(layers, "Overrides 0.0 µs · Game 9.0 µs · Defaults 1.0 µs");
        assert_eq!(
            total,
            "All plugins 7.0 µs of 20 µs a line. One plugin may add 5 µs."
        );

        let off = PluginDisabled {
            plugin: id.clone(),
            reason: DisabledReason::Overran(3),
        };
        let (layers, _) = cost_lines(&rows, &costs, &[off], &whole);
        assert!(layers.contains("Game 0.0 µs"), "{layers}");
    }
}
