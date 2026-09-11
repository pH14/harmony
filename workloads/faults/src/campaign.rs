// SPDX-License-Identifier: AGPL-3.0-or-later

//! Fault-package implementation of the game-neutral campaign interface.

use std::{
    collections::BTreeMap,
    error::Error,
    io::Write,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use searcher::{
    search::{
        archive::{RetentionPolicy, SelectorPolicy},
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignConfig,
            CampaignJobResult, CampaignModeReport, CampaignOrigin, CampaignProgressRecord,
            CampaignStreamHeader, CampaignTypes, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            Evaluation, GamePolicies, InitialDrawState, InputPolicy, RefinementRequest, Reporting,
            SnapshotCheckpoint, TargetExecution, postcard_result_sha256, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    },
    target::ExitKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{
        DURATION_IDENTIFIER, FaultArchiveKey, FaultArchiveReport, FaultBugRecord,
        FaultCoordinateTelemetry, FaultInput, FaultMilestones, FaultProgressWatermark,
        KEY_POLICY_IDENTIFIER, MAX_RECORDED_BUGS, REPLACEMENT_IDENTIFIER, action_time, archive_key,
        bug_outcome, merge_milestones, merge_progress_watermark, milestone_key, milestones,
        sample_action,
    },
    bundle::{FaultVocabulary, MAX_NODES},
    consonance::{FaultConfig, FaultTarget, identity, snapshot_memory_charge},
    target::{FaultAction, FaultObservations, FaultSnapshot, MAX_FAULT_ACTIONS, WAIT_MAX_SCALE},
};

/// Stream format written by fault-package campaigns.
pub const CAMPAIGN_STREAM_FORMAT: &str = "faultlab-consonance-campaign-stream-v1";
/// Snapshot checkpoint format written by fault-package campaigns.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-consonance-snapshot-checkpoint-v1";
/// Recorded terminal condition: an armed assertion stop or a guest crash.
pub const TERMINAL_POLICY_IDENTIFIER: &str = "assertion_or_crash";

const VOCABULARY_FIELD: &str = "action_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const IMAGE_FIELD: &str = "image";
const HORIZON_FIELD: &str = "horizon_nanos";
const COORDINATE_POLICY_FIELD: &str = "event_coordinate_policy";
const COORDINATE_POLICY_IDENTIFIER: &str = "prefix_bisection_v2";
const REFINEMENT_COORDINATE_POLICY_IDENTIFIER: &str = "prefix_bisection_v3";
const LEGACY_COORDINATE_POLICY_IDENTIFIER: &str = "legacy_global_anchor";

/// Header placeholder for a run with no adaptive draw table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultNoTableHeader;

/// The recorded run policy: the action alphabet the workload bundle admits.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultCampaignRun {
    /// Nodes and hooks the bundle declares.
    pub vocabulary: FaultVocabulary,
    /// Whether suffix draws receive their exact selected parent input. Old
    /// streams omit this policy and retain their parent-independent draws.
    coordinate_refinement: bool,
    /// Whether admitted coordinate observations enqueue parent redraws. This
    /// is versioned separately so streams from the prior parent-aware draw
    /// remain replayable.
    refinement_redraw: bool,
}

/// Image identity shared by fault-package workers.
pub struct FaultGame {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: FaultConfig,
    identity: String,
    /// The image's sealed setup `Moment`, learned from the first target a
    /// worker boots. It is a deterministic property of the image, so every
    /// worker learns the same one.
    root_seal: OnceLock<u64>,
    /// Admission-ordered observations used to update the coordinator's draw
    /// brackets. Workers never mutate this state, so completion order cannot
    /// affect the next selection.
    coordinate_brackets: Mutex<FaultDrawState>,
    /// Event outcomes produced by workers and consumed at ordered admission.
    /// The full input key avoids making completion order observable.
    event_outcomes: Mutex<BTreeMap<[u8; 32], Vec<bool>>>,
    /// Admission-thread marker consumed by the optional generic redraw hook.
    event_observation: Mutex<Option<[u8; 32]>>,
    /// Stream policy selected by the current live or replay run.
    refinement_redraw: AtomicBool,
}

