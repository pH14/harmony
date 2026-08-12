// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic snapshot-backed quality-diversity search for SMB completion.

use std::{cmp::Reverse, collections::BTreeMap, error::Error, num::NonZeroUsize};

use libafl::executors::ExitKind;
use libafl_bolts::rands::{Rand, StdRand};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    phase4b::{
        ButtonChord, MAX_HOLD_FRAMES, MAX_SMB_ACTIONS, SmbInput, SmbMacro, SmbMilestoneInputs,
        SmbMilestoneTimes, SmbMilestones, SmbObservations, SmbProgressWatermark, SmbSnapshot,
        SmbTarget, smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
    target::Target,
};

const MAX_ARCHIVE_ENTRIES: usize = 32_768;
const MAX_ENTRIES_PER_KEY: usize = 2;
const FRONTIER_WINDOW: usize = 128;
const RANKING_REBUILD_INTERVAL: u64 = 512;
const RANKING_STALE_EXECUTIONS: u64 = 1_024;
const GENERATED_MUTATOR_RETIRE_AFTER: u64 = 128;
const FRONTIER_PROGRESS_BAND: u16 = 8;
const STATE_FINGERPRINT_MASK: u8 = 0x3f;
const FROZEN_BUTTON_MASKS: [u8; 9] = [0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10];

/// Largest bounded action horizon accepted by the completion-only archive.
pub const MAX_SMB_COMPLETION_ACTIONS: usize = 512;

/// Pure generated score used only to choose a replacement inside a full archive cell.
pub trait SmbRanking {
    /// Return one comparable deterministic score from one state's recorded observations.
    fn score(&self, observations: &[SmbObservations]) -> i64;
}

/// Mechanical accounting for one installed generated ranking.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRankingAccounting {
    /// Whether this campaign installed a ranking.
    pub installed: bool,
    /// Whether the ranking remained active at campaign end.
    pub active: bool,
    /// Full-cell replacements selected while the ranking was active.
    pub replacements: u64,
    /// New archive cells reached by descendants of ranking-selected replacements.
    pub descendant_novelty: u64,
    /// Execution-count rebuild at which an ineffective ranking was retired.
    pub retired_at_execution: Option<u64>,
}

/// Mechanical accounting for one installed generated archive mutator.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbGeneratedMutatorAccounting {
    /// Whether this campaign installed a generated mutator.
    pub installed: bool,
    /// Whether the mutator remained active at campaign end.
    pub active: bool,
    /// Times the host selected and invoked the generated mutator.
    pub attempts: u64,
    /// Changed, bounded candidates emitted by the generated mutator.
    pub offspring: u64,
    /// Emitted candidates that retained at least one archive state.
    pub retained_offspring: u64,
    /// Consecutive emitted candidates that retained no archive state.
    pub consecutive_nonretained: u64,
    /// Execution at which mechanical retirement occurred.
    pub retired_at_execution: Option<u64>,
}

/// Duration distribution used by completion suffix mutation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveDurationPolicy {
    /// Frozen H1 distribution: three quarters short, one quarter full-range.
    Legacy,
    /// Generic two-stratum distribution covering short control and long time horizons.
    Stratified,
}

/// Number of adjacent chords sampled for one archive expansion.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbArchiveSuffixPolicy {
    /// Frozen H1 behavior: one chord with probability 3/4, otherwise two.
    OneOrTwo,
    /// Bounded temporal burst: lengths one through four with geometric tail probabilities.
    BurstUpToFour,
}

/// Frozen search parameters used by a generated-ranking archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbRankingSearchConfig {
    /// Maximum clean-reset action count.
    pub max_actions: usize,
    /// Seeded hold-duration distribution.
    pub duration_policy: SmbArchiveDurationPolicy,
    /// Seeded adjacent-chord distribution.
    pub suffix_policy: SmbArchiveSuffixPolicy,
}

/// One bounded quality-diversity cell for an action-boundary snapshot.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct SmbArchiveKey {
    /// Mechanical world number.
    pub world: u8,
    /// Mechanical level number.
    pub level: u8,
    /// Current 16-pixel progress bucket.
    pub progress: u16,
    /// Coarse player vertical-position bucket.
    pub player_y_bucket: u8,
    /// Mechanical player engine state.
    pub player_engine_state: u8,
    /// Six-bit deterministic fingerprint of otherwise-hidden work RAM state.
    pub state_fingerprint: u8,
}

/// Serializable lineage and retention record for one archived testcase.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveEntryReport {
    /// Stable insertion-order archive identifier.
    pub id: u64,
    /// Archive parent selected for the suffix execution.
    pub parent_id: Option<u64>,
    /// Target execution that created the entry; zero denotes bootstrap.
    pub created_execution: u64,
    /// Complete clean-reset input represented by this snapshot.
    pub input: SmbInput,
    /// Route-agnostic quality-diversity key.
    pub key: SmbArchiveKey,
    /// Strongest milestones observed along this input.
    pub milestones: SmbMilestones,
}

