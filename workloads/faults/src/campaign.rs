// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, num::NonZeroU64, path::PathBuf, sync::OnceLock};

use searcher::{
    search::{
        archive::{RetentionPolicy, SelectorPolicy},
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignConfig,
            CampaignJobResult, CampaignModeReport, CampaignOrigin, CampaignProgressRecord,
            CampaignStreamHeader, CampaignTypes, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            Evaluation, InitialDrawState, InputPolicy, Reporting, SnapshotCheckpoint,
            TargetExecution, WorkloadPolicies, postcard_result_sha256, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
        duration::{DurationDraw, DurationRequest},
        rollout::{ExecutionDisposition, Outcome},
    },
    target::ExitKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{
        DURATION_IDENTIFIER, FaultArchiveKey, FaultArchiveReport, FaultBugRecord, FaultInput,
        FaultMilestones, FaultProgressWatermark, KEY_POLICY_IDENTIFIER, MAX_RECORDED_BUGS,
        REPLACEMENT_IDENTIFIER, action_cost, archive_key, bug_outcome, merge_milestones,
        merge_progress_watermark, milestone_key, milestones, sample_action,
    },
    bundle::FaultVocabulary,
    consonance::{FaultConfig, FaultTarget, identity, snapshot_memory_charge},
    target::{FaultAction, FaultObservations, FaultSnapshot, MAX_FAULT_ACTIONS},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "faultlab-consonance-campaign-stream-v4";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-consonance-snapshot-checkpoint-v4";
pub const TERMINAL_POLICY_IDENTIFIER: &str = "assertion_or_crash";

const VOCABULARY_FIELD: &str = "action_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const IMAGE_FIELD: &str = "image";
const HORIZON_FIELD: &str = "horizon_nanos";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultNoTableHeader;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultCampaignRun {
    pub vocabulary: FaultVocabulary,
}

pub struct FaultWorkload {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: FaultConfig,
    identity: String,
    root_seal: OnceLock<u64>,
}

impl FaultWorkload {
    #[must_use]
    pub fn new(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Self {
        Self {
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            config: config.clone(),
            identity: identity(kernel, initramfs, config),
            root_seal: OnceLock::new(),
        }
    }

    #[must_use]
    pub fn config(&self) -> &FaultConfig {
        &self.config
    }

    #[must_use]
    pub fn root_seal(&self) -> Option<u64> {
        self.root_seal.get().copied()
    }

    #[must_use]
    pub fn image_identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Clone, Default)]
pub struct FaultCampaignEvidence {
    aggregate: FaultMilestones,
    watermark: FaultProgressWatermark,
    watchdog_cutoffs: u64,
    champion_input: FaultInput,
    champion_milestones: FaultMilestones,
    bugs: Vec<FaultBugRecord>,
}

pub type FaultCampaignOrigin = CampaignOrigin<FaultWorkload>;
pub type FaultSnapshotCheckpoint = SnapshotCheckpoint<FaultSnapshot>;
pub type FaultCampaignStreamHeader = CampaignStreamHeader<FaultNoTableHeader>;
pub type FaultCampaignModeReport = CampaignModeReport<FaultAction, FaultArchiveReport>;
pub type FaultCampaignProgressRecord = CampaignProgressRecord<FaultArchiveKey>;
type FaultCampaignActionResult = CampaignActionResult<FaultWorkload>;
type FaultCampaignJobResult = CampaignJobResult<FaultWorkload>;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultCampaignReport {
    #[serde(flatten)]
    pub campaign: FaultCampaignModeReport,
    pub bugs_found: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executions_to_first_bug: Option<u64>,
}

impl FaultCampaignReport {
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

pub struct FaultCampaignConfig {
    pub campaign_seed: u64,
    pub vocabulary: FaultVocabulary,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub selector: SelectorPolicy,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub objective_witness_path: Option<PathBuf>,
}

impl FaultCampaignConfig {
    fn generic(&self) -> CampaignConfig<FaultWorkload> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker: DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: FaultCampaignRun {
                vocabulary: self.vocabulary.clone(),
            },
            suffix: self.suffix,
            mixture: self.mixture,
            retention: self.retention,
            selector: self.selector.clone(),
            objective_witness_path: self.objective_witness_path.clone(),
        }
    }
}

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("fault package stream is missing {field}").into())
}