impl FaultGame {
    /// Build a game context over one workload image.
    #[must_use]
    pub fn new(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Self {
        Self {
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            config: config.clone(),
            identity: identity(kernel, initramfs, config),
            root_seal: OnceLock::new(),
            coordinate_brackets: Mutex::new(FaultDrawState::default()),
            event_outcomes: Mutex::new(BTreeMap::new()),
            event_observation: Mutex::new(None),
            refinement_redraw: AtomicBool::new(false),
        }
    }

    /// How the image boots and how long each action runs.
    #[must_use]
    pub fn config(&self) -> &FaultConfig {
        &self.config
    }

    /// The sealed setup `Moment`, once a target has booted.
    #[must_use]
    pub fn root_seal(&self) -> Option<u64> {
        self.root_seal.get().copied()
    }

    /// Pinned image identity recorded in streams.
    #[must_use]
    pub fn image_identity(&self) -> &str {
        &self.identity
    }

    fn reset_coordinate_brackets(&self) {
        *self
            .coordinate_brackets
            .lock()
            .expect("fault coordinate state mutex poisoned") = FaultDrawState::default();
        self.event_outcomes
            .lock()
            .expect("fault event outcome mutex poisoned")
            .clear();
        *self
            .event_observation
            .lock()
            .expect("fault event observation mutex poisoned") = None;
    }

    fn coordinate_state(&self) -> FaultDrawState {
        self.coordinate_brackets
            .lock()
            .expect("fault coordinate state mutex poisoned")
            .clone()
    }

    fn observe_event_coordinate(
        &self,
        prefix: &[FaultAction],
        node: u16,
        ordinal: u64,
        fired: bool,
    ) {
        self.coordinate_brackets
            .lock()
            .expect("fault coordinate state mutex poisoned")
            .observe(prefix, node, ordinal, fired);
    }

    fn record_event_outcome(&self, input: Vec<FaultAction>, fired: bool) {
        self.event_outcomes
            .lock()
            .expect("fault event outcome mutex poisoned")
            .entry(event_context_digest(&input))
            .or_default()
            .push(fired);
    }

    fn take_event_outcome(&self, input: &[FaultAction]) -> Option<bool> {
        let mut outcomes = self
            .event_outcomes
            .lock()
            .expect("fault event outcome mutex poisoned");
        let key = event_context_digest(input);
        let values = outcomes.get_mut(&key)?;
        let fired = values.pop();
        if values.is_empty() {
            outcomes.remove(&key);
        }
        fired
    }
}

/// Campaign evidence owned by the adapter.
#[derive(Clone, Default)]
pub struct FaultCampaignEvidence {
    aggregate: FaultMilestones,
    watermark: FaultProgressWatermark,
    champion_input: FaultInput,
    champion_milestones: FaultMilestones,
    bugs: Vec<FaultBugRecord>,
    coordinate_attempted: u64,
    coordinate_fired: u64,
    coordinate_refined: u64,
}

const EVENT_NODE_COUNT: usize = MAX_NODES as usize;
/// Keep each context's bisection history bounded. The cap is a state-size
/// bound, not a workload coordinate or a timing assumption.
const MAX_COORDINATE_OBSERVATIONS: usize = 32;
/// A campaign can retain many prefixes, but coordinate refinement only needs
/// a bounded set of their exact contexts. Eviction is deterministic.
const MAX_COORDINATE_CONTEXTS: usize = EVENT_NODE_COUNT * 4;
const MAX_LEGACY_COORDINATE_ANCHORS: usize = 256;
const MAX_TRANSIENT_EVENT_OUTCOMES: usize = MAX_COORDINATE_CONTEXTS;

fn adaptive_state_memory_reserve() -> usize {
    // One table is the policy draw state and one is the admission-ordered
    // observation fold owned by FaultGame. Transient outcomes use fixed-size
    // digests and are consumed as soon as their job is admitted.
    std::mem::size_of::<EventCoordinateContext>() * MAX_COORDINATE_CONTEXTS * 2
        + std::mem::size_of::<(u16, u64)>() * MAX_LEGACY_COORDINATE_ANCHORS * 2
        + (std::mem::size_of::<[u8; 32]>() + std::mem::size_of::<bool>())
            * MAX_TRANSIENT_EVENT_OUTCOMES
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventCoordinateObservation {
    ordinal: u64,
    fired: bool,
}

/// The observed reachability interval for one node at one action prefix.
///
/// A fired ordinal is a reachable point and an unfired ordinal is an
/// unreachable point. Both outcomes remain recorded separately. The sampled
/// points then partition the reachable interval, so refinement continues
/// through its interior instead of converging only on its largest endpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventCoordinateBracket {
    fired: Option<u64>,
    unfired: Option<u64>,
    observations_seen: u64,
    observations: [Option<EventCoordinateObservation>; MAX_COORDINATE_OBSERVATIONS],
}

impl Default for EventCoordinateBracket {
    fn default() -> Self {
        Self {
            fired: None,
            unfired: None,
            observations_seen: 0,
            observations: [None; MAX_COORDINATE_OBSERVATIONS],
        }
    }
}

impl EventCoordinateBracket {
    fn observe(&mut self, ordinal: u64, fired: bool) {
        self.observations_seen = self.observations_seen.saturating_add(1);
        if fired {
            self.fired = Some(self.fired.map_or(ordinal, |current| current.max(ordinal)));
        } else {
            self.unfired = Some(self.unfired.map_or(ordinal, |current| current.min(ordinal)));
        }
        let observation = EventCoordinateObservation { ordinal, fired };
        if self.observations.contains(&Some(observation)) {
            return;
        }
        if let Some(slot) = self.observations.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(observation);
        }
    }

    /// Return the exact midpoint of the largest still-unobserved interval.
    /// The lower endpoint is the protocol's positive coordinate origin; the
    /// upper endpoint is the greatest currently reachable candidate, or one
    /// below the first known unfired point. No workload timing enters here.
    fn next_probe(self) -> Option<u64> {
        let upper = self.unfired.map_or_else(
            || self.fired.unwrap_or(0),
            |ordinal| ordinal.saturating_sub(1),
        );
        if upper == 0 {
            return None;
        }

        // Once the exact observation table is full, continue through the
        // finite reachable interval instead of repeatedly returning a
        // midpoint that can no longer be recorded. At most the table's 32
        // remembered points can block consecutive candidates, so 33 probes
        // are sufficient to find an unremembered coordinate whenever one
        // exists.
        if self.observations.iter().all(Option::is_some) {
            for offset in 0..=MAX_COORDINATE_OBSERVATIONS {
                let candidate = u64::try_from(
                    (u128::from(self.observations_seen) + offset as u128) % u128::from(upper) + 1,
                )
                .expect("a coordinate reduced modulo a u64 bound fits u64");
                if !self
                    .observations
                    .iter()
                    .flatten()
                    .any(|observation| observation.ordinal == candidate)
                {
                    return Some(candidate);
                }
            }
            return None;
        }

        let mut points = Vec::with_capacity(MAX_COORDINATE_OBSERVATIONS + 2);
        points.push(0_u128);
        points.push(u128::from(upper) + 1);
        for observation in self.observations.into_iter().flatten() {
            if observation.ordinal >= 1 && observation.ordinal <= upper {
                points.push(u128::from(observation.ordinal));
            }
        }
        points.sort_unstable();
        points.dedup();

        points
            .windows(2)
            .filter_map(|window| {
                let lower = window[0];
                let exclusive_upper = window[1];
                let width = exclusive_upper.saturating_sub(lower).saturating_sub(1);
                if width == 0 {
                    None
                } else {
                    Some((width, lower + 1 + (width - 1) / 2))
                }
            })
            .max_by_key(|(width, lower)| (*width, std::cmp::Reverse(*lower)))
            .and_then(|(_, midpoint)| u64::try_from(midpoint).ok())
    }
}

/// One prefix-scoped node bracket. Prefixes are identified by a stable digest
/// of the exact action input supplied during parent-aware suffix generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EventCoordinateContext {
    prefix: [u8; 32],
    node: u16,
    bracket: EventCoordinateBracket,
}

