// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    collections::{BTreeMap, BTreeSet, btree_map::Entry},
    mem::size_of,
};

pub const CONTINUATION_IDENTIFIER: &str = "position_edges_preference_queue_v1";

const EDGE_NODE_OVERHEAD: usize = 192;

const PENDING_NODE_OVERHEAD: usize = 128;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation<P, A> {
    pub source: P,
    pub parent: u64,
    pub donor: u64,
    pub leaf: u64,
    pub destination: P,
    pub wave: u32,
    pub preference: u8,
    pub gains: u8,
    pub actions: Vec<A>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Edge<A> {
    donor: u64,
    leaf: u64,
    cost: u64,
    gains: u8,
    actions: Vec<A>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Pending<P> {
    sequence: u64,
    preference: u8,
    parent: u64,
    wave: u32,
    cursor: Option<P>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound(
    serialize = "P: Serialize, A: Serialize",
    deserialize = "P: DeserializeOwned, A: DeserializeOwned"
))]
pub(crate) struct ContinuationBank<P: Ord, A> {
    longest_edge: usize,
    edges: BTreeMap<(P, P), Edge<A>>,
    exits: BTreeMap<P, BTreeSet<P>>,
    entrances: BTreeMap<P, BTreeSet<P>>,
    pending: BTreeMap<(u8, u64), P>,
    pending_source: BTreeMap<P, Pending<P>>,
    next_sequence: u64,
    memory_bytes: usize,
    improved_work: u64,
}

impl<P: Copy + Ord, A: Clone> ContinuationBank<P, A> {
    pub fn new(longest_edge: usize) -> Self {
        Self {
            longest_edge,
            edges: BTreeMap::new(),
            exits: BTreeMap::new(),
            entrances: BTreeMap::new(),
            pending: BTreeMap::new(),
            pending_source: BTreeMap::new(),
            next_sequence: 0,
            memory_bytes: 0,
            improved_work: 0,
        }
    }

    fn edge_bytes(actions: usize) -> usize {
        size_of::<(P, P)>()
            .saturating_add(size_of::<Edge<A>>())
            .saturating_add(actions.saturating_mul(size_of::<A>()))
            .saturating_add(2 * size_of::<P>())
            .saturating_add(EDGE_NODE_OVERHEAD)
    }

    fn pending_bytes() -> usize {
        size_of::<u64>()
            .saturating_add(2 * size_of::<P>())
            .saturating_add(size_of::<Pending<P>>())
            .saturating_add(PENDING_NODE_OVERHEAD)
    }

    pub fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn pending_count(&self) -> usize {
        self.pending_source.len()
    }

    #[cfg(test)]
    pub fn queue_len(&self) -> usize {
        self.pending.len()
    }

    #[cfg(test)]
    pub fn improved_work(&self) -> u64 {
        self.improved_work
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &mut self,
        from: P,
        to: P,
        donor: u64,
        leaf: u64,
        actions: &[A],
        cost: u64,
        gains: u8,
    ) {
        if from == to || actions.is_empty() || actions.len() > self.longest_edge {
            return;
        }
        let edge = || Edge {
            donor,
            leaf,
            cost,
            gains,
            actions: actions.to_vec(),
        };
        match self.edges.get_mut(&(from, to)) {
            Some(held) => {
                if cost >= held.cost {
                    return;
                }
                let edge = edge();
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_sub(Self::edge_bytes(held.actions.len()))
                    .saturating_add(Self::edge_bytes(actions.len()));
                *held = edge;
            }
            None => {
                let edge = edge();
                self.memory_bytes = self
                    .memory_bytes
                    .saturating_add(Self::edge_bytes(actions.len()));
                self.edges.insert((from, to), edge);
                self.exits.entry(from).or_default().insert(to);
                self.entrances.entry(to).or_default().insert(from);
            }
        }
    }

    pub fn improved(&mut self, source: P, parent: u64, wave: u32, preference: u8) {
        self.improved_work = self.improved_work.saturating_add(1);
        if !self.exits.contains_key(&source) {
            return;
        }
        match self.pending_source.entry(source) {
            Entry::Occupied(occupied) => {
                let held = occupied.into_mut();
                let previous = (held.preference, held.sequence);
                held.parent = parent;
                held.wave = wave;
                held.cursor = None;
                if preference != held.preference {
                    held.preference = preference;
                    let sequence = held.sequence;
                    self.pending.remove(&previous);
                    self.pending.insert((preference, sequence), source);
                }
            }
            Entry::Vacant(vacant) => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                self.pending.insert((preference, sequence), source);
                vacant.insert(Pending {
                    sequence,
                    preference,
                    parent,
                    wave,
                    cursor: None,
                });
                self.memory_bytes = self.memory_bytes.saturating_add(Self::pending_bytes());
            }
        }
    }

    pub fn pop(&mut self, from_highest_preference: bool) -> Option<Continuation<P, A>> {
        loop {
            let (key, source) = self.next_pending(from_highest_preference)?;
            let Some(held) = self.pending_source.get(&source).copied() else {
                self.pending.remove(&key);
                continue;
            };
            let next = self.exits.get(&source).and_then(|exits| match held.cursor {
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
                self.drop_pending(source);
                continue;
            };
            let Some(edge) = self.edges.get(&(source, to)) else {
                self.drop_pending(source);
                continue;
            };
            let continuation = Continuation {
                source,
                parent: held.parent,
                donor: edge.donor,
                leaf: edge.leaf,
                destination: to,
                wave: held.wave,
                preference: held.preference,
                gains: edge.gains,
                actions: edge.actions.clone(),
            };
            self.pending.remove(&key);
            let refreshed = self.next_sequence;
            self.next_sequence = self.next_sequence.saturating_add(1);
            self.pending.insert((held.preference, refreshed), source);
            if let Some(entry) = self.pending_source.get_mut(&source) {
                entry.sequence = refreshed;
                entry.cursor = Some(to);
            }
            return Some(continuation);
        }
    }

    fn next_pending(&self, from_highest_preference: bool) -> Option<((u8, u64), P)> {
        if from_highest_preference {
            let (highest, _) = self.pending.keys().next_back().copied()?;
            return self
                .pending
                .range((highest, 0)..=(highest, u64::MAX))
                .next()
                .map(|(key, source)| (*key, *source));
        }
        self.pending
            .iter()
            .next()
            .map(|(key, source)| (*key, *source))
    }

    pub fn set_longest_edge(&mut self, longest_edge: usize) {
        self.longest_edge = longest_edge;
    }

    pub fn retain_preferences(&mut self, preferences: usize) {
        let dropped: Vec<P> = self
            .pending_source
            .iter()
            .filter(|(_, held)| usize::from(held.preference) >= preferences)
            .map(|(source, _)| *source)
            .collect();
        for source in dropped {
            self.drop_pending(source);
        }
    }

    fn drop_pending(&mut self, source: P) {
        if let Some(held) = self.pending_source.remove(&source) {
            self.pending.remove(&(held.preference, held.sequence));
            self.memory_bytes = self.memory_bytes.saturating_sub(Self::pending_bytes());
        }
    }

    pub fn retain(&mut self, live: &BTreeSet<P>) {
        let stale_sources = self
            .exits
            .keys()
            .chain(self.pending_source.keys())
            .copied()
            .filter(|source| !live.contains(source))
            .collect::<BTreeSet<_>>();
        for source in stale_sources {
            self.remove_source(source);
        }
        let stale_places = self
            .entrances
            .keys()
            .copied()
            .filter(|place| !live.contains(place))
            .collect::<BTreeSet<_>>();
        for place in stale_places {
            self.remove_place(place);
        }
    }

    pub fn remove_source(&mut self, source: P) {
        self.drop_pending(source);
        for to in self.exits.remove(&source).unwrap_or_default() {
            self.drop_edge(source, to);
            if let Some(sources) = self.entrances.get_mut(&to) {
                sources.remove(&source);
                if sources.is_empty() {
                    self.entrances.remove(&to);
                }
            }
        }
    }

    pub fn remove_place(&mut self, place: P) {
        for from in self.entrances.remove(&place).unwrap_or_default() {
            self.drop_edge(from, place);
            if let Some(targets) = self.exits.get_mut(&from) {
                targets.remove(&place);
                if targets.is_empty() {
                    self.exits.remove(&from);
                    self.drop_pending(from);
                }
            }
        }
    }

    fn drop_edge(&mut self, from: P, to: P) {
        if let Some(edge) = self.edges.remove(&(from, to)) {
            self.memory_bytes = self
                .memory_bytes
                .saturating_sub(Self::edge_bytes(edge.actions.len()));
        }
    }
}

#[cfg(test)]
impl<P: Copy + Ord, A: Clone> ContinuationBank<P, A> {
    fn improved_reference(&mut self, source: P, parent: u64, wave: u32, preference: u8) {
        self.improved_work = self.improved_work.saturating_add(1);
        if !self.exits.contains_key(&source) {
            return;
        }
        match self.pending_source.get_mut(&source) {
            Some(held) => {
                let previous = (held.preference, held.sequence);
                held.parent = parent;
                held.wave = wave;
                held.cursor = None;
                if preference != held.preference {
                    held.preference = preference;
                    let sequence = held.sequence;
                    self.pending.remove(&previous);
                    self.pending.insert((preference, sequence), source);
                }
            }
            None => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                self.pending.insert((preference, sequence), source);
                self.pending_source.insert(
                    source,
                    Pending {
                        sequence,
                        preference,
                        parent,
                        wave,
                        cursor: None,
                    },
                );
                self.memory_bytes = self.memory_bytes.saturating_add(Self::pending_bytes());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_entry_updates_preserve_serialized_bank_and_dispatches() {
        use crate::search::rand::RomuDuoJrRand;
        let mut rand = RomuDuoJrRand::with_seed(421);
        let mut actual = ContinuationBank::<u64, u8>::new(8);
        let mut reference = ContinuationBank::<u64, u8>::new(8);
        for step in 0..4096 {
            let from = rand.next_u64() % 32;
            let to = rand.next_u64() % 32;
            let len = (rand.next_u64() % 11) as usize;
            let actions = vec![(step % 256) as u8; len];
            let cost = rand.next_u64() % 32;
            let gains = rand.next_u64() as u8;
            actual.record(from, to, step, step + 1, &actions, cost, gains);
            reference.record(from, to, step, step + 1, &actions, cost, gains);
            let preference = (rand.next_u64() % 8) as u8;
            actual.improved(from, step + 2, step as u32, preference);
            reference.improved_reference(from, step + 2, step as u32, preference);
            if step % 3 == 0 {
                let highest = step % 2 == 0;
                assert_eq!(actual.pop(highest), reference.pop(highest));
            }
            if step % 17 == 0 {
                actual.remove_source(to);
                reference.remove_source(to);
            }
            if step % 19 == 0 {
                actual.remove_place(from);
                reference.remove_place(from);
            }
            if step % 31 == 0 {
                actual.retain_preferences(4);
                reference.retain_preferences(4);
            }
            if step == 4000 {
                actual.next_sequence = u64::MAX;
                reference.next_sequence = u64::MAX;
                actual.improved_work = u64::MAX;
                reference.improved_work = u64::MAX;
            }
            assert_eq!(
                postcard::to_stdvec(&actual).unwrap(),
                postcard::to_stdvec(&reference).unwrap()
            );
        }
    }

    #[test]
    fn pending_entry_insertion_avoids_the_second_tree_search() {
        use std::{
            cmp::Ordering,
            sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
        };
        static COMPARISONS: AtomicUsize = AtomicUsize::new(0);
        #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
        struct Key(u64);
        impl Ord for Key {
            fn cmp(&self, other: &Self) -> Ordering {
                COMPARISONS.fetch_add(1, AtomicOrdering::Relaxed);
                self.0.cmp(&other.0)
            }
        }
        impl PartialOrd for Key {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        let fixture = || {
            let mut bank = ContinuationBank::new(1);
            for source in 1..=128 {
                bank.record(Key(source), Key(0), source, source + 1, &[7_u8], 1, 0);
                if source <= 64 {
                    bank.improved_reference(Key(source), source, 0, 1);
                }
            }
            bank
        };
        let mut reference = fixture();
        let mut actual = fixture();
        COMPARISONS.store(0, AtomicOrdering::Relaxed);
        reference.improved_reference(Key(65), 100, 1, 2);
        let baseline = COMPARISONS.load(AtomicOrdering::Relaxed);
        COMPARISONS.store(0, AtomicOrdering::Relaxed);
        actual.improved(Key(65), 100, 1, 2);
        let candidate = COMPARISONS.load(AtomicOrdering::Relaxed);
        assert!(
            candidate < baseline,
            "candidate={candidate}, baseline={baseline}"
        );
        assert_eq!(
            postcard::to_stdvec(&actual).unwrap(),
            postcard::to_stdvec(&reference).unwrap()
        );
    }

    fn pending_entry_fixture(count: u64) -> ContinuationBank<u64, u8> {
        let mut bank = ContinuationBank::new(6);
        for source in 1..=count + 128 {
            bank.record(source, 0, source, source + 1, &[7; 6], 1, 3);
            if source <= count {
                bank.improved_reference(source, source + 2, 0, 1);
            }
        }
        bank
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn pending_entry_benchmark() {
        use std::{hint::black_box, time::Instant};
        if std::env::var_os("DISSONANCE_BENCHMARK_PENDING_ENTRY").is_none() {
            return;
        }
        for count in [0_u64, 1, 16, 256, 4096] {
            for shuffled in [false, true] {
                for mode in 0..4 {
                    if count == 0 && mode != 0 {
                        continue;
                    }
                    let mut banks = [pending_entry_fixture(count), pending_entry_fixture(count)];
                    let batch = count.clamp(1, 128);
                    let mut ratios = Vec::new();
                    for round in 0..100 {
                        let mut elapsed = [0_u128; 2];
                        for new in if round % 2 == 0 {
                            [false, true, true, false]
                        } else {
                            [true, false, false, true]
                        } {
                            let bank = &mut banks[usize::from(new)];
                            if mode == 1 {
                                for id in 1..=batch {
                                    bank.improved_reference(id, 0, 0, 1);
                                }
                            }
                            let start = Instant::now();
                            for repetition in 0..if mode == 0 { 1 } else { 32 } {
                                for index in 0..batch {
                                    let id = black_box(
                                        1 + if shuffled { index * 73 % batch } else { index },
                                    );
                                    let source = match mode {
                                        0 => count + id,
                                        3 => count + 128 + id,
                                        _ => id,
                                    };
                                    let preference = if mode == 1 { 2 + repetition % 2 } else { 1 };
                                    if new {
                                        bank.improved(source, id + 3, 1, preference);
                                    } else {
                                        bank.improved_reference(source, id + 3, 1, preference);
                                    }
                                }
                            }
                            elapsed[usize::from(new)] += start.elapsed().as_nanos();
                            if mode == 0 {
                                for id in 1..=batch {
                                    bank.drop_pending(count + id);
                                }
                            }
                        }
                        assert_eq!(
                            postcard::to_stdvec(&banks[0]).unwrap(),
                            postcard::to_stdvec(&banks[1]).unwrap()
                        );
                        ratios.push(elapsed[1] as f64 / elapsed[0] as f64);
                    }
                    ratios.sort_by(f64::total_cmp);
                    eprintln!(
                        "pending_entry count={count} shuffled={shuffled} mode={mode} ratio={:.4} p10={:.4} p90={:.4}",
                        ratios[ratios.len() / 2],
                        ratios[ratios.len() / 10],
                        ratios[ratios.len() * 9 / 10]
                    );
                }
            }
        }
    }

    fn bank() -> ContinuationBank<u8, u8> {
        ContinuationBank::new(4)
    }

    fn record(
        bank: &mut ContinuationBank<u8, u8>,
        from: u8,
        to: u8,
        donor: u64,
        leaf: u64,
        actions: &[u8],
        cost: u64,
    ) {
        bank.record(from, to, donor, leaf, actions, cost, 0);
    }

    #[test]
    fn a_raised_longest_edge_records_longer_edges_and_trimming_drops_queued_sources() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7; 6], 100);
        assert_eq!(bank.edge_count(), 0);
        bank.set_longest_edge(6);
        record(&mut bank, 1, 2, 10, 11, &[7; 6], 100);
        assert_eq!(bank.edge_count(), 1);
        bank.improved(1, 5, 0, 1);
        bank.retain_preferences(2);
        assert_eq!(bank.queue_len(), 1);
        bank.retain_preferences(1);
        assert_eq!(bank.queue_len(), 0);
    }

    #[test]
    fn a_cheaper_tail_replaces_an_edge_and_a_costlier_one_does_not() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 100);
        record(&mut bank, 1, 2, 20, 21, &[8], 200);
        let charged = bank.memory_bytes();
        assert_eq!(bank.edge_count(), 1);
        bank.improved(1, 5, 0, 0);
        let taken = bank.pop(false).expect("edge");
        assert_eq!((taken.donor, taken.leaf, taken.actions), (10, 11, vec![7]));
        record(&mut bank, 1, 2, 30, 31, &[9, 9], 50);
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
    fn rejected_edges_do_not_clone_actions_or_change_state() {
        use std::{cell::Cell, rc::Rc};

        #[derive(Serialize)]
        struct Action {
            value: u8,
            #[serde(skip)]
            clones: Rc<Cell<usize>>,
        }

        impl Clone for Action {
            fn clone(&self) -> Self {
                self.clones.set(self.clones.get() + 1);
                Self {
                    value: self.value,
                    clones: Rc::clone(&self.clones),
                }
            }
        }

        let clones = Rc::new(Cell::new(0));
        let actions = (0..128)
            .map(|value| Action {
                value,
                clones: Rc::clone(&clones),
            })
            .collect::<Vec<_>>();
        let mut bank = ContinuationBank::<u8, Action>::new(128);
        bank.record(1, 2, 10, 11, &actions, 100, 1);
        assert_eq!(clones.get(), 128);
        let before = postcard::to_allocvec(&bank).unwrap();
        for cost in [100, 101, u64::MAX] {
            bank.record(1, 2, 20, 21, &actions, cost, 7);
            assert_eq!(clones.get(), 128);
            assert_eq!(postcard::to_allocvec(&bank).unwrap(), before);
        }
        bank.record(1, 2, 30, 31, &actions[..64], 99, 2);
        assert_eq!(clones.get(), 192);
        let held = bank.edges.get(&(1, 2)).unwrap();
        assert_eq!(
            (held.donor, held.leaf, held.cost, held.gains),
            (30, 31, 99, 2)
        );
        assert_eq!(held.actions.len(), 64);
        assert_eq!(
            bank.memory_bytes(),
            ContinuationBank::<u8, Action>::edge_bytes(64)
        );
    }

    #[test]
    fn an_edge_carries_the_resource_change_it_was_recorded_with() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 100, 0b10);
        bank.improved(1, 5, 0, 0);
        assert_eq!(bank.pop(false).expect("edge").gains, 0b10);
    }

    #[test]
    fn a_lower_preference_source_is_served_before_an_older_higher_one() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 4, 5, 14, 15, &[9], 1);
        bank.improved(4, 200, 0, 1);
        bank.improved(1, 100, 0, 0);
        assert_eq!(
            bank.pop(false).expect("lowest preference first").parent,
            100
        );
        assert_eq!(
            bank.pop(false).expect("then the higher preference").parent,
            200
        );
    }

    #[test]
    fn the_highest_preference_pop_skips_the_queue_ahead_of_it() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(4, 200, 0, 1);
        let taken = bank.pop(true).expect("highest preference");
        assert_eq!((taken.parent, taken.preference), (200, 1));
    }

    #[test]
    fn a_queued_source_takes_the_preference_of_its_latest_improvement() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 1);
        bank.improved(4, 200, 0, 1);
        bank.improved(4, 300, 0, 0);
        assert_eq!(bank.queue_len(), 2);
        assert_eq!(bank.pop(false).expect("promoted source").parent, 300);
        bank.improved(1, 400, 0, 2);
        let taken = bank.pop(false).expect("the latest holder");
        assert_eq!((taken.parent, taken.preference), (400, 2));
    }

    #[test]
    fn an_edge_longer_than_the_cap_or_onto_its_own_position_is_not_recorded() {
        let mut bank = bank();
        record(&mut bank, 1, 1, 10, 11, &[7], 1);
        record(&mut bank, 1, 2, 10, 11, &[], 1);
        record(&mut bank, 1, 2, 10, 11, &[1, 2, 3, 4, 5], 1);
        assert_eq!(bank.edge_count(), 0);
        assert_eq!(bank.memory_bytes(), 0);
    }

    #[test]
    fn two_sources_into_one_position_keep_separate_edges() {
        let mut bank = bank();
        bank.record(1, 2, 10, 11, &[7], 1, 0);
        bank.record(3, 2, 12, 13, &[8], 1, 0);
        assert_eq!(bank.edge_count(), 2);
        bank.improved(3, 300, 0, 0);
        assert_eq!(bank.pop(false).expect("the improved source").donor, 12);
    }

    #[test]
    fn queuing_a_source_twice_updates_it_in_place() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 1, 3, 12, 13, &[8], 1);
        record(&mut bank, 4, 5, 14, 15, &[9], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(4, 200, 0, 0);
        let charged = bank.memory_bytes();
        assert_eq!(bank.pop(false).expect("first exit").destination, 2);
        bank.improved(1, 300, 2, 0);
        assert_eq!(bank.pending_count(), 2);
        assert_eq!(bank.queue_len(), 2);
        assert_eq!(bank.memory_bytes(), charged);
        let next = bank.pop(false).expect("rotated to the other source");
        assert_eq!((next.destination, next.parent), (5, 200));
        let back = bank.pop(false).expect("back to the reset source");
        assert_eq!((back.destination, back.parent, back.wave), (2, 300, 2));
    }

    #[test]
    fn popping_walks_a_sources_exits_and_drops_it_when_they_run_out() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 1, 3, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        assert_eq!(bank.pop(false).expect("first").destination, 2);
        assert_eq!(bank.pop(false).expect("second").destination, 3);
        assert!(bank.pop(false).is_none());
        assert_eq!(bank.pending_count(), 0);
        assert_eq!(bank.queue_len(), 0);
    }

    #[test]
    fn a_source_improved_between_every_pop_does_not_hold_the_front() {
        let mut bank = bank();
        for to in 10..60_u8 {
            record(&mut bank, 1, to, 10, 11, &[7], 1);
        }
        record(&mut bank, 2, 90, 20, 21, &[8], 1);
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
    fn improving_a_source_costs_nothing_proportional_to_its_exits() {
        let mut bank = bank();
        for to in 0..=250_u8 {
            record(&mut bank, 251, to, 10, 11, &[7], 1);
        }
        for _ in 0..100 {
            bank.improved(251, 1, 0, 0);
        }
        assert_eq!(bank.improved_work(), 100);
    }

    #[test]
    fn removing_a_place_releases_its_edges_and_every_pending_entry() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 3, 2, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(3, 300, 0, 0);
        assert_eq!(bank.pending_count(), 2);
        bank.remove_place(2);
        assert_eq!(bank.edge_count(), 0);
        assert_eq!(bank.pending_count(), 0);
        assert_eq!(bank.queue_len(), 0);
        assert_eq!(bank.memory_bytes(), 0);
        assert!(bank.pop(false).is_none());
    }

    #[test]
    fn deleting_and_recreating_a_source_leaves_the_queue_the_size_it_reports() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        bank.improved(1, 100, 0, 0);
        for _ in 0..50 {
            record(&mut bank, 3, 4, 12, 13, &[8], 1);
            bank.improved(3, 300, 0, 0);
            bank.remove_source(3);
        }
        assert_eq!(bank.queue_len(), bank.pending_count());
        assert_eq!(bank.pending_count(), 1);
        assert_eq!(bank.pop(false).expect("the untouched source").parent, 100);
    }

    #[test]
    fn retain_drops_everything_outside_the_live_sets() {
        let mut bank = bank();
        record(&mut bank, 1, 2, 10, 11, &[7], 1);
        record(&mut bank, 3, 4, 12, 13, &[8], 1);
        bank.improved(1, 100, 0, 0);
        bank.improved(3, 300, 0, 0);
        bank.retain(&BTreeSet::from([1, 2]));
        assert_eq!(bank.edge_count(), 1);
        assert_eq!(bank.pending_count(), 1);
        assert_eq!(bank.pop(false).expect("the live source").destination, 2);
    }
}