/// Deterministic progress sample from one archive campaign.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveProgressPoint {
    /// Completed target executions.
    pub executions: u64,
    /// Strongest milestone state observed so far.
    pub milestones: SmbMilestones,
    /// Number of active retained archive entries.
    pub active_entries: usize,
    /// Number of occupied quality-diversity cells.
    pub occupied_cells: usize,
    /// Number of terminal death transitions seen so far.
    pub deaths: u64,
}

/// Complete deterministic report for one snapshot-backed suffix-search campaign.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbArchiveReport {
    /// Caller-provided seeded RNG value.
    pub seed: u64,
    /// Number of suffix target executions.
    pub executions: u64,
    /// Strongest milestone values reached.
    pub milestones: SmbMilestones,
    /// Furthest per-frame mechanical position, including action interiors.
    #[serde(default)]
    pub progress_watermark: SmbProgressWatermark,
    /// First execution reaching each frozen milestone rung.
    pub first_reached: SmbMilestoneTimes,
    /// First clean-reset input reaching each rung.
    pub first_inputs: SmbMilestoneInputs,
    /// Current best clean-reset input.
    pub champion_input: SmbInput,
    /// Insertion and replacement records for retained testcases.
    pub entries: Vec<SmbArchiveEntryReport>,
    /// Fixed-interval deterministic progress curve.
    pub progress_curve: Vec<SmbArchiveProgressPoint>,
    /// Candidate snapshots admitted to the active archive.
    pub retained: u64,
    /// Candidate snapshots rejected by bounded quality-diversity retention.
    pub rejected: u64,
    /// Terminal death transitions observed.
    pub deaths: u64,
    /// Execution-count accounting for the optional generated ranking.
    #[serde(default)]
    pub ranking: SmbRankingAccounting,
    /// Execution-count accounting for the optional generated archive mutator.
    #[serde(default)]
    pub generated_mutator: SmbGeneratedMutatorAccounting,
}

/// Mechanical outcome of one fixed frontier-viability continuation.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbViabilityClass {
    /// The continuation reached player engine kill state `$0b`.
    KillState,
    /// The continuation ended in vertical bucket 15 without registering kill.
    BelowPlayable,
    /// The continuation ended outside the two doomed classes.
    Controllable,
}

/// Viability result for one active archive representative.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityEntry {
    /// Stable source archive identifier.
    pub id: u64,
    /// Whether this entry belongs to the maximal progress-39 frontier.
    pub frontier: bool,
    /// Recorded archive key at the audited endpoint.
    pub key: SmbArchiveKey,
    /// No-input continuation followed by the nine frozen controller masks.
    pub continuations: Vec<SmbViabilityClass>,
    /// True only when no continuation remains controllable.
    pub doomed: bool,
}

/// Count summary for one audited archive slice.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbViabilityCounts {
    /// Audited active representatives.
    pub total: u64,
    /// Representatives with no controllable continuation.
    pub doomed: u64,
}

/// Deterministic D27 frontier-viability report.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbFrontierViabilityReport {
    /// Frames applied by every fixed continuation.
    pub continuation_frames: u8,
    /// No-input plus frozen mask order used by every entry.
    pub continuation_masks: Vec<Option<u8>>,
    /// Maximal progress-39 counts.
    pub frontier: SmbViabilityCounts,
    /// Inclusive progress-32-through-39 approach-band counts.
    pub approach_band: SmbViabilityCounts,
    /// Stable per-entry evidence in input lexical order.
    pub entries: Vec<SmbViabilityEntry>,
}

#[derive(Clone, Debug)]
struct ArchiveEntry {
    report: SmbArchiveEntryReport,
    snapshot: SmbSnapshot,
    observations: Vec<SmbObservations>,
    ranking_lineage: bool,
}

struct ArchiveCandidate {
    input: SmbInput,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
}

struct Archive<'a> {
    entries: Vec<ArchiveEntry>,
    active: Vec<bool>,
    cells: BTreeMap<SmbArchiveKey, Vec<usize>>,
    input_ids: BTreeMap<SmbInput, usize>,
    retained: u64,
    rejected: u64,
    ranking: Option<&'a dyn SmbRanking>,
    ranking_accounting: SmbRankingAccounting,
    first_ranking_replacement: Option<u64>,
    last_descendant_novelty: Option<u64>,
    experimental_search: bool,
}

impl<'a> Archive<'a> {
    fn new(ranking: Option<&'a dyn SmbRanking>, experimental_search: bool) -> Self {
        Self {
            entries: Vec::new(),
            active: Vec::new(),
            cells: BTreeMap::new(),
            input_ids: BTreeMap::new(),
            retained: 0,
            rejected: 0,
            ranking,
            ranking_accounting: SmbRankingAccounting {
                installed: ranking.is_some(),
                active: ranking.is_some(),
                ..SmbRankingAccounting::default()
            },
            first_ranking_replacement: None,
            last_descendant_novelty: None,
            experimental_search,
        }
    }

