//! Renders the real window to PNG files with gpui's headless renderer, so the
//! layout can be looked at without a display. It needs a GPU, so it's macOS
//! only for now and ignored by default:
//!
//! ```text
//! cargo test -p mudling-desktop window_images -- --ignored
//! ```
//!
//! The images land in `target/window-images`, or in `$MUDLING_WINDOW_IMAGES`.
//! The chat text is made up for this file: it comes from no real session.

use crate::assets::{Assets, load_fonts};
use crate::dock::{DockState, Side, Slot};
use crate::hud::HudPlacement;
use crate::map_view::harness::{duplicated_map, sample_map};
use crate::measure;
use crate::settings::{
    Downloaded, Listing, PluginRow, SavedServer as SheetServer, Section, Sending,
};
use crate::shell::Shell;
use crate::theme::Theme;
use crate::title_bar::SessionLabel;
use futures::channel::mpsc::unbounded;
use gpui::{
    AppContext, Context, HeadlessAppContext, Modifiers, MouseMoveEvent, Pixels, PlatformInput,
    Point, Size, Window, WindowAppearance, point, px, size,
};
use mudling_core::LinePipeline;
use mudling_core::defaults::aliases::AliasRule;
use mudling_core::defaults::detect::{Source, Suggestion};
use mudling_core::defaults::hud::Vital;
use mudling_core::defaults::triggers::{Highlight, Scope, TriggerRule};
use mudling_core::gear::{ItemRef, Reading};
use mudling_core::git::FolderFiles;
use mudling_core::logs::LogSettings;
use mudling_core::map::FindFilter;
use mudling_core::net::DisconnectReason;
use mudling_core::persist::{Batch, SavedCharacter, SavedServer, SavedServers, StatMap, Timestamp};
use mudling_core::plugin::luau::{DisabledReason, PluginDisabled};
use mudling_core::plugin::{Layer, Plugin, PluginCost, PluginId};
use mudling_core::registry::{self, Fetched, IndexError};
use mudling_core::stats::Summary;
use mudling_core::store::{
    ConnectionStatus, Diff, ItemsState, Revision, RulesState, SettingsState, TerminalChange,
    TerminalState, VitalsState, WornItem,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

const WIDE: Size<Pixels> = size(px(1434.), px(897.));
const DEFAULT: Size<Pixels> = size(px(1300.), px(800.));
const NARROW: Size<Pixels> = size(px(760.), px(640.));
const UNDER_80_COLUMNS: Size<Pixels> = size(px(560.), px(640.));

const SAMPLE_CHAT: [&str; 11] = [
    "\x1b[36mYou are carrying:\x1b[0m",
    "",
    "     \x1b[33m(Glowing) (Humming)\x1b[0m a copper bracelet",
    "     \x1b[33m(Glowing)\x1b[0m a small necklace",
    "     a small boat",
    "     \x1b[33m(Glowing)\x1b[0m an alligator tooth",
    "     \x1b[33m(Glowing)\x1b[0m orb of knowledge",
    "( 5) a bright ball of light",
    "",
    "\x1b[32mYou get 20 standard bolts.\x1b[0m",
    "\x1b[32mYou donate 50 standard bolts.\x1b[0m",
];

/// A ruler of exactly 80 columns, a table and a map that fit them, and prose
/// and a word too long for the column.
const WRAPPING_CHAT: [&str; 10] = [
    "12345678901234567890123456789012345678901234567890123456789012345678901234567890",
    "\x1b[33mThe goblin swings a rusty sword at you and misses, stumbling past the \
     torch and down the quiet stone path, then over the old bridge and away towards \
     the river, where it trips on a root and drops its lantern.\x1b[0m",
    "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa!",
    "| Name   | Lvl | Class  |",
    "| Mira   |  12 | Mage   |",
    "| Oskar  |   7 | Ranger |",
    "    N",
    "  W-+-E    [ ]--[ ]",
    "    S       |",
    "           [*]",
];

fn sample_plugin(id: &str, name: &str, layer: Layer, on: bool) -> PluginRow {
    PluginRow {
        id: PluginId::try_from(id.to_owned()).expect("the id is valid"),
        name: name.to_owned().into(),
        layer,
        detail: "1.0.0 · MIT".into(),
        holds: "".into(),
        always_on: false,
        on,
        flips: 0,
        elsewhere: None,
        sending: Sending::No,
        asks: Vec::new(),
        folder: None,
        added_from: None,
        source: None,
        problem: None,
        held_back: None,
    }
}

/// A made-up plugin that sends commands, approved or waiting.
fn sample_sender(id: &str, name: &str, sending: Sending) -> PluginRow {
    PluginRow {
        sending,
        asks: measure::sample_asks(),
        folder: Some(id.to_owned().into()),
        ..sample_plugin(id, name, Layer::Game, true)
    }
}

/// A made-up plugin that sends commands, as if just downloaded and checked.
fn sample_fetched() -> Fetched {
    let files: FolderFiles = [
        (
            "plugin.toml",
            "id = \"avatar-extras\"\nname = \"Avatar extras\"\nversion = \"0.4.1\"\napi = 1\n\
             license = \"Apache-2.0\"\nlayer = \"game\"\n\
             permissions = [\"send_commands\", \"read_state\", \"write_store:extras\"]\n\
             [servers]\nmssp_names = [\"Avatar\"]\n\
             [points]\ntext = \"text.toml\"\n[hooks]\nfiles = [\"hooks.luau\"]\n",
        ),
        (
            "text.toml",
            "[[pattern]]\nname = \"tell\"\npattern = '''^\\w+ tells you'''\n",
        ),
        ("hooks.luau", "function on_tell() end\n"),
    ]
    .map(|(name, text)| (name.to_owned(), text.as_bytes().to_vec()))
    .into();
    let plugin = Plugin::from_files(|name| {
        let bytes = files
            .get(name)
            .ok_or_else(|| std::io::Error::other(name.to_owned()))?;
        Ok(String::from_utf8(bytes.clone()).expect("the sample is text"))
    })
    .expect("the sample plugin loads");
    let (Listing::Listed { index, .. }, _) = measure::sample_directory() else {
        unreachable!("the sample directory is listed");
    };
    let entry = index.plugins[1].clone();
    Fetched {
        hash: entry.hash.clone(),
        source: registry::Source::Directory(entry),
        commit: index.commit,
        files,
        plugin,
    }
}

fn sample_cost(id: &str, nanos: u64) -> PluginCost {
    PluginCost {
        plugin: PluginId::try_from(id.to_owned()).expect("the id is valid"),
        sampled_lines: 1,
        per_line: Summary::of(&[Duration::from_nanos(nanos)]),
    }
}

/// Opens the sheet on `section`, with made-up servers, plugins and a plugin
/// turned off for overrunning.
fn open_sheet(shell: &mut Shell, section: Section, window: &mut Window, cx: &mut Context<Shell>) {
    let settings = SettingsState {
        server: Some("mud.example".to_owned()),
        plugin_costs: vec![
            sample_cost("sample-tweaks", 300),
            sample_cost("sample-game", 2_800),
            sample_cost("sample-helper", 9_400),
            sample_cost("sample-defaults", 1_200),
        ],
        plugin_line_cost: Summary::of(&[Duration::from_nanos(4_100)]),
        turned_off: vec![PluginDisabled {
            plugin: PluginId::try_from("sample-helper".to_owned()).expect("a good id"),
            reason: DisabledReason::Overran(3),
        }],
        ..SettingsState::default()
    };
    shell.show_sample_problems(&settings, cx);
    shell.settings_sheet().update(cx, |sheet, cx| {
        sheet.show_plugins(
            vec![
                sample_plugin("sample-tweaks", "Sample's tweaks", Layer::Override, true),
                sample_plugin("sample-game", "Sample game", Layer::Game, true),
                sample_plugin("sample-helper", "Sample helper", Layer::Game, true),
                sample_sender("sample-extras", "Sample extras", Sending::Approved),
                sample_sender("sample-chat", "Sample chat", Sending::Waiting),
                PluginRow {
                    folder: Some("sample-notes".into()),
                    held_back: Some("sha256:sample-notes-from-elsewhere".into()),
                    ..sample_plugin("sample-notes", "Sample notes", Layer::Game, false)
                },
                sample_plugin("sample-defaults", "MUDling defaults", Layer::Default, true),
            ],
            cx,
        );
        sheet.show_servers(
            vec![SheetServer {
                name: "Sample game".into(),
                host: "mud.example".into(),
                port: 4000,
                characters: vec!["Sample".into()],
            }],
            None,
            cx,
        );
        sheet.show_logs(sample_log_settings(), measure::sample_logs(), cx);
    });
    shell.open_settings(window, cx);
    shell
        .settings_sheet()
        .update(cx, |sheet, cx| sheet.select(section, cx));
}

fn sample_log_settings() -> LogSettings {
    let mut settings = LogSettings {
        debug: true,
        ..LogSettings::default()
    };
    settings.set_chat("mud.example", true);
    settings
}

/// The map the join scenes tidy up.
const SAMPLE_SIDE: i64 = 12;
const SAMPLE_REPEAT: i64 = 9;
const SAMPLE_ROOM: i64 = (1 << 40) + 13;

/// What arrives while the split is open.
const LOOT: [&str; 2] = [
    "\x1b[32mYou get a copper bracelet.\x1b[0m",
    "\x1b[36mA bright spirit of friendship floats in.\x1b[0m",
];
const NEW_LINES: usize = 14;

fn images_folder() -> PathBuf {
    std::env::var_os("MUDLING_WINDOW_IMAGES").map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/window-images"),
        PathBuf::from,
    )
}

