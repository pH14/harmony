// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    mem::size_of,
    ops::Bound,
};

use super::{
    archive::{ArchiveKey, Position, Slot, position_of, slot_of_key},
    heap,
};

pub const LABEL_IDENTIFIER: &str = "slot_labels_edge_reliability_v1";

pub const MAX_LABEL_PREFERENCES: usize = 8;

pub const RELAXATION_FLOOR: usize = 8;

pub const RELAXATION_QUEUE_SHARE: usize = 64;

const UNRELIABLE_IN: u32 = 4;

pub type Hop<K> = (Position<K>, Position<K>);

pub type Route<K> = (Slot<K>, Vec<Hop<K>>);

#[must_use]
pub fn slot_position<K: ArchiveKey>(slot: Slot<K>) -> Position<K> {
    ((slot.0).1, slot.1)
}

#[must_use]
pub fn label_preferences<K: ArchiveKey>() -> usize {
    K::preferences().min(MAX_LABEL_PREFERENCES)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound(
    serialize = "A: Serialize, K: Serialize",
    deserialize = "A: DeserializeOwned, K: DeserializeOwned"
))]
pub struct LabelEdge<A, K> {
    pub donor: u64,
    pub leaf: u64,
    pub donor_key: K,
    pub leaf_key: K,
    pub cost: u64,
    pub actions: Vec<A>,
    pub attempted: u32,
    pub landed: u32,
}

impl<A, K> LabelEdge<A, K> {
    #[must_use]
    pub fn weight(&self) -> (u32, u32) {
        (
            self.landed.saturating_add(1),
            self.attempted.saturating_add(2),
        )
    }

    #[must_use]
    pub fn carries(&self) -> bool {
        let (landed, attempted) = self.weight();
        u64::from(landed).saturating_mul(u64::from(UNRELIABLE_IN)) >= u64::from(attempted)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "")]
pub struct LabelEntry<K: ArchiveKey> {
    pub key: K,
    pub parent: Slot<K>,
    pub cost: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "")]
pub struct Label<K: ArchiveKey> {
    entries: Vec<Option<LabelEntry<K>>>,
}

impl<K: ArchiveKey> Label<K> {
    #[must_use]
    pub fn entry(&self, preference: usize) -> Option<&LabelEntry<K>> {
        self.entries.get(preference).and_then(Option::as_ref)
    }

    #[must_use]
    pub fn entries(&self) -> &[Option<LabelEntry<K>>] {
        &self.entries
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct LabelAccounting {
    pub edges: usize,
    pub labels: usize,
    pub queued: usize,
    pub relaxations: u64,
    pub examinations: u64,
    pub changed: u64,
    pub realized: u64,
    pub hops_attempted: u64,
    pub hops_landed: u64,
    pub arrived: u64,
    pub replaced: u64,
    pub useful: u64,
    pub execution_work: u64,
}

type Cursor<K> = (Position<K>, Option<Slot<K>>);

#[derive(Deserialize, Serialize)]
#[serde(bound(serialize = "K: Serialize", deserialize = "K: DeserializeOwned"))]
struct Recomputation<K: ArchiveKey> {
    target: Slot<K>,
    after: Option<Cursor<K>>,
    best: Vec<Option<LabelEntry<K>>>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound(
    serialize = "A: Serialize, K: Serialize",
    deserialize = "A: DeserializeOwned, K: DeserializeOwned"
))]
pub struct LabelGraph<A, K: ArchiveKey> {
    longest_edge: usize,
    edges: BTreeMap<(Position<K>, Position<K>), LabelEdge<A, K>>,
    exits: BTreeMap<Position<K>, BTreeSet<Position<K>>>,
    entrances: BTreeMap<Position<K>, BTreeSet<Position<K>>>,
    occupied: BTreeMap<Position<K>, BTreeSet<K::Progress>>,
    labelled: BTreeMap<Position<K>, BTreeSet<K::Progress>>,
    labels: BTreeMap<Slot<K>, Label<K>>,
    queue: BTreeSet<(u8, u64, Slot<K>)>,
    queued: BTreeMap<Slot<K>, (u8, u64)>,
    stale: BTreeSet<Slot<K>>,
    forget: BTreeSet<Slot<K>>,
    forgetting: Option<(Slot<K>, Cursor<K>)>,
    recomputing: Option<Recomputation<K>>,
    scanning: Option<(Slot<K>, Position<K>)>,
    edge_actions: usize,
    relaxations: u64,
    examinations: u64,
    changed: u64,
}

impl<A: Clone, K: ArchiveKey> LabelGraph<A, K> {
    #[must_use]
    pub fn new(longest_edge: usize) -> Self {
        Self {
            longest_edge,
            edges: BTreeMap::new(),
            exits: BTreeMap::new(),
            entrances: BTreeMap::new(),
            occupied: BTreeMap::new(),
            labelled: BTreeMap::new(),
            labels: BTreeMap::new(),
            queue: BTreeSet::new(),
            queued: BTreeMap::new(),
            stale: BTreeSet::new(),
            forget: BTreeSet::new(),
            forgetting: None,
            recomputing: None,
            scanning: None,
            edge_actions: 0,
            relaxations: 0,
            examinations: 0,
            changed: 0,
        }
    }

    #[must_use]
    pub fn longest_edge(&self) -> usize {
        self.longest_edge
    }

    pub fn reset_labels(&mut self) {
        self.labels.clear();
        self.labelled.clear();
        self.queue.clear();
        self.queued.clear();
        self.forget.clear();
        self.forgetting = None;
        self.recomputing = None;
        self.scanning = None;
        self.stale = self
            .occupied
            .iter()
            .flat_map(|(position, tiers)| {
                tiers
                    .iter()
                    .map(move |tier| ((*tier, position.0), position.1))
            })
            .collect();
    }

