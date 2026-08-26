// SPDX-License-Identifier: AGPL-3.0-or-later

//! Generic snapshot archive: retention, parent selection, and retire counters.
//!
//! The archive never names a game concept. Everything game-specific arrives
//! through [`ArchiveKey`]: the key locates a state, its groups pool entries
//! for selection and retirement, and its lineage carries whatever ancestry
//! the key needs to complete itself (for Super Mario Bros, the visited-room
//! list).

use std::{cmp::Reverse, collections::BTreeMap, error::Error, fmt::Debug, num::NonZeroUsize};

use crate::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

/// A quality-diversity archive key.
///
/// Group depths run from finest to coarsest: depth 0 is the retention slot
/// (the slot a candidate competes for under [`MAX_ENTRIES_PER_KEY`]), depth 1
/// is the selection cell the recency window samples inside, and higher depths
/// pool entries into ever-coarser selection classes up to `groups() - 1`, the
/// coarsest class whose deepest member starts the selection walk.
///
/// A key may declare any `groups() >= 1`. Selection reads the coarsest depth
/// as its walk class and `min(1, groups() - 1)` as its selection cell, so a
/// one-group key collapses class, cell, and retention slot onto depth 0 and a
/// two-group key collapses class onto the cell. Depths past the coarsest are
/// never read, and `group` is never called with a depth at or past `groups()`.
pub trait ArchiveKey: Copy + Ord + Serialize + DeserializeOwned {
    /// One pooled identity at some depth.
    type Group: Copy + Ord;
    /// Count of group depths, pinned by the recorded key policy. At least one.
    fn groups() -> usize;
    /// The key's pooled identity at `depth`; depth 0 is the retention slot.
    /// Only depths below [`groups`](Self::groups) are ever passed.
    fn group(self, depth: usize) -> Self::Group;
    /// Ancestry state a key needs to complete itself.
    type Lineage: Clone + Default;
    /// Complete a freshly decoded key against its parent's key and lineage.
    fn complete(self, parent: Option<(Self, &Self::Lineage)>) -> Self;
    /// Fold a completed key into a lineage.
    fn record(lineage: &mut Self::Lineage, key: Self);
}

/// Compiled ceiling on archive entries. A ceiling is not an allocation:
/// memory tracks actual retention, and a whole-tree resume inherits the
/// source population in full. At the ceiling the archive rejects every
/// admission, freezing the search, so the ceiling must exceed a full
/// campaign's retention. Campaign runs register their own per-run bound at
/// or below this.
pub const MAX_ARCHIVE_ENTRIES: usize = 4_194_304;
/// Entries one retention slot holds before candidates must displace.
pub const MAX_ENTRIES_PER_KEY: usize = 2;

/// One recorded input: actions in execution order.
#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Input<A: Ord> {
    /// Actions in execution order.
    pub actions: Vec<A>,
}

impl<A: Ord> Default for Input<A> {
    fn default() -> Self {
        Self {
            actions: Vec::new(),
        }
    }
}

/// Identifier recorded for the admission rule: the 45-frame probe under
/// three masks.
pub const RETENTION_IDENTIFIER: &str = "probe_at_admission_45";

/// Identifier recorded for the no-screening admission rule: an alive
/// endpoint is admitted under the normal slot rules and the probe never
/// runs.
pub const RETENTION_ADMIT_ALIVE_IDENTIFIER: &str = "admit_alive";

/// Per-run admission rule, recorded in the stream header; replay validates
/// under the recorded value. The probe mechanism itself is game-owned; the
/// generic layer only records and resolves the identifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetentionPolicy {
    /// Admit an alive endpoint only if one of the game's fixed input
    /// continuations survives the probe horizon from its snapshot.
    ProbeAtAdmission45,
    /// Admit every alive endpoint; the probe never runs on this path.
    AdmitAlive,
}

/// The recorded identifier of an admission rule.
#[must_use]
pub fn retention_policy_identifier(policy: RetentionPolicy) -> &'static str {
    match policy {
        RetentionPolicy::ProbeAtAdmission45 => RETENTION_IDENTIFIER,
        RetentionPolicy::AdmitAlive => RETENTION_ADMIT_ALIVE_IDENTIFIER,
    }
}

/// The admission rule a recorded identifier names.
///
/// # Errors
///
/// Returns an error when the identifier names no compiled admission rule.
pub fn retention_policy_from_identifier(
    identifier: &str,
) -> Result<RetentionPolicy, Box<dyn Error>> {
    match identifier {
        RETENTION_IDENTIFIER => Ok(RetentionPolicy::ProbeAtAdmission45),
        RETENTION_ADMIT_ALIVE_IDENTIFIER => Ok(RetentionPolicy::AdmitAlive),
        _ => Err(format!("retention policy {identifier} is not recognized").into()),
    }
}

/// Identifier recorded for the parent selector: the group walk from the
/// deepest coarsest class down to one selection cell, then the
/// recency-concentrated draw within it.
pub const SELECTOR_IDENTIFIER: &str = "room_cell_uniform_128";

/// Give-up thresholds for the retiring selector: consecutive barren draws at
/// which a class is skipped in selection exactly as exhausted classes are
/// skipped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RetireThresholds {
    /// One entry's own draws since its last retained descendant.
    pub entry: u64,
    /// Pooled thresholds for group depths `1..groups() - 1`, finest first.
    pub groups: Vec<u64>,
}

/// Per-run parent selector, recorded in the stream header; replay validates
/// under the recorded value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectorPolicy {
    /// The compiled group walk with the recency window.
    GroupUniform,
    /// The same walk with barren classes retired at the given thresholds.
    /// Retirement is soft: entries stay serialized and replayable, and the
    /// deterministic all-exhausted reset also clears the barren counters,
    /// so the search can never seal itself out.
    Retire(RetireThresholds),
}

/// The recorded identifier of a parent selector.
#[must_use]
pub fn selector_policy_identifier(policy: &SelectorPolicy) -> String {
    match policy {
        SelectorPolicy::GroupUniform => SELECTOR_IDENTIFIER.to_owned(),
        SelectorPolicy::Retire(thresholds) => {
            let values = std::iter::once(thresholds.entry)
                .chain(thresholds.groups.iter().copied())
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
                .join(",");
            format!("{SELECTOR_IDENTIFIER}_retire:{values}")
        }
    }
}

