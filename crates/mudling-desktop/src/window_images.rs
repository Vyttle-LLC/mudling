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
use crate::dock::{DockState, Side};
use crate::shell::Shell;
use crate::theme::Theme;
use crate::title_bar::{self, SessionLabel};
use futures::channel::mpsc::unbounded;
use gpui::{
    AppContext, Context, HeadlessAppContext, Pixels, Size, Window, WindowAppearance, px, size,
};
use mudling_core::LinePipeline;
use mudling_core::defaults::hud::Vital;
use mudling_core::store::{Diff, Revision, TerminalChange, TerminalState, VitalsState};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

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

fn open_room_panel(shell: &mut Shell, _: &mut Window, cx: &mut Context<Shell>) {
    crate::measure::fill_room_panel(shell, cx);
    shell
        .dock(Side::Right)
        .update(cx, |dock, cx| dock.move_to(DockState::Open, cx));
}

/// Opens the window at `window_size`, lets `arrange` set it up, and saves what
/// it draws as `<name>.png`.
fn render(
    name: &str,
    appearance: WindowAppearance,
    window_size: Size<Pixels>,
    arrange: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) + 'static,
) {
    let mut cx = HeadlessAppContext::with_platform(
        Arc::new(gpui_apple::AppleTextSystem::new()),
        Arc::new(Assets),
        gpui_platform::current_headless_renderer,
    );
    cx.update(|cx| {
        load_fonts(cx);
        crate::input::bind_keys(cx);
        title_bar::bind_keys(cx);
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
            window.refresh();
        })
        .expect("the window is open");
    cx.run_until_parked();

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
}