    pub fn set_longest_edge(&mut self, longest_edge: usize) {
        self.longest_edge = longest_edge;
    }

    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        let edges = self.edges.len();
        let slots = self.occupied.values().map(BTreeSet::len).sum::<usize>()
            + self.labelled.values().map(BTreeSet::len).sum::<usize>();
        self.edge_actions
            .saturating_mul(size_of::<A>())
            .saturating_add(edges.saturating_mul(heap::MIN_BLOCK))
            .saturating_add(heap::btree_bytes::<
                (Position<K>, Position<K>),
                LabelEdge<A, K>,
            >(1, edges))
            .saturating_add(heap::btree_bytes::<Position<K>, BTreeSet<Position<K>>>(
                1,
                self.exits.len() + self.entrances.len(),
            ))
            .saturating_add(heap::btree_bytes::<Position<K>, ()>(
                self.exits.len() + self.entrances.len(),
                edges.saturating_mul(2),
            ))
            .saturating_add(heap::btree_bytes::<Position<K>, BTreeSet<K::Progress>>(
                1,
                self.occupied.len() + self.labelled.len(),
            ))
            .saturating_add(heap::btree_bytes::<K::Progress, ()>(
                self.occupied.len() + self.labelled.len(),
                slots,
            ))
            .saturating_add(heap::btree_bytes::<Slot<K>, Label<K>>(1, self.labels.len()))
            .saturating_add(
                self.labels
                    .len()
                    .saturating_mul(label_preferences::<K>())
                    .saturating_mul(size_of::<Option<LabelEntry<K>>>()),
            )
            .saturating_add(heap::btree_bytes::<(u8, u64, Slot<K>), ()>(
                1,
                self.queue.len(),
            ))
            .saturating_add(heap::btree_bytes::<Slot<K>, (u8, u64)>(
                1,
                self.queued.len(),
            ))
            .saturating_add(heap::btree_bytes::<Slot<K>, ()>(
                1,
                self.stale.len() + self.forget.len(),
            ))
    }

    #[must_use]
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    #[must_use]
    pub fn label_count(&self) -> usize {
        self.labels.len()
    }

    #[must_use]
    pub fn queue_len(&self) -> usize {
        self.queue.len()
            + self.stale.len()
            + self.forget.len()
            + usize::from(self.forgetting.is_some())
            + usize::from(self.recomputing.is_some())
            + usize::from(self.scanning.is_some())
    }

    #[must_use]
    pub fn relaxation_budget(&self) -> usize {
        RELAXATION_FLOOR.max(self.queue.len() / RELAXATION_QUEUE_SHARE)
    }

    #[must_use]
    pub fn accounting(&self) -> LabelAccounting {
        LabelAccounting {
            edges: self.edges.len(),
            labels: self.labels.len(),
            queued: self.queue_len(),
            relaxations: self.relaxations,
            examinations: self.examinations,
            changed: self.changed,
            ..LabelAccounting::default()
        }
    }

    #[must_use]
    pub fn edge(&self, from: Position<K>, to: Position<K>) -> Option<&LabelEdge<A, K>> {
        self.edges.get(&(from, to))
    }

    #[must_use]
    pub fn label(&self, slot: Slot<K>) -> Option<&Label<K>> {
        self.labels.get(&slot)
    }

    pub fn labels(&self) -> impl Iterator<Item = (&Slot<K>, &Label<K>)> {
        self.labels.iter()
    }