    fn insert(
        &mut self,
        parent_id: Option<usize>,
        execution: u64,
        candidate: ArchiveCandidate,
        snapshot: SmbSnapshot,
        observations: &[SmbObservations],
    ) -> Result<Option<usize>, Box<dyn Error>> {
        let ArchiveCandidate {
            input,
            key,
            milestones,
        } = candidate;
        if let Some(existing) = self.input_ids.get(&input) {
            return Ok(Some(*existing));
        }
        let cell = self.cells.entry(key).or_default();
        let new_cell = cell.is_empty();
        let mut ranking_replacement = false;
        let replace = if cell.len() < MAX_ENTRIES_PER_KEY {
            None
        } else if self.ranking_accounting.active {
            let ranking = self.ranking.ok_or("active SMB ranking is missing")?;
            let candidate_quality = (ranking.score(observations), Reverse(input.actions.len()));
            cell.iter()
                .copied()
                .min_by_key(|id| {
                    (
                        ranking.score(&self.entries[*id].observations),
                        Reverse(self.entries[*id].report.input.actions.len()),
                    )
                })
                .filter(|id| {
                    let existing_quality = (
                        ranking.score(&self.entries[*id].observations),
                        Reverse(self.entries[*id].report.input.actions.len()),
                    );
                    candidate_quality > existing_quality
                })
                .inspect(|_| ranking_replacement = true)
        } else {
            cell.iter()
                .copied()
                .max_by_key(|id| entry_cost(&self.entries[*id].report))
                .filter(|id| input.actions.len() < self.entries[*id].report.input.actions.len())
        };
        if cell.len() >= MAX_ENTRIES_PER_KEY && replace.is_none() {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if self.entries.len() >= MAX_ARCHIVE_ENTRIES {
            self.rejected = self.rejected.saturating_add(1);
            return Ok(None);
        }
        if let Some(replaced) = replace {
            self.active[replaced] = false;
            cell.retain(|id| *id != replaced);
        }
        let parent_ranking_lineage = parent_id
            .and_then(|id| self.entries.get(id))
            .is_some_and(|entry| entry.ranking_lineage);
        let ranking_lineage = parent_ranking_lineage || ranking_replacement;
        if ranking_replacement {
            self.ranking_accounting.replacements =
                self.ranking_accounting.replacements.saturating_add(1);
            self.first_ranking_replacement.get_or_insert(execution);
        }
        if new_cell && parent_ranking_lineage && execution > 0 {
            self.ranking_accounting.descendant_novelty =
                self.ranking_accounting.descendant_novelty.saturating_add(1);
            self.last_descendant_novelty = Some(execution);
        }
        let id = self.entries.len();
        let report = SmbArchiveEntryReport {
            id: u64::try_from(id)?,
            parent_id: parent_id.map(u64::try_from).transpose()?,
            created_execution: execution,
            input: input.clone(),
            key,
            milestones,
        };
        self.entries.push(ArchiveEntry {
            report,
            snapshot,
            observations: observations.to_vec(),
            ranking_lineage,
        });
        self.active.push(true);
        cell.push(id);
        self.input_ids.insert(input, id);
        self.retained = self.retained.saturating_add(1);
        Ok(Some(id))
    }

    fn finish_execution(&mut self, execution: u64) {
        if !self.ranking_accounting.active || !execution.is_multiple_of(RANKING_REBUILD_INTERVAL) {
            return;
        }
        let Some(first_replacement) = self.first_ranking_replacement else {
            return;
        };
        let last_gain = self.last_descendant_novelty.unwrap_or(first_replacement);
        if execution.saturating_sub(last_gain) >= RANKING_STALE_EXECUTIONS {
            self.ranking_accounting.active = false;
            self.ranking_accounting.retired_at_execution = Some(execution);
        }
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

    fn choose_parent(
        &self,
        rand: &mut StdRand,
        max_actions: usize,
    ) -> Result<usize, Box<dyn Error>> {
        let active = self.active_ids(max_actions);
        if active.is_empty() {
            return Err("SMB archive has no expandable entry".into());
        }
        let use_frontier = rand.below(NonZeroUsize::new(4).ok_or("invalid frontier odds")?) != 0;
        if !use_frontier {
            return Ok(active[rand.below(NonZeroUsize::new(active.len()).ok_or("empty archive")?)]);
        }
        if !self.experimental_search {
            let mut ordered = active;
            ordered.sort_by_key(|id| {
                (
                    milestone_key(self.entries[*id].report.milestones),
                    self.entries[*id].report.key,
                    self.entries[*id].report.id,
                )
            });
            let start = ordered.len().saturating_sub(FRONTIER_WINDOW);
            let frontier = &ordered[start..];
            return Ok(
                frontier[rand.below(NonZeroUsize::new(frontier.len()).ok_or("empty frontier")?)]
            );
        }
        let best = active
            .iter()
            .map(|id| frontier_quality(&self.entries[*id].report))
            .max()
            .ok_or("empty frontier")?;
        let frontier = active
            .into_iter()
            .filter(|id| {
                let quality = frontier_quality(&self.entries[*id].report);
                quality.0 == best.0
                    && quality.1 == best.1
                    && quality.2 == best.2
                    && quality.3.saturating_add(FRONTIER_PROGRESS_BAND - 1) >= best.3
            })
            .collect::<Vec<_>>();
        Ok(frontier[rand.below(NonZeroUsize::new(frontier.len()).ok_or("empty frontier")?)])
    }
}

/// Run deterministic snapshot-backed short-horizon suffix search.
pub fn run_smb_archive_search(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_action_limit(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        MAX_SMB_ACTIONS,
    )
}

/// Audit whether active frontier and approach-band representatives can recover.
pub fn audit_smb_frontier_viability(
    rom: &[u8],
    source: &SmbArchiveReport,
) -> Result<SmbFrontierViabilityReport, Box<dyn Error>> {
    let active = active_source_entries(source);
    let mut selected = active
        .into_iter()
        .filter(|entry| {
            entry.key.world == 0 && entry.key.level == 2 && (32..=39).contains(&entry.key.progress)
        })
        .collect::<Vec<_>>();
    selected.sort_by_key(|entry| (entry.input.clone(), entry.id));
    let continuation_masks = std::iter::once(None)
        .chain(FROZEN_BUTTON_MASKS.into_iter().map(Some))
        .collect::<Vec<_>>();
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let genesis = target
        .snapshot()
        .ok_or("failed to snapshot audit genesis")?;
    let mut prior_input = SmbInput::default();
    let mut prior_snapshots = vec![genesis];
    let mut entries = Vec::with_capacity(selected.len());
    for entry in selected {
        let common = prior_input
            .actions
            .iter()
            .zip(&entry.input.actions)
            .take_while(|(left, right)| left == right)
            .count();
        target.restore(&prior_snapshots[common])?;
        prior_snapshots.truncate(common + 1);
        for action in &entry.input.actions[common..] {
            target.apply(action);
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot audit replay prefix")?;
            prior_snapshots.push(snapshot);
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
        }
        let endpoint = target
            .snapshot()
            .ok_or("failed to snapshot audit endpoint")?;
        let mut continuations = Vec::with_capacity(continuation_masks.len());
        for mask in &continuation_masks {
            target.restore(&endpoint)?;
            target.apply(&ButtonChord::new(mask.unwrap_or(0), 120));
            let state = smb_mechanical_state_from_wram(target.wram());
            continuations.push(if state.player_engine_state == 0x0b {
                SmbViabilityClass::KillState
            } else if state.player_y_bucket == 15 {
                SmbViabilityClass::BelowPlayable
            } else {
                SmbViabilityClass::Controllable
            });
        }
        let doomed = continuations
            .iter()
            .all(|class| *class != SmbViabilityClass::Controllable);
        entries.push(SmbViabilityEntry {
            id: entry.id,
            frontier: entry.key.progress == 39,
            key: entry.key,
            continuations,
            doomed,
        });
        prior_input = entry.input.clone();
    }
    let counts = |frontier: bool| {
        let matching = entries.iter().filter(|entry| entry.frontier == frontier);
        SmbViabilityCounts {
            total: u64::try_from(matching.clone().count()).unwrap_or(u64::MAX),
            doomed: u64::try_from(matching.filter(|entry| entry.doomed).count())
                .unwrap_or(u64::MAX),
        }
    };
    let frontier = counts(true);
    let nonfrontier = counts(false);
    Ok(SmbFrontierViabilityReport {
        continuation_frames: 120,
        continuation_masks,
        frontier,
        approach_band: SmbViabilityCounts {
            total: frontier.total.saturating_add(nonfrontier.total),
            doomed: frontier.doomed.saturating_add(nonfrontier.doomed),
        },
        entries,
    })
}

fn active_source_entries(source: &SmbArchiveReport) -> Vec<&SmbArchiveEntryReport> {
    let mut cells = BTreeMap::<SmbArchiveKey, Vec<&SmbArchiveEntryReport>>::new();
    for entry in &source.entries {
        let cell = cells.entry(entry.key).or_default();
        if cell.len() < MAX_ENTRIES_PER_KEY {
            cell.push(entry);
            continue;
        }
        if let Some((index, existing)) = cell
            .iter()
            .enumerate()
            .max_by_key(|(_, existing)| entry_cost(existing))
            && entry.input.actions.len() < existing.input.actions.len()
        {
            cell[index] = entry;
        }
    }
    cells.into_values().flatten().collect()
}

/// Run completion search with an explicit bounded completion-only action horizon.
pub fn run_smb_archive_search_with_action_limit(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_config(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        SmbArchiveDurationPolicy::Legacy,
    )
}

/// Run completion search with explicit bounded horizon and duration policy.
pub fn run_smb_archive_search_with_config(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_with_config_and_suffix(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        SmbArchiveSuffixPolicy::OneOrTwo,
    )
}

/// Run frozen completion search with explicit bounded duration and suffix policies.
pub fn run_smb_archive_search_with_config_and_suffix(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        suffix_policy,
        None,
        None,
        false,
    )
}

/// Run completion search with explicit bounded duration and suffix policies.
pub fn run_smb_archive_search_with_policies(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        max_actions,
        duration_policy,
        suffix_policy,
        None,
        None,
        true,
    )
}