/// The parent selector a recorded identifier names, under a key with
/// `pooled_depths` pooled group depths (`groups() - 2`).
///
/// # Errors
///
/// Returns an error when the identifier names no compiled selector or its
/// thresholds do not parse, miscount, or contain a zero.
pub fn selector_policy_from_identifier(
    identifier: &str,
    pooled_depths: usize,
) -> Result<SelectorPolicy, Box<dyn Error>> {
    if identifier == SELECTOR_IDENTIFIER {
        return Ok(SelectorPolicy::GroupUniform);
    }
    let prefix = format!("{SELECTOR_IDENTIFIER}_retire:");
    let Some(values) = identifier.strip_prefix(&prefix) else {
        return Err(format!("parent selector {identifier} is not recognized").into());
    };
    let parsed = values
        .split(',')
        .map(|value| value.parse::<u64>())
        .collect::<Result<Vec<_>, _>>()?;
    if parsed.len() != pooled_depths + 1 {
        return Err(format!(
            "retiring selector needs exactly {} thresholds",
            pooled_depths + 1
        )
        .into());
    }
    if parsed.contains(&0) {
        return Err("retiring selector thresholds must be nonzero".into());
    }
    Ok(SelectorPolicy::Retire(RetireThresholds {
        entry: parsed[0],
        groups: parsed[1..].to_vec(),
    }))
}

/// Selections since the last retained descendant at which a parent is exhausted.
pub(crate) const SELECTION_EXHAUSTION_THRESHOLD: u64 = 64;

/// A concentrated cell draw samples only this many of the cell's
/// greatest-id members.
const CONCENTRATION_WINDOW: usize = 128;

/// Which selection path one recorded draw took.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorPath {
    /// The one-in-four uniform draw over all active entries.
    Uniform,
    /// The group walk: deepest coarsest class first, one unexhausted group
    /// chosen uniformly at each depth, then the concentrated recency draw
    /// within the chosen selection cell.
    RoomCellUniform,
}

/// One selector draw, recorded so selection-time state is checkable.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectorDraw {
    /// Path this draw took.
    pub path: SelectorPath,
    /// Fully exhausted classes skipped before this draw found its cell.
    pub classes_skipped: u64,
    /// Whether this draw found every active entry exhausted and reset the
    /// exhaustion counters.
    pub counter_reset: bool,
    /// Sampled-set state, present only on cell draws.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concentration: Option<ConcentrationDraw>,
}

/// Concentrated sampled-set state at one cell draw.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConcentrationDraw {
    /// Members of the concentrated sampled set at this draw.
    pub window_size: u64,
    /// Sampled-set members at this draw that were never members before.
    pub entered_window: u64,
}

/// Per-campaign accounting for the selector.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectorAccounting {
    /// Parent selections drawn through the uniform path.
    pub uniform_selections: u64,
    /// Parent selections drawn through the cell path.
    pub cell_selections: u64,
    /// Selections that produced at least one retained descendant.
    pub productive_selections: u64,
    /// Fully exhausted classes skipped across all draws.
    pub classes_skipped: u64,
    /// Deterministic all-exhausted counter resets.
    pub counter_resets: u64,
    /// Concentrated-window accounting.
    pub concentration: ConcentrationAccounting,
    /// Retirement accounting, present only under a retiring selector so
    /// reports recorded under the compiled selector keep their exact bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retirement: Option<RetirementAccounting>,
}

/// Retirement state at report time under a retiring selector.
///
/// Reports recorded before the archive went generic froze the three-depth
/// wire names `cells_over_threshold`, `bands_over_threshold`, and
/// `rooms_over_threshold`; a three-depth value keeps those exact bytes and
/// any other depth count serializes the vector directly. Both forms load.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RetirementAccounting {
    /// Entries whose own barren streak is at or over the entry threshold.
    pub entries_over_threshold: u64,
    /// Pooled classes at or over their depth's threshold, finest depth first.
    pub groups_over_threshold: Vec<u64>,
}

#[derive(Deserialize, Serialize)]
struct RetirementWireNamed {
    entries_over_threshold: u64,
    cells_over_threshold: u64,
    bands_over_threshold: u64,
    rooms_over_threshold: u64,
}

#[derive(Deserialize, Serialize)]
struct RetirementWireGeneric {
    entries_over_threshold: u64,
    groups_over_threshold: Vec<u64>,
}

impl Serialize for RetirementAccounting {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if let [cells, bands, rooms] = self.groups_over_threshold[..] {
            RetirementWireNamed {
                entries_over_threshold: self.entries_over_threshold,
                cells_over_threshold: cells,
                bands_over_threshold: bands,
                rooms_over_threshold: rooms,
            }
            .serialize(serializer)
        } else {
            RetirementWireGeneric {
                entries_over_threshold: self.entries_over_threshold,
                groups_over_threshold: self.groups_over_threshold.clone(),
            }
            .serialize(serializer)
        }
    }
}

impl<'de> Deserialize<'de> for RetirementAccounting {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Named(RetirementWireNamed),
            Generic(RetirementWireGeneric),
        }
        Ok(match Wire::deserialize(deserializer)? {
            Wire::Named(wire) => Self {
                entries_over_threshold: wire.entries_over_threshold,
                groups_over_threshold: vec![
                    wire.cells_over_threshold,
                    wire.bands_over_threshold,
                    wire.rooms_over_threshold,
                ],
            },
            Wire::Generic(wire) => Self {
                entries_over_threshold: wire.entries_over_threshold,
                groups_over_threshold: wire.groups_over_threshold,
            },
        })
    }
}

/// Per-campaign accounting for the concentrated recency window.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConcentrationAccounting {
    /// Fixed cap on the sampled set.
    pub window_cap: u64,
    /// Sampled-set size at the most recent cell draw.
    pub final_window_size: u64,
    /// Cell draws taken through the concentrated window.
    pub window_draws: u64,
    /// Distinct parents that were ever sampled-set members.
    pub distinct_window_parents: u64,
    /// Draws per parent through the window, in thousandths:
    /// `window_draws * 1000 / distinct_window_parents`, floored.
    pub draws_per_parent_milli: u64,
}

/// Deterministic progress sample from one archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(bound = "M: Serialize + DeserializeOwned")]
pub struct ProgressPoint<M> {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: M,
    /// Number of active retained archive entries.
    pub active_entries: usize,
    /// Number of occupied quality-diversity slots.
    pub occupied_cells: usize,
    /// Number of terminal death transitions seen so far.
    pub deaths: u64,
}

/// Per-entry selection counters.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct EntrySelectorCounters {
    /// Times this entry was selected as a parent.
    pub selected: u64,
    /// Selections of this entry that produced at least one retained descendant.
    pub productive: u64,
}

