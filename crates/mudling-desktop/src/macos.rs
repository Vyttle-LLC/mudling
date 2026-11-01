//! macOS's own answers. The Reduce Motion setting, and `--ax-check`: reads the
//! window's accessibility tree from inside the app, through the same
//! `NSAccessibility` methods the system calls for VoiceOver, and checks the
//! chat, the input, the live region, the title bar, the Room panel's buttons, the
//! HUD, find, the settings sheet and the empty state. Reading another app's tree
//! with `AXUIElement` needs the Accessibility permission; this needs none.

// Objective-C messages to AppKit's accessibility protocol are unsafe calls.
#![allow(unsafe_code)]

use crate::hud::HudPlacement;
use crate::launcher::Launcher;
use crate::measure;
use crate::settings::Section;
use crate::shell::Shell;
use crate::stream;
use gpui::{App, AsyncApp, Context, Entity, Window, px};
use mudling_core::logs::{Kind, LogFile};
use mudling_core::persist::{self, Timestamp};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, Message, class, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSPoint, NSString};
use serde::Serialize;
use std::time::Duration;
use tempfile::TempDir;

/// One element as an accessibility client sees it.
#[derive(Debug, Serialize)]
pub struct AxElement {
    pub role: Option<String>,
    pub subrole: Option<String>,
    pub title: Option<String>,
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AxElement>,
}

impl AxElement {
    fn read(element: &AnyObject) -> AxElement {
        // SAFETY: each getter returns nil or an object of the type asked for:
        // NSString for role, subrole and title, NSArray for children.
        unsafe {
            let role: Option<Retained<NSString>> = getter(element, sel!(accessibilityRole));
            let subrole: Option<Retained<NSString>> = getter(element, sel!(accessibilitySubrole));
            let title: Option<Retained<NSString>> = getter(element, sel!(accessibilityTitle));
            let value: Option<Retained<AnyObject>> = getter(element, sel!(accessibilityValue));
            let children: Option<Retained<NSArray<AnyObject>>> =
                getter(element, sel!(accessibilityChildren));
            AxElement {
                role: role.map(|s| s.to_string()),
                subrole: subrole.map(|s| s.to_string()),
                title: title.map(|s| s.to_string()),
                value: value.map(|v| describe(&v)),
                children: children
                    .map(|c| c.iter().map(|child| AxElement::read(&child)).collect())
                    .unwrap_or_default(),
            }
        }
    }

    fn find_all<'a>(
        &'a self,
        matches: &impl Fn(&AxElement) -> bool,
        found: &mut Vec<&'a AxElement>,
    ) {
        if matches(self) {
            found.push(self);
        }
        for child in &self.children {
            child.find_all(matches, found);
        }
    }

    fn find(&self, matches: impl Fn(&AxElement) -> bool) -> Vec<&AxElement> {
        let mut found = Vec::new();
        self.find_all(&matches, &mut found);
        found
    }

    fn is(&self, role: &str, subrole: Option<&str>) -> bool {
        self.role.as_deref() == Some(role)
            && (subrole.is_none() || self.subrole.as_deref() == subrole)
    }
}

/// Calls a no-argument getter, if the element has it. Some elements the
/// system inserts (cell proxies, for one) answer only part of the protocol.
///
/// # Safety
/// The getter must return nil or an object of type `T`.
unsafe fn getter<T: Message>(element: &AnyObject, selector: Sel) -> Option<Retained<T>> {
    let responds: bool = unsafe { msg_send![element, respondsToSelector: selector] };
    if !responds {
        return None;
    }
    unsafe { msg_send![element, performSelector: selector] }
}

fn describe(object: &AnyObject) -> String {
    // SAFETY: every NSObject answers `description` with an NSString.
    let description: Retained<NSString> = unsafe { msg_send![object, description] };
    description.to_string()
}

/// The first window's content view, where AccessKit attaches its tree.
fn content_view() -> Option<Retained<objc2_app_kit::NSView>> {
    let window = NSApplication::sharedApplication(MainThreadMarker::new()?)
        .windows()
        .firstObject()?;
    window.contentView()
}

fn window_tree() -> Option<AxElement> {
    content_view().map(|view| AxElement::read(&view))
}

