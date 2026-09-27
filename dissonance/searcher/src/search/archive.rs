// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Debug,
    mem::size_of,
    num::NonZeroUsize,
    sync::Arc,
};

use crate::search::{
    continuation::{Continuation, ContinuationBank},
    rand::RomuDuoJrRand,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

fn retain_marked<T>(values: Vec<T>, keep: &[bool]) -> Vec<T> {
    values
        .into_iter()
        .zip(keep)
        .filter_map(|(value, keep)| (*keep).then_some(value))
        .collect()
}

pub trait ArchiveKey: Copy + Ord + Serialize + DeserializeOwned {
    type Place: Copy + Ord + Debug;
    type Progress: Copy + Ord + Debug;
    type Identity: Copy + Ord + Debug;
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
    type Lineage: Clone + Default;
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

pub const SELECTOR_IDENTIFIER: &str = "tier_cell_count_decay_v3";

const TIER_RANK_CAP: u8 = 8;

const TIER_RANK_SHIFT: u32 = 3;

const MAX_TIER_RANK_SHIFT: u32 = (u64::BITS - 1) / TIER_RANK_CAP as u32;

const COUNT_DECAY_EXPONENT: u32 = 2;

const COUNT_DECAY_SCALE: u64 = 1 << 32;

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

fn draw_weighted(rand: &mut RomuDuoJrRand, weights: &[u64]) -> Result<usize, Box<dyn Error>> {
    let total = weights
        .iter()
        .fold(0_u64, |sum, weight| sum.saturating_add(*weight));
    let total = NonZeroUsize::new(usize::try_from(total)?).ok_or("weighted draw over nothing")?;
    let mut draw = u64::try_from(rand.below(total))?;
    for (index, weight) in weights.iter().enumerate() {
        if draw < *weight {
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
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectorDraw {
    pub path: SelectorPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier_rank: Option<u8>,
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
    pub cell_selections: u64,
    pub productive_selections: u64,
    pub cell_resets: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tier_draws_by_rank: Vec<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub draws_by_cell: BTreeMap<String, u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioAccounting>,
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
    #[serde(
        bound = "A: Serialize + DeserializeOwned + Ord + Clone, K: ArchiveKey, M: Serialize + \
                     DeserializeOwned + Clone"
    )]
    struct Wire<A: Ord, K, M> {
        id: u64,
        parent_id: Option<u64>,
        created_execution: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input: Option<Input<A>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_suffix: Option<Vec<A>>,
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
        let wires: Vec<Wire<A, K, M>> = entries
            .iter()
            .map(|entry| {
                let parent = entry
                    .parent_id
                    .and_then(|id| index_of.get(&id))
                    .map(|index| &entries[*index].input.actions)
                    .filter(|parent| entry.input.actions.starts_with(parent));
                let (input, input_suffix) = match parent {
                    Some(parent) => (None, Some(entry.input.actions[parent.len()..].to_vec())),
                    None => (Some(entry.input.clone()), None),
                };
                Wire {
                    id: entry.id,
                    parent_id: entry.parent_id,
                    created_execution: entry.created_execution,
                    input,
                    input_suffix,
                    key: entry.key,
                    milestones: entry.milestones.clone(),
                    selector: entry.selector,
                }
            })
            .collect();
        wires.serialize(serializer)
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
        let wires = Vec::<Wire<A, K, M>>::deserialize(deserializer)?;
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

#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntry<A: Ord, K, M, S> {
    pub(crate) id: u64,
    pub(crate) parent_id: Option<u64>,
    pub(crate) created_execution: u64,
    pub(crate) input_suffix: Vec<A>,
    pub(crate) input_len: usize,
    input_node: usize,
    pub(crate) key: K,
    pub(crate) milestones: M,
    pub(crate) snapshot: Option<Arc<S>>,
}

pub(crate) struct CampaignSpliceTail<A> {
    pub(crate) donor_id: usize,
    pub(crate) leaf_id: usize,
    pub(crate) actions: Vec<A>,
}

pub struct ArchiveCandidate<A: Ord, K, M> {
    pub suffix: Vec<A>,
    pub key: K,
    pub milestones: M,
}

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
    selector_accounting: SelectorAccounting,
    cost_in_group: Vec<u64>,
    replacement_cost_displaced: u64,
    portfolio_replacements: Vec<u64>,
    portfolio_cross_improvements: u64,
    replacement_preferences: Vec<u8>,
    lineages: Vec<K::Lineage>,
    deepest_leaf: Vec<(K, usize)>,
    action_cost: fn(&A) -> u64,
    live_progress: Option<(K, u64)>,
    selector_indexed: bool,
    active_ids: ActiveIds,
    tiers: BTreeMap<K::Progress, BTreeMap<K::Place, CellMembers>>,
    donors: BTreeMap<Slot<K>, BTreeSet<DonorRank<K>>>,
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
    snapshot_memory_charge: Option<fn(&S) -> usize>,
    resident_snapshot_order: VecDeque<usize>,
    snapshot_evictions: u64,
    history_compactions: u64,
    historical_entries_dropped: u64,
    input_reconstructions: std::cell::Cell<u64>,
    continuations: Option<ContinuationBank<Position<K>, A>>,
    continuation_wave: u32,
    continuation_accounting: ContinuationAccounting,
    landed: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
struct CellState {
    active: usize,
    draws: u64,
    draws_total: u64,
}

#[derive(Default)]
struct CellMembers {
    ids: BTreeSet<usize>,
}

struct DonorRank<K: ArchiveKey> {
    leaf_key: K,
    leaf_id: usize,
    donor_id: usize,
}

impl<K: ArchiveKey> Clone for DonorRank<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ArchiveKey> Copy for DonorRank<K> {}

impl<K: ArchiveKey> PartialEq for DonorRank<K> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<K: ArchiveKey> Eq for DonorRank<K> {}

impl<K: ArchiveKey> PartialOrd for DonorRank<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: ArchiveKey> Ord for DonorRank<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        leaf_order(
            (self.leaf_key, self.leaf_id),
            (other.leaf_key, other.leaf_id),
        )
        .then_with(|| self.donor_id.cmp(&other.donor_id))
    }
}

struct InputNode<A: Ord> {
    parent: Option<usize>,
    action: Option<A>,
    children: BTreeMap<A, usize>,
    owner: Option<u64>,
}

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

#[derive(Default)]
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
            selector_accounting: SelectorAccounting::default(),
            cost_in_group: Vec::new(),
            replacement_cost_displaced: 0,
            portfolio_replacements: vec![0; K::preferences().max(1)],
            portfolio_cross_improvements: 0,
            replacement_preferences: Vec::new(),
            lineages: Vec::new(),
            deepest_leaf: Vec::new(),
            action_cost,
            live_progress: None,
            selector_indexed: false,
            active_ids: ActiveIds::default(),
            tiers: BTreeMap::new(),
            donors: BTreeMap::new(),
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
        if !self.preserve_inactive_snapshots && !worker_holds_snapshot {
            self.entries[id].snapshot.take();
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

    fn compact_history(&mut self, force: bool) -> Result<(), &'static str> {
        let history_target = self
            .memory_limit
            .map_or(usize::MAX, |limit| limit / 4)
            .max(1);
        let entry_pressure = self.entries.len()
            >= self
                .max_entries
                .saturating_add(HISTORY_COMPACTION_MIN_DROPS);
        if !force && self.history_memory_bytes() <= history_target && !entry_pressure {
            return Ok(());
        }
        if !force
            && self.entries.len().saturating_sub(self.active_count) < HISTORY_COMPACTION_MIN_DROPS
        {
            return Ok(());
        }

        let keep = self
            .entries
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
            .collect::<Vec<_>>();
        let dropped = keep.iter().filter(|keep| !**keep).count();
        if !force && dropped < HISTORY_COMPACTION_MIN_DROPS {
            return Ok(());
        }

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
        let live_ids = self
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        self.landed.retain(|id| live_ids.contains(id));

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
        self.donors.clear();
        if selector_indexed {
            self.rebuild_selector_index();
        }
        self.enforce_snapshot_memory_budget()?;
        Ok(())
    }

    pub(crate) fn preserve_inactive_snapshots(
        &mut self,
        preserve: bool,
    ) -> Result<(), &'static str> {
        self.preserve_inactive_snapshots = preserve;
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
            if let Some(index) = self.index_of_id(id) {
                self.reclaim_inactive_snapshot(index);
            }
        }
    }

    pub(crate) fn preserve_recorded_metadata_uses(&mut self, uses: BTreeMap<u64, u32>) {
        self.metadata_pins = uses;
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
        self.tiers.clear();
        self.donors.clear();
        let active = self.active_ids.ids().collect::<Vec<_>>();
        for id in active {
            self.insert_active_cell_member(id);
        }
    }

    pub(crate) fn prepare_selection(&mut self) {
        self.ensure_selector_index();
        self.establish_liveness_anchor();
        self.reactivate_liveness_anchor();
    }

    fn ensure_selector_index(&mut self) {
        if !self.selector_indexed {
            self.rebuild_selector_index();
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
        let key = self.entries[id].key;
        let deepest = self.deepest_leaf[id];
        let slot = slot_of_key(key);
        if let Some(donors) = self.donors.get_mut(&slot) {
            donors.remove(&DonorRank {
                leaf_key: deepest.0,
                leaf_id: deepest.1,
                donor_id: id,
            });
            if donors.is_empty() {
                self.donors.remove(&slot);
            }
        }
        let Some(places) = self.tiers.get_mut(&key.progress()) else {
            return;
        };
        let mut removed_cell = false;
        if let Some(members) = places.get_mut(&key.place()) {
            members.ids.remove(&id);
            removed_cell = members.ids.is_empty();
        }
        if removed_cell {
            places.remove(&key.place());
        }
        if places.is_empty() {
            self.tiers.remove(&key.progress());
        }
    }

    fn insert_active_cell_member(&mut self, id: usize) {
        let key = self.entries[id].key;
        let deepest = self.deepest_leaf[id];
        self.tiers
            .entry(key.progress())
            .or_default()
            .entry(key.place())
            .or_default()
            .ids
            .insert(id);
        self.donors
            .entry(slot_of_key(key))
            .or_default()
            .insert(DonorRank {
                leaf_key: deepest.0,
                leaf_id: deepest.1,
                donor_id: id,
            });
    }

    fn update_index_deepest_leaf(&mut self, id: usize, previous: (K, usize), current: (K, usize)) {
        if !self.selector_indexed {
            return;
        }
        let key = self.entries[id].key;
        let Some(donors) = self.donors.get_mut(&slot_of_key(key)) else {
            return;
        };
        if !donors.remove(&DonorRank {
            leaf_key: previous.0,
            leaf_id: previous.1,
            donor_id: id,
        }) {
            return;
        }
        donors.insert(DonorRank {
            leaf_key: current.0,
            leaf_id: current.1,
            donor_id: id,
        });
    }

    #[must_use]
    pub fn lineage(&self, id: usize) -> Option<&K::Lineage> {
        self.lineages.get(id)
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
    pub fn live_progress(&self) -> Option<(K, u64, u64)> {
        self.live_progress
            .map(|(deepest, cheapest)| (deepest, cheapest, self.retained))
    }

    fn rank_slot_preferences(
        &self,
        slot: &[usize],
        key: K,
        cost_in_group: u64,
    ) -> (Vec<bool>, Vec<usize>) {
        let capacity = K::capacity().max(1);
        let preferences = K::preferences().max(1);
        let members: Vec<(K, u64, u64)> = slot
            .iter()
            .map(|id| {
                (
                    self.entries[*id].key,
                    self.cost_in_group[*id],
                    self.entries[*id].id,
                )
            })
            .chain(std::iter::once((key, cost_in_group, self.next_entry_id)))
            .collect();
        let candidate = members.len().saturating_sub(1);
        let mut won = Vec::new();
        let mut retained = vec![false; members.len()];
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
                retained[*index] = true;
                if *index == candidate {
                    won.push(preference);
                }
            }
        }
        (retained, won)
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

    pub fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate<A, K, M>,
        snapshot: S,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        self.insert_after(parent_id, None, execution, candidate, snapshot)
            .map(|(id, _)| id)
    }

    pub fn insert_after(
        &mut self,
        parent_id: Option<usize>,
        previous: Option<K>,
        execution: u64,
        candidate: ArchiveCandidate<A, K, M>,
        snapshot: S,
    ) -> Result<(Option<usize>, K), Box<dyn Error>> {
        let ArchiveCandidate {
            suffix,
            key,
            milestones,
        } = candidate;
        if let Some(existing) = self.existing_input_id(parent_id, &suffix) {
            return Ok((Some(existing), self.entries[existing].key));
        }
        if parent_id.is_some_and(|id| self.entries.get(id).is_none()) {
            return Err("archive candidate parent is missing".into());
        }
        let parent_ctx =
            parent_id.map(|id| (previous.unwrap_or(self.entries[id].key), &self.lineages[id]));
        let key = key.complete(parent_ctx);
        let mut lineage =
            parent_id.map_or_else(K::Lineage::default, |id| self.lineages[id].clone());
        K::record(&mut lineage, key);
        let candidate_cost_in_group = self.cost_in_group_of(parent_id, &suffix, key);
        let slot = self
            .slots
            .get(&slot_of_key(key))
            .cloned()
            .unwrap_or_default();
        let new_cell = self
            .cells
            .get(&cell_of(key))
            .is_none_or(|state| state.active == 0);
        let new_slot = slot.is_empty();
        let (ranked, won_preferences) =
            self.rank_slot_preferences(&slot, key, candidate_cost_in_group);
        let admitted = ranked.last().copied().unwrap_or(true);
        let displaced: Vec<usize> = slot
            .iter()
            .copied()
            .zip(&ranked)
            .filter_map(|(id, retained)| (!retained).then_some(id))
            .collect();
        if !admitted {
            self.rejected = self.rejected.saturating_add(1);
            return Ok((None, key));
        }
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
                let tail_cost: u64 = suffix.iter().map(|action| (self.action_cost)(action)).sum();
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
                    &suffix,
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
            .checked_add(suffix.len())
            .ok_or("archive candidate input length overflow")?;
        self.historical_input_actions = self.historical_input_actions.saturating_add(input_len);
        self.stored_input_actions = self.stored_input_actions.saturating_add(suffix.len());
        let (input_node, new_nodes) = self.index_retained_input(parent_id, &suffix, stable_id)?;
        self.entries.push(ArchiveEntry {
            id: stable_id,
            parent_id: parent_id.map(|parent| self.entries[parent].id),
            created_execution: execution,
            input_suffix: suffix.clone(),
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
        let mut ancestor = parent_id;
        while let Some(current) = ancestor {
            let previous = self.deepest_leaf[current];
            if leaf_order(previous, (key, id)) != Ordering::Less {
                break;
            }
            self.update_index_deepest_leaf(current, previous, (key, id));
            self.deepest_leaf[current] = (key, id);
            ancestor = self.entries[current]
                .parent_id
                .and_then(|parent| self.id_to_index.get(&parent).copied());
        }
        self.activate_membership(id);
        let carried_in =
            parent_id.is_some_and(|parent| cell_of(self.entries[parent].key) != cell_of(key));
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
            .saturating_add(Self::history_entry_memory_charge(suffix.len(), new_nodes));
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
            .donors
            .get(&slot_of_key(parent_key))?
            .iter()
            .rev()
            .find_map(|rank| (rank.donor_id != parent).then_some(rank.donor_id))?;
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
        let place = self.draw_cell(rand, progress)?;
        let id = self.draw_holder(rand, progress, place)?;
        Ok((
            id,
            SelectorDraw {
                path: SelectorPath::Tiers,
                tier_rank: Some(rank),
            },
        ))
    }

    fn draw_tier(&self, rand: &mut RomuDuoJrRand) -> Result<(K::Progress, u8), Box<dyn Error>> {
        let shift = checked_tier_rank_shift(K::tier_rank_shift())?;
        let tiers = self.tiers.keys().rev().copied().collect::<Vec<_>>();
        let weights = (0..tiers.len())
            .map(|rank| tier_weight(u8::try_from(rank).unwrap_or(u8::MAX), shift))
            .collect::<Vec<_>>();
        let index = draw_weighted(rand, &weights)?;
        Ok((tiers[index], u8::try_from(index).unwrap_or(u8::MAX)))
    }

    fn draw_cell(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
    ) -> Result<K::Place, Box<dyn Error>> {
        let places = self
            .tiers
            .get(&progress)
            .ok_or("tier draw chose an absent tier")?;
        let candidates = places.keys().copied().collect::<Vec<_>>();
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
        let index = draw_weighted(rand, &weights)?;
        Ok(candidates[index])
    }

    fn draw_holder(
        &self,
        rand: &mut RomuDuoJrRand,
        progress: K::Progress,
        place: K::Place,
    ) -> Result<usize, Box<dyn Error>> {
        let members = self
            .tiers
            .get(&progress)
            .and_then(|places| places.get(&place))
            .ok_or("cell draw chose an absent cell")?;
        let ids = members.ids.iter().copied().collect::<Vec<_>>();
        let weights = ids
            .iter()
            .map(|id| count_decay(self.selected[*id]))
            .collect::<Vec<_>>();
        let index = draw_weighted(rand, &weights)?;
        Ok(ids[index])
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
        let state = self.cells.entry(cell_of(key)).or_default();
        state.draws = state.draws.saturating_add(1);
        state.draws_total = state.draws_total.saturating_add(1);
        match draw.path {
            SelectorPath::Continuation => {
                let count = self
                    .selector_accounting
                    .continuation_selections
                    .get_or_insert(0);
                *count = count.saturating_add(1);
            }
            SelectorPath::Tiers => {
                self.selector_accounting.cell_selections =
                    self.selector_accounting.cell_selections.saturating_add(1);
            }
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
                let held = (0..preferences)
                    .filter(|preference| self.champions_slot(slot, *id, *preference))
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
mod tests {
    use std::cmp::Ordering;

    use super::{
        ActiveIds, Archive, ArchiveCandidate, ArchiveKey, DonorRank, HISTORY_COMPACTION_MIN_DROPS,
        Input, InputIndex, MAINTENANCE_QUANTUM, MAX_ENTRIES_PER_KEY, MAX_TIER_RANK_SHIFT,
        SelectorAccounting, SelectorDraw, SelectorPath, checked_tier_rank_shift, tier_weight,
    };
    use crate::search::rand::RomuDuoJrRand;
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
    fn live_donor_index_tracks_a_new_deepest_descendant() {
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

        let donors = archive
            .donors
            .get(&super::slot_of_key(root_key))
            .expect("root slot donors");
        assert!(donors.contains(&DonorRank {
            leaf_key: child_key,
            leaf_id: child,
            donor_id: root,
        }));
        assert!(!donors.contains(&DonorRank {
            leaf_key: root_key,
            leaf_id: root,
            donor_id: root,
        }));
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
        let counts = draw_counts(&mut archive, 0x7ea1_0001, 4096);
        assert!(counts[2] > counts[1] && counts[1] > counts[0]);
        assert!(counts[0] > 0);
        let report = archive.selector_report();
        assert_eq!(report.tier_draws_by_rank.len(), 3);
        assert_eq!(report.tier_draws_by_rank.iter().sum::<u64>(), 4096);
    }

    #[test]
    fn the_progress_order_beats_the_key_order_when_choosing_the_top_tier() {
        let mut archive = tier_archive(&[(2, 1, 0), (1, 9, 0)]);
        let counts = draw_counts(&mut archive, 0x7ea1_0002, 1024);
        assert!(counts[0] > counts[1]);
    }

    #[test]
    fn cells_in_one_tier_share_draws_by_their_own_count() {
        let mut archive = tier_archive(&[(1, 1, 0), (1, 1, 0)]);
        archive.entries[1].key.region = [1, 0, 0];
        let key = archive.entries[1].key;
        archive.deactivate(1);
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
