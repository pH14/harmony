// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    sync::Mutex,
};

use searcher::search::{archive::ArchiveKey, assertion::AssertionKey};
use serde::Serialize;

use crate::{archive::FaultArchiveKey, key::FaultKeys, target::FaultObservations};

type Pairs = BTreeMap<(FaultArchiveKey, Option<AssertionKey>), u64>;

#[derive(Debug, Default)]
pub struct Shadow {
    pairs: Mutex<Pairs>,
    unrecorded: Mutex<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Disagreements {
    pub absent: u64,
    pub cell: usize,
    pub slot: usize,
    pub progress: usize,
    pub preference: usize,
    pub preference_count: bool,
    pub capacity: bool,
}

impl Disagreements {
    #[must_use]
    pub fn total(&self) -> u64 {
        self.absent
            + [self.cell, self.slot, self.progress, self.preference]
                .into_iter()
                .map(|count| u64::try_from(count).unwrap_or(u64::MAX))
                .sum::<u64>()
            + u64::from(self.preference_count)
            + u64::from(self.capacity)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShadowReport {
    pub policy: String,
    pub states: u64,
    pub distinct_pairs: usize,
    pub hand_cells: usize,
    pub strategy_cells: usize,
    pub hand_slots: usize,
    pub strategy_slots: usize,
    pub unrecorded: u64,
    pub disagreements: Disagreements,
    pub examples: Vec<String>,
}

fn adjacent_disagreements<T: Copy>(
    items: &mut [T],
    compiled: impl Fn(&T, &T) -> Ordering,
    hand: impl Fn(&T, &T) -> Ordering,
) -> Vec<(T, T)> {
    items.sort_by(|a, b| compiled(a, b).then_with(|| hand(a, b)));
    items
        .windows(2)
        .filter(|pair| compiled(&pair[0], &pair[1]) != hand(&pair[0], &pair[1]))
        .map(|pair| (pair[0], pair[1]))
        .collect()
}

fn not_one_to_one<A: Ord + Copy, B: Ord + Copy>(pairs: &BTreeSet<(A, B)>) -> Vec<(A, B)> {
    let mut forward: BTreeMap<A, BTreeSet<B>> = BTreeMap::new();
    let mut backward: BTreeMap<B, BTreeSet<A>> = BTreeMap::new();
    for (a, b) in pairs {
        forward.entry(*a).or_default().insert(*b);
        backward.entry(*b).or_default().insert(*a);
    }
    pairs
        .iter()
        .filter(|(a, b)| forward[a].len() > 1 || backward[b].len() > 1)
        .copied()
        .collect()
}

impl Shadow {
    pub fn observe(
        &self,
        keys: &FaultKeys,
        hand: FaultArchiveKey,
        observations: &FaultObservations,
    ) {
        let strategy = keys.key(observations);
        match self.pairs.lock() {
            Ok(mut pairs) => *pairs.entry((hand, strategy)).or_default() += 1,
            Err(_) => {
                if let Ok(mut unrecorded) = self.unrecorded.lock() {
                    *unrecorded += 1;
                }
            }
        }
    }

    pub fn report(&self, keys: &FaultKeys) -> Result<ShadowReport, Box<dyn Error>> {
        let pairs = self
            .pairs
            .lock()
            .map_err(|_| "the shadow key table is poisoned")?
            .clone();
        let shape = keys.compiled().shape();
        let mut disagreements = Disagreements {
            preference_count: FaultArchiveKey::preferences() != shape.preferences,
            capacity: FaultArchiveKey::capacity() != shape.capacity,
            ..Disagreements::default()
        };
        let mut examples = Vec::new();
        let mut keyed: Vec<(FaultArchiveKey, AssertionKey)> = Vec::new();
        let mut states = 0;
        for ((hand, key), count) in &pairs {
            states += count;
            match key {
                Some(key) => keyed.push((*hand, *key)),
                None => {
                    disagreements.absent += count;
                    if examples.len() < 8 {
                        examples.push(format!("absent: hand {hand:?}"));
                    }
                }
            }
        }
        let cells: BTreeSet<_> = keyed
            .iter()
            .map(|(hand, key)| {
                (
                    (hand.progress(), hand.place()),
                    (key.progress, key.place.get()),
                )
            })
            .collect();
        let cell_faults = not_one_to_one(&cells);
        disagreements.cell = cell_faults.len();
        examples.extend(
            cell_faults
                .iter()
                .take(4)
                .map(|fault| format!("cell: {fault:?}")),
        );
        let slots: BTreeSet<_> = keyed
            .iter()
            .map(|(hand, key)| {
                (
                    ((hand.progress(), hand.place()), hand.identity()),
                    ((key.progress, key.place.get()), key.identity),
                )
            })
            .collect();
        let slot_faults = not_one_to_one(&slots);
        disagreements.slot = slot_faults.len();
        examples.extend(
            slot_faults
                .iter()
                .take(4)
                .map(|fault| format!("slot: {fault:?}")),
        );
        let mut progress: Vec<_> = keyed
            .iter()
            .map(|(hand, key)| (hand.progress(), key.progress))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let progress_faults =
            adjacent_disagreements(&mut progress, |a, b| a.1.cmp(&b.1), |a, b| a.0.cmp(&b.0));
        disagreements.progress = progress_faults.len();
        examples.extend(
            progress_faults
                .iter()
                .take(4)
                .map(|fault| format!("progress: {fault:?}")),
        );
        let mut places: Vec<_> = keyed
            .iter()
            .map(|(hand, key)| {
                (
                    (hand.progress(), hand.place()),
                    (key.progress, key.place.get()),
                )
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let order_faults =
            adjacent_disagreements(&mut places, |a, b| a.1.cmp(&b.1), |a, b| a.0.cmp(&b.0));
        disagreements.cell += order_faults.len();
        examples.extend(
            order_faults
                .iter()
                .take(4)
                .map(|fault| format!("cell order: {fault:?}")),
        );
        let mut by_slot: BTreeMap<(u32, u64, u32), Vec<(FaultArchiveKey, AssertionKey)>> =
            BTreeMap::new();
        for (hand, key) in &keyed {
            by_slot
                .entry((key.progress, key.place.get(), key.identity))
                .or_default()
                .push((*hand, *key));
        }
        let preferences = shape.preferences.max(FaultArchiveKey::preferences());
        for held in by_slot.values_mut() {
            for preference in 0..preferences {
                let faults = adjacent_disagreements(
                    held,
                    |a, b| a.1.preference_cmp(preference, b.1),
                    |a, b| a.0.preference_cmp(preference, b.0),
                );
                disagreements.preference += faults.len();
                examples.extend(
                    faults
                        .iter()
                        .take(2)
                        .map(|fault| format!("preference {preference}: {fault:?}")),
                );
            }
        }
        Ok(ShadowReport {
            policy: keys.compiled().policy_identifier().to_owned(),
            states,
            distinct_pairs: pairs.len(),
            hand_cells: cells
                .iter()
                .map(|(hand, _)| *hand)
                .collect::<BTreeSet<_>>()
                .len(),
            strategy_cells: cells
                .iter()
                .map(|(_, key)| *key)
                .collect::<BTreeSet<_>>()
                .len(),
            hand_slots: slots
                .iter()
                .map(|(hand, _)| *hand)
                .collect::<BTreeSet<_>>()
                .len(),
            strategy_slots: slots
                .iter()
                .map(|(_, key)| *key)
                .collect::<BTreeSet<_>>()
                .len(),
            unrecorded: *self
                .unrecorded
                .lock()
                .map_err(|_| "the shadow count is poisoned")?,
            disagreements,
            examples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_order_preserving_relabeling_has_no_faults_and_a_swap_or_merge_does() {
        let mut same = vec![(1, 10), (2, 20), (3, 30)];
        assert!(
            adjacent_disagreements(&mut same, |a, b| a.1.cmp(&b.1), |a, b| a.0.cmp(&b.0))
                .is_empty()
        );
        let mut swapped = vec![(1, 20), (2, 10), (3, 30)];
        assert_eq!(
            adjacent_disagreements(&mut swapped, |a, b| a.1.cmp(&b.1), |a, b| a.0.cmp(&b.0)).len(),
            1
        );
    }

    #[test]
    fn a_cell_that_splits_or_joins_is_a_fault() {
        let bijection: BTreeSet<_> = [(1, 'a'), (2, 'b')].into_iter().collect();
        assert!(not_one_to_one(&bijection).is_empty());
        let split: BTreeSet<_> = [(1, 'a'), (1, 'b'), (2, 'c')].into_iter().collect();
        assert_eq!(not_one_to_one(&split), vec![(1, 'a'), (1, 'b')]);
    }
}
