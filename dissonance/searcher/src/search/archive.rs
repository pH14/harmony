// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Debug,
    mem::size_of,
    num::{NonZeroU64, NonZeroUsize},
    sync::Arc,
};

use crate::search::{
    continuation::{Continuation, ContinuationBank},
    draw::SUFFIX_DOUBLING_LIMIT,
    rand::RomuDuoJrRand,
    weighted_set::WeightedSet,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

fn unset_action_cost<A>() -> fn(&A) -> u64 {
    |_| 0
}

fn retain_marked<T>(values: Vec<T>, keep: &[bool]) -> Vec<T> {
    values
        .into_iter()
        .zip(keep)
        .filter_map(|(value, keep)| (*keep).then_some(value))
        .collect()
}

pub trait ArchiveKey: Copy + Ord + Serialize + DeserializeOwned {
    type Place: Copy + Ord + Debug + Serialize + DeserializeOwned;
    type Progress: Copy + Ord + Debug + Serialize + DeserializeOwned;
    type Identity: Copy + Ord + Debug + Serialize + DeserializeOwned;
    fn place(self) -> Self::Place;
    fn progress(self) -> Self::Progress;
    fn identity(self) -> Self::Identity;
    fn capacity() -> usize {
        MAX_ENTRIES_PER_KEY
    }
    fn preferences() -> usize {
        0
    }
    fn tier_rank_shift() -> u32 {
        TIER_RANK_SHIFT
    }
    fn preference_cmp(self, _preference: usize, _other: Self) -> Ordering {
        Ordering::Equal
    }
    type Lineage: Clone + Default + Serialize + DeserializeOwned;
    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self;
    fn record(lineage: &mut Self::Lineage, key: Self);
}

pub type Cell<K> = (<K as ArchiveKey>::Progress, <K as ArchiveKey>::Place);

pub type Slot<K> = (Cell<K>, <K as ArchiveKey>::Identity);

pub type Position<K> = (<K as ArchiveKey>::Place, <K as ArchiveKey>::Identity);

pub type Edge<K> = (Position<K>, Position<K>);

#[must_use]
pub fn cell_of<K: ArchiveKey>(key: K) -> Cell<K> {
    (key.progress(), key.place())
}

#[must_use]
pub fn slot_of_key<K: ArchiveKey>(key: K) -> Slot<K> {
    (cell_of(key), key.identity())
}

#[must_use]
pub fn position_of<K: ArchiveKey>(key: K) -> Position<K> {
    (key.place(), key.identity())
}

fn leaf_order<K: ArchiveKey>(left: (K, usize), right: (K, usize)) -> Ordering {
    left.0
        .progress()
        .cmp(&right.0.progress())
        .then_with(|| left.1.cmp(&right.1))
}

fn leaf_advances<K: ArchiveKey>(leaf: (K, usize), parent: (K, usize)) -> bool {
    leaf_order(leaf, parent) == Ordering::Greater
}

pub const MAX_ARCHIVE_ENTRIES: usize = 4_194_304;
pub const MAX_ENTRIES_PER_KEY: usize = 2;
#[cfg(not(test))]
const HISTORY_COMPACTION_MIN_DROPS: usize = 4_096;
#[cfg(test)]
const HISTORY_COMPACTION_MIN_DROPS: usize = 16;
const HISTORY_COMPACTION_SHARE: usize = 16;
pub const REPLAY_DISTANCE_ACTIONS: usize = 8;
const MAINTENANCE_QUANTUM: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Input<A: Ord> {
    pub actions: Vec<A>,
}

impl<A: Ord> Default for Input<A> {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
        }
    }
}

pub const RETENTION_PROBE_IDENTIFIER: &str = "probe_at_admission";

pub const RETENTION_UNPROBED_IDENTIFIER: &str = "unprobed";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    ProbeAtAdmission,
    Unprobed,
}

#[must_use]
pub fn retention_policy_identifier(policy: RetentionPolicy) -> &'static str {
    match policy {
        RetentionPolicy::ProbeAtAdmission => RETENTION_PROBE_IDENTIFIER,
        RetentionPolicy::Unprobed => RETENTION_UNPROBED_IDENTIFIER,
    }
}

pub fn retention_policy_from_identifier(
    identifier: &str,
) -> Result<RetentionPolicy, Box<dyn Error>> {
    match identifier {
        RETENTION_PROBE_IDENTIFIER => Ok(RetentionPolicy::ProbeAtAdmission),
        RETENTION_UNPROBED_IDENTIFIER => Ok(RetentionPolicy::Unprobed),
        _ => Err(format!("retention policy {identifier} is not recognized").into()),
    }
}

pub const SELECTOR_IDENTIFIER: &str = "tier_pace_yield_cell_recent_count_decay_v1";

const TIER_RANK_CAP: u8 = 8;

const TIER_RANK_SHIFT: u32 = 3;

const MAX_TIER_RANK_SHIFT: u32 = (u64::BITS - 1) / TIER_RANK_CAP as u32;

const COUNT_DECAY_EXPONENT: u32 = 2;

const COUNT_DECAY_SCALE: u64 = 1 << 32;

const BEST_HOLDER_SHARE_DENOMINATOR: usize = 4;

#[must_use]
fn count_decay(draws: u64) -> u64 {
    let divisor = draws
        .saturating_add(1)
        .saturating_pow(COUNT_DECAY_EXPONENT)
        .max(1);
    (COUNT_DECAY_SCALE / divisor).max(1)
}

fn checked_tier_rank_shift(shift: u32) -> Result<u32, Box<dyn Error>> {
    if shift > MAX_TIER_RANK_SHIFT {
        return Err(format!(
            "tier rank shift {shift} exceeds the largest supported shift {MAX_TIER_RANK_SHIFT}"
        )
        .into());
    }
    Ok(shift)
}

#[must_use]
fn tier_weight(rank: u8, shift: u32) -> u64 {
    1_u64 << (u32::from(TIER_RANK_CAP.saturating_sub(rank.min(TIER_RANK_CAP))) * shift)
}

fn draw_weighted(
    rand: &mut RomuDuoJrRand,
    weights: impl Iterator<Item = u64> + Clone,
) -> Result<usize, Box<dyn Error>> {
    let total = weights
        .clone()
        .fold(0_u64, |sum, weight| sum.saturating_add(weight));
    let total = NonZeroU64::new(total).ok_or("weighted draw over nothing")?;
    let mut draw = rand.below_u64(total);
    for (index, weight) in weights.enumerate() {
        if draw < weight {
            return Ok(index);
        }
        draw -= weight;
    }
    Err("weighted draw exceeded its total".into())
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorPath {
    Continuation,
    Tiers,
    Recent,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectorDraw {
    pub path: SelectorPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_rank: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub best_preference: Option<u8>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContinuationAccounting {
    pub edges: usize,
    pub pending: usize,
    pub jobs: u64,
    pub execution_work: u64,
    pub landed: u64,
    pub replaced: u64,
    pub opened_new_cell: u64,
    pub useful: u64,
    pub gaining_dispatched: u64,
    pub longest_wave: u32,
    pub energy: u16,
    pub reservations_drawn: u64,
    pub reservations_taken: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectorAccounting {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_selections: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_selections: Option<u64>,
    pub cell_selections: u64,
    pub productive_selections: u64,
    pub cell_resets: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_draws_by_rank: Vec<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub best_holder_draws: Vec<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub draws_by_cell: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tier_runs: BTreeMap<String, TierRuns>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioAccounting>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct TierRuns {
    pub current: u64,
    pub longest: u64,
    #[serde(default)]
    pub draws: u64,
    #[serde(default)]
    pub yields: u64,
    #[serde(default)]
    pub wins: u64,
    #[serde(default)]
    pub next_tier: String,
    #[serde(default)]
    pub next_draws: u64,
    #[serde(default)]
    pub next_yields: u64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PortfolioAccounting {
    pub preferences: usize,
    pub exclusive_holders: u64,
    pub shared_holders: u64,
    pub replacements_by_preference: Vec<u64>,
    pub cross_preference_improvements: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "M: Serialize + DeserializeOwned, P: Serialize + DeserializeOwned")]
pub struct ProgressPoint<M, P = ()> {
    pub executions: u64,
    pub milestones: M,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<P>,
    pub active_entries: usize,
    pub occupied_cells: usize,
    pub terminal_endpoints: u64,
    pub execution_failures: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntrySelectorCounters {
    pub selected: u64,
    pub productive: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    bound = "A: Serialize + DeserializeOwned + Ord + Clone, K: ArchiveKey, M: Serialize + \
                 DeserializeOwned + Clone"
)]
pub struct ArchiveEntryReport<A: Ord, K, M> {
    pub id: u64,
    pub parent_id: Option<u64>,
    pub created_execution: u64,
    pub input: Input<A>,
    pub key: K,
    pub milestones: M,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<EntrySelectorCounters>,
}

pub mod entries_by_suffix {
    use serde::de::DeserializeOwned;
    use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};

    use super::{ArchiveEntryReport, ArchiveKey, EntrySelectorCounters, Input};

    #[derive(Deserialize, Serialize)]
    #[serde(bound(
        deserialize = "I: DeserializeOwned, T: DeserializeOwned, K: ArchiveKey, M: DeserializeOwned"
    ))]
    struct Wire<I, T, K, M> {
        id: u64,
        parent_id: Option<u64>,
        created_execution: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<I>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_suffix: Option<T>,
        key: K,
        milestones: M,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<EntrySelectorCounters>,
    }

    pub fn serialize<S, A, K, M>(
        entries: &[ArchiveEntryReport<A, K, M>],
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        A: Serialize + DeserializeOwned + Ord + Clone,
        K: ArchiveKey,
        M: Serialize + DeserializeOwned + Clone,
    {
        let index_of: std::collections::BTreeMap<u64, usize> = entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.id, index))
            .collect();
        let wires = entries.iter().map(|entry| {
            let parent = entry
                .parent_id
                .and_then(|id| index_of.get(&id))
                .map(|index| &entries[*index].input.actions)
                .filter(|parent| entry.input.actions.starts_with(parent));
            let (input, input_suffix) = match parent {
                Some(parent) => (None, Some(&entry.input.actions[parent.len()..])),
                None => (Some(&entry.input), None),
            };
            Wire {
                id: entry.id,
                parent_id: entry.parent_id,
                created_execution: entry.created_execution,
                input,
                input_suffix,
                key: entry.key,
                milestones: &entry.milestones,
                selector: entry.selector,
            }
        });
        serializer.collect_seq(wires)
    }

    pub fn deserialize<'de, D, A, K, M>(
        deserializer: D,
    ) -> Result<Vec<ArchiveEntryReport<A, K, M>>, D::Error>
    where
        D: Deserializer<'de>,
        A: Serialize + DeserializeOwned + Ord + Clone,
        K: ArchiveKey,
        M: Serialize + DeserializeOwned + Clone,
    {
        let wires = Vec::<Wire<Input<A>, Vec<A>, K, M>>::deserialize(deserializer)?;
        let mut entries: Vec<ArchiveEntryReport<A, K, M>> = Vec::with_capacity(wires.len());
        let mut index_of = std::collections::BTreeMap::<u64, usize>::new();
        for wire in wires {
            let input = match (wire.input, wire.input_suffix) {
                (Some(input), None) => input,
                (None, Some(suffix)) => {
                    let mut actions = match wire.parent_id.and_then(|id| index_of.get(&id)) {
                        Some(index) => entries[*index].input.actions.clone(),
                        None => {
                            return Err(D::Error::custom(format!(
                                "archive entry {} carries an input suffix without a loaded parent",
                                wire.id
                            )));
                        }
                    };
                    actions.extend(suffix);
                    Input { actions }
                }
                _ => {
                    return Err(D::Error::custom(format!(
                        "archive entry {} must carry exactly one of input and input_suffix",
                        wire.id
                    )));
                }
            };
            index_of.insert(wire.id, entries.len());
            entries.push(ArchiveEntryReport {
                id: wire.id,
                parent_id: wire.parent_id,
                created_execution: wire.created_execution,
                input,
                key: wire.key,
                milestones: wire.milestones,
                selector: wire.selector,
            });
        }
        Ok(entries)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(bound(
    serialize = "A: Serialize, K: Serialize, M: Serialize",
    deserialize = "A: DeserializeOwned, K: DeserializeOwned, M: DeserializeOwned"
))]
pub(crate) struct ArchiveEntry<A: Ord, K, M, S> {
    pub(crate) id: u64,
    pub(crate) parent_id: Option<u64>,
    pub(crate) created_execution: u64,
    pub(crate) input_suffix: Vec<A>,
    pub(crate) input_len: usize,
    input_node: usize,
    pub(crate) key: K,
    pub(crate) milestones: M,
    #[serde(skip)]
    pub(crate) snapshot: Option<Arc<S>>,
}

pub(crate) struct CampaignSpliceTail<A> {
    pub(crate) donor_id: usize,
    pub(crate) leaf_id: usize,
    pub(crate) actions: Vec<A>,
}

pub struct ArchiveCandidate<T, K, M> {
    pub suffix: T,
    pub key: K,
    pub milestones: M,
}

#[derive(Deserialize, Serialize)]
#[serde(bound(
    serialize = "A: Serialize, M: Serialize",
    deserialize = "A: DeserializeOwned, M: DeserializeOwned"
))]
pub struct Archive<A: Ord, K: ArchiveKey, M, S> {
    pub max_entries: usize,
    pub(crate) entries: Vec<ArchiveEntry<A, K, M, S>>,
    id_to_index: BTreeMap<u64, usize>,
    next_entry_id: u64,
    pub active: Vec<bool>,
    active_count: usize,
    pub slots: BTreeMap<Slot<K>, Vec<usize>>,
    cells: BTreeMap<Cell<K>, CellState>,
    input_index: InputIndex<A>,
    historical_input_actions: usize,
    stored_input_actions: usize,
    history_memory_bytes: usize,
    pub retained: u64,
    pub rejected: u64,
    selected: Vec<u64>,
    productive: Vec<u64>,
    opened_cell: Vec<bool>,
    opened_slot: Vec<bool>,
    #[serde(with = "crate::search::checkpoint::json_bytes")]
    selector_accounting: SelectorAccounting,
    cost_in_group: Vec<u64>,
    replacement_cost_displaced: u64,
    portfolio_replacements: Vec<u64>,
    portfolio_cross_improvements: u64,
    replacement_preferences: Vec<u8>,
    in_place_lengths: Vec<u8>,
    lineages: Vec<K::Lineage>,
    deepest_leaf: Vec<(K, usize)>,
    #[serde(skip, default = "unset_action_cost::<A>")]
    action_cost: fn(&A) -> u64,
    live_progress: Option<(K, u64)>,
    selector_indexed: bool,
    #[serde(skip)]
    selector_weighted: bool,
    active_ids: ActiveIds,
    tiers: BTreeMap<K::Progress, TierCells<K>>,
    #[serde(skip)]
    parent_index: Vec<usize>,
    preserve_inactive_snapshots: bool,
    metadata_pins: BTreeMap<u64, u32>,
    inflight_snapshot_pins: BTreeMap<u64, u32>,
    inflight_snapshot_charges: BTreeMap<u64, usize>,
    inflight_snapshot_bytes: usize,
    resident_snapshots: usize,
    snapshot_selectable: Vec<bool>,
    keyframe: Vec<bool>,
    keyframe_id: Vec<u64>,
    keyframe_dependents: Vec<usize>,
    referenced: Vec<bool>,
    drop_hand: usize,
    entry_drops: u64,
    memory_limit: Option<usize>,
    liveness_anchor: Option<u64>,
    #[cfg(test)]
    liveness_anchor_reactivations: u64,
    resident_snapshot_bytes: usize,
    #[serde(skip)]
    snapshot_memory_charge: Option<fn(&S) -> usize>,
    resident_snapshot_order: VecDeque<usize>,
    snapshot_evictions: u64,
    history_compactions: u64,
    historical_entries_dropped: u64,
    #[serde(skip)]
    keep_releases: usize,
    #[serde(skip)]
    failed_compaction: Option<(usize, usize)>,
    input_reconstructions: std::cell::Cell<u64>,
    continuations: Option<ContinuationBank<Position<K>, A>>,
    continuation_wave: u32,
    continuation_accounting: ContinuationAccounting,
    landed: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
struct CellState {
    active: usize,
    draws: u64,
    draws_total: u64,
}

#[derive(Clone, Copy, Debug)]
struct HolderRank<K> {
    key: K,
    cost: u64,
    entry: u64,
    index: usize,
    preference: usize,
}

impl<K: ArchiveKey> Ord for HolderRank<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key
            .preference_cmp(self.preference, other.key)
            .then_with(|| (other.cost, other.entry).cmp(&(self.cost, self.entry)))
    }
}

impl<K: ArchiveKey> PartialOrd for HolderRank<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: ArchiveKey> PartialEq for HolderRank<K> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<K: ArchiveKey> Eq for HolderRank<K> {}

#[derive(Deserialize, Serialize)]
#[serde(bound = "")]
struct CellMembers<K> {
    ids: WeightedSet<usize>,
    #[serde(skip)]
    ranked: Vec<BTreeSet<HolderRank<K>>>,
}

impl<K> Default for CellMembers<K> {
    fn default() -> Self {
        Self {
            ids: WeightedSet::default(),
            ranked: Vec::new(),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(transparent, bound = "K: ArchiveKey")]
struct TierCells<K: ArchiveKey> {
    places: BTreeMap<K::Place, CellMembers<K>>,
    #[serde(skip)]
    weights: WeightedSet<K::Place>,
}

impl<K: ArchiveKey> Default for TierCells<K> {
    fn default() -> Self {
        Self {
            places: BTreeMap::new(),
            weights: WeightedSet::default(),
        }
    }
}

fn draw_member<T: Copy + Ord>(
    rand: &mut RomuDuoJrRand,
    set: &WeightedSet<T>,
) -> Result<T, Box<dyn Error>> {
    let total = NonZeroU64::new(set.total()).ok_or("weighted draw over nothing")?;
    let draw = rand.below_u64(total);
    set.find(draw)
        .ok_or_else(|| "weighted draw exceeded its total".into())
}

#[derive(Deserialize, Serialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: DeserializeOwned"))]
struct InputNode<A: Ord> {
    parent: Option<usize>,
    action: Option<A>,
    children: BTreeMap<A, usize>,
    owner: Option<u64>,
}

#[derive(Deserialize, Serialize)]
#[serde(bound(serialize = "A: Serialize", deserialize = "A: DeserializeOwned"))]
struct InputIndex<A: Ord> {
    nodes: Vec<Option<InputNode<A>>>,
    free: Vec<usize>,
    live_nodes: usize,
}

impl<A: Ord> Default for InputIndex<A> {
    fn default() -> Self {
        Self {
            nodes: vec![Some(InputNode {
                parent: None,
                action: None,
                children: BTreeMap::new(),
                owner: None,
            })],
            free: Vec::new(),
            live_nodes: 1,
        }
    }
}

impl<A: Clone + Ord> InputIndex<A> {
    fn walk(&self, mut node: usize, actions: &[A]) -> Option<usize> {
        for action in actions {
            node = *self.nodes.get(node)?.as_ref()?.children.get(action)?;
        }
        Some(node)
    }

    fn ensure_path(
        &mut self,
        mut node: usize,
        actions: &[A],
    ) -> Result<(usize, usize), &'static str> {
        let mut inserted = 0_usize;
        for action in actions {
            let existing = self
                .nodes
                .get(node)
                .and_then(Option::as_ref)
                .ok_or("input prefix starts at a missing node")?
                .children
                .get(action)
                .copied();
            if let Some(child) = existing {
                node = child;
                continue;
            }
            let child_node = InputNode {
                parent: Some(node),
                action: Some(action.clone()),
                children: BTreeMap::new(),
                owner: None,
            };
            let child = if let Some(free) = self.free.pop() {
                self.nodes[free] = Some(child_node);
                free
            } else {
                self.nodes.push(Some(child_node));
                self.nodes.len() - 1
            };
            self.live_nodes = self.live_nodes.saturating_add(1);
            let parent = self
                .nodes
                .get_mut(node)
                .and_then(Option::as_mut)
                .ok_or("input prefix parent disappeared while extending it")?;
            parent.children.insert(action.clone(), child);
            node = child;
            inserted = inserted.saturating_add(1);
        }
        Ok((node, inserted))
    }

    fn owner(&self, node: usize) -> Option<u64> {
        self.nodes.get(node)?.as_ref()?.owner
    }

    fn set_owner(&mut self, node: usize, owner: Option<u64>) {
        if let Some(node) = self.nodes.get_mut(node).and_then(Option::as_mut) {
            node.owner = owner;
        }
    }

    fn materialize(&self, mut node: usize, expected_len: usize) -> Option<Vec<A>> {
        let mut reversed = Vec::with_capacity(expected_len);
        while node != 0 {
            let current = self.nodes.get(node)?.as_ref()?;
            reversed.push(current.action.as_ref()?.clone());
            node = current.parent?;
            if reversed.len() > expected_len {
                return None;
            }
        }
        if reversed.len() != expected_len {
            return None;
        }
        reversed.reverse();
        Some(reversed)
    }

    fn step_back(&self, node: usize) -> Option<(usize, &A)> {
        if node == 0 {
            return None;
        }
        let current = self.nodes.get(node)?.as_ref()?;
        Some((current.parent?, current.action.as_ref()?))
    }

    fn prefix_is_available(&self, mut node: usize, expected_len: usize) -> bool {
        if expected_len > self.nodes.len().saturating_sub(1) {
            return false;
        }
        for _ in 0..expected_len {
            let Some((parent, _)) = self.step_back(node) else {
                return false;
            };
            node = parent;
        }
        node == 0
    }

    fn depths(&self) -> Vec<Option<usize>> {
        let mut known: Vec<Option<Option<usize>>> = vec![None; self.nodes.len()];
        if let Some(root) = known.first_mut() {
            *root = Some(Some(0));
        }
        let mut path = Vec::new();
        for start in 0..self.nodes.len() {
            let mut node = start;
            let mut depth = loop {
                if let Some(depth) = known[node] {
                    break depth;
                }
                if path.len() >= self.nodes.len() {
                    break None;
                }
                path.push(node);
                match self.step_back(node) {
                    Some((parent, _)) if parent < self.nodes.len() => node = parent,
                    _ => break None,
                }
            };
            while let Some(node) = path.pop() {
                depth = depth.and_then(|depth| depth.checked_add(1));
                known[node] = Some(depth);
            }
        }
        known.into_iter().map(Option::flatten).collect()
    }

    fn materialize_splice_tail(
        &self,
        (mut donor, donor_len, donor_id): (usize, usize, u64),
        mut leaf: usize,
        leaf_len: usize,
        limit: usize,
    ) -> Result<Vec<A>, &'static str> {
        if self.owner(donor) != Some(donor_id) && !self.prefix_is_available(donor, donor_len) {
            return Err("splice donor prefix is unavailable");
        }
        if leaf_len > self.nodes.len().saturating_sub(1) {
            return Err("splice leaf prefix is unavailable");
        }
        let Some(distance) = leaf_len.checked_sub(donor_len) else {
            return if self.prefix_is_available(leaf, leaf_len) {
                Err("splice leaf is not a descendant of its donor")
            } else {
                Err("splice leaf prefix is unavailable")
            };
        };
        let kept = distance.min(limit);
        for _ in kept..distance {
            leaf = self
                .step_back(leaf)
                .ok_or("splice leaf prefix is unavailable")?
                .0;
        }
        let mut suffix = Vec::with_capacity(kept);
        for _ in 0..kept {
            let (parent, action) = self
                .step_back(leaf)
                .ok_or("splice leaf prefix is unavailable")?;
            suffix.push(action.clone());
            leaf = parent;
        }
        if leaf != donor {
            let mut matches = true;
            for _ in 0..donor_len {
                let (parent, action) = self
                    .step_back(leaf)
                    .ok_or("splice leaf prefix is unavailable")?;
                let (donor_parent, donor_action) = self
                    .step_back(donor)
                    .ok_or("splice donor prefix is unavailable")?;
                matches &= action == donor_action;
                leaf = parent;
                donor = donor_parent;
            }
            if leaf != 0 {
                return Err("splice leaf prefix is unavailable");
            }
            if !matches {
                return Err("splice leaf is not a descendant of its donor");
            }
        }
        suffix.reverse();
        Ok(suffix)
    }

    fn actions_between(&self, ancestor: usize, mut node: usize, distance: usize) -> Option<Vec<A>> {
        let mut reversed = Vec::with_capacity(distance);
        for _ in 0..distance {
            let current = self.nodes.get(node)?.as_ref()?;
            reversed.push(current.action.as_ref()?.clone());
            node = current.parent?;
        }
        (node == ancestor).then(|| {
            reversed.reverse();
            reversed
        })
    }

    fn remove_owner_and_prune(&mut self, mut node: usize, owner: u64) -> usize {
        let current_owner = self.owner(node);
        if current_owner.is_some() && current_owner != Some(owner) {
            return 0;
        }
        if current_owner == Some(owner) {
            self.set_owner(node, None);
        }
        let mut removed = 0_usize;
        while node != 0 {
            let removable = self.nodes[node]
                .as_ref()
                .is_some_and(|node| node.owner.is_none() && node.children.is_empty());
            if !removable {
                break;
            }
            let Some(retired) = self.nodes[node].take() else {
                break;
            };
            let Some(parent) = retired.parent else {
                break;
            };
            if let (Some(action), Some(parent_node)) = (retired.action, self.nodes[parent].as_mut())
            {
                parent_node.children.remove(&action);
            }
            self.free.push(node);
            self.live_nodes = self.live_nodes.saturating_sub(1);
            removed = removed.saturating_add(1);
            node = parent;
        }
        removed
    }

    fn compact(&mut self) -> Vec<Option<usize>> {
        let mut remap = vec![None; self.nodes.len()];
        let mut next = 0_usize;
        for (old, node) in self.nodes.iter().enumerate() {
            if node.is_some() {
                remap[old] = Some(next);
                next = next.saturating_add(1);
            }
        }
        let old_nodes = std::mem::take(&mut self.nodes);
        self.nodes = old_nodes
            .into_iter()
            .flatten()
            .map(|mut node| {
                node.parent = node.parent.and_then(|parent| remap[parent]);
                if node.children.len() == 1 {
                    let child = node.children.values_mut().next().expect("one child");
                    if let Some(mapped) = remap[*child] {
                        *child = mapped;
                        return Some(node);
                    }
                }
                node.children = std::mem::take(&mut node.children)
                    .into_iter()
                    .filter_map(|(action, child)| remap[child].map(|mapped| (action, mapped)))
                    .collect();
                Some(node)
            })
            .collect();
        self.nodes.shrink_to_fit();
        self.free.clear();
        self.free.shrink_to_fit();
        self.live_nodes = self.nodes.len();
        remap
    }
}

#[cfg(test)]
impl<A: Clone + Ord> InputIndex<A> {
    fn compact_reference(&mut self) -> Vec<Option<usize>> {
        let mut remap = vec![None; self.nodes.len()];
        let mut next = 0_usize;
        for (old, node) in self.nodes.iter().enumerate() {
            if node.is_some() {
                remap[old] = Some(next);
                next = next.saturating_add(1);
            }
        }
        let old_nodes = std::mem::take(&mut self.nodes);
        self.nodes = old_nodes
            .into_iter()
            .flatten()
            .map(|mut node| {
                node.parent = node.parent.and_then(|parent| remap[parent]);
                node.children = std::mem::take(&mut node.children)
                    .into_iter()
                    .filter_map(|(action, child)| remap[child].map(|mapped| (action, mapped)))
                    .collect();
                Some(node)
            })
            .collect();
        self.nodes.shrink_to_fit();
        self.free.clear();
        self.free.shrink_to_fit();
        self.live_nodes = self.nodes.len();
        remap
    }
}

#[derive(Default, Deserialize, Serialize)]
struct ActiveIds {
    ids: BTreeSet<usize>,
}

impl ActiveIds {
    fn from_ids(ids: impl IntoIterator<Item = usize>) -> Self {
        Self {
            ids: ids.into_iter().collect(),
        }
    }

    fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    fn insert(&mut self, id: usize) {
        self.ids.insert(id);
    }

    fn remove(&mut self, id: usize) -> bool {
        self.ids.remove(&id)
    }

    fn contains(&self, id: usize) -> bool {
        self.ids.contains(&id)
    }

    fn ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.ids.iter().copied()
    }
}

