// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

#[derive(Clone, Debug)]
pub(crate) struct WeightedSet<T> {
    keys: Vec<T>,
    live: Vec<bool>,
    weights: Vec<u64>,
    tree: Vec<u64>,
    total: u64,
    len: usize,
}

impl<T> Default for WeightedSet<T> {
    fn default() -> Self {
        Self {
            keys: Vec::new(),
            live: Vec::new(),
            weights: Vec::new(),
            tree: Vec::new(),
            total: 0,
            len: 0,
        }
    }
}

fn lowest_bit(index: usize) -> usize {
    index & index.wrapping_neg()
}

impl<T: Copy + Ord> WeightedSet<T> {
    pub(crate) fn from_sorted(entries: impl IntoIterator<Item = (T, u64)>) -> Self {
        let (keys, weights): (Vec<T>, Vec<u64>) = entries.into_iter().unzip();
        debug_assert!(keys.windows(2).all(|pair| pair[0] < pair[1]));
        let len = keys.len();
        let mut set = Self {
            keys,
            live: vec![true; len],
            weights,
            tree: Vec::new(),
            total: 0,
            len,
        };
        set.rebuild_tree();
        set
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = T> + '_ {
        self.keys
            .iter()
            .zip(&self.live)
            .filter_map(|(key, live)| live.then_some(*key))
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &T) -> bool {
        self.position(key).is_some()
    }

    pub(crate) fn weight(&self, key: &T) -> Option<u64> {
        self.position(key).map(|position| self.weights[position])
    }

    fn position(&self, key: &T) -> Option<usize> {
        self.keys
            .binary_search(key)
            .ok()
            .filter(|position| self.live[*position])
    }

    pub(crate) fn insert(&mut self, key: T, weight: u64) -> bool {
        match self.keys.binary_search(&key) {
            Ok(position) if self.live[position] => false,
            Ok(position) => {
                self.live[position] = true;
                self.len += 1;
                self.set_at(position, weight);
                true
            }
            Err(position) if position == self.keys.len() => {
                let index = position + 1;
                let covered = self
                    .prefix(index - 1)
                    .wrapping_sub(self.prefix(index - lowest_bit(index)));
                self.keys.push(key);
                self.live.push(true);
                self.weights.push(weight);
                self.tree.push(covered.wrapping_add(weight));
                self.total = self.total.wrapping_add(weight);
                self.len += 1;
                true
            }
            Err(position) => {
                self.keys.insert(position, key);
                self.live.insert(position, true);
                self.weights.insert(position, weight);
                self.len += 1;
                self.rebuild_tree();
                true
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &T) -> bool {
        let Some(position) = self.position(key) else {
            return false;
        };
        self.set_at(position, 0);
        self.live[position] = false;
        self.len -= 1;
        if self.keys.len() > 2 * self.len {
            self.drop_removed();
        }
        true
    }

    pub(crate) fn set_weight(&mut self, key: &T, weight: u64) -> bool {
        let Some(position) = self.position(key) else {
            return false;
        };
        self.set_at(position, weight);
        true
    }

    pub(crate) fn find(&self, draw: u64) -> Option<T> {
        let mut position = 0;
        let mut remaining = draw;
        let mut step = self
            .tree
            .len()
            .checked_ilog2()
            .map_or(0, |bits| 1_usize << bits);
        while step > 0 {
            let next = position + step;
            if next <= self.tree.len() && self.tree[next - 1] <= remaining {
                position = next;
                remaining -= self.tree[next - 1];
            }
            step >>= 1;
        }
        self.keys.get(position).copied()
    }

    fn prefix(&self, mut index: usize) -> u64 {
        let mut sum = 0_u64;
        while index > 0 {
            sum = sum.wrapping_add(self.tree[index - 1]);
            index -= lowest_bit(index);
        }
        sum
    }

    fn set_at(&mut self, position: usize, weight: u64) {
        let delta = weight.wrapping_sub(self.weights[position]);
        self.weights[position] = weight;
        self.total = self.total.wrapping_add(delta);
        let mut index = position + 1;
        while index <= self.tree.len() {
            self.tree[index - 1] = self.tree[index - 1].wrapping_add(delta);
            index += lowest_bit(index);
        }
    }

    fn drop_removed(&mut self) {
        let mut kept = 0;
        for position in 0..self.keys.len() {
            if self.live[position] {
                self.keys[kept] = self.keys[position];
                self.weights[kept] = self.weights[position];
                kept += 1;
            }
        }
        self.keys.truncate(kept);
        self.weights.truncate(kept);
        self.live.clear();
        self.live.resize(kept, true);
        self.rebuild_tree();
    }

    fn rebuild_tree(&mut self) {
        self.tree.clone_from(&self.weights);
        for index in 1..=self.tree.len() {
            let parent = index + lowest_bit(index);
            if parent <= self.tree.len() {
                self.tree[parent - 1] = self.tree[parent - 1].wrapping_add(self.tree[index - 1]);
            }
        }
        self.total = self
            .weights
            .iter()
            .fold(0_u64, |sum, weight| sum.wrapping_add(*weight));
    }
}

impl<T: Copy + Ord + Serialize> Serialize for WeightedSet<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(Some(self.len))?;
        for key in self.iter() {
            sequence.serialize_element(&key)?;
        }
        sequence.end()
    }
}