fn server_lines(texts: &[&str]) -> Diff<TerminalState> {
    let mut pipeline = LinePipeline::default();
    let mut lines = Vec::new();
    for line in texts {
        pipeline.push(format!("{line}\n").as_bytes(), &mut lines);
    }
    Diff {
        revision: Revision::default(),
        change: TerminalChange::Append(lines),
    }
}

/// The canvas's card, with a made-up server.
fn sample_saved() -> SavedServers {
    let last_played = Timestamp::now();
    SavedServers {
        servers: vec![SavedServer {
            host: "mud.example".to_owned(),
            port: 4000,
            updated: last_played,
            last_room: Some("the Donation Room".to_owned()),
            characters: vec![SavedCharacter {
                name: Some("Altissima".to_owned()),
                level: Some(2),
                last_played,
            }],
        }],
        problems: Vec::new(),
    }
}

fn fill_with_sample(shell: &mut Shell, cx: &mut Context<Shell>) {
    shell.chat().update(cx, |chat, cx| {
        chat.apply(&server_lines(&SAMPLE_CHAT), cx);
        chat.echo_unsent("get bracelet", cx);
    });
    let vitals = VitalsState(BTreeMap::from([
        (Vital::Hp, 30),
        (Vital::MaxHp, 42),
        (Vital::Mana, 110),
        (Vital::MaxMana, 115),
        (Vital::Moves, 110),
        (Vital::MaxMoves, 110),
    ]));
    shell.show_sample_vitals(&vitals, cx);
    shell.show_sample_session(
        SessionLabel::new("mud.example", Some("Sample".into()), "Connected · 38 ms"),
        cx,
    );
}