fn merge_action_milestones(aggregate: &mut FaultMilestones, target: &FaultTarget) {
    if target.exit_kind() != ExitKind::Ok {
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation));
    }
}

fn outcome(target: &FaultTarget) -> Outcome {
    Outcome {
        objective_reached: !target.failed() && target.found_bug(),
        disposition: if target.failed() {
            ExecutionDisposition::Failed
        } else if target.observation().stop.is_continuable() {
            ExecutionDisposition::Runnable
        } else {
            ExecutionDisposition::Terminal
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_job(
    target: &mut FaultTarget,
    origin_snapshot: &FaultSnapshot,
    replay: &[FaultAction],
    parent_actions: usize,
    parent_milestones: FaultMilestones,
    suffix: &[FaultAction],
    max_actions: usize,
    stop_rollout_on_objective: bool,
) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
    target.restore(origin_snapshot)?;
    for action in replay {
        if outcome(target).disposition.is_terminal() {
            break;
        }
        target.apply(*action);
    }
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = outcome(target);
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: parent_outcome
                .disposition
                .is_failed()
                .then(|| target.last_action_observations().to_vec()),
            actions,
        });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(*action);
        merge_action_milestones(&mut aggregate, target);
        let observations = target.last_action_observations().to_vec();
        let raw_outcome = outcome(target);
        let objective_reached = raw_outcome.objective_reached && !objective_seen;
        objective_seen |= raw_outcome.objective_reached;
        let outcome = Outcome {
            objective_reached,
            disposition: raw_outcome.disposition,
        };
        let candidate = match target.snapshot() {
            Some(snapshot) if matches!(outcome.disposition, ExecutionDisposition::Runnable) => {
                Some(CampaignCandidate {
                    key: archive_key(target.observation()),
                    viable: true,
                    snapshot,
                })
            }
            _ => None,
        };
        actions.push(CampaignActionResult {
            action: *action,
            observations,
            milestones: aggregate,
            outcome,
            candidate,
        });
        if outcome.should_stop(stop_rollout_on_objective) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        actions,
    })
}

impl CampaignTypes for FaultWorkload {
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
    type DrawState = ();
    type DrawCheckpoint = ();
    type DrawHeader = FaultNoTableHeader;
}

impl Reporting for FaultWorkload {
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn workload_identity_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(&self.kernel);
        digest.update(&self.initramfs);
        format!("{:x}", digest.finalize())
    }

    fn action_cost_unit(&self) -> &'static str {
        "guest_ticks"
    }

    fn execution_work_unit(&self) -> &'static str {
        "guest_ticks"
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
            horizon_nanos: crate::target::DEFAULT_HORIZON_NANOS,
            executions: state.executions,
            milestones: evidence.aggregate,
            progress_watermark: evidence.watermark,
            champion_input: evidence.champion_input.clone(),
            entries: state.entries,
            progress_curve: state.progress_curve,
            retained: state.retained,
            rejected: state.rejected,
            deaths: state
                .terminal_endpoints
                .saturating_sub(state.terminal_objectives),
            watchdog_cutoffs: evidence.watchdog_cutoffs,
            bugs: evidence.bugs.clone(),
            selector: state.selector,
        }
    }
}

impl InputPolicy for FaultWorkload {
    fn max_action_limit(&self) -> usize {
        MAX_FAULT_ACTIONS
    }

    fn max_action_cost(&self) -> u64 {
        u64::from(u16::MAX)
    }