/// Run completion search with a generated ranking confined to full-cell replacement.
pub fn run_smb_archive_search_with_ranking<R: SmbRanking>(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    config: SmbRankingSearchConfig,
    ranking: &R,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        config.max_actions,
        config.duration_policy,
        config.suffix_policy,
        Some(ranking),
        None,
        false,
    )
}

/// Run frozen completion search with one bounded generated semantic mutator choice.
pub fn run_smb_archive_search_with_generated_mutator<M: SmbMacro>(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    config: SmbRankingSearchConfig,
    generated_mutator: &M,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    run_smb_archive_search_internal(
        rom,
        initial_inputs,
        seed,
        execution_budget,
        config.max_actions,
        config.duration_policy,
        config.suffix_policy,
        None,
        Some(generated_mutator),
        false,
    )
}

fn record_generated_mutator_result(
    accounting: &mut SmbGeneratedMutatorAccounting,
    retained: bool,
    execution: u64,
) {
    if retained {
        accounting.retained_offspring = accounting.retained_offspring.saturating_add(1);
        accounting.consecutive_nonretained = 0;
    } else {
        accounting.consecutive_nonretained = accounting.consecutive_nonretained.saturating_add(1);
        if accounting.consecutive_nonretained >= GENERATED_MUTATOR_RETIRE_AFTER {
            accounting.active = false;
            accounting.retired_at_execution = Some(execution);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_smb_archive_search_internal(
    rom: &[u8],
    initial_inputs: &[SmbInput],
    seed: u64,
    execution_budget: u64,
    max_actions: usize,
    duration_policy: SmbArchiveDurationPolicy,
    suffix_policy: SmbArchiveSuffixPolicy,
    ranking: Option<&dyn SmbRanking>,
    generated_mutator: Option<&dyn SmbMacro>,
    experimental_search: bool,
) -> Result<SmbArchiveReport, Box<dyn Error>> {
    if initial_inputs.is_empty() {
        return Err("SMB archive search requires a nonempty initial corpus".into());
    }
    if !(1..=MAX_SMB_COMPLETION_ACTIONS).contains(&max_actions) {
        return Err("SMB completion action limit is outside its bounded range".into());
    }
    if initial_inputs
        .iter()
        .any(|input| input.actions.len() > max_actions)
    {
        return Err("SMB archive input exceeds the configured action limit".into());
    }
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    let mut archive = Archive::new(ranking, experimental_search);
    let mut aggregate = SmbMilestones::default();
    let mut progress_watermark = SmbProgressWatermark::default();
    let mut first_reached = SmbMilestoneTimes::default();
    let mut first_inputs = SmbMilestoneInputs::default();
    let mut champion_input = SmbInput::default();
    let mut champion_milestones = SmbMilestones::default();

    target.reset();
    let genesis_key = archive_key(target.wram());
    let genesis_snapshot = target.snapshot().ok_or("failed to snapshot SMB genesis")?;
    let genesis_id = archive
        .insert(
            None,
            0,
            ArchiveCandidate {
                input: SmbInput::default(),
                key: genesis_key,
                milestones: SmbMilestones::default(),
            },
            genesis_snapshot,
            &[],
        )?
        .ok_or("failed to retain SMB genesis")?;
    for input in initial_inputs {
        target.reset();
        let mut prefix = SmbInput::default();
        let mut milestones = SmbMilestones::default();
        let mut parent_id = genesis_id;
        for action in &input.actions {
            if target.is_dead() {
                break;
            }
            prefix.actions.push(*action);
            target.apply(action);
            merge_progress_watermark(&mut progress_watermark, target.last_action_observations());
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                0,
                &prefix,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = prefix.clone();
            }
            if target.is_dead() || target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot SMB bootstrap prefix")?;
            if let Some(id) = archive.insert(
                Some(parent_id),
                0,
                ArchiveCandidate {
                    input: prefix.clone(),
                    key: archive_key(target.wram()),
                    milestones,
                },
                snapshot,
                target.last_action_observations(),
            )? {
                parent_id = id;
            }
        }
    }

    let mut rand = StdRand::with_seed(seed);
    let mut curve = Vec::new();
    let mut deaths = 0_u64;
    let mut generated_mutator_accounting = SmbGeneratedMutatorAccounting {
        installed: generated_mutator.is_some(),
        active: generated_mutator.is_some(),
        ..SmbGeneratedMutatorAccounting::default()
    };
    for execution in 1..=execution_budget {
        let parent_id = archive.choose_parent(&mut rand, max_actions)?;
        let parent = archive.entries[parent_id].clone();
        target.restore(&parent.snapshot)?;
        let mut input = parent.report.input.clone();
        let mut milestones = parent.report.milestones;
        let use_generated_mutator = generated_mutator_accounting.active
            && rand.below(NonZeroUsize::new(5).ok_or("invalid generated mutator odds")?) == 0;
        let (suffix, generated_emitted) = if use_generated_mutator {
            generated_mutator_accounting.attempts =
                generated_mutator_accounting.attempts.saturating_add(1);
            let candidate = generated_mutator
                .ok_or("active generated SMB mutator is missing")?
                .mutate(&input, rand.next());
            if candidate.actions.len() > max_actions
                || !candidate.actions.starts_with(&input.actions)
                || candidate
                    .actions
                    .iter()
                    .any(|action| action.hold_frames == 0 || action.hold_frames > MAX_HOLD_FRAMES)
            {
                return Err("generated SMB archive mutator violated deterministic bounds".into());
            }
            let emitted = candidate.actions.len() > input.actions.len();
            if emitted {
                generated_mutator_accounting.offspring =
                    generated_mutator_accounting.offspring.saturating_add(1);
            }
            (candidate.actions[input.actions.len()..].to_vec(), emitted)
        } else {
            let suffix_len = match suffix_policy {
                SmbArchiveSuffixPolicy::OneOrTwo => {
                    if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
                        2
                    } else {
                        1
                    }
                }
                SmbArchiveSuffixPolicy::BurstUpToFour => {
                    match rand.below(NonZeroUsize::new(8).ok_or("invalid burst odds")?) {
                        0 => 4,
                        1 => 3,
                        2 | 3 => 2,
                        _ => 1,
                    }
                }
            };
            let mut suffix = Vec::with_capacity(suffix_len);
            for _ in 0..suffix_len {
                suffix.push(sample_chord(
                    &mut rand,
                    duration_policy,
                    experimental_search,
                )?);
            }
            (suffix, false)
        };
        let mut current_parent = parent_id;
        let retained_before = archive.retained;
        for action in suffix {
            if target.is_dead() || input.actions.len() >= max_actions {
                break;
            }
            input.actions.push(action);
            target.apply(&action);
            merge_progress_watermark(&mut progress_watermark, target.last_action_observations());
            merge_action_milestones(&mut milestones, &target)?;
            merge_milestones(&mut aggregate, milestones);
            update_first_inputs(
                &mut first_reached,
                &mut first_inputs,
                milestones,
                execution,
                &input,
            );
            if milestone_key(milestones) > milestone_key(champion_milestones) {
                champion_milestones = milestones;
                champion_input = input.clone();
            }
            if target.is_dead() {
                deaths = deaths.saturating_add(1);
                break;
            }
            if target.exit_kind() != ExitKind::Ok {
                break;
            }
            let snapshot = target.snapshot().ok_or("failed to snapshot SMB suffix")?;
            if let Some(id) = archive.insert(
                Some(current_parent),
                execution,
                ArchiveCandidate {
                    input: input.clone(),
                    key: archive_key(target.wram()),
                    milestones,
                },
                snapshot,
                target.last_action_observations(),
            )? {
                current_parent = id;
            }
        }
        if generated_emitted {
            record_generated_mutator_result(
                &mut generated_mutator_accounting,
                archive.retained > retained_before,
                execution,
            );
        }
        archive.finish_execution(execution);
        if execution % 100 == 0 || execution == execution_budget {
            curve.push(SmbArchiveProgressPoint {
                executions: execution,
                milestones: aggregate,
                active_entries: archive.active.iter().filter(|active| **active).count(),
                occupied_cells: archive.cells.len(),
                deaths,
            });
        }
    }

    Ok(SmbArchiveReport {
        seed,
        executions: execution_budget,
        milestones: aggregate,
        progress_watermark,
        first_reached,
        first_inputs,
        champion_input,
        entries: archive
            .entries
            .into_iter()
            .map(|entry| entry.report)
            .collect(),
        progress_curve: curve,
        retained: archive.retained,
        rejected: archive.rejected,
        deaths,
        ranking: archive.ranking_accounting,
        generated_mutator: generated_mutator_accounting,
    })
}

fn merge_progress_watermark(
    watermark: &mut SmbProgressWatermark,
    observations: &[SmbObservations],
) {
    for observation in observations {
        let decoded = observation.decoded;
        *watermark = (*watermark).max(SmbProgressWatermark {
            world: decoded.world,
            level: decoded.level,
            progress: decoded.progress,
        });
    }
}

fn archive_key(wram: &[u8; 2_048]) -> SmbArchiveKey {
    let state = smb_mechanical_state_from_wram(wram);
    let digest = Sha256::digest(wram);
    SmbArchiveKey {
        world: state.world,
        level: state.level,
        progress: state.progress,
        player_y_bucket: state.player_y_bucket,
        player_engine_state: state.player_engine_state,
        state_fingerprint: digest[0] & STATE_FINGERPRINT_MASK,
    }
}

fn sample_chord(
    rand: &mut StdRand,
    duration_policy: SmbArchiveDurationPolicy,
    experimental_search: bool,
) -> Result<ButtonChord, Box<dyn Error>> {
    let masks: &[u8] = if experimental_search {
        const EXPERIMENTAL_BUTTON_MASKS: [u8; 14] = [
            0x00, 0x01, 0x02, 0x40, 0x80, 0x81, 0x82, 0x83, 0x10, 0x41, 0x42, 0xc0, 0xc1, 0xc2,
        ];
        &EXPERIMENTAL_BUTTON_MASKS
    } else {
        &FROZEN_BUTTON_MASKS
    };
    let buttons =
        masks[rand.below(NonZeroUsize::new(masks.len()).ok_or("empty SMB button vocabulary")?)];
    let hold_frames = match duration_policy {
        SmbArchiveDurationPolicy::Legacy => {
            if rand.below(NonZeroUsize::new(4).ok_or("invalid hold odds")?) != 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(1 + rand.below(NonZeroUsize::new(120).ok_or("invalid hold span")?))?
            }
        }
        SmbArchiveDurationPolicy::Stratified => {
            if rand.below(NonZeroUsize::new(2).ok_or("invalid stratum odds")?) == 0 {
                u8::try_from(
                    2 + rand.below(NonZeroUsize::new(11).ok_or("invalid short hold span")?),
                )?
            } else {
                u8::try_from(
                    96 + rand.below(NonZeroUsize::new(25).ok_or("invalid long hold span")?),
                )?
            }
        }
    };
    Ok(ButtonChord::new(buttons, hold_frames))
}