/// Coordinator-side state for deterministic event-coordinate refinement.
///
/// Each bracket is scoped to the action prefix that produced its observation;
/// observations from unrelated workload states therefore cannot widen or
/// narrow one another. The state is copied from the game-owned admission fold
/// after each record, so live selection and serial replay see the same bounded
/// context table.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultDrawState {
    contexts: Vec<EventCoordinateContext>,
    /// Retained global anchors used only while replaying streams recorded
    /// before parent-scoped bisection became part of the run policy.
    legacy_anchors: Vec<(u16, u64)>,
}

impl Default for FaultDrawState {
    fn default() -> Self {
        Self {
            contexts: Vec::with_capacity(MAX_COORDINATE_CONTEXTS),
            legacy_anchors: Vec::with_capacity(MAX_LEGACY_COORDINATE_ANCHORS),
        }
    }
}

impl FaultDrawState {
    fn observe(&mut self, prefix: &[FaultAction], node: u16, ordinal: u64, fired: bool) {
        let prefix = event_context_digest(prefix);
        if let Some(context) = self
            .contexts
            .iter_mut()
            .find(|context| context.prefix == prefix && context.node == node)
        {
            context.bracket.observe(ordinal, fired);
            return;
        }
        if self.contexts.len() == MAX_COORDINATE_CONTEXTS {
            self.contexts.remove(0);
        }
        let mut bracket = EventCoordinateBracket::default();
        bracket.observe(ordinal, fired);
        self.contexts.push(EventCoordinateContext {
            prefix,
            node,
            bracket,
        });
    }

    fn next_probe(&self, prefix: &[FaultAction], node: u16) -> Option<u64> {
        let prefix = event_context_digest(prefix);
        self.contexts
            .iter()
            .find(|context| context.prefix == prefix && context.node == node)
            .and_then(|context| context.bracket.next_probe())
    }
}

/// Stable, input-only context identity. It is deliberately independent of
/// workload configuration, timing, and host scheduling.
fn event_context_digest(actions: &[FaultAction]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update((actions.len() as u64).to_le_bytes());
    for action in actions {
        match *action {
            FaultAction::Wait(scale) => {
                digest.update([0]);
                digest.update([scale]);
            }
            FaultAction::Kill(node) => {
                digest.update([1]);
                digest.update(node.to_le_bytes());
            }
            FaultAction::EventKill { node, ordinal } => {
                digest.update([2]);
                digest.update(node.to_le_bytes());
                digest.update(ordinal.to_le_bytes());
            }
            FaultAction::Pause(node, ticks) => {
                digest.update([3]);
                digest.update(node.to_le_bytes());
                digest.update(ticks.to_le_bytes());
            }
            FaultAction::Restart(node) => {
                digest.update([4]);
                digest.update(node.to_le_bytes());
            }
            FaultAction::Hook(hook) => {
                digest.update([5]);
                digest.update(hook.to_le_bytes());
            }
            FaultAction::Interrupt(vector) => {
                digest.update([6]);
                digest.update(vector.to_le_bytes());
            }
            FaultAction::Park {
                node,
                addr,
                hits,
                hold_us,
            } => {
                digest.update([7]);
                digest.update(node.to_le_bytes());
                digest.update(addr.to_le_bytes());
                digest.update(hits.to_le_bytes());
                digest.update(hold_us.to_le_bytes());
            }
        }
    }
    digest.finalize().into()
}

fn draw_fault_suffix(
    run: &FaultCampaignRun,
    state: &FaultDrawState,
    parent: Option<&[FaultAction]>,
    shape: SuffixShape,
    mixture: MixtureDraw,
    mutation_seed: u64,
) -> Result<Vec<FaultAction>, Box<dyn Error>> {
    let mut suffix = draw_suffix(
        shape,
        mixture.mixture,
        mixture.weight,
        mutation_seed,
        |_| Ok(None),
        |rand| sample_action(rand, &run.vocabulary),
    )?;
    if !run.coordinate_refinement {
        if !state.legacy_anchors.is_empty() {
            let bound = NonZeroUsize::new(state.legacy_anchors.len())
                .ok_or("legacy event-coordinate anchor state unexpectedly empty")?;
            for (index, action) in suffix.iter_mut().enumerate() {
                let FaultAction::EventKill { .. } = *action else {
                    continue;
                };
                let coordinate_seed = mix_legacy_coordinate_seed(mutation_seed, index);
                let anchor = state.legacy_anchors[legacy_rand_index(coordinate_seed, bound)];
                *action = FaultAction::EventKill {
                    node: anchor.0,
                    ordinal: refine_legacy_event_coordinate(anchor.1, coordinate_seed),
                };
            }
        }
        return Ok(suffix);
    }
    let Some(parent) = parent else {
        return Ok(suffix);
    };
    let mut prefix = parent.to_vec();
    for action in &mut suffix {
        if let FaultAction::EventKill { node, .. } = *action
            && let Some(ordinal) = state.next_probe(&prefix, node)
        {
            *action = FaultAction::EventKill { node, ordinal };
        }
        prefix.push(*action);
    }
    Ok(suffix)
}

fn legacy_rand_index(seed: u64, bound: NonZeroUsize) -> usize {
    let mixed = seed ^ seed.rotate_left(29);
    ((u128::from(mixed) * u128::from(bound.get() as u64)) >> 64) as usize
}