    pub fn occupied_at(&self, position: Position<K>) -> impl Iterator<Item = Slot<K>> + '_ {
        self.occupied
            .get(&position)
            .into_iter()
            .flat_map(move |tiers| {
                tiers
                    .iter()
                    .map(move |tier| ((*tier, position.0), position.1))
            })
    }

    pub fn record(
        &mut self,
        from_key: K,
        to_key: K,
        donor: u64,
        leaf: u64,
        actions: &[A],
        cost: u64,
    ) {
        let from = position_of(from_key);
        let to = position_of(to_key);
        if from == to || actions.len() > self.longest_edge {
            return;
        }
        let gains = (0..label_preferences::<K>())
            .find(|preference| to_key.preference_cmp(*preference, from_key) == Ordering::Greater)
            .map_or(u8::MAX, |preference| {
                u8::try_from(preference).unwrap_or(u8::MAX)
            });
        match self.edges.get_mut(&(from, to)) {
            Some(held) => {
                if cost >= held.cost {
                    return;
                }
                self.edge_actions = self
                    .edge_actions
                    .saturating_sub(held.actions.len())
                    .saturating_add(actions.len());
                held.donor = donor;
                held.leaf = leaf;
                held.donor_key = from_key;
                held.leaf_key = to_key;
                held.cost = cost;
                held.actions = actions.to_vec();
                self.edge_weakened(from, to);
            }
            None => {
                self.edge_actions = self.edge_actions.saturating_add(actions.len());
                self.edges.insert(
                    (from, to),
                    LabelEdge {
                        donor,
                        leaf,
                        donor_key: from_key,
                        leaf_key: to_key,
                        cost,
                        actions: actions.to_vec(),
                        attempted: 0,
                        landed: 0,
                    },
                );
                self.exits.entry(from).or_default().insert(to);
                self.entrances.entry(to).or_default().insert(from);
            }
        }
        self.push(slot_of_key(from_key), gains, 0);
    }

    pub fn holder_changed(&mut self, key: K, preference: Option<usize>) {
        let slot = slot_of_key(key);
        self.occupied
            .entry(slot_position::<K>(slot))
            .or_default()
            .insert((slot.0).0);
        let preference = preference.map_or(u8::MAX, |preference| {
            u8::try_from(preference).unwrap_or(u8::MAX)
        });
        self.push(slot, preference, 0);
    }

    pub fn holder_removed(&mut self, key: K) {
        let slot = slot_of_key(key);
        let position = slot_position::<K>(slot);
        if let Some(tiers) = self.occupied.get_mut(&position) {
            tiers.remove(&(slot.0).0);
            if tiers.is_empty() {
                self.occupied.remove(&position);
            }
        }
        self.forget.insert(slot);
        self.stale.insert(slot);
    }

    pub fn holder_demoted(&mut self, key: K) {
        let slot = slot_of_key(key);
        self.forget.insert(slot);
        self.stale.insert(slot);
    }

    fn edge_weakened(&mut self, from: Position<K>, to: Position<K>) {
        let targets: Vec<Slot<K>> = self.slots_at(to).collect();
        for target in targets {
            if self.clear_entries(target, |parent| slot_position::<K>(parent) == from) {
                self.forget.insert(target);
            }
            self.stale.insert(target);
        }
    }

    pub fn transfer_attempted(&mut self, from: Position<K>, to: Position<K>, landed: bool) {
        let Some(edge) = self.edges.get_mut(&(from, to)) else {
            return;
        };
        edge.attempted = edge.attempted.saturating_add(1);
        if landed {
            edge.landed = edge.landed.saturating_add(1);
            let targets: Vec<Slot<K>> = self.slots_at(to).collect();
            self.stale.extend(targets);
        } else {
            self.edge_weakened(from, to);
        }
    }

    fn slots_at(&self, position: Position<K>) -> impl Iterator<Item = Slot<K>> + '_ {
        let occupied = self.occupied.get(&position).into_iter().flatten();
        let labelled = self.labelled.get(&position).into_iter().flatten();
        occupied
            .chain(labelled)
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(move |tier| ((tier, position.0), position.1))
    }

    fn push(&mut self, slot: Slot<K>, preference: u8, cost: u64) {
        let key = (preference, cost);
        match self.queued.get(&slot).copied() {
            Some(held) if held <= key => {}
            Some(held) => {
                self.queue.remove(&(held.0, held.1, slot));
                self.queue.insert((preference, cost, slot));
                self.queued.insert(slot, key);
            }
            None => {
                self.queue.insert((preference, cost, slot));
                self.queued.insert(slot, key);
            }
        }
    }

    fn pop(&mut self) -> Option<Slot<K>> {
        let (preference, cost, slot) = self.queue.pop_first()?;
        debug_assert_eq!(self.queued.get(&slot), Some(&(preference, cost)));
        self.queued.remove(&slot);
        Some(slot)
    }

    fn target_progress(source: K::Progress, edge: &LabelEdge<A, K>) -> K::Progress {
        if edge.donor_key.progress() == edge.leaf_key.progress() {
            source
        } else {
            source.max(edge.leaf_key.progress())
        }
    }

    fn source(
        &self,
        slot: Slot<K>,
        preference: usize,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
    ) -> Option<(K, u64)> {
        let holder = best_holder(slot, preference);
        let label = self
            .labels
            .get(&slot)
            .and_then(|label| label.entry(preference).copied());
        match (holder, label) {
            (Some(holder), Some(label)) => {
                if label.key.preference_cmp(preference, holder) == Ordering::Greater {
                    Some((label.key, label.cost))
                } else {
                    Some((holder, 0))
                }
            }
            (Some(holder), None) => Some((holder, 0)),
            (None, Some(label)) => Some((label.key, label.cost)),
            (None, None) => None,
        }
    }

    fn better(preference: usize, candidate: &LabelEntry<K>, held: Option<&LabelEntry<K>>) -> bool {
        match held {
            None => true,
            Some(held) => match candidate.key.preference_cmp(preference, held.key) {
                Ordering::Greater => true,
                Ordering::Less => false,
                Ordering::Equal => (candidate.cost, candidate.parent) < (held.cost, held.parent),
            },
        }
    }

    fn recompute(
        &mut self,
        target: Slot<K>,
        after: Option<Cursor<K>>,
        mut best: Vec<Option<LabelEntry<K>>>,
        remaining: usize,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
        raisable: &dyn Fn(Slot<K>) -> bool,
    ) -> usize {
        let position = slot_position::<K>(target);
        let (work, more) =
            self.work_after(self.entrances.get(&position), after, remaining, |from| {
                self.edges
                    .get(&(from, position))
                    .is_some_and(LabelEdge::carries)
            });
        let charged = work.len();
        let last = work.last().copied();
        for (from, source) in work {
            let Some(source) = source else {
                continue;
            };
            let Some(edge) = self.edges.get(&(from, position)) else {
                continue;
            };
            if Self::target_progress((source.0).0, edge) != (target.0).0 || source == target {
                continue;
            }
            self.examinations = self.examinations.saturating_add(1);
            for (preference, best) in best.iter_mut().enumerate() {
                let Some((source_key, source_cost)) = self.source(source, preference, best_holder)
                else {
                    continue;
                };
                if source_key.preference_cmp(preference, edge.donor_key) == Ordering::Less {
                    continue;
                }
                let candidate = LabelEntry {
                    key: source_key.carry(edge.donor_key, edge.leaf_key, edge.weight()),
                    parent: source,
                    cost: source_cost.saturating_add(edge.cost),
                };
                if Self::better(preference, &candidate, best.as_ref()) {
                    *best = Some(candidate);
                }
            }
        }
        if more && let Some(last) = last {
            self.recomputing = Some(Recomputation {
                target,
                after: Some(last),
                best,
            });
            return charged;
        }
        for (preference, best) in best.iter_mut().enumerate() {
            if let (Some(candidate), Some(holder)) =
                (best.as_ref(), best_holder(target, preference))
                && candidate.key.preference_cmp(preference, holder) != Ordering::Greater
            {
                *best = None;
            }
        }
        self.apply(target, best, raisable);
        charged.max(1)
    }

    fn apply(
        &mut self,
        target: Slot<K>,
        mut best: Vec<Option<LabelEntry<K>>>,
        raisable: &dyn Fn(Slot<K>) -> bool,
    ) -> bool {
        let position = slot_position::<K>(target);
        let preferences = best.len();
        let held = self.labels.get(&target);
        if !raisable(target) {
            for (preference, best) in best.iter_mut().enumerate() {
                let old = held.and_then(|label| label.entry(preference));
                if let (Some(candidate), Some(old)) = (best.as_ref(), old)
                    && candidate.key.preference_cmp(preference, old.key) == Ordering::Greater
                {
                    *best = Some(*old);
                } else if let (Some(_), None) = (best.as_ref(), old) {
                    *best = None;
                }
            }
        }
        let unchanged = match held {
            Some(label) => label.entries == best,
            None => best.iter().all(Option::is_none),
        };
        if unchanged {
            return false;
        }
        let lowered = (0..preferences).any(|preference| {
            match (
                held.and_then(|label| label.entry(preference)),
                &best[preference],
            ) {
                (Some(_), None) => true,
                (Some(old), Some(new)) => {
                    new.key.preference_cmp(preference, old.key) == Ordering::Less
                }
                (None, _) => false,
            }
        });
        let changed_preference = (0..preferences)
            .find(|preference| {
                held.and_then(|label| label.entry(*preference)) != best[*preference].as_ref()
            })
            .map_or(u8::MAX, |preference| {
                u8::try_from(preference).unwrap_or(u8::MAX)
            });
        let cost = best
            .iter()
            .flatten()
            .map(|entry| entry.cost)
            .min()
            .unwrap_or(0);
        self.changed = self.changed.saturating_add(1);
        if best.iter().all(Option::is_none) {
            self.labels.remove(&target);
            if let Some(tiers) = self.labelled.get_mut(&position) {
                tiers.remove(&(target.0).0);
                if tiers.is_empty() {
                    self.labelled.remove(&position);
                }
            }
        } else {
            self.labels.insert(target, Label { entries: best });
            self.labelled
                .entry(position)
                .or_default()
                .insert((target.0).0);
        }
        if lowered {
            self.forget.insert(target);
        }
        self.push(target, changed_preference, cost);
        true
    }

    fn improve(
        &mut self,
        source: Slot<K>,
        to: Position<K>,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
        raisable: &dyn Fn(Slot<K>) -> bool,
    ) {
        let position = slot_position::<K>(source);
        let Some(edge) = self.edges.get(&(position, to)) else {
            return;
        };
        if !edge.carries() {
            return;
        }
        let (donor_key, leaf_key, weight, edge_cost) =
            (edge.donor_key, edge.leaf_key, edge.weight(), edge.cost);
        let target = ((Self::target_progress((source.0).0, edge), to.0), to.1);
        if target == source {
            return;
        }
        let preferences = label_preferences::<K>();
        let mut best: Vec<Option<LabelEntry<K>>> = self
            .labels
            .get(&target)
            .map_or_else(|| vec![None; preferences], |label| label.entries.clone());
        for (preference, best) in best.iter_mut().enumerate() {
            let Some((source_key, source_cost)) = self.source(source, preference, best_holder)
            else {
                continue;
            };
            if source_key.preference_cmp(preference, donor_key) == Ordering::Less {
                continue;
            }
            let candidate = LabelEntry {
                key: source_key.carry(donor_key, leaf_key, weight),
                parent: source,
                cost: source_cost.saturating_add(edge_cost),
            };
            if let Some(holder) = best_holder(target, preference)
                && candidate.key.preference_cmp(preference, holder) != Ordering::Greater
            {
                continue;
            }
            if Self::better(preference, &candidate, best.as_ref()) {
                *best = Some(candidate);
            }
        }
        self.examinations = self.examinations.saturating_add(1);
        self.apply(target, best, raisable);
    }

    pub fn relax(
        &mut self,
        budget: usize,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
        raisable: &dyn Fn(Slot<K>) -> bool,
    ) -> usize {
        let mut examined = 0_usize;
        while examined < budget {
            let remaining = budget - examined;
            let forgetting = match self.forgetting.take() {
                Some((slot, cursor)) => Some((slot, Some(cursor))),
                None => self.forget.pop_first().map(|slot| (slot, None)),
            };
            if let Some((slot, after)) = forgetting {
                if let Some(recomputation) = self.recomputing.take() {
                    self.stale.insert(recomputation.target);
                }
                self.relaxations = self.relaxations.saturating_add(1);
                examined = examined.saturating_add(self.forget_exits(slot, after, remaining));
                continue;
            }
            let recomputation = match self.recomputing.take() {
                Some(recomputation) => Some(recomputation),
                None => self.stale.pop_first().map(|target| Recomputation {
                    target,
                    after: None,
                    best: vec![None; label_preferences::<K>()],
                }),
            };
            if let Some(Recomputation {
                target,
                after,
                best,
            }) = recomputation
            {
                self.relaxations = self.relaxations.saturating_add(1);
                examined = examined.saturating_add(self.recompute(
                    target,
                    after,
                    best,
                    remaining,
                    best_holder,
                    raisable,
                ));
                continue;
            }
            let (slot, after) = match self.scanning.take() {
                Some((slot, last)) => (slot, Some(last)),
                None => {
                    let Some(slot) = self.pop() else {
                        break;
                    };
                    self.relaxations = self.relaxations.saturating_add(1);
                    (slot, None)
                }
            };
            let exits = self.exits_after(slot_position::<K>(slot), after, remaining);
            if exits.is_empty() {
                examined = examined.saturating_add(1);
            }
            if exits.len() > remaining {
                self.scanning = Some((slot, exits[remaining - 1]));
            }
            for to in exits.into_iter().take(remaining) {
                self.improve(slot, to, best_holder, raisable);
                examined = examined.saturating_add(1);
            }
        }
        examined
    }

    fn exits_after(
        &self,
        position: Position<K>,
        after: Option<Position<K>>,
        remaining: usize,
    ) -> Vec<Position<K>> {
        self.exits
            .get(&position)
            .map(|to| {
                let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
                to.range((lower, Bound::Unbounded))
                    .take(remaining.saturating_add(1))
                    .copied()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn forget_exits(&mut self, slot: Slot<K>, after: Option<Cursor<K>>, remaining: usize) -> usize {
        let (work, more) = self.work_after(
            self.exits.get(&slot_position::<K>(slot)),
            after,
            remaining,
            |_| true,
        );
        if more && let Some(last) = work.last().copied() {
            self.forgetting = Some((slot, last));
        }
        let charged = work.len().max(1);
        for (_, target) in work {
            let Some(target) = target else {
                continue;
            };
            if self.clear_entries(target, |parent| parent == slot) {
                self.forget.insert(target);
            }
            self.stale.insert(target);
        }
        charged
    }

    fn slots_after(
        &self,
        position: Position<K>,
        after: Option<K::Progress>,
    ) -> impl Iterator<Item = Slot<K>> + '_ {
        let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut occupied = self
            .occupied
            .get(&position)
            .into_iter()
            .flat_map(move |tiers| tiers.range((lower, Bound::Unbounded)))
            .copied()
            .peekable();
        let mut labelled = self
            .labelled
            .get(&position)
            .into_iter()
            .flat_map(move |tiers| tiers.range((lower, Bound::Unbounded)))
            .copied()
            .peekable();
        std::iter::from_fn(move || {
            let tier = match (occupied.peek().copied(), labelled.peek().copied()) {
                (Some(left), Some(right)) => match left.cmp(&right) {
                    Ordering::Less => occupied.next(),
                    Ordering::Greater => labelled.next(),
                    Ordering::Equal => {
                        labelled.next();
                        occupied.next()
                    }
                },
                (Some(_), None) => occupied.next(),
                (None, Some(_)) => labelled.next(),
                (None, None) => None,
            }?;
            Some(((tier, position.0), position.1))
        })
    }

    fn work_after(
        &self,
        neighbours: Option<&BTreeSet<Position<K>>>,
        after: Option<Cursor<K>>,
        remaining: usize,
        expand: impl Fn(Position<K>) -> bool,
    ) -> (Vec<Cursor<K>>, bool) {
        let mut work = Vec::new();
        let Some(neighbours) = neighbours else {
            return (work, false);
        };
        let lower = match after {
            None => Bound::Unbounded,
            Some((position, None)) => Bound::Excluded(position),
            Some((position, Some(_))) => Bound::Included(position),
        };
        for neighbour in neighbours.range((lower, Bound::Unbounded)).copied() {
            let resumed = after
                .and_then(|(position, slot)| (position == neighbour).then_some(slot).flatten());
            if expand(neighbour) {
                let mut found = false;
                for slot in self.slots_after(neighbour, resumed.map(|slot| (slot.0).0)) {
                    if work.len() == remaining {
                        return (work, true);
                    }
                    work.push((neighbour, Some(slot)));
                    found = true;
                }
                if found {
                    continue;
                }
            }
            if resumed.is_some() {
                continue;
            }
            if work.len() == remaining {
                return (work, true);
            }
            work.push((neighbour, None));
        }
        (work, false)
    }

    pub fn relax_fully(
        &mut self,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
        raisable: &dyn Fn(Slot<K>) -> bool,
    ) -> usize {
        let mut examined = 0_usize;
        while self.queue_len() > 0 {
            examined = examined.saturating_add(self.relax(usize::MAX, best_holder, raisable));
        }
        examined
    }

    #[must_use]
    pub fn dominating(
        &self,
        slot: Slot<K>,
        preference: usize,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
    ) -> Option<&LabelEntry<K>> {
        let entry = self.labels.get(&slot)?.entry(preference)?;
        match best_holder(slot, preference) {
            Some(holder) if entry.key.preference_cmp(preference, holder) != Ordering::Greater => {
                None
            }
            _ => Some(entry),
        }
    }

    #[must_use]
    pub fn route(
        &self,
        slot: Slot<K>,
        preference: usize,
        best_holder: &dyn Fn(Slot<K>, usize) -> Option<K>,
    ) -> Option<Route<K>> {
        let mut hops = Vec::new();
        let mut current = slot;
        let mut visited = BTreeSet::new();
        loop {
            if !visited.insert(current) {
                return None;
            }
            let entry = self.labels.get(&current)?.entry(preference)?;
            let parent = entry.parent;
            hops.push((slot_position::<K>(parent), slot_position::<K>(current)));
            let holder = best_holder(parent, preference);
            let label = self
                .labels
                .get(&parent)
                .and_then(|label| label.entry(preference));
            match (holder, label) {
                (Some(holder), Some(label))
                    if label.key.preference_cmp(preference, holder) == Ordering::Greater =>
                {
                    current = parent;
                }
                (Some(_), _) => {
                    hops.reverse();
                    return Some((parent, hops));
                }
                (None, Some(_)) => current = parent,
                (None, None) => return None,
            }
        }
    }

    pub fn retain(&mut self, live: &BTreeSet<Position<K>>) {
        let stale: Vec<Position<K>> = self
            .exits
            .keys()
            .chain(self.entrances.keys())
            .chain(self.occupied.keys())
            .chain(self.labelled.keys())
            .copied()
            .filter(|position| !live.contains(position))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        for position in stale {
            self.remove_position(position);
        }
    }

    pub fn remove_position(&mut self, position: Position<K>) {
        for to in self.exits.remove(&position).unwrap_or_default() {
            self.drop_edge(position, to);
            if let Some(sources) = self.entrances.get_mut(&to) {
                sources.remove(&position);
                if sources.is_empty() {
                    self.entrances.remove(&to);
                }
            }
            let targets: Vec<Slot<K>> = self.slots_at(to).collect();
            for slot in targets {
                if self.clear_entries(slot, |parent| slot_position::<K>(parent) == position) {
                    self.forget.insert(slot);
                }
                self.stale.insert(slot);
            }
        }
        for from in self.entrances.remove(&position).unwrap_or_default() {
            self.drop_edge(from, position);
            if let Some(targets) = self.exits.get_mut(&from) {
                targets.remove(&position);
                if targets.is_empty() {
                    self.exits.remove(&from);
                }
            }
        }
        if self
            .scanning
            .is_some_and(|(slot, _)| slot_position::<K>(slot) == position)
        {
            self.scanning = None;
        }
        if self
            .forgetting
            .is_some_and(|(slot, _)| slot_position::<K>(slot) == position)
        {
            self.forgetting = None;
        }
        if self
            .recomputing
            .as_ref()
            .is_some_and(|recomputation| slot_position::<K>(recomputation.target) == position)
        {
            self.recomputing = None;
        }
        let slots: Vec<Slot<K>> = self.slots_at(position).collect();
        self.occupied.remove(&position);
        self.labelled.remove(&position);
        for slot in slots {
            self.labels.remove(&slot);
            self.stale.remove(&slot);
            self.forget.remove(&slot);
            if let Some((preference, cost)) = self.queued.remove(&slot) {
                self.queue.remove(&(preference, cost, slot));
            }
        }
    }

    fn clear_entries(&mut self, slot: Slot<K>, matches: impl Fn(Slot<K>) -> bool) -> bool {
        let Some(label) = self.labels.get_mut(&slot) else {
            return false;
        };
        let mut cleared = false;
        for entry in &mut label.entries {
            if entry.is_some_and(|entry| matches(entry.parent)) {
                *entry = None;
                cleared = true;
            }
        }
        let position = slot_position::<K>(slot);
        if label.entries.iter().all(Option::is_none) {
            self.labels.remove(&slot);
            if let Some(tiers) = self.labelled.get_mut(&position) {
                tiers.remove(&(slot.0).0);
                if tiers.is_empty() {
                    self.labelled.remove(&position);
                }
            }
        }
        cleared
    }

    fn drop_edge(&mut self, from: Position<K>, to: Position<K>) {
        if let Some(edge) = self.edges.remove(&(from, to)) {
            self.edge_actions = self.edge_actions.saturating_sub(edge.actions.len());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct Key {
        tier: u8,
        place: u8,
        stock: i32,
    }

    fn key(tier: u8, place: u8, stock: i32) -> Key {
        Key { tier, place, stock }
    }

    impl ArchiveKey for Key {
        type Place = u8;
        type Progress = u8;
        type Identity = ();
        fn place(self) -> u8 {
            self.place
        }
        fn progress(self) -> u8 {
            self.tier
        }
        fn identity(self) {}
        fn preferences() -> usize {
            1
        }
        fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
            self.stock.cmp(&other.stock)
        }
        fn carry(self, donor: Self, leaf: Self, weight: (u32, u32)) -> Self {
            let change = i64::from(leaf.stock) - i64::from(donor.stock);
            let change = if change > 0 {
                change * i64::from(weight.0) / i64::from(weight.1)
            } else {
                change
            };
            Key {
                tier: self.tier.max(leaf.tier),
                place: leaf.place,
                stock: i32::try_from(i64::from(self.stock) + change).unwrap_or(i32::MAX),
            }
        }
        type Lineage = ();
        fn complete(self, _parent: Option<(Self, &())>) -> Self {
            self
        }
        fn record(_lineage: &mut (), _key: Self) {}
    }

    fn holders(table: &BTreeMap<Slot<Key>, Key>) -> impl Fn(Slot<Key>, usize) -> Option<Key> + '_ {
        move |slot, _| table.get(&slot).copied()
    }

    fn always(_: Slot<Key>) -> bool {
        true
    }

    fn graph_with_route() -> (LabelGraph<u8, Key>, BTreeMap<Slot<Key>, Key>) {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let a = key(0, 1, 10);
        let b = key(0, 2, 7);
        let c = key(0, 3, 4);
        for k in [a, b, c] {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        graph.record(a, b, 1, 2, &[1, 2], 2);
        graph.record(b, c, 2, 3, &[3], 1);
        (graph, table)
    }

    #[test]
    fn a_better_source_predicts_a_better_arrival_downstream() {
        let (mut graph, mut table) = graph_with_route();
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 0);
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        let at_c = graph
            .dominating(slot_of_key(key(0, 3, 0)), 0, &holders(&table))
            .expect("label beats the holder");
        assert_eq!(at_c.key.stock, 24);
        assert_eq!(at_c.cost, 3);
        let (root, hops) = graph
            .route(slot_of_key(key(0, 3, 0)), 0, &holders(&table))
            .expect("route");
        assert_eq!(root, slot_of_key(improved));
        assert_eq!(hops.len(), 2);
    }

    #[test]
    fn an_unreliable_edge_stops_carrying_and_a_failure_lowers_downstream_labels() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let a = key(0, 1, 10);
        let b = key(0, 2, 20);
        let c = key(0, 3, 15);
        for k in [a, b, c] {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        graph.record(a, b, 1, 2, &[1], 1);
        graph.record(b, c, 2, 3, &[2], 1);
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        let at_b = |graph: &LabelGraph<u8, Key>| {
            graph
                .label(slot_of_key(b))
                .unwrap()
                .entry(0)
                .unwrap()
                .key
                .stock
        };
        assert_eq!(at_b(&graph), 30 + 10 / 2);
        let from = position_of(a);
        let to = position_of(b);
        graph.transfer_attempted(from, to, true);
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(at_b(&graph), 30 + 10 * 2 / 3);
        assert_eq!(
            graph
                .label(slot_of_key(c))
                .unwrap()
                .entry(0)
                .unwrap()
                .key
                .stock,
            30 + 10 * 2 / 3 - 5
        );
        for _ in 0..6 {
            graph.transfer_attempted(from, to, false);
        }
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(b)).is_none());
        assert!(graph.label(slot_of_key(c)).is_none());
        assert_eq!(graph.label_count(), 0);
        assert_eq!(graph.queue_len(), 0);
    }

    #[test]
    fn a_source_below_the_recorded_donor_predicts_nothing() {
        let (mut graph, mut table) = graph_with_route();
        let worse = key(0, 1, 5);
        table.insert(slot_of_key(worse), worse);
        graph.holder_changed(worse, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(key(0, 2, 0))).is_none());
    }

    #[test]
    fn relaxation_obeys_its_budget_and_carries_the_queue() {
        let (mut graph, mut table) = graph_with_route();
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        let examined = graph.relax(1, &holders(&table), &always);
        assert_eq!(examined, 1);
        assert!(graph.queue_len() > 0);
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.queue_len(), 0);
        assert!(graph.label(slot_of_key(key(0, 3, 0))).is_some());
    }

    #[test]
    fn an_unraisable_slot_keeps_its_label_from_rising() {
        let (mut graph, mut table) = graph_with_route();
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        let cold = slot_of_key(key(0, 2, 0));
        graph.relax_fully(&holders(&table), &|slot| slot != cold);
        assert!(graph.label(cold).is_none());
        assert!(graph.label(slot_of_key(key(0, 3, 0))).is_none());
        assert_eq!(graph.label_count(), 0);
    }

    #[test]
    fn an_edge_that_gains_a_tier_targets_the_leaf_tier() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let a = key(0, 1, 10);
        let b = key(1, 2, 10);
        for k in [a, b] {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        graph.record(a, b, 1, 2, &[1], 1);
        let improved = key(0, 1, 20);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        let label = graph.label(slot_of_key(b)).expect("label at the leaf tier");
        assert_eq!(label.entry(0).unwrap().key.stock, 20);
        assert!(graph.label(((0, 2), ())).is_none());
    }

    #[test]
    fn removing_a_position_drops_its_edges_labels_and_queue_entries() {
        let (mut graph, mut table) = graph_with_route();
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        let live: BTreeSet<Position<Key>> = [position_of(key(0, 1, 0)), position_of(key(0, 3, 0))]
            .into_iter()
            .collect();
        graph.retain(&live);
        assert_eq!(graph.edge_count(), 0);
        assert!(graph.label(slot_of_key(key(0, 2, 0))).is_none());
        table.remove(&slot_of_key(key(0, 2, 0)));
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 0);
        assert!(
            graph
                .route(slot_of_key(key(0, 3, 0)), 0, &holders(&table))
                .is_none()
        );
        assert_eq!(graph.queue_len(), 0);
    }

    #[test]
    fn removing_a_position_lowers_labels_beyond_its_neighbours() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let keys = [key(0, 1, 10), key(0, 2, 7), key(0, 3, 4), key(0, 4, 1)];
        for k in keys {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        for (step, pair) in keys.windows(2).enumerate() {
            let step = u64::try_from(step).unwrap();
            graph.record(pair[0], pair[1], step + 1, step + 2, &[1], 1);
        }
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(key(0, 4, 0))).is_some());
        let live: BTreeSet<Position<Key>> = keys[1..].iter().map(|k| position_of(*k)).collect();
        graph.retain(&live);
        table.remove(&slot_of_key(improved));
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 0);
        assert_eq!(graph.queue_len(), 0);
    }

    #[test]
    fn a_wide_exit_scan_resumes_within_the_budget() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let hub = key(0, 1, 10);
        table.insert(slot_of_key(hub), hub);
        graph.holder_changed(hub, Some(0));
        for place in 2..7_u8 {
            let leaf = key(0, place, 5);
            table.insert(slot_of_key(leaf), leaf);
            graph.holder_changed(leaf, Some(0));
            graph.record(hub, leaf, 1, u64::from(place), &[place], 1);
        }
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        let mut whole =
            postcard::from_bytes::<LabelGraph<u8, Key>>(&postcard::to_stdvec(&graph).unwrap())
                .unwrap();
        whole.relax_fully(&holders(&table), &always);
        let mut calls = 0;
        while graph.queue_len() > 0 {
            assert!(graph.relax(2, &holders(&table), &always) <= 2);
            calls += 1;
        }
        assert!(calls >= 3);
        assert_eq!(graph.label_count(), 5);
        assert_eq!(
            postcard::to_stdvec(&graph.labels().collect::<Vec<_>>()).unwrap(),
            postcard::to_stdvec(&whole.labels().collect::<Vec<_>>()).unwrap()
        );
    }

    #[test]
    fn removing_the_root_of_a_cycle_leaves_no_label_supporting_another() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let a = key(0, 1, 10);
        let b = key(0, 2, 10);
        let c = key(0, 3, 10);
        for k in [a, b, c] {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        graph.record(a, b, 1, 2, &[1], 1);
        graph.record(b, c, 2, 3, &[2], 1);
        graph.record(c, b, 3, 4, &[3], 1);
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 2);
        let live: BTreeSet<Position<Key>> = [position_of(b), position_of(c)].into_iter().collect();
        graph.retain(&live);
        table.remove(&slot_of_key(a));
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 0);
        assert_eq!(graph.queue_len(), 0);
    }

    #[test]
    fn a_cheaper_replacement_edge_lowers_the_label_it_predicted() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let a = key(0, 1, 10);
        let b = key(0, 2, 5);
        for k in [a, b] {
            table.insert(slot_of_key(k), k);
            graph.holder_changed(k, Some(0));
        }
        graph.record(a, b, 1, 2, &[1, 2], 2);
        let improved = key(0, 1, 40);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        let at_b = |graph: &LabelGraph<u8, Key>| {
            graph
                .label(slot_of_key(b))
                .unwrap()
                .entry(0)
                .unwrap()
                .key
                .stock
        };
        assert_eq!(at_b(&graph), 35);
        graph.record(key(0, 1, 30), key(0, 2, 6), 3, 4, &[3], 1);
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(at_b(&graph), 16);
    }

    #[test]
    fn a_demoted_holder_lowers_the_labels_it_supported() {
        let (mut graph, mut table) = graph_with_route();
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 2);
        let weaker = key(0, 1, 10);
        table.insert(slot_of_key(weaker), weaker);
        graph.holder_demoted(improved);
        graph.relax_fully(&holders(&table), &always);
        assert_eq!(graph.label_count(), 0);
    }

    #[test]
    fn a_failed_transfer_into_a_wide_entrance_recomputes_within_the_budget() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let hub = key(0, 9, 5);
        table.insert(slot_of_key(hub), hub);
        graph.holder_changed(hub, Some(0));
        for place in 1..7_u8 {
            let source = key(0, place, 10);
            table.insert(slot_of_key(source), source);
            graph.holder_changed(source, Some(0));
            graph.record(source, hub, u64::from(place), 9, &[place], 1);
        }
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(hub)).is_some());
        for _ in 0..8 {
            graph.transfer_attempted(position_of(improved), position_of(hub), false);
        }
        let mut calls = 0;
        while graph.queue_len() > 0 {
            assert!(graph.relax(2, &holders(&table), &always) <= 2);
            calls += 1;
        }
        assert!(calls >= 3);
        assert!(graph.label(slot_of_key(hub)).is_none());
    }

    #[test]
    fn a_demoted_holder_lets_its_own_slot_take_a_label() {
        let (mut graph, mut table) = graph_with_route();
        let strong = key(0, 2, 40);
        table.insert(slot_of_key(strong), strong);
        graph.holder_changed(strong, Some(0));
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(strong)).is_none());
        let weak = key(0, 2, 7);
        table.insert(slot_of_key(weak), weak);
        graph.holder_demoted(strong);
        graph.relax_fully(&holders(&table), &always);
        let label = graph
            .label(slot_of_key(weak))
            .expect("label above the weaker holder");
        assert_eq!(label.entry(0).unwrap().key.stock, 27);
    }

    #[test]
    fn a_recompute_over_many_source_tiers_resumes_within_the_budget() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        let mut table = BTreeMap::new();
        let hub = key(0, 9, 5);
        table.insert(slot_of_key(hub), hub);
        graph.holder_changed(hub, Some(0));
        for tier in 0..6_u8 {
            let source = key(tier, 1, if tier == 0 { 10 } else { 1 });
            table.insert(slot_of_key(source), source);
            graph.holder_changed(source, Some(0));
        }
        graph.record(key(0, 1, 10), hub, 1, 2, &[1], 1);
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax_fully(&holders(&table), &always);
        assert!(graph.label(slot_of_key(hub)).is_some());
        assert_eq!(graph.label_count(), 1);
        graph.transfer_attempted(position_of(improved), position_of(hub), true);
        let mut calls = 0;
        while graph.queue_len() > 0 {
            assert!(graph.relax(2, &holders(&table), &always) <= 2);
            calls += 1;
        }
        assert!(calls >= 3);
        assert!(graph.label(slot_of_key(hub)).is_some());
    }

    #[test]
    fn slots_after_a_tier_merge_held_and_labelled_tiers_in_order() {
        let mut graph = LabelGraph::<u8, Key>::new(8);
        for tier in [0_u8, 2, 4] {
            graph.holder_changed(key(tier, 1, 0), Some(0));
        }
        graph
            .labelled
            .entry(position_of(key(0, 1, 0)))
            .or_default()
            .extend([1_u8, 2, 5]);
        let tiers = |after| {
            graph
                .slots_after(position_of(key(0, 1, 0)), after)
                .map(|slot| (slot.0).0)
                .collect::<Vec<_>>()
        };
        assert_eq!(tiers(None), vec![0, 1, 2, 4, 5]);
        assert_eq!(tiers(Some(2)), vec![4, 5]);
        assert_eq!(tiers(Some(5)), Vec::<u8>::new());
    }

    #[test]
    fn the_graph_round_trips_through_serde() {
        let (mut graph, mut table) = graph_with_route();
        let improved = key(0, 1, 30);
        table.insert(slot_of_key(improved), improved);
        graph.holder_changed(improved, Some(0));
        graph.relax(1, &holders(&table), &always);
        let bytes = postcard::to_stdvec(&graph).unwrap();
        let restored: LabelGraph<u8, Key> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(restored.queue_len(), graph.queue_len());
        assert_eq!(restored.edge_count(), graph.edge_count());
        assert_eq!(postcard::to_stdvec(&restored).unwrap(), bytes);
    }
}
