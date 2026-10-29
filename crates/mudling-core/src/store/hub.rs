//! The coalescing hub between the core's threads and a shell.

use super::{Batch, Diff, Store, StoreKind, Stores, Versioned};
use std::sync::{Mutex, MutexGuard};

type Waker = Box<dyn Fn() + Send + Sync>;

/// Holds the core's stores, and the changes a shell hasn't drained yet.
///
/// Core threads [`write`](Self::write); the shell drains once per frame. The
/// lock is held only to apply one change or to swap out a batch, so the UI
/// thread never waits behind I/O or a scan. A hub serves one shell.
pub struct StoreHub {
    inner: Mutex<Inner>,
    waker: Waker,
}

#[derive(Default)]
struct Inner {
    stores: Stores,
    subscribed: [bool; StoreKind::ALL.len()],
    pending: Batch,
}

impl StoreHub {
    /// `waker` runs on the writing thread when the first change after a
    /// drain arrives, so a shell schedules one frame per batch, not one per
    /// change.
    pub fn new(waker: impl Fn() + Send + Sync + 'static) -> Self {
        StoreHub { inner: Mutex::default(), waker: Box::new(waker) }
    }

    /// Applies `change` to the store. A change that changes nothing bumps no
    /// revision and sends no diff. Returns whether the store changed.
    pub fn write<S: Store>(&self, change: S::Change) -> bool {
        let mut inner = self.lock();
        if !S::slot_mut(&mut inner.stores).write(&change) {
            return false;
        }
        if !inner.subscribed[S::KIND as usize] {
            return true;
        }
        let was_empty = inner.pending.is_empty();
        let revision = S::slot(&inner.stores).revision();
        match S::pending(&mut inner.pending) {
            Some(diff) => {
                S::coalesce(&mut diff.change, change);
                diff.revision = revision;
            }
            empty => *empty = Some(Diff { revision, change }),
        }
        drop(inner);
        if was_empty {
            (self.waker)();
        }
        true
    }

    /// Starts sending diffs for the store, and returns its current state.
    /// Applying every later diff to it reproduces the core's state.
    pub fn subscribe<S: Store>(&self) -> Versioned<S> {
        let mut inner = self.lock();
        inner.subscribed[S::KIND as usize] = true;
        Self::take_snapshot(&mut inner)
    }

    pub fn unsubscribe<S: Store>(&self) {
        let mut inner = self.lock();
        inner.subscribed[S::KIND as usize] = false;
        inner.pending.clear(S::KIND);
    }

    /// The store's current state. Its pending diff is dropped, since the
    /// snapshot already holds it.
    pub fn snapshot<S: Store>(&self) -> Versioned<S> {
        Self::take_snapshot(&mut self.lock())
    }

    /// Reads a store on a core thread, for example to feed a [`Memo`](super::Memo).
    pub fn read<S: Store, R>(&self, read: impl FnOnce(&Versioned<S>) -> R) -> R {
        read(S::slot(&self.lock().stores))
    }

    /// Takes the changes since the last drain, if any. A shell calls this on
    /// its frame tick.
    pub fn drain(&self) -> Option<Batch> {
        let batch = std::mem::take(&mut self.lock().pending);
        (!batch.is_empty()).then_some(batch)
    }

    fn take_snapshot<S: Store>(inner: &mut Inner) -> Versioned<S> {
        inner.pending.clear(S::KIND);
        S::slot(&inner.stores).clone()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A writer that panicked left at most one store mid-change; the
        // stores are still safe to read and write.
        self.inner.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Fields, Revision, StoreState, TerminalChange, TerminalLine, TerminalState, VitalsState};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn lines(texts: &[&str]) -> TerminalChange {
        TerminalChange::Append(texts.iter().map(|text| TerminalLine { text: (*text).to_owned() }).collect())
    }

    fn vitals(hp: &str) -> VitalsState {
        VitalsState(Fields::from([("hp", hp), ("maxhp", "100")]))
    }