impl<'de, T: Copy + Ord + Deserialize<'de>> Deserialize<'de> for WeightedSet<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let keys = BTreeSet::<T>::deserialize(deserializer)?;
        Ok(Self::from_sorted(keys.into_iter().map(|key| (key, 0))))
    }
}

#[cfg(test)]
mod tests {
    use super::WeightedSet;
    use crate::search::rand::RomuDuoJrRand;
    use std::{
        collections::{BTreeMap, BTreeSet},
        num::NonZeroUsize,
    };

    fn linear_find(reference: &BTreeMap<u32, u64>, mut draw: u64) -> Option<u32> {
        for (key, weight) in reference {
            if draw < *weight {
                return Some(*key);
            }
            draw -= weight;
        }
        None
    }

    fn assert_matches(set: &WeightedSet<u32>, reference: &BTreeMap<u32, u64>) {
        assert_eq!(set.len(), reference.len());
        assert_eq!(
            set.iter().collect::<Vec<_>>(),
            reference.keys().copied().collect::<Vec<_>>()
        );
        assert_eq!(
            set.iter().rev().collect::<Vec<_>>(),
            reference.keys().rev().copied().collect::<Vec<_>>()
        );
        let total = reference.values().sum::<u64>();
        assert_eq!(set.total(), total);
        for draw in [
            0,
            1,
            total / 3,
            total / 2,
            total.saturating_sub(1),
            total,
            total + 1,
        ] {
            assert_eq!(set.find(draw), linear_find(reference, draw), "draw {draw}");
        }
        let mut cumulative = 0;
        for weight in reference.values() {
            if *weight > 0 {
                assert_eq!(set.find(cumulative), linear_find(reference, cumulative));
                assert_eq!(
                    set.find(cumulative + weight - 1),
                    linear_find(reference, cumulative + weight - 1)
                );
            }
            cumulative += weight;
        }
    }

    #[test]
    fn random_updates_match_a_linear_prefix_scan() {
        for seed in 0..64 {
            let mut rand = RomuDuoJrRand::with_seed(seed);
            let mut set = WeightedSet::default();
            let mut reference = BTreeMap::new();
            let span = NonZeroUsize::new(if seed % 2 == 0 { 64 } else { 4096 }).unwrap();
            for step in 0..2_000 {
                let key = u32::try_from(rand.below(span)).unwrap();
                let weight =
                    u64::try_from(rand.below(NonZeroUsize::new(1 << 20).unwrap())).unwrap() + 1;
                match rand.below(NonZeroUsize::new(4).unwrap()) {
                    0 => {
                        let appended = reference.keys().next_back().map_or(0, |last| last + 1);
                        assert!(set.insert(appended, weight));
                        reference.insert(appended, weight);
                    }
                    1 => {
                        let inserted = !reference.contains_key(&key);
                        assert_eq!(set.insert(key, weight), inserted);
                        reference.entry(key).or_insert(weight);
                    }
                    2 => assert_eq!(set.remove(&key), reference.remove(&key).is_some()),
                    _ => {
                        let present = reference.contains_key(&key);
                        assert_eq!(set.set_weight(&key, weight), present);
                        if present {
                            reference.insert(key, weight);
                        }
                    }
                }
                assert_eq!(set.contains(&key), reference.contains_key(&key));
                assert_eq!(set.weight(&key), reference.get(&key).copied());
                if step % 37 == 0 {
                    assert_matches(&set, &reference);
                }
            }
            assert_matches(&set, &reference);
        }
    }

    #[test]
    fn a_sorted_build_matches_one_insert_at_a_time() {
        let entries = (0_u32..1000)
            .map(|key| (key * 3, u64::from(key % 17) + 1))
            .collect::<Vec<_>>();
        let built = WeightedSet::from_sorted(entries.iter().copied());
        let mut inserted = WeightedSet::default();
        let reference = entries.iter().copied().collect::<BTreeMap<_, _>>();
        for (key, weight) in entries.iter().rev() {
            inserted.insert(*key, *weight);
        }
        assert_matches(&built, &reference);
        assert_matches(&inserted, &reference);
    }

    #[test]
    fn removing_every_key_leaves_an_empty_set() {
        let mut set = WeightedSet::default();
        for key in 0_u32..100 {
            set.insert(key, u64::from(key) + 1);
        }
        for key in (0_u32..100).rev() {
            assert!(set.remove(&key));
        }
        assert!(set.is_empty());
        assert_eq!(set.total(), 0);
        assert_eq!(set.find(0), None);
        assert!(set.insert(7, 3));
        assert_eq!(set.find(2), Some(7));
        assert_eq!(set.find(3), None);
    }

    #[test]
    fn serializes_as_the_set_of_its_keys() {
        let mut set = WeightedSet::default();
        for key in [5_u32, 1, 9, 3] {
            set.insert(key, 2);
        }
        set.remove(&9);
        let bytes = postcard::to_stdvec(&set).unwrap();
        assert_eq!(
            bytes,
            postcard::to_stdvec(&[1_u32, 3, 5].into_iter().collect::<BTreeSet<_>>()).unwrap()
        );
        let restored: WeightedSet<u32> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.iter().collect::<Vec<_>>(), vec![1, 3, 5]);
        assert_eq!(restored.total(), 0);
    }
}