/// Rules made up for this file.
fn sample_rules() -> RulesState {
    let trigger = |pattern: &str, color, hides, sound: Option<&str>, scope| TriggerRule {
        color,
        hides,
        sound: sound.map(str::to_owned),
        scope,
        ..TriggerRule::new(pattern)
    };
    let character = || Scope::Character("sample".to_owned());
    RulesState {
        server: "mud.example".to_owned(),
        character: "sample".to_owned(),
        separator: None,
        notepad: String::new(),
        triggers: vec![
            trigger(
                "tells you",
                Some(Highlight::Purple),
                false,
                Some("chime"),
                Scope::All,
            ),
            trigger("flaps its wings", None, true, None, Scope::Server),
            trigger(
                "is DEAD!!",
                Some(Highlight::Red),
                false,
                Some("bell"),
                character(),
            ),
        ],
        aliases: vec![
            AliasRule::new("k", "kill $1;kick $1"),
            AliasRule {
                enabled: false,
                ..AliasRule::new("gg", "get $2 $1;wear $2")
            },
        ],
        hotbar: crate::measure::sample_chips(),
    }
}

fn open_gear_tab(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::fill_gear(shell, cx);
    shell
        .dock(Side::Right)
        .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
}

fn open_gear_worn(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_gear_tab(shell, window, cx);
    shell
        .gear_panel()
        .update(cx, |panel, cx| panel.show_worn(cx));
}

