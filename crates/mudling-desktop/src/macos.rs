//! `--ax-check` (macOS): reads the window's accessibility tree from inside
//! the app, through the same `NSAccessibility` methods the system calls for
//! VoiceOver, and checks the chat, the input and the live region. Reading
//! another app's tree with `AXUIElement` needs the Accessibility permission;
//! this needs none.

// Objective-C messages to AppKit's accessibility protocol are unsafe calls.
#![allow(unsafe_code)]

use crate::chat::Chat;
use gpui::{App, Entity};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Sel};
use objc2::{MainThreadMarker, Message, msg_send, sel};
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

#[derive(Serialize)]
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Lines the check sends through the chat. Synthetic: no real chat.
const PROBE_LINES: [&str; 2] = [
    "ax-check: the first new line",
    "ax-check: the second new line",
];

/// Runs the checks, prints a JSON report and exits: 0 if all passed.
pub fn run_ax_check(chat: Entity<Chat>, cx: &mut App) {
    cx.spawn(async move |cx| {
        let pause = Duration::from_millis(500);
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

        chat.update(cx, |chat, cx| {
            for line in PROBE_LINES {
                chat.push_server_line(line, cx);
            }
        });
        cx.background_executor().timer(pause).await;
        let tree = window_tree();
        let statuses = tree.as_ref().map_or_else(Vec::new, |tree| {
            tree.find(|e| e.is("AXGroup", Some("AXApplicationStatus")))
        });
        let expected = PROBE_LINES.join("\n");
        checks.push(Check {
            name: "new lines reach the live region",
            passed: statuses.len() == 1 && statuses[0].value.as_deref() == Some(expected.as_str()),
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

        let passed = checks.iter().all(|check| check.passed);
        let report = serde_json::json!({ "passed": passed, "checks": checks });
        println!(
            "{}",
            serde_json::to_string_pretty(&report).expect("the report serializes")
        );
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