fn merge_action_milestones(
    milestones: &mut SmbMilestones,
    target: &SmbTarget,
) -> Result<(), Box<dyn Error>> {
    for observation in target.last_action_observations() {
        let wram: &[u8; 2_048] = observation
            .wram
            .as_slice()
            .try_into()
            .map_err(|_| "SMB observation WRAM is not exactly 2 KiB")?;
        merge_milestones(milestones, smb_milestones_from_wram(wram));
    }
    Ok(())
}

fn merge_milestones(aggregate: &mut SmbMilestones, current: SmbMilestones) {
    aggregate.max_1_1_scroll_bucket = aggregate
        .max_1_1_scroll_bucket
        .max(current.max_1_1_scroll_bucket);
    aggregate.reached_1_1_flag |= current.reached_1_1_flag;
    aggregate.reached_1_2 |= current.reached_1_2;
    aggregate.reached_onward |= current.reached_onward;
}

fn update_first_inputs(
    times: &mut SmbMilestoneTimes,
    inputs: &mut SmbMilestoneInputs,
    current: SmbMilestones,
    execution: u64,
    input: &SmbInput,
) {
    if current.max_1_1_scroll_bucket > 0 {
        times.progress_into_1_1.get_or_insert(execution);
        inputs
            .progress_into_1_1
            .get_or_insert_with(|| input.clone());
    }
    if current.reached_1_1_flag {
        times.flag_1_1.get_or_insert(execution);
        inputs.flag_1_1.get_or_insert_with(|| input.clone());
    }
    if current.reached_1_2 {
        times.level_1_2.get_or_insert(execution);
        inputs.level_1_2.get_or_insert_with(|| input.clone());
    }
    if current.reached_onward {
        times.onward.get_or_insert(execution);
        inputs.onward.get_or_insert_with(|| input.clone());
    }
}

