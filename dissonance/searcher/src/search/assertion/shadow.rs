// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::{self, Debug},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::{Accumulator, AssertionKey, Compiled, KeyView, Record};
use crate::search::archive::ArchiveKey;

type Pairs<H> = BTreeMap<(Option<H>, Option<AssertionKey>), u64>;

struct Counts<H> {
    pairs: Pairs<H>,
    lineage_changes: u64,
    unknown_restores: u64,
    unobserved: u64,
}

pub struct ShadowTable<H: ArchiveKey> {
    counts: Mutex<Counts<H>>,
}

impl<H: ArchiveKey> Default for ShadowTable<H> {
    fn default() -> Self {
        Self {
            counts: Mutex::new(Counts {
                pairs: BTreeMap::new(),
                lineage_changes: 0,
                unknown_restores: 0,
                unobserved: 0,
            }),
        }
    }
}

impl<H: ArchiveKey> Debug for ShadowTable<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ShadowTable")
    }
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
    pub fn total(&self) -> usize {
        usize::try_from(self.absent).unwrap_or(usize::MAX)
            + self.cell
            + self.slot
            + self.progress
            + self.preference
            + usize::from(self.preference_count)
            + usize::from(self.capacity)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShadowTier {
    pub progress: u32,
    pub view: Vec<i64>,
    pub states: u64,
    pub cells: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ShadowReport {
    pub policy: String,
    pub states: u64,
    pub keyed_states: u64,
    pub lineage_changes: u64,
    pub distinct_pairs: usize,
    pub disagreements: Disagreements,
    pub unknown_restores: u64,
    pub unobserved: u64,
    pub examples: Vec<String>,
    pub tiers: Vec<ShadowTier>,
    pub cells: Vec<KeyView>,
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

impl<H: ArchiveKey + Debug> ShadowTable<H> {
    pub fn count(&self, hand: Option<H>, key: Option<AssertionKey>, lineage_changed: bool) {
        if let Ok(mut counts) = self.counts.lock() {
            *counts.pairs.entry((hand, key)).or_default() += 1;
            counts.lineage_changes += u64::from(lineage_changed);
        }
    }

    pub fn unknown_restore(&self) {
        if let Ok(mut counts) = self.counts.lock() {
            counts.unknown_restores += 1;
        }
    }

    pub fn unobserved(&self) {
        if let Ok(mut counts) = self.counts.lock() {
            counts.unobserved += 1;
        }
    }

    pub fn report(&self, compiled: &Compiled) -> Result<ShadowReport, Box<dyn Error>> {
        let counts = self
            .counts
            .lock()
            .map_err(|_| "the shadow key table is poisoned")?;
        let pairs = &counts.pairs;
        let mut disagreements = Disagreements {
            preference_count: H::preferences() != compiled.shape().preferences,
            capacity: H::capacity() != compiled.shape().capacity,
            ..Disagreements::default()
        };
        let mut examples = Vec::new();
        let mut keyed: Vec<(H, AssertionKey)> = Vec::new();
        let mut states = 0;
        let mut keyed_states = 0;
        type Cell = (u32, u64);
        let mut tiers: BTreeMap<u32, (u64, BTreeSet<Cell>)> = BTreeMap::new();
        for ((hand, key), count) in pairs {
            states += count;
            match (hand, key) {
                (Some(hand), Some(key)) => {
                    keyed_states += count;
                    keyed.push((*hand, *key));
                    let tier = tiers.entry(key.progress).or_default();
                    tier.0 += count;
                    tier.1.insert((key.progress, key.place.get()));
                }
                (None, None) => {}
                _ => {
                    disagreements.absent += count;
                    if examples.len() < 8 {
                        examples.push(format!("absent: hand {hand:?} compiled {key:?}"));
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
        let mut by_cell: BTreeMap<Cell, Vec<(H, AssertionKey)>> = BTreeMap::new();
        for (hand, key) in &keyed {
            by_cell
                .entry((key.progress, key.place.get()))
                .or_default()
                .push((*hand, *key));
        }
        let preferences = compiled.shape().preferences.max(H::preferences());
        for held in by_cell.values_mut() {
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
        let views: Vec<KeyView> = keyed
            .iter()
            .map(|(_, key)| (key.progress, key.place))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|(progress, place)| {
                compiled.view(AssertionKey {
                    progress,
                    place,
                    ..keyed[0].1
                })
            })
            .collect();
        Ok(ShadowReport {
            policy: compiled.policy_identifier().to_owned(),
            states,
            keyed_states,
            lineage_changes: counts.lineage_changes,
            distinct_pairs: pairs.len(),
            disagreements,
            unknown_restores: counts.unknown_restores,
            unobserved: counts.unobserved,
            examples,
            tiers: tiers
                .into_iter()
                .map(|(progress, (states, cells))| ShadowTier {
                    progress,
                    view: compiled
                        .view(AssertionKey {
                            progress,
                            ..keyed[0].1
                        })
                        .progress,
                    states,
                    cells: cells.len(),
                })
                .collect(),
            cells: views,
        })
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(bound = "H: ArchiveKey")]
pub struct ShadowCursor<H: ArchiveKey> {
    accumulator: Accumulator,
    key: Option<AssertionKey>,
    previous: Option<H>,
    lineage: H::Lineage,
    fresh: bool,
}

impl<H: ArchiveKey> Default for ShadowCursor<H> {
    fn default() -> Self {
        Self {
            accumulator: Accumulator::default(),
            key: None,
            previous: None,
            lineage: H::Lineage::default(),
            fresh: false,
        }
    }
}

impl<H: ArchiveKey + Debug> Debug for ShadowCursor<H>
where
    H::Lineage: Debug,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShadowCursor")
            .field("accumulator", &self.accumulator)
            .field("key", &self.key)
            .field("previous", &self.previous)
            .field("lineage", &self.lineage)
            .field("fresh", &self.fresh)
            .finish()
    }
}

impl<H: ArchiveKey> PartialEq for ShadowCursor<H>
where
    H::Lineage: PartialEq,
{
    fn eq(&self, other: &Self) -> bool {
        self.accumulator == other.accumulator
            && self.key == other.key
            && self.previous == other.previous
            && self.lineage == other.lineage
            && self.fresh == other.fresh
    }
}

impl<H: ArchiveKey> Eq for ShadowCursor<H> where H::Lineage: Eq {}

impl<H: ArchiveKey + Debug> ShadowCursor<H> {
    pub fn observe(&mut self, compiled: &Compiled, record: &Record) -> Option<AssertionKey> {
        self.arrive(compiled, record, None)
    }

    pub fn observe_with_objective(
        &mut self,
        compiled: &Compiled,
        record: &Record,
        objective: bool,
    ) -> Option<AssertionKey> {
        self.arrive(compiled, record, Some(objective))
    }

    fn arrive(
        &mut self,
        compiled: &Compiled,
        record: &Record,
        objective: Option<bool>,
    ) -> Option<AssertionKey> {
        let observed = compiled.observe(record, &mut self.accumulator);
        let objective = objective.unwrap_or(observed.goal);
        self.key = observed
            .key
            .filter(|_| objective || observed.rejected.is_none());
        self.fresh = true;
        self.key
    }

    pub fn pair(&mut self, table: &ShadowTable<H>, raw: Option<H>) -> Option<H> {
        if !self.fresh {
            let hand = raw.and(self.previous);
            table.count(hand, self.key, false);
            return hand;
        }
        self.fresh = false;
        let Some(raw) = raw else {
            table.count(None, self.key, false);
            return None;
        };
        let completed = raw.complete(self.previous.map(|previous| (previous, &self.lineage)));
        H::record(&mut self.lineage, completed);
        self.previous = Some(completed);
        table.count(Some(completed), self.key, completed != raw);
        Some(completed)
    }

    #[must_use]
    pub fn key(&self) -> Option<AssertionKey> {
        self.key
    }

    #[must_use]
    pub fn heap_bytes(&self) -> usize {
        self.accumulator.heap_bytes()
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
        let mut merged = vec![(1, 10), (2, 10)];
        assert_eq!(
            adjacent_disagreements(&mut merged, |a, b| a.1.cmp(&b.1), |a, b| a.0.cmp(&b.0)).len(),
            1
        );
    }

    #[test]
    fn a_cell_that_splits_or_joins_is_a_fault() {
        let bijection: BTreeSet<_> = [(1, 'a'), (2, 'b')].into_iter().collect();
        assert!(not_one_to_one(&bijection).is_empty());
        let split: BTreeSet<_> = [(1, 'a'), (1, 'b'), (2, 'c')].into_iter().collect();
        assert_eq!(not_one_to_one(&split), vec![(1, 'a'), (1, 'b')]);
        let joined: BTreeSet<_> = [(1, 'a'), (2, 'a')].into_iter().collect();
        assert_eq!(not_one_to_one(&joined).len(), 2);
    }
}
