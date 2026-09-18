// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
};

const EDGE_NODE_OVERHEAD: usize = 192;

const PENDING_NODE_OVERHEAD: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation<K, A> {
    pub parent: u64,
    pub donor: u64,
    pub leaf: u64,
    pub destination: K,
    pub wave: u32,
    pub tier: u8,
    pub actions: Vec<A>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Edge<A> {
    donor: u64,
    leaf: u64,
    cost: u64,
    actions: Vec<A>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pending<K> {
    sequence: u64,
    tier: u8,
    parent: u64,
    wave: u32,
    cursor: Option<K>,
}

pub(crate) struct ContinuationBank<K: Ord, A> {
    action_cap: usize,
    edges: BTreeMap<(K, K), Edge<A>>,
    exits: BTreeMap<K, BTreeSet<K>>,
    entrances: BTreeMap<K, BTreeSet<K>>,
    pending: BTreeMap<(u8, u64), K>,
    pending_slot: BTreeMap<K, Pending<K>>,
    next_sequence: u64,
    memory_bytes: usize,
    improved_work: u64,
}

impl<K: Copy + Ord, A: Clone> ContinuationBank<K, A> {
    pub fn new(action_cap: usize) -> Self {
        Self {
            action_cap,
            edges: BTreeMap::new(),
            exits: BTreeMap::new(),
            entrances: BTreeMap::new(),
            pending: BTreeMap::new(),
            pending_slot: BTreeMap::new(),
            next_sequence: 0,
            memory_bytes: 0,
            improved_work: 0,
        }
    }

    fn edge_bytes(actions: usize) -> usize {
        size_of::<(K, K)>()
            .saturating_add(size_of::<Edge<A>>())
            .saturating_add(actions.saturating_mul(size_of::<A>()))
            .saturating_add(2 * size_of::<K>())
            .saturating_add(EDGE_NODE_OVERHEAD)
    }

    fn pending_bytes() -> usize {
        size_of::<u64>()
            .saturating_add(2 * size_of::<K>())
            .saturating_add(size_of::<Pending<K>>())
            .saturating_add(PENDING_NODE_OVERHEAD)
    }

    pub fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_slot.len()
    }

    #[cfg(test)]
    pub fn queue_len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub fn improved_work(&self) -> u64 {
        self.improved_work
    }

    pub fn record(&mut self, from: K, to: K, donor: u64, leaf: u64, actions: &[A], cost: u64) {
        if from == to || actions.is_empty() || actions.len() > self.action_cap {
            return;
        }
        let edge = Edge {
            donor,
            leaf,
            cost,
            actions: actions.to_vec(),
        };
        match self.edges.get_mut(&(from, to)) {
            Some(held) => {
                if cost >= held.cost {
                    return;
                }
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_sub(Self::edge_bytes(held.actions.len()))
                    .saturating_add(Self::edge_bytes(actions.len()));
                *held = edge;
            }
            None => {
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_add(Self::edge_bytes(actions.len()));
                self.edges.insert((from, to), edge);
                self.exits.entry(from).or_default().insert(to);
                self.entrances.entry(to).or_default().insert(from);
            }
        }
    }

    pub fn improved(&mut self, slot: K, parent: u64, wave: u32, tier: u8) {
        self.improved_work = self.improved_work.saturating_add(1);
        if !self.exits.contains_key(&slot) {
            return;
        }
        match self.pending_slot.get_mut(&slot) {
            Some(held) => {
                let previous = (held.tier, held.sequence);
                held.parent = parent;
                held.wave = wave;
                held.cursor = None;
                if tier < held.tier {
                    held.tier = tier;
                    let sequence = held.sequence;
                    self.pending.remove(&previous);
                    self.pending.insert((tier, sequence), slot);
                }
            }
            None => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                self.pending.insert((tier, sequence), slot);
                self.pending_slot.insert(
                    slot,
                    Pending {
                        sequence,
                        tier,
                        parent,
                        wave,
                        cursor: None,
                    },
                );
                self.memory_bytes = self.memory_bytes.saturating_add(Self::pending_bytes());
            }
        }
    }

    pub fn pop(&mut self, from_highest_tier: bool) -> Option<Continuation<K, A>> {
        loop {
            let (key, slot) = self.next_pending(from_highest_tier)?;
            let Some(held) = self.pending_slot.get(&slot).copied() else {
                self.pending.remove(&key);
                continue;
            };
            let next = self.exits.get(&slot).and_then(|exits| match held.cursor {
                Some(cursor) => exits
                    .range((
                        std::ops::Bound::Excluded(cursor),
                        std::ops::Bound::Unbounded,
                    ))
                    .next()
                    .copied(),
                None => exits.iter().next().copied(),
            });
            let Some(to) = next else {
                self.drop_pending(slot);
                continue;
            };
            let Some(edge) = self.edges.get(&(slot, to)) else {
                self.drop_pending(slot);
                continue;
            };
            let continuation = Continuation {
                parent: held.parent,
                donor: edge.donor,
                leaf: edge.leaf,
                destination: to,
                wave: held.wave,
                tier: held.tier,
                actions: edge.actions.clone(),
            };
            self.pending.remove(&key);
            let refreshed = self.next_sequence;
            self.next_sequence = self.next_sequence.saturating_add(1);
            self.pending.insert((held.tier, refreshed), slot);
            if let Some(entry) = self.pending_slot.get_mut(&slot) {
                entry.sequence = refreshed;
                entry.cursor = Some(to);
            }
            return Some(continuation);
        }
    }

    fn next_pending(&self, from_highest_tier: bool) -> Option<((u8, u64), K)> {
        if from_highest_tier {
            let (highest, _) = self.pending.keys().next_back().copied()?;
            return self
                .pending
                .range((highest, 0)..=(highest, u64::MAX))
                .next()
                .map(|(key, slot)| (*key, *slot));
        }
        self.pending.iter().next().map(|(key, slot)| (*key, *slot))
    }

    fn drop_pending(&mut self, slot: K) {
        if let Some(held) = self.pending_slot.remove(&slot) {
            self.pending.remove(&(held.tier, held.sequence));
            self.memory_bytes = self.memory_bytes.saturating_sub(Self::pending_bytes());
        }
    }

    pub fn retain_slots(&mut self, live: &BTreeSet<K>) {
        let stale = self
            .exits
            .keys()
            .chain(self.entrances.keys())
            .chain(self.pending_slot.keys())
            .copied()
            .filter(|slot| !live.contains(slot))
            .collect::<BTreeSet<_>>();
        for slot in stale {
            self.remove_slot(slot);
        }
    }

    pub fn remove_slot(&mut self, slot: K) {
        self.drop_pending(slot);
        for to in self.exits.remove(&slot).unwrap_or_default() {
            self.drop_edge(slot, to);
            if let Some(sources) = self.entrances.get_mut(&to) {
                sources.remove(&slot);
                if sources.is_empty() {
                    self.entrances.remove(&to);
                }
            }
        }
        for from in self.entrances.remove(&slot).unwrap_or_default() {
            self.drop_edge(from, slot);
            if let Some(targets) = self.exits.get_mut(&from) {
                targets.remove(&slot);
                if targets.is_empty() {
                    self.exits.remove(&from);
                    self.drop_pending(from);
                }
            }
        }
    }

    fn drop_edge(&mut self, from: K, to: K) {
        if let Some(edge) = self.edges.remove(&(from, to)) {
            self.memory_bytes = self
                .memory_bytes
                .saturating_sub(Self::edge_bytes(edge.actions.len()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bank() -> ContinuationBank<u8, u8> {
        ContinuationBank::new(4)
    }

    #[test]
    fn a_cheaper_tail_replaces_an_edge_and_a_costlier_one_does_not() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 100);
        bank.record(1, 2, 20, 21, &[8], 200);
        let charged = bank.memory_bytes();
        assert_eq!(bank.edge_count(), 1);
        bank.improved(1, 5, 0, 0);
        let taken = bank.pop(false).expect("edge");
        assert_eq!((taken.donor, taken.leaf, taken.actions), (10, 11, vec![7]));
        bank.record(1, 2, 30, 31, &[9, 9], 50);
        assert_eq!(bank.edge_count(), 1);
        assert_ne!(bank.memory_bytes(), charged);
        bank.improved(1, 5, 0, 0);
        let taken = bank.pop(false).expect("edge");
        assert_eq!(
            (taken.donor, taken.leaf, taken.actions),
            (30, 31, vec![9, 9])
        );
    }

    #[test]
    fn a_lower_tier_slot_is_served_before_an_older_higher_tier_one() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(4, 5, 14, 15, &[9], 1);
        bank.improved(4, 200, 0, 1);
        bank.improved(1, 100, 0, 0);
        assert_eq!(bank.pop(false).expect("lowest tier first").parent, 100);
        assert_eq!(bank.pop(false).expect("then the higher tier").parent, 200);
    }

    #[test]
    fn the_highest_tier_pop_skips_the_queue_ahead_of_it() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(4, 200, 0, 1);
        let taken = bank.pop(true).expect("highest tier");
        assert_eq!((taken.parent, taken.tier), (200, 1));
    }

    #[test]
    fn a_better_tier_moves_a_queued_slot_forward_and_a_worse_one_leaves_it() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 1);
        bank.improved(4, 200, 0, 1);
        bank.improved(4, 300, 0, 0);
        assert_eq!(bank.queue_len(), 2);
        assert_eq!(bank.pop(false).expect("promoted slot").parent, 300);
        bank.improved(1, 400, 0, 2);
        assert_eq!(bank.pop(false).expect("still its own tier").tier, 1);
    }

    #[test]
    fn an_edge_longer_than_the_cap_or_onto_itself_is_not_recorded() {
        let mut bank = bank();
        bank.record(1, 1, 10, 11, &[7], 1);
        bank.record(1, 2, 10, 11, &[], 1);
        bank.record(1, 2, 10, 11, &[1, 2, 3, 4, 5], 1);
        assert_eq!(bank.edge_count(), 0);
        assert_eq!(bank.memory_bytes(), 0);
    }

    #[test]
    fn queuing_a_slot_twice_updates_it_in_place() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(1, 3, 12, 13, &[8], 1);
        bank.record(4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(4, 200, 0, 0);
        let charged = bank.memory_bytes();
        assert_eq!(bank.pop(false).expect("first exit").destination, 2);
        bank.improved(1, 300, 2, 0);
        assert_eq!(bank.pending_count(), 2);
        assert_eq!(bank.queue_len(), 2);
        assert_eq!(bank.memory_bytes(), charged);
        let next = bank.pop(false).expect("rotated to the other slot");
        assert_eq!((next.destination, next.parent), (5, 200));
        let back = bank.pop(false).expect("back to the reset slot");
        assert_eq!((back.destination, back.parent, back.wave), (2, 300, 2));
    }

    #[test]
    fn popping_walks_a_slots_exits_and_drops_it_when_they_run_out() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(1, 3, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        assert_eq!(bank.pop(false).expect("first").destination, 2);
        assert_eq!(bank.pop(false).expect("second").destination, 3);
        assert!(bank.pop(false).is_none());
        assert_eq!(bank.pending_count(), 0);
        assert_eq!(bank.queue_len(), 0);
    }

    #[test]
    fn a_slot_improved_between_every_pop_does_not_hold_the_front() {
        let mut bank = bank();
        for to in 10..60_u8 {
            bank.record(1, to, 10, 11, &[7], 1);
        }
        bank.record(2, 90, 20, 21, &[8], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(2, 200, 0, 0);
        let mut reached_b = false;
        for _ in 0..4 {
            let taken = bank.pop(false).expect("a pending exit");
            reached_b |= taken.destination == 90;
            bank.improved(1, 100, 0, 0);
        }
        assert!(reached_b);
    }

    #[test]
    fn improving_a_slot_costs_nothing_proportional_to_its_exits() {
        let mut bank = bank();
        for to in 0..=250_u8 {
            bank.record(251, to, 10, 11, &[7], 1);
        }
        for _ in 0..100 {
            bank.improved(251, 1, 0, 0);
        }
        assert_eq!(bank.improved_work(), 100);
    }

    #[test]
    fn removing_a_slot_releases_its_edges_and_every_pending_entry() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(3, 2, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(3, 300, 0, 0);
        assert_eq!(bank.pending_count(), 2);
        bank.remove_slot(2);
        assert_eq!(bank.edge_count(), 0);
        assert_eq!(bank.pending_count(), 0);
        assert_eq!(bank.queue_len(), 0);
        assert_eq!(bank.memory_bytes(), 0);
        assert!(bank.pop(false).is_none());
    }

    #[test]
    fn deleting_and_recreating_a_slot_leaves_the_queue_the_size_it_reports() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.improved(1, 100, 0, 0);
        for _ in 0..50 {
            bank.record(3, 4, 12, 13, &[8], 1);
            bank.improved(3, 300, 0, 0);
            bank.remove_slot(3);
        }
        assert_eq!(bank.queue_len(), bank.pending_count());
        assert_eq!(bank.pending_count(), 1);
        assert_eq!(bank.pop(false).expect("the untouched slot").parent, 100);
    }

    #[test]
    fn retain_slots_drops_everything_outside_the_live_set() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1);
        bank.record(3, 4, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(3, 300, 0, 0);
        bank.retain_slots(&BTreeSet::from([1, 2]));
        assert_eq!(bank.edge_count(), 1);
        assert_eq!(bank.pending_count(), 1);
        assert_eq!(bank.pop(false).expect("the live slot").destination, 2);
    }
}