    fn policies(&self, run: &FaultCampaignRun) -> WorkloadPolicies {
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
                crate::target::DEFAULT_HORIZON_NANOS.to_string(),
            ),
            (VOCABULARY_FIELD.to_owned(), run.vocabulary.identifier()),
        ])
        .collect()
    }

    fn resolve_recorded(
        &self,
        policies: &WorkloadPolicies,
    ) -> Result<FaultCampaignRun, Box<dyn Error>> {
        let run = FaultCampaignRun {
            vocabulary: FaultVocabulary::from_identifier(recorded(policies, VOCABULARY_FIELD)?)?,
        };
        let expected = self.policies(&run);
        if policies != &expected {
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
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn initial_draw_state(
        &self,
        _run: &FaultCampaignRun,
        _origin: Option<(&str, &FaultArchiveReport)>,
    ) -> Result<InitialDrawState<Self>, Box<dyn Error>> {
        Ok(((), None))
    }

    fn duration_request(
        &self,
        _run: &FaultCampaignRun,
        parent: FaultArchiveKey,
        remaining_work: Option<NonZeroU64>,
    ) -> Option<DurationRequest<FaultArchiveKey>> {
        Some(DurationRequest {
            context: FaultArchiveKey {
                alive: parent.alive,
                event_ready: parent.event_ready,
                hooks_running: u64::from(parent.hooks_running > 0),
                parked: u64::from(parent.parked > 0),
                event_kill_fires: u64::from(parent.event_kill_fires > 0),
                event_park_fires: u64::from(parent.event_park_fires > 0),
                workload_running: parent.workload_running,
                checks_finished: u64::from(parent.checks_finished > 0),
                ..FaultArchiveKey::default()
            },
            max_duration: NonZeroU64::new(remaining_work.map_or(u64::from(u16::MAX), |work| {
                work.get().min(u64::from(u16::MAX))
            }))?,
        })
    }

    fn expand_suffix_duration(
        &self,
        run: &FaultCampaignRun,
        _state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
        draw: DurationDraw<FaultArchiveKey>,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        let ticks = std::num::NonZeroU16::new(u16::try_from(draw.duration.get())?)
            .ok_or("wait duration must be positive")?;
        let mut suffix = draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            mutation_seed,
            |_| Ok(None),
            |rand| sample_action(rand, &run.vocabulary, draw.context.event_ready),
        )?;
        for action in &mut suffix {
            if matches!(action, FaultAction::Wait(_)) {
                *action = FaultAction::Wait(ticks);
            }
        }
        Ok(suffix)
    }

    fn expand_suffix_recorded_duration(
        &self,
        run: &FaultCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        _before: Option<&()>,
        mutation_seed: u64,
        draw: Option<DurationDraw<FaultArchiveKey>>,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        self.expand_suffix_duration(
            run,
            state,
            shape,
            mixture,
            mutation_seed,
            draw.ok_or("fault campaign is missing its duration choice")?,
        )
    }

    fn duration_of_action(
        &self,
        _run: &FaultCampaignRun,
        action: &FaultAction,
    ) -> Option<NonZeroU64> {
        match action {
            FaultAction::Wait(ticks) => NonZeroU64::new(u64::from(ticks.get())),
            _ => None,
        }
    }

    fn expand_suffix(
        &self,
        run: &FaultCampaignRun,
        _state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            mutation_seed,
            |_| Ok(None),
            |rand| sample_action(rand, &run.vocabulary, 0),
        )
    }
}

impl TargetExecution for FaultWorkload {
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

    fn execution_work(&self, target: &FaultTarget) -> u64 {
        target.execution_ticks()
    }

    fn action_cost_fn(&self) -> fn(&FaultAction) -> u64 {
        action_cost
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
        stop_rollout_on_objective: bool,
    ) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            origin_snapshot,
            replay,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            stop_rollout_on_objective,
        )
    }
}

impl Evaluation for FaultWorkload {
    fn execution_disposition(&self, target: &FaultTarget) -> ExecutionDisposition {
        outcome(target).disposition
    }

