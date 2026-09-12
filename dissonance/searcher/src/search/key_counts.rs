// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded selection history independent of archive entry lifetimes.
use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};

pub(crate) const KEY_COUNT_CAPACITY: usize = 16_384;

pub(crate) struct KeyCounts<K: Copy + Ord, const CAP: usize = KEY_COUNT_CAPACITY> {
    counts: BTreeMap<K, (u64, u64)>,
    recency: BTreeSet<(u64, K)>,
    clock: u64,
    pub hits: u64,
    pub evictions: u64,
}

impl<K: Copy + Ord, const CAP: usize> Default for KeyCounts<K, CAP> {
    fn default() -> Self {
        assert!(CAP > 0);
        Self {
            counts: BTreeMap::new(),
            recency: BTreeSet::new(),
            clock: 0,
            hits: 0,
            evictions: 0,
        }
    }
}

impl<K: Copy + Ord, const CAP: usize> KeyCounts<K, CAP> {
    pub fn get(&self, key: K) -> u64 {
        self.counts.get(&key).map_or(0, |(count, _)| *count)
    }

    /// Record one selection, including a recorded skip. The entry's incremented
    /// count is a floor if a cache eviction forgot earlier selections.
    pub fn record(&mut self, key: K, entry_count: u64) {
        self.clock = self.clock.saturating_add(1);
        let count = if let Some((count, stamp)) = self.counts.get(&key).copied() {
            self.recency.remove(&(stamp, key));
            self.hits = self.hits.saturating_add(1);
            count.saturating_add(1).max(entry_count)
        } else {
            if self.counts.len() == CAP {
                let (_, old) = self.recency.pop_first().expect("full counts have recency");
                self.counts.remove(&old);
                self.evictions = self.evictions.saturating_add(1);
            }
            entry_count.max(1)
        };
        self.counts.insert(key, (count, self.clock));
        self.recency.insert((self.clock, key));
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    /// Conservative fixed charge for both ordered indexes, including node
    /// slack. No host allocation timing enters budget enforcement or replay.
    pub fn reserve_bytes() -> usize {
        CAP.saturating_mul(2 * size_of::<K>() + 4 * size_of::<u64>() + 256)
    }
}

#[cfg(test)]
mod tests {
    use super::KeyCounts;

    #[test]
    fn replacement_and_eviction_do_not_erase_recent_key_selections() {
        let mut counts = KeyCounts::<u8, 2>::default();
        counts.record(10, 1);
        counts.record(20, 1);
        counts.record(10, 1);
        assert_eq!(counts.get(10), 2);
        counts.record(30, 1);
        assert_eq!(counts.get(10), 2);
        assert_eq!(counts.get(20), 0);
        assert_eq!(counts.evictions, 1);
        assert_eq!(counts.hits, 1);
        counts.record(20, 50);
        assert_eq!(counts.get(20), 50);
        for i in 0..1000 {
            counts.record((i % 256) as u8, 1);
        }
        assert_eq!(counts.len(), 2);
        assert_eq!(counts.recency.len(), 2);
    }
}
