//! UniFFI bindings to `mudling-core` for the SwiftUI shell.
//!
//! The boundary is coarse (PRD §12): Swift opens a session, subscribes to
//! stores, drains diffs on its frame tick and sends commands. Lines never
//! cross one at a time; only store updates do.

use mudling_core::Line;
use mudling_core::plugin::luau::Value;
use mudling_core::store::{
    CharactersState, CuesState, Diff, GearState, MapState, MobsState, RoomState, RulesState,
    SettingsState, SheetState, Store, StoreHub, StoreKind, TargetState, TerminalChange,
    TerminalState, Versioned, VitalsState,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

pub use mudling_core as core;

uniffi::setup_scaffolding!();

#[uniffi::remote(Enum)]
pub enum StoreKind {
    Terminal,
    Vitals,
    Room,
    Map,
    Mobs,
    Characters,
    Sheet,
    Target,
    Gear,
    Settings,
    Rules,
    Cues,
}

/// Told when changes are waiting, at most once per drained batch. It's
/// called on a core thread; the shell should schedule a frame and drain then.
#[uniffi::export(with_foreign)]
pub trait FrameWaker: Send + Sync {
    fn changes_pending(&self);
}

/// A store's whole state, as a subscription starts.
#[derive(uniffi::Enum)]
pub enum StoreSnapshot {
    Terminal {
        revision: u64,
        lines: Vec<String>,
    },
    Fields {
        store: StoreKind,
        revision: u64,
        fields: HashMap<String, String>,
    },
}

/// A store's changes in one frame. Applied in order to its snapshot, they
/// give the core's state; `fields` are the named values that changed.
#[derive(uniffi::Enum)]
pub enum StoreDiff {
    TerminalAppended {
        revision: u64,
        lines: Vec<String>,
    },
    Fields {
        store: StoreKind,
        revision: u64,
        fields: HashMap<String, String>,
    },
}

#[derive(uniffi::Object)]
pub struct Session {
    hub: StoreHub,
}

#[uniffi::export]
impl Session {
    #[uniffi::constructor]
    pub fn open(waker: Arc<dyn FrameWaker>) -> Arc<Self> {
        Arc::new(Session {
            hub: StoreHub::new(move || waker.changes_pending()),
        })
    }

    pub fn subscribe(&self, store: StoreKind) -> StoreSnapshot {
        with_store!(store, S => S::snapshot(&self.hub.subscribe::<S>()))
    }

    pub fn unsubscribe(&self, store: StoreKind) {
        with_store!(store, S => self.hub.unsubscribe::<S>())
    }

    /// The store's current state, for a view that lost track of it.
    pub fn snapshot(&self, store: StoreKind) -> StoreSnapshot {
        with_store!(store, S => S::snapshot(&self.hub.snapshot::<S>()))
    }

    /// The changes since the last drain; empty when nothing changed.
    pub fn drain(&self) -> Vec<StoreDiff> {
        let Some(batch) = self.hub.drain() else {
            return Vec::new();
        };
        let mudling_core::store::Batch {
            terminal,
            vitals,
            room,
            map,
            mobs,
            characters,
            sheet,
            target,
            gear,
            settings,
            rules,
            cues,
        } = batch;
        [
            terminal.map(TerminalState::diff),
            vitals.map(VitalsState::diff),
            room.map(RoomState::diff),
            map.map(MapState::diff),
            mobs.map(MobsState::diff),
            characters.map(CharactersState::diff),
            sheet.map(SheetState::diff),
            target.map(TargetState::diff),
            gear.map(GearState::diff),
            settings.map(SettingsState::diff),
            rules.map(RulesState::diff),
            cues.map(CuesState::diff),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Only echoed into the terminal store: `net` isn't wired to a session
    /// yet.
    pub fn send_command(&self, command: String) {
        self.hub
            .write::<TerminalState>(TerminalChange::Append(vec![Line::command(&command)]));
    }
}

/// Runs `$body` with `$S` naming the state type of the store `$kind`.
macro_rules! with_store {
    ($kind:expr, $S:ident => $body:expr) => {
        match $kind {
            StoreKind::Terminal => {
                type $S = TerminalState;
                $body
            }
            StoreKind::Vitals => {
                type $S = VitalsState;
                $body
            }
            StoreKind::Room => {
                type $S = RoomState;
                $body
            }
            StoreKind::Map => {
                type $S = MapState;
                $body
            }
            StoreKind::Mobs => {
                type $S = MobsState;
                $body
            }
            StoreKind::Characters => {
                type $S = CharactersState;
                $body
            }
            StoreKind::Sheet => {
                type $S = SheetState;
                $body
            }
            StoreKind::Target => {
                type $S = TargetState;
                $body
            }
            StoreKind::Gear => {
                type $S = GearState;
                $body
            }
            StoreKind::Settings => {
                type $S = SettingsState;
                $body
            }
            StoreKind::Rules => {
                type $S = RulesState;
                $body
            }
            StoreKind::Cues => {
                type $S = CuesState;
                $body
            }
        }
    };
}
use with_store;

/// How a store's state and diffs cross into Swift.
trait Bridged: Store {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot;
    fn diff(diff: Diff<Self>) -> StoreDiff;
}

/// The lines' text, without the prompts the HUD repeats.
fn texts<'a>(lines: impl IntoIterator<Item = &'a Line>) -> Vec<String> {
    lines
        .into_iter()
        .filter_map(Line::without_prompt)
        .map(|line| line.plain().to_owned())
        .collect()
}

impl Bridged for TerminalState {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
        StoreSnapshot::Terminal {
            revision: versioned.revision().0,
            lines: texts(versioned.value().lines().iter()),
        }
    }

    fn diff(diff: Diff<Self>) -> StoreDiff {
        let TerminalChange::Append(lines) = diff.change;
        StoreDiff::TerminalAppended {
            revision: diff.revision.0,
            lines: texts(&lines),
        }
    }
}

impl Bridged for MapState {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
        let map = versioned.value();
        let fields = HashMap::from([
            ("revision".to_owned(), map.revision().to_string()),
            ("rooms".to_owned(), map.rooms().len().to_string()),
        ]);
        StoreSnapshot::Fields {
            store: Self::KIND,
            revision: versioned.revision().0,
            fields,
        }
    }

    fn diff(diff: Diff<Self>) -> StoreDiff {
        let fields = HashMap::from([("revision".to_owned(), diff.change.revision.to_string())]);
        StoreDiff::Fields {
            store: Self::KIND,
            revision: diff.revision.0,
            fields,
        }
    }
}