fn milestone_key(milestones: SmbMilestones) -> (bool, bool, bool, u16) {
    (
        milestones.reached_onward,
        milestones.reached_1_2,
        milestones.reached_1_1_flag,
        milestones.max_1_1_scroll_bucket,
    )
}

fn frontier_quality(entry: &SmbArchiveEntryReport) -> ((bool, bool, bool, u16), u8, u8, u16) {
    (
        milestone_key(entry.milestones),
        entry.key.world,
        entry.key.level,
        entry.key.progress,
    )
}

fn entry_cost(entry: &SmbArchiveEntryReport) -> (usize, u64) {
    (entry.input.actions.len(), entry.id)
}

#[cfg(test)]
mod tests {
    use super::{
        Archive, SmbArchiveDurationPolicy, SmbArchiveSuffixPolicy, SmbGeneratedMutatorAccounting,
        SmbProgressWatermark, SmbRanking, SmbRankingSearchConfig, merge_progress_watermark,
        record_generated_mutator_result, run_smb_archive_search,
        run_smb_archive_search_with_config_and_suffix,
        run_smb_archive_search_with_generated_mutator, run_smb_archive_search_with_ranking,
    };
    use crate::phase4b::{ButtonChord, MAX_SMB_ACTIONS, SmbInput, SmbMacro, SmbObservations};