impl<A, K, M, S> Archive<A, K, M, S>
where
    A: Clone + Debug + Eq + Ord + Serialize + DeserializeOwned,
    K: ArchiveKey,
    M: Clone + Copy + Debug + Eq + Serialize + DeserializeOwned,
    S: Clone,
{
    #[must_use]
    pub fn new(action_cost: fn(&A) -> u64) -> Self {
        Self {
            max_entries: MAX_ARCHIVE_ENTRIES,
            entries: Vec::new(),
            id_to_index: BTreeMap::new(),
            next_entry_id: 0,
            active: Vec::new(),
            active_count: 0,
            slots: BTreeMap::new(),
            cells: BTreeMap::new(),
            input_index: InputIndex::default(),
            historical_input_actions: 0,
            stored_input_actions: 0,
            history_memory_bytes: Self::prefix_node_memory_charge(),
            retained: 0,
            rejected: 0,
            selected: Vec::new(),
            productive: Vec::new(),
            opened_cell: Vec::new(),
            opened_slot: Vec::new(),
            selector_accounting: SelectorAccounting {
                best_holder_draws: vec![0; K::preferences()],
                ..SelectorAccounting::default()
            },
            cost_in_group: Vec::new(),
            replacement_cost_displaced: 0,
            portfolio_replacements: vec![0; K::preferences().max(1)],
            portfolio_cross_improvements: 0,
            replacement_preferences: Vec::new(),
            in_place_lengths: Vec::new(),
            lineages: Vec::new(),
            deepest_leaf: Vec::new(),
            action_cost,
            live_progress: None,
            selector_indexed: false,
            selector_weighted: false,
            active_ids: ActiveIds::default(),
            tiers: BTreeMap::new(),
            parent_index: Vec::new(),
            preserve_inactive_snapshots: false,
            metadata_pins: BTreeMap::new(),
            inflight_snapshot_pins: BTreeMap::new(),
            inflight_snapshot_charges: BTreeMap::new(),
            inflight_snapshot_bytes: 0,
            resident_snapshots: 0,
            snapshot_selectable: Vec::new(),
            keyframe: Vec::new(),
            keyframe_id: Vec::new(),
            keyframe_dependents: Vec::new(),
            referenced: Vec::new(),
            drop_hand: 0,
            entry_drops: 0,
            memory_limit: None,
            liveness_anchor: None,
            #[cfg(test)]
            liveness_anchor_reactivations: 0,
            resident_snapshot_bytes: 0,
            snapshot_memory_charge: None,
            resident_snapshot_order: VecDeque::new(),
            snapshot_evictions: 0,
            history_compactions: 0,
            historical_entries_dropped: 0,
            keep_releases: 0,
            failed_compaction: None,
            input_reconstructions: std::cell::Cell::new(0),
            continuations: None,
            continuation_wave: 0,
            continuation_accounting: ContinuationAccounting::default(),
            landed: BTreeSet::new(),
        }
    }

    pub(crate) fn set_memory_budget(&mut self, bytes: usize, charge: fn(&S) -> usize) {
        self.memory_limit = Some(bytes);
        self.snapshot_memory_charge = Some(charge);
    }

    pub(crate) fn establish_liveness_anchor(&mut self) {
        if self.memory_limit.is_none() {
            return;
        }
        if self.liveness_anchor.is_some_and(|id| {
            self.index_of_id(id).is_some_and(|index| {
                self.snapshot_selectable
                    .get(index)
                    .copied()
                    .unwrap_or(false)
                    && self.entries[index].snapshot.is_some()
            })
        }) {
            return;
        }
        self.release_keep(1);
        self.liveness_anchor = self
            .entries
            .iter()
            .enumerate()
            .find(|(index, _)| {
                self.active.get(*index).copied().unwrap_or(false)
                    && self
                        .snapshot_selectable
                        .get(*index)
                        .copied()
                        .unwrap_or(false)
                    && self.entries[*index].snapshot.is_some()
            })
            .map(|(_, entry)| entry.id);
    }

    fn is_liveness_anchor(&self, id: usize) -> bool {
        self.memory_limit.is_some()
            && self.liveness_anchor == self.entries.get(id).map(|entry| entry.id)
    }

    fn deactivate_liveness_anchor_for_admission(&mut self) -> bool {
        let Some(anchor) = self.liveness_anchor else {
            return false;
        };
        let Some(index) = self.index_of_id(anchor) else {
            return false;
        };
        if !self.active.get(index).copied().unwrap_or(false) {
            return false;
        }
        self.deactivate(index);
        true
    }

    fn snapshot_charge(&self, snapshot: &S) -> usize {
        self.snapshot_memory_charge
            .map_or(0, |charge| charge(snapshot))
    }

    fn reclaim_inactive_snapshot(&mut self, id: usize) {
        if self.snapshot_selectable.get(id).copied().unwrap_or(false)
            || self
                .inflight_snapshot_pins
                .contains_key(&self.entries[id].id)
        {
            return;
        }
        let worker_holds_snapshot = self.entries[id]
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| Arc::strong_count(snapshot) > 1);
        if !self.preserve_inactive_snapshots
            && !worker_holds_snapshot
            && self.entries[id].snapshot.take().is_some()
        {
            self.release_keep(2);
        }
    }

    fn replay_bucket(input_len: usize) -> usize {
        input_len / REPLAY_DISTANCE_ACTIONS
    }

    fn snapshot_retained(&self, id: usize) -> bool {
        (self.keyframe[id] && self.keyframe_dependents[id] > 0) || self.is_liveness_anchor(id)
    }

    fn origin_resident(&self, id: usize) -> bool {
        if self.snapshot_selectable[id] {
            return true;
        }
        self.index_of_id(self.keyframe_id[id])
            .is_some_and(|keyframe| self.snapshot_selectable[keyframe])
    }

    fn deactivate(&mut self, id: usize) -> bool {
        if !self.active.get(id).copied().unwrap_or(false) {
            return false;
        }
        self.active[id] = false;
        self.active_count = self.active_count.saturating_sub(1);
        self.release_keep(2);
        let key = self.entries[id].key;
        let slot_key = slot_of_key(key);
        let remove_slot = if let Some(slot) = self.slots.get_mut(&slot_key) {
            slot.retain(|entry| *entry != id);
            slot.is_empty()
        } else {
            false
        };
        if remove_slot {
            self.slots.remove(&slot_key);
        }
        if let Some(state) = self.cells.get_mut(&cell_of(key)) {
            state.active = state.active.saturating_sub(1);
        }
        self.index_remove(id);
        if !self.is_liveness_anchor(id) {
            self.input_index
                .set_owner(self.entries[id].input_node, None);
        }
        let keyframe = self.index_of_id(self.keyframe_id[id]);
        if let Some(keyframe) = keyframe {
            self.keyframe_dependents[keyframe] =
                self.keyframe_dependents[keyframe].saturating_sub(1);
        }
        if !self.snapshot_retained(id) {
            self.release_snapshot(id);
        }
        if let Some(keyframe) = keyframe
            && keyframe != id
            && !self.active[keyframe]
            && !self.snapshot_retained(keyframe)
        {
            self.release_snapshot(keyframe);
        }
        true
    }

    fn release_snapshot(&mut self, id: usize) -> bool {
        if !self.snapshot_selectable.get(id).copied().unwrap_or(false) {
            return false;
        }
        let charge = self.entries[id]
            .snapshot
            .as_deref()
            .map_or(0, |snapshot| self.snapshot_charge(snapshot));
        let stable_id = self.entries[id].id;
        if self.inflight_snapshot_pins.contains_key(&stable_id) {
            self.inflight_snapshot_charges.insert(stable_id, charge);
            self.inflight_snapshot_bytes = self.inflight_snapshot_bytes.saturating_add(charge);
        }
        self.snapshot_selectable[id] = false;
        self.resident_snapshots = self.resident_snapshots.saturating_sub(1);
        self.resident_snapshot_bytes = self.resident_snapshot_bytes.saturating_sub(charge);
        self.reclaim_inactive_snapshot(id);
        true
    }

    fn enforce_snapshot_memory_budget(&mut self) -> Result<(), &'static str> {
        let Some(limit) = self.memory_limit else {
            return Ok(());
        };
        if self.liveness_anchor_memory_bytes() > limit {
            return Err("memory budget cannot retain the executable liveness anchor");
        }
        let mut visits = 0_usize;
        while self.resident_memory_bytes() > limit && visits < MAINTENANCE_QUANTUM {
            let Some(id) = self.resident_snapshot_order.pop_front() else {
                break;
            };
            visits = visits.saturating_add(1);
            if !self.snapshot_selectable[id] || self.snapshot_retained(id) {
                continue;
            }
            if self.referenced[id] {
                self.referenced[id] = false;
                self.resident_snapshot_order.push_back(id);
                continue;
            }
            if self.resident_snapshots <= 1 {
                self.resident_snapshot_order.push_front(id);
                break;
            }
            self.release_snapshot(id);
            self.snapshot_evictions = self.snapshot_evictions.saturating_add(1);
        }
        while self.resident_memory_bytes() > limit
            && visits < MAINTENANCE_QUANTUM
            && self.active_count > 1
        {
            let (dropped, examined) =
                self.drop_one_entry_within(MAINTENANCE_QUANTUM.saturating_sub(visits));
            visits = visits.saturating_add(examined);
            if !dropped {
                break;
            }
        }
        Ok(())
    }

    fn liveness_anchor_memory_bytes(&self) -> usize {
        let Some(index) = self.liveness_anchor.and_then(|id| self.index_of_id(id)) else {
            return 0;
        };
        let entry = &self.entries[index];
        entry
            .snapshot
            .as_deref()
            .map_or(0, |snapshot| self.snapshot_charge(snapshot))
            .saturating_add(Self::history_entry_memory_charge(
                entry.input_suffix.len(),
                0,
            ))
    }

    fn drop_one_entry(&mut self) -> bool {
        let budget = self.entries.len().saturating_mul(2);
        self.drop_one_entry_within(budget).0
    }

    fn drop_one_entry_within(&mut self, budget: usize) -> (bool, usize) {
        let count = self.entries.len();
        if count == 0 {
            return (false, 0);
        }
        let limit = budget.min(count.saturating_mul(2));
        let mut examined = 0_usize;
        while examined < limit {
            let id = self.drop_hand % count;
            self.drop_hand = (id + 1) % count;
            examined = examined.saturating_add(1);
            if !self.active[id] || self.is_liveness_anchor(id) {
                continue;
            }
            if self.referenced[id] {
                self.referenced[id] = false;
                continue;
            }
            self.deactivate(id);
            self.entry_drops = self.entry_drops.saturating_add(1);
            return (true, examined);
        }
        (false, examined)
    }

    pub(crate) fn maintain_memory_budget(&mut self) -> Result<(), &'static str> {
        self.compact_history_if_needed()?;
        self.enforce_snapshot_memory_budget()
    }

    pub(crate) fn compact_history_if_needed(&mut self) -> Result<(), &'static str> {
        self.compact_history(false)
    }

    pub(crate) fn compact_history_for_final_report(&mut self) -> Result<(), &'static str> {
        self.metadata_pins.clear();
        self.liveness_anchor = None;
        loop {
            let before = self.maintenance_state();
            self.compact_history(true)?;
            if self.maintenance_state() == before {
                break;
            }
        }
        if self
            .memory_limit
            .is_some_and(|limit| self.resident_memory_bytes() > limit)
        {
            return Err("memory budget cannot retain the compacted archive");
        }
        Ok(())
    }

    fn maintenance_state(&self) -> (usize, usize, usize, usize, usize) {
        let referenced_active = self
            .referenced
            .iter()
            .zip(&self.active)
            .filter(|(referenced, active)| **referenced && **active)
            .count();
        (
            self.entries.len(),
            self.active_count,
            self.resident_snapshots,
            referenced_active,
            self.resident_memory_bytes(),
        )
    }

    fn release_keep(&mut self, entries: usize) {
        self.keep_releases = self.keep_releases.wrapping_add(entries);
    }

    fn history_keep(&self) -> Vec<bool> {
        self.entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                self.active.get(index).copied().unwrap_or(false)
                    || self.metadata_pins.contains_key(&entry.id)
                    || self.inflight_snapshot_pins.contains_key(&entry.id)
                    || (self.keyframe[index] && self.keyframe_dependents[index] > 0)
                    || (self.preserve_inactive_snapshots && entry.snapshot.is_some())
                    || (self.liveness_anchor == Some(entry.id) && entry.snapshot.is_some())
                    || entry
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| Arc::strong_count(snapshot) > 1)
            })
            .collect()
    }

    fn compaction_min_drops(&self) -> usize {
        HISTORY_COMPACTION_MIN_DROPS.max(self.entries.len() / HISTORY_COMPACTION_SHARE)
    }

    fn compact_history(&mut self, force: bool) -> Result<(), &'static str> {
        let history_target = self
            .memory_limit
            .map_or(usize::MAX, |limit| limit / 4)
            .max(1);
        let min_drops = self.compaction_min_drops();
        let entry_pressure = self.entries.len() >= self.max_entries.saturating_add(min_drops);
        if !force && self.history_memory_bytes() <= history_target && !entry_pressure {
            return Ok(());
        }
        if !force && self.entries.len().saturating_sub(self.active_count) < min_drops {
            return Ok(());
        }
        if !force
            && let Some((releases, dropped)) = self.failed_compaction
            && self
                .keep_releases
                .wrapping_sub(releases)
                .saturating_add(dropped)
                < min_drops
        {
            debug_assert!(self.history_keep().iter().filter(|keep| !**keep).count() < min_drops);
            return Ok(());
        }

        let keep = self.history_keep();
        let dropped = keep.iter().filter(|keep| !**keep).count();
        if !force && dropped < min_drops {
            self.failed_compaction = Some((self.keep_releases, dropped));
            return Ok(());
        }
        self.failed_compaction = None;

        for (index, entry) in self.entries.iter().enumerate() {
            if keep[index] {
                self.input_index.set_owner(entry.input_node, Some(entry.id));
            }
        }
        for (index, entry) in self.entries.iter().enumerate() {
            if !keep[index] {
                self.input_index
                    .remove_owner_and_prune(entry.input_node, entry.id);
            }
        }
        let node_remap = self.input_index.compact();
        for (index, entry) in self.entries.iter_mut().enumerate() {
            if keep[index] {
                entry.input_node = node_remap
                    .get(entry.input_node)
                    .copied()
                    .flatten()
                    .ok_or("history compaction lost a retained input prefix")?;
            }
        }

        self.entries = retain_marked(std::mem::take(&mut self.entries), &keep);
        self.active = retain_marked(std::mem::take(&mut self.active), &keep);
        self.selected = retain_marked(std::mem::take(&mut self.selected), &keep);
        self.productive = retain_marked(std::mem::take(&mut self.productive), &keep);
        self.opened_cell = retain_marked(std::mem::take(&mut self.opened_cell), &keep);
        self.opened_slot = retain_marked(std::mem::take(&mut self.opened_slot), &keep);
        self.cost_in_group = retain_marked(std::mem::take(&mut self.cost_in_group), &keep);
        self.replacement_preferences =
            retain_marked(std::mem::take(&mut self.replacement_preferences), &keep);
        self.in_place_lengths = retain_marked(std::mem::take(&mut self.in_place_lengths), &keep);
        self.lineages = retain_marked(std::mem::take(&mut self.lineages), &keep);
        self.snapshot_selectable =
            retain_marked(std::mem::take(&mut self.snapshot_selectable), &keep);
        self.keyframe = retain_marked(std::mem::take(&mut self.keyframe), &keep);
        self.keyframe_id = retain_marked(std::mem::take(&mut self.keyframe_id), &keep);
        self.keyframe_dependents =
            retain_marked(std::mem::take(&mut self.keyframe_dependents), &keep);
        self.referenced = retain_marked(std::mem::take(&mut self.referenced), &keep);
        self.drop_hand = 0;

        self.id_to_index.clear();
        self.slots.clear();
        self.resident_snapshot_order.clear();
        self.active_count = 0;
        self.resident_snapshots = 0;
        self.resident_snapshot_bytes = 0;
        self.stored_input_actions = 0;
        for state in self.cells.values_mut() {
            state.active = 0;
        }
        for (index, entry) in self.entries.iter().enumerate() {
            self.id_to_index.insert(entry.id, index);
            self.stored_input_actions = self
                .stored_input_actions
                .saturating_add(entry.input_suffix.len());
            if self.active[index] {
                self.active_count = self.active_count.saturating_add(1);
                self.slots
                    .entry(slot_of_key(entry.key))
                    .or_default()
                    .push(index);
                self.cells.entry(cell_of(entry.key)).or_default().active += 1;
            }
            if self.snapshot_selectable[index] {
                self.resident_snapshots = self.resident_snapshots.saturating_add(1);
                self.resident_snapshot_bytes = self.resident_snapshot_bytes.saturating_add(
                    entry
                        .snapshot
                        .as_deref()
                        .map_or(0, |snapshot| self.snapshot_charge(snapshot)),
                );
                if !self.keyframe[index] {
                    self.resident_snapshot_order.push_back(index);
                }
            }
        }
        self.cells.retain(|_, state| state.active > 0);
        self.retain_indexed_landings();

        if let Some(bank) = &mut self.continuations {
            let live = self
                .entries
                .iter()
                .map(|entry| position_of(entry.key))
                .collect::<BTreeSet<_>>();
            bank.retain(&live);
        }

        self.deepest_leaf = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| (entry.key, index))
            .collect();
        self.history_memory_bytes = Self::prefix_node_memory_charge()
            .saturating_add(
                self.entries
                    .iter()
                    .map(|entry| Self::history_entry_memory_charge(entry.input_suffix.len(), 0))
                    .sum::<usize>(),
            )
            .saturating_add(
                self.input_index
                    .live_nodes
                    .saturating_sub(1)
                    .saturating_mul(Self::prefix_node_memory_charge()),
            );
        self.history_compactions = self.history_compactions.saturating_add(1);
        self.historical_entries_dropped = self
            .historical_entries_dropped
            .saturating_add(u64::try_from(dropped).unwrap_or(u64::MAX));

        let selector_indexed = std::mem::take(&mut self.selector_indexed);
        self.active_ids = ActiveIds::default();
        self.tiers.clear();
        self.parent_index.clear();
        if selector_indexed {
            self.rebuild_selector_index();
        }
        self.enforce_snapshot_memory_budget()?;
        Ok(())
    }

    fn retain_indexed_landings(&mut self) {
        let mut live = self.id_to_index.keys().peekable();
        self.landed.retain(|id| {
            while live.peek().is_some_and(|live| *live < id) {
                live.next();
            }
            live.peek().is_some_and(|live| *live == id)
        });
    }

    pub(crate) fn preserve_inactive_snapshots(
        &mut self,
        preserve: bool,
    ) -> Result<(), &'static str> {
        self.preserve_inactive_snapshots = preserve;
        self.failed_compaction = None;
        if !preserve {
            for id in 0..self.entries.len() {
                if !self.snapshot_selectable[id] {
                    self.entries[id].snapshot.take();
                }
            }
            self.enforce_snapshot_memory_budget()?;
        }
        Ok(())
    }

    pub(crate) fn preserves_inactive_snapshots(&self) -> bool {
        self.preserve_inactive_snapshots
    }

    pub(crate) fn index_of_id(&self, id: u64) -> Option<usize> {
        self.id_to_index.get(&id).copied()
    }

    pub(crate) fn stable_id(&self, index: usize) -> Option<u64> {
        self.entries.get(index).map(|entry| entry.id)
    }

    pub(crate) fn pin_metadata(&mut self, id: u64) -> Result<(), &'static str> {
        if self.index_of_id(id).is_none() {
            return Err("metadata pin names a missing archive id");
        }
        let pins = self.metadata_pins.entry(id).or_default();
        *pins = pins
            .checked_add(1)
            .ok_or("archive metadata pin count overflow")?;
        Ok(())
    }

    pub(crate) fn unpin_metadata(&mut self, id: u64) {
        let remove = if let Some(pins) = self.metadata_pins.get_mut(&id) {
            *pins = pins.saturating_sub(1);
            *pins == 0
        } else {
            false
        };
        if remove {
            self.metadata_pins.remove(&id);
            self.release_keep(1);
            if let Some(index) = self.index_of_id(id) {
                self.reclaim_inactive_snapshot(index);
            }
        }
    }

    pub(crate) fn preserve_recorded_metadata_uses(&mut self, uses: BTreeMap<u64, u32>) {
        self.metadata_pins = uses;
        self.failed_compaction = None;
    }

    fn input_index_start(&self, parent_id: Option<usize>) -> usize {
        let Some(parent_id) = parent_id else {
            return 0;
        };
        let Some(node) = self.entries.get(parent_id).map(|entry| entry.input_node) else {
            return 0;
        };
        node
    }

    pub(crate) fn last_action(&self, id: usize) -> Option<&A> {
        let node = self.entries.get(id)?.input_node;
        self.input_index.nodes.get(node)?.as_ref()?.action.as_ref()
    }

    pub(crate) fn materialize_input(&self, id: usize) -> Result<Input<A>, &'static str> {
        self.input_reconstructions
            .set(self.input_reconstructions.get().saturating_add(1));
        let entry = self.entries.get(id).ok_or("archive input id is missing")?;
        let actions = self
            .input_index
            .materialize(entry.input_node, entry.input_len)
            .ok_or("archive input length disagrees with its prefix path")?;
        Ok(Input { actions })
    }

    pub(crate) fn pin_job_origin(
        &mut self,
        id: usize,
    ) -> Result<(Arc<S>, Vec<A>, u64), &'static str> {
        let (snapshot, replay) = self.job_origin(id)?;
        let snapshot_id = if self.snapshot_selectable[id] {
            self.entries[id].id
        } else {
            self.keyframe_id[id]
        };
        let pins = self.inflight_snapshot_pins.entry(snapshot_id).or_default();
        *pins = pins
            .checked_add(1)
            .ok_or("archive snapshot pin count overflow")?;
        Ok((snapshot, replay, snapshot_id))
    }

    pub(crate) fn unpin_job_origin(&mut self, id: u64) {
        let remove = if let Some(pins) = self.inflight_snapshot_pins.get_mut(&id) {
            *pins = pins.saturating_sub(1);
            *pins == 0
        } else {
            false
        };
        if remove {
            self.inflight_snapshot_pins.remove(&id);
            self.release_keep(1);
            if let Some(charge) = self.inflight_snapshot_charges.remove(&id) {
                self.inflight_snapshot_bytes = self.inflight_snapshot_bytes.saturating_sub(charge);
            }
            if let Some(index) = self.index_of_id(id) {
                self.reclaim_inactive_snapshot(index);
            }
        }
    }

    pub(crate) fn job_origin(&self, id: usize) -> Result<(Arc<S>, Vec<A>), &'static str> {
        let entry = self.entries.get(id).ok_or("job origin entry is missing")?;
        if self.snapshot_selectable[id] {
            let snapshot = entry
                .snapshot
                .clone()
                .ok_or("job origin entry has no resident snapshot")?;
            return Ok((snapshot, Vec::new()));
        }
        let keyframe = self
            .index_of_id(self.keyframe_id[id])
            .ok_or("job origin keyframe is missing")?;
        if !self.snapshot_selectable[keyframe] {
            return Err("job origin keyframe has no resident snapshot");
        }
        let keyframe_entry = &self.entries[keyframe];
        let snapshot = keyframe_entry
            .snapshot
            .clone()
            .ok_or("job origin keyframe has no resident snapshot")?;
        let distance = entry
            .input_len
            .checked_sub(keyframe_entry.input_len)
            .ok_or("job origin keyframe is longer than its dependent")?;
        let replay = self
            .input_index
            .actions_between(keyframe_entry.input_node, entry.input_node, distance)
            .ok_or("job origin keyframe is not on the entry's input path")?;
        Ok((snapshot, replay))
    }

    fn existing_input_id(&self, parent_id: Option<usize>, suffix: &[A]) -> Option<usize> {
        let node = self.input_index_start(parent_id);
        let node = self.input_index.walk(node, suffix)?;
        let stable_id = self.input_index.owner(node)?;
        self.id_to_index.get(&stable_id).copied()
    }

    fn index_retained_input(
        &mut self,
        parent_id: Option<usize>,
        suffix: &[A],
        stable_id: u64,
    ) -> Result<(usize, usize), &'static str> {
        let node = self.input_index_start(parent_id);
        let (node, inserted) = self.input_index.ensure_path(node, suffix)?;
        self.input_index.set_owner(node, Some(stable_id));
        Ok((node, inserted))
    }

    fn prefix_node_memory_charge() -> usize {
        size_of::<BTreeMap<A, usize>>()
            .saturating_add(size_of::<Option<usize>>())
            .saturating_add(size_of::<A>())
            .saturating_add(size_of::<usize>())
            .saturating_add(64)
    }

    fn history_entry_memory_charge(suffix_len: usize, new_nodes: usize) -> usize {
        size_of::<ArchiveEntry<A, K, M, S>>()
            .saturating_add(suffix_len.saturating_mul(size_of::<A>()))
            .saturating_add(size_of::<K::Lineage>())
            .saturating_add(size_of::<(K, usize)>())
            .saturating_add(4_usize.saturating_mul(size_of::<u64>()))
            .saturating_add(4_usize.saturating_mul(size_of::<usize>()))
            .saturating_add(128)
            .saturating_add(new_nodes.saturating_mul(Self::prefix_node_memory_charge()))
    }

    fn cell_memory_charge() -> usize {
        size_of::<Cell<K>>()
            .saturating_add(size_of::<CellState>())
            .saturating_add(size_of::<K::Place>())
            .saturating_add(size_of::<BTreeSet<usize>>())
            .saturating_add(128)
    }

    fn auxiliary_history_memory_bytes(&self) -> usize {
        self.cell_memory_bytes()
    }

    pub(crate) fn all_extensions_retained(&self, parent_id: usize, actions: &[A]) -> bool {
        let Some(mut node) = self.entries.get(parent_id).map(|entry| entry.input_node) else {
            return false;
        };
        for action in actions {
            let Some(child) = self
                .input_index
                .nodes
                .get(node)
                .and_then(Option::as_ref)
                .and_then(|node| node.children.get(action))
                .copied()
            else {
                return false;
            };
            node = child;
            if self.input_index.owner(node).is_none() {
                return false;
            }
        }
        true
    }

    fn rebuild_selector_index(&mut self) {
        self.selector_indexed = true;
        self.active_ids = ActiveIds::from_ids(self.active_ids());
        let mut grouped = BTreeMap::<K::Progress, BTreeMap<K::Place, Vec<usize>>>::new();
        for id in self.active_ids.ids() {
            let key = self.entries[id].key;
            grouped
                .entry(key.progress())
                .or_default()
                .entry(key.place())
                .or_default()
                .push(id);
        }
        self.tiers = grouped
            .into_iter()
            .map(|(progress, places)| {
                let places = places
                    .into_iter()
                    .map(|(place, ids)| {
                        let ids = WeightedSet::from_sorted(ids.into_iter().map(|id| (id, 0)));
                        (
                            place,
                            CellMembers {
                                ids,
                                ranked: Vec::new(),
                            },
                        )
                    })
                    .collect();
                (
                    progress,
                    TierCells {
                        places,
                        weights: WeightedSet::default(),
                    },
                )
            })
            .collect();
        self.reweight_selector_index();
    }

    pub(crate) fn prepare_selection(&mut self) {
        self.ensure_selector_index();
        self.establish_liveness_anchor();
        self.reactivate_liveness_anchor();
    }

    fn ensure_selector_index(&mut self) {
        if !self.selector_indexed {
            self.rebuild_selector_index();
        } else if !self.selector_weighted {
            self.reweight_selector_index();
        }
    }

    fn activate_membership(&mut self, index: usize) {
        let key = self.entries[index].key;
        self.slots.entry(slot_of_key(key)).or_default().push(index);
        self.cells.entry(cell_of(key)).or_default().active += 1;
    }

    fn reactivate_liveness_anchor(&mut self) -> bool {
        if !self.active_ids.is_empty() {
            return false;
        }
        self.establish_liveness_anchor();
        let Some(anchor) = self.liveness_anchor else {
            return false;
        };
        let Some(index) = self.index_of_id(anchor) else {
            return false;
        };
        if !self
            .snapshot_selectable
            .get(index)
            .copied()
            .unwrap_or(false)
            || self.entries[index].snapshot.is_none()
        {
            return false;
        }
        if !self.active.get(index).copied().unwrap_or(false) {
            if self.active_count >= self.max_entries && !self.drop_one_entry() {
                return false;
            }
            let slot_key = slot_of_key(self.entries[index].key);
            let slot_full = self
                .slots
                .get(&slot_key)
                .is_some_and(|slot| slot.len() >= K::capacity().max(1));
            if slot_full {
                let replaced = self
                    .slots
                    .get(&slot_key)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|id| !self.is_liveness_anchor(*id))
                    .max_by_key(|id| (self.cost_in_group[*id], self.entries[*id].id));
                let Some(replaced) = replaced else {
                    return false;
                };
                self.deactivate(replaced);
            }
            self.active[index] = true;
            self.active_count = self.active_count.saturating_add(1);
            let keyframe = self.index_of_id(self.keyframe_id[index]);
            if let Some(keyframe) = keyframe {
                self.keyframe_dependents[keyframe] =
                    self.keyframe_dependents[keyframe].saturating_add(1);
            }
            self.activate_membership(index);
            #[cfg(test)]
            {
                self.liveness_anchor_reactivations =
                    self.liveness_anchor_reactivations.saturating_add(1);
            }
        }
        self.rebuild_selector_index();
        !self.active_ids.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn liveness_anchor_reactivations(&self) -> u64 {
        self.liveness_anchor_reactivations
    }

    fn index_insert(&mut self, id: usize) {
        if !self.selector_indexed {
            return;
        }
        self.active_ids.insert(id);
        self.insert_active_cell_member(id);
    }

    fn index_remove(&mut self, id: usize) {
        if !self.selector_indexed {
            return;
        }
        self.active_ids.remove(id);
        let rank = self.holder_rank(0, id);
        let (progress, place) = (rank.key.progress(), rank.key.place());
        let Some(tier) = self.tiers.get_mut(&progress) else {
            return;
        };
        if let Some(members) = tier.places.get_mut(&place) {
            members.ids.remove(&id);
            for (preference, ranked) in members.ranked.iter_mut().enumerate() {
                ranked.remove(&HolderRank { preference, ..rank });
            }
            if members.ids.is_empty() {
                tier.places.remove(&place);
                tier.weights.remove(&place);
            }
        }
        if tier.places.is_empty() {
            self.tiers.remove(&progress);
        }
    }

    fn insert_active_cell_member(&mut self, id: usize) {
        let rank = self.holder_rank(0, id);
        let (progress, place) = (rank.key.progress(), rank.key.place());
        let place_weight = self.place_weight(progress, place);
        let holder_weight = count_decay(self.selected[id]);
        let tier = self.tiers.entry(progress).or_default();
        tier.weights.insert(place, place_weight);
        let members = tier.places.entry(place).or_default();
        members.ids.insert(id, holder_weight);
        members.ranked.resize_with(K::preferences(), BTreeSet::new);
        for (preference, ranked) in members.ranked.iter_mut().enumerate() {
            ranked.insert(HolderRank { preference, ..rank });
        }
    }

    fn place_weight(&self, progress: K::Progress, place: K::Place) -> u64 {
        count_decay(
            self.cells
                .get(&(progress, place))
                .map_or(0, |state| state.draws),
        )
    }

    fn holder_order(&self, preference: usize, left: usize, right: usize) -> Ordering {
        self.entries[left]
            .key
            .preference_cmp(preference, self.entries[right].key)
            .then_with(|| {
                (self.cost_in_group[right], self.entries[right].id)
                    .cmp(&(self.cost_in_group[left], self.entries[left].id))
            })
    }

    fn cell_best(&self, ids: &WeightedSet<usize>) -> Vec<usize> {
        (0..K::preferences())
            .filter_map(|preference| {
                ids.iter().reduce(|best, id| {
                    if self.holder_order(preference, id, best) == Ordering::Greater {
                        id
                    } else {
                        best
                    }
                })
            })
            .collect()
    }

    fn holder_rank(&self, preference: usize, index: usize) -> HolderRank<K> {
        HolderRank {
            key: self.entries[index].key,
            cost: self.cost_in_group[index],
            entry: self.entries[index].id,
            index,
            preference,
        }
    }

    fn cell_ranks(&self, ids: &WeightedSet<usize>) -> Vec<BTreeSet<HolderRank<K>>> {
        (0..K::preferences())
            .map(|preference| {
                ids.iter()
                    .map(|id| self.holder_rank(preference, id))
                    .collect()
            })
            .collect()
    }

    fn reweight_selector_index(&mut self) {
        let mut tiers = std::mem::take(&mut self.tiers);
        for (progress, tier) in &mut tiers {
            tier.weights = WeightedSet::from_sorted(
                tier.places
                    .keys()
                    .map(|place| (*place, self.place_weight(*progress, *place))),
            );
            for members in tier.places.values_mut() {
                let ids = WeightedSet::from_sorted(
                    members
                        .ids
                        .iter()
                        .map(|id| (id, count_decay(self.selected[id]))),
                );
                members.ranked = self.cell_ranks(&ids);
                members.ids = ids;
            }
        }
        self.tiers = tiers;
        self.selector_weighted = true;
    }

    fn sync_parent_index(&mut self) {
        if self.parent_index.len() > self.entries.len() {
            self.parent_index.clear();
        }
        for index in self.parent_index.len()..self.entries.len() {
            let parent = self.entries[index]
                .parent_id
                .and_then(|parent| self.id_to_index.get(&parent).copied())
                .unwrap_or(usize::MAX);
            self.parent_index.push(parent);
        }
    }

    #[must_use]
    pub fn lineage(&self, id: usize) -> Option<&K::Lineage> {
        self.lineages.get(id)
    }

    pub fn entry_input(&self, id: usize) -> Result<Input<A>, &'static str> {
        self.materialize_input(id)
    }

    #[must_use]
    pub fn entry_key(&self, id: usize) -> Option<K> {
        self.entries.get(id).map(|entry| entry.key)
    }

    #[must_use]
    pub fn replacement_cost_displaced(&self) -> u64 {
        self.replacement_cost_displaced
    }

    #[cfg(test)]
    pub(crate) fn entry_cost_in_group(&self, id: usize) -> u64 {
        self.cost_in_group[id]
    }

    #[must_use]
    pub fn top_progress(&self) -> Option<K::Progress> {
        self.tiers.last_key_value().map(|(progress, _)| *progress)
    }

    pub(crate) fn resident_snapshot_entries(&self) -> impl Iterator<Item = (u64, &S)> {
        self.entries.iter().filter_map(|entry| {
            entry
                .snapshot
                .as_deref()
                .map(|snapshot| (entry.id, snapshot))
        })
    }

    pub(crate) fn restore_runtime(
        &mut self,
        action_cost: fn(&A) -> u64,
        charge: Option<fn(&S) -> usize>,
        snapshots: Vec<(u64, S)>,
    ) -> Result<(), &'static str> {
        self.action_cost = action_cost;
        self.selector_accounting
            .best_holder_draws
            .resize(K::preferences(), 0);
        self.snapshot_memory_charge = if self.memory_limit.is_some() {
            Some(charge.ok_or("a memory-budgeted archive needs its snapshot charge")?)
        } else {
            None
        };
        for (id, snapshot) in snapshots {
            let index = self
                .index_of_id(id)
                .ok_or("a checkpoint snapshot names a missing archive entry")?;
            self.entries[index].snapshot = Some(Arc::new(snapshot));
        }
        self.validate_owned_prefixes()
    }

    fn validate_owned_prefixes(&self) -> Result<(), &'static str> {
        let depths = self.input_index.depths();
        for entry in &self.entries {
            if self.input_index.owner(entry.input_node) == Some(entry.id)
                && depths.get(entry.input_node).copied().flatten() != Some(entry.input_len)
            {
                return Err("a checkpoint entry's input length does not match its stored prefix");
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn live_progress(&self) -> Option<(K, u64, u64)> {
        self.live_progress
            .map(|(deepest, cheapest)| (deepest, cheapest, self.retained))
    }

    fn slot_members(&self, slot: &[usize]) -> Vec<(K, u64, u64)> {
        slot.iter()
            .map(|id| {
                (
                    self.entries[*id].key,
                    self.cost_in_group[*id],
                    self.entries[*id].id,
                )
            })
            .collect()
    }

    fn visit_preference_winners(members: &[(K, u64, u64)], mut winner: impl FnMut(usize, usize)) {
        let capacity = K::capacity().max(1);
        let preferences = K::preferences().max(1);
        let mut order: Vec<usize> = (0..members.len()).collect();
        for preference in 0..preferences {
            order.sort_by(|left, right| {
                let (left_key, left_cost, left_id) = members[*left];
                let (right_key, right_cost, right_id) = members[*right];
                left_key
                    .preference_cmp(preference, right_key)
                    .reverse()
                    .then_with(|| left_cost.cmp(&right_cost))
                    .then_with(|| left_id.cmp(&right_id))
            });
            for index in order.iter().take(capacity) {
                winner(*index, preference);
            }
        }
    }

    fn preferences_won(members: &[(K, u64, u64)]) -> Vec<Vec<usize>> {
        let mut won = vec![Vec::new(); members.len()];
        Self::visit_preference_winners(members, |index, preference| {
            won[index].push(preference);
        });
        won
    }

    fn rank_slot_preferences(
        &self,
        slot: &[usize],
        key: K,
        cost_in_group: u64,
    ) -> (Vec<bool>, Vec<usize>) {
        let mut members = Vec::with_capacity(slot.len() + 1);
        members.extend(slot.iter().map(|id| {
            (
                self.entries[*id].key,
                self.cost_in_group[*id],
                self.entries[*id].id,
            )
        }));
        members.push((key, cost_in_group, self.next_entry_id));
        let mut retained = vec![false; members.len()];
        let mut candidate = Vec::new();
        Self::visit_preference_winners(&members, |index, preference| {
            retained[index] = true;
            if index == slot.len() {
                candidate.push(preference);
            }
        });
        (retained, candidate)
    }

    pub(crate) fn rerank_slot_holders(&mut self) -> usize {
        let displaced: Vec<usize> = self
            .slots
            .values()
            .flat_map(|holders| {
                Self::preferences_won(&self.slot_members(holders))
                    .into_iter()
                    .zip(holders.iter().copied())
                    .filter_map(|(won, id)| won.is_empty().then_some(id))
                    .collect::<Vec<_>>()
            })
            .collect();
        let mut retired = 0;
        for id in displaced {
            if self.deactivate(id) {
                retired += 1;
            }
        }
        self.portfolio_replacements
            .resize(K::preferences().max(1), 0);
        if let Some(bank) = &mut self.continuations {
            bank.retain_preferences(K::preferences());
        }
        retired
    }

    fn cost_in_group_of(&self, parent_id: Option<usize>, suffix: &[A], key: K) -> u64 {
        let cost_of = |actions: &[A]| -> u64 {
            actions
                .iter()
                .map(|action| (self.action_cost)(action))
                .sum()
        };
        let Some(parent) = parent_id.and_then(|id| self.entries.get(id)) else {
            return cost_of(suffix);
        };
        let added = cost_of(suffix);
        if parent.key.progress() == key.progress() {
            self.cost_in_group
                .get(parent_id.unwrap_or_default())
                .copied()
                .unwrap_or(0)
                .saturating_add(added)
        } else {
            added
        }
    }

    pub fn insert<T: AsRef<[A]> + Into<Vec<A>>>(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate<T, K, M>,
        snapshot: S,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        self.insert_after(parent_id, None, execution, candidate, snapshot)
            .map(|(id, _)| id)
    }

    pub(crate) fn complete_after(
        &self,
        parent_id: Option<usize>,
        previous: Option<K>,
        key: K,
    ) -> Result<K, Box<dyn Error>> {
        if parent_id.is_some_and(|id| self.entries.get(id).is_none()) {
            return Err("archive candidate parent is missing".into());
        }
        let parent_ctx =
            parent_id.map(|id| (previous.unwrap_or(self.entries[id].key), &self.lineages[id]));
        Ok(key.complete(parent_ctx))
    }

    pub fn insert_after<T: AsRef<[A]> + Into<Vec<A>>>(
        &mut self,
        parent_id: Option<usize>,
        previous: Option<K>,
        execution: u64,
        candidate: ArchiveCandidate<T, K, M>,
        snapshot: S,
    ) -> Result<(Option<usize>, K), Box<dyn Error>> {
        let ArchiveCandidate {
            suffix,
            key,
            milestones,
        } = candidate;
        if let Some(existing) = self.existing_input_id(parent_id, suffix.as_ref()) {
            return Ok((Some(existing), self.entries[existing].key));
        }
        let key = self.complete_after(parent_id, previous, key)?;
        let candidate_cost_in_group = self.cost_in_group_of(parent_id, suffix.as_ref(), key);
        let slot = self
            .slots
            .get(&slot_of_key(key))
            .map(Vec::as_slice)
            .unwrap_or_default();
        let (ranked, won_preferences) =
            self.rank_slot_preferences(slot, key, candidate_cost_in_group);
        let admitted = ranked.last().copied().unwrap_or(true);
        if !admitted {
            self.rejected = self.rejected.saturating_add(1);
            return Ok((None, key));
        }
        let slot = slot.to_vec();
        let mut lineage =
            parent_id.map_or_else(K::Lineage::default, |id| self.lineages[id].clone());
        K::record(&mut lineage, key);
        let new_cell = self
            .cells
            .get(&cell_of(key))
            .is_none_or(|state| state.active == 0);
        let new_slot = slot.is_empty();
        let displaced: Vec<usize> = slot
            .iter()
            .copied()
            .zip(&ranked)
            .filter_map(|(id, retained)| (!retained).then_some(id))
            .collect();
        let population_retirements = self
            .active_count
            .saturating_sub(self.max_entries)
            .saturating_add(1);
        for _ in 0..population_retirements {
            if self.active_count < self.max_entries {
                break;
            }
            if !self.drop_one_entry() {
                if self.active_count == 1 && self.deactivate_liveness_anchor_for_admission() {
                    continue;
                }
                return Err("archive population limit cannot retire an entry".into());
            }
        }
        if self.active_count >= self.max_entries {
            return Err("archive population limit did not retire an entry".into());
        }
        if won_preferences.len() > 1 {
            self.portfolio_cross_improvements = self.portfolio_cross_improvements.saturating_add(1);
        }
        let mut replacement_preferences = 0_u8;
        if !displaced.is_empty() {
            for preference in &won_preferences {
                if let Some(count) = self.portfolio_replacements.get_mut(*preference) {
                    *count = count.saturating_add(1);
                }
                let strictly_preferred = displaced.iter().any(|replaced| {
                    key.preference_cmp(*preference, self.entries[*replaced].key)
                        == Ordering::Greater
                });
                if strictly_preferred && *preference < 8 {
                    replacement_preferences |= 1 << preference;
                }
            }
        }
        let improved_preference = !slot.is_empty()
            && won_preferences.iter().any(|preference| {
                slot.iter().all(|held| {
                    key.preference_cmp(*preference, self.entries[*held].key) == Ordering::Greater
                })
            });
        let queue_tier = won_preferences
            .iter()
            .filter(|preference| {
                slot.iter().any(|held| {
                    key.preference_cmp(**preference, self.entries[*held].key) == Ordering::Greater
                })
            })
            .min()
            .map(|preference| u8::try_from(*preference).unwrap_or(u8::MAX));
        if let Some(bank) = &mut self.continuations {
            if let Some(tier) = queue_tier {
                bank.improved(
                    position_of(key),
                    self.next_entry_id,
                    self.continuation_wave,
                    tier,
                );
            }
            if let Some(parent) = parent_id {
                let tail_cost: u64 = suffix
                    .as_ref()
                    .iter()
                    .map(|action| (self.action_cost)(action))
                    .sum();
                let entry = &self.entries[parent];
                let gains = (0..K::preferences().min(8))
                    .filter(|preference| {
                        key.preference_cmp(*preference, entry.key) == Ordering::Greater
                    })
                    .fold(0_u8, |mask, preference| mask | (1 << preference));
                bank.record(
                    position_of(entry.key),
                    position_of(key),
                    entry.id,
                    self.next_entry_id,
                    suffix.as_ref(),
                    tail_cost,
                    gains,
                );
            }
        }
        for replaced in displaced {
            self.replacement_cost_displaced = self.replacement_cost_displaced.saturating_add(1);
            self.deactivate(replaced);
        }
        let id = self.entries.len();
        let stable_id = self.next_entry_id;
        self.next_entry_id = self
            .next_entry_id
            .checked_add(1)
            .ok_or("archive stable id space exhausted")?;
        let snapshot_charge = self.snapshot_charge(&snapshot);
        let parent_input_len = parent_id.map_or(0, |parent| self.entries[parent].input_len);
        let input_len = parent_input_len
            .checked_add(suffix.as_ref().len())
            .ok_or("archive candidate input length overflow")?;
        self.historical_input_actions = self.historical_input_actions.saturating_add(input_len);
        self.stored_input_actions = self
            .stored_input_actions
            .saturating_add(suffix.as_ref().len());
        let (input_node, new_nodes) =
            self.index_retained_input(parent_id, suffix.as_ref(), stable_id)?;
        let suffix_len = suffix.as_ref().len();
        self.entries.push(ArchiveEntry {
            id: stable_id,
            parent_id: parent_id.map(|parent| self.entries[parent].id),
            created_execution: execution,
            input_suffix: suffix.into().into_boxed_slice().into_vec(),
            input_len,
            input_node,
            key,
            milestones,
            snapshot: Some(Arc::new(snapshot)),
        });
        self.id_to_index.insert(stable_id, id);
        self.snapshot_selectable.push(true);
        self.resident_snapshots = self.resident_snapshots.saturating_add(1);
        self.resident_snapshot_bytes = self.resident_snapshot_bytes.saturating_add(snapshot_charge);
        let inherited_keyframe = parent_id
            .filter(|parent| {
                Self::replay_bucket(self.entries[*parent].input_len)
                    == Self::replay_bucket(input_len)
            })
            .and_then(|parent| self.index_of_id(self.keyframe_id[parent]))
            .filter(|keyframe| self.snapshot_selectable[*keyframe]);
        let keyframe_index = inherited_keyframe.unwrap_or(id);
        self.keyframe.push(inherited_keyframe.is_none());
        self.keyframe_id.push(if inherited_keyframe.is_none() {
            stable_id
        } else {
            self.entries[keyframe_index].id
        });
        self.keyframe_dependents.push(0);
        self.keyframe_dependents[keyframe_index] =
            self.keyframe_dependents[keyframe_index].saturating_add(1);
        self.referenced.push(true);
        if inherited_keyframe.is_some() {
            self.resident_snapshot_order.push_back(id);
        }
        self.active.push(true);
        self.active_count = self.active_count.saturating_add(1);
        self.lineages.push(lineage);
        self.cost_in_group.push(candidate_cost_in_group);
        self.replacement_preferences.push(replacement_preferences);
        self.in_place_lengths.push(0);
        match &mut self.live_progress {
            Some((deepest, cheapest)) => match key.progress().cmp(&deepest.progress()) {
                Ordering::Greater => {
                    *deepest = key;
                    *cheapest = candidate_cost_in_group;
                }
                Ordering::Equal => {
                    *cheapest = (*cheapest).min(candidate_cost_in_group);
                }
                Ordering::Less => {}
            },
            None => self.live_progress = Some((key, candidate_cost_in_group)),
        }
        self.selected.push(0);
        self.productive.push(0);
        self.opened_cell.push(new_cell);
        self.opened_slot.push(new_slot);
        self.deepest_leaf.push((key, id));
        self.sync_parent_index();
        let mut ancestor = parent_id.unwrap_or(usize::MAX);
        while let Some(previous) = self.deepest_leaf.get(ancestor).copied() {
            if leaf_order(previous, (key, id)) != Ordering::Less {
                break;
            }
            self.deepest_leaf[ancestor] = (key, id);
            ancestor = self.parent_index[ancestor];
        }
        self.activate_membership(id);
        let carried_in =
            parent_id.is_some_and(|parent| cell_of(self.entries[parent].key) != cell_of(key));
        let carried_win = carried_in && (replacement_preferences != 0 || improved_preference);
        if new_cell || carried_win {
            let next = self
                .tiers
                .range(..key.progress())
                .next_back()
                .map(|(progress, _)| (format!("{progress:?}"), self.tier_runs(*progress)));
            let runs = self.tier_runs_mut(key.progress());
            runs.yields = runs.yields.saturating_add(1);
            if new_cell {
                runs.longest = runs.longest.max(runs.current);
                runs.current = 0;
                runs.wins = 0;
                let (tier, next_runs) = next.unwrap_or_default();
                runs.next_tier = tier;
                runs.next_draws = next_runs.draws;
                runs.next_yields = next_runs.yields;
            } else {
                runs.wins = runs.wins.saturating_add(1);
            }
        }
        if replacement_preferences != 0 && carried_in {
            self.reset_cell_draws(cell_of(key));
        }
        if new_cell {
            self.reset_cell_draws(cell_of(key));
            if let Some(parent) = parent_id {
                self.reset_cell_draws(cell_of(self.entries[parent].key));
            }
        }
        self.history_memory_bytes = self
            .history_memory_bytes
            .saturating_add(Self::history_entry_memory_charge(suffix_len, new_nodes));
        self.retained = self.retained.saturating_add(1);
        self.index_insert(id);
        self.enforce_snapshot_memory_budget()?;
        Ok((Some(id), key))
    }

    pub(crate) fn splice_tail_for_campaign(
        &mut self,
        parent: usize,
        longest_tail: usize,
    ) -> Option<CampaignSpliceTail<A>> {
        self.ensure_selector_index();
        let parent_key = self.entries[parent].key;
        let donor_id = self
            .slots
            .get(&slot_of_key(parent_key))?
            .iter()
            .copied()
            .filter(|id| *id != parent && self.active_ids.contains(*id))
            .max_by(|left, right| {
                leaf_order(self.deepest_leaf[*left], self.deepest_leaf[*right])
                    .then_with(|| left.cmp(right))
            })?;
        let (leaf_key, leaf_id) = self.deepest_leaf[donor_id];
        if !leaf_advances((leaf_key, leaf_id), (parent_key, parent)) {
            return None;
        }
        let actions = self
            .recorded_splice_tail(parent, donor_id, leaf_id, longest_tail)
            .ok()?;
        Some(CampaignSpliceTail {
            donor_id,
            leaf_id,
            actions,
        })
    }

    pub(crate) fn recorded_splice_tail(
        &self,
        parent: usize,
        donor: usize,
        leaf: usize,
        longest_tail: usize,
    ) -> Result<Vec<A>, &'static str> {
        let parent_entry = self
            .entries
            .get(parent)
            .ok_or("splice parent id is outside the archive")?;
        let donor_entry = self
            .entries
            .get(donor)
            .ok_or("splice donor id is outside the archive")?;
        let leaf_entry = self
            .entries
            .get(leaf)
            .ok_or("splice leaf id is outside the archive")?;
        if donor == parent {
            return Err("splice donor is the selected parent");
        }
        let parent_key = parent_entry.key;
        if cell_of(parent_key) != cell_of(donor_entry.key) {
            return Err("splice donor is outside the parent's selection cell");
        }
        let suffix = self.input_index.materialize_splice_tail(
            (
                donor_entry.input_node,
                donor_entry.input_len,
                donor_entry.id,
            ),
            leaf_entry.input_node,
            leaf_entry.input_len,
            longest_tail,
        )?;
        if !leaf_advances((leaf_entry.key, leaf), (parent_key, parent)) {
            return Err("splice leaf does not advance past the parent");
        }
        if leaf_entry.input_len == donor_entry.input_len {
            return Err("splice leaf has no actions past its donor");
        }
        Ok(suffix)
    }

    fn active_ids(&self) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| (*active && self.origin_resident(id)).then_some(id))
            .collect()
    }

    pub fn select_parent(
        &mut self,
        rand: &mut RomuDuoJrRand,
    ) -> Result<(usize, SelectorDraw), Box<dyn Error>> {
        self.ensure_selector_index();
        if self.active_ids.is_empty() {
            return Err("archive has no expandable entry".into());
        }
        let (progress, rank) = self.draw_tier(rand)?;
        debug_assert!(
            self.tiers
                .get(&progress)
                .is_some_and(|tier| self.tier_weights_current(progress, tier))
        );
        let recent = self.draw_recent_cell(rand, progress)?;
        let place = match recent {
            Some(place) => place,
            None => self.draw_cell(rand, progress)?,
        };
        let (id, best_preference) = self.draw_holder(rand, progress, place)?;
        Ok((
            id,
            SelectorDraw {
                path: if recent.is_some() {
                    SelectorPath::Recent
                } else {
                    SelectorPath::Tiers
                },
                tier_rank: Some(rank),
                best_preference,
            },
        ))
    }

    fn draw_recent_cell(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
    ) -> Result<Option<K::Place>, Box<dyn Error>> {
        if rand.below(NonZeroUsize::new(2).unwrap()) != 0 {
            return Ok(None);
        }
        let places: BTreeMap<_, _> = self
            .active_ids
            .ids
            .iter()
            .rev()
            .take(256)
            .filter_map(|id| {
                let key = self.entries[*id].key;
                if key.progress() != progress {
                    return None;
                }
                let draws = self.cells.get(&cell_of(key)).map_or(0, |cell| cell.draws);
                (draws < 32).then_some((key.place(), draws))
            })
            .collect();
        if places.is_empty() {
            return Ok(None);
        }
        let index = draw_weighted(rand, places.values().map(|draws| count_decay(*draws)))?;
        Ok(places.into_keys().nth(index))
    }

    fn tier_runs(&self, progress: K::Progress) -> TierRuns {
        self.selector_accounting
            .tier_runs
            .get(&format!("{progress:?}"))
            .cloned()
            .unwrap_or_default()
    }

    fn tier_runs_mut(&mut self, progress: K::Progress) -> &mut TierRuns {
        self.selector_accounting
            .tier_runs
            .entry(format!("{progress:?}"))
            .or_default()
    }

    fn draw_tier(&self, rand: &mut RomuDuoJrRand) -> Result<(K::Progress, u8), Box<dyn Error>> {
        let shift = checked_tier_rank_shift(K::tier_rank_shift())?;
        let tiers = self.tiers.len();
        let halvings = self.tiers.iter().next_back().map_or(0, |(progress, tier)| {
            let runs = self.tier_runs(*progress);
            let cells = u64::try_from(tier.places.len().max(1)).unwrap_or(u64::MAX);
            let pace = runs.longest.max(cells);
            let (next_draws, next_yields) =
                self.tiers.iter().rev().nth(1).map_or((0, 0), |(next, _)| {
                    let counts = self.tier_runs(*next);
                    if format!("{next:?}") == runs.next_tier {
                        (
                            counts.draws.saturating_sub(runs.next_draws),
                            counts.yields.saturating_sub(runs.next_yields),
                        )
                    } else {
                        (counts.draws, counts.yields)
                    }
                });
            let next_yields_more = u128::from(next_yields.saturating_add(1))
                * u128::from(runs.current.saturating_add(1))
                > u128::from(runs.wins.saturating_add(1))
                    * u128::from(next_draws.saturating_add(1));
            if next_yields_more {
                (runs.current.saturating_sub(1) / pace)
                    .max(1)
                    .ilog2()
                    .min(shift)
            } else {
                0
            }
        });
        let weights = (0..tiers).map(|rank| {
            let weight = tier_weight(u8::try_from(rank).unwrap_or(u8::MAX), shift);
            if rank == 0 && tiers > 1 {
                weight >> halvings
            } else {
                weight
            }
        });
        let index = draw_weighted(rand, weights)?;
        let progress = self
            .tiers
            .keys()
            .rev()
            .nth(index)
            .copied()
            .ok_or("tier draw chose an absent tier")?;
        Ok((progress, u8::try_from(index).unwrap_or(u8::MAX)))
    }

    fn draw_cell(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
    ) -> Result<K::Place, Box<dyn Error>> {
        let tier = self
            .tiers
            .get(&progress)
            .ok_or("tier draw chose an absent tier")?;
        draw_member(rand, &tier.weights)
    }

    fn tier_weights_current(&self, progress: K::Progress, tier: &TierCells<K>) -> bool {
        tier.weights.len() == tier.places.len()
            && tier.places.iter().all(|(place, members)| {
                tier.weights.weight(place) == Some(self.place_weight(progress, *place))
                    && members.ids.len() == members.ids.iter().count()
                    && members
                        .ids
                        .iter()
                        .all(|id| members.ids.weight(&id) == Some(count_decay(self.selected[id])))
                    && members.ranked.len() == K::preferences()
                    && members
                        .ranked
                        .iter()
                        .enumerate()
                        .all(|(preference, ranked)| {
                            ranked.len() == members.ids.len()
                                && ranked.iter().all(|rank| {
                                    members.ids.weight(&rank.index).is_some()
                                        && rank.preference == preference
                                        && rank.key == self.entries[rank.index].key
                                        && rank.cost == self.cost_in_group[rank.index]
                                        && rank.entry == self.entries[rank.index].id
                                })
                        })
                    && members
                        .ranked
                        .iter()
                        .map(|ranked| ranked.last().map(|rank| rank.index))
                        .collect::<Option<Vec<_>>>()
                        == Some(self.cell_best(&members.ids))
            })
    }

    fn draw_best_share(&self, rand: &mut RomuDuoJrRand) -> Result<Option<usize>, Box<dyn Error>> {
        let preferences = K::preferences();
        let Some(outcomes) =
            NonZeroUsize::new(preferences.saturating_mul(BEST_HOLDER_SHARE_DENOMINATOR))
        else {
            return Ok(None);
        };
        let outcome = rand.below(outcomes);
        Ok((outcome < preferences).then_some(outcome))
    }

    fn best_holder(
        &self,
        members: &CellMembers<K>,
        preference: usize,
    ) -> Result<usize, Box<dyn Error>> {
        members
            .ranked
            .get(preference)
            .and_then(BTreeSet::last)
            .map(|rank| rank.index)
            .ok_or_else(|| "cell draw chose an empty cell".into())
    }

    fn draw_holder(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
        place: K::Place,
    ) -> Result<(usize, Option<u8>), Box<dyn Error>> {
        let members = self
            .tiers
            .get(&progress)
            .and_then(|tier| tier.places.get(&place))
            .ok_or("cell draw chose an absent cell")?;
        if let Some(preference) = self.draw_best_share(rand)? {
            return Ok((
                self.best_holder(members, preference)?,
                Some(u8::try_from(preference)?),
            ));
        }
        Ok((draw_member(rand, &members.ids)?, None))
    }

    fn champions_slot(&self, slot: &[usize], id: usize, preference: usize) -> bool {
        let capacity = K::capacity().max(1);
        let better = slot
            .iter()
            .filter(|other| **other != id)
            .filter(|other| self.entries.get(**other).is_some())
            .filter(|other| {
                let other = **other;
                match self.entries[other]
                    .key
                    .preference_cmp(preference, self.entries[id].key)
                {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => {
                        (self.cost_in_group[other], self.entries[other].id)
                            < (self.cost_in_group[id], self.entries[id].id)
                    }
                }
            })
            .take(capacity)
            .count();
        better < capacity
    }

    #[cfg(test)]
    fn is_preference_champion(&self, id: usize, preference: usize) -> bool {
        let Some(slot) = self.slots.get(&slot_of_key(self.entries[id].key)) else {
            return true;
        };
        self.champions_slot(slot, id, preference)
    }

    fn reset_cell_draws(&mut self, cell: Cell<K>) {
        if let Some(state) = self.cells.get_mut(&cell)
            && state.draws != 0
        {
            state.draws = 0;
            self.selector_accounting.cell_resets =
                self.selector_accounting.cell_resets.saturating_add(1);
            if let Some(tier) = self.tiers.get_mut(&cell.0) {
                tier.weights.set_weight(&cell.1, count_decay(0));
            }
        }
    }

    #[must_use]
    pub fn opened_new_cell(&self, id: usize) -> bool {
        self.opened_cell.get(id).copied().unwrap_or(false)
    }

    #[must_use]
    pub fn opened_new_slot(&self, id: usize) -> bool {
        self.opened_slot.get(id).copied().unwrap_or(false)
    }

    #[must_use]
    pub fn occupied_cell_count(&self) -> usize {
        self.cells.values().filter(|state| state.active > 0).count()
    }

    #[must_use]
    pub fn cell_draws(&self, key: K) -> u64 {
        self.cells.get(&cell_of(key)).map_or(0, |state| state.draws)
    }

    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_count
    }

    pub(crate) fn retained_snapshots(&self) -> impl Iterator<Item = (Option<&S>, u64)> {
        self.entries
            .iter()
            .zip(&self.active)
            .enumerate()
            .filter_map(|(id, (entry, active))| {
                active.then_some((entry.snapshot.as_deref(), self.selected[id]))
            })
    }

    #[must_use]
    pub fn resident_snapshot_count(&self) -> usize {
        self.resident_snapshots
    }

    #[must_use]
    pub fn resident_snapshot_bytes(&self) -> usize {
        self.resident_snapshot_bytes
            .saturating_add(self.inflight_snapshot_bytes)
    }

    #[must_use]
    pub fn snapshot_evictions(&self) -> u64 {
        self.snapshot_evictions
    }

    pub fn entry_drops(&self) -> u64 {
        self.entry_drops
    }

    #[must_use]
    pub fn live_entry_count(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn history_compactions(&self) -> u64 {
        self.history_compactions
    }

    #[must_use]
    pub fn historical_entries_dropped(&self) -> u64 {
        self.historical_entries_dropped
    }

    #[must_use]
    pub fn input_reconstructions(&self) -> u64 {
        self.input_reconstructions.get()
    }

    #[must_use]
    pub(crate) fn historical_input_actions(&self) -> usize {
        self.historical_input_actions
    }

    #[must_use]
    pub(crate) fn stored_input_actions(&self) -> usize {
        self.stored_input_actions
    }

    pub(crate) fn resume_continuations(&mut self, longest_edge: usize) {
        match (&mut self.continuations, K::preferences() > 0) {
            (Some(bank), true) => bank.set_longest_edge(longest_edge),
            (None, true) => self.continuations = Some(ContinuationBank::new(longest_edge)),
            (_, false) => self.continuations = None,
        }
    }

    pub(crate) fn enable_continuations(&mut self, longest_edge: usize) {
        self.continuations = (K::preferences() > 0).then(|| ContinuationBank::new(longest_edge));
    }

    pub(crate) fn set_admission_wave(&mut self, wave: u32) {
        self.continuation_wave = wave;
    }

    pub(crate) fn pop_continuation(
        &mut self,
        from_highest_preference: bool,
    ) -> Option<Continuation<Position<K>, A>> {
        self.continuations.as_mut()?.pop(from_highest_preference)
    }

    #[must_use]
    pub(crate) fn outranks_slot_holders(
        &self,
        parent_index: usize,
        destination: Position<K>,
        tier: u8,
    ) -> bool {
        let Some(entry) = self.entries.get(parent_index) else {
            return false;
        };
        let key = entry.key;
        let preference = usize::from(tier);
        let (place, identity) = destination;
        let slot = ((key.progress(), place), identity);
        self.slots.get(&slot).is_none_or(|holders| {
            holders.iter().all(|held| {
                self.entries.get(*held).is_none_or(|holder| {
                    key.preference_cmp(preference, holder.key) == Ordering::Greater
                })
            })
        })
    }

    #[must_use]
    pub(crate) fn continuation_pending(&self) -> usize {
        self.continuations
            .as_ref()
            .map_or(0, ContinuationBank::pending_count)
    }

    #[must_use]
    pub fn continuation_report(&self) -> ContinuationAccounting {
        ContinuationAccounting {
            edges: self
                .continuations
                .as_ref()
                .map_or(0, ContinuationBank::edge_count),
            pending: self.continuation_pending(),
            ..self.continuation_accounting.clone()
        }
    }

    pub(crate) fn record_continuation_outcome(
        &mut self,
        landed: Option<usize>,
        replaced: bool,
        opened_new_cell: bool,
        wave: u32,
    ) {
        if let Some(id) = landed
            && let Some(entry) = self.entries.get(id)
        {
            self.landed.insert(entry.id);
        }
        let accounting = &mut self.continuation_accounting;
        accounting.jobs = accounting.jobs.saturating_add(1);
        if landed.is_some() {
            accounting.landed = accounting.landed.saturating_add(1);
        }
        if replaced {
            accounting.replaced = accounting.replaced.saturating_add(1);
        }
        if opened_new_cell {
            accounting.opened_new_cell = accounting.opened_new_cell.saturating_add(1);
        }
        accounting.longest_wave = accounting.longest_wave.max(wave);
    }

    pub(crate) fn record_continuation_dispatch(&mut self, gaining: bool) {
        if gaining {
            self.continuation_accounting.gaining_dispatched = self
                .continuation_accounting
                .gaining_dispatched
                .saturating_add(1);
        }
    }

    pub(crate) fn record_continuation_reservation(&mut self, energy: u16, taken: bool) {
        let accounting = &mut self.continuation_accounting;
        accounting.energy = energy;
        accounting.reservations_drawn = accounting.reservations_drawn.saturating_add(1);
        if taken {
            accounting.reservations_taken = accounting.reservations_taken.saturating_add(1);
        }
    }

    pub(crate) fn add_continuation_execution_work(&mut self, work: u64) {
        self.continuation_accounting.execution_work = self
            .continuation_accounting
            .execution_work
            .saturating_add(work);
    }

    #[must_use]
    pub(crate) fn lands_on_edge(
        &self,
        id: usize,
        source: Position<K>,
        destination: Position<K>,
    ) -> bool {
        self.entries.get(id).is_some_and(|entry| {
            let position = position_of(entry.key);
            if source.0 == destination.0 {
                position == destination
            } else {
                position.0 == destination.0
            }
        })
    }

    #[must_use]
    pub fn history_memory_bytes(&self) -> usize {
        self.history_memory_bytes
            .saturating_add(self.auxiliary_history_memory_bytes())
            .saturating_add(
                self.continuations
                    .as_ref()
                    .map_or(0, ContinuationBank::memory_bytes),
            )
    }

    #[must_use]
    pub(crate) fn entry_metadata_memory_bytes(&self) -> usize {
        self.entries
            .len()
            .saturating_mul(Self::history_entry_memory_charge(0, 0))
    }

    #[must_use]
    pub(crate) fn input_index_memory_bytes(&self) -> usize {
        self.input_index
            .live_nodes
            .saturating_mul(Self::prefix_node_memory_charge())
            .saturating_add(self.stored_input_actions.saturating_mul(size_of::<A>()))
    }

    #[must_use]
    pub(crate) fn cell_memory_bytes(&self) -> usize {
        self.cells.len().saturating_mul(Self::cell_memory_charge())
    }

    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        self.history_memory_bytes()
            .saturating_add(self.resident_snapshot_bytes())
    }

    #[must_use]
    pub(crate) fn input_index_nodes(&self) -> usize {
        self.input_index.live_nodes
    }

    #[must_use]
    pub(crate) fn historical_cell_count(&self) -> usize {
        self.cells.len()
    }

    pub(crate) fn record_isolated_continuation(&mut self, id: usize) {
        self.referenced[id] = true;
        let count = self
            .selector_accounting
            .continuation_selections
            .get_or_insert(0);
        *count = count.saturating_add(1);
    }

    pub fn record_selection(&mut self, id: usize, draw: &SelectorDraw) {
        self.selected[id] = self.selected[id].saturating_add(1);
        self.referenced[id] = true;
        let key = self.entries[id].key;
        let runs = self.tier_runs_mut(key.progress());
        runs.current = runs.current.saturating_add(1);
        runs.draws = runs.draws.saturating_add(1);
        let state = self.cells.entry(cell_of(key)).or_default();
        state.draws = state.draws.saturating_add(1);
        state.draws_total = state.draws_total.saturating_add(1);
        let place_weight = count_decay(state.draws);
        let holder_weight = count_decay(self.selected[id]);
        if let Some(tier) = self.tiers.get_mut(&key.progress()) {
            tier.weights.set_weight(&key.place(), place_weight);
            if let Some(members) = tier.places.get_mut(&key.place()) {
                members.ids.set_weight(&id, holder_weight);
            }
        }
        match draw.path {
            SelectorPath::Continuation => {
                let count = self
                    .selector_accounting
                    .continuation_selections
                    .get_or_insert(0);
                *count = count.saturating_add(1);
            }
            SelectorPath::Tiers | SelectorPath::Recent => {
                if draw.path == SelectorPath::Recent {
                    let count = self.selector_accounting.recent_selections.get_or_insert(0);
                    *count = count.saturating_add(1);
                }
                self.selector_accounting.cell_selections =
                    self.selector_accounting.cell_selections.saturating_add(1);
            }
        }
        if let Some(count) = draw.best_preference.and_then(|preference| {
            self.selector_accounting
                .best_holder_draws
                .get_mut(usize::from(preference))
        }) {
            *count = count.saturating_add(1);
        }
        if let Some(rank) = draw.tier_rank {
            let rank = usize::from(rank);
            if self.selector_accounting.tier_draws_by_rank.len() <= rank {
                self.selector_accounting
                    .tier_draws_by_rank
                    .resize(rank.saturating_add(1), 0);
            }
            self.selector_accounting.tier_draws_by_rank[rank] =
                self.selector_accounting.tier_draws_by_rank[rank].saturating_add(1);
        }
    }

    pub fn record_selection_outcome(&mut self, id: usize, retained_descendant: bool) {
        if !retained_descendant {
            return;
        }
        self.productive[id] = self.productive[id].saturating_add(1);
        self.selector_accounting.productive_selections = self
            .selector_accounting
            .productive_selections
            .saturating_add(1);
        if self
            .entries
            .get(id)
            .is_some_and(|entry| self.landed.contains(&entry.id))
        {
            self.continuation_accounting.useful =
                self.continuation_accounting.useful.saturating_add(1);
        }
    }

    #[must_use]
    pub(crate) fn suffix_limit(&self, id: usize) -> u8 {
        self.in_place_lengths
            .get(id)
            .copied()
            .unwrap_or(0)
            .saturating_mul(2)
            .clamp(1, SUFFIX_DOUBLING_LIMIT)
    }

    pub(crate) fn record_suffix_outcome(
        &mut self,
        id: usize,
        actions: usize,
        kept: bool,
        left_place: bool,
        terminal: bool,
    ) {
        let Some(length) = self.in_place_lengths.get_mut(id) else {
            return;
        };
        if kept || left_place {
            *length = 0;
        } else if !terminal {
            *length = (*length).max(u8::try_from(actions).unwrap_or(u8::MAX));
        }
    }

    #[must_use]
    fn portfolio_holders(&self, preferences: usize) -> (u64, u64) {
        let mut exclusive = 0_u64;
        let mut shared = 0_u64;
        for slot in self.slots.values() {
            for id in slot {
                if !self.active.get(*id).copied().unwrap_or(false)
                    || self.entries.get(*id).is_none()
                {
                    continue;
                }
                let held = if slot.len() <= K::capacity().max(1) {
                    preferences.min(2)
                } else {
                    (0..preferences)
                        .filter(|preference| self.champions_slot(slot, *id, *preference))
                        .take(2)
                        .count()
                };
                match held {
                    0 => {}
                    1 => exclusive = exclusive.saturating_add(1),
                    _ => shared = shared.saturating_add(1),
                }
            }
        }
        (exclusive, shared)
    }

    #[must_use]
    pub fn replacement_preferences(&self, id: usize) -> u8 {
        self.replacement_preferences.get(id).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn selector_counters(&self) -> SelectorAccounting {
        self.selector_accounting.clone()
    }

    pub fn selector_report(&self) -> SelectorAccounting {
        let mut accounting = self.selector_counters();
        accounting.draws_by_cell = self
            .cells
            .iter()
            .filter(|(_, state)| state.draws_total > 0)
            .map(|(cell, state)| (format!("{cell:?}"), state.draws_total))
            .collect();
        let preferences = K::preferences().max(1);
        if preferences > 1 {
            let (exclusive, shared) = self.portfolio_holders(preferences);
            accounting.portfolio = Some(PortfolioAccounting {
                preferences,
                exclusive_holders: exclusive,
                shared_holders: shared,
                replacements_by_preference: self.portfolio_replacements.clone(),
                cross_preference_improvements: self.portfolio_cross_improvements,
            });
        }
        accounting
    }

    #[allow(clippy::type_complexity)]
    pub fn take_entry_reports_and_snapshots(
        &mut self,
    ) -> (Vec<ArchiveEntryReport<A, K, M>>, Vec<(u64, S)>) {
        let inputs = (0..self.entries.len())
            .map(|id| {
                self.materialize_input(id)
                    .unwrap_or_else(|_| Input::default())
            })
            .collect::<Vec<_>>();
        let entries = std::mem::take(&mut self.entries);
        let mut reports = Vec::with_capacity(entries.len());
        let mut snapshots = Vec::with_capacity(entries.len());
        for (id, (entry, input)) in entries.into_iter().zip(inputs).enumerate() {
            let snapshot_id = entry.id;
            let report = ArchiveEntryReport {
                id: entry.id,
                parent_id: entry.parent_id,
                created_execution: entry.created_execution,
                input,
                key: entry.key,
                milestones: entry.milestones,
                selector: Some(EntrySelectorCounters {
                    selected: self.selected[id],
                    productive: self.productive[id],
                }),
            };
            reports.push(report);
            if let Some(snapshot) = entry.snapshot {
                snapshots.push((
                    snapshot_id,
                    Arc::try_unwrap(snapshot).unwrap_or_else(|snapshot| (*snapshot).clone()),
                ));
            }
        }
        (reports, snapshots)
    }
}

