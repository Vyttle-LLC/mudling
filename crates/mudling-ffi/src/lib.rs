//! UniFFI bindings to `mudling-core` for the SwiftUI shell.
//!
//! The boundary is coarse (PRD §12): Swift opens a session, subscribes to
//! stores, drains diffs on its frame tick and sends commands. Lines never
//! cross one at a time; only store updates do.

use mudling_core::store::{
    CharactersState, Diff, Fields, GearState, MapState, MobsState, RoomState, SettingsState, Store,
    StoreHub, StoreKind, TerminalChange, TerminalLine, TerminalState, Versioned, VitalsState,
};
use std::collections::HashMap;
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
    Gear,
    Settings,
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
/// give the core's state.
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
            gear,
            settings,
        } = batch;
        [
            terminal.map(TerminalState::diff),
            vitals.map(VitalsState::diff),
            room.map(RoomState::diff),
            map.map(MapState::diff),
            mobs.map(MobsState::diff),
            characters.map(CharactersState::diff),
            gear.map(GearState::diff),
            settings.map(SettingsState::diff),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Only echoed into the terminal store: `net` isn't wired to a session
    /// yet.
    pub fn send_command(&self, command: String) {
        self.hub
            .write::<TerminalState>(TerminalChange::Append(vec![TerminalLine { text: command }]));
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
            StoreKind::Gear => {
                type $S = GearState;
                $body
            }
            StoreKind::Settings => {
                type $S = SettingsState;
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

fn texts<'a>(lines: impl IntoIterator<Item = &'a TerminalLine>) -> Vec<String> {
    lines.into_iter().map(|line| line.text.clone()).collect()
}

impl Bridged for TerminalState {
    fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
        StoreSnapshot::Terminal {
            revision: versioned.revision().0,
            lines: texts(versioned.value().lines()),
        }
    }

    fn diff(diff: Diff<Self>) -> StoreDiff {
        let TerminalChange::Append(lines) = diff.change;
        StoreDiff::TerminalAppended {
            revision: diff.revision.0,
            lines: lines.into_iter().map(|line| line.text).collect(),
        }
    }
}

fn fields_map(Fields(fields): &Fields) -> HashMap<String, String> {
    fields
        .iter()
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

/// The placeholder stores all cross as their named fields.
macro_rules! bridge_fields_stores {
    ($($state:ident),+) => {
        $(impl Bridged for $state {
            fn snapshot(versioned: &Versioned<Self>) -> StoreSnapshot {
                StoreSnapshot::Fields {
                    store: Self::KIND,
                    revision: versioned.revision().0,
                    fields: fields_map(&versioned.value().0),
                }
            }

            fn diff(diff: Diff<Self>) -> StoreDiff {
                StoreDiff::Fields {
                    store: Self::KIND,
                    revision: diff.revision.0,
                    fields: fields_map(&diff.change.0),
                }
            }
        })+
    };
}

bridge_fields_stores!(
    VitalsState,
    RoomState,
    MapState,
    MobsState,
    CharactersState,
    GearState,
    SettingsState
);

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
