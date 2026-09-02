// SPDX-License-Identifier: AGPL-3.0-or-later

//! Generic snapshot archive: retention, parent selection, and retire counters.
//!
//! The archive never names a game concept. Everything game-specific arrives
//! through [`ArchiveKey`]: the key locates a state, its groups pool entries
//! for selection and retirement, and its lineage carries whatever ancestry
//! the key needs to complete itself (for Super Mario Bros, the visited-room
//! list).

use std::{
    cell::Cell,
    cmp::Reverse,
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    fmt::Debug,
    mem::size_of,
    num::NonZeroUsize,
    sync::Arc,
};

use crate::search::rand::RomuDuoJrRand;
use serde::{Deserialize, Serialize, de::DeserializeOwned};

fn retain_marked<T>(values: Vec<T>, keep: &[bool]) -> Vec<T> {
    values
        .into_iter()
        .zip(keep)
        .filter_map(|(value, keep)| (*keep).then_some(value))
        .collect()
}

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
/// Dead metadata reclaimed per compaction. A fixed batch bounds physical
/// slack while keeping the linear pack operation off the per-job hot path.
#[cfg(not(test))]
const HISTORY_COMPACTION_MIN_DROPS: usize = 4_096;
// Keep the same threshold semantics while making corruption of compaction
// control flow fail well inside the mutation-test timeout.
#[cfg(test)]
const HISTORY_COMPACTION_MIN_DROPS: usize = 16;

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
/// recency-concentrated draw within it. The string is pinned by every stream
/// already written.
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
    /// The same walk with barren groups down-weighted instead of retired: a
    /// group's draw weight halves every `scale` barren selections at its
    /// depth and floors at 1/256 of a fresh group, so a hard frontier keeps
    /// receiving draws instead of falling back to shallower classes. The
    /// entry threshold still retires single entries hard.
    Energy(RetireThresholds),
    /// The energy walk with a second weight factor biasing each pooled group
    /// draw toward the frontier: a group's weight also halves for each rank
    /// it sits below the deepest (greatest) group value at its depth, with a
    /// floor of 1 so shallow groups keep a live tail. Both factors multiply;
    /// the entry threshold still retires single entries hard.
    EnergyFrontier(RetireThresholds),
    /// The frontier walk with a cost-weighted cell draw: within the recency
    /// window, entries are ranked by frames spent in their group, and an
    /// entry's weight halves per `CHEAPEST_RANK_SCALE` ranks above the
    /// cheapest, flooring at 1. Cheap entries hold the most unspent budget
    /// under any workload clock, so they take most of the cell's draws.
    EnergyFrontierCheapest(RetireThresholds),
}

/// The recorded identifier of a parent selector.
#[must_use]
pub fn selector_policy_identifier(policy: &SelectorPolicy) -> String {
    match policy {
        SelectorPolicy::GroupUniform => SELECTOR_IDENTIFIER.to_owned(),
        SelectorPolicy::Retire(thresholds) => {
            format!(
                "{SELECTOR_IDENTIFIER}_retire:{}",
                threshold_values(thresholds)
            )
        }
        SelectorPolicy::Energy(scales) => {
            format!("{SELECTOR_IDENTIFIER}_energy:{}", threshold_values(scales))
        }
        SelectorPolicy::EnergyFrontier(scales) => {
            format!(
                "{SELECTOR_IDENTIFIER}_energy_frontier:{}",
                threshold_values(scales)
            )
        }
        SelectorPolicy::EnergyFrontierCheapest(scales) => {
            format!(
                "{SELECTOR_IDENTIFIER}_energy_frontier_cheapest:{}",
                threshold_values(scales)
            )
        }
    }
}