/// Serializable lineage and retention record for one archived testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(
    bound = "A: Serialize + DeserializeOwned + Ord + Clone, K: ArchiveKey, M: Serialize + \
                 DeserializeOwned + Clone"
)]
pub struct ArchiveEntryReport<A: Ord, K, M> {
    /// Stable insertion-order archive identifier.
    pub id: u64,
    /// Archive parent selected for the suffix execution.
    pub parent_id: Option<u64>,
    /// Target execution that created the entry; zero denotes bootstrap.
    pub created_execution: u64,
    /// Complete clean-reset input represented by this snapshot.
    pub input: Input<A>,
    /// Route-agnostic quality-diversity key.
    pub key: K,
    /// Strongest milestones observed along this input.
    pub milestones: M,
    /// Selection counters, absent on an entry the search has not run over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<EntrySelectorCounters>,
}

/// Serialized form of an entry list: every entry extends its parent, so the
/// actions past the parent's length identify the input once the parent is
/// rebuilt, at a small fraction of the size of the full input.
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

    /// Serialize entries with parent-relative input suffixes.
    ///
    /// # Errors
    ///
    /// Returns any serializer error.
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

    /// Rebuild full inputs from parent-relative suffixes.
    ///
    /// # Errors
    ///
    /// Returns a deserializer error for malformed entries.
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

/// One retained entry: its report and its machine snapshot.
#[derive(Clone, Debug)]
pub struct ArchiveEntry<A: Ord, K, M, S> {
    /// The serializable record.
    pub report: ArchiveEntryReport<A, K, M>,
    /// The retained machine snapshot.
    pub snapshot: S,
}

/// One candidate offered to retention.
pub struct ArchiveCandidate<A: Ord, K, M> {
    /// Complete clean-reset input of the candidate.
    pub input: Input<A>,
    /// Decoded key; its lineage-dependent parts are completed at insert.
    pub key: K,
    /// Strongest milestones observed along the input.
    pub milestones: M,
}

/// The generic snapshot archive.
pub struct Archive<A: Ord, K: ArchiveKey, M, S> {
    /// Retention stops when the entry count reaches this bound; campaign
    /// runs record their bound in the stream header and replay under it.
    pub max_entries: usize,
    /// Retained entries in insertion order.
    pub entries: Vec<ArchiveEntry<A, K, M, S>>,
    /// Whether each entry is still active (not displaced).
    pub active: Vec<bool>,
    /// Retention slots: active entry ids per depth-0 group.
    pub slots: BTreeMap<K::Group, Vec<usize>>,
    /// Archive id per already-retained input.
    pub input_ids: BTreeMap<Input<A>, usize>,
    /// Candidates admitted to the active archive.
    pub retained: u64,
    /// Candidates rejected by bounded quality-diversity retention.
    pub rejected: u64,
    selected: Vec<u64>,
    productive: Vec<u64>,
    since_retained: Vec<u64>,
    in_window_ever: Vec<bool>,
    selector_accounting: SelectorAccounting,
    /// Time each retained entry spent inside its own coarsest group, in
    /// entry-id order, in the game's action-duration unit.
    time_in_group: Vec<u64>,
    replacement_time_displaced: u64,
    /// Per-entry lineage, aligned with `entries`.
    lineages: Vec<K::Lineage>,
    /// Parent selector this archive selects under.
    pub selector_policy: SelectorPolicy,
    /// Pooled barren streak per group, one map per depth `1..groups() - 1`,
    /// finest first.
    group_barren: Vec<BTreeMap<K::Group, u64>>,
    /// Duration of one action in the cost unit the replacement rule uses.
    action_time: fn(&A) -> u64,
    /// Action bound the selector index was built for; `None` until the first
    /// selection. The index is derived state: rebuilding it from `entries`
    /// and `active` yields the same draws, so it is never serialized.
    frontier_cap: Option<usize>,
    /// Active entry ids under the frontier cap, ascending.
    active_list: Vec<usize>,
    /// Active entries under the cap, pooled by walk class (deepest first)
    /// and, inside each class, by selection cell.
    classes: BTreeMap<Reverse<K::Group>, ClassCells<K>>,
}

/// Members of one walk class, ascending per selection cell.
type ClassCells<K> = BTreeMap<<K as ArchiveKey>::Group, Vec<usize>>;