    fn counting_hub() -> (StoreHub, Arc<AtomicUsize>) {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wakes);
        let hub = StoreHub::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        });
        (hub, wakes)
    }

    #[test]
    fn a_no_op_write_does_not_bump_the_revision() {
        let (hub, wakes) = counting_hub();
        hub.subscribe::<VitalsState>();

        assert!(hub.write::<VitalsState>(vitals("90")));
        hub.drain();
        assert!(!hub.write::<VitalsState>(vitals("90")));
        assert!(!hub.write::<TerminalState>(lines(&[])));

        assert_eq!(hub.read(|v: &Versioned<VitalsState>| v.revision()), Revision(1));
        assert_eq!(hub.read(|v: &Versioned<TerminalState>| v.revision()), Revision(0));
        assert!(hub.drain().is_none());
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn diffs_coalesce_within_a_frame() {
        let (hub, wakes) = counting_hub();
        hub.subscribe::<VitalsState>();
        hub.subscribe::<TerminalState>();

        hub.write::<TerminalState>(lines(&["one"]));
        hub.write::<VitalsState>(vitals("90"));
        hub.write::<TerminalState>(lines(&["two", "three"]));
        hub.write::<VitalsState>(vitals("80"));

        let batch = hub.drain().expect("a frame of changes");
        assert_eq!(batch.kinds(), [StoreKind::Terminal, StoreKind::Vitals]);
        let terminal = batch.terminal.expect("the terminal changed");
        assert_eq!(terminal.revision, Revision(2));
        assert_eq!(terminal.change, lines(&["one", "two", "three"]));
        let vitals_diff = batch.vitals.expect("the vitals changed");
        assert_eq!((vitals_diff.revision, vitals_diff.change), (Revision(2), vitals("80")));
        assert_eq!(wakes.load(Ordering::Relaxed), 1);

        assert!(hub.drain().is_none());
        hub.write::<VitalsState>(vitals("70"));
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn unsubscribed_stores_send_no_diffs() {
        let (hub, wakes) = counting_hub();
        hub.write::<VitalsState>(vitals("90"));
        hub.subscribe::<VitalsState>();
        hub.write::<VitalsState>(vitals("80"));
        hub.unsubscribe::<VitalsState>();
        hub.write::<VitalsState>(vitals("70"));

        assert!(hub.drain().is_none());
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(hub.read(|v: &Versioned<VitalsState>| v.revision()), Revision(3));
    }

    #[test]
    fn a_snapshot_plus_later_diffs_equals_the_state() {
        let (hub, _) = counting_hub();
        hub.write::<TerminalState>(lines(&["before subscribing"]));
        hub.write::<VitalsState>(vitals("100"));

        let mut shell = Stores {
            terminal: hub.subscribe::<TerminalState>(),
            vitals: hub.subscribe::<VitalsState>(),
            ..Stores::default()
        };
        for frame in 0..5 {
            hub.write::<TerminalState>(lines(&["a line", "and another"]));
            hub.write::<VitalsState>(vitals(&(90 - frame).to_string()));
            hub.write::<VitalsState>(vitals("90"));
            if frame == 2 {
                // A shell re-reading one store mid-frame mustn't double-apply.
                shell.terminal = hub.snapshot::<TerminalState>();
            }
            if let Some(batch) = hub.drain() {
                shell.apply_batch(&batch);
            }
        }

        hub.read(|core: &Versioned<TerminalState>| assert_eq!(*core, shell.terminal));
        hub.read(|core: &Versioned<VitalsState>| assert_eq!(*core, shell.vitals));
        assert_eq!(shell.terminal.value().lines().len(), 11);
    }

    #[test]
    fn the_terminal_store_keeps_its_newest_lines() {
        let mut terminal = TerminalState::default();
        let many: Vec<String> = (0..crate::store::TERMINAL_CAPACITY + 3).map(|n| n.to_string()).collect();
        let many: Vec<&str> = many.iter().map(String::as_str).collect();
        terminal.apply(&lines(&many));
        terminal.apply(&lines(&["newest"]));

        let held = terminal.lines();
        assert_eq!(held.len(), crate::store::TERMINAL_CAPACITY);
        assert_eq!(held.front().unwrap().text, "4");
        assert_eq!(held.back().unwrap().text, "newest");
    }
}