    struct ScriptedFrameRanking;

    impl SmbRanking for ScriptedFrameRanking {
        fn score(&self, observations: &[SmbObservations]) -> i64 {
            observations.last().map_or(0, |observation| {
                i64::try_from(observation.frame_count).unwrap_or(i64::MAX)
            })
        }
    }

    struct ScriptedArchiveMutator;

    impl SmbMacro for ScriptedArchiveMutator {
        fn mutate(&self, input: &SmbInput, seed: u64) -> SmbInput {
            let mut candidate = input.clone();
            if candidate.actions.len() < MAX_SMB_ACTIONS {
                candidate
                    .actions
                    .push(ButtonChord::new(if seed & 1 == 0 { 0x81 } else { 0x82 }, 8));
            }
            candidate
        }
    }

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[test]
    fn progress_watermark_uses_action_interiors() {
        let mut watermark = SmbProgressWatermark::default();
        let mut first = SmbObservations {
            frame_count: 1,
            wram: Vec::new(),
            decoded: Default::default(),
            milestones: Default::default(),
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        };
        first.decoded.world = 0;
        first.decoded.level = 2;
        first.decoded.progress = 41;
        let mut endpoint = first.clone();
        endpoint.frame_count = 2;
        endpoint.decoded.progress = 39;
        merge_progress_watermark(&mut watermark, &[first, endpoint]);
        assert_eq!(watermark.progress, 41);
    }

