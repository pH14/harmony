// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

const MAX_HINTS: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LocalityKey(pub [u8; 32]);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkerLocality(BTreeMap<LocalityKey, u8>);

impl WorkerLocality {
    pub fn new(hints: impl IntoIterator<Item = (LocalityKey, u8)>) -> Self {
        let mut keys = BTreeMap::<LocalityKey, u8>::new();
        for (key, rank) in hints.into_iter().take(MAX_HINTS) {
            keys.entry(key)
                .and_modify(|existing| *existing = (*existing).max(rank))
                .or_insert(rank);
        }
        Self(keys)
    }

    pub(crate) fn rank(&self, key: Option<LocalityKey>) -> Option<u8> {
        key.and_then(|key| self.0.get(&key).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hints_are_bounded_and_duplicate_keys_keep_the_best_rank() {
        let key = LocalityKey([7; 32]);
        let hints = WorkerLocality::new([(key, 0), (key, 2), (key, 1)]);
        assert_eq!(hints.rank(Some(key)), Some(2));
        assert_eq!(hints.rank(None), None);
        assert_eq!(hints.rank(Some(LocalityKey([8; 32]))), None);
        let hints = WorkerLocality::new((0..=64).map(|value| (LocalityKey([value; 32]), value)));
        assert_eq!(hints.0.len(), MAX_HINTS);
        assert_eq!(hints.rank(Some(LocalityKey([63; 32]))), Some(63));
        assert_eq!(hints.rank(Some(LocalityKey([64; 32]))), None);
    }
}
