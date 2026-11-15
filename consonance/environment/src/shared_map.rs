// SPDX-License-Identifier: AGPL-3.0-or-later
use std::{
    cmp::Ordering,
    collections::{BTreeMap, btree_map},
    fmt,
    iter::Peekable,
    sync::Arc,
};

const MIN_RECENT: usize = 64;

#[derive(Clone)]
pub(crate) struct SharedMap<K, V> {
    base: Arc<BTreeMap<K, V>>,
    recent: BTreeMap<K, V>,
    shadowed: usize,
}

impl<K, V> Default for SharedMap<K, V> {
    fn default() -> Self {
        Self {
            base: Arc::new(BTreeMap::new()),
            recent: BTreeMap::new(),
            shadowed: 0,
        }
    }
}

impl<K: Ord + Clone, V: Clone> SharedMap<K, V> {
    pub(crate) fn len(&self) -> usize {
        self.base.len() + self.recent.len() - self.shadowed
    }

    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.recent.get(key).or_else(|| self.base.get(key))
    }

    pub(crate) fn insert(&mut self, key: K, value: V) -> Option<V> {
        let in_base = self.base.get(&key);
        let previous = match self.recent.insert(key, value) {
            Some(previous) => Some(previous),
            None => {
                if in_base.is_some() {
                    self.shadowed += 1;
                }
                in_base.cloned()
            }
        };
        if self.recent.len() > MIN_RECENT.max(self.base.len().isqrt()) {
            let base = Arc::make_mut(&mut self.base);
            for (key, value) in std::mem::take(&mut self.recent) {
                base.insert(key, value);
            }
            self.shadowed = 0;
        }
        previous
    }

    pub(crate) fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            base: self.base.iter().peekable(),
            recent: self.recent.iter().peekable(),
        }
    }
}

pub(crate) struct Iter<'a, K, V> {
    base: Peekable<btree_map::Iter<'a, K, V>>,
    recent: Peekable<btree_map::Iter<'a, K, V>>,
}

impl<'a, K: Ord, V> Iterator for Iter<'a, K, V> {
    type Item = (&'a K, &'a V);

    fn next(&mut self) -> Option<Self::Item> {
        let order = match (self.base.peek(), self.recent.peek()) {
            (Some((base, _)), Some((recent, _))) => base.cmp(recent),
            (Some(_), None) => Ordering::Less,
            (None, _) => Ordering::Greater,
        };
        match order {
            Ordering::Less => self.base.next(),
            Ordering::Greater => self.recent.next(),
            Ordering::Equal => {
                self.base.next();
                self.recent.next()
            }
        }
    }
}

impl<'a, K: Ord + Clone, V: Clone> IntoIterator for &'a SharedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<K: Ord + Clone, V: Clone + PartialEq> PartialEq for SharedMap<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl<K: Ord + Clone, V: Clone + Eq> Eq for SharedMap<K, V> {}

impl<K: Ord + Clone + fmt::Debug, V: Clone + fmt::Debug> fmt::Debug for SharedMap<K, V> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn next(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    #[test]
    fn matches_a_btree_map_through_inserts_clones_and_merges() {
        let mut state = 0x5eed_u64;
        let mut model = BTreeMap::new();
        let mut map = SharedMap::default();
        let mut held = Vec::new();
        for step in 0..20_000_u64 {
            let key = next(&mut state) % 6_000;
            let value = next(&mut state);
            assert_eq!(map.insert(key, value), model.insert(key, value));
            if step % 97 == 0 {
                held.push((map.clone(), model.clone()));
            }
            if step % 1_013 == 0 {
                assert_eq!(map.len(), model.len());
                assert!(map.iter().eq(model.iter()));
                assert_eq!(map.get(&key), model.get(&key));
                assert_eq!(map.get(&6_001), None);
            }
        }
        assert_eq!(map.len(), model.len());
        assert!(map.iter().eq(model.iter()));
        for (copy, expected) in &held {
            assert_eq!(copy.len(), expected.len());
            assert!(copy.iter().eq(expected.iter()));
        }
    }

    #[test]
    fn equality_and_debug_ignore_how_entries_are_split() {
        let mut ascending = SharedMap::default();
        let mut descending = SharedMap::default();
        let mut model = BTreeMap::new();
        for key in 0..500_u64 {
            ascending.insert(key, key);
            model.insert(key, key);
        }
        for key in (0..500_u64).rev() {
            descending.insert(key, key);
        }
        ascending.insert(7, 70);
        descending.insert(7, 70);
        model.insert(7, 70);
        assert_eq!(ascending, descending);
        assert_eq!(format!("{ascending:?}"), format!("{model:?}"));
        descending.insert(8, 0);
        assert_ne!(ascending, descending);
    }
}
