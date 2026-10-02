// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{cmp::Ordering, collections::BTreeSet};

use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeSeq};

use super::rand::splitmix64;

const NONE: usize = usize::MAX;

#[derive(Clone, Debug)]
struct Node<T> {
    key: T,
    weight: u64,
    left_sum: u64,
    sum: u64,
    left: usize,
    right: usize,
}

#[derive(Clone, Debug)]
pub(crate) struct WeightedSet<T> {
    nodes: Vec<Node<T>>,
    free: Vec<usize>,
    root: usize,
    len: usize,
}

impl<T> Default for WeightedSet<T> {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            free: Vec::new(),
            root: NONE,
            len: 0,
        }
    }
}

fn priority(slot: usize) -> u64 {
    let mut state = slot as u64;
    splitmix64(&mut state)
}

impl<T: Copy + Ord> WeightedSet<T> {
    pub(crate) fn from_sorted(entries: impl IntoIterator<Item = (T, u64)>) -> Self {
        let mut set = Self::default();
        let mut spine = Vec::new();
        for (key, weight) in entries {
            let slot = set.nodes.len();
            debug_assert!(slot == 0 || set.nodes[slot - 1].key < key);
            set.nodes.push(Node {
                key,
                weight,
                left_sum: 0,
                sum: weight,
                left: NONE,
                right: NONE,
            });
            let mut below = NONE;
            while let Some(&top) = spine.last() {
                if priority(top) >= priority(slot) {
                    break;
                }
                spine.pop();
                set.update(top);
                below = top;
            }
            set.nodes[slot].left = below;
            if let Some(&top) = spine.last() {
                set.nodes[top].right = slot;
            }
            spine.push(slot);
        }
        while let Some(top) = spine.pop() {
            set.update(top);
            set.root = top;
        }
        set.len = set.nodes.len();
        set
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn total(&self) -> u64 {
        self.sum(self.root)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = T> + '_ {
        let mut path = Vec::new();
        let mut node = self.root;
        std::iter::from_fn(move || {
            while node != NONE {
                path.push(node);
                node = self.nodes[node].left;
            }
            let top = path.pop()?;
            node = self.nodes[top].right;
            Some(self.nodes[top].key)
        })
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, key: &T) -> bool {
        self.position(key).is_some()
    }

    pub(crate) fn weight(&self, key: &T) -> Option<u64> {
        self.position(key).map(|node| self.nodes[node].weight)
    }

    fn position(&self, key: &T) -> Option<usize> {
        let mut node = self.root;
        while node != NONE {
            node = match key.cmp(&self.nodes[node].key) {
                Ordering::Less => self.nodes[node].left,
                Ordering::Greater => self.nodes[node].right,
                Ordering::Equal => return Some(node),
            };
        }
        None
    }

    pub(crate) fn insert(&mut self, key: T, weight: u64) -> bool {
        if self.position(&key).is_some() {
            return false;
        }
        let node = Node {
            key,
            weight,
            left_sum: 0,
            sum: weight,
            left: NONE,
            right: NONE,
        };
        let slot = match self.free.pop() {
            Some(slot) => {
                self.nodes[slot] = node;
                slot
            }
            None => {
                self.nodes.push(node);
                self.nodes.len() - 1
            }
        };
        self.root = self.insert_at(self.root, slot);
        self.len += 1;
        true
    }

    pub(crate) fn remove(&mut self, key: &T) -> bool {
        let (root, removed) = self.remove_at(self.root, key);
        self.root = root;
        let Some(slot) = removed else {
            return false;
        };
        self.free.push(slot);
        self.len -= 1;
        true
    }

    pub(crate) fn set_weight(&mut self, key: &T, weight: u64) -> bool {
        let Some(target) = self.position(key) else {
            return false;
        };
        let delta = weight.wrapping_sub(self.nodes[target].weight);
        self.nodes[target].weight = weight;
        let mut node = self.root;
        while node != NONE {
            let current = &mut self.nodes[node];
            current.sum = current.sum.wrapping_add(delta);
            node = match key.cmp(&current.key) {
                Ordering::Less => {
                    current.left_sum = current.left_sum.wrapping_add(delta);
                    current.left
                }
                Ordering::Greater => current.right,
                Ordering::Equal => NONE,
            };
        }
        true
    }

    pub(crate) fn find(&self, draw: u64) -> Option<T> {
        let mut remaining = draw;
        let mut node = self.root;
        while node != NONE {
            let current = &self.nodes[node];
            if remaining < current.left_sum {
                node = current.left;
                continue;
            }
            remaining -= current.left_sum;
            if remaining < current.weight {
                return Some(current.key);
            }
            remaining -= current.weight;
            node = current.right;
        }
        None
    }

    fn sum(&self, node: usize) -> u64 {
        if node == NONE {
            0
        } else {
            self.nodes[node].sum
        }
    }

    fn update(&mut self, node: usize) {
        let Node {
            weight,
            left,
            right,
            ..
        } = self.nodes[node];
        let left_sum = self.sum(left);
        let sum = left_sum.wrapping_add(weight).wrapping_add(self.sum(right));
        let current = &mut self.nodes[node];
        current.left_sum = left_sum;
        current.sum = sum;
    }

    fn insert_at(&mut self, node: usize, slot: usize) -> usize {
        if node == NONE {
            return slot;
        }
        if priority(slot) > priority(node) {
            let (left, right) = self.split(node, self.nodes[slot].key);
            self.nodes[slot].left = left;
            self.nodes[slot].right = right;
            self.update(slot);
            return slot;
        }
        if self.nodes[slot].key < self.nodes[node].key {
            let left = self.insert_at(self.nodes[node].left, slot);
            self.nodes[node].left = left;
        } else {
            let right = self.insert_at(self.nodes[node].right, slot);
            self.nodes[node].right = right;
        }
        self.update(node);
        node
    }

    fn split(&mut self, node: usize, key: T) -> (usize, usize) {
        if node == NONE {
            return (NONE, NONE);
        }
        if self.nodes[node].key < key {
            let (left, right) = self.split(self.nodes[node].right, key);
            self.nodes[node].right = left;
            self.update(node);
            (node, right)
        } else {
            let (left, right) = self.split(self.nodes[node].left, key);
            self.nodes[node].left = right;
            self.update(node);
            (left, node)
        }
    }

    fn merge(&mut self, left: usize, right: usize) -> usize {
        if left == NONE {
            return right;
        }
        if right == NONE {
            return left;
        }
        if priority(left) > priority(right) {
            let merged = self.merge(self.nodes[left].right, right);
            self.nodes[left].right = merged;
            self.update(left);
            left
        } else {
            let merged = self.merge(left, self.nodes[right].left);
            self.nodes[right].left = merged;
            self.update(right);
            right
        }
    }

    fn remove_at(&mut self, node: usize, key: &T) -> (usize, Option<usize>) {
        if node == NONE {
            return (NONE, None);
        }
        match key.cmp(&self.nodes[node].key) {
            Ordering::Less => {
                let (left, removed) = self.remove_at(self.nodes[node].left, key);
                self.nodes[node].left = left;
                if removed.is_some() {
                    self.update(node);
                }
                (node, removed)
            }
            Ordering::Greater => {
                let (right, removed) = self.remove_at(self.nodes[node].right, key);
                self.nodes[node].right = right;
                if removed.is_some() {
                    self.update(node);
                }
                (node, removed)
            }
            Ordering::Equal => {
                let merged = self.merge(self.nodes[node].left, self.nodes[node].right);
                (merged, Some(node))
            }
        }
    }

    #[cfg(test)]
    fn depth(&self) -> usize {
        let mut deepest = 0;
        let mut pending = vec![(self.root, 0)];
        while let Some((node, depth)) = pending.pop() {
            if node == NONE {
                continue;
            }
            deepest = deepest.max(depth + 1);
            pending.push((self.nodes[node].left, depth + 1));
            pending.push((self.nodes[node].right, depth + 1));
        }
        deepest
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
    fn descending_and_ascending_inserts_keep_the_tree_shallow() {
        let count = 100_000_u32;
        let bound = 4 * usize::try_from(count.ilog2()).unwrap() + 8;
        let mut descending = WeightedSet::default();
        let mut ascending = WeightedSet::default();
        for key in (0..count).rev() {
            descending.insert(key, u64::from(key % 5));
            ascending.insert(count - 1 - key, 1);
        }
        for key in (0..count).step_by(2) {
            assert!(descending.remove(&key));
        }
        let built = WeightedSet::from_sorted((0..count).map(|key| (key, 1)));
        for set in [&descending, &ascending, &built] {
            assert!(set.depth() <= bound, "depth {}", set.depth());
        }
        let odd = (0..count).filter(|key| key % 2 == 1).collect::<Vec<_>>();
        assert_eq!(descending.iter().collect::<Vec<_>>(), odd);
        let mut cumulative = 0;
        for key in odd {
            let weight = u64::from(key % 5);
            if weight > 0 {
                assert_eq!(descending.find(cumulative), Some(key));
                assert_eq!(descending.find(cumulative + weight - 1), Some(key));
            }
            cumulative += weight;
        }
        assert_eq!(descending.total(), cumulative);
        assert_eq!(descending.find(cumulative), None);
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