fn threshold_values(thresholds: &RetireThresholds) -> String {
    std::iter::once(thresholds.entry)
        .chain(thresholds.groups.iter().copied())
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join(",")
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
    let retire_prefix = format!("{SELECTOR_IDENTIFIER}_retire:");
    let energy_prefix = format!("{SELECTOR_IDENTIFIER}_energy:");
    let frontier_prefix = format!("{SELECTOR_IDENTIFIER}_energy_frontier:");
    let cheapest_prefix = format!("{SELECTOR_IDENTIFIER}_energy_frontier_cheapest:");
    enum Parsed {
        Retire,
        Energy,
        EnergyFrontier,
        EnergyFrontierCheapest,
    }
    let (values, selector) = if let Some(values) = identifier.strip_prefix(&retire_prefix) {
        (values, Parsed::Retire)
    } else if let Some(values) = identifier.strip_prefix(&cheapest_prefix) {
        (values, Parsed::EnergyFrontierCheapest)
    } else if let Some(values) = identifier.strip_prefix(&frontier_prefix) {
        (values, Parsed::EnergyFrontier)
    } else if let Some(values) = identifier.strip_prefix(&energy_prefix) {
        (values, Parsed::Energy)
    } else {
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
    let thresholds = RetireThresholds {
        entry: parsed[0],
        groups: parsed[1..].to_vec(),
    };
    Ok(match selector {
        Parsed::Retire => SelectorPolicy::Retire(thresholds),
        Parsed::Energy => SelectorPolicy::Energy(thresholds),
        Parsed::EnergyFrontier => SelectorPolicy::EnergyFrontier(thresholds),
        Parsed::EnergyFrontierCheapest => SelectorPolicy::EnergyFrontierCheapest(thresholds),
    })
}

/// Selections since the last retained descendant at which a parent is exhausted.
pub(crate) const SELECTION_EXHAUSTION_THRESHOLD: u64 = 64;

/// A concentrated cell draw samples only this many of the cell's
/// greatest-id members.
const CONCENTRATION_WINDOW: usize = 128;

/// Cost ranks per halving of a cell entry's draw weight under the cheapest
/// concentration, and of a cell's draw weight within its band by its
/// cheapest offered member, so a band's draws follow the routes that reach
/// it with the most time left; a compiled property of the recorded
/// identifier, like the window itself.
const CHEAPEST_RANK_SCALE: usize = 4;

/// Selections after which a cell stops counting as new and competes on
/// energy and cost alone, so a fresh cell that keeps failing cannot hold
/// the front of its band's novelty order.
const CELL_NOVELTY_DRAWS: u64 = 4;

/// Newer live cells per halving of a cell's draw weight within its band
/// under the energy frontier selectors, so a cell opened moments ago is
/// tried before the band's older cells dilute it.
const CELL_NOVELTY_RANK_SCALE: usize = 8;

/// Which selection path one recorded draw took.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectorPath {
    /// The one-in-four uniform draw over all active entries.
    Uniform,
    /// The group walk: deepest coarsest class first, one unexhausted group
    /// chosen uniformly at each depth, then the concentrated recency draw
    /// within the chosen selection cell. The recorded value is pinned by
    /// every stream already written.
    #[serde(rename = "room_cell_uniform")]
    GroupWalk,
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
    #[serde(default, alias = "tie_class_selections")]
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
#[serde(bound = "M: Serialize + DeserializeOwned, P: Serialize + DeserializeOwned")]
pub struct ProgressPoint<M, P = ()> {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: M,
    /// Strongest route-agnostic mechanical progress observed so far.
    ///
    /// The default keeps reports written before mechanical progress was added
    /// readable without assigning them target-specific meaning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<P>,
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

/// One retained entry in compact live form.
///
/// Full inputs are a reporting surface. The live archive stores only the
/// actions added after the parent so historical lineage remains compact.
#[derive(Clone, Debug)]
pub(crate) struct ArchiveEntry<A: Ord, K, M, S> {
    pub(crate) id: u64,
    pub(crate) parent_id: Option<u64>,
    pub(crate) created_execution: u64,
    pub(crate) input_suffix: Vec<A>,
    pub(crate) input_len: usize,
    /// Leaf in the live prefix tree. Unlike `parent_id`, this path remains
    /// self-contained when older archive metadata is compacted away.
    input_node: usize,
    pub(crate) key: K,
    pub(crate) milestones: M,
    /// The restorable machine snapshot while this entry can still be selected.
    ///
    /// Replaced entries keep their immutable report but release this payload;
    /// already-dispatched jobs own their own [`Arc`] and remain unaffected.
    pub(crate) snapshot: Option<Arc<S>>,
}

/// One dispatch-time splice resolution and the exact tail it selected.
pub(crate) struct CampaignSpliceTail<A> {
    pub(crate) donor_id: usize,
    pub(crate) leaf_id: usize,
    pub(crate) actions: Vec<A>,
}

/// One candidate offered to retention.
pub struct ArchiveCandidate<A: Ord, K, M> {
    /// Actions added after the named parent (the complete input for genesis).
    pub suffix: Vec<A>,
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
    pub(crate) entries: Vec<ArchiveEntry<A, K, M, S>>,
    /// Stable stream id to current compact in-memory slot.
    id_to_index: BTreeMap<u64, usize>,
    /// Next stable stream id. Slots may be compacted; this id never rewinds.
    next_entry_id: u64,
    /// Whether each entry is still active (not displaced).
    pub active: Vec<bool>,
    /// Number of active retained entries, maintained as admissions displace
    /// and append so progress reporting never scans historical entries.
    active_count: usize,
    /// Retention slots: active entry ids per depth-0 group.
    pub slots: BTreeMap<K::Group, Vec<usize>>,
    /// Prefix-sharing index of already-retained inputs.
    input_index: InputIndex<A>,
    /// Action elements the former complete per-entry inputs would have held.
    /// This diagnostic counter owns no actions and never scans history.
    historical_input_actions: usize,
    /// Action elements physically retained as parent-relative suffixes.
    stored_input_actions: usize,
    /// Deterministic conservative charge for compact history and indexes.
    history_memory_bytes: usize,
    /// Candidates admitted to the active archive.
    pub retained: u64,
    /// Candidates rejected by bounded quality-diversity retention.
    pub rejected: u64,
    selected: Vec<u64>,
    productive: Vec<u64>,
    since_retained: Vec<u64>,
    in_window_ever: Vec<bool>,
    /// Per entry: whether its retention slot was empty when it arrived.
    opened_slot: Vec<bool>,
    opened_cell: Vec<bool>,
    cells_seen: BTreeSet<K::Group>,
    selector_accounting: SelectorAccounting,
    /// Time each retained entry spent inside its own coarsest group, in
    /// entry-id order, in the game's action-duration unit.
    time_in_group: Vec<u64>,
    replacement_time_displaced: u64,
    /// Per-entry lineage, aligned with `entries`.
    lineages: Vec<K::Lineage>,
    /// Per entry: the greatest key in its subtree and the entry holding it,
    /// maintained on insert so splice draws find a stored tail without a
    /// scan.
    deepest_leaf: Vec<(K, usize)>,
    /// Parent selector this archive selects under.
    pub selector_policy: SelectorPolicy,
    /// Pooled barren streak per group, one map per depth `1..groups() - 1`,
    /// finest first.
    group_barren: Vec<BTreeMap<K::Group, u64>>,
    /// Duration of one action in the cost unit the replacement rule uses.
    action_time: fn(&A) -> u64,
    /// Deepest retained key and its cheapest group time for the live sidecar.
    /// Both are monotone over append-only history, so reporting is constant
    /// time instead of rescanning every historical entry once per minute.
    live_progress: Option<(K, u64)>,
    /// Action bound the selector index was built for; `None` until the first
    /// selection. The index is derived state: rebuilding it from `entries`
    /// and `active` yields the same draws, so it is never serialized.
    frontier_cap: Option<usize>,
    /// Active entry ids under the frontier cap, ranked by ascending id.
    active_ids: ActiveIds,
    /// Active entries under the cap, pooled by walk class (deepest first)
    /// and, inside each class, by selection cell.
    classes: BTreeMap<Reverse<K::Group>, ClassCells<K>>,
    /// Live child groups beneath each selected parent group, indexed by the
    /// child depth. These are derived selector state and preserve the exact
    /// ordered group sets the walk previously rebuilt on every draw.
    live_children: LiveChildren<K>,
    /// Number of live selection cells in each pooled group, indexed by depth.
    /// The cell depth itself is represented by `CellMembers::sampleable`.
    live_group_cells: LiveGroupCells<K>,
    /// Active and live progress-band groups per walk class. Their key counts
    /// reproduce `classes_skipped` without rescanning every cell.
    active_skip_groups: BTreeMap<K::Group, BTreeMap<K::Group, usize>>,
    live_skip_groups: BTreeMap<K::Group, BTreeMap<K::Group, usize>>,
    /// Whole-tree import may need a replaced ancestor again before all source
    /// descendants have been rebuilt. Normal live search releases replaced
    /// snapshots immediately because only active entries are selectable.
    preserve_inactive_snapshots: bool,
    /// Replay-only remaining recorded job uses per parent snapshot. When
    /// present, an inactive payload is preserved only until its final use.
    preserved_snapshot_uses: Option<BTreeMap<u64, u32>>,
    /// Non-selectable metadata still referenced by dispatched jobs. These
    /// pins never affect selection or logical-budget decisions; they only
    /// defer physical reclamation until a bounded in-flight use completes.
    metadata_pins: BTreeMap<u64, u32>,
    /// Number of entry snapshots still resident, maintained in constant time.
    resident_snapshots: usize,
    /// Whether each entry's snapshot belongs to the selectable breeding
    /// population. Replay may temporarily keep a non-selectable payload for
    /// an already-dispatched job without changing this logical state.
    snapshot_selectable: Vec<bool>,
    /// Deterministic logical-byte ceiling for history plus selectable snapshots.
    memory_limit: Option<usize>,
    /// Stable id of the one executable snapshot retained as a bounded liveness
    /// anchor. This is present only for memory-budgeted campaigns.
    liveness_anchor: Option<u64>,
    #[cfg(test)]
    liveness_anchor_reactivations: u64,
    /// Logical bytes charged to snapshots still resident.
    resident_snapshot_bytes: usize,
    /// Game-owned, deterministic logical-size accounting for one snapshot.
    snapshot_memory_charge: Option<fn(&S) -> usize>,
    /// Resident ids in insertion order. Dropped ids are harmless tombstones
    /// until they reach the front; each id is examined at most once.
    resident_snapshot_order: VecDeque<usize>,
    /// Snapshots displaced solely by the global memory budget.
    snapshot_evictions: u64,
    /// Deterministic metadata compactions completed by this archive.
    history_compactions: u64,
    /// Non-selectable historical entries retired from live memory.
    historical_entries_dropped: u64,
    /// Rare full-input reconstructions from the compact prefix structure.
    input_reconstructions: Cell<u64>,
}

/// Members of one walk class, ascending per selection cell.
type ClassCells<K> = BTreeMap<<K as ArchiveKey>::Group, CellMembers<K>>;
type GroupPair<K> = (<K as ArchiveKey>::Group, <K as ArchiveKey>::Group);
type LiveChildren<K> = Vec<BTreeMap<GroupPair<K>, BTreeSet<<K as ArchiveKey>::Group>>>;
type LiveGroupCells<K> = Vec<BTreeMap<GroupPair<K>, usize>>;

struct CellMembers<K: ArchiveKey> {
    ids: BTreeSet<usize>,
    sampleable: BTreeSet<usize>,
    donors: BTreeSet<DonorRank<K>>,
    /// Id of the entry that most recently opened the cell; ids are creation
    /// order, so it orders the band's cells by novelty.
    opened: usize,
    /// Selections of the cell's current members, so novelty expires once
    /// the cell has had `CELL_NOVELTY_DRAWS` tries.
    drawn: u64,
}

type DonorRank<K> = (K, usize, usize);

impl<K: ArchiveKey> Default for CellMembers<K> {
    fn default() -> Self {
        Self {
            ids: BTreeSet::new(),
            sampleable: BTreeSet::new(),
            donors: BTreeSet::new(),
            opened: 0,
            drawn: 0,
        }
    }
}

impl<K: ArchiveKey> CellMembers<K> {
    /// The id that opened the cell while the cell still counts as new.
    fn novelty(&self) -> Option<usize> {
        (self.drawn < CELL_NOVELTY_DRAWS).then_some(self.opened)
    }
}

/// Trie over retained action sequences. Entries store parent-relative
/// suffixes; duplicate lookup shares every prefix and compares one action at
/// a time without retaining another complete input per entry.
struct InputNode<A: Ord> {
    parent: Option<usize>,
    action: Option<A>,
    children: BTreeMap<A, usize>,
    /// Stable archive id of the selectable entry ending at this prefix.
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

    /// Remove a terminal owner and reclaim its now-unreferenced tail. Shared
    /// ancestors remain until their final selectable descendant disappears.
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

    /// Pack live prefix nodes and return the old-to-new node mapping. This
    /// releases the high-water Vec allocation after deterministic pruning;
    /// otherwise logical live-node accounting could fall while RSS retained
    /// every historical tombstone.
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

/// Append-friendly ordered set over monotonically assigned archive ids.
///
/// A bit per id records membership while a Fenwick tree over 64-bit words
/// maps the selector's uniformly drawn rank back to the same ascending id
/// that the former compact `Vec` exposed. Inserts, removals, and rank draws
/// are logarithmic without moving every later id after a displacement.
#[derive(Default)]
struct ActiveIds {
    words: Vec<u64>,
    fenwick: Vec<usize>,
    len: usize,
}

impl ActiveIds {
    fn from_ids(ids: impl IntoIterator<Item = usize>) -> Self {
        let mut active = Self::default();
        active.fenwick.push(0);
        for id in ids {
            active.insert(id);
        }
        active
    }

    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn len(&self) -> usize {
        self.len
    }

    fn insert(&mut self, id: usize) {
        if self.fenwick.is_empty() {
            self.fenwick.push(0);
        }
        let word_index = id / u64::BITS as usize;
        let missing_words = word_index
            .saturating_add(1)
            .saturating_sub(self.words.len());
        for _ in 0..missing_words {
            self.push_empty_word();
        }
        let bit = 1_u64 << (id % u64::BITS as usize);
        let Some(word) = self.words.get_mut(word_index) else {
            return;
        };
        if *word & bit != 0 {
            return;
        }
        *word |= bit;
        self.len = self.len.saturating_add(1);
        self.update_word(word_index, true);
    }

    fn remove(&mut self, id: usize) -> bool {
        let word_index = id / u64::BITS as usize;
        let bit = 1_u64 << (id % u64::BITS as usize);
        let Some(word) = self.words.get_mut(word_index) else {
            return false;
        };
        if *word & bit == 0 {
            return false;
        }
        *word &= !bit;
        self.len = self.len.saturating_sub(1);
        self.update_word(word_index, false);
        true
    }

    fn select(&self, rank: usize) -> Option<usize> {
        if rank >= self.len {
            return None;
        }
        let target = rank.saturating_add(1);
        let mut tree_index = 0_usize;
        let mut prefix = 0_usize;
        for shift in (0..=self.words.len().ilog2()).rev() {
            let step = 1_usize << shift;
            let next = tree_index.saturating_add(step);
            if next <= self.words.len() && prefix.saturating_add(self.fenwick[next]) < target {
                tree_index = next;
                prefix = prefix.saturating_add(self.fenwick[next]);
            }
        }
        let word_index = tree_index;
        let mut word = *self.words.get(word_index)?;
        let within = target.saturating_sub(prefix);
        for _ in 1..within {
            word &= word.saturating_sub(1);
        }
        let bit = usize::try_from(word.trailing_zeros()).ok()?;
        word_index.checked_mul(u64::BITS as usize)?.checked_add(bit)
    }

    fn ids(&self) -> impl Iterator<Item = usize> + '_ {
        self.words
            .iter()
            .enumerate()
            .flat_map(|(word_index, word)| {
                (0..u64::BITS as usize).filter_map(move |bit| {
                    (*word & (1_u64 << bit) != 0)
                        .then(|| {
                            word_index
                                .checked_mul(u64::BITS as usize)
                                .and_then(|base| base.checked_add(bit))
                        })
                        .flatten()
                })
            })
    }

    fn push_empty_word(&mut self) {
        let tree_index = self.words.len().saturating_add(1);
        let low_bit = tree_index & tree_index.wrapping_neg();
        let lower = tree_index.saturating_sub(low_bit);
        let inherited = self
            .prefix_sum(tree_index.saturating_sub(1))
            .saturating_sub(self.prefix_sum(lower));
        self.words.push(0);
        self.fenwick.push(inherited);
    }

    fn prefix_sum(&self, mut words: usize) -> usize {
        let mut sum = 0_usize;
        for _ in 0..usize::BITS {
            if words == 0 {
                break;
            }
            sum = sum.saturating_add(self.fenwick[words]);
            words &= words - 1;
        }
        debug_assert_eq!(words, 0);
        sum
    }

    fn update_word(&mut self, word_index: usize, add: bool) {
        let mut tree_index = word_index.saturating_add(1);
        while tree_index < self.fenwick.len() {
            if add {
                self.fenwick[tree_index] = self.fenwick[tree_index].saturating_add(1);
            } else {
                self.fenwick[tree_index] = self.fenwick[tree_index].saturating_sub(1);
            }
            let low_bit = tree_index & tree_index.wrapping_neg();
            tree_index = tree_index.saturating_add(low_bit);
        }
    }
}

fn update_count<T: Copy + Ord>(map: &mut BTreeMap<T, usize>, key: T, add: bool) -> bool {
    if add {
        let count = map.entry(key).or_default();
        let transitioned = *count == 0;
        *count = count.saturating_add(1);
        transitioned
    } else {
        let Some(count) = map.get_mut(&key) else {
            return false;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(&key);
            true
        } else {
            false
        }
    }
}

fn decrement_nested_count<T: Copy + Ord>(
    map: &mut BTreeMap<T, BTreeMap<T, usize>>,
    outer: T,
    inner: T,
) {
    let Some(counts) = map.get_mut(&outer) else {
        return;
    };
    if let Some(count) = counts.get_mut(&inner) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            counts.remove(&inner);
        }
    }
    if counts.is_empty() {
        map.remove(&outer);
    }
}