fn mix_legacy_coordinate_seed(seed: u64, index: usize) -> u64 {
    let mut value = seed ^ (index as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn refine_legacy_event_coordinate(anchor: u64, seed: u64) -> u64 {
    if seed & 3 == 0 {
        return anchor.max(1);
    }
    let highest = 63 - anchor.max(1).leading_zeros();
    let level = ((seed >> 2) % u64::from(highest + 1)) as u32;
    let radius = 1_u64 << level;
    if seed & 3 == 1 {
        anchor.saturating_sub(radius).max(1)
    } else {
        anchor.saturating_add(radius).max(1)
    }
}

/// Fault-package campaign origin.
pub type FaultCampaignOrigin = CampaignOrigin<FaultGame>;
/// Fault-package whole-tree snapshot checkpoint.
pub type FaultSnapshotCheckpoint = SnapshotCheckpoint<FaultSnapshot>;
/// Fault-package stream header.
pub type FaultCampaignStreamHeader = CampaignStreamHeader<FaultNoTableHeader>;
/// The generic campaign report of a fault-package run.
pub type FaultCampaignModeReport = CampaignModeReport<FaultAction, FaultArchiveReport>;
/// Fault-package progress sidecar record.
pub type FaultCampaignProgressRecord = CampaignProgressRecord<FaultArchiveKey>;
type FaultCampaignActionResult = CampaignActionResult<FaultGame>;
type FaultCampaignJobResult = CampaignJobResult<FaultGame>;

/// The campaign report plus the fault-package outcome: how many bugs the run
/// found and how many executions the first one cost.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultCampaignReport {
    /// The generic campaign report.
    #[serde(flatten)]
    pub campaign: FaultCampaignModeReport,
    /// Bugs found, bounded by [`MAX_RECORDED_BUGS`].
    pub bugs_found: u64,
    /// Ordered admission position of the execution that found the first bug;
    /// absent when the run found none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executions_to_first_bug: Option<u64>,
}

impl FaultCampaignReport {
    /// Wrap one generic campaign report with its bug outcome.
    #[must_use]
    pub fn new(campaign: FaultCampaignModeReport) -> Self {
        let (bugs_found, executions_to_first_bug) = bug_outcome(&campaign.archive.bugs);
        Self {
            bugs_found,
            executions_to_first_bug,
            campaign,
        }
    }
}

/// Fixed configuration for one live fault-package campaign.
pub struct FaultCampaignConfig {
    /// Campaign seed.
    pub campaign_seed: u64,
    /// The action alphabet, read from the workload's bundle.
    pub vocabulary: FaultVocabulary,
    /// Worker thread count.
    pub workers: u32,
    /// Admitted execution budget.
    pub execution_budget: u64,
    /// Maximum actions in one clean-reset input.
    pub action_limit: usize,
    /// Operator-supplied host label.
    pub host: String,
    /// Optional live-only wall cutoff.
    pub wall_budget: Option<std::time::Duration>,
    /// Maximum retained archive entries.
    pub archive_entry_limit: usize,
    /// Deterministic logical-memory budget for live search structures.
    pub memory_budget_mib: Option<usize>,
    /// Live-only: materialize full archive inputs and snapshots at completion.
    pub materialize_final_artifacts: bool,
    /// Admission policy.
    pub retention: RetentionPolicy,
    /// Generic parent selector.
    pub selector: SelectorPolicy,
    /// Generic suffix-length shape.
    pub suffix: SuffixShape,
    /// Generic draw mixture.
    pub mixture: DrawMixture,
    /// Live-only path receiving the first bug-finding input.
    pub victory_input_path: Option<PathBuf>,
}

impl FaultCampaignConfig {
    fn generic(&self) -> CampaignConfig<FaultGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            // The fault campaign stops at its first bug and replays it.
            continue_after_victory: false,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker: DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: FaultCampaignRun {
                vocabulary: self.vocabulary.clone(),
                coordinate_refinement: true,
                refinement_redraw: true,
            },
            suffix: self.suffix,
            mixture: self.mixture,
            retention: self.retention,
            selector: self.selector.clone(),
            victory_input_path: self.victory_input_path.clone(),
        }
    }
}

fn recorded<'a>(policies: &'a GamePolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("fault package stream is missing {field}").into())
}

fn merge_action_milestones(aggregate: &mut FaultMilestones, target: &FaultTarget) {
    if target.exit_kind() != ExitKind::Ok {
        // A failed evaluator may not have produced a complete observation, so
        // the parent's milestones stand and the generic campaign records the
        // action as failed.
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation));
    }
}

#[allow(clippy::too_many_arguments)] // Mirrors the game-neutral `Game::execute_job` signature.
fn execute_job(
    target: &mut FaultTarget,
    origin_snapshot: &FaultSnapshot,
    replay: &[FaultAction],
    parent_actions: usize,
    parent_milestones: FaultMilestones,
    suffix: &[FaultAction],
    max_actions: usize,
) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
    target.restore(origin_snapshot)?;
    for action in replay {
        target.apply(*action);
    }
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.found_bug() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(*action);
        merge_action_milestones(&mut aggregate, target);
        let observations = target.last_action_observations().to_vec();
        let failed = target.exit_kind() != ExitKind::Ok;
        let victory = !failed && target.found_bug();
        // An endpoint that stopped anywhere but its horizon deadline has no
        // successor, so the search records it without a parent snapshot.
        let candidate = match target.snapshot() {
            Some(snapshot) if !victory && !failed => Some(CampaignCandidate {
                key: archive_key(target.observation()),
                viable: true,
                snapshot,
            }),
            _ => None,
        };
        let dead = candidate.is_none() && !victory && !failed;
        actions.push(CampaignActionResult {
            action: *action,
            observations,
            milestones: aggregate,
            dead,
            victory,
            failed,
            candidate,
        });
        if dead || victory || failed {
            break;
        }
    }
    Ok(CampaignJobResult { actions })
}

impl CampaignTypes for FaultGame {
    type Target = FaultTarget;
    type Action = FaultAction;
    type Key = FaultArchiveKey;
    type Milestones = FaultMilestones;
    type Progress = FaultProgressWatermark;
    type Snapshot = FaultSnapshot;
    type Observations = FaultObservations;
    type Evidence = FaultCampaignEvidence;
    type ArchiveReport = FaultArchiveReport;
    type Run = FaultCampaignRun;
    type DrawState = FaultDrawState;
    type DrawCheckpoint = ();
    type TableHeader = FaultNoTableHeader;
}