/// The mockup's Worn view: 14 places to wear something across 12 slots, 9 of
/// them filled, with the sample's sets. Made up, as all of this file's are.
fn full_worn() -> ItemsState {
    let mut items = crate::measure::sample_items_with_sets();
    let mut book = (*items.book).clone();
    let mut batch = Batch::new();
    let stats = |pairs: &[(&str, i64)]| -> StatMap {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
    };
    let reading = |word: &str, pairs: &[(&str, i64)]| Reading {
        keywords: word.to_owned(),
        item_type: "armor".to_owned(),
        base: stats(pairs),
        ..Reading::default()
    };
    let worn = [
        (
            "finger",
            "a pink ice ring",
            &[("int", 2), ("mana_max", 15)][..],
        ),
        ("finger", "a pink ice ring", &[("int", 2), ("mana_max", 15)]),
        (
            "neck",
            "a mushroom-shaped amulet",
            &[("con", 1), ("ac", -4)],
        ),
        (
            "neck",
            "the mantle of Bonk",
            &[("str", 1), ("damroll", 3), ("ac", -2)],
        ),
        (
            "body",
            "(Glowing) a glowing suit of bronze plate",
            &[("hitroll", 2), ("ac", -12)],
        ),
        ("head", "a Tiamat mask", &[("wis", 2), ("damroll", 4)]),
        ("arms", "a tulip tattoo", &[]),
        (
            "offhand",
            "(Glowing) a Golden Unicorn horn",
            &[("dex", 2), ("hitroll", 7), ("damroll", 7)],
        ),
        ("about", "a linen robe", &[("ac", -3)]),
    ];
    for (_, name, effects) in worn.iter().filter(|(_, _, effects)| !effects.is_empty()) {
        let item = ItemRef { name, copy: None };
        book.learn(reading(name, effects), item, Timestamp(0), &mut batch);
    }
    // A second mask, read stronger: the mask's damage is a range.
    let mask = ItemRef {
        name: "a Tiamat mask",
        copy: Some("31"),
    };
    let stronger = reading("mask", &[("wis", 2), ("damroll", 6)]);
    book.learn(stronger, mask, Timestamp(0), &mut batch);
    items.book = Arc::new(book);
    items.worn = (worn.iter())
        .map(|(slot, name, _)| WornItem {
            slot: (*slot).to_owned(),
            item: (*name).to_owned(),
            copy: None,
        })
        .collect();
    let slots = [
        "light", "finger", "neck", "body", "head", "legs", "feet", "hands", "arms", "offhand",
        "about", "waist",
    ];
    items.slots = slots.map(str::to_owned).to_vec().into();
    items
}

fn open_full_worn(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_gear_sets(shell, window, cx);
    shell.gear_panel().update(cx, |panel, cx| {
        panel.show_items(full_worn(), cx);
        panel.show_worn(cx);
    });
}

fn open_gear_sets(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::fill_gear_sets(shell, cx);
    shell
        .dock(Side::Right)
        .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
}

fn edit_gear_set(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_gear_sets(shell, window, cx);
    shell
        .gear_panel()
        .update(cx, |panel, cx| panel.edit_first_set(window, cx));
}

/// The sample's chat, with a line naming an item for the pointer to rest on.
fn name_an_item(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_gear_tab(shell, window, cx);
    let line = [crate::measure::SAMPLE_ITEM_LINE];
    shell
        .chat()
        .update(cx, |chat, cx| chat.apply(&server_lines(&line), cx));
}

fn open_room_panel(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::fill_room_panel(shell, None, cx);
    shell
        .dock(Side::Right)
        .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
}

/// The Find panel on its sample, on `filter` if it's named.
fn open_find_panel(
    shell: &mut Shell,
    filter: Option<FindFilter>,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    shell.find_panel().update(cx, |panel, cx| {
        panel.start_over(crate::measure::sample_find(), cx);
        panel.choose(filter.unwrap_or_default(), cx);
    });
    shell.focus_find_panel(window, cx);
}

/// The map with its marks and the selected room's card, and the Room panel
/// with its own card.
fn open_map_marks(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    shell.show_sample_map(&sample_map(20, false), 24, cx);
    crate::measure::fill_room_panel(shell, None, cx);
    shell.open_panels(cx);
}

/// The map with rooms that look like duplicates.
fn open_duplicates(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    shell.show_sample_map(&duplicated_map(SAMPLE_SIDE, SAMPLE_REPEAT), SAMPLE_ROOM, cx);
    shell.open_panels(cx);
    shell
        .map()
        .update(cx, |map, cx| map.find_duplicates_now(cx));
}

fn open_review(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_duplicates(shell, window, cx);
    shell
        .map()
        .update(cx, |map, cx| map.review_sample(window, cx));
}

fn open_picked(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    open_duplicates(shell, window, cx);
    shell.map().update(cx, |map, cx| map.pick_sample(cx));
}

/// The Notes tab, as the canvas draws it.
fn open_notes_tab(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::open_notes(shell, window, cx);
    shell.notes().update(cx, |notes, cx| notes.finish(cx));
}

fn edit_a_note(shell: &mut Shell, window: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::open_notes(shell, window, cx);
}

/// Reads back to the top of the sample, then 14 lines arrive.
fn split_sample(shell: &mut Shell, cx: &mut Context<Shell>) {
    shell.chat().update(cx, |chat, cx| {
        chat.open_history_at(0, cx);
        for _ in 0..NEW_LINES / LOOT.len() {
            chat.apply(&server_lines(&LOOT), cx);
        }
    });
}