    fn objective_reached(
        &self,
        _run: &FaultCampaignRun,
        target: &FaultTarget,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(outcome(target).objective_reached)
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

    fn merge_preparation_failure(
        &self,
        evidence: &mut FaultCampaignEvidence,
        observations: &[FaultObservations],
    ) {
        evidence.watchdog_cutoffs = evidence.watchdog_cutoffs.saturating_add(
            observations
                .iter()
                .filter(|observation| observation.watchdog_cutoff)
                .count() as u64,
        );
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
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        evidence.watchdog_cutoffs = evidence.watchdog_cutoffs.saturating_add(
            action
                .observations
                .iter()
                .filter(|observation| observation.watchdog_cutoff)
                .count() as u64,
        );
        let bug = action
            .observations
            .iter()
            .find(|observation| observation.is_bug())
            .filter(|_| evidence.bugs.len() < MAX_RECORDED_BUGS);
        let champion = (milestone_key(action.milestones)
            > milestone_key(evidence.champion_milestones))
        .then_some(action.milestones);
        if bug.is_some() || champion.is_some() {
            let input = input()?;
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

pub fn run_fault_campaign_checkpointed(
    game: &FaultWorkload,
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
    use crate::consonance::DEFAULT_RAM_MIB;

    fn config(knobs: &[&str]) -> FaultConfig {
        FaultConfig {
            knobs: knobs.iter().map(|knob| (*knob).to_owned()).collect(),
            ram_mib: DEFAULT_RAM_MIB,
        }
    }

    fn game() -> FaultWorkload {
        FaultWorkload::new(b"kernel", b"initramfs", &config(&[]))
    }

    fn run(nodes: u16, hooks: Vec<u32>) -> FaultCampaignRun {
        FaultCampaignRun {
            vocabulary: FaultVocabulary::new(nodes, hooks).expect("vocabulary"),
        }
    }

    #[test]
    fn preparation_failure_counts_cutoffs_without_importing_oracle_evidence() {
        let game = game();
        let mut evidence = FaultCampaignEvidence::default();
        let observation = FaultObservations {
            watchdog_cutoff: true,
            stop: crate::target::FaultStop::Crash,
            ticks: 123,
            ..FaultObservations::default()
        };
        game.merge_preparation_failure(&mut evidence, &[observation]);
        assert_eq!(evidence.watchdog_cutoffs, 1);
        assert!(evidence.bugs.is_empty());
        assert_eq!(evidence.aggregate, FaultMilestones::default());
        assert_eq!(evidence.watermark, FaultProgressWatermark::default());
    }

    #[test]
    fn wait_contexts_separate_lifecycle_states() {
        let game = game();
        let run = run(3, vec![]);
        let parent = FaultArchiveKey {
            alive: 7,
            ..FaultArchiveKey::default()
        };
        let request = game.duration_request(&run, parent, None).unwrap();
        let down = FaultArchiveKey { alive: 3, ..parent };
        assert_ne!(
            request.context,
            game.duration_request(&run, down, None).unwrap().context
        );
        assert_eq!(
            game.duration_request(&run, parent, NonZeroU64::new(16))
                .unwrap()
                .max_duration
                .get(),
            16
        );
    }

    #[test]
    fn recorded_wait_choices_survive_suffix_reconstruction() {
        let game = game();
        let run = run(3, vec![]);
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let draw = DurationDraw {
            context: FaultArchiveKey::default(),
            max_duration: NonZeroU64::new(u64::from(u16::MAX)).unwrap(),
            duration: NonZeroU64::new(8192).unwrap(),
        };
        let mut waits = 0;
        for seed in 0..128 {
            let suffix = game
                .expand_suffix_duration(&run, &(), SuffixShape::OneOrTwo, mixture, seed, draw)
                .unwrap();
            let replay = game
                .expand_suffix_recorded_duration(
                    &run,
                    &(),
                    SuffixShape::OneOrTwo,
                    mixture,
                    None,
                    seed,
                    Some(draw),
                )
                .unwrap();
            assert_eq!(suffix, replay);
            for action in suffix {
                if let FaultAction::Wait(ticks) = action {
                    assert_eq!(ticks.get(), 8192);
                    waits += 1;
                }
            }
        }
        assert!(waits > 0);
        assert!(
            game.expand_suffix_recorded_duration(
                &run,
                &(),
                SuffixShape::OneOrTwo,
                mixture,
                None,
                1,
                None
            )
            .is_err()
        );
        let out_of_range = DurationDraw {
            duration: NonZeroU64::new(65536).unwrap(),
            ..draw
        };
        assert!(
            game.expand_suffix_duration(&run, &(), SuffixShape::OneOrTwo, mixture, 1, out_of_range)
                .is_err()
        );
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
        let postgres = FaultWorkload::new(b"kernel", b"postgres-initramfs", &config(&[]));
        assert_ne!(etcd.image_identity(), postgres.image_identity());
        assert_ne!(
            etcd.workload_identity_sha256(),
            postgres.workload_identity_sha256()
        );
    }

    #[test]
    fn the_recorded_policies_pin_the_knobs_and_the_horizon() {
        let game = game();
        let policies = game.policies(&run(1, vec![1, 2]));
        let tuned = FaultWorkload::new(b"kernel", b"initramfs", &config(&["faultlab.puts=20"]));
        assert!(tuned.resolve_recorded(&policies).is_err());
        let mut different_timing = policies.clone();
        different_timing.insert(HORIZON_FIELD.to_owned(), "100000000".to_owned());
        assert!(game.resolve_recorded(&different_timing).is_err());
        assert_eq!(
            policies.get(HORIZON_FIELD).map(String::as_str),
            Some("500000000")
        );
    }
}
