//! macOS's own answers. The Reduce Motion setting, and `--ax-check`: reads the
//! window's accessibility tree from inside the app, through the same
//! `NSAccessibility` methods the system calls for VoiceOver, and checks the
//! chat, the input, the live region, the title bar and the settings sheet. Reading another app's
//! tree with `AXUIElement` needs the Accessibility permission; this needs none.

// Objective-C messages to AppKit's accessibility protocol are unsafe calls.
#![allow(unsafe_code)]

use crate::shell::Shell;
use crate::stream;
use gpui::{App, Entity};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, Message, class, msg_send, sel};
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSArray, NSString};
use serde::Serialize;
use std::time::Duration;

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

/// The element the window reports as focused. (The app-level query answers
/// with the window when the app isn't frontmost, as in a scripted run.)
fn focused_element() -> Option<AxElement> {
    let view = content_view()?;
    // SAFETY: the getter returns nil or an accessibility element.
    let focused: Option<Retained<AnyObject>> =
        unsafe { getter(&view, sel!(accessibilityFocusedUIElement)) };
    focused.map(|element| AxElement::read(&element))
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
const SETTINGS_CONTROLS: [&str; 10] = [
    "Servers & characters",
    "Plugins",
    "Triggers & aliases",
    "Appearance",
    "Shortcuts",
    "Data & sync",
    "Vault",
    "Close settings",
    "Theme",
    "Larger chat text",
];

/// Text the sheet's Appearance section shows, read from its nodes' values.
const SETTINGS_TEXT: [&str; 2] = ["A Vyttle app", "The chat will read like this"];

const TITLE_BAR_CONTROLS: [&str; 5] = [
    "Left panel",
    "Session: Recording",
    "Search",
    "Right panel",
    "Settings",
];

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
        let focused = focused_element();
        checks.push(Check {
            name: "focus is on a text field named Command",
            passed: focused.as_ref().is_some_and(|f| {
                f.is("AXTextField", None) && f.title.as_deref() == Some("Command")
            }),
            detail: format!("{:?}", focused.as_ref().map(|f| (&f.role, &f.title))),
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
        let accesskit_tree = cx
            .update(|cx| {
                cx.windows().first().and_then(|w| {
                    w.update(cx, |_, window, _| window.debug_a11y_tree_json())
                        .ok()
                })
            })
            .flatten();
        let accesskit_focus = accesskit_tree.as_deref().and_then(focused_accesskit_node);
        checks.push(Check {
            name: "AccessKit tree (shared with NVDA and Orca) focuses a TextInput named Command",
            passed: accesskit_focus
                .as_ref()
                .is_some_and(|(role, label)| role == "TextInput" && label == "Command"),
            detail: format!("{accesskit_focus:?}"),
        });

        let opened = cx.update(|cx| {
            let window = cx.windows().into_iter().next()?;
            window
                .update(cx, |_, window, cx| {
                    shell.update(cx, |shell, cx| shell.open_settings(window, cx));
                })
                .ok()
        });
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
        let accesskit_tree = cx
            .update(|cx| {
                cx.windows().first().and_then(|w| {
                    w.update(cx, |_, window, _| window.debug_a11y_tree_json())
                        .ok()
                })
            })
            .flatten();
        let accesskit_focus = accesskit_tree.as_deref().and_then(focused_accesskit_node);
        checks.push(Check {
            name: "AccessKit tree moves focus to a section tab when settings opens",
            passed: accesskit_focus.as_ref().is_some_and(|(role, label)| {
                role == "Tab" && SETTINGS_CONTROLS.contains(&label.as_str())
            }),
            detail: format!("{accesskit_focus:?}"),
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