impl Reporting for FaultGame {
    fn diagnostics(evidence: &FaultCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "event_coordinate_attempted": evidence.coordinate_attempted,
            "event_coordinate_fired": evidence.coordinate_fired,
            "event_coordinate_refined": evidence.coordinate_refined,
        }))
    }

    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn image_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(&self.kernel);
        digest.update(&self.initramfs);
        format!("{:x}", digest.finalize())
    }

    fn result_sha256(&self, result: &FaultCampaignJobResult) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &FaultCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> FaultArchiveReport {
        FaultArchiveReport {
            seed: state.seed,
            root_seal: self.root_seal.get().copied().unwrap_or_default(),
            horizon_nanos: self.config.horizon_nanos,
            executions: state.executions,
            milestones: evidence.aggregate,
            progress_watermark: evidence.watermark,
            champion_input: evidence.champion_input.clone(),
            entries: state.entries,
            progress_curve: state.progress_curve,
            retained: state.retained,
            rejected: state.rejected,
            deaths: state.deaths,
            bugs: evidence.bugs.clone(),
            coordinate_telemetry: FaultCoordinateTelemetry {
                attempted: evidence.coordinate_attempted,
                fired: evidence.coordinate_fired,
                refined: evidence.coordinate_refined,
            },
            selector: state.selector,
        }
    }
}

impl InputPolicy for FaultGame {
    fn max_action_limit(&self) -> usize {
        MAX_FAULT_ACTIONS
    }

    fn longest_action_time(&self) -> u64 {
        1 << WAIT_MAX_SCALE
    }

    fn policies(&self, run: &FaultCampaignRun) -> GamePolicies {
        [
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, TERMINAL_POLICY_IDENTIFIER),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .chain([
            (IMAGE_FIELD.to_owned(), self.identity.clone()),
            (
                HORIZON_FIELD.to_owned(),
                self.config.horizon_nanos.to_string(),
            ),
            (VOCABULARY_FIELD.to_owned(), run.vocabulary.identifier()),
            (
                COORDINATE_POLICY_FIELD.to_owned(),
                if run.refinement_redraw {
                    REFINEMENT_COORDINATE_POLICY_IDENTIFIER
                } else if run.coordinate_refinement {
                    COORDINATE_POLICY_IDENTIFIER
                } else {
                    LEGACY_COORDINATE_POLICY_IDENTIFIER
                }
                .to_owned(),
            ),
        ])
        .collect()
    }

