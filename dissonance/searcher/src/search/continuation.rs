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
    parent: u64,
    wave: u32,
    cursor: Option<K>,
}

pub(crate) struct ContinuationBank<K: Ord, A> {
    action_cap: usize,
    edges: BTreeMap<(K, K), Edge<A>>,
    exits: BTreeMap<K, BTreeSet<K>>,
    entrances: BTreeMap<K, BTreeSet<K>>,
    pending: BTreeMap<u64, K>,
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

    pub fn queue_len(&self) -> usize {
        self.pending.len()
    }

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

    pub fn improved(&mut self, slot: K, parent: u64, wave: u32) {
        self.improved_work = self.improved_work.saturating_add(1);
        if !self.exits.contains_key(&slot) {
            return;
        }
        match self.pending_slot.get_mut(&slot) {
            Some(held) => {
                held.parent = parent;
                held.wave = wave;
                held.cursor = None;
            }
            None => {
                let sequence = self.next_sequence;
                self.next_sequence = self.next_sequence.saturating_add(1);
                self.pending.insert(sequence, slot);
                self.pending_slot.insert(
                    slot,
                    Pending {
                        sequence,
                        parent,
                        wave,
                        cursor: None,
                    },
                );
                self.memory_bytes = self.memory_bytes.saturating_add(Self::pending_bytes());
            }
        }
    }

    pub fn pop(&mut self) -> Option<Continuation<K, A>> {
        loop {
            let (sequence, slot) = self.pending.iter().next().map(|(k, v)| (*k, *v))?;
            let Some(held) = self.pending_slot.get(&slot).copied() else {
                self.pending.remove(&sequence);
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
                actions: edge.actions.clone(),
            };
            self.pending.remove(&sequence);
            let refreshed = self.next_sequence;
            self.next_sequence = self.next_sequence.saturating_add(1);
            self.pending.insert(refreshed, slot);
            if let Some(entry) = self.pending_slot.get_mut(&slot) {
                entry.sequence = refreshed;
                entry.cursor = Some(to);
            }
            return Some(continuation);
        }
    }

    fn drop_pending(&mut self, slot: K) {
        if let Some(held) = self.pending_slot.remove(&slot) {
            self.pending.remove(&held.sequence);
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
