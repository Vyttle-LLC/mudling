//! Derived data cached against the revisions of its inputs (PRD §5 #7).

/// Holds one derived value and the input revisions it was computed from.
///
/// The key is usually a tuple of [`Revision`](super::Revision)s, one per
/// input store, so a Find list or a gear plan recomputes only when an input
/// really changed.
#[derive(Debug)]
pub struct Memo<K, V> {
    cached: Option<(K, V)>,
}

impl<K, V> Default for Memo<K, V> {
    fn default() -> Self {
        Memo { cached: None }
    }
}

impl<K: PartialEq, V> Memo<K, V> {
    /// The value for `key`, computing it only if `key` differs from the
    /// last one.
    pub fn get(&mut self, key: K, compute: impl FnOnce() -> V) -> &V {
        if self.cached.as_ref().is_none_or(|(cached_key, _)| *cached_key != key) {
            self.cached = Some((key, compute()));
        }
        &self.cached.as_ref().expect("the value was just cached").1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Revision;

    #[test]
    fn recomputes_only_when_an_input_revision_changes() {
        let mut memo = Memo::default();
        let mut computed = 0;
        let mut get = |memo: &mut Memo<_, _>, key: (Revision, Revision)| {
            *memo.get(key, || {
                computed += 1;
                key.0.0 + key.1.0
            })
        };

        assert_eq!(get(&mut memo, (Revision(1), Revision(1))), 2);
        assert_eq!(get(&mut memo, (Revision(1), Revision(1))), 2);
        assert_eq!(get(&mut memo, (Revision(1), Revision(2))), 3);
        assert_eq!(get(&mut memo, (Revision(1), Revision(2))), 3);
        assert_eq!(get(&mut memo, (Revision(3), Revision(2))), 5);
        assert_eq!(computed, 3);
    }
}
