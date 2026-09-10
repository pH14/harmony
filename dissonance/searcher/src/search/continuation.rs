// SPDX-License-Identifier: AGPL-3.0-or-later

//! Bounded reuse of learned transitions after a same-slot preference improvement.
//! No map topology or action sequence is supplied by a workload.

use std::{
    collections::{BTreeMap, VecDeque},
    mem::size_of,
};

pub(crate) const ACTION_CAP: usize = 128;
const EXIT_CAP: usize = 8192;
const EXITS_PER_SLOT: usize = 8;
const QUEUE_CAP: usize = 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Continuation<A> {
    pub parent: u64,
    pub donor: u64,
    pub leaf: u64,
    pub actions: Vec<A>,
}

pub(crate) struct ContinuationBank<K: Ord, A> {
    exits: BTreeMap<K, BTreeMap<K, Continuation<A>>>,
    order: VecDeque<(K, K)>,
    pending: VecDeque<Continuation<A>>,
}

impl<K: Copy + Ord, A: Clone> Default for ContinuationBank<K, A> {
    fn default() -> Self {
        Self {
            exits: BTreeMap::new(),
            order: VecDeque::new(),
            pending: VecDeque::new(),
        }
    }
}

impl<K: Copy + Ord, A: Clone> ContinuationBank<K, A> {
    /// A fixed conservative reserve covers map nodes, deque slack, and all
    /// bounded action payloads. Consumption can occur at different moments in
    /// serial replay; the charge is independent of that host-side timing.
    pub fn reserve_bytes() -> usize {
        let record = size_of::<Continuation<A>>() + ACTION_CAP * size_of::<A>();
        EXIT_CAP
            .saturating_mul(record + 8 * size_of::<K>() + 256)
            .saturating_add(QUEUE_CAP.saturating_mul(record + 64) * 2)
    }

    pub fn record(&mut self, from: K, to: K, donor: u64, leaf: u64, actions: &[A]) {
        if from == to || actions.is_empty() || actions.len() > ACTION_CAP {
            return;
        }
        if let Some(exits) = self.exits.get(&from)
            && exits.len() >= EXITS_PER_SLOT
            && !exits.contains_key(&to)
        {
            return;
        }
        let exists = self
            .exits
            .get(&from)
            .is_some_and(|exits| exits.contains_key(&to));
        if !exists {
            if self.order.len() == EXIT_CAP {
                let (old_from, old_to) = self.order.pop_front().expect("full exit order");
                let exits = self.exits.get_mut(&old_from).expect("indexed exit");
                exits.remove(&old_to);
                if exits.is_empty() {
                    self.exits.remove(&old_from);
                }
            }
            self.order.push_back((from, to));
        }
        self.exits.entry(from).or_default().insert(
            to,
            Continuation {
                parent: donor,
                donor,
                leaf,
                actions: actions.to_vec(),
            },
        );
    }

    pub fn improved(&mut self, place: K, parent: u64) {
        if let Some(exits) = self.exits.get(&place) {
            for exit in exits.values() {
                if self.pending.len() == QUEUE_CAP {
                    self.pending.pop_front();
                }
                self.pending.push_back(Continuation {
                    parent,
                    ..exit.clone()
                });
            }
        }
    }

    pub fn pop(&mut self) -> Option<Continuation<A>> {
        self.pending.pop_back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn improvement_reuses_only_observed_exits_with_the_new_parent() {
        let mut bank = ContinuationBank::default();
        bank.record(10_u16, 11, 1, 2, &[7_u8, 8]);
        bank.record(10, 12, 1, 3, &[9]);
        bank.record(12, 13, 3, 4, &[4]);
        bank.improved(10, 99);
        assert_eq!(
            bank.pop(),
            Some(Continuation {
                parent: 99,
                donor: 1,
                leaf: 3,
                actions: vec![9]
            })
        );
        assert_eq!(
            bank.pop(),
            Some(Continuation {
                parent: 99,
                donor: 1,
                leaf: 2,
                actions: vec![7, 8]
            })
        );
        assert!(bank.pop().is_none());
    }

    #[test]
    fn legacy_partial_batch_priority_depends_on_destination_labels() {
        // The same three observed edges, arrival order and action payloads;
        // only the opaque destination names change. A one-attempt budget
        // exposes label priority even though draining the whole batch does not.
        let mut first_counts = BTreeMap::new();
        for labels in [
            [1, 2, 3],
            [1, 3, 2],
            [2, 1, 3],
            [2, 3, 1],
            [3, 1, 2],
            [3, 2, 1],
        ] {
            let mut bank = ContinuationBank::default();
            for (index, destination) in labels.into_iter().enumerate() {
                let index = u8::try_from(index).unwrap();
                bank.record(0_u8, destination, 10, 11 + u64::from(index), &[index]);
            }
            bank.improved(0, 20);
            let attempts: Vec<_> = std::iter::from_fn(|| bank.pop()).collect();
            assert!(attempts.iter().all(|attempt| attempt.parent == 20));
            let actions: Vec<_> = attempts.iter().map(|attempt| attempt.actions[0]).collect();
            assert_eq!(
                actions
                    .iter()
                    .copied()
                    .collect::<std::collections::BTreeSet<_>>(),
                [0, 1, 2].into_iter().collect()
            );
            assert_eq!(labels[usize::from(actions[0])], 3);
            *first_counts.entry(actions[0]).or_insert(0) += 1;
        }
        assert_eq!(first_counts, BTreeMap::from([(0, 2), (1, 2), (2, 2)]));
    }

    #[test]
    fn latest_exit_replaces_the_old_tape_and_memory_stays_bounded() {
        let mut bank = ContinuationBank::default();
        bank.record(1_u32, 2, 0, 1, &[1_u8]);
        bank.record(1, 2, 2, 3, &[2, 3]);
        bank.improved(1, 4);
        assert_eq!(bank.pop().unwrap().actions, vec![2, 3]);
        let charge = ContinuationBank::<u32, u8>::reserve_bytes();
        for n in 0..(EXIT_CAP as u32 * 3) {
            bank.record(n, n + 1, u64::from(n), u64::from(n + 1), &[1]);
            bank.improved(n, u64::from(n + 2));
        }
        assert_eq!(bank.order.len(), EXIT_CAP);
        assert_eq!(bank.pending.len(), QUEUE_CAP);
        assert_eq!(
            bank.exits.values().map(BTreeMap::len).sum::<usize>(),
            EXIT_CAP
        );
        assert_eq!(charge, ContinuationBank::<u32, u8>::reserve_bytes());
        bank.improved(0, u64::MAX);
        assert_ne!(
            bank.pop().unwrap().parent,
            u64::MAX,
            "evicted exits must not be reused"
        );
    }

    #[test]
    fn self_loops_and_oversize_tapes_are_not_learned() {
        let mut bank = ContinuationBank::default();
        bank.record(1_u8, 1, 1, 2, &[1_u8]);
        bank.record(1, 2, 1, 2, &[]);
        bank.record(1, 2, 1, 2, &[1; ACTION_CAP + 1]);
        bank.improved(1, 3);
        assert!(bank.pop().is_none());
    }
}