/// Keeps the window out of the player's way for a check: off the screen but for
/// a sliver at its right edge, letting every click through, above other windows
/// and on every Space. gpui stops drawing a window that's covered, so one that's
/// merely behind others would lose its frames. A clear window keeps them, but
/// macOS holds its drawables back, which skews the frames being measured.
///
/// It's clear only while it's shown: macOS moves a window onto the screen as it
/// shows one, so it can only leave once it's there.
pub fn show_unseen(cx: &App) {
    let Some(screen) = cx.primary_display().map(|display| display.bounds()) else {
        return;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(window) = NSApplication::sharedApplication(mtm)
        .windows()
        .firstObject()
    else {
        return;
    };
    let sliver = NSPoint::new(
        f64::from(f32::from(screen.size.width)) - SLIVER_WIDTH,
        SLIVER_HEIGHT - window.frame().size.height,
    );
    // SAFETY: all of these are NSWindow's own methods, called on the main thread.
    unsafe {
        let _: () = msg_send![&window, setAlphaValue: 0.0_f64];
        let _: () = msg_send![&window, setIgnoresMouseEvents: true];
        let _: () = msg_send![&window, setLevel: FLOATING_LEVEL];
        let _: () = msg_send![&window, setCollectionBehavior: ON_EVERY_SPACE];
        let _: () = msg_send![&window, orderFront: std::ptr::null::<AnyObject>()];
    }
    window.setFrameOrigin(sliver);
    // SAFETY: as above.
    let _: () = unsafe { msg_send![&window, setAlphaValue: 1.0_f64] };
}

/// The role and label of what has focus in the app, from gpui's own AccessKit
/// tree. A background window has no focus as far as macOS is concerned, and
/// AppKit's focus queries answer with the window, so this is what can tell
/// right from wrong while another app is in front.
fn app_focus(cx: &AsyncApp) -> Option<(String, String)> {
    let tree = cx.update(|cx| {
        let window = *cx.windows().first()?;
        window
            .update(cx, |_, window, _| window.debug_a11y_tree_json())
            .ok()
    })?;
    focused_accesskit_node(&tree?)
}

/// Whether the app's focus is on `label`, shown to macOS as `native_role`.
fn focus_is_on(
    focus: Option<&(String, String)>,
    tree: Option<&AxElement>,
    (role, native_role): (&str, &str),
    label: &str,
) -> bool {
    let reaches_macos = |tree: &AxElement| {
        !tree
            .find(|e| e.is(native_role, None) && e.title.as_deref() == Some(label))
            .is_empty()
    };
    focus
        .is_some_and(|(focused_role, focused_label)| focused_role == role && focused_label == label)
        && tree.is_some_and(reaches_macos)
}

/// Whether the player turned on Reduce Motion in System Settings.
#[cfg_attr(test, allow(dead_code))]
pub fn reduce_motion() -> bool {
    // SAFETY: both are NSWorkspace's own methods: the shared instance, and a
    // BOOL property on it.
    unsafe {
        let workspace: Retained<AnyObject> = msg_send![class!(NSWorkspace), sharedWorkspace];
        msg_send![&workspace, accessibilityDisplayShouldReduceMotion]
    }
}

#[derive(Serialize)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Lines the check sends through the chat. Synthetic: no real chat.
const PROBE_LINES: [&str; 2] = [
    "ax-check: the first new line",
    "ax-check: the second new line, long enough to wrap to a second row or a third however \
     wide the window is, and to be read as one line all the same by a screen reader",
];

/// What a screen reader must find in the open settings sheet.
const SETTINGS_CONTROLS: [&str; 14] = [
    "Servers & characters",
    "Plugins",
    "Triggers & aliases",
    "Appearance",
    "Shortcuts",
    "Data & sync",
    "Logs",
    "Vault",
    "Close settings",
    "Theme",
    "Larger chat text",
    "HUD placement",
    "Floating card",
    "Show prompt lines",
];

/// The server the Logs section's sample log is for.
const LOG_SERVER: &str = "mud.example";
/// What a screen reader must find in the Logs section with one chat log kept.
const LOGS_CONTROLS: [&str; 5] = [
    "Keep debug logs, on",
    "Keep chat logs for mud.example, on",
    "Clear chat logs",
    "Pin chat log of mud.example, 2026-03-01 00:04",
    "Export chat log of mud.example, 2026-03-01 00:04",
];

/// A log to show, with no file behind it: the check clicks nothing.
fn sample_log() -> LogFile {
    LogFile {
        kind: Kind::Chat,
        path: "sample.log".into(),
        server: LOG_SERVER.to_owned(),
        started: Timestamp(1_772_323_440_000),
        bytes: 2_500_000,
        pinned: false,
        live: false,
    }
}
/// What a screen reader must find in Data & sync, filled with made-up rows.
const DATA_CONTROLS: [&str; 3] = ["Sync now", "Hide history", "Stop syncing"];
/// Text Data & sync shows, read from its nodes' values.
const DATA_TEXT: [&str; 3] = ["ssh-agent", "Last synced", "Never synced"];
/// What a screen reader must find in Vault, filled with made-up rows.
const VAULT_CONTROLS: [&str; 7] = [
    "Reveal the password for Sample on mud.example",
    "Change the password for Sample on mud.example",
    "Add a password",
    "Lock the vault after",
    "Lock now",
    "Change master password",
    "Enter the old master password",
];
/// Typed into a masked field, and must reach no node. Made up for this check.
const TYPED_SECRET: &str = "ax-check typed secret";

/// The HUD's meters, by name, with a value each must read as.
const HUD_METERS: [(&str, &str); 4] = [
    ("Hit points", "3,120/4,300"),
    ("Mana", "1,040/1,800"),
    ("Moves", "810/900"),
    ("Next level", "1,240 to go"),
];
const CARD_NAME: &str = "Status card. The arrow keys move it.";

/// Text the sheet's Appearance section shows, read from its nodes' values.
const SETTINGS_TEXT: [&str; 2] = ["A Vyttle app", "The chat will read like this"];

/// What arrives with the chat split. Synthetic: no real chat.
const SPLIT_PROBE_LINES: [&str; 1] = ["ax-check: a line arriving while the chat is split"];
/// How far back the check splits the chat: past anything still on screen.
const SPLIT_DISTANCE: f32 = 2_000.;
/// Found in the one line that says it.
const FIND_PROBE: &str = "while the chat is split";
const FIND_FIELD: &str = "Find in the scrollback";

/// What a screen reader must find around the command bar. `look` is one of
/// the chips [`crate::measure::fill_hotbar`] puts there.
const COMMAND_BAR_CONTROLS: [&str; 4] = ["Hotbar", "look", "Add a chip", "Pin to the hotbar"];

const TITLE_BAR_CONTROLS: [&str; 5] = [
    "Left panel",
    "Session: Recording",
    "Search",
    "Right panel",
    "Settings",
];

/// How much of a check's window stays on the screen, in points.
const SLIVER_WIDTH: f64 = 2.;
const SLIVER_HEIGHT: f64 = 100.;
/// `NSFloatingWindowLevel`.
const FLOATING_LEVEL: isize = 3;
/// `NSWindowCollectionBehavior`'s `CanJoinAllSpaces`, `Stationary` and
/// `FullScreenAuxiliary`: it stays put, on every Space, over full-screen apps.
const ON_EVERY_SPACE: usize = 1 << 0 | 1 << 4 | 1 << 8;

/// The same control as AccessKit and macOS each call it.
const TEXT_FIELD: (&str, &str) = ("TextInput", "AXTextField");
const BUTTON: (&str, &str) = ("Button", "AXButton");

/// The saved server the empty state's walk shows. Synthetic: no real game.
const SAMPLE_SERVER: &str = "mud.example";
/// What a screen reader must find on "Where to today?", the first card's
/// Connect first.
const EMPTY_STATE_CONTROLS: [&str; 5] = [
    "Connect to mud.example",
    "Add a server",
    "Quick connect",
    "Connect",
    "Settings",
];
/// Text the empty state shows, read from its nodes' values.
const EMPTY_STATE_TEXT: [&str; 3] = [
    "Where to today?",
    "Pick a character to log back in",
    "stay on this Mac",
];
/// What the Add form adds, its field first.
const ADD_FORM_CONTROLS: [&str; 3] = ["Server address", "Save", "Cancel"];

/// Runs the checks, prints a JSON report and exits: 0 if all passed.
pub fn run_ax_check(shell: Entity<Shell>, cx: &mut App) {
    let chat = shell.read(cx).chat();
    cx.spawn(async move |cx| {
        let pause = Duration::from_millis(500);
        while !shell.read_with(cx, |shell, _| shell.replay_finished()) {
            cx.background_executor().timer(pause).await;
        }
        // The first query switches AccessKit on; gpui sends the full tree with
        // the next frame.
        window_tree();
        cx.background_executor().timer(pause).await;

        let mut checks = Vec::new();
        let tree = window_tree();
        let chat_nodes = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| {
                e.is("AXGroup", Some("AXApplicationLog")) && e.title.as_deref() == Some("Chat")
            })
        });
        checks.push(Check {
            name: "chat is an application log named Chat",
            passed: chat_nodes.len() == 1,
            detail: format!("{} matching element(s)", chat_nodes.len()),
        });
        let lines = chat_nodes
            .first()
            .map_or_else(Vec::new, |chat| chat.find(|e| e.is("AXStaticText", None)));
        let newest = chat.read_with(cx, |chat, _| chat.newest_text());
        checks.push(Check {
            name: "visible chat lines are static text, newest last",
            passed: !lines.is_empty() && lines.last().and_then(|l| l.value.clone()) == newest,
            detail: format!(
                "{} line element(s) of {} in the scrollback",
                lines.len(),
                chat.read_with(cx, |c, _| c.len())
            ),
        });
        let focused = app_focus(cx);
        checks.push(Check {
            name: "focus is on a text field named Command",
            passed: focus_is_on(focused.as_ref(), tree.as_ref(), TEXT_FIELD, "Command"),
            detail: format!("{focused:?}"),
        });

        let unnamed: Vec<_> = TITLE_BAR_CONTROLS
            .into_iter()
            .filter(|name| {
                let named = |e: &AxElement| e.title.as_deref() == Some(*name);
                tree.as_ref().is_none_or(|tree| tree.find(named).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "title bar controls are named",
            passed: unnamed.is_empty(),
            detail: format!("not found: {unnamed:?}"),
        });

        shell.update(cx, |shell, _| shell.replay(stream::reads(&PROBE_LINES)));
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let statuses = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXGroup", Some("AXApplicationStatus")))
        });
        let wrapped = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXStaticText", None) && e.value.as_deref() == Some(PROBE_LINES[1]))
        });
        checks.push(Check {
            name: "a wrapped line is one static text holding all of its text",
            passed: wrapped.len() == 1,
            detail: format!("{} matching element(s)", wrapped.len()),
        });
        let expected = PROBE_LINES.join("\n");
        checks.push(Check {
            name: "new lines reach the live region, as its value (macOS) and name (NVDA, Orca)",
            passed: statuses.len() == 1
                && statuses[0].value.as_deref() == Some(expected.as_str())
                && statuses[0].title.as_deref() == Some(expected.as_str()),
            detail: format!(
                "{:?}",
                statuses.iter().map(|s| &s.value).collect::<Vec<_>>()
            ),
        });
        shell.update(cx, |shell, cx| {
            crate::measure::fill_room_panel(shell, None, cx);
            crate::measure::fill_hotbar(shell, cx);
            shell.open_panels(cx);
        });
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let unnamed: Vec<_> = COMMAND_BAR_CONTROLS
            .into_iter()
            .filter(|name| {
                let named = |e: &AxElement| e.title.as_deref() == Some(*name);
                tree.as_ref().is_none_or(|tree| tree.find(named).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the hotbar and the pin are named",
            passed: unnamed.is_empty(),
            detail: format!("not found: {unnamed:?}"),
        });
        let named_buttons = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXButton", None))
                .into_iter()
                .filter_map(|button| button.title.as_deref())
                .collect()
        });
        let room_buttons = ["Go north", "Open the door and go east", "Get a stone"];
        checks.push(Check {
            name: "the Room panel's buttons carry their names",
            passed: room_buttons.iter().all(|name| named_buttons.contains(name)),
            detail: format!("{named_buttons:?}"),
        });
        chat.update(cx, |chat, cx| chat.scroll_back(px(SPLIT_DISTANCE), cx));
        cx.background_executor().timer(pause).await;
        let probe = stream::reads(&SPLIT_PROBE_LINES);
        shell.update(cx, |shell, _| shell.replay(probe));
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let history_lines = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXGroup", None) && e.title.as_deref() == Some("Chat history"))
                .first()
                .map_or_else(Vec::new, |pane| pane.find(|e| e.is("AXStaticText", None)))
        });
        checks.push(Check {
            name: "with the chat split, the history pane's lines are static text one by one",
            passed: history_lines.len() > 1,
            detail: format!("{} line element(s)", history_lines.len()),
        });
        let logs = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| {
                e.is("AXGroup", Some("AXApplicationLog")) && e.title.as_deref() == Some("Chat")
            })
        });
        let live_newest = logs
            .first()
            .map(|log| log.find(|e| e.is("AXStaticText", None)))
            .and_then(|lines| lines.last().and_then(|line| line.value.clone()));
        checks.push(Check {
            name: "with the chat split, the log ends on the newest line",
            passed: live_newest.as_deref() == Some(SPLIT_PROBE_LINES[0]),
            detail: format!("{live_newest:?}"),
        });
        let statuses = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXGroup", Some("AXApplicationStatus")))
        });
        checks.push(Check {
            name: "with the chat split, new lines still reach the live region",
            passed: statuses.len() == 1
                && statuses[0].value.as_deref() == Some(SPLIT_PROBE_LINES[0])
                && statuses[0].title.as_deref() == Some(SPLIT_PROBE_LINES[0]),
            detail: format!(
                "{:?}",
                statuses.iter().map(|s| &s.value).collect::<Vec<_>>()
            ),
        });
        let chip_named = |e: &AxElement| {
            e.is("AXButton", None)
                && e.title
                    .as_deref()
                    .is_some_and(|title| title.starts_with("Back to live"))
        };
        let chips = tree
            .as_ref()
            .map_or_else(Vec::new, |tree| tree.find(chip_named));
        checks.push(Check {
            name: "with the chat split, the Back to live chip is a button",
            passed: chips.len() == 1,
            detail: format!("{:?}", chips.iter().map(|c| &c.title).collect::<Vec<_>>()),
        });

        let accesskit_focus = app_focus(cx);
        checks.push(Check {
            name: "AccessKit tree (shared with NVDA and Orca) focuses a TextInput named Command",
            passed: accesskit_focus
                .as_ref()
                .is_some_and(|(role, label)| role == "TextInput" && label == "Command"),
            detail: format!("{accesskit_focus:?}"),
        });

        let in_window = |run: &dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>),
                         cx: &mut AsyncApp| {
            cx.update(|cx| {
                let window = cx.windows().into_iter().next()?;
                window
                    .update(cx, |_, window, cx| {
                        shell.update(cx, |shell, cx| run(shell, window, cx));
                    })
                    .ok()
            })
        };
        in_window(
            &|shell, window, cx| {
                let (vitals, characters, target) = crate::measure::sample_hud(0);
                shell.show_sample_hud(&vitals, &characters, &target, cx);
                shell.place_hud(HudPlacement::Attached, true, window, cx);
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let unread: Vec<_> = HUD_METERS
            .into_iter()
            .filter(|(name, value)| {
                let reads = |e: &AxElement| {
                    e.title.as_deref() == Some(*name) && e.value.as_deref() == Some(*value)
                };
                tree.as_ref().is_none_or(|tree| tree.find(reads).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the HUD's meters read their names and values",
            passed: unread.is_empty(),
            detail: format!("not read: {unread:?}"),
        });
        let toggle = |e: &AxElement| {
            e.is("AXButton", None) && e.title.as_deref() == Some("Hide status details")
        };
        checks.push(Check {
            name: "the opened HUD has a button that closes it",
            passed: tree
                .as_ref()
                .is_some_and(|tree| tree.find(toggle).len() == 1),
            detail: format!("{:?}", tree.as_ref().map(|tree| tree.find(toggle).len())),
        });
        in_window(
            &|shell, window, cx| {
                shell.place_hud(HudPlacement::Floating, false, window, cx);
                shell
                    .hud()
                    .update(cx, |hud, cx| hud.use_shortcut(window, cx));
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let focused = app_focus(cx);
        checks.push(Check {
            name: "the shortcut brings the keyboard to the named floating card",
            passed: focused
                .as_ref()
                .is_some_and(|(_, label)| label.as_str() == CARD_NAME),
            detail: format!("{focused:?}"),
        });
        in_window(
            &|shell, window, cx| shell.place_hud(HudPlacement::Attached, false, window, cx),
            cx,
        );
        cx.background_executor().timer(pause).await;

        in_window(
            &|shell, window, cx| {
                shell.open_find(window, cx);
                chat.update(cx, |chat, cx| chat.type_in_find(FIND_PROBE, window, cx));
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let focused = app_focus(cx);
        checks.push(Check {
            name: "find's field takes focus, with a name",
            passed: focus_is_on(focused.as_ref(), tree.as_ref(), TEXT_FIELD, FIND_FIELD),
            detail: format!("{focused:?}"),
        });
        let statuses = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXGroup", Some("AXApplicationStatus")))
        });
        let said = |text: &Option<String>| text.as_deref().is_some_and(|t| t.ends_with(" result"));
        checks.push(Check {
            name: "find's result count reaches a live region",
            passed: statuses.iter().any(|s| said(&s.value) && said(&s.title)),
            detail: format!(
                "{:?}",
                statuses.iter().map(|s| &s.value).collect::<Vec<_>>()
            ),
        });

        let opened = in_window(
            &|shell, window, cx| {
                shell.close_find(window, cx);
                shell.open_settings(window, cx);
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing: Vec<_> = SETTINGS_CONTROLS
            .into_iter()
            .filter(|name| {
                let named = |e: &AxElement| e.title.as_deref() == Some(*name);
                tree.as_ref().is_none_or(|tree| tree.find(named).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the settings sheet names its sections and controls",
            passed: opened.is_some() && missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });
        let unread: Vec<_> = SETTINGS_TEXT
            .into_iter()
            .filter(|text| {
                let reads = |e: &AxElement| e.value.as_deref().is_some_and(|v| v.contains(*text));
                tree.as_ref().is_none_or(|tree| tree.find(reads).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the settings sheet's text reaches the tree",
            passed: unread.is_empty(),
            detail: format!("not found: {unread:?}"),
        });
        let accesskit_focus = app_focus(cx);
        checks.push(Check {
            name: "AccessKit tree moves focus to a section tab when settings opens",
            passed: accesskit_focus.as_ref().is_some_and(|(role, label)| {
                role == "Tab" && SETTINGS_CONTROLS.contains(&label.as_str())
            }),
            detail: format!("{accesskit_focus:?}"),
        });

        let on_logs = in_window(
            &|shell, _, cx| {
                shell.show_sample_logs(LOG_SERVER, sample_log(), cx);
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing: Vec<_> = LOGS_CONTROLS
            .into_iter()
            .filter(|name| {
                let named = |e: &AxElement| e.title.as_deref() == Some(*name);
                tree.as_ref().is_none_or(|tree| tree.find(named).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the Logs section names its switches, pin, export and clear",
            passed: on_logs.is_some() && missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });

        let named_in = |tree: &Option<AxElement>, names: &[&'static str]| -> Vec<&'static str> {
            let unnamed = |name: &&'static str| {
                let named = |e: &AxElement| e.title.as_deref() == Some(*name);
                tree.as_ref().is_none_or(|tree| tree.find(named).is_empty())
            };
            names.iter().copied().filter(unnamed).collect()
        };
        let opened = in_window(
            &|shell, window, cx| measure::open_sample_section(shell, Section::Data, window, cx),
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing = named_in(&tree, &DATA_CONTROLS);
        checks.push(Check {
            name: "Data & sync names its controls",
            passed: opened.is_some() && missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });
        let unread: Vec<_> = DATA_TEXT
            .into_iter()
            .filter(|text| {
                let reads = |e: &AxElement| e.value.as_deref().is_some_and(|v| v.contains(*text));
                tree.as_ref().is_none_or(|tree| tree.find(reads).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "Data & sync's text reaches the tree",
            passed: unread.is_empty(),
            detail: format!("not found: {unread:?}"),
        });

        let opened = in_window(
            &|shell, window, cx| {
                measure::open_sample_section(shell, Section::Vault, window, cx);
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing = named_in(&tree, &VAULT_CONTROLS);
        checks.push(Check {
            name: "Vault names its controls",
            passed: opened.is_some() && missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });

        in_window(
            &|shell, window, cx| {
                shell.settings_sheet().update(cx, |sheet, cx| {
                    sheet.type_sample_password(TYPED_SECRET, window, cx);
                });
            },
            cx,
        );
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let masked = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.subrole.as_deref() == Some("AXSecureTextField"))
        });
        let leaks = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| {
                [&e.title, &e.value]
                    .into_iter()
                    .flatten()
                    .any(|text| text.contains(TYPED_SECRET))
            })
        });
        checks.push(Check {
            name: "a masked field reaches the tree as a mask and nothing else does",
            passed: !masked.is_empty() && leaks.is_empty(),
            detail: format!("{} masked, {} leaks", masked.len(), leaks.len()),
        });

        // Throwaway, like the replay's: the check never reads the player's list.
        let sample_data = TempDir::new().expect("a folder for the sample server");
        persist::save_server(sample_data.path(), SAMPLE_SERVER, 4000)
            .expect("the sample server saves");
        let launcher = Launcher::new(sample_data.path().to_owned(), Vec::new(), Vec::new(), || {});
        let reached_empty_state = cx.update(|cx| {
            let window = cx.windows().into_iter().next()?;
            window
                .update(cx, |_, window, cx| {
                    shell.update(cx, |shell, cx| {
                        shell.leave_for_empty_state(launcher, window, cx);
                    });
                })
                .ok()
        });
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing = named_in(&tree, &EMPTY_STATE_CONTROLS);
        checks.push(Check {
            name: "the empty state names its cards and controls",
            passed: reached_empty_state.is_some() && missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });
        let unread: Vec<_> = EMPTY_STATE_TEXT
            .into_iter()
            .filter(|text| {
                let reads = |e: &AxElement| e.value.as_deref().is_some_and(|v| v.contains(*text));
                tree.as_ref().is_none_or(|tree| tree.find(reads).is_empty())
            })
            .collect();
        checks.push(Check {
            name: "the empty state's text reaches the tree",
            passed: unread.is_empty(),
            detail: format!("not found: {unread:?}"),
        });
        let focused = app_focus(cx);
        checks.push(Check {
            name: "focus starts on the first card's Connect",
            passed: focus_is_on(
                focused.as_ref(),
                tree.as_ref(),
                BUTTON,
                EMPTY_STATE_CONTROLS[0],
            ),
            detail: format!("{focused:?}"),
        });
        cx.update(|cx| {
            let window = cx.windows().into_iter().next()?;
            window
                .update(cx, |_, window, cx| {
                    let empty_state = shell.read(cx).empty_state();
                    empty_state.update(cx, |empty_state, cx| {
                        empty_state.start_adding(window, cx);
                    });
                })
                .ok()
        });
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let missing = named_in(&tree, &ADD_FORM_CONTROLS);
        checks.push(Check {
            name: "the Add form names its field and buttons",
            passed: missing.is_empty(),
            detail: format!("not found: {missing:?}"),
        });
        let focused = app_focus(cx);
        checks.push(Check {
            name: "the Add form takes focus in its text field",
            passed: focus_is_on(
                focused.as_ref(),
                tree.as_ref(),
                TEXT_FIELD,
                ADD_FORM_CONTROLS[0],
            ),
            detail: format!("{focused:?}"),
        });

        let passed = checks.iter().all(|check| check.passed);
        let report = serde_json::json!({ "passed": passed, "checks": checks });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("the report serializes")
        );
        // Exiting runs no destructors, so the replay's folder goes first.
        if let Some(saved) = shell.update(cx, |shell, cx| shell.leave(cx)) {
            crate::wait_for_save(saved, cx).await;
        }
        drop(sample_data);
        std::process::exit(if passed { 0 } else { 1 });
    })
    .detach();
}

/// The role and label of the focused node in gpui's AccessKit tree dump.
fn focused_accesskit_node(tree_json: &str) -> Option<(String, String)> {
    let tree: serde_json::Value = serde_json::from_str(tree_json).ok()?;
    let focus = tree.get("gpui_focus")?.as_str()?;
    let aria = tree.get("nodes")?.get(focus)?.get("aria")?;
    let field = |name: &str| {
        aria.get(name)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned()
    };
    Some((field("role"), field("label")))
}