/// The HUD's sample at `placement`, over a fight whose prompts the game sent.
fn sample_fight(
    placement: HudPlacement,
    open: bool,
    shell: &mut Shell,
    window: &mut Window,
    cx: &mut Context<Shell>,
) {
    const FIGHT: [&str; 4] = [
        "<3120hp 1040m 810mv>",
        "You slash the goblin. It winces.",
        "The goblin scratches you.",
        "<3098hp 1040m 810mv>",
    ];
    let Diff { change, revision } = server_lines(&FIGHT);
    let TerminalChange::Append(lines) = change;
    let lines = lines.into_iter().map(|line| {
        if line.plain().starts_with('<') {
            line.with_prompt(line.plain().len())
        } else {
            line
        }
    });
    let change = TerminalChange::Append(lines.collect());
    let fight = Diff { change, revision };
    shell.chat().update(cx, |chat, cx| chat.apply(&fight, cx));
    let (vitals, characters, target) = crate::measure::sample_hud(0);
    shell.show_sample_hud(&vitals, &characters, &target, cx);
    shell.place_hud(placement, open, window, cx);
}

/// Opens the window at `window_size`, lets `arrange` set it up, and saves what
/// it draws as `<name>.png`.
fn render(
    name: &str,
    appearance: WindowAppearance,
    window_size: Size<Pixels>,
    arrange: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) {
    render_hovering(name, appearance, window_size, None, arrange);
}

/// [`render`], with the pointer resting on `hover` for as long as a tooltip
/// takes to come up.
fn render_hovering(
    name: &str,
    appearance: WindowAppearance,
    window_size: Size<Pixels>,
    hover: Option<Point<Pixels>>,
    arrange: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        load_fonts(cx);
        crate::bind_keys(cx);
        cx.set_reduce_motion(true);
    });
    let (_waker, woken) = unbounded();
    let window = cx
        .open_window(window_size, |window, cx| {
            cx.new(|cx| Shell::new(None, woken, window, cx))
        })
        .expect("the window opens");
    window
        .update(&mut cx, |shell, window, cx| {
            cx.set_global(Theme::for_appearance(appearance));
            fill_with_sample(shell, cx);
            arrange(shell, window, cx);
            // A choice made in `arrange` wears the headless window's own ground.
            let choice = cx.global::<Theme>().choice;
            cx.set_global(Theme::chosen(choice, appearance));
            window.refresh();
        })
        .expect("the window is open");
    cx.run_until_parked();
    if let Some(position) = hover {
        let _ = cx.capture_screenshot(window.into());
        window
            .update(&mut cx, |_, window, cx| {
                let moved = MouseMoveEvent {
                    position,
                    pressed_button: None,
                    modifiers: Modifiers::none(),
                };
                window.dispatch_event(PlatformInput::MouseMove(moved), cx);
            })
            .expect("the window is open");
        cx.advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
    }

    let image = cx
        .capture_screenshot(window.into())
        .expect("the frame renders");
    let folder = images_folder();
    std::fs::create_dir_all(&folder).expect("the folder can be made");
    let path = folder.join(format!("{name}.png"));
    image.save(&path).expect("the image can be written");
    println!("{}", path.display());
}