    #[test]
    fn same_seed_archive_reports_match_and_stay_bounded() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let first = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("first archive campaign");
        let second = run_smb_archive_search(&rom, &initial, 0x5eed_e000, 32)
            .expect("second archive campaign");
        assert_eq!(first, second);
        assert_eq!(first.executions, 32);
        assert!(
            first
                .entries
                .iter()
                .all(|entry| entry.input.actions.len() <= MAX_SMB_ACTIONS)
        );
        let burst_first = run_smb_archive_search_with_config_and_suffix(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            MAX_SMB_ACTIONS,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
        )
        .expect("first frozen burst campaign");
        let burst_second = run_smb_archive_search_with_config_and_suffix(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            MAX_SMB_ACTIONS,
            SmbArchiveDurationPolicy::Stratified,
            SmbArchiveSuffixPolicy::BurstUpToFour,
        )
        .expect("replayed frozen burst campaign");
        assert_eq!(burst_first, burst_second);
    }

    #[test]
    fn scripted_ranking_replays_and_retirement_uses_execution_rebuilds() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let first = run_smb_archive_search_with_ranking(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            SmbRankingSearchConfig {
                max_actions: MAX_SMB_ACTIONS,
                duration_policy: SmbArchiveDurationPolicy::Legacy,
                suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
            },
            &ScriptedFrameRanking,
        )
        .expect("first ranked archive campaign");
        let second = run_smb_archive_search_with_ranking(
            &rom,
            &initial,
            0x5eed_e000,
            32,
            SmbRankingSearchConfig {
                max_actions: MAX_SMB_ACTIONS,
                duration_policy: SmbArchiveDurationPolicy::Legacy,
                suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
            },
            &ScriptedFrameRanking,
        )
        .expect("replayed ranked archive campaign");
        assert_eq!(first, second);
        assert!(first.ranking.installed);
        let mut archive = Archive::new(Some(&ScriptedFrameRanking), false);
        archive.ranking_accounting.replacements = 1;
        archive.first_ranking_replacement = Some(1);
        archive.finish_execution(1_024);
        assert!(archive.ranking_accounting.active);
        archive.finish_execution(1_536);
        assert_eq!(archive.ranking_accounting.retired_at_execution, Some(1_536));
        assert!(!archive.ranking_accounting.active);
    }

    #[test]
    fn generated_mutator_retirement_counts_emitted_offspring_not_time() {
        let mut accounting = SmbGeneratedMutatorAccounting {
            installed: true,
            active: true,
            ..SmbGeneratedMutatorAccounting::default()
        };
        for execution in 1..128 {
            record_generated_mutator_result(&mut accounting, false, execution);
        }
        assert!(accounting.active);
        assert_eq!(accounting.consecutive_nonretained, 127);
        record_generated_mutator_result(&mut accounting, true, 128);
        assert!(accounting.active);
        assert_eq!(accounting.retained_offspring, 1);
        assert_eq!(accounting.consecutive_nonretained, 0);
        for execution in 129..=256 {
            record_generated_mutator_result(&mut accounting, false, execution);
        }
        assert!(!accounting.active);
        assert_eq!(accounting.retired_at_execution, Some(256));
    }

    #[test]
    fn scripted_generated_archive_mutator_replays_exactly() {
        let rom = synthetic_nrom();
        let initial = vec![SmbInput::default()];
        let config = SmbRankingSearchConfig {
            max_actions: MAX_SMB_ACTIONS,
            duration_policy: SmbArchiveDurationPolicy::Legacy,
            suffix_policy: SmbArchiveSuffixPolicy::OneOrTwo,
        };
        let first = run_smb_archive_search_with_generated_mutator(
            &rom,
            &initial,
            0x5eed_ef14,
            64,
            config,
            &ScriptedArchiveMutator,
        )
        .expect("first generated-mutator archive campaign");
        let replay = run_smb_archive_search_with_generated_mutator(
            &rom,
            &initial,
            0x5eed_ef14,
            64,
            config,
            &ScriptedArchiveMutator,
        )
        .expect("replayed generated-mutator archive campaign");
        assert_eq!(first, replay);
        assert!(first.generated_mutator.installed);
        assert!(first.generated_mutator.attempts > 0);
        assert!(first.generated_mutator.offspring > 0);
    }
}
