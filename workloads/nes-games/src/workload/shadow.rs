// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt::Debug,
    hash::{DefaultHasher, Hash, Hasher},
    sync::{Arc, Mutex},
};

use machine::nes::WRAM_SIZE;
use serde::Serialize;

use super::{Game, Lowered};
use crate::search::{
    archive::ArchiveKey,
    assertion::{AssertionKey, KeyView},
};

type Pairs<H> = BTreeMap<(Option<H>, Option<AssertionKey>), u64>;

trait Lane<H>: Send + Sync {
    fn fork(&self) -> Box<dyn Lane<H>>;
    fn observe(&self, hand: Option<H>, ram: &[u8]);
    fn start(&self, ram: &[u8]);
    fn advance(&self, frames: &[[u8; WRAM_SIZE]]);
    fn saved(&self, tag: u64);
    fn restored(&self, tag: u64, ram: &[u8]);
    fn observe_threaded(&self, hand: Option<H>);
}

pub struct ShadowHook<H>(Box<dyn Lane<H>>);

impl<H> Clone for ShadowHook<H> {
    fn clone(&self) -> Self {
        Self(self.0.fork())
    }
}

impl<H> Debug for ShadowHook<H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ShadowHook")
    }
}

impl<H> ShadowHook<H> {
    pub fn observe(&self, hand: Option<H>, ram: &[u8]) {
        self.0.observe(hand, ram);
    }

    pub fn start(&self, ram: &[u8]) {
        self.0.start(ram);
    }

    pub fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        self.0.advance(frames);
    }

    pub fn saved(&self, tag: u64) {
        self.0.saved(tag);
    }

    pub fn restored(&self, tag: u64, ram: &[u8]) {
        self.0.restored(tag, ram);
    }

    pub fn observe_threaded(&self, hand: Option<H>) {
        self.0.observe_threaded(hand);
    }
}

#[must_use]
pub fn snapshot_tag(emulator_state: &[u8], frame_count: u64) -> u64 {
    let mut hasher = DefaultHasher::new();
    emulator_state.hash(&mut hasher);
    frame_count.hash(&mut hasher);
    hasher.finish()
}

struct Threaded<G: Game, H: ArchiveKey> {
    shadow: Arc<Shadow<G, H>>,
    current: Mutex<Option<G::State>>,
}

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Lane<H> for Threaded<G, H> {
    fn fork(&self) -> Box<dyn Lane<H>> {
        Box::new(Self {
            shadow: Arc::clone(&self.shadow),
            current: Mutex::new(None),
        })
    }

    fn observe(&self, hand: Option<H>, ram: &[u8]) {
        let state = self.shadow.lowered.game.decode(None, ram);
        self.shadow.count(hand, &state);
    }

    fn start(&self, ram: &[u8]) {
        if let Ok(mut current) = self.current.lock() {
            *current = Some(self.shadow.lowered.game.decode(None, ram));
        }
    }

    fn advance(&self, frames: &[[u8; WRAM_SIZE]]) {
        let game = &self.shadow.lowered.game;
        if let Ok(mut current) = self.current.lock() {
            for wram in frames {
                *current = Some(game.decode(current.as_ref(), wram));
            }
        }
    }

    fn saved(&self, tag: u64) {
        let Ok(current) = self.current.lock() else {
            return;
        };
        if let (Some(state), Ok(mut saved)) = (current.as_ref(), self.shadow.saved.lock()) {
            saved.insert(tag, state.clone());
        }
    }

    fn restored(&self, tag: u64, ram: &[u8]) {
        let found = self
            .shadow
            .saved
            .lock()
            .ok()
            .and_then(|saved| saved.get(&tag).cloned());
        let state = found.unwrap_or_else(|| {
            if let Ok(mut unknown) = self.shadow.unknown_restores.lock() {
                *unknown += 1;
            }
            self.shadow.lowered.game.decode(None, ram)
        });
        if let Ok(mut current) = self.current.lock() {
            *current = Some(state);
        }
    }

    fn observe_threaded(&self, hand: Option<H>) {
        let state = self.current.lock().ok().and_then(|current| current.clone());
        match state {
            Some(state) => self.shadow.count(hand, &state),
            None => {
                if let Ok(mut unknown) = self.shadow.unknown_restores.lock() {
                    *unknown += 1;
                }
            }
        }
    }
}

pub struct Shadow<G: Game, H: ArchiveKey> {
    lowered: Lowered<G>,
    pairs: Mutex<Pairs<H>>,
    saved: Mutex<BTreeMap<u64, G::State>>,
    unknown_restores: Mutex<u64>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct Disagreements {
    pub absent: u64,
    pub cell: usize,
    pub slot: usize,
    pub progress: usize,
    pub preference: usize,
    pub preference_count: bool,
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
    pub distinct_pairs: usize,
    pub disagreements: Disagreements,
    pub unknown_restores: u64,
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

impl<G: Game, H: ArchiveKey + Debug + Send + Sync + 'static> Shadow<G, H> {
    pub fn new(game: G) -> Result<Arc<Self>, Box<dyn Error>> {
        let lowered = Lowered::new(game, G::STRATEGY)?;
        if !lowered.compiled.is_pure() {
            return Err(format!(
                "{} keeps aggregates or latched stages, so a shadow key needs the parent's accumulator",
                G::NAME
            )
            .into());
        }
        Ok(Arc::new(Self {
            lowered,
            pairs: Mutex::default(),
            saved: Mutex::default(),
            unknown_restores: Mutex::default(),
        }))
    }

    #[must_use]
    pub fn hook(self: &Arc<Self>) -> ShadowHook<H> {
        ShadowHook(Box::new(Threaded {
            shadow: Arc::clone(self),
            current: Mutex::new(None),
        }))
    }

    fn count(&self, hand: Option<H>, state: &G::State) {
        let (record, _) = self.lowered.record(state);
        let key = self.lowered.compiled.key(&record);
        if let Ok(mut pairs) = self.pairs.lock() {
            *pairs.entry((hand, key)).or_default() += 1;
        }
    }

    pub fn report(&self) -> Result<ShadowReport, Box<dyn Error>> {
        let pairs = self
            .pairs
            .lock()
            .map_err(|_| "the shadow key table is poisoned")?
            .clone();
        let compiled = &self.lowered.compiled;
        let mut disagreements = Disagreements {
            preference_count: H::preferences() != compiled.shape().preferences,
            ..Disagreements::default()
        };
        let mut examples = Vec::new();
        let mut keyed: Vec<(H, AssertionKey)> = Vec::new();
        let mut states = 0;
        let mut keyed_states = 0;
        let mut tiers: BTreeMap<u32, (u64, BTreeSet<Cell>)> = BTreeMap::new();
        type Cell = (u32, u64);
        for ((hand, key), count) in &pairs {
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
        let mut by_slot: BTreeMap<(u32, u64, u32), Vec<(H, AssertionKey)>> = BTreeMap::new();
        for (hand, key) in &keyed {
            by_slot
                .entry((key.progress, key.place.get(), key.identity))
                .or_default()
                .push((*hand, *key));
        }
        let preferences = compiled.shape().preferences.max(H::preferences());
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
            distinct_pairs: pairs.len(),
            disagreements,
            unknown_restores: self
                .unknown_restores
                .lock()
                .map_err(|_| "the shadow restore count is poisoned")?
                .to_owned(),
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