#[test]
#[ignore = "needs a GPU; writes image files"]
fn window_images() {
    use WindowAppearance::{Dark, Light};
    render("main-dark", Dark, WIDE, |_, _, _| {});
    render("main-light", Light, WIDE, |_, _, _| {});
    render("panels-open-dark", Dark, WIDE, |shell, _, cx| {
        shell.open_panels(cx);
    });
    render("panels-open-light", Light, WIDE, |shell, _, cx| {
        shell.open_panels(cx);
    });
    for (name, appearance) in [("map-history-dark", Dark), ("map-history-light", Light)] {
        render(name, appearance, WIDE, |shell, _, cx| {
            shell.open_panels(cx);
            shell
                .map_history()
                .update(cx, |history, cx| history.show_sample(cx));
        });
    }
    render(
        "map-history-asking-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            shell.open_panels(cx);
            shell.map_history().update(cx, |history, cx| {
                history.show_sample(cx);
                history.ask_about_sample(window, cx);
            });
        },
    );
    render(
        "panels-open-default-size-dark",
        Dark,
        DEFAULT,
        |shell, _, cx| {
            shell.open_panels(cx);
        },
    );
    render("wrapped-dark", Dark, DEFAULT, |shell, _, cx| {
        shell.chat().update(cx, |chat, cx| {
            chat.apply(&server_lines(&WRAPPING_CHAT), cx);
            chat.echo_unsent(
                "say a command long enough to wrap beside the marker that starts it, \
                 because a player can always type more than a server would ever send",
                cx,
            );
        });
        shell.open_panels(cx);
    });
    render(
        "wrapped-narrow-dark",
        Dark,
        UNDER_80_COLUMNS,
        |shell, _, cx| {
            shell
                .chat()
                .update(cx, |chat, cx| chat.apply(&server_lines(&WRAPPING_CHAT), cx));
        },
    );
    render("split-dark", Dark, WIDE, |shell, _, cx| {
        split_sample(shell, cx)
    });
    render("split-light", Light, WIDE, |shell, _, cx| {
        split_sample(shell, cx)
    });
    render("split-panels-open-dark", Dark, DEFAULT, |shell, _, cx| {
        split_sample(shell, cx);
        shell.open_panels(cx);
    });
    render("split-wrapped-dark", Dark, DEFAULT, |shell, _, cx| {
        shell
            .chat()
            .update(cx, |chat, cx| chat.apply(&server_lines(&WRAPPING_CHAT), cx));
        split_sample(shell, cx);
    });
    render("rails-dark", Dark, WIDE, |shell, _, cx| {
        for side in [Side::Left, Side::Right] {
            shell
                .dock(side)
                .update(cx, |dock, cx| dock.move_to(DockState::Rail, cx));
        }
    });
    render("narrow-overlay-dark", Dark, NARROW, |shell, _, cx| {
        shell
            .dock(Side::Right)
            .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
    });
    render("multi-line-dark", Dark, WIDE, |shell, window, cx| {
        shell.type_sample(
            "look\nget all corpse\na line long enough to wrap around the end of the field, \
             because players paste long aliases and expect to see all of what they pasted",
            window,
            cx,
        );
    });
    render("speedwalk-dark", Dark, WIDE, |shell, window, cx| {
        shell.type_sample("3n2e", window, cx);
    });
    render("hotbar-dark", Dark, WIDE, |shell, _, cx| {
        crate::measure::fill_hotbar(shell, cx);
    });
    render("hotbar-light", Light, WIDE, |shell, _, cx| {
        crate::measure::fill_hotbar(shell, cx);
    });
    render(
        "hotbar-alias-preview-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            crate::measure::fill_hotbar(shell, cx);
            shell.type_sample("k spirit", window, cx);
            shell.show_sample_alias_preview("kill spirit; kick spirit", cx);
        },
    );
    render("hotbar-narrow-dark", Dark, NARROW, |shell, _, cx| {
        crate::measure::fill_hotbar(shell, cx);
    });
    for (name, appearance) in [
        ("settings-triggers-dark", Dark),
        ("settings-triggers-light", Light),
    ] {
        render(name, appearance, WIDE, |shell, window, cx| {
            shell.show_sample_rules(sample_rules(), cx);
            open_sheet(shell, Section::Triggers, window, cx);
        });
    }
    render(
        "settings-triggers-form-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            shell.show_sample_rules(sample_rules(), cx);
            open_sheet(shell, Section::Triggers, window, cx);
            shell.settings_sheet().update(cx, |sheet, cx| {
                sheet.edit_sample_trigger(window, cx);
            });
        },
    );
    render(
        "settings-triggers-no-game-dark",
        Dark,
        DEFAULT,
        |shell, window, cx| {
            open_sheet(shell, Section::Triggers, window, cx);
        },
    );
    render("title-bar-server-only-dark", Dark, WIDE, |shell, _, cx| {
        shell.show_sample_session(SessionLabel::new("mud.example", None, "Connected"), cx);
    });
    render("title-bar-narrow-dark", Dark, NARROW, |_, _, _| {});
    render("title-bar-menu-dark", Dark, WIDE, |shell, window, cx| {
        shell.open_session_menu(window, cx);
    });
    render("title-bar-menu-light", Light, WIDE, |shell, window, cx| {
        shell.open_session_menu(window, cx);
    });
    render("room-panel-dark", Dark, WIDE, open_room_panel);
    render("room-panel-light", Light, WIDE, open_room_panel);
    for (name, tab) in [("character", Slot::Character), ("skills", Slot::Skills)] {
        for (mode, appearance) in [("dark", Dark), ("light", Light)] {
            render(
                &format!("{name}-tab-{mode}"),
                appearance,
                WIDE,
                move |shell, _, cx| {
                    crate::measure::open_sheet_tab(shell, tab, cx);
                },
            );
        }
    }
    render("find-panel-dark", Dark, WIDE, |shell, window, cx| {
        open_find_panel(shell, None, window, cx);
    });
    render("find-panel-light", Light, WIDE, |shell, window, cx| {
        open_find_panel(shell, None, window, cx);
    });
    render("find-panel-items-dark", Dark, WIDE, |shell, window, cx| {
        open_find_panel(shell, Some(FindFilter::Items), window, cx);
    });
    render(
        "find-panel-searching-dark",
        Dark,
        DEFAULT,
        |shell, window, cx| {
            open_find_panel(shell, Some(FindFilter::Rooms), window, cx);
            shell
                .find_panel()
                .update(cx, |panel, cx| panel.type_text("no such room", window, cx));
        },
    );
    render(
        "find-panel-new-map-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            shell.focus_find_panel(window, cx);
        },
    );
    render("gear-tab-dark", Dark, WIDE, open_gear_tab);
    render("gear-tab-light", Light, WIDE, open_gear_tab);
    render("gear-tab-worn-dark", Dark, WIDE, open_gear_worn);
    render("gear-sets-dark", Dark, WIDE, open_gear_sets);
    render("gear-sets-light", Light, WIDE, open_gear_sets);
    render("gear-worn-full-dark", Dark, WIDE, open_full_worn);
    render("gear-worn-full-light", Light, WIDE, open_full_worn);
    render("gear-set-editor-dark", Dark, WIDE, edit_gear_set);
    render("gear-set-editor-light", Light, WIDE, edit_gear_set);
    render_hovering(
        "gear-tooltip-dark",
        Dark,
        WIDE,
        Some(point(px(320.), px(310.))),
        name_an_item,
    );
    render("notes-tab-dark", Dark, WIDE, open_notes_tab);
    render("notes-tab-light", Light, WIDE, open_notes_tab);
    render("note-editing-dark", Dark, WIDE, edit_a_note);
    render("map-marks-dark", Dark, WIDE, open_map_marks);
    render("map-marks-light", Light, WIDE, open_map_marks);
    render("map-duplicates-dark", Dark, WIDE, open_duplicates);
    render("map-duplicates-light", Light, WIDE, open_duplicates);
    render("map-review-dark", Dark, WIDE, open_review);
    render("map-review-light", Light, WIDE, open_review);
    render("map-picked-dark", Dark, WIDE, open_picked);
    render("map-picked-light", Light, WIDE, open_picked);
    render("empty-state-dark", Dark, WIDE, |shell, window, cx| {
        shell.show_sample_empty_state(&sample_saved(), window, cx);
    });
    render("empty-state-light", Light, WIDE, |shell, window, cx| {
        shell.show_sample_empty_state(&sample_saved(), window, cx);
    });
    render(
        "empty-state-first-run-dark",
        Dark,
        DEFAULT,
        |shell, window, cx| {
            shell.show_sample_empty_state(&SavedServers::default(), window, cx);
        },
    );
    render(
        "empty-state-narrow-dark",
        Dark,
        NARROW,
        |shell, window, cx| {
            shell.show_sample_empty_state(&sample_saved(), window, cx);
        },
    );
    render("empty-state-menu-dark", Dark, WIDE, |shell, window, cx| {
        shell.show_sample_empty_state(&sample_saved(), window, cx);
        shell.open_session_menu(window, cx);
    });
    render("connection-ended-dark", Dark, WIDE, |shell, _, cx| {
        let failed = "connection refused".to_owned();
        let settings = SettingsState {
            connection: ConnectionStatus::Closed(DisconnectReason::Failed(failed)),
            suggestion: Some(Suggestion {
                plugin: PluginId::try_from("sample".to_owned()).expect("a valid id"),
                source: Source::Host,
            }),
            ..SettingsState::default()
        };
        shell.show_sample_offers(&settings, cx);
    });
    for (name, appearance) in [
        ("settings-inspector-dark", Dark),
        ("settings-inspector-light", Light),
    ] {
        render(name, appearance, WIDE, |shell, window, cx| {
            open_sheet(shell, Section::Plugins, window, cx);
            measure::open_sample_inspector(shell, &measure::sample_inspected(), window, cx);
        });
    }
    for (name, appearance) in [
        ("settings-directory-dark", Dark),
        ("settings-directory-light", Light),
    ] {
        render(name, appearance, WIDE, measure::open_sample_directory);
    }
    render(
        "settings-directory-refused-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            measure::open_sample_directory(shell, window, cx);
            shell.settings_sheet().update(cx, |sheet, cx| {
                let why = IndexError::Forged.to_string().into();
                sheet.show_directory(Listing::Refused(why), BTreeMap::new(), cx);
            });
        },
    );
    for (name, appearance) in [
        ("settings-add-plugin-dark", Dark),
        ("settings-add-plugin-light", Light),
    ] {
        render(name, appearance, WIDE, measure::open_sample_add_plugin);
    }
    render(
        "settings-add-plugin-forget-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            measure::open_sample_section(shell, Section::Plugins, window, cx);
            let colors = PluginId::try_from("sample-colors".to_owned()).expect("a valid id");
            shell.settings_sheet().update(cx, |sheet, cx| {
                sheet.confirm_remove(&colors, window, cx);
            });
        },
    );
    for (name, appearance) in [
        ("settings-approval-dark", Dark),
        ("settings-approval-light", Light),
    ] {
        render(name, appearance, WIDE, measure::open_sample_approval);
    }
    render(
        "settings-approval-install-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            measure::open_sample_directory(shell, window, cx);
            let downloaded = Downloaded {
                fetched: Arc::new(sample_fetched()),
                keeps_approval: false,
                replaces: None,
            };
            shell.settings_sheet().update(cx, |sheet, cx| {
                sheet.fetched(Ok(downloaded), window, cx);
            });
        },
    );
    render("settings-trust-dark", Dark, WIDE, |shell, window, cx| {
        measure::open_sample_section(shell, Section::Plugins, window, cx);
        shell.settings_sheet().update(cx, |sheet, cx| {
            let why = "It arrived from another device in a copy this device didn't \
                       install. The directory doesn't list this copy.";
            let shown = "sha256:sample-notes-from-elsewhere";
            sheet.ask_to_trust("sample-notes", shown, why.to_owned(), window, cx);
        });
    });
    render("settings-remove-dark", Dark, WIDE, |shell, window, cx| {
        measure::open_sample_section(shell, Section::Plugins, window, cx);
        let helper = PluginId::try_from("sample-helper".to_owned()).expect("a valid id");
        shell.settings_sheet().update(cx, |sheet, cx| {
            sheet.confirm_remove(&helper, window, cx);
        });
    });
    for (name, appearance) in [
        ("settings-server-plugins-dark", Dark),
        ("settings-server-plugins-light", Light),
    ] {
        render(name, appearance, WIDE, |shell, window, cx| {
            open_sheet(shell, Section::Servers, window, cx);
            measure::open_sample_server_plugins(shell, window, cx);
        });
    }
    for (name, appearance, section) in [
        ("settings-plugins-dark", Dark, Section::Plugins),
        ("settings-plugins-light", Light, Section::Plugins),
        ("settings-appearance-dark", Dark, Section::Appearance),
        ("settings-shortcuts-dark", Dark, Section::Shortcuts),
        ("settings-servers-dark", Dark, Section::Servers),
        ("settings-vault-dark", Dark, Section::Vault),
        ("settings-logs-dark", Dark, Section::Logs),
        ("settings-logs-light", Light, Section::Logs),
    ] {
        render(name, appearance, WIDE, move |shell, window, cx| {
            open_sheet(shell, section, window, cx);
        });
    }
    for (name, appearance, section) in [
        ("settings-data-dark", Dark, Section::Data),
        ("settings-data-light", Light, Section::Data),
        ("settings-vault-filled-dark", Dark, Section::Vault),
        ("settings-vault-filled-light", Light, Section::Vault),
    ] {
        render(name, appearance, WIDE, move |shell, window, cx| {
            measure::open_sample_section(shell, section, window, cx);
        });
    }
    render(
        "settings-vault-form-dark",
        Dark,
        WIDE,
        |shell, window, cx| {
            measure::open_sample_section(shell, Section::Vault, window, cx);
            shell.settings_sheet().update(cx, |sheet, cx| {
                sheet.type_sample_password("not a real one", window, cx);
            });
        },
    );
    render("settings-narrow-dark", Dark, NARROW, |shell, window, cx| {
        open_sheet(shell, Section::Plugins, window, cx);
    });
    for (place, placement, open) in [
        ("attached", HudPlacement::Attached, false),
        ("attached-open", HudPlacement::Attached, true),
        ("floating", HudPlacement::Floating, false),
        ("top-strip", HudPlacement::TopStrip, false),
        ("hidden", HudPlacement::Hidden, false),
    ] {
        for (ground, appearance) in [("dark", Dark), ("light", Light)] {
            render(
                &format!("hud-{place}-{ground}"),
                appearance,
                DEFAULT,
                move |shell, window, cx| sample_fight(placement, open, shell, window, cx),
            );
        }
    }
}