    fn resolve_recorded(
        &self,
        policies: &GamePolicies,
    ) -> Result<FaultCampaignRun, Box<dyn Error>> {
        let coordinate_policy = policies.get(COORDINATE_POLICY_FIELD).map(String::as_str);
        let coordinate_refinement = coordinate_policy == Some(COORDINATE_POLICY_IDENTIFIER)
            || coordinate_policy == Some(REFINEMENT_COORDINATE_POLICY_IDENTIFIER);
        let refinement_redraw = coordinate_policy == Some(REFINEMENT_COORDINATE_POLICY_IDENTIFIER);
        let run = FaultCampaignRun {
            vocabulary: FaultVocabulary::from_identifier(recorded(policies, VOCABULARY_FIELD)?)?,
            coordinate_refinement,
            refinement_redraw,
        };
        let expected = self.policies(&run);
        if policies != &expected {
            // Streams written before parent-aware refinement did not carry a
            // coordinate policy. They remain replayable through their legacy
            // parent-independent draw path.
            if !policies.contains_key(COORDINATE_POLICY_FIELD) {
                let mut legacy_expected = expected.clone();
                legacy_expected.remove(COORDINATE_POLICY_FIELD);
                if policies == &legacy_expected {
                    return Ok(run);
                }
            }
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("fault package stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("fault package stream carries an unknown game policy".into());
        }
        Ok(run)
    }

    fn draw_state_memory_reserve_bytes(
        &self,
        _run: &FaultCampaignRun,
        _max_actions: usize,
    ) -> usize {
        adaptive_state_memory_reserve()
    }

    fn draw_state_memory_bytes(&self, _state: &FaultDrawState) -> usize {
        adaptive_state_memory_reserve()
    }

    fn initial_draw_state(
        &self,
        run: &FaultCampaignRun,
        _origin: Option<(&str, &FaultArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        self.refinement_redraw
            .store(run.refinement_redraw, Ordering::Relaxed);
        self.reset_coordinate_brackets();
        Ok((self.coordinate_state(), None))
    }

    fn expand_suffix(
        &self,
        run: &FaultCampaignRun,
        state: &FaultDrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        draw_fault_suffix(run, state, None, shape, mixture, mutation_seed)
    }

    fn expand_suffix_needs_parent_input(&self, run: &FaultCampaignRun) -> bool {
        run.coordinate_refinement
    }

    fn expand_suffix_with_parent(
        &self,
        run: &FaultCampaignRun,
        state: &FaultDrawState,
        parent: &FaultInput,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        draw_fault_suffix(
            run,
            state,
            Some(&parent.actions),
            shape,
            mixture,
            mutation_seed,
        )
    }

    fn expand_suffix_recorded_with_parent(
        &self,
        run: &FaultCampaignRun,
        state: &FaultDrawState,
        parent: &FaultInput,
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&Self::DrawCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        if before.is_some() {
            return Err("recorded stream carries an unsupported draw checkpoint".into());
        }
        self.expand_suffix_with_parent(run, state, parent, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        run: &FaultCampaignRun,
        state: &mut FaultDrawState,
        retained: &[(usize, &[FaultAction])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        if run.coordinate_refinement {
            *state = self.coordinate_state();
        } else {
            for (_, actions) in retained {
                for action in *actions {
                    if let FaultAction::EventKill { node, ordinal } = *action {
                        state.legacy_anchors.push((node, ordinal));
                        if state.legacy_anchors.len() > MAX_LEGACY_COORDINATE_ANCHORS {
                            let drop = state.legacy_anchors.len() - MAX_LEGACY_COORDINATE_ANCHORS;
                            state.legacy_anchors.drain(..drop);
                        }
                    }
                }
            }
        }
        Ok(None)
    }
}

impl TargetExecution for FaultGame {
    fn new_target(&self) -> Result<FaultTarget, String> {
        let target = FaultTarget::new(&self.kernel, &self.initramfs, &self.config)?;
        let seal = *self.root_seal.get_or_init(|| target.root_seal());
        if seal != target.root_seal() {
            return Err(format!(
                "the image sealed setup at {} on one worker and {seal} on another",
                target.root_seal()
            ));
        }
        Ok(target)
    }

    fn reset(&self, target: &mut FaultTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut FaultTarget,
        snapshot: &FaultSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn frames_clocked(&self, target: &FaultTarget) -> u64 {
        target.horizons_clocked()
    }

    fn action_time_fn(&self) -> fn(&FaultAction) -> u64 {
        action_time
    }

    fn snapshot_memory_charge(snapshot: &FaultSnapshot) -> usize {
        snapshot_memory_charge(snapshot)
    }

    fn apply_action(
        &self,
        target: &mut FaultTarget,
        action: &FaultAction,
        aggregate: &mut FaultMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(*action);
        merge_action_milestones(aggregate, target);
        Ok(())
    }

    fn snapshot(&self, target: &mut FaultTarget) -> Result<FaultSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "the fault endpoint has no successor to snapshot".into())
    }

    fn execute_job(
        &self,
        _run: &FaultCampaignRun,
        target: &mut FaultTarget,
        origin_snapshot: &FaultSnapshot,
        replay: &[FaultAction],
        parent_actions: usize,
        parent_milestones: FaultMilestones,
        suffix: &[FaultAction],
        max_actions: usize,
        _retention: RetentionPolicy,
    ) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
        let result = execute_job(
            target,
            origin_snapshot,
            replay,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
        )?;
        let outcomes = target.event_kill_outcomes();
        let suffix_outcomes = outcomes
            .get(replay.len()..)
            .ok_or("fault target lost replay action outcomes")?;
        let mut input = origin_snapshot.actions.clone();
        input.extend_from_slice(replay);
        for (index, action) in result.actions.iter().enumerate() {
            input.push(action.action);
            if matches!(action.action, FaultAction::EventKill { .. })
                && let Some(Some(fired)) = suffix_outcomes.get(index).copied()
            {
                self.record_event_outcome(input.clone(), fired);
            }
        }
        Ok(result)
    }
}

impl Evaluation for FaultGame {
    fn is_terminal(&self, target: &FaultTarget) -> bool {
        target.exit_kind() != ExitKind::Ok || target.snapshot().is_none()
    }

    fn is_run_terminal(
        &self,
        _run: &FaultCampaignRun,
        target: &FaultTarget,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok && !target.found_bug() {
            return Err("the fault terminal predicate cannot inspect a failed VM".into());
        }
        Ok(target.found_bug())
    }

    fn current_key(&self, target: &FaultTarget) -> Result<FaultArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.observation()))
    }

    fn complete_candidate_key(
        &self,
        key: FaultArchiveKey,
        _snapshot: &FaultSnapshot,
    ) -> Result<FaultArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut FaultMilestones, from: FaultMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &FaultCampaignEvidence) -> FaultMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &FaultCampaignEvidence) -> FaultProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut FaultCampaignEvidence,
        source: &FaultArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut FaultCampaignEvidence,
        target: &FaultTarget,
    ) -> Result<(), Box<dyn Error>> {
        merge_progress_watermark(
            &mut evidence.watermark,
            std::slice::from_ref(target.observation()),
        );
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut FaultCampaignEvidence,
        value: FaultMilestones,
        input: &FaultInput,
    ) {
        merge_milestones(&mut evidence.aggregate, value);
        if milestone_key(value) > milestone_key(evidence.champion_milestones) {
            evidence.champion_milestones = value;
            evidence.champion_input = input.clone();
        }
    }

    fn merge_action_evidence<F>(
        &self,
        evidence: &mut FaultCampaignEvidence,
        action: &FaultCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<FaultInput, Box<dyn Error>>,
    {
        let mut input = Some(input);
        let event_input = if matches!(action.action, FaultAction::EventKill { .. }) {
            let factory = input.take().expect("action input closure already consumed");
            Some(factory()?)
        } else {
            None
        };
        if let Some(input) = event_input.as_ref()
            && let Some(fired) = self.take_event_outcome(&input.actions)
            && let FaultAction::EventKill { node, ordinal } = action.action
        {
            let prefix_len = input.actions.len().saturating_sub(1);
            self.observe_event_coordinate(&input.actions[..prefix_len], node, ordinal, fired);
            *self
                .event_observation
                .lock()
                .expect("fault event observation mutex poisoned") =
                Some(event_context_digest(&input.actions));
            evidence.coordinate_attempted = evidence.coordinate_attempted.saturating_add(1);
            if fired {
                evidence.coordinate_fired = evidence.coordinate_fired.saturating_add(1);
            }
        }
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let bug = action
            .observations
            .iter()
            .find(|observation| observation.is_bug())
            .filter(|_| evidence.bugs.len() < MAX_RECORDED_BUGS);
        let champion = (milestone_key(action.milestones)
            > milestone_key(evidence.champion_milestones))
        .then_some(action.milestones);
        if bug.is_some() || champion.is_some() {
            let input = match event_input {
                Some(input) => input,
                None => {
                    let factory = input.take().expect("action input closure already consumed");
                    factory()?
                }
            };
            if let Some(observations) = bug {
                evidence.bugs.push(FaultBugRecord {
                    execution: sequence,
                    input: input.clone(),
                    observations: observations.clone(),
                });
            }
            if let Some(milestones) = champion {
                evidence.champion_milestones = milestones;
                evidence.champion_input = input;
            }
        }
        Ok(())
    }

    fn refinement_request<F>(
        &self,
        parent_id: u64,
        action: &FaultCampaignActionResult,
        input: F,
    ) -> Result<Option<RefinementRequest<Self::Action>>, Box<dyn Error>>
    where
        F: FnOnce() -> Result<FaultInput, Box<dyn Error>>,
    {
        if !self.refinement_redraw.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let FaultAction::EventKill { node, .. } = action.action else {
            return Ok(None);
        };
        let input = input()?;
        let observed = self
            .event_observation
            .lock()
            .expect("fault event observation mutex poisoned")
            .take();
        if observed != Some(event_context_digest(&input.actions)) {
            return Ok(None);
        }
        let prefix_len = input.actions.len().saturating_sub(1);
        let Some(ordinal) = self
            .coordinate_state()
            .next_probe(&input.actions[..prefix_len], node)
        else {
            return Ok(None);
        };
        Ok(Some(RefinementRequest {
            parent_id,
            prefix: input.actions[..prefix_len].to_vec(),
            action: FaultAction::EventKill { node, ordinal },
        }))
    }

    fn refinement_dispatched(
        &self,
        evidence: &mut FaultCampaignEvidence,
        _request: &RefinementRequest<Self::Action>,
    ) {
        evidence.coordinate_refined = evidence.coordinate_refined.saturating_add(1);
    }

    fn source_entries<'a>(
        &self,
        source: &'a FaultArchiveReport,
    ) -> &'a [crate::archive::FaultArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &FaultArchiveReport) -> Result<FaultInput, Box<dyn Error>> {
        source
            .entries
            .iter()
            .max_by_key(|entry| {
                (
                    entry.key,
                    std::cmp::Reverse(entry.input.actions.len()),
                    std::cmp::Reverse(entry.id),
                )
            })
            .map(|entry| entry.input.clone())
            .ok_or_else(|| "the fault source archive has no retained entries".into())
    }
}

