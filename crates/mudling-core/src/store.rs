//! State split into stores, so a shell redraws only what changed (PRD §5 #5).
//!
//! Each store holds one kind of state and a [`Revision`] that bumps only on a
//! real change. Core threads write through a [`StoreHub`], which coalesces
//! the changes of a frame into one [`Batch`]. The shell drains that batch on
//! its frame tick and applies it to its own copy of the stores it subscribed
//! to. Derived data is cached in a [`Memo`] keyed on its inputs' revisions
//! (§5 #7).

mod hub;
mod memo;
mod payload;

pub use hub::StoreHub;
pub use memo::Memo;
pub use payload::{
    CharactersState, Fields, GearState, MapState, MobsState, RoomState, SettingsState,
    TERMINAL_CAPACITY, TerminalChange, TerminalLine, TerminalState, VitalsState,
};

/// How many real changes a store has seen. A shell compares revisions to
/// know whether a view is stale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision(pub u64);

impl Revision {
    fn next(self) -> Revision {
        Revision(self.0 + 1)
    }
}

/// What a store's state is, and how a change applies to it.
pub trait StoreState: Clone + Default + Send + 'static {
    /// One write to the store. It's also what a diff carries to a shell.
    type Change: Clone + Send;

    /// Applies `change`, returning whether the state actually changed.
    fn apply(&mut self, change: &Self::Change) -> bool;

    /// Folds `later` into `earlier`, so applying the result equals applying
    /// both in order. This is how a frame's changes become one diff.
    fn coalesce(earlier: &mut Self::Change, later: Self::Change);
}

/// A store's state with its revision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Versioned<S> {
    value: S,
    revision: Revision,
}

impl<S: StoreState> Versioned<S> {
    pub fn value(&self) -> &S {
        &self.value
    }

    pub fn revision(&self) -> Revision {
        self.revision
    }

    /// Applies a change, bumping the revision only if the state changed.
    pub fn write(&mut self, change: &S::Change) -> bool {
        let changed = self.value.apply(change);
        if changed {
            self.revision = self.revision.next();
        }
        changed
    }

    /// Brings a shell's copy up to date with a diff from the hub.
    pub fn apply_diff(&mut self, diff: &Diff<S>) {
        self.value.apply(&diff.change);
        self.revision = diff.revision;
    }
}

/// A store's coalesced changes for one frame, and its revision after them.
#[derive(Clone, Debug, PartialEq)]
pub struct Diff<S: StoreState> {
    pub revision: Revision,
    pub change: S::Change,
}

/// A store in the store set: its kind, and where it lives in [`Stores`] and
/// [`Batch`].
pub trait Store: StoreState {
    const KIND: StoreKind;
    fn slot(stores: &Stores) -> &Versioned<Self>;
    fn slot_mut(stores: &mut Stores) -> &mut Versioned<Self>;
    fn pending(batch: &mut Batch) -> &mut Option<Diff<Self>>;
}

/// Declares the store set (PRD §12) once: the kinds, every store's state
/// together, and a frame's batch of diffs.
macro_rules! store_set {
    ($($field:ident: $state:ty => $kind:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum StoreKind {
            $($kind),+
        }

        impl StoreKind {
            pub const ALL: [StoreKind; [$(StoreKind::$kind),+].len()] = [$(StoreKind::$kind),+];
        }

        /// Every store's state. The hub holds the core's copy; a shell keeps
        /// its own, updated by [`Stores::apply_batch`].
        #[derive(Clone, Debug, Default)]
        pub struct Stores {
            $(pub $field: Versioned<$state>),+
        }

        impl Stores {
            pub fn apply_batch(&mut self, batch: &Batch) {
                $(if let Some(diff) = &batch.$field {
                    self.$field.apply_diff(diff);
                })+
            }
        }

        /// One frame's diffs: at most one per store, and only for stores
        /// that changed and are subscribed.
        #[derive(Clone, Debug, Default)]
        pub struct Batch {
            $(pub $field: Option<Diff<$state>>),+
        }

        impl Batch {
            pub fn is_empty(&self) -> bool {
                true $(&& self.$field.is_none())+
            }

            pub fn kinds(&self) -> Vec<StoreKind> {
                let mut kinds = Vec::new();
                $(if self.$field.is_some() {
                    kinds.push(StoreKind::$kind);
                })+
                kinds
            }

            fn clear(&mut self, kind: StoreKind) {
                match kind {
                    $(StoreKind::$kind => self.$field = None),+
                }
            }
        }

        $(impl Store for $state {
            const KIND: StoreKind = StoreKind::$kind;

            fn slot(stores: &Stores) -> &Versioned<Self> {
                &stores.$field
            }

            fn slot_mut(stores: &mut Stores) -> &mut Versioned<Self> {
                &mut stores.$field
            }

            fn pending(batch: &mut Batch) -> &mut Option<Diff<Self>> {
                &mut batch.$field
            }
        })+
    };
}

store_set! {
    terminal: TerminalState => Terminal,
    vitals: VitalsState => Vitals,
    room: RoomState => Room,
    map: MapState => Map,
    mobs: MobsState => Mobs,
    characters: CharactersState => Characters,
    gear: GearState => Gear,
    settings: SettingsState => Settings,
}