/// The stores that cross as named fields, each both as a state and as a
/// change.
trait FieldStore: Store<Change = Self> {
    fn fields(&self) -> HashMap<String, String>;
}

fn value_fields(values: &BTreeMap<String, Value>) -> HashMap<String, String> {
    let text = |value: &Value| match value {
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Text(value) => value.clone(),
    };
    values
        .iter()
        .map(|(key, value)| (key.clone(), text(value)))
        .collect()
}

impl FieldStore for VitalsState {
    fn fields(&self) -> HashMap<String, String> {
        self.0
            .iter()
            .map(|(vital, value)| (vital.name().to_owned(), value.to_string()))
            .collect()
    }
}

impl FieldStore for CharactersState {
    fn fields(&self) -> HashMap<String, String> {
        value_fields(&self.0)
    }
}

impl FieldStore for GearState {
    fn fields(&self) -> HashMap<String, String> {
        value_fields(&self.0)
    }
}

impl FieldStore for RoomState {
    fn fields(&self) -> HashMap<String, String> {
        self.room
            .iter()
            .flat_map(|room| {
                [
                    ("id".to_owned(), room.id.to_string()),
                    ("name".to_owned(), room.name.clone()),
                    ("zone".to_owned(), room.zone.clone()),
                ]
            })
            .collect()
    }
}

impl FieldStore for MobsState {
    fn fields(&self) -> HashMap<String, String> {
        self.marks
            .iter()
            .map(|(mark, name)| (name.clone(), mark.name().to_owned()))
            .collect()
    }
}

impl FieldStore for SheetState {
    fn fields(&self) -> HashMap<String, String> {
        HashMap::from([
            ("character".to_owned(), self.character.clone()),
            ("skills".to_owned(), self.skills.len().to_string()),
            ("reading".to_owned(), self.reading.to_string()),
        ])
    }
}

impl FieldStore for TargetState {
    fn fields(&self) -> HashMap<String, String> {
        self.0
            .iter()
            .flat_map(|target| {
                let number = |name: &str, value: Option<i64>| {
                    value.map(|value| (name.to_owned(), value.to_string()))
                };
                [
                    Some(("name".to_owned(), target.name.clone())),
                    number("level", target.level),
                    number("health", target.health),
                    number("max_health", target.max_health),
                ]
            })
            .flatten()
            .collect()
    }
}

impl FieldStore for SettingsState {
    fn fields(&self) -> HashMap<String, String> {
        HashMap::from([
            ("connection".to_owned(), format!("{:?}", self.connection)),
            ("echo_off".to_owned(), self.echo_off.to_string()),
        ])
    }
}

impl FieldStore for RulesState {
    fn fields(&self) -> HashMap<String, String> {
        HashMap::from([
            ("triggers".to_owned(), self.triggers.len().to_string()),
            ("aliases".to_owned(), self.aliases.len().to_string()),
            ("hotbar".to_owned(), self.hotbar.join("\n")),
        ])
    }
}

/// Cues aren't state: each diff names the sounds to play once.
impl Bridged for CuesState {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
        StoreSnapshot::Fields {
            store: Self::KIND,
            revision: versioned.revision().0,
            fields: HashMap::new(),
        }
    }

    fn diff(diff: Diff<Self>) -> StoreDiff {
        let fields = HashMap::from([("sounds".to_owned(), diff.change.join("\n"))]);
        StoreDiff::Fields {
            store: Self::KIND,
            revision: diff.revision.0,
            fields,
        }
    }
}

impl<S: FieldStore> Bridged for S {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
        StoreSnapshot::Fields {
            store: Self::KIND,
            revision: versioned.revision().0,
            fields: versioned.value().fields(),
        }
    }

    fn diff(diff: Diff<Self>) -> StoreDiff {
        StoreDiff::Fields {
            store: Self::KIND,
            revision: diff.revision.0,
            fields: diff.change.fields(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct CountingWaker(AtomicUsize);

    impl FrameWaker for CountingWaker {
        fn changes_pending(&self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn a_command_reaches_a_subscribed_shell_as_one_diff() {
        let waker = Arc::new(CountingWaker::default());
        let session = Session::open(waker.clone());
        let StoreSnapshot::Terminal { revision: 0, lines } = session.subscribe(StoreKind::Terminal)
        else {
            panic!("an empty terminal snapshot");
        };
        assert!(lines.is_empty());

        session.send_command("look".to_owned());
        session.send_command("score".to_owned());

        let diffs = session.drain();
        let [StoreDiff::TerminalAppended { revision: 2, lines }] = diffs.as_slice() else {
            panic!("one terminal diff");
        };
        assert_eq!(lines, &["look", "score"]);
        assert_eq!(waker.0.load(Ordering::Relaxed), 1);
        assert!(session.drain().is_empty());
    }
}