#[cfg(test)]
impl<A, K, M, S> Archive<A, K, M, S>
where
    A: Clone + Debug + Eq + Ord + Serialize + DeserializeOwned,
    K: ArchiveKey,
    M: Clone + Copy + Debug + Eq + Serialize + DeserializeOwned,
    S: Clone,
{
    fn retain_indexed_landings_reference(&mut self) {
        let live_ids = self
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        self.landed.retain(|id| live_ids.contains(id));
    }

    fn recorded_splice_tail_reference(
        &self,
        parent: usize,
        donor: usize,
        leaf: usize,
        longest_tail: usize,
    ) -> Result<Vec<A>, &'static str> {
        let parent_entry = self
            .entries
            .get(parent)
            .ok_or("splice parent id is outside the archive")?;
        let donor_entry = self
            .entries
            .get(donor)
            .ok_or("splice donor id is outside the archive")?;
        let leaf_entry = self
            .entries
            .get(leaf)
            .ok_or("splice leaf id is outside the archive")?;
        if donor == parent {
            return Err("splice donor is the selected parent");
        }
        let parent_key = parent_entry.key;
        if cell_of(parent_key) != cell_of(donor_entry.key) {
            return Err("splice donor is outside the parent's selection cell");
        }
        let donor_input = self
            .input_index
            .materialize(donor_entry.input_node, donor_entry.input_len)
            .ok_or("splice donor prefix is unavailable")?;
        let leaf_input = self
            .input_index
            .materialize(leaf_entry.input_node, leaf_entry.input_len)
            .ok_or("splice leaf prefix is unavailable")?;
        if !leaf_input.starts_with(&donor_input) {
            return Err("splice leaf is not a descendant of its donor");
        }
        if !leaf_advances((leaf_entry.key, leaf), (parent_key, parent)) {
            return Err("splice leaf does not advance past the parent");
        }
        let suffix = &leaf_input[donor_input.len()..];
        if suffix.is_empty() {
            return Err("splice leaf has no actions past its donor");
        }
        Ok(suffix.iter().take(longest_tail).cloned().collect())
    }

    fn select_parent_candidates_reference(
        &mut self,
        rand: &mut RomuDuoJrRand,
    ) -> Result<(usize, SelectorDraw), Box<dyn Error>> {
        self.ensure_selector_index();
        if self.active_ids.is_empty() {
            return Err("archive has no expandable entry".into());
        }
        let (progress, rank) = self.draw_tier(rand)?;
        let recent = self.draw_recent_cell(rand, progress)?;
        let place = match recent {
            Some(place) => place,
            None => self.draw_cell_reference(rand, progress)?,
        };
        let (id, best_preference) = self.draw_holder_reference(rand, progress, place)?;
        Ok((
            id,
            SelectorDraw {
                path: if recent.is_some() {
                    SelectorPath::Recent
                } else {
                    SelectorPath::Tiers
                },
                tier_rank: Some(rank),
                best_preference,
            },
        ))
    }

    fn draw_cell_reference(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
    ) -> Result<K::Place, Box<dyn Error>> {
        let tier = self
            .tiers
            .get(&progress)
            .ok_or("tier draw chose an absent tier")?;
        let candidates = tier.places.keys().copied().collect::<Vec<_>>();
        let weights = candidates
            .iter()
            .map(|place| {
                count_decay(
                    self.cells
                        .get(&(progress, *place))
                        .map_or(0, |state| state.draws),
                )
            })
            .collect::<Vec<_>>();
        let index = draw_weighted(rand, weights.iter().copied())?;
        Ok(candidates[index])
    }

    fn draw_holder_reference(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
        place: K::Place,
    ) -> Result<(usize, Option<u8>), Box<dyn Error>> {
        let members = self
            .tiers
            .get(&progress)
            .and_then(|tier| tier.places.get(&place))
            .ok_or("cell draw chose an absent cell")?;
        let ids = members.ids.iter().collect::<Vec<_>>();
        let preferences = K::preferences();
        if preferences > 0 {
            let outcome = rand.below(
                NonZeroUsize::new(preferences * BEST_HOLDER_SHARE_DENOMINATOR)
                    .ok_or("empty share draw")?,
            );
            if outcome < preferences {
                let mut ranked = ids.clone();
                ranked.sort_by(|left, right| {
                    self.entries[*right]
                        .key
                        .preference_cmp(outcome, self.entries[*left].key)
                        .then_with(|| self.cost_in_group[*left].cmp(&self.cost_in_group[*right]))
                        .then_with(|| self.entries[*left].id.cmp(&self.entries[*right].id))
                });
                return Ok((ranked[0], Some(u8::try_from(outcome)?)));
            }
        }
        let weights = ids
            .iter()
            .map(|id| count_decay(self.selected[*id]))
            .collect::<Vec<_>>();
        let index = draw_weighted(rand, weights.iter().copied())?;
        Ok((ids[index], None))
    }

    fn champions_slot_reference(&self, slot: &[usize], id: usize, preference: usize) -> bool {
        let capacity = K::capacity().max(1);
        let better = slot
            .iter()
            .filter(|other| **other != id)
            .filter(|other| self.entries.get(**other).is_some())
            .filter(|other| {
                let other = **other;
                match self.entries[other]
                    .key
                    .preference_cmp(preference, self.entries[id].key)
                {
                    Ordering::Greater => true,
                    Ordering::Less => false,
                    Ordering::Equal => {
                        (self.cost_in_group[other], self.entries[other].id)
                            < (self.cost_in_group[id], self.entries[id].id)
                    }
                }
            })
            .count();
        better < capacity
    }

    fn portfolio_holders_reference(&self, preferences: usize) -> (u64, u64) {
        let mut exclusive = 0_u64;
        let mut shared = 0_u64;
        for slot in self.slots.values() {
            for id in slot {
                if !self.active.get(*id).copied().unwrap_or(false)
                    || self.entries.get(*id).is_none()
                {
                    continue;
                }
                let held = (0..preferences)
                    .filter(|preference| self.champions_slot_reference(slot, *id, *preference))
                    .count();
                match held {
                    0 => {}
                    1 => exclusive = exclusive.saturating_add(1),
                    _ => shared = shared.saturating_add(1),
                }
            }
        }
        (exclusive, shared)
    }
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::{
        ActiveIds, Archive, ArchiveCandidate, ArchiveKey, CellMembers, CellState,
        HISTORY_COMPACTION_MIN_DROPS, HISTORY_COMPACTION_SHARE, Input, InputIndex,
        MAINTENANCE_QUANTUM, MAX_ENTRIES_PER_KEY, MAX_TIER_RANK_SHIFT, SelectorAccounting,
        SelectorDraw, SelectorPath, TierCells, WeightedSet, checked_tier_rank_shift, tier_weight,
    };
    use crate::search::{draw::SUFFIX_DOUBLING_LIMIT, rand::RomuDuoJrRand};
    use serde::{Deserialize, Serialize};
    use std::{collections::BTreeMap, sync::Arc};

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
    struct TestAction {
        code: u8,
        length: u8,
    }

    impl TestAction {
        fn new(code: u8, length: u8) -> Self {
            Self {
                code,
                length: length.max(1),
            }
        }

        fn duration(action: &Self) -> u64 {
            u64::from(action.length)
        }
    }

    #[derive(
        Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize,
    )]
    struct TestKey {
        major: u8,
        minor: u8,
        progress: u16,
        y_bucket: u8,
        state_fingerprint: u8,
        x_bucket: u8,
        time_bucket: u8,
        region: [u8; 3],
    }

    impl ArchiveKey for TestKey {
        type Place = ([u8; 3], u16, u8, u8, u8);
        type Progress = (u8, u8);
        type Identity = u8;

        fn place(self) -> Self::Place {
            (
                self.region,
                self.progress,
                self.x_bucket,
                self.y_bucket,
                self.time_bucket,
            )
        }

        fn progress(self) -> Self::Progress {
            (self.major, self.minor)
        }

        fn identity(self) -> Self::Identity {
            self.state_fingerprint
        }

        type Lineage = Vec<[u8; 3]>;

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    type TestArchive = Archive<u8, TestKey, (), ()>;

    type TimedArchive = Archive<TestAction, TestKey, (), ()>;

    #[test]
    fn suffix_storage_is_materialized_only_for_retained_candidates() {
        struct Suffix<'a> {
            actions: &'a [u8],
            copies: &'a std::cell::Cell<usize>,
        }
        impl AsRef<[u8]> for Suffix<'_> {
            fn as_ref(&self) -> &[u8] {
                self.actions
            }
        }
        impl From<Suffix<'_>> for Vec<u8> {
            fn from(suffix: Suffix<'_>) -> Self {
                suffix.copies.set(suffix.copies.get() + 1);
                suffix.actions.to_vec()
            }
        }
        let copies = std::cell::Cell::new(0);
        let mut archive = Archive::<u8, PreferredKey, (), ()>::new(|_| 1);
        let mut insert = |actions: &[u8], quality, parent| {
            archive.insert(
                parent,
                1,
                ArchiveCandidate {
                    suffix: Suffix {
                        actions,
                        copies: &copies,
                    },
                    key: PreferredKey { slot: 0, quality },
                    milestones: (),
                },
                (),
            )
        };
        assert_eq!(insert(&[1], 10, None).unwrap(), Some(0));
        assert_eq!(copies.get(), 1);
        assert_eq!(insert(&[1], 20, None).unwrap(), Some(0));
        assert_eq!(insert(&[2; 128], 0, Some(0)).unwrap(), None);
        assert!(insert(&[3], 20, Some(99)).is_err());
        assert_eq!(copies.get(), 1);
        assert_eq!(insert(&[4, 5], 20, Some(0)).unwrap(), Some(1));
        assert_eq!(copies.get(), 2);
        assert_eq!(archive.entry_input(1).unwrap().actions, [1, 4, 5]);
        assert!(archive.entry_input(usize::MAX).is_err());
        assert_eq!(archive.entries[1].input_suffix.capacity(), 2);
    }

    #[test]
    fn borrowed_and_owned_admission_preserve_the_same_archive() {
        let mut owned = Archive::<u8, PreferredKey, u64, ()>::new(|action| u64::from(*action));
        let mut borrowed = Archive::<u8, PreferredKey, u64, ()>::new(|action| u64::from(*action));
        for step in 0..512_u64 {
            let suffix = vec![1 + (step % 17) as u8; 1 + (step % 9) as usize];
            let parent = if step % 3 == 0 || owned.entries.is_empty() {
                None
            } else {
                Some(step as usize % owned.entries.len())
            };
            let key = PreferredKey {
                slot: (step % 7) as u8,
                quality: (step % 11) as u8,
            };
            let previous = Some(PreferredKey {
                slot: 1,
                quality: 2,
            });
            let left = owned
                .insert_after(
                    parent,
                    previous,
                    step,
                    ArchiveCandidate {
                        suffix: suffix.clone(),
                        key,
                        milestones: step,
                    },
                    (),
                )
                .unwrap();
            let right = borrowed
                .insert_after(
                    parent,
                    previous,
                    step,
                    ArchiveCandidate {
                        suffix: suffix.as_slice(),
                        key,
                        milestones: step,
                    },
                    (),
                )
                .unwrap();
            assert_eq!(left, right);
            assert_eq!(
                postcard::to_stdvec(&owned).unwrap(),
                postcard::to_stdvec(&borrowed).unwrap()
            );
            assert_eq!(
                owned.resident_memory_bytes(),
                borrowed.resident_memory_bytes()
            );
        }
        assert_eq!(
            owned.take_entry_reports_and_snapshots(),
            borrowed.take_entry_reports_and_snapshots()
        );
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct ReportKey<const P: usize, const C: usize> {
        slot: u64,
        scores: [u64; 8],
    }
    impl<const P: usize, const C: usize> ArchiveKey for ReportKey<P, C> {
        type Place = u64;
        type Progress = ();
        type Identity = ();
        type Lineage = ();
        fn place(self) -> u64 {
            self.slot
        }
        fn progress(self) {}
        fn identity(self) {}
        fn preferences() -> usize {
            P
        }
        fn capacity() -> usize {
            C
        }
        fn preference_cmp(self, p: usize, other: Self) -> std::cmp::Ordering {
            self.scores[p].cmp(&other.scores[p])
        }
        fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_: &mut Self::Lineage, _: Self) {}
    }

    fn portfolio_fixture<const P: usize, const C: usize>(
        shared: bool,
    ) -> Archive<u64, ReportKey<P, C>, (), ()> {
        let mut archive = Archive::new(|_| 1);
        let mut action = 0;
        for slot in 0..128 {
            for preference in 0..if shared { 1 } else { P } {
                for rank in 0..C {
                    action += 1;
                    let mut scores = [0; 8];
                    if shared {
                        scores.fill(100 + rank as u64);
                    } else {
                        scores[preference] = 100 + rank as u64;
                    }
                    archive
                        .insert(
                            None,
                            0,
                            ArchiveCandidate {
                                suffix: vec![action],
                                key: ReportKey { slot, scores },
                                milestones: (),
                            },
                            (),
                        )
                        .unwrap();
                }
            }
        }
        archive
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn paired_portfolio<const P: usize, const C: usize>() {
        use std::{hint::black_box, time::Instant};
        for shared in [false, true] {
            let archive = portfolio_fixture::<P, C>(shared);
            assert_eq!(
                archive.portfolio_holders(P),
                archive.portfolio_holders_reference(P)
            );
            if std::env::var_os("DISSONANCE_BENCHMARK_PORTFOLIO").is_none() {
                continue;
            }
            let elapsed = |reference| {
                let start = Instant::now();
                for _ in 0..5 {
                    black_box(if reference {
                        archive.portfolio_holders_reference(P)
                    } else {
                        archive.portfolio_holders(P)
                    });
                }
                start.elapsed().as_secs_f64()
            };
            let mut ratios = Vec::new();
            for round in 0..100 {
                let flip = round % 2 == 0;
                let a = elapsed(flip);
                let b = elapsed(!flip);
                let c = elapsed(!flip);
                let d = elapsed(flip);
                ratios.push(if flip {
                    (b + c) / (a + d)
                } else {
                    (a + d) / (b + c)
                });
            }
            ratios.sort_by(f64::total_cmp);
            println!(
                "P={P} C={C} shared={shared}: ratio q25={:.4} median={:.4} q75={:.4}",
                ratios[25], ratios[50], ratios[75]
            );
        }
    }

    #[test]
    fn portfolio_classification_matches_reference_fixtures() {
        paired_portfolio::<2, 1>();
        paired_portfolio::<2, 2>();
        paired_portfolio::<4, 1>();
        paired_portfolio::<4, 2>();
        paired_portfolio::<8, 1>();
        paired_portfolio::<8, 2>();
    }

    fn compare_portfolio_classification<const P: usize, const C: usize>() {
        let mut archive = Archive::<u64, ReportKey<P, C>, (), ()>::new(|_| 1);
        let mut rand = RomuDuoJrRand::with_seed(413);
        for sequence in 0..256 {
            let mut scores = [0; 8];
            for score in &mut scores {
                *score = rand.next_u64() % 4;
            }
            archive
                .insert(
                    None,
                    sequence,
                    ArchiveCandidate {
                        suffix: vec![sequence; 1 + (sequence % 3) as usize],
                        key: ReportKey {
                            slot: sequence % 7,
                            scores,
                        },
                        milestones: (),
                    },
                    (),
                )
                .unwrap();
            for preferences in 0..=P.max(1) {
                assert_eq!(
                    archive.portfolio_holders(preferences),
                    archive.portfolio_holders_reference(preferences)
                );
            }
        }
        for slot in archive.slots.values_mut() {
            slot.reverse();
        }
        assert_eq!(
            archive.portfolio_holders(P),
            archive.portfolio_holders_reference(P)
        );
        for id in (0..archive.active.len()).step_by(3) {
            archive.active[id] = false;
        }
        for slot in archive.slots.values_mut() {
            slot.push(usize::MAX);
        }
        assert_eq!(
            archive.portfolio_holders(P),
            archive.portfolio_holders_reference(P)
        );
    }

    #[test]
    fn portfolio_classification_matches_full_counts_with_ties_and_missing_members() {
        compare_portfolio_classification::<0, 0>();
        compare_portfolio_classification::<1, 1>();
        compare_portfolio_classification::<2, 1>();
        compare_portfolio_classification::<2, 2>();
        compare_portfolio_classification::<4, 0>();
        compare_portfolio_classification::<4, 1>();
        compare_portfolio_classification::<4, 2>();
        compare_portfolio_classification::<8, 4>();
    }

    #[test]
    fn portfolio_classification_stops_comparing_once_the_result_is_known() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        static COMPARISONS: AtomicUsize = AtomicUsize::new(0);
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        struct Key(ReportKey<4, 2>);
        impl ArchiveKey for Key {
            type Place = u64;
            type Progress = ();
            type Identity = ();
            type Lineage = ();
            fn place(self) -> u64 {
                self.0.slot
            }
            fn progress(self) {}
            fn identity(self) {}
            fn capacity() -> usize {
                2
            }
            fn preferences() -> usize {
                4
            }
            fn preference_cmp(self, preference: usize, other: Self) -> std::cmp::Ordering {
                COMPARISONS.fetch_add(1, AtomicOrdering::Relaxed);
                self.0.preference_cmp(preference, other.0)
            }
            fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
                self
            }
            fn record(_: &mut Self::Lineage, _: Self) {}
        }
        for shared in [false, true] {
            let mut archive = Archive::<u64, Key, (), ()>::new(|_| 1);
            for id in 0..if shared { 2 } else { 8 } {
                let mut scores = [0; 8];
                if shared {
                    scores.fill(10);
                } else {
                    scores[id / 2] = 10;
                }
                archive
                    .insert(
                        None,
                        0,
                        ArchiveCandidate {
                            suffix: vec![id as u64],
                            key: Key(ReportKey { slot: 0, scores }),
                            milestones: (),
                        },
                        (),
                    )
                    .unwrap();
            }
            COMPARISONS.store(0, AtomicOrdering::Relaxed);
            let expected = archive.portfolio_holders_reference(4);
            let full = COMPARISONS.load(AtomicOrdering::Relaxed);
            COMPARISONS.store(0, AtomicOrdering::Relaxed);
            assert_eq!(archive.portfolio_holders(4), expected);
            let short = COMPARISONS.load(AtomicOrdering::Relaxed);
            assert!(short < full, "short={short}, full={full}");
            if shared {
                assert_eq!(short, 0);
            }
        }
    }

    #[test]
    fn admitted_suffix_keeps_its_allocation() {
        let mut archive = TestArchive::new(|_| 1);
        let suffix = vec![1, 2, 3, 4];
        let allocation = suffix.as_ptr();
        let id = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix,
                    key: TestKey::default(),
                    milestones: (),
                },
                (),
            )
            .expect("insert suffix")
            .expect("retain suffix");
        assert_eq!(archive.entries[id].input_suffix.as_ptr(), allocation);
        assert_eq!(archive.materialize_input(id).unwrap().actions, [1, 2, 3, 4]);
    }

    #[test]
    fn admitted_suffix_does_not_retain_unused_capacity() {
        let mut archive = TestArchive::new(|_| 1);
        let mut suffix = Vec::with_capacity(1024);
        suffix.extend([1, 2, 3, 4]);
        let id = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix,
                    key: TestKey::default(),
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        let stored = &archive.entries[id].input_suffix;
        assert_eq!(stored.capacity(), stored.len());
        assert_eq!(stored, &[1, 2, 3, 4]);
    }

    fn check_preference_ranking<K: ArchiveKey>(
        archive: &Archive<u8, K, (), ()>,
        slot: &[usize],
        key: K,
        cost: u64,
    ) {
        let mut members = archive.slot_members(slot);
        members.push((key, cost, archive.next_entry_id));
        let mut expected = vec![false; members.len()];
        let mut candidate = Vec::new();
        for (index, (key, cost, id)) in members.iter().enumerate() {
            for preference in 0..K::preferences().max(1) {
                let ahead = members
                    .iter()
                    .filter(|(other, other_cost, other_id)| {
                        other
                            .preference_cmp(preference, *key)
                            .then_with(|| cost.cmp(other_cost))
                            .then_with(|| id.cmp(other_id))
                            == Ordering::Greater
                    })
                    .count();
                if ahead < K::capacity().max(1) {
                    expected[index] = true;
                    if index == slot.len() {
                        candidate.push(preference);
                    }
                }
            }
        }
        assert_eq!(
            archive.rank_slot_preferences(slot, key, cost),
            (expected, candidate)
        );
    }

    #[test]
    fn preference_ranking_matches_pairwise_ranks_and_ties() {
        let mut portfolio = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        let mut flat = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        for value in 0..8_u8 {
            let suffix = vec![value; usize::from(value % 3) + 1];
            portfolio
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: suffix.clone(),
                        key: PortfolioKey {
                            slot: value,
                            first: value % 3,
                            second: (value / 3) % 3,
                        },
                        milestones: (),
                    },
                    (),
                )
                .unwrap()
                .unwrap();
            flat.insert(
                None,
                0,
                ArchiveCandidate {
                    suffix,
                    key: FlatKey([u16::from(value); 4]),
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        }
        for mask in 0..256_u16 {
            let mut slot = (0..8)
                .filter(|index| mask & (1 << index) != 0)
                .collect::<Vec<_>>();
            for _ in 0..2 {
                for first in 0..4 {
                    for second in 0..4 {
                        for cost in 0..5 {
                            check_preference_ranking(
                                &portfolio,
                                &slot,
                                PortfolioKey {
                                    slot: 0,
                                    first,
                                    second,
                                },
                                cost,
                            );
                        }
                    }
                }
                for cost in 0..5 {
                    check_preference_ranking(&flat, &slot, FlatKey([0; 4]), cost);
                }
                slot.reverse();
            }
        }
    }

    #[test]
    fn suffix_report_serialization_borrows_actions_and_milestones() {
        use super::{ArchiveEntryReport, entries_by_suffix};
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

        static CLONES: AtomicUsize = AtomicUsize::new(0);

        #[derive(Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        struct Counted(u8);

        impl Clone for Counted {
            fn clone(&self) -> Self {
                CLONES.fetch_add(1, AtomicOrdering::Relaxed);
                Self(self.0)
            }
        }

        #[derive(Deserialize, Serialize)]
        struct Reports {
            #[serde(with = "entries_by_suffix")]
            entries: Vec<ArchiveEntryReport<Counted, FlatKey, Vec<Counted>>>,
        }

        let entry = |id, parent_id, actions: &[u8]| ArchiveEntryReport {
            id,
            parent_id,
            created_execution: 7,
            input: Input {
                actions: actions.iter().copied().map(Counted).collect(),
            },
            key: FlatKey([0; 4]),
            milestones: vec![Counted(3)],
            selector: None,
        };
        let reports = Reports {
            entries: vec![
                entry(10, None, &[1, 2]),
                entry(11, Some(10), &[1, 2, 4]),
                entry(12, Some(99), &[5]),
                entry(13, Some(10), &[1, 9]),
                entry(14, Some(10), &[1, 2]),
            ],
        };
        let expected = concat!(
            r#"{"entries":["#,
            r#"{"id":10,"parent_id":null,"created_execution":7,"input":{"actions":[1,2]},"key":[0,0,0,0],"milestones":[3]},"#,
            r#"{"id":11,"parent_id":10,"created_execution":7,"input_suffix":[4],"key":[0,0,0,0],"milestones":[3]},"#,
            r#"{"id":12,"parent_id":99,"created_execution":7,"input":{"actions":[5]},"key":[0,0,0,0],"milestones":[3]},"#,
            r#"{"id":13,"parent_id":10,"created_execution":7,"input":{"actions":[1,9]},"key":[0,0,0,0],"milestones":[3]},"#,
            r#"{"id":14,"parent_id":10,"created_execution":7,"input_suffix":[],"key":[0,0,0,0],"milestones":[3]}]}"#,
        );
        CLONES.store(0, AtomicOrdering::Relaxed);
        assert_eq!(serde_json::to_string(&reports).unwrap(), expected);
        assert_eq!(CLONES.load(AtomicOrdering::Relaxed), 0);
        let restored: Reports = serde_json::from_str(expected).unwrap();
        assert_eq!(restored.entries, reports.entries);
        let empty = Reports {
            entries: Vec::new(),
        };
        assert_eq!(serde_json::to_string(&empty).unwrap(), r#"{"entries":[]}"#);
    }

    #[test]
    fn input_index_walk_owner_and_pruning_are_exact() {
        let mut index = InputIndex::<u8>::default();
        let (leaf, inserted) = index
            .ensure_path(0, &[1, 2, 3])
            .expect("insert input prefix");
        assert_eq!(inserted, 3);
        assert_eq!(index.live_nodes, 4);
        assert_eq!(index.walk(0, &[1, 2, 3]), Some(leaf));
        assert_eq!(index.walk(0, &[1, 2, 4]), None);
        assert_eq!(index.owner(leaf), None);

        index.set_owner(leaf, Some(42));
        assert_eq!(index.owner(leaf), Some(42));
        assert_eq!(index.remove_owner_and_prune(leaf, 7), 0);
        assert_eq!(index.owner(leaf), Some(42));
        assert_eq!(index.remove_owner_and_prune(leaf, 42), 3);
        assert_eq!(index.live_nodes, 1);
        assert_eq!(index.walk(0, &[1, 2, 3]), None);
    }

    #[test]
    fn snapshot_charge_uses_the_configured_machine_accounting() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        assert_eq!(archive.snapshot_charge(&()), 0);
        archive.set_memory_budget(1024, |_| 17);
        assert_eq!(archive.snapshot_charge(&()), 17);
    }

    #[test]
    fn snapshot_release_preserves_an_inflight_worker_reference() {
        let mut archive = flat_archive(&[[1, 2, 3, 4]]);
        let in_flight = Arc::clone(
            archive.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );

        assert!(archive.release_snapshot(0));
        assert!(archive.entries[0].snapshot.is_some());
        assert_eq!(Arc::strong_count(&in_flight), 2);
    }

    #[test]
    fn retired_origins_stay_charged_until_the_last_ordered_admission() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 11);
        for index in 0_u8..3 {
            archive
                .insert(
                    None,
                    index.into(),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([index.into(), index.into(), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .unwrap()
                .unwrap();
        }
        assert!(archive.pin_job_origin(99).is_err());
        let (first, replay, id) = archive.pin_job_origin(1).unwrap();
        assert!(replay.is_empty());
        let (second, _, same_id) = archive.pin_job_origin(1).unwrap();
        assert_eq!(id, same_id);
        assert_eq!(archive.resident_snapshot_bytes(), 33);
        archive.deactivate(1);
        assert!(!archive.release_snapshot(1));
        assert_eq!(archive.resident_snapshot_bytes(), 33);
        assert_eq!(archive.inflight_snapshot_bytes, 11);
        drop(first);
        drop(second);
        archive.compact_history(true).unwrap();
        assert!(archive.index_of_id(id).is_some());
        assert_eq!(archive.resident_snapshot_bytes(), 33);
        archive.unpin_job_origin(id);
        assert_eq!(archive.inflight_snapshot_bytes, 11);
        archive.unpin_job_origin(id);
        assert_eq!(archive.resident_snapshot_bytes(), 22);
        assert_eq!(archive.inflight_snapshot_bytes, 0);
        assert!(
            archive.entries[archive.index_of_id(id).unwrap()]
                .snapshot
                .is_none()
        );
        archive.unpin_job_origin(id);
        archive.unpin_job_origin(u64::MAX);
        assert_eq!(archive.resident_snapshot_bytes(), 22);
        assert!(archive.inflight_snapshot_pins.is_empty());
        assert!(archive.inflight_snapshot_charges.is_empty());
    }

    #[test]
    fn metadata_unpin_reclaims_budget_evicted_worker_snapshot() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        for index in 0_u8..2 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([u16::from(index), u16::from(index), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert budgeted entry")
                .expect("retain budgeted entry");
        }

        let stable_id = archive.stable_id(0).expect("stable id");
        archive.memory_limit = Some(archive.history_memory_bytes().saturating_add(1));
        archive
            .pin_metadata(stable_id)
            .expect("pin worker metadata");
        let worker = Arc::clone(
            archive.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget maintenance");
        assert!(!archive.snapshot_selectable[0]);
        assert!(archive.entries[0].snapshot.is_some());
        let logical_counters = (
            archive.active_count(),
            archive.resident_snapshot_count(),
            archive.resident_snapshot_bytes(),
            archive.snapshot_evictions(),
        );

        drop(worker);
        archive.unpin_metadata(stable_id);

        assert!(archive.entries[0].snapshot.is_none());
        assert_eq!(
            (
                archive.active_count(),
                archive.resident_snapshot_count(),
                archive.resident_snapshot_bytes(),
                archive.snapshot_evictions(),
            ),
            logical_counters
        );
    }

    #[test]
    fn metadata_unpin_keeps_explicitly_preserved_inactive_snapshot() {
        let mut archive = flat_archive(&[[1, 2, 3, 4]]);
        let stable_id = archive.stable_id(0).expect("stable id");
        archive
            .preserve_inactive_snapshots(true)
            .expect("preserve snapshots");
        archive
            .pin_metadata(stable_id)
            .expect("pin replay metadata");
        let worker = Arc::clone(
            archive.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );
        assert!(archive.release_snapshot(0));
        let logical_counters = (
            archive.active_count(),
            archive.resident_snapshot_count(),
            archive.resident_snapshot_bytes(),
            archive.snapshot_evictions(),
        );

        drop(worker);
        archive.unpin_metadata(stable_id);

        assert!(archive.entries[0].snapshot.is_some());
        assert_eq!(
            (
                archive.active_count(),
                archive.resident_snapshot_count(),
                archive.resident_snapshot_bytes(),
                archive.snapshot_evictions(),
            ),
            logical_counters
        );
    }
    #[test]
    fn selector_accounting_requires_the_current_cell_counter_name() {
        let current: SelectorAccounting = serde_json::from_str(
            r#"{"cell_selections":2,"productive_selections":3,"cell_resets":4,
                "tier_draws_by_rank":[5,6]}"#,
        )
        .expect("current selector accounting parses");
        assert_eq!(current.cell_selections, 2);
        assert_eq!(current.tier_draws_by_rank, vec![5, 6]);
        assert!(current.tier_runs.is_empty());
        assert!(
            serde_json::from_str::<SelectorAccounting>(
                r#"{"tie_class_selections":2,"productive_selections":3,"cell_resets":4}"#,
            )
            .is_err()
        );
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct FlatKey([u16; 4]);

    impl ArchiveKey for FlatKey {
        type Place = [u16; 3];
        type Progress = ();
        type Identity = u16;

        fn place(self) -> Self::Place {
            [self.0[1], self.0[2], self.0[3]]
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {
            self.0[0]
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct WideShiftKey(u8);

    impl ArchiveKey for WideShiftKey {
        type Place = u8;
        type Progress = u8;
        type Identity = ();

        fn place(self) -> Self::Place {
            self.0
        }

        fn progress(self) -> Self::Progress {
            self.0
        }

        fn identity(self) -> Self::Identity {}

        fn tier_rank_shift() -> u32 {
            MAX_TIER_RANK_SHIFT + 1
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    fn materialized_weighted_reference(
        rand: &mut RomuDuoJrRand,
        weights: &[u64],
    ) -> Result<usize, Box<dyn std::error::Error>> {
        let total = weights
            .iter()
            .fold(0_u64, |sum, weight| sum.saturating_add(*weight));
        let total = std::num::NonZeroUsize::new(usize::try_from(total)?)
            .ok_or("weighted draw over nothing")?;
        let mut draw = u64::try_from(rand.below(total))?;
        for (index, weight) in weights.iter().enumerate() {
            if draw < *weight {
                return Ok(index);
            }
            draw -= weight;
        }
        Err("weighted draw exceeded its total".into())
    }

    #[test]
    fn streamed_weights_preserve_draws_errors_and_rng_state() {
        let mut cases = vec![
            vec![],
            vec![0],
            vec![u64::MAX, 1],
            vec![1, u64::MAX],
            vec![u64::MAX; 4],
        ];
        for encoding in 0..256 {
            cases.push(
                (0..4)
                    .map(|offset| (encoding >> (offset * 2)) & 3)
                    .collect(),
            );
        }
        for weights in cases {
            for seed in 0..64 {
                let mut reference = RomuDuoJrRand::with_seed(seed);
                let mut actual = reference;
                for _ in 0..16 {
                    let expected = materialized_weighted_reference(&mut reference, &weights)
                        .map_err(|error| error.to_string());
                    let result = super::draw_weighted(&mut actual, weights.iter().copied())
                        .map_err(|error| error.to_string());
                    assert_eq!(result, expected);
                    assert_eq!(
                        postcard::to_stdvec(&actual).unwrap(),
                        postcard::to_stdvec(&reference).unwrap()
                    );
                }
            }
        }
    }

    fn candidate_draw_fixture(count: usize, draws: u64) -> Archive<u8, SpliceKey, (), ()> {
        let mut archive = Archive::new(|_| 1);
        archive.selected = (0..count * 3 + 1)
            .map(|id| draws.saturating_add((id % 7) as u64))
            .collect();
        let mut tier = TierCells::default();
        for index in 0..count {
            let place = u16::try_from(index * 3 + 1).unwrap();
            tier.places.insert(place, CellMembers::default());
            archive.cells.insert(
                ((0, 0), place),
                CellState {
                    draws: draws.saturating_add((index % 5) as u64),
                    ..CellState::default()
                },
            );
        }
        let members = tier.places.entry(1).or_default();
        for id in 0..count {
            members.ids.insert(id * 3 + 1, 0);
        }
        if count == 0 {
            tier.places.clear();
        }
        archive.tiers.insert((0, 0), tier);
        archive.reweight_selector_index();
        archive
    }

    #[test]
    fn indexed_candidates_preserve_draws_and_rng_state() {
        for count in [0, 1, 2, 7, 16, 256, 4096] {
            for draws in [0, 1, 65_535, u64::MAX] {
                let archive = candidate_draw_fixture(count, draws);
                for seed in 0..16 {
                    let mut actual = RomuDuoJrRand::with_seed(seed);
                    let mut reference = actual;
                    for _ in 0..32 {
                        assert_eq!(
                            archive
                                .draw_cell(&mut actual, (0, 0))
                                .map_err(|e| e.to_string()),
                            archive
                                .draw_cell_reference(&mut reference, (0, 0))
                                .map_err(|e| e.to_string())
                        );
                        assert_eq!(
                            postcard::to_stdvec(&actual).unwrap(),
                            postcard::to_stdvec(&reference).unwrap()
                        );
                        assert_eq!(
                            archive
                                .draw_holder(&mut actual, (0, 0), 1)
                                .map_err(|e| e.to_string()),
                            archive
                                .draw_holder_reference(&mut reference, (0, 0), 1)
                                .map_err(|e| e.to_string())
                        );
                        assert_eq!(
                            postcard::to_stdvec(&actual).unwrap(),
                            postcard::to_stdvec(&reference).unwrap()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn indexed_candidate_draws_preserve_missing_state_and_empty_member_errors() {
        for case in 0..4 {
            let mut archive = candidate_draw_fixture(7, 1);
            match case {
                0 => archive.cells.clear(),
                1 => archive.tiers.clear(),
                2 => archive.tiers.get_mut(&(0, 0)).unwrap().places.clear(),
                _ => {
                    archive
                        .tiers
                        .get_mut(&(0, 0))
                        .unwrap()
                        .places
                        .get_mut(&1)
                        .unwrap()
                        .ids = WeightedSet::default();
                }
            }
            archive.reweight_selector_index();
            let mut actual = RomuDuoJrRand::with_seed(17);
            let mut reference = actual;
            for _ in 0..64 {
                assert_eq!(
                    archive
                        .draw_cell(&mut actual, (0, 0))
                        .map_err(|e| e.to_string()),
                    archive
                        .draw_cell_reference(&mut reference, (0, 0))
                        .map_err(|e| e.to_string())
                );
                assert_eq!(
                    postcard::to_stdvec(&actual).unwrap(),
                    postcard::to_stdvec(&reference).unwrap()
                );
                assert_eq!(
                    archive
                        .draw_holder(&mut actual, (0, 0), 1)
                        .map_err(|e| e.to_string()),
                    archive
                        .draw_holder_reference(&mut reference, (0, 0), 1)
                        .map_err(|e| e.to_string())
                );
                assert_eq!(
                    postcard::to_stdvec(&actual).unwrap(),
                    postcard::to_stdvec(&reference).unwrap()
                );
            }
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn paired_candidate_timing(
        mut draw: impl FnMut(bool, &mut RomuDuoJrRand),
        repeats: usize,
    ) -> f64 {
        use std::time::Instant;
        let mut ratios = Vec::new();
        for round in 0..80 {
            let mut elapsed = [0_u128; 2];
            for new in if round % 2 == 0 {
                [false, true, true, false]
            } else {
                [true, false, false, true]
            } {
                let mut rand = RomuDuoJrRand::with_seed(round);
                let start = Instant::now();
                for _ in 0..repeats {
                    draw(new, &mut rand);
                }
                elapsed[usize::from(new)] += start.elapsed().as_nanos();
            }
            ratios.push(elapsed[1] as f64 / elapsed[0] as f64);
        }
        ratios.sort_by(f64::total_cmp);
        ratios[ratios.len() / 2]
    }

    #[test]
    fn indexed_candidate_draw_benchmark() {
        use std::hint::black_box;
        if std::env::var_os("DISSONANCE_BENCHMARK_CANDIDATES").is_none() {
            return;
        }
        for count in [1, 2, 16, 256, 4096] {
            for draws in [0, u64::MAX] {
                let archive = candidate_draw_fixture(count, draws);
                for holder in [false, true] {
                    let ratio = paired_candidate_timing(
                        |new, rand| {
                            if holder {
                                black_box(if new {
                                    archive.draw_holder(rand, (0, 0), 1)
                                } else {
                                    archive.draw_holder_reference(rand, (0, 0), 1)
                                })
                                .unwrap();
                            } else {
                                black_box(if new {
                                    archive.draw_cell(rand, (0, 0))
                                } else {
                                    archive.draw_cell_reference(rand, (0, 0))
                                })
                                .unwrap();
                            }
                        },
                        (16_384 / count).max(8),
                    );
                    eprintln!(
                        "candidate_draw count={count} draws={draws} holder={holder} ratio={ratio:.4}"
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_candidates_preserve_complete_parent_selection() {
        use std::hint::black_box;
        for count in [1, 16, 256, 4096] {
            let mut archive = Archive::<u64, TierKey<3>, (), ()>::new(|_| 1);
            for tier in 0..count {
                archive
                    .insert(
                        None,
                        0,
                        ArchiveCandidate {
                            suffix: vec![tier as u64],
                            key: TierKey(tier),
                            milestones: (),
                        },
                        (),
                    )
                    .unwrap();
            }
            let mut actual = RomuDuoJrRand::with_seed(419);
            let mut reference = actual;
            for _ in 0..256 {
                assert_eq!(
                    archive.select_parent(&mut actual).unwrap(),
                    archive
                        .select_parent_candidates_reference(&mut reference)
                        .unwrap()
                );
                assert_eq!(
                    postcard::to_stdvec(&actual).unwrap(),
                    postcard::to_stdvec(&reference).unwrap()
                );
            }
            if std::env::var_os("DISSONANCE_BENCHMARK_CANDIDATES").is_some() {
                let ratio = paired_candidate_timing(
                    |new, rand| {
                        black_box(if new {
                            archive.select_parent(rand)
                        } else {
                            archive.select_parent_candidates_reference(rand)
                        })
                        .unwrap();
                    },
                    1024,
                );
                eprintln!("complete_parent tiers={count} ratio={ratio:.4}");
            }
        }
    }

    #[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct TierKey<const SHIFT: u32>(usize);

    impl<const SHIFT: u32> ArchiveKey for TierKey<SHIFT> {
        type Place = ();
        type Progress = usize;
        type Identity = ();
        type Lineage = ();
        fn place(self) {}
        fn progress(self) -> usize {
            self.0
        }
        fn identity(self) {}
        fn tier_rank_shift() -> u32 {
            SHIFT
        }
        fn complete(self, _: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }
        fn record(_: &mut Self::Lineage, _: Self) {}
    }

    fn compare_streamed_tiers<const SHIFT: u32>() {
        for count in [0, 1, 2, 7, 8, 9, 16, 255, 256, 257, 4096] {
            let mut archive = Archive::<u8, TierKey<SHIFT>, (), ()>::new(|_| 1);
            for tier in 0..count {
                archive.tiers.insert(tier * 3 + 5, TierCells::default());
            }
            let tiers = archive.tiers.keys().rev().copied().collect::<Vec<_>>();
            let weights = (0..tiers.len())
                .map(|rank| tier_weight(u8::try_from(rank).unwrap_or(u8::MAX), SHIFT))
                .collect::<Vec<_>>();
            for seed in 0..32 {
                let mut reference = RomuDuoJrRand::with_seed(seed);
                let mut actual = reference;
                for _ in 0..64 {
                    let expected = materialized_weighted_reference(&mut reference, &weights)
                        .map(|index| (tiers[index], u8::try_from(index).unwrap_or(u8::MAX)))
                        .map_err(|error| error.to_string());
                    assert_eq!(
                        archive
                            .draw_tier(&mut actual)
                            .map_err(|error| error.to_string()),
                        expected
                    );
                    assert_eq!(
                        postcard::to_stdvec(&actual).unwrap(),
                        postcard::to_stdvec(&reference).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn streamed_tiers_preserve_materialized_selection_and_rng_state() {
        compare_streamed_tiers::<0>();
        compare_streamed_tiers::<1>();
        compare_streamed_tiers::<2>();
        compare_streamed_tiers::<3>();
        compare_streamed_tiers::<4>();
        compare_streamed_tiers::<5>();
        compare_streamed_tiers::<6>();
        compare_streamed_tiers::<7>();
    }

    #[test]
    fn tier_weights_keep_their_values_and_an_overflowing_shift_is_rejected() {
        assert_eq!(MAX_TIER_RANK_SHIFT, 7);
        assert_eq!(
            (0..=9).map(|rank| tier_weight(rank, 1)).collect::<Vec<_>>(),
            vec![256, 128, 64, 32, 16, 8, 4, 2, 1, 1]
        );
        assert_eq!(
            (0..=9).map(|rank| tier_weight(rank, 3)).collect::<Vec<_>>(),
            [24, 21, 18, 15, 12, 9, 6, 3, 0, 0].map(|exponent| 1_u64 << exponent)
        );
        assert_eq!(tier_weight(0, MAX_TIER_RANK_SHIFT), 1 << 56);
        assert_eq!(checked_tier_rank_shift(MAX_TIER_RANK_SHIFT).ok(), Some(7));

        let mut archive = Archive::<u8, WideShiftKey, (), ()>::new(|_| 1);
        for place in [1, 2] {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![place],
                        key: WideShiftKey(place),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert a wide-shift entry")
                .expect("retain a wide-shift entry");
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5417_0008);
        let error = archive
            .select_parent(&mut rand)
            .expect_err("a shift above the largest supported one is rejected")
            .to_string();
        assert_eq!(
            error,
            "tier rank shift 8 exceeds the largest supported shift 7"
        );
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct PreferredKey {
        slot: u8,
        quality: u8,
    }

    impl ArchiveKey for PreferredKey {
        type Place = u8;
        type Progress = ();
        type Identity = ();

        fn place(self) -> Self::Place {
            self.slot
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {}

        fn capacity() -> usize {
            1
        }

        fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
            self.quality.cmp(&other.quality)
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }
    #[test]
    fn opaque_preference_displaces_only_a_weaker_same_slot_representative() {
        let mut archive = Archive::<u8, PreferredKey, (), ()>::new(|_| 1);
        let insert = |archive: &mut Archive<u8, PreferredKey, (), ()>, input, quality| {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![input],
                        key: PreferredKey { slot: 7, quality },
                        milestones: (),
                    },
                    (),
                )
                .expect("insert preferred entry")
        };

        assert_eq!(insert(&mut archive, 1, 2), Some(0));
        assert_eq!(insert(&mut archive, 2, 5), Some(1));
        assert_eq!(archive.active, vec![false, true]);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![1]));
        assert_eq!(insert(&mut archive, 3, 1), None);
        assert_eq!(archive.active, vec![false, true]);
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct PortfolioKey {
        slot: u8,
        first: u8,
        second: u8,
    }

    impl ArchiveKey for PortfolioKey {
        type Place = u8;
        type Progress = ();
        type Identity = ();

        fn place(self) -> Self::Place {
            self.slot
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {}

        fn capacity() -> usize {
            1
        }

        fn preferences() -> usize {
            2
        }

        fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
            match preference {
                0 => (self.first, self.second).cmp(&(other.first, other.second)),
                _ => (self.second, self.first).cmp(&(other.second, other.first)),
            }
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    thread_local! {
        static SECOND_ORDER_FLIPPED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct SwitchedOrderKey {
        first: u8,
        second: u8,
    }

    impl ArchiveKey for SwitchedOrderKey {
        type Place = u8;
        type Progress = ();
        type Identity = ();

        fn place(self) -> Self::Place {
            7
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {}

        fn capacity() -> usize {
            1
        }

        fn preferences() -> usize {
            2
        }

        fn preference_cmp(self, preference: usize, other: Self) -> Ordering {
            if preference == 0 || SECOND_ORDER_FLIPPED.get() {
                (self.first, self.second).cmp(&(other.first, other.second))
            } else {
                (self.second, self.first).cmp(&(other.second, other.first))
            }
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    #[test]
    fn resuming_continuations_follows_the_key_preference_count() {
        let mut portfolio = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        assert!(portfolio.continuations.is_none());
        portfolio.resume_continuations(4);
        assert!(portfolio.continuations.is_some());
        let mut plain = Archive::<u8, TestKey, (), ()>::new(|_| 1);
        plain.continuations = Some(crate::search::continuation::ContinuationBank::new(4));
        plain.resume_continuations(4);
        assert!(plain.continuations.is_none());
    }

    #[test]
    fn a_carried_in_arrival_that_takes_one_preference_counts_as_a_win() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        let mut insert = |input, parent, slot, first, second| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        suffix: vec![input],
                        key: PortfolioKey {
                            slot,
                            first,
                            second,
                        },
                        milestones: (),
                    },
                    (),
                )
                .expect("insert portfolio entry")
        };
        let parent = insert(1, None, 1, 1, 1);
        assert_eq!(insert(2, None, 2, 10, 2), Some(1));
        assert_eq!(insert(3, parent, 2, 5, 8), Some(2));
        assert_eq!(archive.slots.get(&(((), 2), ())), Some(&vec![1, 2]));
        let runs = archive.tier_runs(());
        assert_eq!((runs.yields, runs.wins), (3, 1));
    }

    #[test]
    fn reranking_slot_holders_retires_holders_that_win_no_preference() {
        SECOND_ORDER_FLIPPED.set(false);
        let mut archive = Archive::<u8, SwitchedOrderKey, (), ()>::new(|_| 1);
        for (input, first, second) in [(1, 10, 20), (2, 5, 200)] {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![input],
                        key: SwitchedOrderKey { first, second },
                        milestones: (),
                    },
                    (),
                )
                .expect("insert holder");
        }
        assert_eq!(archive.rerank_slot_holders(), 0);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![0, 1]));
        SECOND_ORDER_FLIPPED.set(true);
        assert_eq!(archive.rerank_slot_holders(), 1);
        SECOND_ORDER_FLIPPED.set(false);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![0]));
        assert_eq!(archive.active, vec![true, false]);
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct RegionKey {
        region: u8,
        spot: u8,
        first: u8,
    }

    impl ArchiveKey for RegionKey {
        type Place = u8;
        type Progress = ();
        type Identity = u8;

        fn place(self) -> Self::Place {
            self.region
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {
            self.spot
        }

        fn capacity() -> usize {
            1
        }

        fn preferences() -> usize {
            1
        }

        fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
            self.first.cmp(&other.first)
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    fn insert_region_at(
        archive: &mut Archive<u8, RegionKey, (), ()>,
        parent: Option<usize>,
        input: u8,
        (region, spot, first): (u8, u8, u8),
    ) -> usize {
        archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    suffix: vec![input],
                    key: RegionKey {
                        region,
                        spot,
                        first,
                    },
                    milestones: (),
                },
                (),
            )
            .expect("insert region entry")
            .expect("the region entry is kept")
    }

    #[test]
    fn a_move_inside_one_region_records_an_edge_and_an_improvement_there_queues_it() {
        let mut archive = Archive::<u8, RegionKey, (), ()>::new(|_| 1);
        archive.enable_continuations(4);
        let left = insert_region_at(&mut archive, None, 1, (1, 0, 5));
        insert_region_at(&mut archive, Some(left), 2, (1, 3, 5));
        assert_eq!(archive.continuation_pending(), 0);
        let richer = insert_region_at(&mut archive, None, 3, (1, 0, 9));
        let taken = archive
            .pop_continuation(false)
            .expect("the improved position is queued");
        assert_eq!((taken.source, taken.destination), ((1, 0), (1, 3)));
        assert_eq!(archive.index_of_id(taken.parent), Some(richer));
        assert!(archive.outranks_slot_holders(richer, taken.destination, taken.preference));
    }

    #[test]
    fn a_new_position_inside_an_open_cell_opens_a_slot_and_no_cell() {
        let mut archive = Archive::<u8, RegionKey, (), ()>::new(|_| 1);
        let origin = insert_region_at(&mut archive, None, 1, (1, 0, 5));
        assert!(archive.opened_new_slot(origin) && archive.opened_new_cell(origin));
        let beside = insert_region_at(&mut archive, Some(origin), 2, (1, 4, 5));
        assert!(archive.opened_new_slot(beside));
        assert!(!archive.opened_new_cell(beside));
        let richer = insert_region_at(&mut archive, Some(origin), 3, (1, 4, 9));
        assert!(!archive.opened_new_slot(richer));
    }

    #[test]
    fn an_edge_inside_a_region_lands_on_its_position_and_one_across_lands_anywhere_there() {
        let mut archive = Archive::<u8, RegionKey, (), ()>::new(|_| 1);
        let origin = insert_region_at(&mut archive, None, 1, (1, 0, 5));
        let beside = insert_region_at(&mut archive, Some(origin), 2, (1, 4, 5));
        let across = insert_region_at(&mut archive, Some(origin), 3, (2, 6, 5));
        assert!(archive.lands_on_edge(beside, (1, 0), (1, 4)));
        assert!(!archive.lands_on_edge(beside, (1, 0), (1, 3)));
        assert!(archive.lands_on_edge(across, (1, 0), (2, 1)));
        assert!(!archive.lands_on_edge(beside, (1, 0), (2, 1)));
    }

    fn insert_portfolio(
        archive: &mut Archive<u8, PortfolioKey, (), ()>,
        input: u8,
        first: u8,
        second: u8,
    ) -> Option<usize> {
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![input],
                    key: PortfolioKey {
                        slot: 7,
                        first,
                        second,
                    },
                    milestones: (),
                },
                (),
            )
            .expect("insert portfolio entry")
    }

    fn insert_portfolio_at(
        archive: &mut Archive<u8, PortfolioKey, (), ()>,
        parent: Option<usize>,
        suffix: Vec<u8>,
        slot: u8,
        first: u8,
        second: u8,
    ) -> Option<usize> {
        archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    suffix,
                    key: PortfolioKey {
                        slot,
                        first,
                        second,
                    },
                    milestones: (),
                },
                (),
            )
            .expect("insert portfolio entry")
    }

    fn portfolio_bank() -> Archive<u8, PortfolioKey, (), ()> {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        archive.enable_continuations(4);
        archive
    }

    #[test]
    fn a_cheaper_arrival_at_equal_preference_queues_no_slot() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2, 2], 2, 10, 20).expect("exit");
        assert_eq!(archive.continuation_pending(), 0);
        insert_portfolio_at(&mut archive, Some(origin), vec![3], 2, 10, 20).expect("cheaper");
        assert_eq!(archive.continuation_pending(), 0);
    }

    #[test]
    fn a_strictly_preferred_arrival_queues_its_slot_once() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 10, 20).expect("exit");
        insert_portfolio_at(&mut archive, Some(origin), vec![3], 1, 12, 30).expect("preferred");
        assert_eq!(archive.continuation_pending(), 1);
        insert_portfolio_at(&mut archive, Some(origin), vec![4], 1, 14, 40).expect("both again");
        assert_eq!(archive.continuation_pending(), 1);
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct ResourceKey {
        place: [u16; 3],
        amount: u8,
    }

    impl ArchiveKey for ResourceKey {
        type Place = [u16; 3];
        type Progress = ();
        type Identity = ();

        fn place(self) -> Self::Place {
            self.place
        }

        fn progress(self) -> Self::Progress {}

        fn identity(self) -> Self::Identity {}

        fn capacity() -> usize {
            1
        }

        fn preferences() -> usize {
            1
        }

        fn preference_cmp(self, _preference: usize, other: Self) -> Ordering {
            self.amount.cmp(&other.amount)
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    fn insert_resource(
        archive: &mut Archive<u8, ResourceKey, (), ()>,
        parent: Option<usize>,
        input: u8,
        place: [u16; 3],
        amount: u8,
    ) -> Option<usize> {
        archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    suffix: vec![input],
                    key: ResourceKey { place, amount },
                    milestones: (),
                },
                (),
            )
            .expect("insert resource entry")
    }
    #[test]
    fn recent_cells_are_bounded_and_old_cells_remain_selectable() {
        let mut archive = Archive::<u16, ResourceKey, (), ()>::new(|_| 1);
        for place in 0..300 {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![place],
                        key: ResourceKey {
                            place: [place, 0, 0],
                            amount: 5,
                        },
                        milestones: (),
                    },
                    (),
                )
                .unwrap()
                .unwrap();
        }
        let mut rand = RomuDuoJrRand::with_seed(731);
        let mut recent = 0;
        let mut old = 0;
        for _ in 0..4096 {
            let (id, draw) = archive.select_parent(&mut rand).unwrap();
            if draw.path == SelectorPath::Recent {
                assert!(id >= 44);
                assert!(archive.cell_draws(archive.entries[id].key) < 32);
                recent += 1;
            }
            old += usize::from(id < 44);
            archive.record_selection(id, &draw);
        }
        assert!(recent > 1000);
        assert!(old > 100);
        assert_eq!(archive.selector_report().recent_selections, Some(recent));
        for state in archive.cells.values_mut() {
            state.draws = 32;
        }
        archive.selector_weighted = false;
        for _ in 0..128 {
            let (_, draw) = archive.select_parent(&mut rand).unwrap();
            assert_eq!(draw.path, SelectorPath::Tiers);
        }
    }

    #[test]
    fn recent_cells_deduplicate_holders_and_filter_the_drawn_tier() {
        let mut single = TestArchive::new(|_| 1);
        let mut repeated = TestArchive::new(|_| 1);
        for archive in [&mut single, &mut repeated] {
            for progress in [1, 2] {
                archive
                    .insert(
                        None,
                        0,
                        ArchiveCandidate {
                            suffix: vec![progress as u8],
                            key: TestKey {
                                major: 1,
                                progress,
                                ..TestKey::default()
                            },
                            milestones: (),
                        },
                        (),
                    )
                    .unwrap()
                    .unwrap();
            }
        }
        for key in [
            TestKey {
                major: 1,
                progress: 1,
                state_fingerprint: 1,
                ..TestKey::default()
            },
            TestKey {
                major: 1,
                progress: 1,
                state_fingerprint: 2,
                ..TestKey::default()
            },
            TestKey {
                major: 2,
                progress: 3,
                ..TestKey::default()
            },
        ] {
            repeated
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![3, key.major, key.state_fingerprint],
                        key,
                        milestones: (),
                    },
                    (),
                )
                .unwrap()
                .unwrap();
        }
        single.rebuild_selector_index();
        repeated.rebuild_selector_index();
        assert_eq!(single.active_ids.ids.len(), 2);
        assert_eq!(repeated.active_ids.ids.len(), 5);
        let mut single_rand = RomuDuoJrRand::with_seed(733);
        let mut repeated_rand = single_rand;
        let mut recent = 0;
        for _ in 0..4096 {
            let expected = single.draw_recent_cell(&mut single_rand, (1, 0)).unwrap();
            let actual = repeated
                .draw_recent_cell(&mut repeated_rand, (1, 0))
                .unwrap();
            assert_eq!(actual, expected);
            recent += usize::from(actual.is_some());
        }
        assert!(recent > 1000);
        assert_eq!(
            postcard::to_stdvec(&single_rand).unwrap(),
            postcard::to_stdvec(&repeated_rand).unwrap()
        );
    }

    #[test]
    fn recent_selection_reconstructs_from_serialized_archive_state() {
        let mut archive = Archive::<u16, ResourceKey, (), ()>::new(|_| 1);
        for place in 0..300 {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![place],
                        key: ResourceKey {
                            place: [place, 0, 0],
                            amount: 5,
                        },
                        milestones: (),
                    },
                    (),
                )
                .unwrap()
                .unwrap();
        }
        let mut rand = RomuDuoJrRand::with_seed(732);
        for _ in 0..100 {
            let (id, draw) = archive.select_parent(&mut rand).unwrap();
            archive.record_selection(id, &draw);
        }
        let bytes = postcard::to_stdvec(&archive).unwrap();
        let mut restored: Archive<u16, ResourceKey, (), ()> = postcard::from_bytes(&bytes).unwrap();
        let mut restored_rand = rand;
        for _ in 0..256 {
            let a = archive.select_parent(&mut rand).unwrap();
            let b = restored.select_parent(&mut restored_rand).unwrap();
            assert_eq!(a, b);
            archive.record_selection(a.0, &a.1);
            restored.record_selection(b.0, &b.1);
        }
        assert_eq!(archive.selector_counters(), restored.selector_counters());
        assert_eq!(
            postcard::to_stdvec(&rand).unwrap(),
            postcard::to_stdvec(&restored_rand).unwrap()
        );
    }

    #[test]
    fn a_richer_arrival_from_another_cell_resets_its_cells_draw_count() {
        let mut archive = Archive::<u8, ResourceKey, (), ()>::new(|_| 1);
        archive.rebuild_selector_index();
        let first = insert_resource(&mut archive, None, 1, [1, 1, 1], 5).expect("first");
        assert!(archive.opened_new_cell(first));
        let beside = insert_resource(&mut archive, Some(first), 2, [2, 1, 1], 5).expect("beside");
        assert!(archive.opened_new_cell(beside));
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..5 {
            archive.record_selection(first, &draw);
        }
        assert_eq!(archive.cell_draws(archive.entries[first].key), 5);
        assert!(insert_resource(&mut archive, Some(first), 3, [1, 1, 1], 5).is_none());
        assert_eq!(archive.cell_draws(archive.entries[first].key), 5);
        let repeated =
            insert_resource(&mut archive, Some(first), 4, [1, 1, 1], 6).expect("repeated");
        assert!(!archive.opened_new_cell(repeated));
        assert_eq!(archive.cell_draws(archive.entries[repeated].key), 5);
        assert_eq!(archive.selector_report().cell_resets, 0);
        let better = insert_resource(&mut archive, Some(beside), 5, [1, 1, 1], 7).expect("better");
        assert!(!archive.opened_new_cell(better));
        assert_eq!(archive.cell_draws(archive.entries[better].key), 0);
        assert_eq!(archive.selector_report().cell_resets, 1);
        assert_eq!(archive.occupied_cell_count(), 2);
    }
    #[test]
    fn opening_a_new_cell_resets_the_parents_cell_draw_count() {
        let mut archive = Archive::<u8, ResourceKey, (), ()>::new(|_| 1);
        archive.rebuild_selector_index();
        let first = insert_resource(&mut archive, None, 1, [1, 1, 1], 5).expect("first");
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..7 {
            archive.record_selection(first, &draw);
        }
        assert_eq!(archive.cell_draws(archive.entries[first].key), 7);
        let same_cell =
            insert_resource(&mut archive, Some(first), 2, [1, 1, 1], 6).expect("better");
        assert!(!archive.opened_new_cell(same_cell));
        assert_eq!(archive.cell_draws(archive.entries[first].key), 7);
        let opened = insert_resource(&mut archive, Some(first), 3, [2, 1, 1], 5).expect("opened");
        assert!(archive.opened_new_cell(opened));
        assert_eq!(archive.cell_draws(archive.entries[first].key), 0);
        assert_eq!(archive.cell_draws(archive.entries[opened].key), 0);
        assert_eq!(archive.selector_report().cell_resets, 1);
    }
    #[test]
    fn a_slot_with_no_exits_is_never_queued() {
        let mut archive = portfolio_bank();
        insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, None, vec![2], 1, 12, 30).expect("preferred");
        assert_eq!(archive.continuation_pending(), 0);
    }

    #[test]
    fn a_key_declaring_no_preference_holds_no_bank() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.enable_continuations(4);
        assert_eq!(archive.continuation_pending(), 0);
        assert_eq!(archive.continuation_report().edges, 0);
        assert!(archive.continuations.is_none());
    }

    #[test]
    fn a_slot_keeps_the_champion_of_every_preference() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        assert_eq!(insert_portfolio(&mut archive, 1, 10, 20), Some(0));
        assert_eq!(insert_portfolio(&mut archive, 2, 5, 200), Some(1));
        assert_eq!(archive.active, vec![true, true]);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![0, 1]));
    }

    #[test]
    fn a_candidate_losing_every_preference_is_rejected() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert_eq!(insert_portfolio(&mut archive, 3, 4, 19), None);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![0, 1]));
    }

    #[test]
    fn one_entry_winning_both_preferences_is_stored_once() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert_eq!(insert_portfolio(&mut archive, 3, 60, 240), Some(2));
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![2]));
        assert_eq!(archive.active, vec![false, false, true]);
    }

    #[test]
    fn a_candidate_taking_one_preference_leaves_the_other_champion() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert_eq!(insert_portfolio(&mut archive, 3, 11, 21), Some(2));
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![1, 2]));
        assert_eq!(archive.active, vec![false, true, true]);
    }

    #[test]
    fn each_preference_champion_is_drawn() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert_eq!(archive.slots.get(&(((), 7), ())), Some(&vec![0, 1]));
        let mut rand = RomuDuoJrRand::with_seed(0x51de_5eed);
        let mut drawn = [0_u32; 2];
        for _ in 0..512 {
            let (id, _) = archive
                .select_parent(&mut rand)
                .expect("draw a portfolio parent");
            drawn[id] += 1;
        }
        assert!(drawn[0] > 0, "the first-resource champion was never drawn");
        assert!(drawn[1] > 0, "the second-resource champion was never drawn");
    }

    #[test]
    fn a_champion_of_one_preference_is_not_a_champion_of_the_other() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert!(archive.is_preference_champion(0, 0));
        assert!(!archive.is_preference_champion(0, 1));
        assert!(!archive.is_preference_champion(1, 0));
        assert!(archive.is_preference_champion(1, 1));
    }

    #[test]
    fn a_sole_holder_is_the_champion_of_every_preference() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        assert!(archive.is_preference_champion(0, 0));
        assert!(archive.is_preference_champion(0, 1));
    }
    #[test]
    fn the_portfolio_report_counts_holders() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        let mut rand = RomuDuoJrRand::with_seed(0x7ea1_f0f0);
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand)
                .expect("draw a portfolio parent");
            archive.record_selection(id, &draw);
        }
        let report = archive.selector_report();
        let portfolio = report.portfolio.expect("portfolio accounting is reported");
        assert_eq!(portfolio.preferences, 2);
        assert_eq!(portfolio.exclusive_holders, 2);
        assert_eq!(portfolio.shared_holders, 0);
        assert_eq!(report.cell_selections, 64);
    }
    #[test]
    fn a_quarter_of_holder_draws_go_to_the_best_holder_of_each_preference() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        let first = insert_portfolio(&mut archive, 1, 10, 20).expect("first-resource holder");
        let second = insert_portfolio(&mut archive, 2, 5, 200).expect("second-resource holder");
        assert_eq!(archive.selector_report().best_holder_draws, vec![0, 0]);
        let mut rand = RomuDuoJrRand::with_seed(0xbe57_0004);
        for _ in 0..8_000 {
            let (id, draw) = archive.select_parent(&mut rand).expect("draw a parent");
            match draw.best_preference {
                Some(0) => assert_eq!(id, first),
                Some(1) => assert_eq!(id, second),
                Some(other) => panic!("draw named preference {other}"),
                None => {}
            }
            archive.record_selection(id, &draw);
        }
        let draws = archive.selector_report().best_holder_draws;
        assert_eq!(draws.len(), 2);
        for count in draws {
            assert!(
                (800..1_200).contains(&count),
                "{count} best-holder draws of 8,000"
            );
        }
    }
    #[test]
    fn recent_draws_preserve_best_holder_preference_and_accounting() {
        let mut recent_best = [0_u64; 2];
        for seed in 0..64 {
            let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
            let first = insert_portfolio(&mut archive, 1, 10, 20).expect("first-resource holder");
            let second = insert_portfolio(&mut archive, 2, 5, 200).expect("second-resource holder");
            let mut expected = [0_u64; 2];
            let mut rand = RomuDuoJrRand::with_seed(0xbe57_1000 + seed);
            for _ in 0..32 {
                let (id, draw) = archive.select_parent(&mut rand).expect("draw a parent");
                if let Some(preference) = draw.best_preference {
                    let preference = usize::from(preference);
                    assert_eq!(id, [first, second][preference]);
                    expected[preference] += 1;
                    if draw.path == SelectorPath::Recent {
                        recent_best[preference] += 1;
                    }
                }
                archive.record_selection(id, &draw);
            }
            assert_eq!(archive.selector_report().best_holder_draws, expected);
        }
        assert!(recent_best.iter().all(|count| *count > 0));
    }

    #[test]
    fn taking_one_preference_from_a_surviving_holder_queues_its_exits() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        archive.enable_continuations(4);
        let holder = insert_portfolio_at(&mut archive, None, vec![1], 7, 10, 20)
            .expect("the first holder is kept");
        insert_portfolio_at(&mut archive, Some(holder), vec![2], 8, 10, 20)
            .expect("an exit is recorded");
        assert!(
            archive.pop_continuation(false).is_none(),
            "recording an exit queues nothing on its own"
        );
        insert_portfolio_at(&mut archive, None, vec![3], 7, 5, 200)
            .expect("the second-resource candidate is admitted beside the first-resource holder");
        assert_eq!(archive.slots.get(&(((), 7), ())).map(Vec::len), Some(2));
        assert!(
            archive.pop_continuation(false).is_some(),
            "improving one preference queues the slot's exits"
        );
    }

    #[test]
    fn a_queued_source_reaches_a_neighbour_only_by_beating_the_holders_of_its_slot() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 20, 20)
            .expect("the neighbour slot holds a state with more of the resource");
        let weaker = insert_portfolio_at(&mut archive, None, vec![3], 1, 12, 30)
            .expect("a candidate that takes a preference in its own slot");
        assert_eq!(archive.continuation_pending(), 1);
        let taken = archive.pop_continuation(false).expect("the slot is queued");
        assert_eq!((taken.destination, taken.preference), ((2, ()), 0));
        assert!(!archive.outranks_slot_holders(weaker, taken.destination, taken.preference));
        let stronger = insert_portfolio_at(&mut archive, None, vec![4], 1, 30, 40)
            .expect("a candidate that also beats the neighbour");
        assert!(archive.outranks_slot_holders(stronger, taken.destination, taken.preference));
    }

    #[test]
    fn the_portfolio_report_survives_draining_the_entries() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        let before = archive.selector_report();
        let (entries, _) = archive.take_entry_reports_and_snapshots();
        assert_eq!(entries.len(), 2);
        let portfolio = before.portfolio.expect("portfolio accounting is reported");
        assert_eq!(portfolio.exclusive_holders, 2);
        let after = archive.selector_report();
        assert_eq!(
            after
                .portfolio
                .expect("portfolio accounting is still reported")
                .exclusive_holders,
            0
        );
    }

    #[test]
    fn compaction_keeps_each_entry_with_its_replacement_preferences() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 1, 1);
        let replacing = insert_portfolio(&mut archive, 2, 9, 9).expect("the replacement is kept");
        assert_ne!(archive.replacement_preferences(replacing), 0);
        let marked = archive.replacement_preferences(replacing);
        let before = archive.entries.len();
        archive
            .compact_history_for_final_report()
            .expect("compaction succeeds");
        assert!(
            archive.entries.len() < before,
            "compaction dropped an entry"
        );
        let surviving = archive
            .entries
            .iter()
            .position(|entry| entry.key.first == 9)
            .expect("the replacement survives compaction");
        assert_eq!(archive.replacement_preferences(surviving), marked);
        assert_eq!(archive.replacement_preferences.len(), archive.entries.len());
    }

    #[test]
    fn a_parent_doubles_its_suffix_after_each_job_that_stays_in_its_place() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        let parent = insert_portfolio(&mut archive, 1, 1, 1).expect("the parent is kept");
        assert_eq!(archive.suffix_limit(parent), 1);
        archive.record_suffix_outcome(parent, 1, false, false, false);
        assert_eq!(archive.suffix_limit(parent), 2);
        archive.record_suffix_outcome(parent, 2, false, false, false);
        assert_eq!(archive.suffix_limit(parent), 4);
        archive.record_suffix_outcome(parent, 1, false, false, true);
        assert_eq!(archive.suffix_limit(parent), 4);
        archive.record_suffix_outcome(parent, 48, false, false, false);
        assert_eq!(archive.suffix_limit(parent), SUFFIX_DOUBLING_LIMIT);
        archive.record_suffix_outcome(parent, 64, false, true, false);
        assert_eq!(archive.suffix_limit(parent), 1);
        archive.record_suffix_outcome(parent, 3, false, false, false);
        assert_eq!(archive.suffix_limit(parent), 6);
        archive.record_suffix_outcome(parent, 1, false, false, false);
        assert_eq!(archive.suffix_limit(parent), 6);
        archive.record_suffix_outcome(parent, 6, true, false, false);
        assert_eq!(archive.suffix_limit(parent), 1);
        let replacing = insert_portfolio(&mut archive, 2, 9, 9).expect("the replacement is kept");
        archive.record_suffix_outcome(replacing, 5, false, false, false);
        let before = archive.entries.len();
        archive
            .compact_history_for_final_report()
            .expect("compaction succeeds");
        assert!(
            archive.entries.len() < before,
            "compaction dropped an entry"
        );
        assert_eq!(archive.in_place_lengths.len(), archive.entries.len());
        let surviving = archive
            .entries
            .iter()
            .position(|entry| entry.key.first == 9)
            .expect("the replacement survives compaction");
        assert_eq!(archive.suffix_limit(surviving), 10);
    }

    #[test]
    fn one_preference_reports_no_portfolio() {
        let mut archive = Archive::<u8, PreferredKey, (), ()>::new(|_| 1);
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: PreferredKey {
                        slot: 7,
                        quality: 2,
                    },
                    milestones: (),
                },
                (),
            )
            .expect("insert preferred entry");
        assert!(archive.selector_report().portfolio.is_none());
    }

    #[test]
    fn a_replacement_records_the_preference_it_won_under() {
        let mut archive = Archive::<u8, PortfolioKey, (), ()>::new(|_| 1);
        insert_portfolio(&mut archive, 1, 10, 20);
        insert_portfolio(&mut archive, 2, 5, 200);
        assert_eq!(archive.replacement_preferences(0), 0);
        assert_eq!(archive.replacement_preferences(1), 0);
        let took_first =
            insert_portfolio(&mut archive, 3, 11, 21).expect("first-resource champion");
        assert_eq!(archive.replacement_preferences(took_first), 0b01);
        let took_both = insert_portfolio(&mut archive, 4, 60, 240).expect("both champions");
        assert_eq!(archive.replacement_preferences(took_both), 0b11);
    }

    fn flat_archive(keys: &[[u16; 4]]) -> Archive<u8, FlatKey, (), ()> {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        for (index, components) in keys.iter().enumerate() {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![u8::try_from(index).expect("input byte")],
                        key: FlatKey(*components),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert flat entry")
                .expect("retain flat entry");
        }
        archive
    }

    #[test]
    fn memory_budget_drops_an_entry_without_freezing_admission() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        let root_charge = Archive::<u8, FlatKey, (), ()>::prefix_node_memory_charge();
        let history_charge = 3 * Archive::<u8, FlatKey, (), ()>::history_entry_memory_charge(1, 1);
        let cell_charge = 3 * Archive::<u8, FlatKey, (), ()>::cell_memory_charge();
        archive.set_memory_budget(root_charge + history_charge + cell_charge + 2, |_| 1);
        for index in 0_u8..3 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([u16::from(index), u16::from(index), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert budgeted entry")
                .expect("budget admits a replacement");
        }

        assert_eq!(archive.active_count(), 2);
        assert_eq!(archive.resident_snapshot_count(), 2);
        assert_eq!(archive.resident_snapshot_bytes(), 2);
        assert_eq!(archive.snapshot_evictions(), 0);
        assert_eq!(archive.entry_drops(), 1);
        assert_eq!(
            archive.history_memory_bytes(),
            archive
                .entry_metadata_memory_bytes()
                .saturating_add(archive.input_index_memory_bytes())
                .saturating_add(archive.cell_memory_bytes())
        );
        assert!(!archive.active[0]);
        assert!(archive.entries[0].snapshot.is_none());
        assert!(
            archive.slots.values().flatten().all(|entry| *entry != 0),
            "the evicted entry must leave its retention slot"
        );
        assert!(archive.entries[1].snapshot.is_some());
        assert!(archive.entries[2].snapshot.is_some());

        let mut rand = RomuDuoJrRand::with_seed(1);
        for _ in 0..32 {
            let (selected, _) = archive
                .select_parent(&mut rand)
                .expect("select from budgeted residents");
            assert_ne!(selected, 0);
        }
    }

    #[test]
    fn budget_enforcement_reaches_the_limit_but_keeps_one_snapshot() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        for index in 0_u8..4 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([u16::from(index), u16::from(index), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }

        let exact = archive.resident_memory_bytes();
        archive.memory_limit = Some(exact);
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(archive.resident_snapshot_count(), 4);

        archive.memory_limit = Some(archive.history_memory_bytes().saturating_add(2));
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(archive.resident_snapshot_count(), 2);
        assert_eq!(archive.active_count(), 2);
        assert_eq!(archive.snapshot_evictions(), 0);
        assert_eq!(archive.entry_drops(), 2);

        archive.memory_limit = Some(0);
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(archive.resident_snapshot_count(), 1);
        assert_eq!(archive.active_count(), 1);
        assert_eq!(archive.entry_drops(), 3);
    }

    #[test]
    fn a_budget_below_the_anchor_itself_is_an_error() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        for index in 0_u8..4 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([u16::from(index), u16::from(index), u16::from(index), 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }
        archive.establish_liveness_anchor();
        let anchor_bytes = archive.liveness_anchor_memory_bytes();
        assert!(anchor_bytes > 0, "the anchor holds an irreducible charge");

        archive.memory_limit = Some(anchor_bytes - 1);
        assert_eq!(
            archive.maintain_memory_budget(),
            Err("memory budget cannot retain the executable liveness anchor")
        );
    }

    #[test]
    fn the_final_compaction_refuses_a_budget_the_compacted_archive_exceeds() {
        let anchored = || {
            let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
            archive.set_memory_budget(usize::MAX, |_| 1);
            for index in 0_u8..4 {
                archive
                    .insert(
                        None,
                        u64::from(index),
                        ArchiveCandidate {
                            suffix: vec![index],
                            key: FlatKey([u16::from(index), u16::from(index), u16::from(index), 0]),
                            milestones: (),
                        },
                        (),
                    )
                    .expect("insert entry")
                    .expect("retain entry");
            }
            archive.establish_liveness_anchor();
            archive
        };

        let sweep = |archive: &mut Archive<u8, FlatKey, (), ()>| {
            for _ in 0..64 {
                archive
                    .maintain_memory_budget()
                    .expect("live maintenance cannot rule this budget out");
            }
        };
        let mut floor = anchored();
        let lower_bound = floor.liveness_anchor_memory_bytes();
        floor.memory_limit = Some(lower_bound);
        sweep(&mut floor);
        floor.memory_limit = None;
        floor
            .compact_history_for_final_report()
            .expect("an unbudgeted archive compacts");
        let floor_bytes = floor.resident_memory_bytes();
        assert!(
            lower_bound < floor_bytes,
            "the anchor bound {lower_bound} should undercount the compacted charge {floor_bytes}"
        );

        let mut archive = anchored();
        archive.memory_limit = Some(lower_bound);
        sweep(&mut archive);
        assert_eq!(
            archive.compact_history_for_final_report(),
            Err("memory budget cannot retain the compacted archive")
        );

        let mut affordable = anchored();
        affordable.memory_limit = Some(floor_bytes);
        sweep(&mut affordable);
        affordable
            .compact_history_for_final_report()
            .expect("a budget that holds the compacted archive is satisfiable");
    }

    #[test]
    fn the_final_compaction_releases_an_inactive_liveness_anchor() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1 << 20);
        for index in 0_u8..2 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: vec![index],
                        key: FlatKey([u16::from(index), u16::from(index), u16::from(index), 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }
        archive.establish_liveness_anchor();
        let anchor = archive
            .liveness_anchor
            .expect("an active entry anchors the run");
        let anchor_index = archive.index_of_id(anchor).expect("anchor is archived");
        archive.deactivate(anchor_index);
        assert_eq!(archive.active_count, 1);
        assert_eq!(
            archive.resident_snapshots, 2,
            "the inactive anchor keeps its snapshot"
        );

        archive.memory_limit = Some(archive.resident_memory_bytes() - (1 << 19));
        archive
            .compact_history_for_final_report()
            .expect("the final report does not need the inactive anchor");
        assert_eq!(archive.liveness_anchor, None);
        assert_eq!(archive.entries.len(), 1);
        assert_eq!(archive.resident_snapshots, 1);
        assert!(archive.index_of_id(anchor).is_none());
    }

    #[test]
    fn the_final_compaction_keeps_passing_while_a_quantum_reclaims_no_bytes() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 0);
        archive.set_memory_budget(usize::MAX, |_| 0);
        let entries = MAINTENANCE_QUANTUM * 4;
        for index in 0..entries {
            let key = u16::try_from(index).expect("key fits u16");
            archive
                .insert(
                    None,
                    index as u64,
                    ArchiveCandidate {
                        suffix: vec![u8::try_from(index % 251).expect("suffix byte")],
                        key: FlatKey([key, key, key, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }
        archive.referenced.fill(true);
        archive.drop_hand = 0;
        let limit = archive.resident_memory_bytes() / 4;
        archive.memory_limit = Some(limit);

        let charged_before = archive.resident_memory_bytes();
        archive
            .compact_history(true)
            .expect("one forced pass is not a verdict");
        assert_eq!(
            archive.resident_memory_bytes(),
            charged_before,
            "the first pass spends its quantum on reference bits and frees nothing"
        );

        archive
            .compact_history_for_final_report()
            .expect("later passes reclaim enough to fit the budget");
        assert!(
            archive.resident_memory_bytes() <= limit,
            "the compacted archive charges {} against a {limit} byte budget",
            archive.resident_memory_bytes()
        );
    }

    #[test]
    fn the_maintenance_quantum_counts_entries_the_clock_examines() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        let entries = MAINTENANCE_QUANTUM * 4;
        for index in 0..entries {
            let key = u16::try_from(index).expect("key fits u16");
            archive
                .insert(
                    None,
                    index as u64,
                    ArchiveCandidate {
                        suffix: vec![u8::try_from(index % 251).expect("suffix byte"), key as u8],
                        key: FlatKey([key, key, key, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }
        archive.referenced.fill(true);
        archive.drop_hand = 0;
        archive.memory_limit = Some(0);
        archive
            .maintain_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(
            archive.entry_drops(),
            0,
            "referenced entries are not droppable"
        );
        assert!(
            archive.drop_hand <= MAINTENANCE_QUANTUM,
            "the sweep examined {} entries for a {MAINTENANCE_QUANTUM} entry quantum",
            archive.drop_hand
        );

        archive.referenced.fill(true);
        archive.drop_hand = 0;
        let (dropped, examined) = archive.drop_one_entry_within(4);
        assert!(!dropped);
        assert_eq!(examined, 4);
        assert_eq!(archive.drop_hand, 4);
    }

    #[test]
    fn budget_enforcement_evicts_cached_snapshots_before_dropping_entries() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![0],
                    key: FlatKey([0, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert root")
            .expect("retain root");
        for step in 1_u8..4 {
            archive
                .insert(
                    Some(usize::from(step - 1)),
                    u64::from(step),
                    ArchiveCandidate {
                        suffix: vec![step],
                        key: FlatKey([u16::from(step), u16::from(step), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert child")
                .expect("retain child");
        }
        assert_eq!(archive.resident_snapshot_count(), 4);
        assert!(archive.keyframe[0]);
        assert!(archive.keyframe[1..4].iter().all(|keyframe| !keyframe));
        assert_eq!(archive.keyframe_dependents[0], 4);

        archive.memory_limit = Some(archive.history_memory_bytes().saturating_add(1));
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(archive.active_count(), 4);
        assert_eq!(archive.resident_snapshot_count(), 1);
        assert_eq!(archive.snapshot_evictions(), 3);
        assert_eq!(archive.entry_drops(), 0);
        assert!(archive.entries[0].snapshot.is_some());

        let (pinned, path, origin_id) = archive.pin_job_origin(3).unwrap();
        assert_eq!(origin_id, archive.stable_id(0).unwrap());
        assert_eq!(path, vec![1, 2, 3]);
        assert_eq!(archive.inflight_snapshot_pins.get(&origin_id), Some(&1));
        drop(pinned);
        archive.unpin_job_origin(origin_id);
        assert!(archive.inflight_snapshot_pins.is_empty());

        let (origin, replay) = archive.job_origin(3).expect("replay from the keyframe");
        assert!(Arc::ptr_eq(
            &origin,
            archive.entries[0]
                .snapshot
                .as_ref()
                .expect("keyframe snapshot")
        ));
        assert_eq!(replay, vec![1, 2, 3]);
        drop(origin);
        let (_, replay) = archive.job_origin(0).expect("keyframe is its own origin");
        assert!(replay.is_empty());

        let mut rand = RomuDuoJrRand::with_seed(1);
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..64 {
            let (selected, _) = archive
                .select_parent(&mut rand)
                .expect("select from replayable entries");
            seen.insert(selected);
        }
        assert_eq!(seen.len(), 4, "entries without a snapshot stay selectable");
        let (held, _, keyframe_id) = archive.pin_job_origin(3).unwrap();
        archive.release_snapshot(0);
        drop(held);
        assert_eq!(archive.resident_snapshot_bytes(), 1);
        archive.unpin_job_origin(archive.stable_id(3).unwrap());
        assert!(archive.entries[0].snapshot.is_some());
        archive.unpin_job_origin(keyframe_id);
        assert!(archive.entries[0].snapshot.is_none());
        assert_eq!(archive.resident_snapshot_bytes(), 0);
    }

    #[test]
    fn entry_drop_sweep_clears_reference_bits_first_and_reports_exhaustion() {
        let mut empty = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        assert!(!empty.drop_one_entry());

        let mut archive = flat_archive(&[[0, 0, 0, 0], [1, 1, 0, 0]]);
        assert!(archive.deactivate(0));
        assert!(archive.drop_one_entry());
        assert_eq!(archive.active_count(), 0);
        assert_eq!(archive.resident_snapshot_count(), 0);
        assert_eq!(archive.entry_drops(), 1);
        assert!(!archive.drop_one_entry());
    }

    #[test]
    fn selection_preparation_reactivates_a_budgeted_executable_anchor() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: FlatKey([0, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert root")
            .expect("retain root");
        archive.rebuild_selector_index();
        archive.establish_liveness_anchor();
        archive.cost_in_group[0] = 100;
        for (id, suffix) in [(1_u64, 1_u8), (2, 2)] {
            archive
                .insert(
                    None,
                    id,
                    ArchiveCandidate {
                        suffix: vec![suffix],
                        key: FlatKey([0, 0, 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert same-slot entry")
                .expect("retain same-slot entry");
        }
        assert!(!archive.active[0]);
        assert!(archive.entries[0].snapshot.is_some());
        archive.active_ids = ActiveIds::default();

        let mut rand = RomuDuoJrRand::with_seed(0x5eed_cafe);
        assert!(archive.select_parent(&mut rand).is_err());
        assert_eq!(archive.liveness_anchor_reactivations(), 0);
        archive.prepare_selection();
        assert_eq!(archive.liveness_anchor_reactivations(), 1);
        archive
            .select_parent(&mut rand)
            .expect("budgeted anchor keeps selection live");
        assert!(archive.active[0]);
        assert!(archive.active_ids().contains(&0));
        assert!(
            archive
                .slots
                .values()
                .all(|members| members.len() <= MAX_ENTRIES_PER_KEY)
        );
    }

    #[test]
    fn budgeted_anchor_yields_to_single_entry_admission_and_reactivates() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        archive.set_memory_budget(usize::MAX, |_| 1);
        archive.max_entries = 1;
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: FlatKey([0, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert root")
            .expect("retain root");
        archive.rebuild_selector_index();
        archive.establish_liveness_anchor();
        archive
            .insert(
                None,
                1,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: FlatKey([1, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("anchor yields to one-entry admission")
            .expect("retain admitted entry");
        assert_eq!(archive.active_count(), 1);
        assert!(!archive.active[0]);
        assert!(archive.entries[0].snapshot.is_some());
        assert!(
            archive
                .slots
                .values()
                .all(|members| members.len() <= MAX_ENTRIES_PER_KEY)
        );
        archive.active_ids = ActiveIds::default();
        archive.prepare_selection();
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_cafe);
        let (selected, _) = archive
            .select_parent(&mut rand)
            .expect("displaced anchor reactivates");
        assert_eq!(archive.entries[selected].id, 0);
        assert_eq!(archive.active_count(), 1);
    }

    fn archive_with_prunable_history() -> Archive<u8, FlatKey, (), ()> {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        for index in 0_u16
            ..=u16::try_from(HISTORY_COMPACTION_MIN_DROPS)
                .expect("compaction threshold fits in u16")
        {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: index.to_be_bytes().to_vec(),
                        key: FlatKey([index, index, 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert history entry")
                .expect("retain history entry");
        }
        for index in 0..HISTORY_COMPACTION_MIN_DROPS {
            assert!(archive.deactivate(index));
        }
        archive
    }

    #[test]
    fn the_final_census_offers_active_endpoints_with_their_selection_counts() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        for index in 0_u16..3 {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: index.to_be_bytes().to_vec(),
                        key: FlatKey([index, index, 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry");
        }
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..4 {
            archive.record_selection(1, &draw);
        }
        assert!(archive.deactivate(2));
        let census: Vec<_> = archive
            .retained_snapshots()
            .map(|(snapshot, selections)| (snapshot.is_some(), selections))
            .collect();
        assert_eq!(census, vec![(true, 0), (true, 4)]);
    }

    fn indexed_landings_fixture(entries: usize, stride: usize, stale: bool) -> TestArchive {
        let mut archive = TestArchive::new(|_| 1);
        for index in 0..entries {
            let id = u64::try_from(index).unwrap() * 3 + 1;
            archive.entries.push(super::ArchiveEntry {
                id,
                parent_id: None,
                created_execution: 0,
                input_suffix: Vec::new(),
                input_len: 0,
                input_node: 0,
                key: TestKey::default(),
                milestones: (),
                snapshot: None,
            });
            archive.id_to_index.insert(id, index);
            if stride != 0 && index % stride == 0 {
                archive.landed.insert(id);
                if stale {
                    archive.landed.insert(id + 1);
                }
            }
        }
        archive
    }

    #[test]
    fn indexed_landings_match_rebuilt_live_ids() {
        for entries in [0, 1, 12, 128, 4096] {
            for stride in [0, 1, 7, 64] {
                for stale in [false, true] {
                    let mut actual = indexed_landings_fixture(entries, stride, stale);
                    actual.landed.insert(u64::MAX);
                    let bytes = postcard::to_stdvec(&actual).unwrap();
                    let mut reference: TestArchive = postcard::from_bytes(&bytes).unwrap();
                    actual.retain_indexed_landings();
                    reference.retain_indexed_landings_reference();
                    assert_eq!(
                        postcard::to_stdvec(&actual).unwrap(),
                        postcard::to_stdvec(&reference).unwrap()
                    );
                }
            }
        }
    }

    #[test]
    fn indexed_landings_match_irregular_and_extreme_ids() {
        let mut rand = RomuDuoJrRand::with_seed(427);
        for count in 0..128 {
            let mut actual = indexed_landings_fixture(count, 0, false);
            actual.id_to_index.clear();
            for (index, entry) in actual.entries.iter_mut().enumerate() {
                entry.id = match index % 17 {
                    0 => 0,
                    1 => u64::MAX,
                    _ => rand.next_u64() % 1024,
                };
                actual.id_to_index.insert(entry.id, index);
            }
            actual.landed.extend([0, u64::MAX]);
            for _ in 0..count * 4 {
                actual.landed.insert(rand.next_u64() % 1024);
            }
            let bytes = postcard::to_stdvec(&actual).unwrap();
            let mut reference: TestArchive = postcard::from_bytes(&bytes).unwrap();
            actual.retain_indexed_landings();
            reference.retain_indexed_landings_reference();
            assert_eq!(
                postcard::to_stdvec(&actual).unwrap(),
                postcard::to_stdvec(&reference).unwrap()
            );
        }
    }

    #[test]
    fn compaction_keeps_landings_for_retained_metadata() {
        let mut archive = archive_with_prunable_history();
        let pinned = archive.entries[HISTORY_COMPACTION_MIN_DROPS / 2].id;
        archive.pin_metadata(pinned).unwrap();
        archive.landed = archive.entries.iter().map(|entry| entry.id).collect();
        archive.landed.insert(u64::MAX);
        archive.compact_history(true).unwrap();
        let expected = archive
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<std::collections::BTreeSet<_>>();
        assert!(expected.contains(&pinned));
        assert!(expected.len() < HISTORY_COMPACTION_MIN_DROPS);
        assert_eq!(archive.landed, expected);
        archive.unpin_metadata(pinned);
        archive.compact_history(true).unwrap();
        assert!(!archive.landed.contains(&pinned));
        assert_eq!(
            archive.landed,
            archive.entries.iter().map(|entry| entry.id).collect()
        );
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn paired_landings_cleanup(archive: &mut TestArchive) -> f64 {
        use std::{hint::black_box, time::Instant};
        let original = archive.landed.clone();
        let mut ratios = Vec::new();
        for round in 0..80 {
            let mut elapsed = [0_u128; 2];
            for new in if round % 2 == 0 {
                [false, true, true, false]
            } else {
                [true, false, false, true]
            } {
                archive.landed = original.clone();
                let started = Instant::now();
                if new {
                    archive.retain_indexed_landings();
                } else {
                    archive.retain_indexed_landings_reference();
                }
                black_box(&archive.landed);
                elapsed[usize::from(new)] += started.elapsed().as_nanos();
            }
            ratios.push(elapsed[1] as f64 / elapsed[0] as f64);
        }
        ratios.sort_by(f64::total_cmp);
        ratios[ratios.len() / 2]
    }

    #[test]
    fn indexed_landings_paired_benchmark() {
        if std::env::var_os("DISSONANCE_BENCHMARK_LANDINGS_CLEANUP").is_none() {
            return;
        }
        for entries in [128, 4096, 65_536] {
            for stride in [0, 1, 64] {
                for stale in [false, true] {
                    if stride == 0 && stale {
                        continue;
                    }
                    let mut archive = indexed_landings_fixture(entries, stride, stale);
                    let landed = archive.landed.len();
                    let ratio = paired_landings_cleanup(&mut archive);
                    eprintln!(
                        "landings cleanup entries={entries} landed={landed} stride={stride} stale={stale}: new/old={ratio:.3}"
                    );
                }
            }
        }
        for entries in [32, 128, 512, 4096] {
            let mut archive = indexed_landings_fixture(entries, 0, false);
            archive.id_to_index.clear();
            for (index, entry) in archive.entries.iter_mut().enumerate() {
                entry.id *= 64;
                archive.id_to_index.insert(entry.id, index);
            }
            archive.landed = (0..u64::try_from(entries).unwrap() * 192).collect();
            let landed = archive.landed.len();
            let ratio = paired_landings_cleanup(&mut archive);
            eprintln!(
                "landings cleanup heavily pruned entries={entries} landed={landed}: new/old={ratio:.3}"
            );
        }
    }

    #[test]
    fn history_compaction_obeys_the_quarter_budget_threshold() {
        let mut archive = archive_with_prunable_history();
        let history = archive.history_memory_bytes();
        archive.memory_limit = Some(history.saturating_mul(4));
        archive
            .compact_history_if_needed()
            .expect("history at threshold does not compact");
        assert_eq!(archive.history_compactions(), 0);

        archive.memory_limit = Some(history.saturating_mul(2));
        archive
            .compact_history_if_needed()
            .expect("history above threshold compacts");
        assert_eq!(archive.history_compactions(), 1);
        assert_eq!(
            archive.historical_entries_dropped(),
            u64::try_from(HISTORY_COMPACTION_MIN_DROPS).expect("threshold fits in u64")
        );
    }
    #[test]
    fn continuation_attempts_do_not_count_as_cell_draws() {
        let mut archive = archive_with_prunable_history();
        let id = archive.active.iter().position(|active| *active).unwrap();
        let key = archive.entries[id].key;
        for _ in 0..20 {
            archive.record_isolated_continuation(id);
        }
        assert_eq!(archive.selected[id], 0);
        assert_eq!(archive.cell_draws(key), 0);
        assert_eq!(archive.selector_report().continuation_selections, Some(20));
        archive.record_selection(
            id,
            &SelectorDraw {
                path: SelectorPath::Tiers,
                tier_rank: Some(0),
                best_preference: None,
            },
        );
        assert_eq!(archive.selected[id], 1);
        assert_eq!(archive.cell_draws(key), 1);
    }

    #[test]
    fn a_cells_draw_count_survives_metadata_compaction_and_rebirth() {
        let mut archive = archive_with_prunable_history();
        let old = archive.entries[0].key;
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..19 {
            archive.record_selection(0, &draw);
        }
        assert_eq!(archive.cell_draws(old), 19);
        archive.compact_history_for_final_report().unwrap();
        assert!(archive.entries.iter().all(|entry| entry.key != old));
        assert_eq!(archive.cell_draws(old), 0);
        let new = archive
            .insert(
                None,
                100,
                ArchiveCandidate {
                    suffix: vec![255, 255, 255],
                    key: old,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        assert_eq!(archive.selected[new], 0);
        assert!(archive.opened_new_cell(new));
        archive.record_selection(new, &draw);
        assert_eq!(archive.cell_draws(old), 1);
    }
    #[test]
    fn a_failed_compaction_skips_scans_until_enough_keeps_are_released() {
        let mut archive = archive_with_prunable_history();
        archive.keep_releases = usize::MAX - 1;
        let history = archive.history_memory_bytes();
        archive.memory_limit = Some(history.saturating_mul(2));
        let ids = archive.entries[..HISTORY_COMPACTION_MIN_DROPS]
            .iter()
            .map(|entry| entry.id)
            .collect::<Vec<_>>();
        for id in &ids {
            archive.pin_metadata(*id).unwrap();
        }
        archive.compact_history_if_needed().unwrap();
        let failed = archive.failed_compaction;
        assert_eq!(failed.map(|(_, dropped)| dropped), Some(0));
        for id in &ids[1..] {
            archive.unpin_metadata(*id);
            archive.compact_history_if_needed().unwrap();
            assert_eq!(archive.failed_compaction, failed);
        }
        assert_eq!(archive.history_compactions(), 0);
        archive.unpin_metadata(ids[0]);
        archive.compact_history_if_needed().unwrap();
        assert_eq!(archive.history_compactions(), 1);
        assert_eq!(archive.failed_compaction, None);
        assert_eq!(
            archive.historical_entries_dropped(),
            u64::try_from(HISTORY_COMPACTION_MIN_DROPS).unwrap()
        );
    }

    #[test]
    fn entry_pressure_triggers_history_compaction() {
        let mut archive = archive_with_prunable_history();
        archive.max_entries = 0;
        archive.memory_limit = Some(usize::MAX);
        archive
            .compact_history_if_needed()
            .expect("entry pressure compacts history");
        assert_eq!(archive.history_compactions(), 1);
        assert_eq!(
            archive.historical_entries_dropped(),
            u64::try_from(HISTORY_COMPACTION_MIN_DROPS).expect("threshold fits in u64")
        );
    }

    #[test]
    fn entry_pressure_compaction_waits_for_a_share_of_the_archive() {
        let entries = HISTORY_COMPACTION_MIN_DROPS * HISTORY_COMPACTION_SHARE * 2;
        let min_drops = entries / HISTORY_COMPACTION_SHARE;
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        for index in 0..u16::try_from(entries).expect("entry count fits in u16") {
            archive
                .insert(
                    None,
                    u64::from(index),
                    ArchiveCandidate {
                        suffix: index.to_be_bytes().to_vec(),
                        key: FlatKey([index, index, 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert history entry")
                .expect("retain history entry");
        }
        archive.max_entries = 0;
        archive.memory_limit = Some(usize::MAX);
        for index in 0..min_drops - 1 {
            assert!(archive.deactivate(index));
        }
        archive
            .compact_history_if_needed()
            .expect("a dead tail below the share is left for a later batch");
        assert_eq!(archive.history_compactions(), 0);
        assert!(archive.deactivate(min_drops - 1));
        archive
            .compact_history_if_needed()
            .expect("a dead tail at the share compacts");
        assert_eq!(archive.history_compactions(), 1);
        assert_eq!(
            archive.historical_entries_dropped(),
            u64::try_from(min_drops).expect("drop count fits in u64")
        );
    }

    #[test]
    fn history_compaction_waits_for_the_minimum_drop_batch() {
        let mut archive = flat_archive(&[[0, 0, 0, 0], [1, 1, 0, 0]]);
        assert!(archive.deactivate(0));
        archive.memory_limit = Some(1);
        archive
            .compact_history_if_needed()
            .expect("a sub-threshold dead tail is left for a later batch");
        assert_eq!(archive.history_compactions(), 0);
        assert_eq!(archive.live_entry_count(), 2);
    }

    #[test]
    fn final_compaction_keeps_only_valid_inactive_snapshot_owners() {
        let mut held = flat_archive(&[[0, 0, 0, 0]]);
        let held_id = held.stable_id(0).expect("stable id");
        let worker = Arc::clone(
            held.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );
        assert!(held.deactivate(0));
        held.compact_history(true)
            .expect("worker-owned snapshot survives final compaction");
        assert!(held.index_of_id(held_id).is_some());
        assert_eq!(Arc::strong_count(&worker), 2);

        let mut preserved = flat_archive(&[[0, 0, 0, 0]]);
        let preserved_id = preserved.stable_id(0).expect("stable id");
        preserved
            .preserve_inactive_snapshots(true)
            .expect("enable inactive snapshot preservation");
        assert!(preserved.deactivate(0));
        preserved
            .compact_history(true)
            .expect("explicitly preserved snapshot survives final compaction");
        assert!(preserved.index_of_id(preserved_id).is_some());

        let mut empty = flat_archive(&[[0, 0, 0, 0]]);
        assert!(empty.deactivate(0));
        empty.preserve_inactive_snapshots = true;
        empty
            .compact_history(true)
            .expect("a preservation flag without a snapshot retains nothing");
        assert_eq!(empty.live_entry_count(), 0);
    }

    #[test]
    fn inactive_snapshot_and_metadata_preservation_are_reversible() {
        let mut archive = flat_archive(&[[0, 0, 0, 0]]);
        let stable_id = archive.stable_id(0).expect("stable id");
        assert!(!archive.preserves_inactive_snapshots());
        archive
            .preserve_inactive_snapshots(true)
            .expect("enable inactive snapshot preservation");
        assert!(archive.preserves_inactive_snapshots());
        assert!(archive.deactivate(0));
        assert!(archive.entries[0].snapshot.is_some());
        archive
            .preserve_inactive_snapshots(false)
            .expect("disable inactive snapshot preservation");
        assert!(!archive.preserves_inactive_snapshots());
        assert!(archive.entries[0].snapshot.is_none());

        archive.preserve_recorded_metadata_uses(BTreeMap::from([(stable_id, 2)]));
        assert_eq!(archive.metadata_pins.get(&stable_id), Some(&2));
        archive.unpin_metadata(stable_id);
        assert_eq!(archive.metadata_pins.get(&stable_id), Some(&1));
        archive.unpin_metadata(stable_id);
        assert!(!archive.metadata_pins.contains_key(&stable_id));
    }

    #[test]
    fn compact_prefix_accounting_and_duplicate_lookup_are_exact() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: FlatKey([1, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert root")
            .expect("retain root");
        let child = archive
            .insert(
                Some(root),
                1,
                ArchiveCandidate {
                    suffix: vec![2],
                    key: FlatKey([2, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert child")
            .expect("retain child");
        let leaf = archive
            .insert(
                Some(child),
                2,
                ArchiveCandidate {
                    suffix: vec![3],
                    key: FlatKey([3, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert leaf")
            .expect("retain leaf");

        assert_eq!(archive.existing_input_id(Some(child), &[3]), Some(leaf));
        assert!(archive.all_extensions_retained(root, &[2, 3]));
        assert!(!archive.all_extensions_retained(root, &[2, 4]));
        assert_eq!(archive.live_entry_count(), 3);
        assert_eq!(archive.historical_input_actions(), 6);
        assert_eq!(archive.stored_input_actions(), 3);
        assert_eq!(archive.input_index_nodes(), 4);
        assert_eq!(archive.input_reconstructions(), 0);
        assert_eq!(
            archive
                .materialize_input(leaf)
                .expect("materialize leaf")
                .actions,
            [1, 2, 3]
        );
        assert_eq!(archive.input_reconstructions(), 1);
        assert!(Archive::<u8, FlatKey, (), ()>::prefix_node_memory_charge() > 1);
        assert!(Archive::<u8, FlatKey, (), ()>::cell_memory_charge() > 1);
        assert!(archive.cell_memory_bytes() > 1);

        let retained = archive.retained;
        assert_eq!(
            archive
                .insert(
                    Some(child),
                    3,
                    ArchiveCandidate {
                        suffix: vec![3],
                        key: FlatKey([9, 9, 9, 9]),
                        milestones: (),
                    },
                    (),
                )
                .expect("duplicate lookup"),
            Some(leaf)
        );
        assert_eq!(archive.retained, retained);
    }

    #[test]
    fn progress_and_expandable_ids_follow_exact_archive_state() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|action| u64::from(*action));
        for (index, (suffix, key)) in [
            (5, FlatKey([1, 0, 0, 0])),
            (2, FlatKey([0, 0, 0, 0])),
            (7, FlatKey([2, 0, 0, 0])),
            (9, FlatKey([2, 0, 0, 0])),
        ]
        .into_iter()
        .enumerate()
        {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![suffix],
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("insert progress entry")
                .expect("retain progress entry");
            let cheapest = if index == 0 { 5 } else { 2 };
            assert_eq!(
                archive.live_progress(),
                Some((
                    FlatKey([1, 0, 0, 0]),
                    cheapest,
                    u64::try_from(index + 1).unwrap()
                ))
            );
        }
        assert_eq!(archive.live_progress(), Some((FlatKey([1, 0, 0, 0]), 2, 4)));
        assert_eq!(archive.active_ids(), vec![0, 1, 2, 3]);
        archive.active[1] = false;
        archive.snapshot_selectable[2] = false;
        assert_eq!(archive.active_ids(), vec![0, 3]);
    }
    #[test]
    fn deepest_leaf_tracks_a_new_deepest_descendant() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        let root_key = FlatKey([1, 7, 9, 0]);
        let child_key = FlatKey([2, 7, 9, 0]);
        let root = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: root_key,
                    milestones: (),
                },
                (),
            )
            .expect("insert root")
            .expect("retain root");
        archive.rebuild_selector_index();
        let child = archive
            .insert(
                Some(root),
                1,
                ArchiveCandidate {
                    suffix: vec![2],
                    key: child_key,
                    milestones: (),
                },
                (),
            )
            .expect("insert child")
            .expect("retain child");

        assert_eq!(archive.deepest_leaf[root], (child_key, child));
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct LineageKey {
        value: u8,
        class: u8,
    }

    impl ArchiveKey for LineageKey {
        type Place = u8;
        type Progress = u8;
        type Identity = ();

        fn place(self) -> Self::Place {
            self.value
        }

        fn progress(self) -> Self::Progress {
            self.class
        }

        fn identity(self) -> Self::Identity {}

        type Lineage = Vec<u8>;

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(lineage: &mut Self::Lineage, key: Self) {
            lineage.push(key.value);
        }
    }
    #[test]
    fn rejected_candidates_do_not_clone_or_record_lineage() {
        use std::{cell::Cell, rc::Rc};

        #[derive(Default, Deserialize, Serialize)]
        struct Lineage {
            values: Vec<u8>,
            #[serde(skip)]
            clones: Rc<Cell<usize>>,
            #[serde(skip)]
            records: Rc<Cell<usize>>,
        }
        impl Clone for Lineage {
            fn clone(&self) -> Self {
                self.clones.set(self.clones.get() + 1);
                Self {
                    values: self.values.clone(),
                    clones: Rc::clone(&self.clones),
                    records: Rc::clone(&self.records),
                }
            }
        }
        #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
        struct Key(u8);
        impl ArchiveKey for Key {
            type Place = ();
            type Progress = ();
            type Identity = ();
            type Lineage = Lineage;
            fn place(self) {}
            fn progress(self) {}
            fn identity(self) {}
            fn capacity() -> usize {
                1
            }
            fn preference_cmp(self, _: usize, other: Self) -> Ordering {
                self.0.cmp(&other.0)
            }
            fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self {
                Self(
                    self.0.saturating_add(
                        parent
                            .and_then(|(_, lineage)| lineage.values.last().copied())
                            .unwrap_or(0),
                    ),
                )
            }
            fn record(lineage: &mut Self::Lineage, key: Self) {
                lineage.records.set(lineage.records.get() + 1);
                lineage.values.push(key.0);
            }
        }
        let mut archive = Archive::<u8, Key, (), ()>::new(|_| 1);
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: Key(10),
                    milestones: (),
                },
                (),
            )
            .unwrap();
        let clones = Rc::clone(&archive.lineages[0].clones);
        let records = Rc::clone(&archive.lineages[0].records);
        for execution in 1..=32 {
            assert_eq!(
                archive
                    .insert_after(
                        Some(0),
                        Some(Key(99)),
                        execution,
                        ArchiveCandidate {
                            suffix: [2].as_slice(),
                            key: Key(0),
                            milestones: (),
                        },
                        ()
                    )
                    .unwrap(),
                (None, Key(10))
            );
        }
        assert_eq!(clones.get(), 0);
        assert_eq!(records.get(), 1);
        assert_eq!(archive.lineages[0].values, [10]);
        assert_eq!(
            archive
                .insert_after(
                    Some(0),
                    None,
                    33,
                    ArchiveCandidate {
                        suffix: [3].as_slice(),
                        key: Key(20),
                        milestones: (),
                    },
                    ()
                )
                .unwrap(),
            (Some(1), Key(30))
        );
        assert_eq!(clones.get(), 1);
        assert_eq!(records.get(), 2);
        assert_eq!(archive.lineages[1].values, [10, 30]);
    }

    #[test]
    fn lineage_is_inherited_across_classes() {
        let mut archive = Archive::<u8, LineageKey, (), ()>::new(|_| 1);
        let parent = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![1],
                    key: LineageKey { value: 1, class: 7 },
                    milestones: (),
                },
                (),
            )
            .expect("insert parent")
            .expect("retain parent");
        let same = archive
            .insert(
                Some(parent),
                1,
                ArchiveCandidate {
                    suffix: vec![2],
                    key: LineageKey { value: 2, class: 7 },
                    milestones: (),
                },
                (),
            )
            .expect("insert same-class child")
            .expect("retain same-class child");
        let different = archive
            .insert(
                Some(parent),
                2,
                ArchiveCandidate {
                    suffix: vec![3],
                    key: LineageKey { value: 3, class: 8 },
                    milestones: (),
                },
                (),
            )
            .expect("insert cross-class child")
            .expect("retain cross-class child");

        assert_eq!(archive.lineage(parent), Some(&vec![1]));
        assert_eq!(archive.lineage(same), Some(&vec![1, 2]));
        assert_eq!(archive.lineage(different), Some(&vec![1, 3]));
    }

    #[test]
    fn compaction_preserves_an_evicted_inflight_parents_input_prefix() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        let parent = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![0xff, 0xfe, 0xfd],
                    key: FlatKey([0, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("insert parent")
            .expect("retain parent");
        let parent_id = archive.stable_id(parent).expect("parent stable id");
        archive
            .pin_metadata(parent_id)
            .expect("pin parent metadata");

        for index in 0_u16
            ..u16::try_from(HISTORY_COMPACTION_MIN_DROPS).expect("compaction threshold fits in u16")
        {
            archive
                .insert(
                    Some(parent),
                    u64::from(index).saturating_add(1),
                    ArchiveCandidate {
                        suffix: index.to_be_bytes().to_vec(),
                        key: FlatKey([index.saturating_add(1), index.saturating_add(1), 0, 0]),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert displaced descendant")
                .expect("retain displaced descendant");
        }
        let survivor = archive
            .insert(
                None,
                u64::try_from(HISTORY_COMPACTION_MIN_DROPS)
                    .expect("compaction threshold fits in u64")
                    .saturating_add(2),
                ArchiveCandidate {
                    suffix: vec![0xfe, 0xfd, 0xfc],
                    key: FlatKey([
                        u16::try_from(HISTORY_COMPACTION_MIN_DROPS)
                            .expect("compaction threshold fits in u16")
                            .saturating_add(2),
                        u16::try_from(HISTORY_COMPACTION_MIN_DROPS)
                            .expect("compaction threshold fits in u16")
                            .saturating_add(2),
                        0,
                        0,
                    ]),
                    milestones: (),
                },
                (),
            )
            .expect("insert independent survivor")
            .expect("retain independent survivor");
        let survivor_key = archive.entries[survivor].key;
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..3 {
            archive.record_selection(survivor, &draw);
        }
        assert!(archive.historical_cell_count() > 1);

        for index in 0..survivor {
            assert!(archive.deactivate(index));
        }
        archive.memory_limit = Some(4);
        archive
            .compact_history_if_needed()
            .expect("compact pinned input history");

        let parent = archive
            .index_of_id(parent_id)
            .expect("pinned parent remains");
        assert_eq!(archive.historical_cell_count(), 1);
        assert_eq!(
            archive.cell_draws(survivor_key),
            3,
            "compaction preserves live cell draw counts"
        );
        assert_eq!(
            archive
                .materialize_input(parent)
                .expect("materialize pinned parent")
                .actions,
            [0xff, 0xfe, 0xfd]
        );
        archive
            .insert(
                Some(parent),
                4_099,
                ArchiveCandidate {
                    suffix: vec![9, 9, 9],
                    key: FlatKey([4_099, 0, 0, 0]),
                    milestones: (),
                },
                (),
            )
            .expect("extend pinned parent after compaction")
            .expect("retain child of pinned parent");
    }

    fn splice_tail_fixture_with_actions<A>(
        prefix: usize,
        tail: usize,
        action: impl Fn(u8) -> A,
    ) -> (Archive<A, SpliceKey, (), ()>, [usize; 3])
    where
        A: Clone + std::fmt::Debug + Ord + Serialize + serde::de::DeserializeOwned,
    {
        let mut archive = Archive::new(|_| 1);
        let key = SpliceKey {
            class_label: 1,
            class_progress: 0,
            cell_progress: 0,
            slot: 0,
        };
        let parent = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![action(255)],
                    key,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        let donor = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![action(1); prefix],
                    key,
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        let leaf = archive
            .insert(
                Some(donor),
                0,
                ArchiveCandidate {
                    suffix: (0..tail)
                        .map(|i| action(2 + (i % 200) as u8))
                        .collect::<Vec<_>>(),
                    key: SpliceKey {
                        cell_progress: 1,
                        ..key
                    },
                    milestones: (),
                },
                (),
            )
            .unwrap()
            .unwrap();
        (archive, [parent, donor, leaf])
    }

    fn splice_tail_fixture(
        prefix: usize,
        tail: usize,
    ) -> (Archive<u8, SpliceKey, (), ()>, [usize; 3]) {
        splice_tail_fixture_with_actions(prefix, tail, |action| action)
    }

    fn unowned_splice_tail_fixture(
        prefix: usize,
        tail: usize,
    ) -> (Archive<u8, SpliceKey, (), ()>, [usize; 3]) {
        let (mut archive, ids) = splice_tail_fixture(prefix, tail);
        let donor_node = archive.entries[ids[1]].input_node;
        archive.input_index.set_owner(donor_node, None);
        (archive, ids)
    }

    fn compaction_fixture(branches: u16, depth: usize, prune: bool) -> InputIndex<u16> {
        let mut index = InputIndex::default();
        let mut leaves = Vec::new();
        for branch in 0..branches {
            let mut actions = vec![0; depth.max(1)];
            actions[0] = branch;
            let (leaf, _) = index.ensure_path(0, &actions).unwrap();
            index.set_owner(leaf, Some(u64::from(branch)));
            leaves.push(leaf);
        }
        if prune {
            for branch in (0..branches).step_by(2) {
                index.remove_owner_and_prune(leaves[usize::from(branch)], u64::from(branch));
            }
        }
        index
    }

    fn compare_prefix_compaction(actual: &mut InputIndex<u16>) {
        let bytes = postcard::to_stdvec(actual).unwrap();
        let mut reference: InputIndex<u16> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(actual.compact(), reference.compact_reference());
        assert_eq!(
            postcard::to_stdvec(&actual).unwrap(),
            postcard::to_stdvec(&reference).unwrap()
        );
        let bytes = postcard::to_stdvec(&actual).unwrap();
        assert_eq!(actual.compact(), reference.compact_reference());
        assert_eq!(postcard::to_stdvec(&actual).unwrap(), bytes);
    }

    #[test]
    fn single_child_compaction_matches_rebuilt_maps() {
        for branches in [0, 1, 2, 3, 12, 128] {
            for depth in [1, 8, 128] {
                for prune in [false, true] {
                    compare_prefix_compaction(&mut compaction_fixture(branches, depth, prune));
                }
            }
        }
        let mut index = compaction_fixture(128, 8, false);
        for branch in 0..127 {
            let mut actions = vec![0; 8];
            actions[0] = branch;
            let leaf = index.walk(0, &actions).unwrap();
            index.remove_owner_and_prune(leaf, u64::from(branch));
        }
        assert_eq!(index.nodes[0].as_ref().unwrap().children.len(), 1);
        compare_prefix_compaction(&mut index);
        let child = *index.nodes[0]
            .as_ref()
            .unwrap()
            .children
            .values()
            .next()
            .unwrap();
        index.nodes[child] = None;
        compare_prefix_compaction(&mut index);
    }

    #[test]
    fn single_child_compaction_preserves_branching_trees() {
        let mut index = InputIndex::default();
        for leaf in 0..2048_u16 {
            let actions = (0..11).map(|bit| (leaf >> bit) & 1).collect::<Vec<_>>();
            let (node, _) = index.ensure_path(0, &actions).unwrap();
            index.set_owner(node, Some(u64::from(leaf)));
        }
        assert!(
            index
                .nodes
                .iter()
                .flatten()
                .all(|node| node.children.len() != 1)
        );
        compare_prefix_compaction(&mut index);
        if std::env::var_os("DISSONANCE_BENCHMARK_PREFIX_COMPACTION").is_some() {
            let bytes = postcard::to_stdvec(&index).unwrap();
            let ratio = paired_prefix_compaction(&bytes);
            eprintln!(
                "prefix compaction binary tree nodes={}: new/old={ratio:.3}",
                index.nodes.len()
            );
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "Wall time is used only by the opt-in benchmark"
    )]
    fn paired_prefix_compaction(bytes: &[u8]) -> f64 {
        use std::{hint::black_box, time::Instant};
        let mut ratios = Vec::new();
        for round in 0..80 {
            let mut elapsed = [0_u128; 2];
            for new in if round % 2 == 0 {
                [false, true, true, false]
            } else {
                [true, false, false, true]
            } {
                let mut index: InputIndex<u16> = postcard::from_bytes(bytes).unwrap();
                let started = Instant::now();
                let remap = if new {
                    index.compact()
                } else {
                    index.compact_reference()
                };
                black_box(&remap);
                black_box(&index);
                elapsed[usize::from(new)] += started.elapsed().as_nanos();
                drop(remap);
                drop(index);
            }
            ratios.push(elapsed[1] as f64 / elapsed[0] as f64);
        }
        ratios.sort_by(f64::total_cmp);
        ratios[ratios.len() / 2]
    }

    #[test]
    fn single_child_compaction_paired_benchmark() {
        for (branches, depth) in [(1, 8), (1, 1024), (1, 65_536), (64, 128), (4096, 1)] {
            for prune in [false, true] {
                let mut index = compaction_fixture(branches, depth, prune);
                if std::env::var_os("DISSONANCE_BENCHMARK_PREFIX_COMPACTION").is_some() {
                    let bytes = postcard::to_stdvec(&index).unwrap();
                    let ratio = paired_prefix_compaction(&bytes);
                    eprintln!(
                        "prefix compaction branches={branches} depth={depth} prune={prune} live_nodes={}: new/old={ratio:.3}",
                        index.live_nodes
                    );
                }
                compare_prefix_compaction(&mut index);
            }
        }
    }

    #[test]
    fn bounded_splice_tails_match_complete_inputs() {
        use std::hint::black_box;
        for (prefix, tail, limit) in [
            (0, 1, 128),
            (0, 4096, 128),
            (1, 4096, 128),
            (0, 4096, 4096),
            (4096, 4096, 4096),
            (1, 6, 128),
            (128, 6, 128),
            (4096, 6, 128),
            (65536, 6, 128),
            (128, 128, 128),
            (4096, 128, 128),
            (65536, 128, 128),
            (128, 4096, 128),
            (4096, 4096, 128),
            (65536, 4096, 128),
            (4096, 128, 0),
            (4096, 128, 1),
        ] {
            let (archive, [parent, donor, leaf]) = splice_tail_fixture(prefix, tail);
            assert_eq!(
                archive.recorded_splice_tail(parent, donor, leaf, limit),
                archive.recorded_splice_tail_reference(parent, donor, leaf, limit)
            );
            if std::env::var_os("DISSONANCE_BENCHMARK_SPLICE_TAIL").is_some() {
                let ratio = paired_candidate_timing(
                    |new, _| {
                        black_box(if new {
                            archive.recorded_splice_tail(parent, donor, leaf, limit)
                        } else {
                            archive.recorded_splice_tail_reference(parent, donor, leaf, limit)
                        })
                        .unwrap();
                    },
                    (4096 / (prefix + tail)).clamp(2, 128),
                );
                eprintln!("splice_tail prefix={prefix} tail={tail} limit={limit} ratio={ratio:.4}");
            }
        }
    }

    #[test]
    fn bounded_splice_tails_preserve_errors_and_validation_order() {
        for case in 0..12 {
            let (mut archive, [parent, donor, leaf]) = unowned_splice_tail_fixture(4, 6);
            let donor_node = archive.entries[donor].input_node;
            let leaf_node = archive.entries[leaf].input_node;
            match case {
                0 => {}
                1 => archive.entries[donor].input_len += 1,
                2 => archive.entries[leaf].input_len += 1,
                3 => archive.input_index.nodes[donor_node] = None,
                4 => archive.input_index.nodes[leaf_node] = None,
                5 => {
                    archive.input_index.nodes[donor_node]
                        .as_mut()
                        .unwrap()
                        .parent = Some(donor_node)
                }
                6 => {
                    archive.input_index.nodes[leaf_node]
                        .as_mut()
                        .unwrap()
                        .action = None
                }
                7 => archive.entries[leaf].key.cell_progress = 0,
                8 => archive.entries[donor].key.class_label = 2,
                9 => {
                    archive.entries[leaf].input_node = donor_node;
                    archive.entries[leaf].input_len = 4;
                }
                10 => {
                    archive.entries[leaf].input_node = archive.entries[parent].input_node;
                    archive.entries[leaf].input_len = 1;
                }
                _ => archive.entries[leaf].input_node = usize::MAX,
            }
            for p in [parent, donor, leaf, usize::MAX] {
                for d in [parent, donor, leaf, usize::MAX] {
                    for l in [parent, donor, leaf, usize::MAX] {
                        for limit in [0, 1, 6, 128, usize::MAX] {
                            assert_eq!(
                                archive.recorded_splice_tail(p, d, l, limit),
                                archive.recorded_splice_tail_reference(p, d, l, limit),
                                "case={case} p={p} d={d} l={l} limit={limit}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn bounded_splice_tails_match_noncanonical_prefixes() {
        for case in 0..7 {
            let (mut archive, [parent, donor, leaf]) = splice_tail_fixture(4, 6);
            let mut original = archive.entries[donor].input_node;
            let mut copied = 0;
            let copied_root = archive.input_index.nodes.len();
            let mut actions = Vec::new();
            while original != 0 {
                let node = archive.input_index.nodes[original].as_ref().unwrap();
                actions.push(node.action.unwrap());
                original = node.parent.unwrap();
            }
            for (index, action) in actions.into_iter().rev().enumerate() {
                let node = super::InputNode {
                    parent: Some(copied),
                    action: Some(if (case == 1 || case == 5 || case == 6) && index == 1 {
                        99
                    } else {
                        action
                    }),
                    children: BTreeMap::new(),
                    owner: None,
                };
                copied = archive.input_index.nodes.len();
                archive.input_index.nodes.push(Some(node));
            }
            match case {
                2 => {
                    archive.input_index.nodes[copied_root]
                        .as_mut()
                        .unwrap()
                        .action = None
                }
                3 | 5 => {
                    archive.input_index.nodes[copied_root]
                        .as_mut()
                        .unwrap()
                        .parent = None
                }
                4 | 6 => {
                    archive.input_index.nodes[copied_root]
                        .as_mut()
                        .unwrap()
                        .parent = Some(copied_root)
                }
                _ => {}
            }
            let mut start = archive.entries[leaf].input_node;
            for _ in 0..5 {
                start = archive.input_index.nodes[start]
                    .as_ref()
                    .unwrap()
                    .parent
                    .unwrap();
            }
            archive.input_index.nodes[start].as_mut().unwrap().parent = Some(copied);
            for limit in [0, 1, 6, 128] {
                assert_eq!(
                    archive.recorded_splice_tail(parent, donor, leaf, limit),
                    archive.recorded_splice_tail_reference(parent, donor, leaf, limit)
                );
            }
        }
    }

    #[test]
    fn restore_rejects_an_owned_entry_whose_length_misses_its_prefix() {
        let (archive, [_, donor, _]) = splice_tail_fixture(4, 6);
        let bytes = postcard::to_stdvec(&archive).expect("encode archive");
        let mut restored: Archive<u8, SpliceKey, (), ()> =
            postcard::from_bytes(&bytes).expect("decode archive");
        restored
            .restore_runtime(|_| 1, None, Vec::new())
            .expect("restore a consistent archive");
        restored.entries[donor].input_len += 1;
        assert_eq!(
            restored.restore_runtime(|_| 1, None, Vec::new()),
            Err("a checkpoint entry's input length does not match its stored prefix")
        );
        let donor_node = restored.entries[donor].input_node;
        restored.entries[donor].input_len -= 1;
        restored.input_index.nodes[donor_node]
            .as_mut()
            .unwrap()
            .parent = Some(donor_node);
        assert_eq!(
            restored.restore_runtime(|_| 1, None, Vec::new()),
            Err("a checkpoint entry's input length does not match its stored prefix")
        );
    }

    #[test]
    fn bounded_splice_tails_validate_mixed_prefix_metadata() {
        let mut rand = RomuDuoJrRand::with_seed(423);
        for case in 0..512 {
            let (mut archive, [parent, donor, leaf]) = unowned_splice_tail_fixture(4, 6);
            let node_count = archive.input_index.nodes.len();
            let leaf_node = archive.entries[leaf].input_node;
            let node = archive.input_index.nodes[leaf_node].as_mut().unwrap();
            node.parent = Some((rand.next_u64() as usize) % (node_count + 2));
            if case % 5 == 0 {
                node.action = None;
            }
            archive.entries[leaf].input_node = (rand.next_u64() as usize) % (node_count + 2);
            archive.entries[leaf].input_len = (rand.next_u64() % 16) as usize;
            if case % 4 == 0 {
                archive.entries[donor].input_node = (rand.next_u64() as usize) % (node_count + 2);
                archive.entries[donor].input_len = (rand.next_u64() % 16) as usize;
            }
            for limit in [0, 1, 128] {
                assert_eq!(
                    archive.recorded_splice_tail(parent, donor, leaf, limit),
                    archive.recorded_splice_tail_reference(parent, donor, leaf, limit),
                    "case={case}, limit={limit}"
                );
            }
        }
    }

    #[test]
    fn bounded_splice_validation_rejects_lengths_exceeding_stored_nodes() {
        let (mut archive, [parent, donor, leaf]) = unowned_splice_tail_fixture(4, 6);
        let donor_node = archive.entries[donor].input_node;
        let donor_parent = archive.input_index.nodes[donor_node]
            .as_ref()
            .unwrap()
            .parent;
        archive.entries[donor].input_len = usize::MAX;
        archive.input_index.nodes[donor_node]
            .as_mut()
            .unwrap()
            .parent = Some(donor_node);
        assert_eq!(
            archive.recorded_splice_tail(parent, donor, leaf, 128),
            Err("splice donor prefix is unavailable")
        );
        archive.entries[donor].input_len = 4;
        archive.input_index.nodes[donor_node]
            .as_mut()
            .unwrap()
            .parent = donor_parent;
        let leaf_node = archive.entries[leaf].input_node;
        archive.entries[leaf].input_len = usize::MAX;
        archive.input_index.nodes[leaf_node]
            .as_mut()
            .unwrap()
            .parent = Some(leaf_node);
        assert_eq!(
            archive.recorded_splice_tail(parent, donor, leaf, 128),
            Err("splice leaf prefix is unavailable")
        );
    }

    #[test]
    fn bounded_splice_tails_clone_only_the_requested_actions() {
        use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
        static CLONES: AtomicUsize = AtomicUsize::new(0);
        static COMPARISONS: AtomicUsize = AtomicUsize::new(0);
        #[derive(Debug, Deserialize, Eq, Ord, PartialOrd, Serialize)]
        struct Action(u8);
        impl PartialEq for Action {
            fn eq(&self, other: &Self) -> bool {
                COMPARISONS.fetch_add(1, AtomicOrdering::Relaxed);
                self.0 == other.0
            }
        }
        impl Clone for Action {
            fn clone(&self) -> Self {
                CLONES.fetch_add(1, AtomicOrdering::Relaxed);
                Self(self.0)
            }
        }
        let (archive, [parent, donor, leaf]) = splice_tail_fixture_with_actions(4096, 256, Action);
        for limit in [0, 1, 128, 256, 512] {
            CLONES.store(0, AtomicOrdering::Relaxed);
            COMPARISONS.store(0, AtomicOrdering::Relaxed);
            let tail = archive
                .recorded_splice_tail(parent, donor, leaf, limit)
                .unwrap();
            assert_eq!(COMPARISONS.load(AtomicOrdering::Relaxed), 0);
            assert_eq!(tail.len(), limit.min(256));
            assert_eq!(CLONES.load(AtomicOrdering::Relaxed), tail.len());
            assert_eq!(tail.capacity(), tail.len());
            CLONES.store(0, AtomicOrdering::Relaxed);
            let reference = archive
                .recorded_splice_tail_reference(parent, donor, leaf, limit)
                .unwrap();
            assert_eq!(
                CLONES.load(AtomicOrdering::Relaxed),
                4096 + 4352 + tail.len()
            );
            assert_eq!(tail, reference);
        }
    }

    #[test]
    fn a_splice_tail_extends_past_the_parent_from_a_donor_in_its_slot() {
        let mut archive = Archive::<u8, FlatKey, (), ()>::new(|_| 1);
        let insert = |archive: &mut Archive<u8, FlatKey, (), ()>,
                      parent: Option<usize>,
                      components: [u16; 4],
                      actions: Vec<u8>| {
            let parent_len = parent.map_or(0, |id| archive.entries[id].input_len);
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        suffix: actions[parent_len..].to_vec(),
                        key: FlatKey(components),
                        milestones: (),
                    },
                    (),
                )
                .expect("insert entry")
                .expect("retain entry")
        };
        let root = insert(&mut archive, None, [1, 2, 3, 4], vec![0]);
        let arrival = insert(&mut archive, None, [1, 2, 3, 4], vec![9]);
        let beside = insert(&mut archive, None, [0, 2, 3, 4], vec![8]);
        let middle = insert(&mut archive, Some(root), [1, 2, 3, 6], vec![0, 1]);
        let leaf = insert(&mut archive, Some(middle), [1, 2, 3, 7], vec![0, 1, 2]);
        let dispatched = archive
            .splice_tail_for_campaign(arrival, 8)
            .expect("dispatch-time splice");
        assert_eq!((dispatched.donor_id, dispatched.leaf_id), (root, leaf));
        assert_eq!(dispatched.actions, vec![1, 2]);
        assert!(
            archive.splice_tail_for_campaign(beside, 8).is_none(),
            "a donor in another slot of the same cell does not splice"
        );
        assert_eq!(
            archive
                .recorded_splice_tail(arrival, dispatched.donor_id, dispatched.leaf_id, 1)
                .expect("capped recorded splice"),
            vec![1]
        );
        let later = insert(&mut archive, Some(leaf), [1, 2, 3, 8], vec![0, 1, 2, 3]);
        assert_eq!(
            archive
                .splice_tail_for_campaign(arrival, 8)
                .map(|splice| splice.actions),
            Some(vec![1, 2, 3]),
            "a later admission may advance the current donor frontier"
        );
        assert_eq!(
            archive
                .recorded_splice_tail(arrival, dispatched.donor_id, dispatched.leaf_id, 8)
                .expect("recorded dispatch-time splice"),
            vec![1, 2],
            "recorded ids preserve the in-flight job's original suffix"
        );
        assert!(
            archive
                .recorded_splice_tail(arrival, dispatched.donor_id, later, 8)
                .is_ok()
        );
        assert!(
            archive.splice_tail_for_campaign(leaf, 8).is_none(),
            "the deepest entry has no deeper donor in its slot"
        );
    }
    #[test]
    fn selection_runs_on_keys_without_progress() {
        let keys = [[1, 2, 3, 4], [1, 2, 3, 5], [9, 8, 7, 4]];
        let mut archive = flat_archive(&keys);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_0001);
        for _ in 0..128 {
            let (id, draw) = archive
                .select_parent(&mut rand)
                .expect("selection under a flat key");
            assert!(id < keys.len());
            assert_eq!(draw.tier_rank, Some(0));
            archive.record_selection(id, &draw);
        }
        let report = archive.selector_report();
        assert_eq!(report.tier_draws_by_rank, vec![128]);
        assert_eq!(report.draws_by_cell.values().sum::<u64>(), 128);
    }

    fn probe_key(major: u8, minor: u8, progress: u16, vertical: u8) -> TestKey {
        TestKey {
            major,
            minor,
            progress,
            y_bucket: vertical,
            state_fingerprint: 0,
            x_bucket: 0,
            time_bucket: 0,
            region: [0; 3],
        }
    }

    fn chain_insert(
        archive: &mut TimedArchive,
        parent: Option<usize>,
        prefix: &Input<TestAction>,
        code: u8,
        hold: u8,
        key: TestKey,
    ) -> (Option<usize>, Input<TestAction>) {
        let mut input = prefix.clone();
        input.actions.push(TestAction::new(code, hold));
        let id = archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    suffix: vec![TestAction::new(code, hold)],
                    key,
                    milestones: (),
                },
                (),
            )
            .expect("chained insert");
        (id, input)
    }

    #[test]
    fn cost_in_group_counts_from_the_recorded_coarse_transition() {
        let mut archive = TimedArchive::new(TestAction::duration);
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: (),
                },
                (),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        assert_eq!(archive.entry_cost_in_group(genesis), 0);
        let (first, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &Input::default(),
            0x01,
            30,
            probe_key(0, 0, 4, 0),
        );
        let first = first.expect("first retained");
        assert_eq!(archive.entry_cost_in_group(first), 30);
        let (second, input) = chain_insert(
            &mut archive,
            Some(first),
            &input,
            0x01,
            20,
            probe_key(0, 0, 8, 0),
        );
        let second = second.expect("second retained");
        assert_eq!(archive.entry_cost_in_group(second), 50);
        let (crossed, input) = chain_insert(
            &mut archive,
            Some(second),
            &input,
            0x01,
            40,
            probe_key(0, 1, 2, 0),
        );
        let crossed = crossed.expect("crossing retained");
        assert_eq!(archive.entry_cost_in_group(crossed), 40);
        let (after, _) = chain_insert(
            &mut archive,
            Some(crossed),
            &input,
            0x01,
            10,
            probe_key(0, 1, 6, 0),
        );
        assert_eq!(archive.entry_cost_in_group(after.expect("retained")), 50);
    }

    #[test]
    fn the_time_rule_displaces_a_slower_route_into_a_full_slot() {
        let slot = probe_key(0, 0, 16, 0);
        let mut archive = TimedArchive::new(TestAction::duration);
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: Vec::new(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: (),
                },
                (),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        for code in [0x01_u8, 0x02] {
            chain_insert(
                &mut archive,
                Some(genesis),
                &Input::default(),
                code,
                120,
                slot,
            );
        }
        assert_eq!(archive.active_count(), 3);
        let (fast, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &Input::default(),
            0x04,
            5,
            probe_key(0, 0, 8, 0),
        );
        let admitted = chain_insert(&mut archive, fast, &input, 0x04, 6, slot)
            .0
            .expect("the route of cost eleven displaces a slower one");
        assert_eq!(archive.entry_cost_in_group(admitted), 11);
        assert_eq!(archive.replacement_cost_displaced(), 1);
        assert_eq!(archive.active_count(), 4);
        let (slower, _) = chain_insert(&mut archive, fast, &input, 0x04, 200, slot);
        assert!(
            slower.is_none(),
            "a slower route never displaces a faster one"
        );
        assert_eq!(archive.active_count(), 4);
    }
    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct SpliceKey {
        class_label: u16,
        class_progress: u16,
        cell_progress: u16,
        slot: u16,
    }

    impl ArchiveKey for SpliceKey {
        type Place = u16;
        type Progress = (u16, u16);
        type Identity = u16;
        type Lineage = ();

        fn place(self) -> Self::Place {
            self.class_label
        }

        fn progress(self) -> Self::Progress {
            (self.class_progress, self.cell_progress)
        }

        fn identity(self) -> Self::Identity {
            self.slot
        }

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }
    #[test]
    fn a_splice_leaf_advances_by_progress_not_by_label() {
        let mut archive = Archive::<u8, SpliceKey, (), ()>::new(|_| 1);
        let insert = |archive: &mut Archive<u8, SpliceKey, (), ()>,
                      parent: Option<usize>,
                      action: u8,
                      key: SpliceKey| {
            archive
                .insert(
                    parent,
                    0,
                    ArchiveCandidate {
                        suffix: vec![action],
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("splice insert")
                .expect("splice retention")
        };
        let parent = insert(
            &mut archive,
            None,
            1,
            SpliceKey {
                class_label: 9,
                class_progress: 0,
                cell_progress: 0,
                slot: 0,
            },
        );
        let donor = insert(
            &mut archive,
            None,
            2,
            SpliceKey {
                class_label: 9,
                class_progress: 0,
                cell_progress: 0,
                slot: 0,
            },
        );
        let leaf = insert(
            &mut archive,
            Some(donor),
            3,
            SpliceKey {
                class_label: 1,
                class_progress: 0,
                cell_progress: 5,
                slot: 0,
            },
        );
        assert!(
            archive.entries[leaf].key < archive.entries[parent].key,
            "the fixture needs a leaf that sorts below its parent"
        );
        let tail = archive
            .recorded_splice_tail(parent, donor, leaf, 8)
            .expect("a leaf ahead by progress splices past its parent");
        assert_eq!(tail, vec![3]);
        let campaign = archive
            .splice_tail_for_campaign(parent, 8)
            .expect("the campaign splice finds the same leaf");
        assert_eq!(campaign.donor_id, donor);
        assert_eq!(campaign.leaf_id, leaf);
        assert_eq!(campaign.actions, vec![3]);
    }

    fn tier_archive(keys: &[(u8, u8, u16)]) -> TestArchive {
        let mut archive = TestArchive::new(|_| 1);
        for (index, (major, minor, progress)) in keys.iter().enumerate() {
            let mut key = probe_key(*major, *minor, *progress, 0);
            key.state_fingerprint = u8::try_from(index).expect("fingerprint byte");
            archive.prepare_selection();
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: vec![u8::try_from(index).expect("input byte")],
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("insert tier entry")
                .expect("retain tier entry");
        }
        archive
    }

    fn draw_counts(archive: &mut TestArchive, seed: u64, draws: usize) -> Vec<u64> {
        let mut rand = RomuDuoJrRand::with_seed(seed);
        for _ in 0..draws {
            let (id, draw) = archive.select_parent(&mut rand).expect("a tier draw");
            archive.record_selection(id, &draw);
        }
        archive.selected.clone()
    }

    #[test]
    fn the_deepest_progress_tier_takes_most_draws_and_lower_tiers_still_draw() {
        let mut archive = tier_archive(&[(1, 1, 0), (1, 2, 0), (2, 1, 0)]);
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_0001, 4096);
        assert!(counts[2] > counts[1] && counts[1] > counts[0]);
        assert!(counts[0] > 0);
        draw_counts(&mut archive, 0x7ea1_0001, 4096);
        let report = archive.selector_report();
        assert_eq!(report.tier_draws_by_rank.len(), 3);
        assert_eq!(report.tier_draws_by_rank.iter().sum::<u64>(), 4096);
    }

    #[test]
    fn the_progress_order_beats_the_key_order_when_choosing_the_top_tier() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_0002, 1024);
        assert!(counts[0] > counts[1] * 4);
    }

    fn unrecorded_draw_counts(archive: &mut TestArchive, seed: u64, draws: usize) -> Vec<u64> {
        let mut rand = RomuDuoJrRand::with_seed(seed);
        let mut counts = vec![0_u64; archive.selected.len()];
        for _ in 0..draws {
            let (id, _) = archive.select_parent(&mut rand).expect("a tier draw");
            counts[id] += 1;
        }
        counts
    }

    #[test]
    fn a_top_tier_that_opens_no_cell_falls_to_the_next_tiers_weight() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(0, &draw);
        }
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_0005, 4096);
        assert!(
            counts[0] * 4 < counts[1] * 5 && counts[1] * 4 < counts[0] * 5,
            "{counts:?}"
        );
    }

    #[test]
    fn a_top_tier_that_wins_as_often_as_the_next_tier_yields_keeps_its_weight() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for id in [0, 1] {
            for _ in 0..64 {
                archive.record_selection(id, &draw);
            }
        }
        archive.tier_runs_mut((2, 1)).wins = 64;
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_0009, 4096);
        assert!(counts[0] > counts[1] * 5, "{counts:?}");
    }

    fn roughly_equal(left: u64, right: u64) -> bool {
        left * 4 < right * 5 && right * 4 < left * 5
    }

    #[test]
    fn a_top_tier_without_a_snapshot_compares_against_the_next_tiers_whole_counts() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let top = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(0, &top);
        }
        let runs = archive.tier_runs_mut((2, 1));
        runs.wins = 32;
        runs.next_tier = String::new();
        runs.next_draws = 0;
        runs.next_yields = 0;
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_000a, 4096);
        assert!(roughly_equal(counts[0], counts[1]), "{counts:?}");
        let next = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(1),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(1, &next);
        }
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_000a, 4096);
        assert!(counts[0] > counts[1] * 5, "{counts:?}");
    }

    #[test]
    fn a_tier_that_appears_below_the_top_replaces_the_snapshot_tier() {
        let mut archive = tier_archive(&[(1, 9, 0), (3, 1, 0), (2, 1, 0)]);
        assert_eq!(
            archive.tier_runs((3, 1)).next_tier,
            format!("{:?}", (1_u8, 9_u8))
        );
        let top = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(1, &top);
        }
        archive.tier_runs_mut((3, 1)).wins = 32;
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_000b, 4096);
        assert!(roughly_equal(counts[1], counts[2]), "{counts:?}");
        let middle = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(1),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(2, &middle);
        }
        let counts = unrecorded_draw_counts(&mut archive, 0x7ea1_000b, 4096);
        assert!(counts[1] > counts[2] * 5, "{counts:?}");
    }

    #[test]
    fn tier_runs_survive_a_checkpoint() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..64 {
            archive.record_selection(0, &draw);
        }
        let runs = archive.selector_counters().tier_runs;
        assert_eq!(runs.values().map(|runs| runs.current).max(), Some(64));
        let mut restored: TestArchive =
            postcard::from_bytes(&postcard::to_stdvec(&archive).expect("encode archive"))
                .expect("decode archive");
        restored
            .restore_runtime(|_| 1, None, Vec::new())
            .expect("restore archive");
        assert_eq!(restored.selector_counters().tier_runs, runs);
        assert_eq!(
            unrecorded_draw_counts(&mut restored, 0x7ea1_0008, 4096),
            unrecorded_draw_counts(&mut archive, 0x7ea1_0008, 4096)
        );
    }

    #[test]
    fn a_top_tier_keeps_its_weight_until_its_draws_pass_twice_its_longest_run() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..40 {
            archive.record_selection(0, &draw);
        }
        let mut key = probe_key(2, 1, 5, 0);
        key.state_fingerprint = 2;
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![2],
                    key,
                    milestones: (),
                },
                (),
            )
            .expect("insert tier entry")
            .expect("retain tier entry");
        for _ in 0..80 {
            archive.record_selection(0, &draw);
        }
        let full = top_tier_draws(&mut archive, 0x7ea1_0006, 4096);
        assert!(full[0] > full[1] * 5, "{full:?}");
        archive.record_selection(0, &draw);
        let halved = top_tier_draws(&mut archive, 0x7ea1_0007, 4096);
        assert!(
            halved[0] > halved[1] * 2 && halved[0] < halved[1] * 6,
            "{halved:?}"
        );
    }

    fn top_tier_draws(archive: &mut TestArchive, seed: u64, draws: usize) -> [u64; 2] {
        let mut rand = RomuDuoJrRand::with_seed(seed);
        let mut counts = [0_u64; 2];
        for _ in 0..draws {
            let (id, _) = archive.select_parent(&mut rand).expect("a tier draw");
            counts[usize::from(id == 1)] += 1;
        }
        counts
    }

    #[test]
    fn cells_in_one_tier_share_draws_by_their_own_count() {
        let mut archive = tier_archive(&[(1, 1, 0), (1, 1, 0)]);
        archive.deactivate(1);
        let mut key = archive.entries[1].key;
        key.region = [1, 0, 0];
        archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    suffix: vec![9],
                    key,
                    milestones: (),
                },
                (),
            )
            .expect("insert second cell")
            .expect("retain second cell");
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..1000 {
            archive.record_selection(0, &draw);
        }
        let before = archive.selected.clone();
        let counts = draw_counts(&mut archive, 0x7ea1_0003, 256);
        let fresh = counts[2] - before[2];
        let stale = counts[0] - before[0];
        assert!(fresh > stale * 8, "fresh {fresh} stale {stale}");
    }

    #[test]
    fn holders_in_one_cell_share_draws_by_their_own_count() {
        let mut archive = tier_archive(&[(1, 1, 0), (1, 1, 0)]);
        let draw = SelectorDraw {
            path: SelectorPath::Tiers,
            tier_rank: Some(0),
            best_preference: None,
        };
        for _ in 0..1000 {
            archive.record_selection(0, &draw);
        }
        let before = archive.selected.clone();
        let counts = draw_counts(&mut archive, 0x7ea1_0004, 256);
        assert!(counts[1] - before[1] > (counts[0] - before[0]) * 8);
    }

    #[test]
    fn a_landed_continuation_that_breeds_counts_as_useful() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        let neighbour =
            insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 5, 5).expect("neighbour");
        archive.record_continuation_outcome(Some(neighbour), true, true, 1);
        archive.record_continuation_outcome(None, false, false, 1);
        let report = archive.continuation_report();
        assert_eq!((report.jobs, report.landed, report.replaced), (2, 1, 1));
        assert_eq!(report.opened_new_cell, 1);
        archive.record_selection_outcome(neighbour, true);
        archive.record_selection_outcome(origin, true);
        archive.record_selection_outcome(neighbour, false);
        assert_eq!(archive.continuation_report().useful, 1);
    }

    #[test]
    fn an_edge_records_the_preferences_its_tail_gains() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 30, 5).expect("gaining");
        insert_portfolio_at(&mut archive, None, vec![3], 1, 12, 30).expect("improved origin");
        let taken = archive.pop_continuation(false).expect("the queued source");
        assert_eq!(taken.gains, 0b01);
        assert_eq!(taken.destination, (2, ()));
    }

    #[test]
    fn positions_ignore_the_resources_a_holder_carries() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 5, 5).expect("neighbour");
        let richer =
            insert_portfolio_at(&mut archive, None, vec![3], 1, 40, 40).expect("richer origin");
        assert_eq!(archive.continuation_pending(), 1);
        let taken = archive
            .pop_continuation(false)
            .expect("the richer origin is queued");
        assert_eq!(archive.index_of_id(taken.parent), Some(richer));
    }

    #[test]
    fn a_destination_holder_check_looks_at_every_holder_of_the_slot() {
        let mut archive = portfolio_bank();
        let origin = insert_portfolio_at(&mut archive, None, vec![1], 1, 10, 20).expect("origin");
        insert_portfolio_at(&mut archive, Some(origin), vec![2], 2, 20, 20).expect("holder");
        let weaker = insert_portfolio_at(&mut archive, None, vec![3], 1, 12, 30).expect("weaker");
        assert!(!archive.outranks_slot_holders(weaker, (2, ()), 0));
        assert!(archive.outranks_slot_holders(weaker, (3, ()), 0));
        let stronger =
            insert_portfolio_at(&mut archive, None, vec![4], 1, 30, 40).expect("stronger");
        assert!(archive.outranks_slot_holders(stronger, (2, ()), 0));
    }
}