impl<A, K, M, S> Archive<A, K, M, S>
where
    A: Clone + Debug + Eq + Ord + Serialize + DeserializeOwned,
    K: ArchiveKey,
    M: Clone + Copy + Debug + Eq + Serialize + DeserializeOwned,
    S: Clone,
{
    /// An empty archive under the compiled selector.
    #[must_use]
    pub fn new(action_time: fn(&A) -> u64) -> Self {
        Self {
            max_entries: MAX_ARCHIVE_ENTRIES,
            entries: Vec::new(),
            active: Vec::new(),
            slots: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
            selected: Vec::new(),
            productive: Vec::new(),
            since_retained: Vec::new(),
            in_window_ever: Vec::new(),
            selector_accounting: SelectorAccounting {
                concentration: ConcentrationAccounting {
                    window_cap: u64::try_from(CONCENTRATION_WINDOW).unwrap_or(u64::MAX),
                    ..ConcentrationAccounting::default()
                },
                ..SelectorAccounting::default()
            },
            time_in_group: Vec::new(),
            replacement_time_displaced: 0,
            lineages: Vec::new(),
            selector_policy: SelectorPolicy::GroupUniform,
            group_barren: vec![BTreeMap::new(); K::groups().saturating_sub(2)],
            action_time,
            frontier_cap: None,
            active_list: Vec::new(),
            classes: BTreeMap::new(),
        }
    }

    /// The walk's class depth: the coarsest group.
    fn class_depth() -> usize {
        Self::coarsest_depth()
    }

    /// The selection cell's depth. Depth 1 whenever the key declares one,
    /// and the coarsest depth otherwise, so a key with fewer than two group
    /// depths still indexes and walks.
    fn cell_depth() -> usize {
        1.min(Self::coarsest_depth())
    }

    /// Rebuild the selector index for `max_actions`.
    fn rebuild_selector_index(&mut self, max_actions: usize) {
        self.frontier_cap = Some(max_actions);
        self.active_list = self.active_ids(max_actions);
        self.classes = BTreeMap::new();
        let depth = Self::class_depth();
        for id in &self.active_list {
            let key = self.entries[*id].report.key;
            self.classes
                .entry(Reverse(key.group(depth)))
                .or_default()
                .entry(key.group(Self::cell_depth()))
                .or_default()
                .push(*id);
        }
    }

    /// Add a fresh entry to the selector index. Ids only grow, so pushes
    /// keep every list ascending.
    fn index_insert(&mut self, id: usize) {
        let Some(cap) = self.frontier_cap else {
            return;
        };
        if self.entries[id].report.input.actions.len() >= cap {
            return;
        }
        self.active_list.push(id);
        let key = self.entries[id].report.key;
        self.classes
            .entry(Reverse(key.group(Self::class_depth())))
            .or_default()
            .entry(key.group(Self::cell_depth()))
            .or_default()
            .push(id);
    }

    /// Drop a displaced entry from the selector index.
    fn index_remove(&mut self, id: usize) {
        if self.frontier_cap.is_none() {
            return;
        }
        if let Ok(position) = self.active_list.binary_search(&id) {
            self.active_list.remove(position);
        }
        let key = self.entries[id].report.key;
        let class = Reverse(key.group(Self::class_depth()));
        let cell = key.group(Self::cell_depth());
        if let Some(cells) = self.classes.get_mut(&class) {
            if let Some(members) = cells.get_mut(&cell) {
                if let Ok(position) = members.binary_search(&id) {
                    members.remove(position);
                }
                if members.is_empty() {
                    cells.remove(&cell);
                }
            }
            if cells.is_empty() {
                self.classes.remove(&class);
            }
        }
    }

    /// The coarsest group depth: the replacement clock's group, the lineage
    /// inheritance boundary, and the walk's deepest-first level.
    fn coarsest_depth() -> usize {
        K::groups().saturating_sub(1)
    }

    /// An entry's lineage.
    #[must_use]
    pub fn lineage(&self, id: usize) -> Option<&K::Lineage> {
        self.lineages.get(id)
    }

    /// Slot collisions the time-in-group rule decided, counted for the report.
    #[must_use]
    pub fn replacement_time_displaced(&self) -> u64 {
        self.replacement_time_displaced
    }

    /// Time a retained entry spent inside its own coarsest group.
    #[cfg(test)]
    pub(crate) fn entry_time_in_group(&self, id: usize) -> u64 {
        self.time_in_group[id]
    }

    /// Deepest recorded key, the least time any entry with that key spent
    /// inside its coarsest group, and the retained total.
    ///
    /// Read-only. Nothing here consumes randomness or mutates archive state, so
    /// calling it cannot change what a run records.
    #[must_use]
    pub fn live_progress(&self) -> Option<(K, u64, u64)> {
        let deepest = self.entries.iter().map(|entry| entry.report.key).max()?;
        let cheapest = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.report.key == deepest)
            .map(|(index, _)| self.time_in_group.get(index).copied().unwrap_or(0))
            .min()
            .unwrap_or(0);
        Some((deepest, cheapest, self.retained))
    }

    /// Time a candidate spent inside its own coarsest group.
    ///
    /// An input extends its parent's, so the time added since the parent is
    /// the duration of the actions past the parent's length. A candidate
    /// whose parent already sits in the same coarsest group inherits the
    /// parent's count; one whose parent sits elsewhere entered the group
    /// during those actions and starts the count there. A candidate with no
    /// parent — genesis, and only genesis — counts its whole input.
    fn time_in_group_of(&self, parent_id: Option<usize>, input: &Input<A>, key: K) -> u64 {
        let time_of = |actions: &[A]| -> u64 {
            actions
                .iter()
                .map(|action| (self.action_time)(action))
                .sum()
        };
        let Some(parent) = parent_id.and_then(|id| self.entries.get(id)) else {
            return time_of(&input.actions);
        };
        let parent_actions = parent.report.input.actions.len();
        let added = time_of(input.actions.get(parent_actions..).unwrap_or(&[]));
        let depth = Self::coarsest_depth();
        if parent.report.key.group(depth) == key.group(depth) {
            self.time_in_group
                .get(parent_id.unwrap_or_default())
                .copied()
                .unwrap_or(0)
                .saturating_add(added)
        } else {
            added
        }
    }

    /// Offer a candidate to retention.
    ///
    /// # Errors
    ///
    /// Returns an error on id-space overflow or a missing parent.
    pub fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate<A, K, M>,
        snapshot: S,
    ) -> Result<Option<usize>, Box<dyn Error>> {
        let ArchiveCandidate {
            input,
            key,
            milestones,
        } = candidate;
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        if parent_id.is_some_and(|id| self.entries.get(id).is_none()) {
            return Err("archive candidate parent is missing".into());
        }
        let parent_ctx = parent_id.map(|id| (self.entries[id].report.key, &self.lineages[id]));
        let key = key.complete(parent_ctx);
        let depth = Self::coarsest_depth();
        let mut lineage = match parent_id {
            Some(id) if self.entries[id].report.key.group(depth) == key.group(depth) => {
                self.lineages[id].clone()
            }
            _ => K::Lineage::default(),
        };
        K::record(&mut lineage, key);
        let candidate_time_in_group = self.time_in_group_of(parent_id, &input, key);
        // The costliest entry in the group's own clock loses to a candidate
        // that reached the same slot in strictly less time. The entry id
        // breaks ties so the choice stays a total order over the slot.
        let slot = self.slots.entry(key.group(0)).or_default().clone();
        let slot_full = slot.len() >= MAX_ENTRIES_PER_KEY;
        let replace = if slot_full {
            slot.iter()
                .copied()
                .max_by_key(|id| (self.time_in_group[*id], self.entries[*id].report.id))
                .filter(|id| candidate_time_in_group < self.time_in_group[*id])
        } else {
            None
        };
        if slot_full && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= self.max_entries {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            if let Some(slot) = self.slots.get_mut(&key.group(0)) {
                slot.retain(|id| *id != replaced);
            }
            self.replacement_time_displaced = self.replacement_time_displaced.saturating_add(1);
            self.index_remove(replaced);
        }
        let id = self.entries.len();
        let report = ArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
            selector: None,
        };
        self.entries.push(ArchiveEntry { report, snapshot });
        self.active.push(true);
        self.lineages.push(lineage);
        self.time_in_group.push(candidate_time_in_group);
        self.selected.push(0);
        self.productive.push(0);
        self.since_retained.push(0);
        self.in_window_ever.push(false);
        self.slots.entry(key.group(0)).or_default().push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        self.index_insert(id);
        Ok(Some(id))
    }

    fn active_ids(&self, max_actions: usize) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| {
                (*active && self.entries[id].report.input.actions.len() < max_actions).then_some(id)
            })
            .collect()
    }

    /// Choose a parent: one in four draws is uniform over every expandable
    /// entry; the rest walk the group depths from the deepest coarsest class
    /// down to one selection cell, then sample the cell's recency window.
    /// When every entry is exhausted the exhaustion counters reset once and
    /// the draw repeats.
    ///
    /// # Errors
    ///
    /// Returns an error when no expandable entry exists, or when the
    /// deterministic reset frees nothing.
    pub fn select_parent(
        &mut self,
        rand: &mut RomuDuoJrRand,
        max_actions: usize,
    ) -> Result<(usize, SelectorDraw), Box<dyn Error>> {
        if self.frontier_cap != Some(max_actions) {
            self.rebuild_selector_index(max_actions);
        }
        if self.active_list.is_empty() {
            return Err("archive has no expandable entry".into());
        }
        let use_walk = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_walk {
            let count = NonZeroUsize::new(self.active_list.len()).ok_or("empty archive")?;
            let id = self.active_list[rand.below(count)];
            return Ok((
                id,
                SelectorDraw {
                    path: SelectorPath::Uniform,
                    classes_skipped: 0,
                    counter_reset: false,
                    concentration: None,
                },
            ));
        }
        let mut counter_reset = false;
        let mut classes_skipped = 0_u64;
        loop {
            if let Some(cell) = self.walk_to_cell(rand, &mut classes_skipped, counter_reset)? {
                let (id, concentration) = self.draw_from_cell(rand, cell)?;
                return Ok((
                    id,
                    SelectorDraw {
                        path: SelectorPath::RoomCellUniform,
                        classes_skipped,
                        counter_reset,
                        concentration: Some(concentration),
                    },
                ));
            }
            if counter_reset {
                return Err("selection counter reset freed no entry".into());
            }
            // The reset draw selects as if every streak counter were zero;
            // the durable clear happens when the reset-marked record is
            // applied, so counter state stays a pure function of the record
            // stream and live jobs still in flight at shutdown cannot leave
            // state replay never sees.
            counter_reset = true;
        }
    }

    /// Walk to one unexhausted selection cell. The coarsest classes are
    /// tried deepest first; within the first class holding an unexhausted
    /// entry, each finer group depth down to the cell is drawn uniformly
    /// among the groups still holding an unexhausted entry, so sparse deep
    /// groups carry the same weight as dense shallow ones. Exhausted groups
    /// at the pooled threshold depth are counted as skipped. `None` when
    /// every active entry is exhausted.
    fn walk_to_cell(
        &self,
        rand: &mut RomuDuoJrRand,
        classes_skipped: &mut u64,
        ignore_streaks: bool,
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let skip_depth = 2.min(Self::coarsest_depth());
        for cell_map in self.classes.values() {
            let mut cells = Vec::new();
            let mut subclass_live = BTreeMap::<K::Group, bool>::new();
            for members in cell_map.values() {
                // Every member of a cell shares its groups, so the pooled
                // barren thresholds are checked once per cell; only the
                // per-entry streak varies inside.
                let key = self.entries[members[0]].report.key;
                let group_live = ignore_streaks || self.groups_unexhausted(key);
                let live = if group_live {
                    members
                        .iter()
                        .copied()
                        .filter(|id| ignore_streaks || self.entry_unexhausted(*id))
                        .collect::<Vec<usize>>()
                } else {
                    Vec::new()
                };
                let subclass = subclass_live.entry(key.group(skip_depth)).or_insert(false);
                *subclass |= !live.is_empty();
                if !live.is_empty() {
                    cells.push((key, live));
                }
            }
            *classes_skipped = classes_skipped.saturating_add(
                u64::try_from(subclass_live.values().filter(|live| !**live).count())
                    .unwrap_or(u64::MAX),
            );
            if cells.is_empty() {
                continue;
            }
            for depth in (2..Self::coarsest_depth()).rev() {
                let mut groups = cells
                    .iter()
                    .map(|(key, _)| key.group(depth))
                    .collect::<Vec<_>>();
                groups.sort_unstable();
                groups.dedup();
                let count = NonZeroUsize::new(groups.len()).ok_or("group draw over no groups")?;
                let chosen = groups.swap_remove(rand.below(count));
                cells.retain(|(key, _)| key.group(depth) == chosen);
            }
            let count =
                NonZeroUsize::new(cells.len()).ok_or("cell draw over an exhausted class")?;
            return Ok(Some(cells.swap_remove(rand.below(count)).1));
        }
        Ok(None)
    }

    /// The per-entry half of the exhaustion rule.
    fn entry_unexhausted(&self, id: usize) -> bool {
        if self.since_retained[id] >= SELECTION_EXHAUSTION_THRESHOLD {
            return false;
        }
        match &self.selector_policy {
            SelectorPolicy::GroupUniform => true,
            SelectorPolicy::Retire(thresholds) => self.since_retained[id] < thresholds.entry,
        }
    }

    /// The pooled-group half of the exhaustion rule, shared by every member
    /// of a selection cell.
    fn groups_unexhausted(&self, key: K) -> bool {
        match &self.selector_policy {
            SelectorPolicy::GroupUniform => true,
            SelectorPolicy::Retire(thresholds) => {
                thresholds
                    .groups
                    .iter()
                    .enumerate()
                    .all(|(offset, threshold)| {
                        self.group_barren
                            .get(offset)
                            .and_then(|map| map.get(&key.group(offset + 1)))
                            .copied()
                            .unwrap_or(0)
                            < *threshold
                    })
            }
        }
    }

    /// Uniform draw within the chosen cell, narrowed to the cell's
    /// `CONCENTRATION_WINDOW` greatest-id members.
    ///
    /// Entry ids are creation order, so the greatest ids are the cell's most
    /// recently retained members. Membership is recomputed at every draw: a
    /// member leaves when `CONCENTRATION_WINDOW` newer sampleable cell
    /// members exist, or immediately when it exhausts.
    fn draw_from_cell(
        &mut self,
        rand: &mut RomuDuoJrRand,
        mut cell: Vec<usize>,
    ) -> Result<(usize, ConcentrationDraw), Box<dyn Error>> {
        cell.sort_unstable();
        let window = &cell[cell.len().saturating_sub(CONCENTRATION_WINDOW)..];
        let mut entered_window = 0_u64;
        for id in window {
            if !self.in_window_ever[*id] {
                self.in_window_ever[*id] = true;
                entered_window = entered_window.saturating_add(1);
            }
        }
        let id = window[rand.below(NonZeroUsize::new(window.len()).ok_or("empty tie window")?)];
        Ok((
            id,
            ConcentrationDraw {
                window_size: u64::try_from(window.len())?,
                entered_window,
            },
        ))
    }

    /// Account one recorded selection of `id`.
    pub fn record_selection(&mut self, id: usize, draw: &SelectorDraw) {
        // The reset-marked draw is the only place streak counters clear.
        // Applying it here, in stream order, keeps counter state a pure
        // function of the record stream, so live and replay agree at every
        // stream position. Retirement is soft: the reset also clears the
        // pooled barren counters, so the search can never seal itself out.
        if draw.counter_reset {
            for counter in &mut self.since_retained {
                *counter = 0;
            }
            for map in &mut self.group_barren {
                map.clear();
            }
        }
        self.selected[id] = self.selected[id].saturating_add(1);
        self.since_retained[id] = self.since_retained[id].saturating_add(1);
        if matches!(self.selector_policy, SelectorPolicy::Retire(_)) {
            let key = self.entries[id].report.key;
            for (offset, map) in self.group_barren.iter_mut().enumerate() {
                let counter = map.entry(key.group(offset + 1)).or_insert(0);
                *counter = counter.saturating_add(1);
            }
        }
        match draw.path {
            SelectorPath::Uniform => {
                self.selector_accounting.uniform_selections = self
                    .selector_accounting
                    .uniform_selections
                    .saturating_add(1);
            }
            SelectorPath::RoomCellUniform => {
                self.selector_accounting.cell_selections =
                    self.selector_accounting.cell_selections.saturating_add(1);
            }
        }
        self.selector_accounting.classes_skipped = self
            .selector_accounting
            .classes_skipped
            .saturating_add(draw.classes_skipped);
        self.selector_accounting.counter_resets = self
            .selector_accounting
            .counter_resets
            .saturating_add(u64::from(draw.counter_reset));
        if let Some(concentration) = draw.concentration.as_ref() {
            let accounting = &mut self.selector_accounting.concentration;
            accounting.window_draws = accounting.window_draws.saturating_add(1);
            accounting.final_window_size = concentration.window_size;
            accounting.distinct_window_parents = accounting
                .distinct_window_parents
                .saturating_add(concentration.entered_window);
            accounting.draws_per_parent_milli = accounting
                .window_draws
                .saturating_mul(1000)
                .checked_div(accounting.distinct_window_parents)
                .unwrap_or(0);
        }
    }

    /// Account one selection's discovery outcome.
    pub fn record_selection_outcome(&mut self, id: usize, retained_descendant: bool) {
        if !retained_descendant {
            return;
        }
        self.productive[id] = self.productive[id].saturating_add(1);
        self.since_retained[id] = 0;
        if matches!(self.selector_policy, SelectorPolicy::Retire(_)) {
            let key = self.entries[id].report.key;
            for (offset, map) in self.group_barren.iter_mut().enumerate() {
                map.insert(key.group(offset + 1), 0);
            }
        }
        self.selector_accounting.productive_selections = self
            .selector_accounting
            .productive_selections
            .saturating_add(1);
    }

    /// The per-campaign selector accounting for the report.
    #[must_use]
    pub fn selector_report(&self) -> SelectorAccounting {
        let mut accounting = self.selector_accounting.clone();
        if let SelectorPolicy::Retire(thresholds) = &self.selector_policy {
            let entries_over_threshold = u64::try_from(
                self.since_retained
                    .iter()
                    .zip(&self.active)
                    .filter(|(streak, active)| **active && **streak >= thresholds.entry)
                    .count(),
            )
            .unwrap_or(u64::MAX);
            let groups_over_threshold = thresholds
                .groups
                .iter()
                .enumerate()
                .map(|(offset, threshold)| {
                    self.group_barren
                        .get(offset)
                        .map(|map| {
                            u64::try_from(
                                map.values().filter(|streak| **streak >= *threshold).count(),
                            )
                            .unwrap_or(u64::MAX)
                        })
                        .unwrap_or(0)
                })
                .collect();
            accounting.retirement = Some(RetirementAccounting {
                entries_over_threshold,
                groups_over_threshold,
            });
        }
        accounting
    }

    /// Clone the entry reports, stamping per-entry selection counters.
    #[must_use]
    pub fn entry_reports_snapshot(&self) -> Vec<ArchiveEntryReport<A, K, M>> {
        self.entries
            .iter()
            .enumerate()
            .map(|(id, entry)| {
                let mut report = entry.report.clone();
                report.selector = Some(EntrySelectorCounters {
                    selected: self.selected[id],
                    productive: self.productive[id],
                });
                report
            })
            .collect()
    }

    /// Extract the entry reports, stamping per-entry selection counters.
    pub fn take_entry_reports(&mut self) -> Vec<ArchiveEntryReport<A, K, M>> {
        std::mem::take(&mut self.entries)
            .into_iter()
            .enumerate()
            .map(|(id, entry)| {
                let mut report = entry.report;
                report.selector = Some(EntrySelectorCounters {
                    selected: self.selected[id],
                    productive: self.productive[id],
                });
                report
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, ArchiveCandidate, ArchiveKey, Input, RetireThresholds,
        SELECTION_EXHAUSTION_THRESHOLD, SelectorDraw, SelectorPath, SelectorPolicy,
        selector_policy_from_identifier,
    };
    use crate::search::rand::RomuDuoJrRand;
    use crate::smb::archive::{MAX_SMB_COMPLETION_ACTIONS, SmbArchiveKey};
    use crate::smb::target::ButtonChord;
    use serde::{Deserialize, Serialize};

    type TestArchive = Archive<u8, SmbArchiveKey, (), ()>;

    /// A key of exactly `DEPTHS` group depths over four components, for
    /// covering geometries no compiled game declares. Depth `d` erases the
    /// finest `d` components, and `group` asserts its depth is in range so a
    /// selector that reads past the coarsest depth fails the test rather
    /// than returning a plausible group.
    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct FlatKey<const DEPTHS: usize>([u16; 4]);

    impl<const DEPTHS: usize> ArchiveKey for FlatKey<DEPTHS> {
        type Group = [u16; 4];

        fn groups() -> usize {
            DEPTHS
        }

        fn group(self, depth: usize) -> Self::Group {
            assert!(depth < DEPTHS, "group depth {depth} is past {DEPTHS} depths");
            let mut group = self.0;
            for component in group.iter_mut().take(depth) {
                *component = 0;
            }
            group
        }

        type Lineage = ();

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(_lineage: &mut Self::Lineage, _key: Self) {}
    }

    fn flat_archive<const DEPTHS: usize>(
        keys: &[[u16; 4]],
    ) -> Archive<u8, FlatKey<DEPTHS>, (), ()> {
        let mut archive = Archive::<u8, FlatKey<DEPTHS>, (), ()>::new(|_| 1);
        for (index, components) in keys.iter().enumerate() {
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input: Input {
                            actions: vec![u8::try_from(index).expect("input byte")],
                        },
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

    /// Selection must run on every geometry the key contract allows, down to
    /// a single group depth where the walk class, the selection cell, and the
    /// retention slot are all depth 0.
    #[test]
    fn selection_runs_on_keys_with_fewer_than_three_group_depths() {
        fn draws<const DEPTHS: usize>(seed: u64) {
            let keys = [[1, 2, 3, 4], [1, 2, 3, 5], [9, 8, 7, 4]];
            let mut archive = flat_archive::<DEPTHS>(&keys);
            let mut rand = RomuDuoJrRand::with_seed(seed);
            for _ in 0..128 {
                let (id, draw) = archive
                    .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                    .expect("selection under a shallow key");
                assert!(id < keys.len());
                archive.record_selection(id, &draw);
            }
        }
        draws::<1>(0x5eed_0001);
        draws::<2>(0x5eed_0002);
        draws::<3>(0x5eed_0003);
    }

    /// The retiring selector takes one threshold per pooled depth plus the
    /// per-entry threshold. A key with fewer than three depths pools nothing,
    /// so the identifier carries the entry threshold alone.
    #[test]
    fn the_retiring_selector_parses_under_a_shallow_key() {
        for depths in [1_usize, 2] {
            let pooled = depths.saturating_sub(2);
            let policy = selector_policy_from_identifier("room_cell_uniform_128_retire:3", pooled)
                .expect("shallow retiring selector");
            assert_eq!(
                policy,
                SelectorPolicy::Retire(RetireThresholds {
                    entry: 3,
                    groups: Vec::new(),
                })
            );
            assert!(
                selector_policy_from_identifier("room_cell_uniform_128_retire:3,6", pooled).is_err()
            );
        }
    }

    fn selector_archive(keys: &[(u8, u8, u16)]) -> TestArchive {
        let mut archive = TestArchive::new(|_| 1);
        for (index, (world, level, progress)) in keys.iter().enumerate() {
            let input = Input {
                actions: vec![
                    u8::try_from(index / 256).expect("input byte"),
                    u8::try_from(index % 256).expect("input byte"),
                ],
            };
            let key = SmbArchiveKey {
                world: *world,
                level: *level,
                progress: *progress,
                player_y_bucket: u8::try_from(index / 64).expect("vertical bucket"),
                player_engine_state: 0,
                state_fingerprint: u8::try_from(index % 64).expect("fingerprint"),
                room_x_bucket: 0,
                room: [0; 3],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input,
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("insert selector entry")
                .expect("retain selector entry");
        }
        archive
    }

    #[test]
    fn the_walk_splits_a_class_over_its_unexhausted_cells() {
        // Two bands of one 8-4 room: 60 entries at (304, y 11) fill band 38
        // alone; 3 at (303, y 7) and 1 at (300, y 4) share band 37. The
        // band draw is uniform, so the crowded cell gets half the draws
        // despite holding 60 of 64 entries, and the band-37 cells split the
        // other half evenly.
        let mut keys: Vec<(u8, u8, u16)> = Vec::new();
        keys.extend(std::iter::repeat_n((7, 3, 304), 60));
        keys.extend(std::iter::repeat_n((7, 3, 303), 3));
        keys.push((7, 3, 300));
        let mut archive = selector_archive(&keys);
        for (index, entry) in archive.entries.iter_mut().enumerate() {
            entry.report.key.room = [3, 5, 16];
            entry.report.key.player_y_bucket = match index {
                0..=59 => 11,
                60..=62 => 7,
                _ => 4,
            };
        }
        let mut rand = RomuDuoJrRand::with_seed(0xce11_0000);
        let mut per_cell = std::collections::BTreeMap::<(u16, u8), u64>::new();
        let mut cell_draws = 0_u64;
        for _ in 0..900 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("walk selection");
            if draw.path != SelectorPath::RoomCellUniform {
                continue;
            }
            cell_draws += 1;
            let key = archive.entries[id].report.key;
            *per_cell
                .entry((key.progress, key.player_y_bucket))
                .or_default() += 1;
            archive.record_selection(id, &draw);
            archive.record_selection_outcome(id, true);
        }
        assert!(cell_draws > 600);
        assert_eq!(per_cell.len(), 3, "cells drawn: {per_cell:?}");
        let crowded = per_cell[&(304, 11)];
        assert!(
            crowded * 5 > cell_draws * 2 && crowded * 5 < cell_draws * 3,
            "crowded cell off its half share: {per_cell:?}"
        );
        for cell in [(303, 7), (300, 4)] {
            let share = per_cell[&cell];
            assert!(
                share * 5 > cell_draws && share * 5 < cell_draws * 2,
                "band-37 cell off its quarter share: {per_cell:?}"
            );
        }
    }

    #[test]
    fn a_pooled_barren_class_is_retired_and_the_reset_frees_it() {
        // Two cells in one band at (1, 0) plus one band below. A single
        // barren draw of entry 0 puts the whole band over a threshold of
        // one, so cell draws must fall through to the lower band even
        // though entry 1 was never drawn.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 145), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        archive.selector_policy = SelectorPolicy::Retire(RetireThresholds {
            entry: 64,
            groups: vec![64, 1, 64],
        });
        let barren_draw = SelectorDraw {
            path: SelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        archive.record_selection(0, &barren_draw);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e30);
        let mut fell_through = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SelectorPath::RoomCellUniform {
                assert_eq!(id, 2, "cell draws must fall through to the 124 band");
                assert_eq!(draw.classes_skipped, 1);
                assert!(!draw.counter_reset);
                fell_through += 1;
            }
        }
        assert!(fell_through > 0);
        // A retained descendant of the lower band's entry resets nothing in
        // the retired band; a retained descendant of entry 1 clears the
        // pooled counter and the band returns to selection.
        archive.record_selection(1, &barren_draw);
        archive.record_selection_outcome(1, true);
        let mut upper_band_seen = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection after reset");
            if draw.path == SelectorPath::RoomCellUniform && (id == 0 || id == 1) {
                upper_band_seen = true;
            }
        }
        assert!(
            upper_band_seen,
            "a keeper must return its band to selection"
        );
        let accounting = archive.selector_report();
        let retirement = accounting.retirement.expect("retirement accounting");
        assert_eq!(retirement.groups_over_threshold[1], 0);
    }

    #[test]
    fn a_retired_coarse_class_falls_to_the_reset_when_nothing_else_lives() {
        // One room only: a single barren draw retires it at a room
        // threshold of one, and the deterministic all-exhausted reset must
        // clear the pooled counters and free it rather than seal the search.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.room = [3, 5, 0];
        }
        archive.selector_policy = SelectorPolicy::Retire(RetireThresholds {
            entry: 64,
            groups: vec![64, 64, 1],
        });
        let barren_draw = SelectorDraw {
            path: SelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        archive.record_selection(0, &barren_draw);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e31);
        let mut reset_seen = false;
        for _ in 0..64 {
            let (_, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection under a retired class");
            if draw.path == SelectorPath::RoomCellUniform {
                if draw.counter_reset {
                    reset_seen = true;
                    break;
                }
                panic!("a cell draw before the reset must not reach a retired class");
            }
        }
        assert!(reset_seen, "the all-exhausted reset must free the class");
    }

    #[test]
    fn the_selector_starves_exhausted_parents_and_falls_through() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124), (1, 0, 123), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SelectorDraw {
            path: SelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
            archive.record_selection(0, &exhausting_draw);
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e1f);
        let mut fell_through = 0;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SelectorPath::RoomCellUniform {
                fell_through += 1;
                assert!(
                    id == 1 || id == 2,
                    "cell draws must fall through to the 124 band"
                );
                assert_eq!(draw.classes_skipped, 1);
                assert!(!draw.counter_reset);
            }
        }
        assert!(fell_through > 0);
        assert_eq!(
            archive.selector_report().cell_selections,
            SELECTION_EXHAUSTION_THRESHOLD
        );
    }

    #[test]
    fn the_selector_resets_deterministically_when_all_are_exhausted() {
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (0, 0, 100)];
        let mut archive = selector_archive(&keys);
        let exhausting_draw = SelectorDraw {
            path: SelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for id in 0..keys.len() {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e20);
        let mut reset_seen = false;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection");
            if draw.path == SelectorPath::RoomCellUniform {
                assert!(
                    draw.counter_reset,
                    "the first cell draw after full exhaustion must reset"
                );
                assert_eq!(draw.classes_skipped, 2);
                assert_eq!(id, 0);
                archive.record_selection(id, &draw);
                reset_seen = true;
                break;
            }
        }
        assert!(reset_seen);
        assert_eq!(archive.selector_report().counter_resets, 1);
    }

    #[test]
    fn the_cell_draw_samples_only_the_recency_window() {
        // 140 entries in one cell: the window is the 128 greatest ids.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 124); 140];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.player_y_bucket = 0;
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e21);
        let mut cell_draws = 0;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            match draw.path {
                SelectorPath::RoomCellUniform => {
                    cell_draws += 1;
                    assert!(
                        id >= 12,
                        "cell draws must come from the 128 most recent members, got {id}"
                    );
                    let concentration = draw.concentration.expect("concentration record");
                    assert_eq!(concentration.window_size, 128);
                }
                SelectorPath::Uniform => {
                    assert!(draw.concentration.is_none());
                }
            }
        }
        assert!(cell_draws > 0);
    }

    #[test]
    fn concentrated_window_slides_off_exhausted_members() {
        // 129 members at one progress: the window starts as ids 1..=128; when
        // all of them exhaust, the sampled set must refill from the
        // next-most-recent unexhausted member below, not skip the cell.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 124); 129];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.report.key.player_y_bucket = 0;
        }
        let exhausting_draw = SelectorDraw {
            path: SelectorPath::RoomCellUniform,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        for id in 1..=128 {
            for _ in 0..SELECTION_EXHAUSTION_THRESHOLD {
                archive.record_selection(id, &exhausting_draw);
            }
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e22);
        let mut slid = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            if draw.path == SelectorPath::RoomCellUniform {
                assert_eq!(id, 0, "the only unexhausted member must be sampled");
                assert_eq!(draw.classes_skipped, 0);
                assert!(!draw.counter_reset);
                let concentration = draw.concentration.expect("concentration record");
                assert_eq!(concentration.window_size, 1);
                slid = true;
            }
        }
        assert!(slid);
    }

    type ChordArchive = Archive<ButtonChord, SmbArchiveKey, (), ()>;

    fn probe_key(world: u8, level: u8, progress: u16, vertical: u8) -> SmbArchiveKey {
        SmbArchiveKey {
            world,
            level,
            progress,
            player_y_bucket: vertical,
            player_engine_state: 0,
            state_fingerprint: 0,
            room_x_bucket: 0,
            room: [0; 3],
        }
    }

    /// Insert one action onto a parent and report the new entry's identifier.
    fn chain_insert(
        archive: &mut ChordArchive,
        parent: Option<usize>,
        prefix: &Input<ButtonChord>,
        buttons: u8,
        hold: u8,
        key: SmbArchiveKey,
    ) -> (Option<usize>, Input<ButtonChord>) {
        let mut input = prefix.clone();
        input.actions.push(ButtonChord::new(buttons, hold));
        let id = archive
            .insert(
                parent,
                0,
                ArchiveCandidate {
                    input: input.clone(),
                    key,
                    milestones: (),
                },
                (),
            )
            .expect("chained insert");
        (id, input)
    }

    #[test]
    fn time_in_group_counts_from_the_recorded_coarse_transition() {
        let mut archive = ChordArchive::new(crate::smb::archive::chord_time);
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: Input::default(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: (),
                },
                (),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        assert_eq!(archive.entry_time_in_group(genesis), 0);
        // Two actions inside the genesis pair accumulate their held frames.
        let (first, input) = chain_insert(
            &mut archive,
            Some(genesis),
            &Input::default(),
            0x01,
            30,
            probe_key(0, 0, 4, 0),
        );
        let first = first.expect("first retained");
        assert_eq!(archive.entry_time_in_group(first), 30);
        let (second, input) = chain_insert(
            &mut archive,
            Some(first),
            &input,
            0x01,
            20,
            probe_key(0, 0, 8, 0),
        );
        let second = second.expect("second retained");
        assert_eq!(archive.entry_time_in_group(second), 50);
        // Crossing into the next pair restarts the count at the crossing
        // action, and the next action inside the new pair adds to that.
        let (crossed, input) = chain_insert(
            &mut archive,
            Some(second),
            &input,
            0x01,
            40,
            probe_key(0, 1, 2, 0),
        );
        let crossed = crossed.expect("crossing retained");
        assert_eq!(archive.entry_time_in_group(crossed), 40);
        let (after, _) = chain_insert(
            &mut archive,
            Some(crossed),
            &input,
            0x01,
            10,
            probe_key(0, 1, 6, 0),
        );
        assert_eq!(archive.entry_time_in_group(after.expect("retained")), 50);
    }

    #[test]
    fn the_time_rule_displaces_a_slower_route_into_a_full_slot() {
        let slot = probe_key(0, 0, 16, 0);
        // Three routes into one slot. The first two are short in actions and
        // long in frames; the third is longer in actions and much shorter in
        // frames, which is exactly the collision the group clock cares about.
        let mut archive = ChordArchive::new(crate::smb::archive::chord_time);
        let genesis = archive
            .insert(
                None,
                0,
                ArchiveCandidate {
                    input: Input::default(),
                    key: probe_key(0, 0, 0, 0),
                    milestones: (),
                },
                (),
            )
            .expect("genesis insert")
            .expect("genesis retained");
        for buttons in [0x01_u8, 0x02] {
            chain_insert(
                &mut archive,
                Some(genesis),
                &Input::default(),
                buttons,
                120,
                slot,
            );
        }
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
            .expect("the eleven-frame route displaces a slower one");
        assert_eq!(archive.entry_time_in_group(admitted), 11);
        assert_eq!(archive.replacement_time_displaced(), 1);
        let (slower, _) = chain_insert(&mut archive, fast, &input, 0x04, 200, slot);
        assert!(
            slower.is_none(),
            "a slower route never displaces a faster one"
        );
    }
}