/// Run a fault-package campaign and return its report plus whole-tree
/// checkpoint.
///
/// # Errors
///
/// Returns an error when the campaign cannot run to completion.
pub fn run_fault_campaign_checkpointed(
    game: &FaultGame,
    config: &FaultCampaignConfig,
    origin: &FaultCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(FaultCampaignReport, FaultSnapshotCheckpoint), Box<dyn Error>> {
    let (report, checkpoint) =
        run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)?;
    Ok((FaultCampaignReport::new(report), checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{consonance::DEFAULT_RAM_MIB, target::DEFAULT_HORIZON_NANOS};

    fn config(knobs: &[&str]) -> FaultConfig {
        FaultConfig {
            knobs: knobs.iter().map(|knob| (*knob).to_owned()).collect(),
            horizon_nanos: DEFAULT_HORIZON_NANOS,
            ram_mib: DEFAULT_RAM_MIB,
        }
    }

    fn game() -> FaultGame {
        FaultGame::new(b"kernel", b"initramfs", &config(&[]))
    }

    fn run(nodes: u16, hooks: Vec<u32>) -> FaultCampaignRun {
        FaultCampaignRun {
            vocabulary: FaultVocabulary::new(nodes, hooks).expect("vocabulary"),
            coordinate_refinement: true,
            refinement_redraw: true,
        }
    }

    #[test]
    fn the_recorded_policy_set_is_exact_and_adapter_owned() {
        let game = game();
        let policies = game.policies(&run(1, vec![1, 2]));
        assert_eq!(
            game.resolve_recorded(&policies).expect("resolve"),
            run(1, vec![1, 2])
        );
        let mut foreign = policies.clone();
        foreign.insert("workload".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
        let mut wrong = policies;
        wrong.insert(TERMINAL_POLICY_FIELD.to_owned(), "deadline".to_owned());
        assert!(game.resolve_recorded(&wrong).is_err());
    }

    #[test]
    fn the_stream_pins_the_bundle_alphabet() {
        let game = game();
        let etcd = game.policies(&run(1, vec![1, 2]));
        let postgres = game.policies(&run(1, vec![1, 2, 3, 4]));
        assert_ne!(etcd, postgres, "two bundles never record the same alphabet");
        assert_eq!(
            game.resolve_recorded(&postgres).expect("resolve"),
            run(1, vec![1, 2, 3, 4])
        );
        let mut unknown = etcd;
        unknown.insert(VOCABULARY_FIELD.to_owned(), "hand-written".to_owned());
        assert!(game.resolve_recorded(&unknown).is_err());
    }

    #[test]
    fn the_image_identity_covers_the_workload_image_bytes() {
        let etcd = game();
        let postgres = FaultGame::new(b"kernel", b"postgres-initramfs", &config(&[]));
        assert_ne!(etcd.image_identity(), postgres.image_identity());
        assert_ne!(etcd.image_sha256(), postgres.image_sha256());
    }

    #[test]
    fn the_recorded_policies_pin_the_knobs_and_the_horizon() {
        let game = game();
        let policies = game.policies(&run(1, vec![1, 2]));
        let tuned = FaultGame::new(b"kernel", b"initramfs", &config(&["faultlab.puts=20"]));
        assert!(tuned.resolve_recorded(&policies).is_err());
        let short = FaultGame::new(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                horizon_nanos: 100_000_000,
                ..config(&[])
            },
        );
        assert!(short.resolve_recorded(&policies).is_err());
        assert_eq!(game.image_sha256(), short.image_sha256());
        assert_eq!(
            policies.get(HORIZON_FIELD).map(String::as_str),
            Some("2000000000")
        );
    }

    #[test]
    fn event_coordinate_brackets_keep_fired_and_unfired_bounds() {
        let mut bracket = EventCoordinateBracket::default();
        assert_eq!(bracket.next_probe(), None);
        bracket.observe(8, true);
        bracket.observe(40, false);
        assert_eq!(bracket.fired, Some(8));
        assert_eq!(bracket.unfired, Some(40));
        assert_eq!(bracket.next_probe(), Some(24));

        // Repeated observations tighten the same two sides independently and
        // keep selecting an interior point instead of repeating the boundary.
        bracket.observe(12, true);
        bracket.observe(32, false);
        assert_eq!(bracket.fired, Some(12));
        assert_eq!(bracket.unfired, Some(32));
        assert_eq!(bracket.next_probe(), Some(22));

        let mut interior = EventCoordinateBracket::default();
        interior.observe(8, true);
        assert_eq!(interior.next_probe(), Some(4));
        interior.observe(4, true);
        assert_eq!(interior.next_probe(), Some(2));

        let mut one_event = EventCoordinateBracket::default();
        one_event.observe(2, false);
        assert_eq!(one_event.next_probe(), Some(1));
        one_event.observe(1, true);
        assert_eq!(one_event.next_probe(), None);

        let mut bounded = EventCoordinateBracket::default();
        bounded.observe(128, true);
        for ordinal in 1..MAX_COORDINATE_OBSERVATIONS as u64 {
            bounded.observe(ordinal, true);
        }
        let first = bounded.next_probe().expect("unobserved coordinate");
        bounded.observe(first, true);
        assert_ne!(bounded.next_probe(), Some(first));
    }

    #[test]
    fn legacy_event_coordinate_refinement_remains_replayable() {
        let anchor = 100_u64;
        assert_eq!(refine_legacy_event_coordinate(anchor, 0), anchor);
        assert_eq!(refine_legacy_event_coordinate(anchor, 2), anchor + 1);
        assert_eq!(refine_legacy_event_coordinate(anchor, (5 << 2) | 1), 68);
        assert_eq!(refine_legacy_event_coordinate(1, 1), 1);
        assert_eq!(refine_legacy_event_coordinate(u64::MAX, 2), u64::MAX);
        assert_ne!(
            mix_legacy_coordinate_seed(7, 0),
            mix_legacy_coordinate_seed(7, 1)
        );
    }

    #[test]
    fn event_coordinate_observations_are_folded_in_admission_order() {
        let game = game();
        let first_prefix = [FaultAction::Wait(0)];
        let second_prefix = [FaultAction::Kill(0)];
        let fired = FaultAction::EventKill {
            node: 2,
            ordinal: 8,
        };
        let unfired = FaultAction::EventKill {
            node: 2,
            ordinal: 40,
        };
        let mut fired_input = first_prefix.to_vec();
        fired_input.push(fired);
        let mut unfired_input = first_prefix.to_vec();
        unfired_input.push(unfired);
        game.record_event_outcome(fired_input.clone(), true);
        game.record_event_outcome(unfired_input.clone(), false);
        game.observe_event_coordinate(
            &fired_input[..fired_input.len() - 1],
            2,
            8,
            game.take_event_outcome(&fired_input).unwrap(),
        );
        game.observe_event_coordinate(
            &unfired_input[..unfired_input.len() - 1],
            2,
            40,
            game.take_event_outcome(&unfired_input).unwrap(),
        );

        let state = game.coordinate_state();
        let bracket = state
            .contexts
            .iter()
            .find(|context| {
                context.prefix == event_context_digest(&first_prefix) && context.node == 2
            })
            .expect("first prefix bracket")
            .bracket;
        assert_eq!(bracket.fired, Some(8));
        assert_eq!(bracket.unfired, Some(40));
        assert_eq!(bracket.next_probe(), Some(24));

        game.observe_event_coordinate(&second_prefix, 2, 64, true);
        let second = state.contexts.iter().find(|context| {
            context.prefix == event_context_digest(&second_prefix) && context.node == 2
        });
        assert!(
            second.is_none(),
            "the copied state must not observe later admissions"
        );
        let state = game.coordinate_state();
        let second = state
            .contexts
            .iter()
            .find(|context| {
                context.prefix == event_context_digest(&second_prefix) && context.node == 2
            })
            .expect("second prefix bracket");
        assert_eq!(second.bracket.fired, Some(64));
        assert_eq!(second.bracket.unfired, None);
    }

    #[test]
    fn admitted_event_observation_requests_the_next_exact_prefix_probe() {
        let game = game();
        let run = run(1, vec![]);
        game.initial_draw_state(&run, None).expect("draw state");
        let prefix = vec![FaultAction::Wait(0), FaultAction::Kill(0)];
        let observed = FaultAction::EventKill {
            node: 0,
            ordinal: 8,
        };
        let mut input = prefix.clone();
        input.push(observed);
        game.record_event_outcome(input.clone(), true);
        let action = FaultCampaignActionResult {
            action: observed,
            observations: Vec::new(),
            milestones: FaultMilestones::default(),
            dead: false,
            victory: false,
            failed: false,
            candidate: None,
        };
        let mut evidence = FaultCampaignEvidence::default();
        game.merge_action_evidence(&mut evidence, &action, 1, || {
            Ok(FaultInput {
                actions: input.clone(),
            })
        })
        .expect("merge event observation");
        let request = game
            .refinement_request(7, &action, || {
                Ok(FaultInput {
                    actions: input.clone(),
                })
            })
            .expect("refinement hook")
            .expect("fired event requests a redraw");
        assert_eq!(request.parent_id, 7);
        assert_eq!(request.prefix, prefix);
        assert_eq!(
            request.action,
            FaultAction::EventKill {
                node: 0,
                ordinal: 4,
            }
        );
        assert_eq!(evidence.coordinate_attempted, 1);
        assert_eq!(evidence.coordinate_fired, 1);
        game.refinement_dispatched(&mut evidence, &request);
        assert_eq!(evidence.coordinate_refined, 1);
    }

    #[test]
    fn live_and_recorded_refinement_draws_replay_the_same_pending_suffix() {
        let game = game();
        let run = run(1, vec![]);
        let state = FaultDrawState::default();
        let parent = FaultInput {
            actions: vec![FaultAction::Wait(0)],
        };
        let request = RefinementRequest {
            parent_id: 7,
            prefix: vec![FaultAction::Wait(0), FaultAction::Kill(0)],
            action: FaultAction::EventKill {
                node: 0,
                ordinal: 4,
            },
        };
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let live = game
            .expand_suffix_for_refinement(
                &run,
                &state,
                &parent,
                &request,
                SuffixShape::OneOrTwo,
                mixture,
                19,
            )
            .expect("live refinement draw");
        let recorded = game
            .expand_suffix_recorded_for_refinement(
                &run,
                &state,
                &parent,
                &request,
                SuffixShape::OneOrTwo,
                mixture,
                None,
                19,
            )
            .expect("recorded refinement draw");
        assert_eq!(live, recorded);
        assert_eq!(live.first(), Some(&FaultAction::Kill(0)));
        assert_eq!(
            live.get(1),
            Some(&FaultAction::EventKill {
                node: 0,
                ordinal: 4,
            })
        );
    }
}