fn update_child<T: Copy + Ord>(
    children: &mut BTreeMap<(T, T), BTreeSet<T>>,
    class: T,
    parent: T,
    child: T,
    add: bool,
) {
    let key = (class, parent);
    if add {
        children.entry(key).or_default().insert(child);
    } else if let Some(values) = children.get_mut(&key) {
        values.remove(&child);
        if values.is_empty() {
            children.remove(&key);
        }
    }
}

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
            id_to_index: BTreeMap::new(),
            next_entry_id: 0,
            active: Vec::new(),
            active_count: 0,
            slots: BTreeMap::new(),
            input_index: InputIndex::default(),
            historical_input_actions: 0,
            stored_input_actions: 0,
            history_memory_bytes: Self::prefix_node_memory_charge(),
            retained: 0,
            rejected: 0,
            selected: Vec::new(),
            productive: Vec::new(),
            since_retained: Vec::new(),
            in_window_ever: Vec::new(),
            opened_slot: Vec::new(),
            opened_cell: Vec::new(),
            cells_seen: BTreeSet::new(),
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
            deepest_leaf: Vec::new(),
            selector_policy: SelectorPolicy::GroupUniform,
            group_barren: vec![BTreeMap::new(); K::groups().saturating_sub(2)],
            action_time,
            live_progress: None,
            frontier_cap: None,
            active_ids: ActiveIds::default(),
            classes: BTreeMap::new(),
            live_children: vec![BTreeMap::new(); K::groups()],
            live_group_cells: vec![BTreeMap::new(); K::groups()],
            active_skip_groups: BTreeMap::new(),
            live_skip_groups: BTreeMap::new(),
            preserve_inactive_snapshots: false,
            preserved_snapshot_uses: None,
            metadata_pins: BTreeMap::new(),
            resident_snapshots: 0,
            snapshot_selectable: Vec::new(),
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
            input_reconstructions: Cell::new(0),
        }
    }

    /// Configure the deterministic logical-byte ceiling for compact history
    /// plus the selectable snapshot population. This is recorded by the
    /// campaign; wall-clock RSS and allocator behavior never enter eviction.
    pub(crate) fn set_memory_budget(&mut self, bytes: usize, charge: fn(&S) -> usize) {
        self.memory_limit = Some(bytes);
        self.snapshot_memory_charge = Some(charge);
    }

    /// Select the lowest stable-id active executable snapshot as the bounded
    /// campaign's deterministic liveness anchor.
    pub(crate) fn establish_liveness_anchor(&mut self, max_actions: usize) {
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
                    && self.entries[index].input_len < max_actions
            })
        }) {
            return;
        }
        self.liveness_anchor = self
            .entries
            .iter()
            .enumerate()
            .find(|(index, entry)| {
                self.active.get(*index).copied().unwrap_or(false)
                    && self
                        .snapshot_selectable
                        .get(*index)
                        .copied()
                        .unwrap_or(false)
                    && self.entries[*index].snapshot.is_some()
                    && entry.input_len < max_actions
            })
            .map(|(_, entry)| entry.id);
    }

    fn is_liveness_anchor(&self, id: usize) -> bool {
        self.memory_limit.is_some()
            && self.liveness_anchor == self.entries.get(id).map(|entry| entry.id)
    }

    /// Make room for an admission by moving the protected anchor out of the
    /// active population while retaining its selectable snapshot. The anchor
    /// is reinserted by `select_parent` only if the selector later empties.
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
        self.active[index] = false;
        self.active_count = self.active_count.saturating_sub(1);
        let slot_key = self.entries[index].key.group(0);
        let remove_slot = if let Some(slot) = self.slots.get_mut(&slot_key) {
            slot.retain(|id| *id != index);
            slot.is_empty()
        } else {
            false
        };
        if remove_slot {
            self.slots.remove(&slot_key);
        }
        self.index_remove(index);
        true
    }

    fn snapshot_charge(&self, snapshot: &S) -> usize {
        self.snapshot_memory_charge
            .map_or(0, |charge| charge(snapshot))
    }

    fn release_snapshot(&mut self, id: usize, budget_eviction: bool) -> bool {
        if !self.snapshot_selectable.get(id).copied().unwrap_or(false) {
            return false;
        }
        if budget_eviction && self.is_liveness_anchor(id) {
            return false;
        }
        let charge = self.entries[id]
            .snapshot
            .as_deref()
            .map_or(0, |snapshot| self.snapshot_charge(snapshot));
        self.index_remove(id);
        self.snapshot_selectable[id] = false;
        self.resident_snapshots = self.resident_snapshots.saturating_sub(1);
        self.resident_snapshot_bytes = self.resident_snapshot_bytes.saturating_sub(charge);
        let has_future_use = self
            .preserved_snapshot_uses
            .as_ref()
            .is_none_or(|uses| uses.contains_key(&self.entries[id].id));
        let worker_holds_snapshot = self.entries[id]
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| Arc::strong_count(snapshot) > 1);
        if (!self.preserve_inactive_snapshots || !has_future_use) && !worker_holds_snapshot {
            self.entries[id].snapshot.take();
        }
        self.input_index
            .set_owner(self.entries[id].input_node, None);
        if budget_eviction {
            if self.active.get(id).copied().unwrap_or(false) {
                self.active[id] = false;
                self.active_count = self.active_count.saturating_sub(1);
                let slot_key = self.entries[id].key.group(0);
                let remove_slot = if let Some(slot) = self.slots.get_mut(&slot_key) {
                    slot.retain(|entry| *entry != id);
                    slot.is_empty()
                } else {
                    false
                };
                if remove_slot {
                    self.slots.remove(&slot_key);
                }
            }
            self.snapshot_evictions = self.snapshot_evictions.saturating_add(1);
        }
        true
    }

    fn enforce_snapshot_memory_budget(&mut self) -> Result<(), &'static str> {
        let Some(limit) = self.memory_limit else {
            return Ok(());
        };
        while self.resident_memory_bytes() > limit && self.resident_snapshots > 1 {
            if !self.release_oldest_selectable(true) {
                break;
            }
        }
        if self.resident_memory_bytes() > limit && self.liveness_anchor.is_some() {
            return Err("memory budget cannot retain the executable liveness anchor");
        }
        Ok(())
    }

    fn release_oldest_selectable(&mut self, budget_eviction: bool) -> bool {
        let candidates = self.resident_snapshot_order.len();
        for _ in 0..candidates {
            let Some(id) = self.resident_snapshot_order.pop_front() else {
                break;
            };
            if !self.snapshot_selectable.get(id).copied().unwrap_or(false) {
                continue;
            }
            if self.is_liveness_anchor(id) {
                continue;
            }
            return self.release_snapshot(id, budget_eviction);
        }
        false
    }

    /// Reclaim non-selectable history once its deterministic charge reaches a
    /// quarter of the campaign budget. The append-only stream remains the
    /// authoritative history; this rebuild only compacts live acceleration
    /// structures and preserves stable stream ids.
    pub(crate) fn compact_history_if_needed(&mut self) -> Result<(), &'static str> {
        self.compact_history(false)
    }

    /// Normalize the final acceleration state to the breeding population.
    ///
    /// All worker jobs have joined before this is called, so any remaining
    /// metadata pins came from speculative work beyond the execution ceiling
    /// and have no recorded future consumer. Both live execution and replay
    /// perform this forced rebuild, making final artifacts independent of the
    /// transient parallel dispatch window.
    pub(crate) fn compact_history_for_final_report(&mut self) -> Result<(), &'static str> {
        self.metadata_pins.clear();
        self.compact_history(true)
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

        let keep = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                self.active.get(index).copied().unwrap_or(false)
                    || self.metadata_pins.contains_key(&entry.id)
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

        // Snapshot eviction clears a prefix owner immediately, but an
        // in-flight job pins its entry metadata until ordered admission. Make
        // every retained entry an owner again before pruning dead branches so
        // removing a displaced descendant cannot prune an in-flight parent's
        // still-required path.
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
        self.since_retained = retain_marked(std::mem::take(&mut self.since_retained), &keep);
        self.in_window_ever = retain_marked(std::mem::take(&mut self.in_window_ever), &keep);
        self.opened_slot = retain_marked(std::mem::take(&mut self.opened_slot), &keep);
        self.opened_cell = retain_marked(std::mem::take(&mut self.opened_cell), &keep);
        self.time_in_group = retain_marked(std::mem::take(&mut self.time_in_group), &keep);
        self.lineages = retain_marked(std::mem::take(&mut self.lineages), &keep);
        self.snapshot_selectable =
            retain_marked(std::mem::take(&mut self.snapshot_selectable), &keep);

        // Novelty and pooled-barrenness are compact historical metadata, not
        // reasons to keep snapshots or dead entries resident. Once history is
        // compacted, keys absent from the selectable breeding population can
        // be rediscovered deterministically and must not make these maps grow
        // with the lifetime execution count.
        self.cells_seen = self
            .entries
            .iter()
            .zip(&self.active)
            .filter_map(|(entry, active)| active.then_some(entry.key.group(Self::cell_depth())))
            .collect();
        for (offset, barren) in self.group_barren.iter_mut().enumerate() {
            let live_groups = self
                .entries
                .iter()
                .zip(&self.active)
                .filter_map(|(entry, active)| active.then_some(entry.key.group(offset + 1)))
                .collect::<BTreeSet<_>>();
            barren.retain(|group, _| live_groups.contains(group));
        }

        self.id_to_index.clear();
        self.slots.clear();
        self.resident_snapshot_order.clear();
        self.active_count = 0;
        self.resident_snapshots = 0;
        self.resident_snapshot_bytes = 0;
        self.stored_input_actions = 0;
        for (index, entry) in self.entries.iter().enumerate() {
            self.id_to_index.insert(entry.id, index);
            self.stored_input_actions = self
                .stored_input_actions
                .saturating_add(entry.input_suffix.len());
            if self.active[index] {
                self.active_count = self.active_count.saturating_add(1);
                self.slots
                    .entry(entry.key.group(0))
                    .or_default()
                    .push(index);
            }
            if self.snapshot_selectable[index] {
                self.resident_snapshots = self.resident_snapshots.saturating_add(1);
                self.resident_snapshot_bytes = self.resident_snapshot_bytes.saturating_add(
                    entry
                        .snapshot
                        .as_deref()
                        .map_or(0, |snapshot| self.snapshot_charge(snapshot)),
                );
                self.resident_snapshot_order.push_back(index);
            }
        }

        // Descendant hints are a rebuildable splice cache. Resetting them to
        // each surviving entry is deterministic; newly admitted descendants
        // repopulate the hints without retaining dead history.
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

        let frontier_cap = self.frontier_cap.take();
        self.active_ids = ActiveIds::default();
        self.classes.clear();
        self.live_children.iter_mut().for_each(BTreeMap::clear);
        self.live_group_cells.iter_mut().for_each(BTreeMap::clear);
        self.active_skip_groups.clear();
        self.live_skip_groups.clear();
        if let Some(cap) = frontier_cap {
            self.rebuild_selector_index(cap);
        }
        self.enforce_snapshot_memory_budget()?;
        Ok(())
    }

    /// Preserve replaced snapshots temporarily while rebuilding a source tree.
    pub(crate) fn preserve_inactive_snapshots(
        &mut self,
        preserve: bool,
    ) -> Result<(), &'static str> {
        self.preserve_inactive_snapshots = preserve;
        if !preserve {
            self.preserved_snapshot_uses = None;
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

    /// Resolve one immutable stream id to its current compact live slot.
    pub(crate) fn index_of_id(&self, id: u64) -> Option<usize> {
        self.id_to_index.get(&id).copied()
    }

    /// Immutable stream id of one current compact live slot.
    pub(crate) fn stable_id(&self, index: usize) -> Option<u64> {
        self.entries.get(index).map(|entry| entry.id)
    }

    /// Keep one entry's compact metadata through an in-flight job.
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

    /// Release one in-flight metadata reference.
    pub(crate) fn unpin_metadata(&mut self, id: u64) {
        let remove = if let Some(pins) = self.metadata_pins.get_mut(&id) {
            *pins = pins.saturating_sub(1);
            *pins == 0
        } else {
            false
        };
        if remove {
            self.metadata_pins.remove(&id);
        }
    }

    /// Install deterministic future metadata uses while replaying serially.
    pub(crate) fn preserve_recorded_metadata_uses(&mut self, uses: BTreeMap<u64, u32>) {
        self.metadata_pins = uses;
    }

    /// Preserve replay parents only until their final recorded job consumes
    /// them, instead of retaining every displaced payload for the whole run.
    pub(crate) fn preserve_recorded_snapshot_uses(&mut self, uses: BTreeMap<u64, u32>) {
        self.preserve_inactive_snapshots = true;
        self.preserved_snapshot_uses = Some(uses);
    }

    /// Consume one replay parent use and release its inactive payload when no
    /// later recorded job can reference it.
    pub(crate) fn consume_recorded_snapshot_use(&mut self, id: u64) {
        let exhausted = if let Some(uses) = self.preserved_snapshot_uses.as_mut() {
            let Some(remaining) = uses.get_mut(&id) else {
                return;
            };
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                uses.remove(&id);
                true
            } else {
                false
            }
        } else {
            false
        };
        if exhausted
            && let Some(index) = self.index_of_id(id)
            && !self
                .snapshot_selectable
                .get(index)
                .copied()
                .unwrap_or(false)
        {
            self.entries[index].snapshot.take();
        }
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

    /// Rebuild one complete input from compact parent-relative suffixes.
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
            // Conservative allocator/B-tree node overhead per unique edge.
            .saturating_add(64)
    }

    fn history_entry_memory_charge(suffix_len: usize, new_nodes: usize) -> usize {
        size_of::<ArchiveEntry<A, K, M, S>>()
            .saturating_add(suffix_len.saturating_mul(size_of::<A>()))
            .saturating_add(size_of::<K::Lineage>())
            .saturating_add(size_of::<(K, usize)>())
            .saturating_add(4_usize.saturating_mul(size_of::<u64>()))
            .saturating_add(4_usize.saturating_mul(size_of::<usize>()))
            // Ordered slot/selector nodes and vector slack.
            .saturating_add(128)
            .saturating_add(new_nodes.saturating_mul(Self::prefix_node_memory_charge()))
    }

    fn historical_group_memory_charge(value_bytes: usize) -> usize {
        size_of::<K::Group>()
            .saturating_add(value_bytes)
            // Conservative allocator/B-tree node overhead per remembered key.
            .saturating_add(64)
    }

    fn auxiliary_history_memory_bytes(&self) -> usize {
        self.novelty_memory_bytes()
            .saturating_add(self.barren_memory_bytes())
    }

    /// Whether every successive action after `parent_id` reaches an input
    /// already retained in the archive.
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
        self.active_ids = ActiveIds::from_ids(self.active_ids(max_actions));
        self.classes = BTreeMap::new();
        self.live_children.iter_mut().for_each(BTreeMap::clear);
        self.live_group_cells.iter_mut().for_each(BTreeMap::clear);
        self.active_skip_groups.clear();
        self.live_skip_groups.clear();
        let active = self.active_ids.ids().collect::<Vec<_>>();
        for id in active {
            self.insert_active_cell_member(id);
        }
    }

    /// Rebuild the selector around the bounded liveness anchor after a stale
    /// active-id index was emptied by deterministic archive maintenance.
    fn reactivate_liveness_anchor(&mut self, max_actions: usize) -> bool {
        if !self.active_ids.is_empty() {
            return false;
        }
        self.establish_liveness_anchor(max_actions);
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
            || self.entries[index].input_len >= max_actions
        {
            return false;
        }
        if !self.active.get(index).copied().unwrap_or(false) {
            if self.active_count >= self.max_entries && !self.release_oldest_selectable(true) {
                return false;
            }
            let slot_key = self.entries[index].key.group(0);
            let slot_full = self
                .slots
                .get(&slot_key)
                .is_some_and(|slot| slot.len() >= MAX_ENTRIES_PER_KEY);
            if slot_full {
                let replaced = self
                    .slots
                    .get(&slot_key)
                    .into_iter()
                    .flatten()
                    .copied()
                    .filter(|id| !self.is_liveness_anchor(*id))
                    .max_by_key(|id| (self.time_in_group[*id], self.entries[*id].id));
                let Some(replaced) = replaced else {
                    return false;
                };
                self.active[replaced] = false;
                self.active_count = self.active_count.saturating_sub(1);
                if let Some(slot) = self.slots.get_mut(&slot_key) {
                    slot.retain(|id| *id != replaced);
                }
                self.release_snapshot(replaced, false);
            }
            self.active[index] = true;
            self.active_count = self.active_count.saturating_add(1);
            self.slots
                .entry(self.entries[index].key.group(0))
                .or_default()
                .push(index);
            #[cfg(test)]
            {
                self.liveness_anchor_reactivations =
                    self.liveness_anchor_reactivations.saturating_add(1);
            }
        }
        if !self.resident_snapshot_order.contains(&index) {
            self.resident_snapshot_order.push_back(index);
        }
        self.rebuild_selector_index(max_actions);
        !self.active_ids.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn liveness_anchor_reactivations(&self) -> u64 {
        self.liveness_anchor_reactivations
    }

    /// Add a fresh entry to the selector index. Ids only grow, so pushes
    /// keep every list ascending.
    fn index_insert(&mut self, id: usize) {
        let Some(cap) = self.frontier_cap else {
            return;
        };
        if self.entries[id].input_len >= cap {
            return;
        }
        self.active_ids.insert(id);
        self.insert_active_cell_member(id);
    }

    /// Drop a displaced entry from the selector index.
    fn index_remove(&mut self, id: usize) {
        if self.frontier_cap.is_none() {
            return;
        }
        self.active_ids.remove(id);
        let key = self.entries[id].key;
        let class = Reverse(key.group(Self::class_depth()));
        let cell = key.group(Self::cell_depth());
        let was_sampleable = self.entry_unexhausted(id);
        let deepest = self.deepest_leaf[id];
        let mut became_dead = false;
        let mut removed_cell = false;
        let mut removed_class = false;
        if let Some(cells) = self.classes.get_mut(&class) {
            if let Some(members) = cells.get_mut(&cell) {
                if members.ids.remove(&id) {
                    members.drawn = members.drawn.saturating_sub(self.selected[id]);
                    members.donors.remove(&(deepest.0, deepest.1, id));
                    if was_sampleable {
                        members.sampleable.remove(&id);
                    }
                    became_dead = members.sampleable.is_empty() && was_sampleable;
                }
                if members.ids.is_empty() {
                    cells.remove(&cell);
                    removed_cell = true;
                }
            }
            removed_class = cells.is_empty();
        }
        if removed_class {
            self.classes.remove(&class);
        }
        if became_dead {
            self.set_cell_live(key, false);
        }
        if removed_cell {
            self.remove_active_cell(key);
        }
    }

    fn insert_active_cell_member(&mut self, id: usize) {
        let key = self.entries[id].key;
        let class = Reverse(key.group(Self::class_depth()));
        let cell = key.group(Self::cell_depth());
        let sampleable = self.entry_unexhausted(id);
        let deepest = self.deepest_leaf[id];
        let members = self
            .classes
            .entry(class)
            .or_default()
            .entry(cell)
            .or_default();
        let new_cell = members.ids.is_empty();
        let was_live = !members.sampleable.is_empty();
        if new_cell {
            members.opened = id;
        }
        members.ids.insert(id);
        members.drawn = members.drawn.saturating_add(self.selected[id]);
        members.donors.insert((deepest.0, deepest.1, id));
        if sampleable {
            members.sampleable.insert(id);
        }
        if new_cell {
            self.insert_active_cell(key);
        }
        if !was_live && sampleable {
            self.set_cell_live(key, true);
        }
    }

    fn update_index_deepest_leaf(&mut self, id: usize, previous: (K, usize), current: (K, usize)) {
        if self.frontier_cap.is_none() {
            return;
        }
        let key = self.entries[id].key;
        let class = Reverse(key.group(Self::class_depth()));
        let cell = key.group(Self::cell_depth());
        let Some(members) = self
            .classes
            .get_mut(&class)
            .and_then(|cells| cells.get_mut(&cell))
        else {
            return;
        };
        if !members.ids.contains(&id) {
            return;
        }
        members.donors.remove(&(previous.0, previous.1, id));
        members.donors.insert((current.0, current.1, id));
    }

    fn insert_active_cell(&mut self, key: K) {
        let class = key.group(Self::class_depth());
        let skip = key.group(2.min(Self::class_depth()));
        *self
            .active_skip_groups
            .entry(class)
            .or_default()
            .entry(skip)
            .or_default() += 1;
    }

    fn remove_active_cell(&mut self, key: K) {
        let class = key.group(Self::class_depth());
        let skip = key.group(2.min(Self::class_depth()));
        decrement_nested_count(&mut self.active_skip_groups, class, skip);
    }

    fn set_cell_live(&mut self, key: K, live: bool) {
        let class = key.group(Self::class_depth());
        let skip = key.group(2.min(Self::class_depth()));
        if live {
            *self
                .live_skip_groups
                .entry(class)
                .or_default()
                .entry(skip)
                .or_default() += 1;
        } else {
            decrement_nested_count(&mut self.live_skip_groups, class, skip);
        }

        let cell_depth = Self::cell_depth();
        let class_depth = Self::class_depth();
        if cell_depth < class_depth {
            update_child(
                &mut self.live_children[cell_depth],
                class,
                key.group(cell_depth + 1),
                key.group(cell_depth),
                live,
            );
        }
        for depth in (cell_depth + 1)..=class_depth {
            let group = key.group(depth);
            let map_key = (class, group);
            let transitioned = update_count(&mut self.live_group_cells[depth], map_key, live);
            if transitioned && depth < class_depth {
                update_child(
                    &mut self.live_children[depth],
                    class,
                    key.group(depth + 1),
                    group,
                    live,
                );
            }
        }
    }

    fn set_entry_sampleable(&mut self, id: usize, sampleable: bool) {
        let key = self.entries[id].key;
        let class = Reverse(key.group(Self::class_depth()));
        let cell = key.group(Self::cell_depth());
        let Some(members) = self
            .classes
            .get_mut(&class)
            .and_then(|cells| cells.get_mut(&cell))
        else {
            return;
        };
        // A selected job may finish after an earlier admission displaced its
        // parent. The cell can still exist for other entries, but that stale
        // parent must not change their cached live-member count.
        if !members.ids.contains(&id) {
            return;
        }
        let was_live = !members.sampleable.is_empty();
        if sampleable {
            members.sampleable.insert(id);
        } else {
            members.sampleable.remove(&id);
        }
        let live = !members.sampleable.is_empty();
        if live != was_live {
            self.set_cell_live(key, live);
        }
    }

    fn rebuild_live_selector_index(&mut self) {
        self.live_children.iter_mut().for_each(BTreeMap::clear);
        self.live_group_cells.iter_mut().for_each(BTreeMap::clear);
        self.live_skip_groups.clear();
        for cells in self.classes.values_mut() {
            for members in cells.values_mut() {
                members.sampleable.clear();
            }
        }
        let active = self.active_ids.ids().collect::<Vec<_>>();
        for id in active {
            if self.entry_unexhausted(id) {
                self.set_entry_sampleable(id, true);
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
        self.live_progress
            .map(|(deepest, cheapest)| (deepest, cheapest, self.retained))
    }

    /// Time a candidate spent inside its own coarsest group.
    ///
    /// An input extends its parent's, so the time added since the parent is
    /// the duration of the actions past the parent's length. A candidate
    /// whose parent already sits in the same coarsest group inherits the
    /// parent's count; one whose parent sits elsewhere entered the group
    /// during those actions and starts the count there. A candidate with no
    /// parent — genesis, and only genesis — counts its whole input.
    fn time_in_group_of(&self, parent_id: Option<usize>, suffix: &[A], key: K) -> u64 {
        let time_of = |actions: &[A]| -> u64 {
            actions
                .iter()
                .map(|action| (self.action_time)(action))
                .sum()
        };
        let Some(parent) = parent_id.and_then(|id| self.entries.get(id)) else {
            return time_of(suffix);
        };
        let added = time_of(suffix);
        let depth = Self::coarsest_depth();
        if parent.key.group(depth) == key.group(depth) {
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
        self.insert_after(parent_id, None, execution, candidate, snapshot)
            .map(|(id, _)| id)
    }

    /// Offer a candidate to retention, completing its key against
    /// `previous` when given: the completed key of the boundary just before
    /// this one on the same input, which retention did not keep. A job walks
    /// many boundaries past its parent, and the ones landing in full slots
    /// still carry the lineage's position forward, so completion follows the
    /// last boundary rather than the last retained entry. Returns the
    /// completed key with the outcome.
    ///
    /// # Errors
    ///
    /// Returns an error on id-space overflow or a missing parent.
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
        let depth = Self::coarsest_depth();
        let mut lineage = match parent_id {
            Some(id) if self.entries[id].key.group(depth) == key.group(depth) => {
                self.lineages[id].clone()
            }
            _ => K::Lineage::default(),
        };
        K::record(&mut lineage, key);
        let candidate_time_in_group = self.time_in_group_of(parent_id, &suffix, key);
        // The costliest entry in the group's own clock loses to a candidate
        // that reached the same slot in strictly less time. The entry id
        // breaks ties so the choice stays a total order over the slot.
        let slot = self.slots.entry(key.group(0)).or_default().clone();
        let new_slot = slot.is_empty();
        let slot_full = slot.len() >= MAX_ENTRIES_PER_KEY;
        let replace = if slot_full {
            slot.iter()
                .copied()
                .max_by_key(|id| (self.time_in_group[*id], self.entries[*id].id))
                .filter(|id| candidate_time_in_group < self.time_in_group[*id])
        } else {
            None
        };
        if slot_full && replace.is_none() {
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
            if !self.release_oldest_selectable(true) {
                if self.active_count == 1 && self.deactivate_liveness_anchor_for_admission() {
                    continue;
                }
                return Err("archive population limit cannot retire an entry".into());
            }
        }
        if self.active_count >= self.max_entries {
            return Err("archive population limit did not retire an entry".into());
        }
        if let Some(replaced) = replace {
            let replaced_anchor = self.is_liveness_anchor(replaced);
            self.active[replaced] = false;
            self.active_count = self.active_count.saturating_sub(1);
            if let Some(slot) = self.slots.get_mut(&key.group(0)) {
                slot.retain(|id| *id != replaced);
            }
            self.replacement_time_displaced = self.replacement_time_displaced.saturating_add(1);
            self.index_remove(replaced);
            if !replaced_anchor {
                self.release_snapshot(replaced, false);
            }
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
        self.resident_snapshot_order.push_back(id);
        self.active.push(true);
        self.active_count = self.active_count.saturating_add(1);
        self.lineages.push(lineage);
        self.time_in_group.push(candidate_time_in_group);
        match &mut self.live_progress {
            Some((deepest, cheapest)) if key > *deepest => {
                *deepest = key;
                *cheapest = candidate_time_in_group;
            }
            Some((deepest, cheapest)) if key == *deepest => {
                *cheapest = (*cheapest).min(candidate_time_in_group);
            }
            Some(_) => {}
            None => self.live_progress = Some((key, candidate_time_in_group)),
        }
        self.selected.push(0);
        self.productive.push(0);
        self.since_retained.push(0);
        self.in_window_ever.push(false);
        self.opened_slot.push(new_slot);
        // A one-group key has no pooled cell depth; slot novelty stands in.
        let new_cell = if K::groups() > 1 {
            self.cells_seen.insert(key.group(1))
        } else {
            new_slot
        };
        self.opened_cell.push(new_cell);
        self.deepest_leaf.push((key, id));
        let mut ancestor = parent_id;
        while let Some(current) = ancestor {
            let previous = self.deepest_leaf[current];
            if previous >= (key, id) {
                break;
            }
            self.update_index_deepest_leaf(current, previous, (key, id));
            self.deepest_leaf[current] = (key, id);
            ancestor = self.entries[current]
                .parent_id
                .and_then(|parent| self.id_to_index.get(&parent).copied());
        }
        self.slots.entry(key.group(0)).or_default().push(id);
        self.history_memory_bytes = self
            .history_memory_bytes
            .saturating_add(Self::history_entry_memory_charge(suffix.len(), new_nodes));
        self.retained = self.retained.saturating_add(1);
        self.index_insert(id);
        self.enforce_snapshot_memory_budget()?;
        Ok((Some(id), key))
    }

    /// Derive a campaign splice after ensuring the selection-cell index is
    /// present under the recorded action limit. Live selection normally
    /// builds this index first; serial replay must establish the same derived
    /// state without repeating the random parent draw.
    pub(crate) fn splice_tail_for_campaign(
        &mut self,
        parent: usize,
        max_actions: usize,
        cap: usize,
    ) -> Option<CampaignSpliceTail<A>> {
        if self.frontier_cap != Some(max_actions) {
            self.rebuild_selector_index(max_actions);
        }
        let parent_key = self.entries[parent].key;
        let class = Reverse(parent_key.group(Self::class_depth()));
        let cell = parent_key.group(Self::cell_depth());
        let members = self.classes.get(&class)?.get(&cell)?;
        let donor_id = members
            .donors
            .iter()
            .rev()
            .find_map(|(_, _, donor)| (*donor != parent).then_some(*donor))?;
        let (leaf_key, leaf_id) = self.deepest_leaf[donor_id];
        if leaf_key <= parent_key {
            return None;
        }
        let actions = self
            .recorded_splice_tail(parent, donor_id, leaf_id, cap)
            .ok()?;
        Some(CampaignSpliceTail {
            donor_id,
            leaf_id,
            actions,
        })
    }

    /// Rebuild a recorded dispatch-time splice from append-only archive ids.
    pub(crate) fn recorded_splice_tail(
        &self,
        parent: usize,
        donor: usize,
        leaf: usize,
        cap: usize,
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
        let donor_key = donor_entry.key;
        if parent_key.group(Self::class_depth()) != donor_key.group(Self::class_depth())
            || parent_key.group(Self::cell_depth()) != donor_key.group(Self::cell_depth())
        {
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
        if leaf_entry.key <= parent_key {
            return Err("splice leaf does not advance past the parent");
        }
        let suffix = &leaf_input[donor_input.len()..];
        if suffix.is_empty() {
            return Err("splice leaf has no actions past its donor");
        }
        Ok(suffix.iter().take(cap).cloned().collect())
    }

    fn active_ids(&self, max_actions: usize) -> Vec<usize> {
        self.active
            .iter()
            .enumerate()
            .filter_map(|(id, active)| {
                (*active
                    && self.snapshot_selectable[id]
                    && self.entries[id].input_len < max_actions)
                    .then_some(id)
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
        if self.active_ids.is_empty() && !self.reactivate_liveness_anchor(max_actions) {
            return Err("archive has no expandable entry".into());
        }
        let use_walk = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_walk {
            let count = NonZeroUsize::new(self.active_ids.len()).ok_or("empty archive")?;
            let id = self
                .active_ids
                .select(rand.below(count))
                .ok_or("active-id rank is outside the archive")?;
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
                        path: SelectorPath::GroupWalk,
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
        if !ignore_streaks && !matches!(self.selector_policy, SelectorPolicy::Retire(_)) {
            return self.walk_live_index(rand, classes_skipped);
        }
        self.walk_to_cell_scan(rand, classes_skipped, ignore_streaks)
    }

    fn walk_to_cell_scan(
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
                // per-entry streak varies inside. Liveness is counted here
                // and only the finally chosen cell's live members are
                // materialized, so a draw allocates one member list rather
                // than one per cell.
                let Some(member) = members.ids.iter().next().copied() else {
                    continue;
                };
                let key = self.entries[member].key;
                let group_live = ignore_streaks || self.groups_unexhausted(key);
                let live = group_live
                    && members.ids.iter().any(|id| {
                        self.active.get(*id).copied().unwrap_or(false)
                            && (ignore_streaks || self.entry_unexhausted(*id))
                    });
                let subclass = subclass_live.entry(key.group(skip_depth)).or_insert(false);
                *subclass |= live;
                if live {
                    cells.push((key, members));
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
                let mut deepest = BTreeMap::<K::Group, K::Group>::new();
                for (key, _) in &cells {
                    let band = key.group(Self::frontier_depth());
                    deepest
                        .entry(key.group(depth))
                        .and_modify(|frontier| *frontier = (*frontier).max(band))
                        .or_insert(band);
                }
                let mut groups = deepest.keys().copied().collect::<Vec<_>>();
                let frontier = deepest.values().copied().collect::<Vec<_>>();
                let index = self.draw_group_index(rand, depth, &groups, Some(&frontier), None)?;
                let chosen = groups.swap_remove(index);
                cells.retain(|(key, _)| key.group(depth) == chosen);
            }
            if cells.is_empty() {
                return Err("cell draw over an exhausted class".into());
            }
            // A one-group key collapses the cell onto the retention slot and
            // has no pooled depth to weight, so its cell draw stays uniform.
            let index = if Self::coarsest_depth() >= 1 {
                let cell_groups = cells
                    .iter()
                    .map(|(key, _)| key.group(1))
                    .collect::<Vec<_>>();
                let ranked = cells
                    .iter()
                    .map(|(_, members)| {
                        let offered = members.ids.iter().filter(|id| {
                            self.active.get(**id).copied().unwrap_or(false)
                                && (ignore_streaks || self.entry_unexhausted(**id))
                        });
                        (members.novelty(), self.cheapest_offered(offered))
                    })
                    .collect::<Vec<_>>();
                self.draw_group_index(rand, 1, &cell_groups, None, Some(&ranked))?
            } else {
                let count = NonZeroUsize::new(cells.len()).ok_or("cell draw over no cells")?;
                rand.below(count)
            };
            let members = cells.swap_remove(index).1;
            return Ok(Some(
                members
                    .ids
                    .iter()
                    .copied()
                    .filter(|id| {
                        self.active.get(*id).copied().unwrap_or(false)
                            && (ignore_streaks || self.entry_unexhausted(*id))
                    })
                    .collect(),
            ));
        }
        Ok(None)
    }

    fn walk_live_index(
        &self,
        rand: &mut RomuDuoJrRand,
        classes_skipped: &mut u64,
    ) -> Result<Option<Vec<usize>>, Box<dyn Error>> {
        let class_depth = Self::class_depth();
        let cell_depth = Self::cell_depth();
        for (reverse_class, cells) in &self.classes {
            let class = reverse_class.0;
            let active_skip = self.active_skip_groups.get(&class).map_or(0, BTreeMap::len);
            let live_skip = self.live_skip_groups.get(&class).map_or(0, BTreeMap::len);
            *classes_skipped = classes_skipped.saturating_add(
                u64::try_from(active_skip.saturating_sub(live_skip)).unwrap_or(u64::MAX),
            );
            if live_skip == 0 {
                continue;
            }

            let mut parent = class;
            for depth in ((cell_depth + 1)..class_depth).rev() {
                let groups = self
                    .live_children
                    .get(depth)
                    .and_then(|children| children.get(&(class, parent)))
                    .ok_or("live selector hierarchy is missing a pooled group")?
                    .iter()
                    .copied()
                    .collect::<Vec<_>>();
                let frontier = groups
                    .iter()
                    .map(|group| self.deepest_live_band(class, depth, *group))
                    .collect::<Result<Vec<_>, _>>()?;
                let index = self.draw_group_index(rand, depth, &groups, Some(&frontier), None)?;
                parent = groups[index];
            }

            let cell = if cell_depth < class_depth {
                let groups = self
                    .live_children
                    .get(cell_depth)
                    .and_then(|children| children.get(&(class, parent)))
                    .ok_or("live selector hierarchy is missing a selection cell")?
                    .iter()
                    .copied()
                    .collect::<Vec<_>>();
                if class_depth >= 1 {
                    let ranked = groups
                        .iter()
                        .map(|cell| {
                            let members = cells.get(cell);
                            (
                                members.and_then(CellMembers::novelty),
                                members.and_then(|members| {
                                    self.cheapest_offered(members.sampleable.iter())
                                }),
                            )
                        })
                        .collect::<Vec<_>>();
                    groups[self.draw_group_index(rand, cell_depth, &groups, None, Some(&ranked))?]
                } else {
                    groups[rand
                        .below(NonZeroUsize::new(groups.len()).ok_or("cell draw over no cells")?)]
                }
            } else {
                let only = [class];
                if class_depth >= 1 {
                    let _ = self.draw_group_index(rand, cell_depth, &only, None, None)?;
                } else {
                    let _ = rand.below(NonZeroUsize::new(1).ok_or("cell draw over no cells")?);
                }
                class
            };
            let members = cells
                .get(&cell)
                .ok_or("live selector hierarchy chose an absent cell")?;
            let mut sampleable = members
                .sampleable
                .iter()
                .rev()
                .take(CONCENTRATION_WINDOW)
                .copied()
                .collect::<Vec<_>>();
            sampleable.reverse();
            return Ok(Some(sampleable));
        }
        Ok(None)
    }

    /// The depth whose groups order the walk's frontier rank: the pooled
    /// threshold depth, which for a key with a progress band is the band.
    fn frontier_depth() -> usize {
        2.min(Self::class_depth())
    }

    /// The deepest live frontier-depth group under `group` at `depth`
    /// within `class`. A group's own key orders by whatever its pooled
    /// fields leave, which for a room is its identity bytes, so the walk
    /// ranks groups above the frontier depth by their frontier instead.
    fn deepest_live_band(
        &self,
        class: K::Group,
        depth: usize,
        group: K::Group,
    ) -> Result<K::Group, Box<dyn Error>> {
        let mut current = group;
        for level in (Self::frontier_depth()..depth).rev() {
            current = *self
                .live_children
                .get(level)
                .and_then(|children| children.get(&(class, current)))
                .and_then(BTreeSet::last)
                .ok_or("live selector hierarchy is missing a pooled group's frontier")?;
        }
        Ok(current)
    }

    /// Draw one index into `groups` at `depth`, where `frontier[i]` is the
    /// deepest live frontier-depth group under `groups[i]`, or `None` below
    /// the frontier depth where every group shares one and `cells[i]` is
    /// instead the id that opened cell `groups[i]` while it still counts as
    /// new and the frames in group of the cheapest member its draw offers.
    /// Under the energy selector each group's weight halves every `scale`
    /// barren selections and floors at 1/256 of a fresh group, then halves
    /// again for every distinct deeper frontier, or for every
    /// `CELL_NOVELTY_RANK_SCALE` newer cells that still count as new plus
    /// every `CHEAPEST_RANK_SCALE` cells with a cheaper best member, so the
    /// rank order survives the energy floor; a cell past its novelty draws
    /// competes on energy and cost alone; every other selector draws
    /// uniformly, so their recorded rand streams keep their exact bytes.
    fn draw_group_index(
        &self,
        rand: &mut RomuDuoJrRand,
        depth: usize,
        groups: &[K::Group],
        frontier: Option<&[K::Group]>,
        cells: Option<&[(Option<usize>, Option<u64>)]>,
    ) -> Result<usize, Box<dyn Error>> {
        let count = NonZeroUsize::new(groups.len()).ok_or("group draw over no groups")?;
        let (scales, ranked_by_frontier) = match &self.selector_policy {
            SelectorPolicy::Energy(scales) => (scales, false),
            SelectorPolicy::EnergyFrontier(scales)
            | SelectorPolicy::EnergyFrontierCheapest(scales) => (scales, true),
            _ => return Ok(rand.below(count)),
        };
        // A key with no pooled depth at this position has no barren counter
        // to weight by; its draw stays uniform.
        let Some(scale) = scales.groups.get(depth - 1).copied() else {
            return Ok(rand.below(count));
        };
        // Frontier rank depends only on the set of distinct frontiers. Build
        // that ordering once instead of filtering, allocating, sorting, and
        // deduplicating the complete list once per candidate group.
        let ranked = match frontier {
            Some(frontier) if ranked_by_frontier => {
                let mut ranked = frontier.to_vec();
                ranked.sort_unstable();
                ranked.dedup();
                ranked
            }
            _ => Vec::new(),
        };
        let (newest, cheapest) = match cells {
            Some(cells) if ranked_by_frontier && frontier.is_none() => {
                let mut newest = cells.iter().filter_map(|cell| cell.0).collect::<Vec<_>>();
                newest.sort_unstable();
                let mut cheapest = cells.iter().filter_map(|cell| cell.1).collect::<Vec<_>>();
                cheapest.sort_unstable();
                (newest, cheapest)
            }
            _ => (Vec::new(), Vec::new()),
        };
        let mut weights = Vec::with_capacity(groups.len());
        for (index, group) in groups.iter().enumerate() {
            let barren = self
                .group_barren
                .get(depth - 1)
                .and_then(|map| map.get(group))
                .copied()
                .unwrap_or(0);
            let halvings = usize::try_from((barren / scale).min(8)).unwrap_or(8);
            let energy = 256_usize >> halvings;
            if !ranked_by_frontier {
                weights.push(energy);
                continue;
            }
            let (rank, span) = match (frontier, cells) {
                (Some(frontier), _) => {
                    let position = ranked
                        .binary_search(&frontier[index])
                        .map_err(|_| "energy frontier group is missing from its own rank table")?;
                    (ranked.len().saturating_sub(position.saturating_add(1)), 8)
                }
                (None, Some(cells)) => {
                    let (opened, cost) = cells[index];
                    let novelty = match opened {
                        Some(opened) => {
                            let position = newest
                                .binary_search(&opened)
                                .map_err(|_| "energy cell is missing from its own novelty table")?;
                            newest.len().saturating_sub(position.saturating_add(1))
                                / CELL_NOVELTY_RANK_SCALE
                        }
                        None => 8,
                    };
                    let costlier = cost.map_or(0, |cost| {
                        cheapest.partition_point(|cheaper| *cheaper < cost) / CHEAPEST_RANK_SCALE
                    });
                    (novelty.saturating_add(costlier), 16)
                }
                (None, None) => (0, 8),
            };
            weights.push((energy << span) >> rank.min(span));
        }
        let total = NonZeroUsize::new(weights.iter().sum()).ok_or("energy weights sum to zero")?;
        let mut draw = rand.below(total);
        for (index, weight) in weights.iter().enumerate() {
            if draw < *weight {
                return Ok(index);
            }
            draw -= weight;
        }
        Err("energy draw exceeded its weight total".into())
    }

    /// The per-entry half of the exhaustion rule.
    fn entry_unexhausted(&self, id: usize) -> bool {
        if self.since_retained[id] >= SELECTION_EXHAUSTION_THRESHOLD {
            return false;
        }
        match &self.selector_policy {
            SelectorPolicy::GroupUniform => true,
            SelectorPolicy::Retire(thresholds)
            | SelectorPolicy::Energy(thresholds)
            | SelectorPolicy::EnergyFrontier(thresholds)
            | SelectorPolicy::EnergyFrontierCheapest(thresholds) => {
                self.since_retained[id] < thresholds.entry
            }
        }
    }

    /// The pooled-group half of the exhaustion rule, shared by every member
    /// of a selection cell.
    fn groups_unexhausted(&self, key: K) -> bool {
        match &self.selector_policy {
            SelectorPolicy::GroupUniform
            | SelectorPolicy::Energy(_)
            | SelectorPolicy::EnergyFrontier(_)
            | SelectorPolicy::EnergyFrontierCheapest(_) => true,
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

    /// Frames in group of the cheapest member a cell's draw offers, given
    /// the cell's sampleable ids in ascending order: the draw sees only the
    /// `CONCENTRATION_WINDOW` greatest of them.
    fn cheapest_offered<'a>(
        &self,
        sampleable: impl DoubleEndedIterator<Item = &'a usize>,
    ) -> Option<u64> {
        sampleable
            .rev()
            .take(CONCENTRATION_WINDOW)
            .map(|id| self.time_in_group[*id])
            .min()
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
        cell: Vec<usize>,
    ) -> Result<(usize, ConcentrationDraw), Box<dyn Error>> {
        let window = &cell[cell.len().saturating_sub(CONCENTRATION_WINDOW)..];
        let mut entered_window = 0_u64;
        for id in window {
            if !self.in_window_ever[*id] {
                self.in_window_ever[*id] = true;
                entered_window = entered_window.saturating_add(1);
            }
        }
        let id = if matches!(
            self.selector_policy,
            SelectorPolicy::EnergyFrontierCheapest(_)
        ) {
            let mut ranked = window
                .iter()
                .map(|id| (self.time_in_group[*id], *id))
                .collect::<Vec<_>>();
            ranked.sort_unstable();
            let weights = (0..ranked.len())
                .map(|rank| 256_usize >> (rank / CHEAPEST_RANK_SCALE).min(8))
                .collect::<Vec<_>>();
            let total =
                NonZeroUsize::new(weights.iter().sum()).ok_or("cheapest weights sum to zero")?;
            let mut draw = rand.below(total);
            let mut chosen = ranked[0].1;
            for (weight, (_, id)) in weights.iter().zip(&ranked) {
                if draw < *weight {
                    chosen = *id;
                    break;
                }
                draw -= weight;
            }
            chosen
        } else {
            window[rand.below(NonZeroUsize::new(window.len()).ok_or("empty tie window")?)]
        };
        Ok((
            id,
            ConcentrationDraw {
                window_size: u64::try_from(window.len())?,
                entered_window,
            },
        ))
    }

    /// Whether entry `id` was the first to occupy its retention slot.
    #[must_use]
    pub fn opened_new_slot(&self, id: usize) -> bool {
        self.opened_slot.get(id).copied().unwrap_or(false)
    }

    /// Whether entry `id` was the first to occupy its selection cell. Cell
    /// novelty pools out the fingerprint bits, so per-pose noise variants do
    /// not count as discovery.
    #[must_use]
    pub fn opened_new_cell(&self, id: usize) -> bool {
        self.opened_cell.get(id).copied().unwrap_or(false)
    }

    /// Number of entries currently participating in retention.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active_count
    }

    /// Number of machine snapshots currently resident for dispatch.
    #[must_use]
    pub fn resident_snapshot_count(&self) -> usize {
        self.resident_snapshots
    }

    /// Deterministic logical bytes charged to resident snapshots.
    #[must_use]
    pub fn resident_snapshot_bytes(&self) -> usize {
        self.resident_snapshot_bytes
    }

    /// Snapshots displaced solely by the global memory budget.
    #[must_use]
    pub fn snapshot_evictions(&self) -> u64 {
        self.snapshot_evictions
    }

    /// Compact entries currently held by the live acceleration structure.
    #[must_use]
    pub fn live_entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Deterministic history compactions completed so far.
    #[must_use]
    pub fn history_compactions(&self) -> u64 {
        self.history_compactions
    }

    /// Historical entries retired from live memory after their stream record.
    #[must_use]
    pub fn historical_entries_dropped(&self) -> u64 {
        self.historical_entries_dropped
    }

    /// Full inputs lazily reconstructed from the compact prefix structure.
    #[must_use]
    pub fn input_reconstructions(&self) -> u64 {
        self.input_reconstructions.get()
    }

    /// Complete input action elements retained across historical reports.
    #[must_use]
    pub(crate) fn historical_input_actions(&self) -> usize {
        self.historical_input_actions
    }

    /// Parent-relative action elements physically retained by live history.
    #[must_use]
    pub(crate) fn stored_input_actions(&self) -> usize {
        self.stored_input_actions
    }

    /// Deterministic conservative bytes charged to compact history/indexes.
    #[must_use]
    pub fn history_memory_bytes(&self) -> usize {
        self.history_memory_bytes
            .saturating_add(self.auxiliary_history_memory_bytes())
    }

    /// Deterministic bytes charged to compact entry metadata, excluding its
    /// action-prefix storage and historical selector maps.
    #[must_use]
    pub(crate) fn entry_metadata_memory_bytes(&self) -> usize {
        self.entries
            .len()
            .saturating_mul(Self::history_entry_memory_charge(0, 0))
    }

    /// Deterministic bytes charged to the shared action-prefix index.
    #[must_use]
    pub(crate) fn input_index_memory_bytes(&self) -> usize {
        self.input_index
            .live_nodes
            .saturating_mul(Self::prefix_node_memory_charge())
            .saturating_add(self.stored_input_actions.saturating_mul(size_of::<A>()))
    }

    /// Deterministic bytes charged to remembered novelty cells.
    #[must_use]
    pub(crate) fn novelty_memory_bytes(&self) -> usize {
        self.cells_seen
            .len()
            .saturating_mul(Self::historical_group_memory_charge(0))
    }

    /// Deterministic bytes charged to pooled selector-barrenness counters.
    #[must_use]
    pub(crate) fn barren_memory_bytes(&self) -> usize {
        self.group_barren
            .iter()
            .map(BTreeMap::len)
            .sum::<usize>()
            .saturating_mul(Self::historical_group_memory_charge(size_of::<u64>()))
    }

    /// Total deterministic bytes charged to live search state.
    #[must_use]
    pub fn resident_memory_bytes(&self) -> usize {
        self.history_memory_bytes()
            .saturating_add(self.resident_snapshot_bytes)
    }

    /// Unique action-prefix nodes retained by duplicate detection.
    #[must_use]
    pub(crate) fn input_index_nodes(&self) -> usize {
        self.input_index.live_nodes
    }

    /// Distinct selection cells remembered for historical novelty accounting.
    #[must_use]
    pub(crate) fn historical_cell_count(&self) -> usize {
        self.cells_seen.len()
    }

    /// Pooled selector groups carrying a live barren counter.
    #[must_use]
    pub(crate) fn barren_group_count(&self) -> usize {
        self.group_barren.iter().map(BTreeMap::len).sum()
    }

    /// Account one recorded selection of `id`.
    pub fn record_selection(&mut self, id: usize, draw: &SelectorDraw) {
        // The reset-marked draw is the only place streak counters clear.
        // Applying it here, in stream order, keeps counter state a pure
        // function of the record stream, so live and replay agree at every
        // stream position. Retirement is soft: the reset also clears the
        // pooled barren counters, so the search can never seal itself out.
        if draw.counter_reset {
            self.since_retained.fill(0);
            for map in &mut self.group_barren {
                map.clear();
            }
            self.rebuild_live_selector_index();
        }
        let was_sampleable = self.entry_unexhausted(id);
        self.selected[id] = self.selected[id].saturating_add(1);
        self.since_retained[id] = self.since_retained[id].saturating_add(1);
        let key = self.entries[id].key;
        if let Some(members) = self
            .classes
            .get_mut(&Reverse(key.group(Self::class_depth())))
            .and_then(|cells| cells.get_mut(&key.group(Self::cell_depth())))
            && members.ids.contains(&id)
        {
            members.drawn = members.drawn.saturating_add(1);
        }
        if was_sampleable && !self.entry_unexhausted(id) {
            self.set_entry_sampleable(id, false);
        }
        if matches!(
            self.selector_policy,
            SelectorPolicy::Retire(_)
                | SelectorPolicy::Energy(_)
                | SelectorPolicy::EnergyFrontier(_)
                | SelectorPolicy::EnergyFrontierCheapest(_)
        ) {
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
            SelectorPath::GroupWalk => {
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
    pub fn record_selection_outcome(
        &mut self,
        id: usize,
        retained_descendant: bool,
        new_slot_descendant: bool,
        new_cell_descendant: bool,
    ) {
        if !retained_descendant {
            return;
        }
        let was_sampleable = self.entry_unexhausted(id);
        self.productive[id] = self.productive[id].saturating_add(1);
        self.since_retained[id] = 0;
        if !was_sampleable && self.entry_unexhausted(id) {
            self.set_entry_sampleable(id, true);
        }
        // Retire clears its pooled counters on any retained child so
        // historical streams replay unchanged; energy clears only when a
        // child opened a new retention slot, because admission that keeps
        // most results makes plain retention too common to signal anything.
        let clears_groups = match self.selector_policy {
            SelectorPolicy::Retire(_) => true,
            SelectorPolicy::Energy(_) | SelectorPolicy::EnergyFrontier(_) => new_slot_descendant,
            SelectorPolicy::EnergyFrontierCheapest(_) => new_cell_descendant,
            SelectorPolicy::GroupUniform => false,
        };
        if clears_groups {
            let key = self.entries[id].key;
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
        if let SelectorPolicy::Retire(thresholds)
        | SelectorPolicy::Energy(thresholds)
        | SelectorPolicy::EnergyFrontier(thresholds)
        | SelectorPolicy::EnergyFrontierCheapest(thresholds) = &self.selector_policy
        {
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

    /// Extract entry reports and snapshots together, stamping per-entry
    /// selection counters without cloning the snapshot set.
    #[allow(clippy::type_complexity)] // The paired vectors preserve one-pass entry ownership.
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
    use super::{
        ActiveIds, Archive, ArchiveCandidate, ArchiveKey, HISTORY_COMPACTION_MIN_DROPS, Input,
        InputIndex, MAX_ENTRIES_PER_KEY, RetireThresholds, SELECTION_EXHAUSTION_THRESHOLD,
        SelectorAccounting, SelectorDraw, SelectorPath, SelectorPolicy,
        selector_policy_from_identifier,
    };
    use crate::search::rand::RomuDuoJrRand;
    use crate::smb::archive::{MAX_SMB_COMPLETION_ACTIONS, SmbArchiveKey};
    use crate::smb::target::ButtonChord;
    use serde::{Deserialize, Serialize};
    use std::{cmp::Reverse, collections::BTreeMap, sync::Arc};

    type TestArchive = Archive<u8, SmbArchiveKey, (), ()>;

    #[test]
    fn active_ids_preserve_ascending_rank_across_word_boundaries() {
        let ids = [0, 1, 63, 64, 65, 127, 128, 191, 255];
        let active = ActiveIds::from_ids(ids);
        assert_eq!(active.len(), ids.len());
        assert_eq!(active.ids().collect::<Vec<_>>(), ids);
        for (rank, id) in ids.into_iter().enumerate() {
            assert_eq!(active.select(rank), Some(id));
        }
        assert_eq!(active.select(ids.len()), None);
    }

    #[test]
    fn active_ids_remove_without_changing_survivor_ranks() {
        let mut active = ActiveIds::from_ids(0..260);
        for id in [0, 2, 63, 64, 129, 200, 259] {
            assert!(active.remove(id));
            assert!(!active.remove(id));
        }
        active.insert(64);
        active.insert(259);
        active.insert(259);
        let expected = (0..260)
            .filter(|id| ![0, 2, 63, 129, 200].contains(id))
            .collect::<Vec<_>>();
        assert_eq!(active.len(), expected.len());
        assert_eq!(active.ids().collect::<Vec<_>>(), expected);
        assert_eq!(
            (0..active.len())
                .map(|rank| active.select(rank).expect("rank is in bounds"))
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn active_ids_empty_after_removing_every_member() {
        let mut active = ActiveIds::from_ids([0, 64, 129]);
        assert!(!active.is_empty());
        for id in [0, 64, 129] {
            assert!(active.remove(id));
        }
        assert!(active.is_empty());
        assert_eq!(active.len(), 0);
        assert_eq!(active.select(0), None);
        assert_eq!(active.ids().next(), None);
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
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
        assert_eq!(archive.snapshot_charge(&()), 0);
        archive.set_memory_budget(1024, |_| 17);
        assert_eq!(archive.snapshot_charge(&()), 17);
    }

    #[test]
    fn snapshot_release_preserves_an_inflight_worker_reference() {
        let mut archive = flat_archive::<3>(&[[1, 2, 3, 4]]);
        let in_flight = Arc::clone(
            archive.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );

        assert!(archive.release_snapshot(0, false));
        assert!(archive.entries[0].snapshot.is_some());
        assert_eq!(Arc::strong_count(&in_flight), 2);
    }

    #[test]
    fn historical_tie_class_counter_loads_as_cell_selections() {
        let accounting: SelectorAccounting = serde_json::from_str(
            r#"{"uniform_selections":1,"tie_class_selections":2,
                "productive_selections":3,"classes_skipped":4,"counter_resets":5,
                "concentration":{"window_cap":128,"final_window_size":6,
                "window_draws":7,"distinct_window_parents":8,
                "draws_per_parent_milli":9}}"#,
        )
        .expect("historical selector accounting parses");
        assert_eq!(accounting.cell_selections, 2);
    }

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
            assert!(
                depth < DEPTHS,
                "group depth {depth} is past {DEPTHS} depths"
            );
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
    fn memory_budget_evicts_the_oldest_snapshot_without_freezing_admission() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
        let root_charge = Archive::<u8, FlatKey<3>, (), ()>::prefix_node_memory_charge();
        let history_charge =
            3 * Archive::<u8, FlatKey<3>, (), ()>::history_entry_memory_charge(1, 1);
        let novelty_charge =
            3 * Archive::<u8, FlatKey<3>, (), ()>::historical_group_memory_charge(0);
        archive.set_memory_budget(root_charge + history_charge + novelty_charge + 2, |_| 1);
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
        assert_eq!(archive.snapshot_evictions(), 1);
        assert_eq!(
            archive.history_memory_bytes(),
            archive
                .entry_metadata_memory_bytes()
                .saturating_add(archive.input_index_memory_bytes())
                .saturating_add(archive.novelty_memory_bytes())
                .saturating_add(archive.barren_memory_bytes())
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
                .select_parent(&mut rand, 8)
                .expect("select from budgeted residents");
            assert_ne!(selected, 0);
        }
    }

    #[test]
    fn budget_enforcement_reaches_the_limit_but_keeps_one_snapshot() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        assert_eq!(archive.snapshot_evictions(), 2);

        archive.memory_limit = Some(0);
        archive
            .enforce_snapshot_memory_budget()
            .expect("budget remains satisfiable");
        assert_eq!(archive.resident_snapshot_count(), 1);
        assert_eq!(archive.snapshot_evictions(), 3);
    }

    #[test]
    fn oldest_selectable_release_skips_dead_order_entries_and_reports_exhaustion() {
        let mut empty = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
        assert!(!empty.release_oldest_selectable(true));

        let mut archive = flat_archive::<3>(&[[0, 0, 0, 0], [1, 1, 0, 0]]);
        assert!(archive.release_snapshot(0, true));
        assert!(archive.release_oldest_selectable(true));
        assert_eq!(archive.resident_snapshot_count(), 0);
        assert_eq!(archive.snapshot_evictions(), 2);
        assert!(!archive.release_oldest_selectable(true));
    }

    #[test]
    fn selector_reactivates_a_budgeted_executable_anchor() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        archive.rebuild_selector_index(1);
        archive.establish_liveness_anchor(1);
        archive.time_in_group[0] = 100;
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
        let (selected, _) = archive
            .select_parent(&mut rand, 1)
            .expect("budgeted anchor keeps selection live");
        assert_eq!(archive.entries[selected].input_len, 0);
        assert!(archive.entries[selected].input_len < 1);
        assert!(
            archive
                .slots
                .values()
                .all(|members| members.len() <= MAX_ENTRIES_PER_KEY)
        );
    }

    #[test]
    fn budgeted_anchor_yields_to_single_entry_admission_and_reactivates() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        archive.rebuild_selector_index(1);
        archive.establish_liveness_anchor(1);
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
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_cafe);
        let (selected, _) = archive
            .select_parent(&mut rand, 1)
            .expect("displaced anchor reactivates");
        assert_eq!(archive.entries[selected].id, 0);
        assert_eq!(archive.active_count(), 1);
        assert!(archive.entries[selected].input_len < 1);
    }

    fn archive_with_prunable_history() -> Archive<u8, FlatKey<3>, (), ()> {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
            assert!(archive.release_snapshot(index, true));
        }
        archive
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
        let mut archive = flat_archive::<3>(&[[0, 0, 0, 0], [1, 1, 0, 0]]);
        assert!(archive.release_snapshot(0, true));
        archive.memory_limit = Some(1);
        archive
            .compact_history_if_needed()
            .expect("a sub-threshold dead tail is left for a later batch");
        assert_eq!(archive.history_compactions(), 0);
        assert_eq!(archive.live_entry_count(), 2);
    }

    #[test]
    fn final_compaction_keeps_only_valid_inactive_snapshot_owners() {
        let mut held = flat_archive::<3>(&[[0, 0, 0, 0]]);
        let held_id = held.stable_id(0).expect("stable id");
        let worker = Arc::clone(
            held.entries[0]
                .snapshot
                .as_ref()
                .expect("resident snapshot"),
        );
        assert!(held.release_snapshot(0, true));
        held.compact_history(true)
            .expect("worker-owned snapshot survives final compaction");
        assert!(held.index_of_id(held_id).is_some());
        assert_eq!(Arc::strong_count(&worker), 2);

        let mut preserved = flat_archive::<3>(&[[0, 0, 0, 0]]);
        let preserved_id = preserved.stable_id(0).expect("stable id");
        preserved
            .preserve_inactive_snapshots(true)
            .expect("enable inactive snapshot preservation");
        assert!(preserved.release_snapshot(0, true));
        preserved
            .compact_history(true)
            .expect("explicitly preserved snapshot survives final compaction");
        assert!(preserved.index_of_id(preserved_id).is_some());

        let mut empty = flat_archive::<3>(&[[0, 0, 0, 0]]);
        assert!(empty.release_snapshot(0, true));
        empty.preserve_inactive_snapshots = true;
        empty
            .compact_history(true)
            .expect("a preservation flag without a snapshot retains nothing");
        assert_eq!(empty.live_entry_count(), 0);
    }

    #[test]
    fn inactive_snapshot_and_metadata_preservation_are_reversible() {
        let mut archive = flat_archive::<3>(&[[0, 0, 0, 0]]);
        let stable_id = archive.stable_id(0).expect("stable id");
        assert!(!archive.preserves_inactive_snapshots());
        archive
            .preserve_inactive_snapshots(true)
            .expect("enable inactive snapshot preservation");
        assert!(archive.preserves_inactive_snapshots());
        assert!(archive.release_snapshot(0, true));
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
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        assert!(Archive::<u8, FlatKey<3>, (), ()>::prefix_node_memory_charge() > 1);
        assert!(Archive::<u8, FlatKey<3>, (), ()>::historical_group_memory_charge(8) > 1);
        archive.group_barren[0].insert(archive.entries[root].key.group(1), 1);
        assert!(archive.barren_memory_bytes() > 1);

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
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|action| u64::from(*action));
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
            let expected = match index {
                0 | 1 => Some((FlatKey([1, 0, 0, 0]), 5, u64::try_from(index + 1).unwrap())),
                2 | 3 => Some((FlatKey([2, 0, 0, 0]), 7, u64::try_from(index + 1).unwrap())),
                _ => unreachable!(),
            };
            assert_eq!(archive.live_progress(), expected);
        }
        assert_eq!(archive.live_progress(), Some((FlatKey([2, 0, 0, 0]), 7, 4)));
        assert!(archive.active_ids(1).is_empty());
        archive.active[1] = false;
        archive.snapshot_selectable[2] = false;
        assert_eq!(archive.active_ids(2), vec![0, 3]);
    }

    #[test]
    fn live_donor_index_tracks_a_new_deepest_descendant() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        archive.rebuild_selector_index(8);
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

        let members = archive
            .classes
            .get(&Reverse(
                root_key.group(Archive::<u8, FlatKey<3>, (), ()>::class_depth()),
            ))
            .and_then(|cells| {
                cells.get(&root_key.group(Archive::<u8, FlatKey<3>, (), ()>::cell_depth()))
            })
            .expect("root selection cell");
        assert!(members.donors.contains(&(child_key, child, root)));
        assert!(!members.donors.contains(&(root_key, root, root)));
    }

    #[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
    struct LineageKey {
        value: u8,
        class: u8,
    }

    impl ArchiveKey for LineageKey {
        type Group = (u8, u8);

        fn groups() -> usize {
            2
        }

        fn group(self, depth: usize) -> Self::Group {
            match depth {
                0 => (self.value, self.class),
                1 => (0, self.class),
                _ => panic!("unexpected lineage-key depth"),
            }
        }

        type Lineage = Vec<u8>;

        fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
            self
        }

        fn record(lineage: &mut Self::Lineage, key: Self) {
            lineage.push(key.value);
        }
    }

    #[test]
    fn lineage_is_inherited_only_within_the_coarsest_group() {
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
        assert_eq!(archive.lineage(different), Some(&vec![3]));
    }

    #[test]
    fn compaction_preserves_an_evicted_inflight_parents_input_prefix() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
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
        let dead_group = archive.entries[parent].key.group(1);
        let survivor_group = archive.entries[survivor].key.group(1);
        archive.group_barren[0].insert(dead_group, 9);
        archive.group_barren[0].insert(survivor_group, 3);
        assert!(archive.historical_cell_count() > 1);
        assert_eq!(archive.barren_group_count(), 2);

        for index in 0..survivor {
            assert!(archive.release_snapshot(index, true));
        }
        archive.memory_limit = Some(4);
        archive
            .compact_history_if_needed()
            .expect("compact pinned input history");

        let parent = archive
            .index_of_id(parent_id)
            .expect("pinned parent remains");
        assert_eq!(archive.historical_cell_count(), 1);
        assert_eq!(archive.barren_group_count(), 1);
        assert_eq!(
            archive.group_barren[0].get(&survivor_group),
            Some(&3),
            "compaction preserves live breeding-population counters"
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
    fn live_hierarchy_matches_the_original_cell_scan_across_exhaustion() {
        let keys = [
            [0, 0, 0, 0],
            [1, 0, 0, 0],
            [0, 1, 0, 0],
            [1, 1, 0, 0],
            [0, 0, 1, 0],
            [1, 0, 1, 0],
            [0, 1, 1, 0],
            [1, 1, 1, 0],
            [0, 0, 0, 1],
            [1, 0, 0, 1],
            [0, 1, 0, 1],
            [1, 1, 0, 1],
        ];
        let mut archive = flat_archive::<5>(&keys);
        archive.selector_policy = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 2,
            groups: vec![3, 4, 5],
        });
        archive.rebuild_selector_index(10);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_cace);
        for step in 0..100 {
            let mut cached_rand = rand;
            let mut scanned_rand = rand;
            let mut cached_skipped = 0;
            let mut scanned_skipped = 0;
            let cached = archive
                .walk_live_index(&mut cached_rand, &mut cached_skipped)
                .expect("cached walk");
            let scanned = archive
                .walk_to_cell_scan(&mut scanned_rand, &mut scanned_skipped, false)
                .expect("scanned walk");
            assert_eq!(cached, scanned);
            assert_eq!(cached_skipped, scanned_skipped);
            assert_eq!(cached_rand.next_u64(), scanned_rand.next_u64());
            rand = cached_rand;

            let Some(cached) = cached else {
                let draw = SelectorDraw {
                    path: SelectorPath::GroupWalk,
                    classes_skipped: cached_skipped,
                    counter_reset: true,
                    concentration: None,
                };
                archive.record_selection(0, &draw);
                continue;
            };
            let id = cached[step % cached.len()];
            let draw = SelectorDraw {
                path: SelectorPath::GroupWalk,
                classes_skipped: cached_skipped,
                counter_reset: false,
                concentration: None,
            };
            archive.record_selection(id, &draw);
            archive.record_selection_outcome(id, step % 3 == 0, false, false);
        }
    }

    #[test]
    fn displaced_inflight_parent_does_not_change_its_surviving_cell_count() {
        let mut archive = flat_archive::<5>(&[[0, 0, 0, 0], [1, 0, 0, 0]]);
        archive.selector_policy = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 1,
            groups: vec![3, 4, 5],
        });
        archive.rebuild_selector_index(10);

        // Model a result still in flight when an earlier admission displaces
        // its selected parent. Another entry keeps the same cell present.
        archive.active[0] = false;
        archive.index_remove(0);
        let draw = SelectorDraw {
            path: SelectorPath::GroupWalk,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        archive.record_selection(0, &draw);

        let mut cached_rand = RomuDuoJrRand::with_seed(0xd15f_1ace);
        let mut scanned_rand = cached_rand;
        let mut cached_skipped = 0;
        let mut scanned_skipped = 0;
        assert_eq!(
            archive
                .walk_live_index(&mut cached_rand, &mut cached_skipped)
                .expect("cached walk"),
            archive
                .walk_to_cell_scan(&mut scanned_rand, &mut scanned_skipped, false)
                .expect("scanned walk")
        );
        assert_eq!(cached_skipped, scanned_skipped);
        assert_eq!(cached_rand.next_u64(), scanned_rand.next_u64());

        archive.record_selection_outcome(0, true, true, true);
        let mut cached_rand = RomuDuoJrRand::with_seed(0x0af7_c0de);
        let mut scanned_rand = cached_rand;
        let mut cached_skipped = 0;
        let mut scanned_skipped = 0;
        assert_eq!(
            archive
                .walk_live_index(&mut cached_rand, &mut cached_skipped)
                .expect("cached walk after stale outcome"),
            archive
                .walk_to_cell_scan(&mut scanned_rand, &mut scanned_skipped, false)
                .expect("scanned walk after stale outcome")
        );
        assert_eq!(cached_skipped, scanned_skipped);
        assert_eq!(cached_rand.next_u64(), scanned_rand.next_u64());
    }

    /// A splice tail is the cell-mate's stored path to its deepest
    /// descendant, capped, and absent when no cell-mate reaches deeper than
    /// the parent.
    #[test]
    fn a_splice_tail_extends_past_the_parent_from_a_cell_mate() {
        let mut archive = Archive::<u8, FlatKey<3>, (), ()>::new(|_| 1);
        let insert = |archive: &mut Archive<u8, FlatKey<3>, (), ()>,
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
        let middle = insert(&mut archive, Some(root), [1, 2, 3, 6], vec![0, 1]);
        let leaf = insert(&mut archive, Some(middle), [1, 2, 3, 7], vec![0, 1, 2]);
        let arrival = insert(&mut archive, None, [0, 2, 3, 4], vec![9]);
        let dispatched = archive
            .splice_tail_for_campaign(arrival, MAX_SMB_COMPLETION_ACTIONS, 8)
            .expect("dispatch-time splice");
        assert_eq!((dispatched.donor_id, dispatched.leaf_id), (root, leaf));
        assert_eq!(dispatched.actions, vec![1, 2]);
        assert_eq!(
            archive
                .recorded_splice_tail(arrival, dispatched.donor_id, dispatched.leaf_id, 1)
                .expect("capped recorded splice"),
            vec![1]
        );
        let later = insert(&mut archive, Some(leaf), [1, 2, 3, 8], vec![0, 1, 2, 3]);
        assert_eq!(
            archive
                .splice_tail_for_campaign(arrival, MAX_SMB_COMPLETION_ACTIONS, 8)
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
            archive
                .splice_tail_for_campaign(leaf, MAX_SMB_COMPLETION_ACTIONS, 8)
                .is_none(),
            "the deepest entry has no deeper cell-mate"
        );
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
                selector_policy_from_identifier("room_cell_uniform_128_retire:3,6", pooled)
                    .is_err()
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
                time_bucket: 0,
                loop_standing: 15,
                room: [0; 3],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: input.actions,
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
            entry.key.room = [3, 5, 16];
            entry.key.player_y_bucket = match index {
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
            if draw.path != SelectorPath::GroupWalk {
                continue;
            }
            cell_draws += 1;
            let key = archive.entries[id].key;
            *per_cell
                .entry((key.progress, key.player_y_bucket))
                .or_default() += 1;
            archive.record_selection(id, &draw);
            archive.record_selection_outcome(id, true, true, true);
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
            path: SelectorPath::GroupWalk,
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
            if draw.path == SelectorPath::GroupWalk {
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
        archive.record_selection_outcome(1, true, true, true);
        let mut upper_band_seen = false;
        for _ in 0..64 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("selection after reset");
            if draw.path == SelectorPath::GroupWalk && (id == 0 || id == 1) {
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
    fn an_energy_barren_band_fades_but_keeps_receiving_draws() {
        // Same shape as the retirement test: two cells in one band plus one
        // band below. Under the energy selector a deeply barren band must
        // keep a small draw share instead of being skipped outright.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 145), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        archive.selector_policy = SelectorPolicy::Energy(RetireThresholds {
            entry: 1_024,
            groups: vec![1_024, 1, 1_024],
        });
        let barren_draw = SelectorDraw {
            path: SelectorPath::GroupWalk,
            classes_skipped: 0,
            counter_reset: false,
            concentration: None,
        };
        // Nine barren selections put the upper band at nine halvings' worth
        // of barrenness, clamped to the 1/256 floor.
        for _ in 0..9 {
            archive.record_selection(0, &barren_draw);
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e32);
        let mut upper = 0_u64;
        let mut lower = 0_u64;
        let mut walks = 0_u64;
        for _ in 0..4_096 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("energy selection");
            if draw.path != SelectorPath::GroupWalk {
                continue;
            }
            assert_eq!(draw.classes_skipped, 0);
            assert!(!draw.counter_reset);
            walks += 1;
            if id == 2 {
                lower += 1;
            } else {
                upper += 1;
            }
        }
        assert!(upper > 0, "a barren band must keep a floor share");
        assert!(
            upper * 20 < walks,
            "a barren band at the floor must fade: {upper}/{walks}"
        );
        assert!(lower * 2 > walks, "the fresh band must dominate");
    }

    #[test]
    fn the_cheapest_concentration_prefers_low_cost_cell_members() {
        // Forty entries in one cell whose costs rise with their ids: the
        // cost-weighted draw must concentrate on the cheapest ranks while
        // the plain frontier window stays uniform.
        let mut archive = TestArchive::new(|_| 1);
        for index in 0..40_u8 {
            let input = Input {
                actions: vec![0; usize::from(index) + 1],
            };
            let key = SmbArchiveKey {
                world: 1,
                level: 0,
                progress: 144,
                player_y_bucket: 0,
                player_engine_state: 0,
                state_fingerprint: index,
                room_x_bucket: 0,
                time_bucket: 0,
                loop_standing: 15,
                room: [0; 3],
            };
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        suffix: input.actions,
                        key,
                        milestones: (),
                    },
                    (),
                )
                .expect("insert cell entry");
        }
        archive.selector_policy = SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
            entry: 1_024,
            groups: vec![1_024, 1_024, 1_024],
        });
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e34);
        let cell = (0..40_usize).collect::<Vec<_>>();
        let mut cheapest_block = 0_u64;
        for _ in 0..4_096 {
            let (id, _) = archive
                .draw_from_cell(&mut rand, cell.clone())
                .expect("cheapest cell draw");
            if id < 16 {
                cheapest_block += 1;
            }
        }
        // Sixteen full-weight ranks against halved and quartered tails must
        // take well over the uniform 40% share.
        assert!(
            cheapest_block > 2_400,
            "cheapest ranks drew {cheapest_block}/4096"
        );
    }

    #[test]
    fn the_frontier_selector_weights_the_deepest_band_over_a_fresh_shallow_one() {
        // Two bands with zero barrenness everywhere: energy alone would draw
        // them evenly, so the frontier factor must be what separates them.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 145), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        archive.selector_policy = SelectorPolicy::EnergyFrontier(RetireThresholds {
            entry: 1_024,
            groups: vec![1_024, 1_024, 1_024],
        });
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e33);
        let mut deep = 0_u64;
        let mut shallow = 0_u64;
        let mut walks = 0_u64;
        for _ in 0..4_096 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("frontier selection");
            if draw.path != SelectorPath::GroupWalk {
                continue;
            }
            walks += 1;
            if id == 2 {
                shallow += 1;
            } else {
                deep += 1;
            }
        }
        assert!(shallow > 0, "the shallow band must keep a floor share");
        assert!(
            deep > shallow * 3 / 2,
            "the deepest band must dominate: {deep}/{shallow} of {walks}"
        );
    }

    #[test]
    fn a_retired_coarse_class_falls_to_the_reset_when_nothing_else_lives() {
        // One room only: a single barren draw retires it at a room
        // threshold of one, and the deterministic all-exhausted reset must
        // clear the pooled counters and free it rather than seal the search.
        let keys: Vec<(u8, u8, u16)> = vec![(1, 0, 144), (1, 0, 124)];
        let mut archive = selector_archive(&keys);
        for entry in &mut archive.entries {
            entry.key.room = [3, 5, 0];
        }
        archive.selector_policy = SelectorPolicy::Retire(RetireThresholds {
            entry: 64,
            groups: vec![64, 64, 1],
        });
        let barren_draw = SelectorDraw {
            path: SelectorPath::GroupWalk,
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
            if draw.path == SelectorPath::GroupWalk {
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
            path: SelectorPath::GroupWalk,
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
            if draw.path == SelectorPath::GroupWalk {
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
            path: SelectorPath::GroupWalk,
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
            if draw.path == SelectorPath::GroupWalk {
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
            entry.key.player_y_bucket = 0;
        }
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_5e21);
        let mut cell_draws = 0;
        for _ in 0..256 {
            let (id, draw) = archive
                .select_parent(&mut rand, MAX_SMB_COMPLETION_ACTIONS)
                .expect("concentrated selection");
            match draw.path {
                SelectorPath::GroupWalk => {
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
            entry.key.player_y_bucket = 0;
        }
        let exhausting_draw = SelectorDraw {
            path: SelectorPath::GroupWalk,
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
            if draw.path == SelectorPath::GroupWalk {
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
            time_bucket: 0,
            loop_standing: 15,
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
                    suffix: vec![ButtonChord::new(buttons, hold)],
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
                    suffix: Vec::new(),
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
                    suffix: Vec::new(),
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
            .expect("the eleven-frame route displaces a slower one");
        assert_eq!(archive.entry_time_in_group(admitted), 11);
        assert_eq!(archive.replacement_time_displaced(), 1);
        assert_eq!(archive.active_count(), 4);
        let (slower, _) = chain_insert(&mut archive, fast, &input, 0x04, 200, slot);
        assert!(
            slower.is_none(),
            "a slower route never displaces a faster one"
        );
        assert_eq!(archive.active_count(), 4);
    }
}
