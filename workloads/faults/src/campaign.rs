// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    io::Write,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use consonance_client::{cache::CacheIndex, session::WorkerLauncher};

use searcher::{
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignConfig,
            CampaignExecutionOptions, CampaignJobResult, CampaignModeReport, CampaignOrigin,
            CampaignProgressRecord, CampaignStreamHeader, CampaignTypes, Evaluation, InputPolicy,
            Reporting, SnapshotCheckpoint, TargetExecution, ThreadPlacement, WorkloadPolicies,
            default_window, postcard_value_sha256, run_campaign_checkpointed_with_options,
        },
        draw::{DrawMixture, MixtureDraw, draw_suffix},
        draw_tables::{DrawTableHeader, DrawTables, biased_step},
        duration::{DurationDraw, DurationRequest},
        empirical_steps::EmpiricalStepCheckpoint,
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome},
        telemetry::TargetCounters,
    },
    target::ExitKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{
        DURATION_IDENTIFIER, DrawProfile, FaultArchiveKey, FaultArchiveReport, FaultBugRecord,
        FaultFailureRecord, FaultHangRecord, FaultInput, FaultMilestones, FaultProgressWatermark,
        KEY_POLICY_IDENTIFIER, MAX_RECORDED_BUGS, MAX_RECORDED_FAILURES, MAX_RECORDED_HANGS,
        ParkThresholds, REPLACEMENT_IDENTIFIER, UndecidedChecks, action_cost, archive_key,
        bug_outcome, merge_milestones, merge_progress_watermark, milestone_key, milestones,
        park_threshold_bucket, sample_action, sample_profiled_action,
    },
    assertion::Assertions,
    bundle::FaultVocabulary,
    consonance::{
        FaultConfig, FaultTarget, identity, replay_endpoint_certificate, snapshot_memory_charge,
        validate_snapshot_prefix_certificates,
    },
    target::{
        FaultAction, FaultEndpointCertificate, FaultObservations, FaultOperation, FaultSnapshot,
    },
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "faultlab-consonance-campaign-stream-v12";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-consonance-snapshot-root-v10";
pub const UML_CAMPAIGN_STREAM_FORMAT: &str = "faultlab-uml-campaign-stream-v9";
pub const UML_SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-uml-snapshot-root-v6";
pub const TERMINAL_POLICY_IDENTIFIER: &str = "assertion_or_crash";
const ADAPTIVE_DURATION_MAX_TICKS: u64 = 1_024;

const VOCABULARY_FIELD: &str = "action_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const HOST_FAILURE_POLICY_FIELD: &str = "host_failure_policy";
const HOST_FAILURE_POLICY: &str = "host_incomplete_v3";
const IMAGE_FIELD: &str = "image";
const ACTION_FORMAT_FIELD: &str = "action_format";
const ACTION_FORMAT: &str = "fault-action-duration-coverage-v5-event-history";

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
    snapshot_cache: Option<Arc<dyn CacheIndex>>,
    session_worker: Option<WorkerLauncher>,
}

impl FaultWorkload {
    pub fn new(kernel: &[u8], initramfs: &[u8], config: &FaultConfig) -> Result<Self, String> {
        Ok(Self {
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            config: config.clone(),
            identity: identity(kernel, initramfs, config)?,
            root_seal: OnceLock::new(),
            snapshot_cache: None,
            session_worker: None,
        })
    }

    #[must_use]
    pub fn with_snapshot_cache(mut self, cache: Option<Arc<dyn CacheIndex>>) -> Self {
        self.snapshot_cache = cache;
        self
    }

    #[must_use]
    pub fn with_session_worker(mut self, worker: Option<WorkerLauncher>) -> Self {
        self.session_worker = worker;
        self
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

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct FaultCampaignEvidence {
    root_actions: Vec<FaultAction>,
    aggregate: FaultMilestones,
    watermark: FaultProgressWatermark,
    champion_input: FaultInput,
    champion_milestones: FaultMilestones,
    bugs: Vec<FaultBugRecord>,
    hangs: Vec<FaultHangRecord>,
    #[serde(default)]
    failures: Vec<FaultFailureRecord>,
    assertions: Assertions,
    park_sites: BTreeMap<u64, u64>,
    park_reads: BTreeMap<u64, u64>,
    park_thresholds: BTreeMap<u32, ParkThresholds>,
    undecided_checks: UndecidedChecks,
    undecided_execution: Option<(u64, u64)>,
}

impl FaultCampaignEvidence {
    fn note_undecided_keys(&mut self, sequence: u64, keys: u64) {
        let counted = match self.undecided_execution {
            Some((execution, counted)) if execution == sequence => counted,
            _ => 0,
        };
        let totals = &mut self.undecided_checks;
        totals.keys = totals.keys.saturating_sub(counted).saturating_add(keys);
        match (counted > 0, keys > 0) {
            (false, true) => totals.executions = totals.executions.saturating_add(1),
            (true, false) => totals.executions = totals.executions.saturating_sub(1),
            _ => {}
        }
        self.undecided_execution = Some((sequence, keys));
    }
}

pub type FaultCampaignOrigin = CampaignOrigin<FaultWorkload>;
pub type FaultSnapshotCheckpoint = SnapshotCheckpoint<FaultSnapshot>;
pub type FaultCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type FaultCampaignModeReport = CampaignModeReport<FaultAction, FaultArchiveReport>;
pub type FaultCampaignProgressRecord = CampaignProgressRecord<FaultArchiveKey>;
type FaultCampaignActionResult = CampaignActionResult<FaultWorkload>;
type FaultCampaignJobResult = CampaignJobResult<FaultWorkload>;

fn snapshot_digest(snapshot: &FaultSnapshot) -> Result<String, Box<dyn Error>> {
    postcard_value_sha256(&(
        "fault-snapshot-identity-v2",
        snapshot.history.receipt().root(),
        &snapshot.observation,
        snapshot.failed,
        snapshot.endpoint,
        snapshot
            .event_history
            .map(|history| (control_proto::EventHistory::VERSION, history)),
    ))
}

fn fault_result_sha256(result: &FaultCampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|result| {
            let candidate = result
                .candidate
                .as_ref()
                .map(|candidate| {
                    Ok::<_, Box<dyn Error>>((
                        &candidate.key,
                        candidate.viable,
                        snapshot_digest(&candidate.snapshot)?,
                    ))
                })
                .transpose()?;
            Ok::<_, Box<dyn Error>>((
                &result.action,
                &result.observations,
                &result.milestones,
                &result.outcome,
                candidate,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
    postcard_value_sha256(&(
        "fault-result-history-v2",
        &result.preparation_failure,
        &result.hang,
        actions,
    ))
}

fn endpoint_result_sha256(
    result: &FaultCampaignJobResult,
    endpoint: FaultEndpointCertificate,
    history: Option<(u64, [u8; 32])>,
) -> Result<String, Box<dyn Error>> {
    if matches!(endpoint, FaultEndpointCertificate::KvmWholeState(_)) != history.is_some() {
        return Err("fault result event history does not match its endpoint availability".into());
    }
    postcard_value_sha256(&(
        "fault-campaign-result-endpoint-v2",
        fault_result_sha256(result)?,
        endpoint,
        history.map(|history| (control_proto::EventHistory::VERSION, history)),
    ))
}

fn kvm_result_endpoint(
    result: &FaultCampaignJobResult,
    actions: &[FaultAction],
    observation: &FaultObservations,
    fresh_hash: impl FnOnce() -> Result<[u8; 32], String>,
) -> Result<FaultEndpointCertificate, Box<dyn Error>> {
    if let Some(last) = result.actions.last()
        && matches!(last.outcome.disposition, ExecutionDisposition::Runnable)
    {
        let candidate = last
            .candidate
            .as_ref()
            .ok_or("a runnable final action lacks its candidate certificate")?;
        validate_snapshot_prefix_certificates(&candidate.snapshot, false)?;
        if !candidate.viable
            || candidate.snapshot.failed
            || !candidate.snapshot.history.matches_actions(actions)
            || &candidate.snapshot.observation != observation
            || candidate.key != archive_key(observation)
        {
            return Err("the final candidate does not describe the job endpoint".into());
        }
        return match candidate.snapshot.endpoint {
            Some(FaultEndpointCertificate::KvmWholeState(hash)) => {
                Ok(FaultEndpointCertificate::KvmWholeState(hash))
            }
            _ => Err("the runnable final candidate lacks a KVM whole-state certificate".into()),
        };
    }
    Ok(FaultEndpointCertificate::KvmWholeState(fresh_hash()?))
}

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
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub mixture: DrawMixture,
    pub objective_witness_path: Option<PathBuf>,
    pub placement: Option<ThreadPlacement>,
}

impl FaultCampaignConfig {
    fn generic(&self) -> CampaignConfig<FaultWorkload> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: self.archive_entry_limit,
            window: default_window(self.workers),
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: FaultCampaignRun {
                vocabulary: self.vocabulary.clone(),
            },
            mixture: self.mixture,
            retention: self.retention,
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

#[derive(Debug)]
struct AttemptIncomplete {
    phase: &'static str,
    cause: &'static str,
}

impl std::fmt::Display for AttemptIncomplete {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "fault campaign incomplete: {} during {}; reservation not admitted",
            self.cause, self.phase
        )
    }
}

impl Error for AttemptIncomplete {}

pub(crate) fn require_completed_attempt(
    target: &FaultTarget,
    phase: &'static str,
) -> Result<(), Box<dyn Error>> {
    if target.watchdog_cutoffs() != 0 || target.infrastructure_failure() {
        return Err(Box::new(AttemptIncomplete {
            phase,
            cause: if target.infrastructure_failure() {
                "host infrastructure failure"
            } else {
                "guest progress watchdog cutoff"
            },
        }));
    }
    Ok(())
}

fn job_hung(target: &FaultTarget, phase: &'static str) -> Result<bool, Box<dyn Error>> {
    if target.infrastructure_failure() {
        return Err(Box::new(AttemptIncomplete {
            phase,
            cause: "host infrastructure failure",
        }));
    }
    Ok(target.watchdog_cutoffs() != 0)
}

fn hung_job(attempted: &[FaultAction]) -> FaultCampaignJobResult {
    CampaignJobResult {
        preparation_failure: None,
        hang: Some(attempted.to_vec()),
        actions: Vec::new(),
    }
}

pub(crate) fn capture_replayed_prefix(
    target: &mut FaultTarget,
    actions: &[FaultAction],
) -> Result<FaultSnapshot, Box<dyn Error>> {
    let mut endpoint = None;
    for (index, action) in actions.iter().enumerate() {
        if index + 1 == actions.len() {
            let captured = target.apply_replayed_and_snapshot(*action);
            require_completed_attempt(target, "branch prefix replay")?;
            endpoint = captured.map_err(|error| format!("certify branch endpoint: {error}"))?;
        } else {
            target.apply_replayed(*action);
            require_completed_attempt(target, "branch prefix replay")?;
        }
    }
    if target.actions().len() != actions.len() {
        return Err("branch prefix terminated before its selected point".into());
    }
    let snapshot = if actions.is_empty() {
        target
            .snapshot()
            .map_err(|error| format!("certify branch endpoint: {error}"))?
    } else {
        endpoint
    };
    snapshot.ok_or_else(|| "cannot search from a terminal point; rewind further".into())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_job(
    target: &mut FaultTarget,
    origin_snapshot: &FaultSnapshot,
    replay: &[FaultAction],
    replay_certificates: &[Option<[u8; 32]>],
    parent_milestones: FaultMilestones,
    suffix: &[FaultAction],
    stop: searcher::search::rollout::RolloutStop,
) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
    if replay.len() != replay_certificates.len() {
        return Err("fault replay certificate count does not match its action path".into());
    }
    let _cancel = crate::consonance::JobCancel::install(stop.cancellation().cloned());
    target.restore(origin_snapshot)?;
    if job_hung(target, "parent reconstruction")? {
        return Ok(hung_job(&[]));
    }
    for (action, certificate) in replay.iter().zip(replay_certificates) {
        if outcome(target).disposition.is_terminal() {
            return Err("fault replay stopped before its archived parent endpoint".into());
        }
        crate::job_observation::tail(false);
        let checked = target.apply_replayed_certified(*action, *certificate);
        target.verify_integrity()?;
        if job_hung(target, "parent replay")? {
            return Ok(hung_job(&[]));
        }
        checked?;
        crate::job_observation::tail(true);
    }
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = outcome(target);
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Err("fault parent reconstruction stopped before its archived endpoint".into());
    }
    for (index, action) in suffix.iter().enumerate() {
        let captured = target.apply_and_snapshot(*action);
        target.verify_integrity()?;
        if job_hung(target, "suffix execution")? {
            return Ok(hung_job(&suffix[..=index]));
        }
        let captured = captured?;
        merge_action_milestones(&mut aggregate, target);
        let observations = target.last_action_observations().to_vec();
        let raw_outcome = outcome(target);
        let objective_reached = raw_outcome.objective_reached && !objective_seen;
        objective_seen |= raw_outcome.objective_reached;
        let outcome = Outcome {
            objective_reached,
            disposition: raw_outcome.disposition,
        };
        let candidate = match captured {
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
        if outcome.should_stop(&stop) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        hang: None,
        actions,
    })
}

impl CampaignTypes for FaultWorkload {
    type JobObservation = crate::job_observation::FaultJobObservation;
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
}

impl Reporting for FaultWorkload {
    fn write_evidence_checkpoint(
        evidence: &Self::Evidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        serde_json::to_writer(out, evidence)?;
        Ok(())
    }

    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<Self::Evidence, Box<dyn Error>> {
        Ok(serde_json::from_slice(bytes)?)
    }

    fn stream_format(&self) -> &'static str {
        if self.config.uml().is_some() {
            UML_CAMPAIGN_STREAM_FORMAT
        } else {
            CAMPAIGN_STREAM_FORMAT
        }
    }

    fn checkpoint_format(&self) -> &'static str {
        if self.config.uml().is_some() {
            UML_SNAPSHOT_CHECKPOINT_FORMAT
        } else {
            SNAPSHOT_CHECKPOINT_FORMAT
        }
    }

    fn workload_identity_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(&self.kernel);
        digest.update(&self.initramfs);
        if let Some(guest) = self.config.uml() {
            digest.update(guest.identity.as_bytes());
        }
        format!("{:x}", digest.finalize())
    }

    fn action_cost_unit(&self) -> &'static str {
        "guest_ticks_plus_host_ticks_per_action"
    }

    fn execution_work_unit(&self) -> &'static str {
        "guest_ticks"
    }

    fn result_sha256(&self, result: &FaultCampaignJobResult) -> Result<String, Box<dyn Error>> {
        fault_result_sha256(result)
    }

    fn certified_result_sha256(
        &self,
        result: &FaultCampaignJobResult,
        target: &Self::Target,
    ) -> Result<String, Box<dyn Error>> {
        match result.hang {
            Some(_) if !job_hung(target, "result certification")? => {
                return Err("a hung job result names a target that did not hang".into());
            }
            Some(_) => {}
            None => require_completed_attempt(target, "result certification")?,
        }
        let endpoint = if self.config.uml().is_some() {
            FaultEndpointCertificate::UmlWholeStateUnavailable
        } else if target.failed() {
            FaultEndpointCertificate::KvmUnavailableAfterFailure
        } else {
            kvm_result_endpoint(result, target.actions(), target.observation(), || {
                target.state_hash_current()
            })?
        };
        let history = if matches!(endpoint, FaultEndpointCertificate::KvmWholeState(_)) {
            Some(target.event_history_current()?)
        } else {
            None
        };
        endpoint_result_sha256(result, endpoint, history)
    }

    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        validate_snapshot_prefix_certificates(snapshot, self.config.uml().is_some())?;
        snapshot_digest(snapshot)
    }

    fn archive_report(
        &self,
        evidence: &FaultCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> FaultArchiveReport {
        FaultArchiveReport {
            seed: state.seed,
            root_seal: self.root_seal.get().copied().unwrap_or_default(),
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
            watchdog_cutoffs: state.hung_jobs,
            bugs: evidence.bugs.clone(),
            hangs: evidence.hangs.clone(),
            failures: evidence.failures.clone(),
            selector: state.selector,
            assertions: evidence.assertions.clone(),
            park_sites: evidence.park_sites.clone(),
            park_reads: evidence.park_reads.clone(),
            park_thresholds: evidence.park_thresholds.clone(),
            undecided_checks: evidence.undecided_checks,
        }
    }
}

impl InputPolicy for FaultWorkload {
    fn policies(&self, run: &FaultCampaignRun) -> WorkloadPolicies {
        [
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, TERMINAL_POLICY_IDENTIFIER),
            (HOST_FAILURE_POLICY_FIELD, HOST_FAILURE_POLICY),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .chain([
            (IMAGE_FIELD.to_owned(), self.identity.clone()),
            (ACTION_FORMAT_FIELD.to_owned(), ACTION_FORMAT.to_owned()),
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

    fn sample_alphabet(
        &self,
        run: &FaultCampaignRun,
        _previous: Option<&FaultAction>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<FaultAction, Box<dyn Error>> {
        sample_action(rand, &run.vocabulary, 0, std::num::NonZeroU16::MIN, None)
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
                event_kill_fires: u64::from(parent.event_kill_fires > 0),
                event_park_fires: u64::from(parent.event_park_fires > 0),
                workload_running: parent.workload_running,
                checks_finished: u64::from(parent.checks_finished > 0),
                ..FaultArchiveKey::default()
            },
            max_duration: NonZeroU64::new(
                remaining_work
                    .map_or(ADAPTIVE_DURATION_MAX_TICKS, NonZeroU64::get)
                    .min(ADAPTIVE_DURATION_MAX_TICKS),
            )?,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_suffix_recorded_duration(
        &self,
        run: &FaultCampaignRun,
        state: &DrawTables<FaultAction>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        previous: Option<&FaultAction>,
        draw: Option<DurationDraw<FaultArchiveKey>>,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        self.expand_duration_recorded_or_live(
            run,
            state,
            mixture,
            before,
            mutation_seed,
            previous,
            draw.ok_or("fault campaign is missing its duration choice")?,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn expand_duration_recorded_or_live(
        &self,
        run: &FaultCampaignRun,
        state: &DrawTables<FaultAction>,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
        _previous: Option<&FaultAction>,
        draw: DurationDraw<FaultArchiveKey>,
        replay: bool,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        held_suffix(run, state, mixture, before, mutation_seed, replay, draw)
    }

    fn duration_of_action(
        &self,
        _run: &FaultCampaignRun,
        action: &FaultAction,
    ) -> Option<NonZeroU64> {
        NonZeroU64::new(action.ticks())
    }

    fn finish_stream_record(
        &self,
        _run: &FaultCampaignRun,
        state: &mut DrawTables<FaultAction>,
        retained: &[(usize, &[FaultAction])],
        evidence: &FaultCampaignEvidence,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        state.finish_record_with_feedback(retained, || park_feedback(evidence))
    }
}

const PARK_FEEDBACK_SCALE: u128 = 1024;

const PARK_FEEDBACK_PRIOR_LANDINGS: u128 = 10;

fn park_feedback(evidence: &FaultCampaignEvidence) -> BTreeMap<u64, u64> {
    let all_reads: u128 = evidence.park_reads.values().copied().map(u128::from).sum();
    let all_landings: u128 = evidence
        .park_sites
        .values()
        .copied()
        .map(u128::from)
        .sum::<u128>()
        .max(1);
    evidence
        .park_reads
        .iter()
        .filter(|(_, reads)| **reads > 0)
        .map(|(site, reads)| {
            let landings = u128::from(evidence.park_sites.get(site).copied().unwrap_or(0));
            let numerator = u128::from(*reads)
                .saturating_mul(all_landings)
                .saturating_add(PARK_FEEDBACK_PRIOR_LANDINGS.saturating_mul(all_reads))
                .saturating_mul(PARK_FEEDBACK_SCALE);
            let denominator = landings
                .saturating_add(PARK_FEEDBACK_PRIOR_LANDINGS)
                .saturating_mul(all_landings);
            let weight = u64::try_from(numerator / denominator).unwrap_or(u64::MAX);
            (*site, weight.max(1))
        })
        .collect()
}

fn event_is_ready(action: &FaultAction, event_ready: u64) -> bool {
    match &action.operation {
        FaultOperation::EventKill { node, .. } | FaultOperation::EventPark { node, .. } => 1_u64
            .checked_shl(u32::from(*node))
            .is_some_and(|bit| event_ready & bit != 0),
        _ => true,
    }
}

const PROFILE_SEED_MIX: u64 = 0x5157_4152_4d5f_6b31;

fn held_suffix(
    run: &FaultCampaignRun,
    state: &DrawTables<FaultAction>,
    mixture: MixtureDraw,
    before: Option<&EmpiricalStepCheckpoint>,
    mutation_seed: u64,
    replay: bool,
    draw: DurationDraw<FaultArchiveKey>,
) -> Result<Vec<FaultAction>, Box<dyn Error>> {
    let ticks = std::num::NonZeroU16::new(u16::try_from(draw.duration.get())?)
        .ok_or("action duration must be positive")?;
    let event_ready = draw.context.event_ready;
    let profile = DrawProfile::draw(&mut RomuDuoJrRand::with_seed(
        mutation_seed ^ PROFILE_SEED_MIX,
    ))?;
    let mut suffix =
        state.draw(before, replay, |view| {
            draw_suffix(
                mixture.mixture,
                mixture.weight,
                mutation_seed,
                None,
                |rand| {
                    Ok(biased_step(view, rand)?
                        .filter(|action| event_is_ready(action, event_ready)))
                },
                |_, rand| {
                    sample_profiled_action(
                        rand,
                        &run.vocabulary,
                        event_ready,
                        ticks,
                        Some(view),
                        profile,
                    )
                },
            )
        })?;
    for action in &mut suffix {
        *action = action.with_ticks(ticks);
    }
    Ok(suffix)
}

impl TargetExecution for FaultWorkload {
    fn worker_locality(&self, target: &FaultTarget) -> searcher::search::locality::WorkerLocality {
        target.worker_locality()
    }

    fn snapshot_locality(
        &self,
        snapshot: &FaultSnapshot,
    ) -> Option<searcher::search::locality::LocalityKey> {
        Some(searcher::search::locality::LocalityKey(
            snapshot.history.receipt().root(),
        ))
    }

    fn begin_job_observation(&self) {
        crate::job_observation::begin();
    }
    fn take_job_observation(&self) -> Option<Self::JobObservation> {
        crate::consonance::take_job_observation()
    }
    fn begin_coordinator_session(&self) -> Result<(), Box<dyn Error>> {
        crate::consonance::begin_coordinator_session()
    }
    fn finish_coordinator_session(&self) -> Result<(), Box<dyn Error>> {
        crate::consonance::finish_coordinator_session()
    }
    fn verify_integrity(&self) -> Result<(), Box<dyn Error>> {
        if let Some(cache) = &self.snapshot_cache {
            cache.verify_integrity()?;
        }
        Ok(())
    }

    fn new_target(&self) -> Result<FaultTarget, String> {
        let target = FaultTarget::new(
            &self.kernel,
            &self.initramfs,
            &self.config,
            self.snapshot_cache.clone(),
            self.session_worker.clone(),
        )?;
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
        target.restore(snapshot)?;
        require_completed_attempt(target, "parent reconstruction")
    }

    fn execution_work(&self, target: &FaultTarget) -> u64 {
        target.execution_ticks()
    }

    fn telemetry(&self, _target: &FaultTarget) -> TargetCounters {
        crate::consonance::thread_work_telemetry()
    }

    fn action_cost_fn(&self) -> fn(&FaultAction) -> u64 {
        action_cost
    }

    fn snapshot_retention()
    -> Option<Box<dyn searcher::search::archive::SnapshotRetention<FaultSnapshot>>> {
        Some(Box::new(crate::history::FaultHistoryRetention::default()))
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
        target.verify_integrity()?;
        require_completed_attempt(target, "imported prefix execution")?;
        merge_action_milestones(aggregate, target);
        Ok(())
    }

    fn apply_action_and_snapshot(
        &self,
        target: &mut FaultTarget,
        action: &FaultAction,
        aggregate: &mut FaultMilestones,
    ) -> Result<Option<FaultSnapshot>, Box<dyn Error>> {
        let captured = target.apply_and_snapshot(*action);
        target.verify_integrity()?;
        require_completed_attempt(target, "imported prefix execution")?;
        let captured = captured?;
        merge_action_milestones(aggregate, target);
        Ok(captured)
    }

    fn snapshot(&self, target: &mut FaultTarget) -> Result<FaultSnapshot, Box<dyn Error>> {
        target
            .snapshot()?
            .ok_or_else(|| "the fault endpoint has no successor to snapshot".into())
    }

    fn candidate_replay_certificate(
        &self,
        snapshot: &FaultSnapshot,
    ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
        if snapshot.failed || !snapshot.observation.stop.is_continuable() {
            return Err("fault replay candidate is not a runnable endpoint".into());
        }
        validate_snapshot_prefix_certificates(snapshot, self.config.uml().is_some())?;
        match (self.config.uml().is_some(), snapshot.endpoint) {
            (false, Some(FaultEndpointCertificate::KvmWholeState(hash))) => {
                Ok(Some(replay_endpoint_certificate(
                    hash,
                    snapshot
                        .event_history
                        .ok_or("KVM snapshot lacks ordered event history")?,
                    &snapshot.observation,
                    snapshot.failed,
                )?))
            }
            (true, Some(FaultEndpointCertificate::UmlWholeStateUnavailable)) => Ok(None),
            _ => Err("fault replay candidate lacks a certificate for its backend".into()),
        }
    }

    fn current_replay_certificate(
        &self,
        target: &FaultTarget,
    ) -> Result<Option<[u8; 32]>, Box<dyn Error>> {
        if target.failed() || !target.observation().stop.is_continuable() {
            return Err("fault replay action did not reach a runnable endpoint".into());
        }
        if self.config.uml().is_some() {
            return Ok(None);
        }
        Ok(Some(replay_endpoint_certificate(
            target.state_hash_current()?,
            target.event_history_current()?,
            target.observation(),
            target.failed(),
        )?))
    }

    fn execute_job_certified(
        &self,
        _run: &FaultCampaignRun,
        target: &mut FaultTarget,
        origin_snapshot: &FaultSnapshot,
        replay: &[FaultAction],
        replay_certificates: &[Option<[u8; 32]>],
        parent_milestones: FaultMilestones,
        suffix: &[FaultAction],
        _retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            origin_snapshot,
            replay,
            replay_certificates,
            parent_milestones,
            suffix,
            stop,
        )
    }

    fn execute_job(
        &self,
        _run: &FaultCampaignRun,
        target: &mut FaultTarget,
        origin_snapshot: &FaultSnapshot,
        replay: &[FaultAction],
        parent_milestones: FaultMilestones,
        suffix: &[FaultAction],
        _retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            origin_snapshot,
            replay,
            &vec![None; replay.len()],
            parent_milestones,
            suffix,
            stop,
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
        require_completed_attempt(target, "bootstrap or rollout evaluation")?;
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
        evidence.assertions.merge(&target.observation().assertions);
        evidence.root_actions = target.actions().to_vec();
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
        if action
            .observations
            .iter()
            .any(|observation| observation.watchdog_cutoff || observation.infrastructure_failure)
        {
            return Err(Box::new(AttemptIncomplete {
                phase: "action evidence",
                cause: if action
                    .observations
                    .iter()
                    .any(|observation| observation.infrastructure_failure)
                {
                    "host infrastructure failure"
                } else {
                    "guest progress watchdog cutoff"
                },
            }));
        }
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        if let FaultOperation::EventPark { edges, .. } = action.action.operation
            && !action.observations.is_empty()
        {
            evidence
                .park_thresholds
                .entry(park_threshold_bucket(u64::from(edges)))
                .or_default()
                .armed += 1;
        }
        for observation in &action.observations {
            evidence.assertions.merge(&observation.assertions);
            for park in &observation.parks {
                *evidence.park_sites.entry(park.site).or_default() += 1;
                evidence
                    .park_thresholds
                    .entry(park_threshold_bucket(park.edges))
                    .or_default()
                    .fired += 1;
            }
            for read in &observation.park_reads {
                *evidence.park_reads.entry(read.site).or_default() += 1;
            }
            evidence.note_undecided_keys(
                sequence,
                observation
                    .check
                    .as_ref()
                    .map_or(0, |check| check.undecided_keys),
            );
        }
        let bug = action
            .observations
            .iter()
            .find(|observation| observation.is_bug())
            .filter(|_| evidence.bugs.len() < MAX_RECORDED_BUGS);
        let champion = (milestone_key(action.milestones)
            > milestone_key(evidence.champion_milestones))
        .then_some(action.milestones);
        let failed = action.outcome.disposition.is_failed()
            && evidence.failures.len() < MAX_RECORDED_FAILURES;
        if bug.is_some() || champion.is_some() || failed {
            let mut input = input()?;
            input
                .actions
                .splice(0..0, evidence.root_actions.iter().copied());
            if failed {
                evidence.failures.push(FaultFailureRecord {
                    execution: sequence,
                    input: input.clone(),
                });
            }
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

    fn merge_hang_evidence<F>(
        &self,
        evidence: &mut FaultCampaignEvidence,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<FaultInput, Box<dyn Error>>,
    {
        if evidence.hangs.len() >= MAX_RECORDED_HANGS {
            return Ok(());
        }
        let mut input = input()?;
        input
            .actions
            .splice(0..0, evidence.root_actions.iter().copied());
        evidence.hangs.push(FaultHangRecord {
            execution: sequence,
            input,
        });
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
    run_fault_campaign_with_plan(game, config, origin, stream, progress, None, None)
}

pub fn run_fault_campaign_with_plan(
    game: &FaultWorkload,
    config: &FaultCampaignConfig,
    origin: &FaultCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    checkpoints: Option<searcher::search::checkpoint::CheckpointPlan>,
    interrupt: Option<&'static std::sync::atomic::AtomicBool>,
) -> Result<(FaultCampaignReport, FaultSnapshotCheckpoint), Box<dyn Error>> {
    let mut generic = config.generic();
    if let CampaignOrigin::SearchCheckpoint { path, .. } = origin {
        generic.window = searcher::search::checkpoint::read_header(path)?.window;
        generic.stop_campaign_on_objective = false;
    }
    let (report, snapshot) = run_campaign_checkpointed_with_options(
        game,
        &generic,
        origin,
        stream,
        progress,
        CampaignExecutionOptions {
            placement: config.placement.clone(),
            checkpoints,
            interrupt,
            ..CampaignExecutionOptions::default()
        },
    )?;
    Ok((FaultCampaignReport::new(report), snapshot))
}

#[cfg(test)]
pub(crate) fn fixture_workload(
    kernel: &[u8],
    initramfs: &[u8],
    config: &FaultConfig,
) -> FaultWorkload {
    let identity = if config.uml().is_some() {
        identity(kernel, initramfs, config).unwrap()
    } else {
        let root = config
            .root
            .as_ref()
            .map(|bytes| format!(";root-sha256={:x}", Sha256::digest(bytes.as_slice())))
            .unwrap_or_default();
        format!(
            "test-admitted-vm;kernel-sha256={:x};initramfs-sha256={:x};ram-mib={};cmdline-sha256={:x}{root}",
            Sha256::digest(kernel),
            Sha256::digest(initramfs),
            config.ram_mib,
            Sha256::digest(config.cmdline()),
        )
    };
    FaultWorkload {
        kernel: kernel.to_vec(),
        initramfs: initramfs.to_vec(),
        config: config.clone(),
        identity,
        root_seal: OnceLock::new(),
        snapshot_cache: None,
        session_worker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion::AssertionSet;
    use crate::consonance::{DEFAULT_RAM_MIB, GuestBackend};
    use searcher::search::draw_tables::{DEFAULT_DRAW_TABLE_PARAMETERS, DrawVersionSchedule};
    use searcher::search::{
        archive::{SelectorDraw, SelectorPath},
        campaign::{CampaignJobRecord, CampaignStreamRecord},
    };

    #[test]
    fn endpoint_certificate_distinguishes_state_and_unavailability() {
        let result = FaultCampaignJobResult {
            preparation_failure: None,
            hang: None,
            actions: Vec::new(),
        };
        let first = endpoint_result_sha256(
            &result,
            FaultEndpointCertificate::KvmWholeState([1; 32]),
            Some((0, [0; 32])),
        )
        .unwrap();
        let second = endpoint_result_sha256(
            &result,
            FaultEndpointCertificate::KvmWholeState([2; 32]),
            Some((0, [0; 32])),
        )
        .unwrap();
        let failed = endpoint_result_sha256(
            &result,
            FaultEndpointCertificate::KvmUnavailableAfterFailure,
            None,
        )
        .unwrap();
        let uml = endpoint_result_sha256(
            &result,
            FaultEndpointCertificate::UmlWholeStateUnavailable,
            None,
        )
        .unwrap();
        let different_history = endpoint_result_sha256(
            &result,
            FaultEndpointCertificate::KvmWholeState([1; 32]),
            Some((0, [9; 32])),
        )
        .unwrap();
        assert_ne!(first, different_history);
        assert!(
            endpoint_result_sha256(
                &result,
                FaultEndpointCertificate::KvmUnavailableAfterFailure,
                Some((0, [0; 32]))
            )
            .is_err()
        );
        assert_ne!(first, second);
        assert_ne!(first, failed);
        assert_ne!(first, uml);
        assert_ne!(failed, uml);
        assert_ne!(first, fault_result_sha256(&result).unwrap());
        assert!(
            endpoint_result_sha256(
                &result,
                FaultEndpointCertificate::KvmWholeState([1; 32]),
                None
            )
            .is_err()
        );
        assert!(
            endpoint_result_sha256(
                &result,
                FaultEndpointCertificate::UmlWholeStateUnavailable,
                Some((0, [0; 32]))
            )
            .is_err()
        );
    }

    #[test]
    fn final_runnable_candidate_reuses_the_same_kvm_hash_as_a_fresh_endpoint() {
        let action = FaultAction::new(
            FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()),
            std::num::NonZeroU16::new(1).unwrap(),
        );
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[action],
                &[Some(
                    replay_endpoint_certificate(
                        [17; 32],
                        (0, [0; 32]),
                        &FaultObservations::default(),
                        false,
                    )
                    .unwrap(),
                )],
                Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
                &(FaultObservations::default()),
            ),

            endpoint: Some(FaultEndpointCertificate::KvmWholeState([17; 32])),
            event_history: Some((0, [0; 32])),
            ..FaultSnapshot::default()
        };
        let result = FaultCampaignJobResult {
            preparation_failure: None,
            hang: None,
            actions: vec![FaultCampaignActionResult {
                action,
                observations: vec![FaultObservations::default()],
                milestones: FaultMilestones::default(),
                outcome: Outcome {
                    objective_reached: false,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: Some(CampaignCandidate {
                    key: archive_key(&FaultObservations::default()),
                    viable: true,
                    snapshot,
                }),
            }],
        };
        let mut fresh_calls = 0;
        let reused = kvm_result_endpoint(&result, &[action], &FaultObservations::default(), || {
            fresh_calls += 1;
            Ok([99; 32])
        })
        .unwrap();
        assert_eq!(fresh_calls, 0);
        let fresh = kvm_result_endpoint(
            &FaultCampaignJobResult {
                preparation_failure: None,
                hang: None,
                actions: Vec::new(),
            },
            &[action],
            &FaultObservations::default(),
            || {
                fresh_calls += 1;
                Ok([17; 32])
            },
        )
        .unwrap();
        assert_eq!(fresh_calls, 1);
        assert_eq!(reused, fresh);
        assert_eq!(
            endpoint_result_sha256(&result, reused, Some((0, [0; 32]))).unwrap(),
            endpoint_result_sha256(&result, fresh, Some((0, [0; 32]))).unwrap()
        );
        let mut malformed = result.clone();
        malformed.actions[0]
            .candidate
            .as_mut()
            .unwrap()
            .snapshot
            .endpoint = None;
        assert!(
            kvm_result_endpoint(&malformed, &[action], &FaultObservations::default(), || Ok(
                [17; 32]
            ))
            .is_err()
        );
        assert!(
            kvm_result_endpoint(&result, &[], &FaultObservations::default(), || Ok([17; 32]))
                .is_err()
        );
    }

    #[test]
    fn snapshot_identity_binds_prefix_certificates_and_rejects_missing_ones() {
        let action = FaultAction::new(
            FaultOperation::Wait(std::num::NonZeroU16::new(1).unwrap()),
            std::num::NonZeroU16::new(1).unwrap(),
        );
        let observation = FaultObservations::default();
        let endpoint_hash = [17; 32];
        let final_certificate =
            replay_endpoint_certificate(endpoint_hash, (0, [0; 32]), &observation, false).unwrap();
        let snapshot = FaultSnapshot {
            history: crate::history::FaultHistory::fixture(
                &[action, action],
                &[Some([11; 32]), Some(final_certificate)],
                Some(FaultEndpointCertificate::KvmWholeState(endpoint_hash)),
                &(observation.clone()),
            ),
            observation,
            failed: false,
            endpoint: Some(FaultEndpointCertificate::KvmWholeState(endpoint_hash)),
            event_history: Some((0, [0; 32])),
        };
        let game = game();
        let identity = game.snapshot_identity_sha256(&snapshot).unwrap();

        let mut changed_events = snapshot.clone();
        changed_events.event_history = Some((0, [19; 32]));
        assert_ne!(
            snapshot_digest(&changed_events).unwrap(),
            snapshot_digest(&snapshot).unwrap()
        );
        assert!(game.snapshot_identity_sha256(&changed_events).is_err());
        changed_events.event_history = None;
        assert!(game.snapshot_identity_sha256(&changed_events).is_err());

        let actions = snapshot.history.actions();
        let mut changed = snapshot.clone();
        changed.history = crate::history::FaultHistory::fixture(
            &actions,
            &[Some([23; 32]), Some(final_certificate)],
            snapshot.endpoint,
            &snapshot.observation,
        );
        let encoded = FaultSnapshotCheckpoint {
            format: game.checkpoint_format().into(),
            entries: vec![searcher::search::campaign::SnapshotCheckpointEntry {
                id: 1,
                snapshot: changed.clone(),
            }],
        }
        .to_bytes()
        .unwrap();
        let restored =
            FaultSnapshotCheckpoint::from_bytes(&encoded, game.checkpoint_format()).unwrap();
        assert_eq!(
            restored.entries[0].snapshot.observation,
            snapshot.observation
        );
        assert_eq!(restored.entries[0].snapshot, changed);
        assert_ne!(
            game.snapshot_identity_sha256(&restored.entries[0].snapshot)
                .unwrap(),
            identity
        );
        assert_ne!(game.snapshot_identity_sha256(&changed).unwrap(), identity);
        let mut wrong_final = snapshot.clone();
        wrong_final.history = crate::history::FaultHistory::fixture(
            &actions,
            &[Some([11; 32]), Some([23; 32])],
            snapshot.endpoint,
            &snapshot.observation,
        );
        assert!(game.snapshot_identity_sha256(&wrong_final).is_err());
        let mut malformed = serde_json::to_value(&snapshot.history).unwrap();
        malformed[1][1]["certificate"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<crate::history::FaultHistory>(malformed).is_err());
    }

    #[test]
    fn campaign_job_record_round_trips_assertion_digest() {
        #[derive(Serialize)]
        struct NativeAssertionSet {
            count: u32,
            digest: u128,
        }

        for digest in [0, (1_u128 << 127) | 7] {
            let key = FaultArchiveKey {
                sometimes: AssertionSet { count: 1, digest },
                ..FaultArchiveKey::default()
            };
            assert_eq!(
                postcard::to_allocvec(&key.sometimes).unwrap(),
                postcard::to_allocvec(&NativeAssertionSet { count: 1, digest }).unwrap()
            );
            let record = CampaignStreamRecord::<(), FaultArchiveKey>::Job(CampaignJobRecord {
                sequence: 1,
                reservation: 0,
                parent_id: 0,
                continuation_energy: None,
                mutation_seed: 1,
                execution_work: 1,
                result_sha256: "result".to_owned(),
                decisions: Vec::new(),
                mixture_weight: 0,
                splice_weight: 0,
                splice: None,
                selector: SelectorDraw {
                    path: SelectorPath::Tiers,
                    tier_rank: Some(0),
                    best_preference: None,
                },
                draw_checkpoint_before: None,
                draw_checkpoint_after: None,
                duration_draw: Some(DurationDraw {
                    context: key,
                    max_duration: NonZeroU64::MIN,
                    duration: NonZeroU64::MIN,
                }),
                duration_admission_sequence_at_draw: None,
                duration_remaining_work: None,
                duration_checkpoint_at_draw: None,
                duration_checkpoint_before: None,
                duration_checkpoint_after: None,
                suffix_limit: None,
            });
            let line = serde_json::to_string(&record).unwrap();
            assert!(line.contains(&format!("\"digest\":\"{digest}\"")));
            let decoded: CampaignStreamRecord<(), FaultArchiveKey> =
                serde_json::from_str(&line).unwrap();
            assert_eq!(decoded, record);
            if digest == 0 {
                for noncanonical in ["+0", "00"] {
                    let changed = line.replacen(
                        "\"digest\":\"0\"",
                        &format!("\"digest\":\"{noncanonical}\""),
                        1,
                    );
                    assert_ne!(changed, line);
                    assert!(
                        serde_json::from_str::<CampaignStreamRecord<(), FaultArchiveKey>>(&changed)
                            .is_err()
                    );
                }
            }
        }
    }

    fn config(knobs: &[&str]) -> FaultConfig {
        FaultConfig {
            knobs: knobs.iter().map(|knob| (*knob).to_owned()).collect(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Vm,
            root: None,
            boot_cache: None,
        }
    }

    fn game() -> FaultWorkload {
        fixture_workload(b"kernel", b"initramfs", &config(&[]))
    }

    fn run(nodes: u16, hooks: Vec<u32>) -> FaultCampaignRun {
        FaultCampaignRun {
            vocabulary: FaultVocabulary::new(nodes, hooks).expect("vocabulary"),
        }
    }

    #[test]
    fn campaign_integrity_preserves_cache_contradiction_as_a_typed_error() {
        use consonance_client::cache::{CacheError, LocalIndex, Namespace};

        let index = Arc::new(LocalIndex::new(1 << 20));
        let namespace = Namespace::new(&[b"campaign-test"]);
        let first = index
            .publish(
                namespace,
                b"action",
                None,
                index.extent(1).unwrap(),
                [1; 32],
                0,
            )
            .unwrap();
        let error = index
            .publish(
                namespace,
                b"action",
                None,
                index.extent(1).unwrap(),
                [2; 32],
                0,
            )
            .unwrap_err();
        assert!(matches!(error, CacheError::DivergentDuplicate { .. }));
        index.release(first);
        let game = game().with_snapshot_cache(Some(index.clone()));
        let error = game.verify_integrity().unwrap_err();
        drop(game);
        drop(index);
        let evidence = match error.downcast_ref::<CacheError>().unwrap() {
            CacheError::Quarantined { evidence, .. } => evidence,
            error => panic!("wrong integrity category: {error}"),
        };
        assert_eq!(evidence.existing.target_commitment, [1; 32]);
        assert_eq!(evidence.published.target_commitment, [2; 32]);
        assert_eq!(evidence.existing.chain[0].bytes().len(), 4096);
        assert_eq!(evidence.published.chain[0].bytes().len(), 4096);
    }

    #[test]
    fn park_feedback_shrinks_each_site_read_rate_toward_the_campaign_read_rate() {
        let evidence = FaultCampaignEvidence {
            park_sites: BTreeMap::from([(4, 1), (8, 19), (12, 80)]),
            park_reads: BTreeMap::from([(4, 1), (8, 6), (16, 0)]),
            ..FaultCampaignEvidence::default()
        };
        assert_eq!(
            park_feedback(&evidence),
            BTreeMap::from([(4, 158), (8, 236)])
        );
        let unlanded = FaultCampaignEvidence {
            park_reads: BTreeMap::from([(4, 1)]),
            ..FaultCampaignEvidence::default()
        };
        assert_eq!(park_feedback(&unlanded), BTreeMap::from([(4, 1126)]));
        assert!(park_feedback(&FaultCampaignEvidence::default()).is_empty());
    }

    #[test]
    fn hang_evidence_records_the_rooted_input_and_stops_at_its_bound() {
        let game = game();
        let root = FaultAction::from(FaultOperation::Kill(0, std::num::NonZeroU16::MIN));
        let attempted = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN));
        let mut evidence = FaultCampaignEvidence {
            root_actions: vec![root],
            ..FaultCampaignEvidence::default()
        };
        game.merge_hang_evidence(&mut evidence, 7, || {
            Ok(FaultInput {
                actions: vec![attempted],
            })
        })
        .unwrap();
        assert_eq!(
            evidence.hangs,
            vec![FaultHangRecord {
                execution: 7,
                input: FaultInput {
                    actions: vec![root, attempted],
                },
            }]
        );
        assert!(evidence.bugs.is_empty());
        for sequence in 8..u64::try_from(MAX_RECORDED_HANGS).unwrap() + 20 {
            game.merge_hang_evidence(&mut evidence, sequence, || Ok(FaultInput::default()))
                .unwrap();
        }
        assert_eq!(evidence.hangs.len(), MAX_RECORDED_HANGS);
        game.merge_hang_evidence(&mut evidence, 0, || Err("not materialized".into()))
            .unwrap();
    }

    #[test]
    fn a_failed_execution_records_its_rooted_input_up_to_its_bound() {
        let game = game();
        let root = FaultAction::from(FaultOperation::Kill(0, std::num::NonZeroU16::MIN));
        let failing = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN));
        let mut evidence = FaultCampaignEvidence {
            root_actions: vec![root],
            ..FaultCampaignEvidence::default()
        };
        let result = |disposition| FaultCampaignActionResult {
            action: failing,
            observations: Vec::new(),
            milestones: FaultMilestones::default(),
            outcome: Outcome {
                objective_reached: false,
                disposition,
            },
            candidate: None,
        };
        game.merge_action_evidence(
            &mut evidence,
            &result(ExecutionDisposition::Runnable),
            3,
            || Err("a runnable action needs no input".into()),
        )
        .unwrap();
        assert!(evidence.failures.is_empty());
        game.merge_action_evidence(
            &mut evidence,
            &result(ExecutionDisposition::Failed),
            4,
            || {
                Ok(FaultInput {
                    actions: vec![failing],
                })
            },
        )
        .unwrap();
        assert_eq!(
            evidence.failures,
            vec![FaultFailureRecord {
                execution: 4,
                input: FaultInput {
                    actions: vec![root, failing],
                },
            }]
        );
        assert!(evidence.bugs.is_empty());
        for sequence in 5..u64::try_from(MAX_RECORDED_FAILURES).unwrap() + 20 {
            game.merge_action_evidence(
                &mut evidence,
                &result(ExecutionDisposition::Failed),
                sequence,
                || Ok(FaultInput::default()),
            )
            .unwrap();
        }
        assert_eq!(evidence.failures.len(), MAX_RECORDED_FAILURES);
        game.merge_action_evidence(
            &mut evidence,
            &result(ExecutionDisposition::Failed),
            0,
            || Err("not materialized".into()),
        )
        .unwrap();
    }

    #[test]
    fn park_evidence_counts_parks_the_guest_ran_and_each_landing_and_read_once() {
        let game = game();
        let mut evidence = FaultCampaignEvidence::default();
        let park = FaultOperation::EventPark {
            node: 0,
            edges: 5,
            hold_us: 10_000,
            ticks: std::num::NonZeroU16::MIN,
            target: None,
        };
        let result = |observations| FaultCampaignActionResult {
            action: park.into(),
            observations,
            milestones: FaultMilestones::default(),
            outcome: Outcome {
                objective_reached: false,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
        };
        let landed = FaultObservations {
            parks: vec![crate::target::ParkLanding {
                moment: 1,
                site: 9,
                edges: 5,
            }],
            park_reads: vec![crate::target::ParkRead {
                moment: 2,
                site: 9,
                edges: 1,
            }],
            ..FaultObservations::default()
        };
        let cutoff = FaultObservations {
            watchdog_cutoff: true,
            ..FaultObservations::default()
        };
        for observations in [vec![landed], vec![]] {
            game.merge_action_evidence(&mut evidence, &result(observations), 0, || {
                Err("no input".into())
            })
            .unwrap();
        }
        let before = serde_json::to_vec(&evidence).unwrap();
        for incomplete in [
            cutoff,
            FaultObservations {
                infrastructure_failure: true,
                ..FaultObservations::default()
            },
        ] {
            let error = game
                .merge_action_evidence(&mut evidence, &result(vec![incomplete]), 0, || {
                    panic!("incomplete evidence requested a certified input")
                })
                .unwrap_err();
            assert!(error.to_string().contains("campaign incomplete"), "{error}");
            assert_eq!(serde_json::to_vec(&evidence).unwrap(), before);
        }
        assert_eq!(
            evidence.park_thresholds.get(&2),
            Some(&ParkThresholds { armed: 1, fired: 1 })
        );
        assert_eq!(evidence.park_sites.get(&9), Some(&1));
        assert_eq!(evidence.park_reads.get(&9), Some(&1));
    }

    #[test]
    fn undecided_keys_count_the_latest_check_of_each_execution() {
        let game = game();
        let mut evidence = FaultCampaignEvidence::default();
        let checked = |keys| FaultObservations {
            check: Some(crate::target::CheckEvidence {
                run: 1,
                undecided_keys: keys,
                ..crate::target::CheckEvidence::default()
            }),
            ..FaultObservations::default()
        };
        let result = |observations| FaultCampaignActionResult {
            action: FaultOperation::Wait(std::num::NonZeroU16::MIN).into(),
            observations,
            milestones: FaultMilestones::default(),
            outcome: Outcome {
                objective_reached: false,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
        };
        for (sequence, observations) in [
            (1, vec![checked(2)]),
            (1, vec![checked(3), checked(4)]),
            (2, vec![checked(0)]),
            (3, vec![checked(5)]),
            (3, vec![FaultObservations::default()]),
            (4, vec![checked(1)]),
        ] {
            game.merge_action_evidence(&mut evidence, &result(observations), sequence, || {
                Err("no input".into())
            })
            .unwrap();
        }
        assert_eq!(
            evidence.undecided_checks,
            UndecidedChecks {
                executions: 2,
                keys: 5,
            }
        );
    }

    #[test]
    fn search_reserves_one_job_per_worker() {
        for workers in [1, 3, 7] {
            let config = FaultCampaignConfig {
                campaign_seed: 1,
                vocabulary: FaultVocabulary::new(3, vec![]).unwrap(),
                workers,
                execution_budget: 10,
                host: "host".to_owned(),
                wall_budget: None,
                archive_entry_limit: 16,
                memory_budget_mib: None,
                materialize_final_artifacts: false,
                retention: RetentionPolicy::Unprobed,
                mixture: DrawMixture::AlphabetOnly,
                objective_witness_path: None,
                placement: None,
            };
            assert_eq!(config.generic().window, default_window(workers));
        }
    }

    #[test]
    fn wait_contexts_separate_lifecycle_states() {
        let game = game();
        let run = FaultCampaignRun {
            vocabulary: FaultVocabulary::new(3, vec![])
                .unwrap()
                .with_instrumented_events(true),
        };
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
        assert_eq!(
            game.duration_request(&run, parent, None)
                .unwrap()
                .max_duration
                .get(),
            ADAPTIVE_DURATION_MAX_TICKS
        );
    }

    #[test]
    fn recorded_duration_choices_reach_waits_and_event_holds() {
        let game = game();
        let run = FaultCampaignRun {
            vocabulary: FaultVocabulary::new(3, vec![])
                .unwrap()
                .with_instrumented_events(true),
        };
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let draw = DurationDraw {
            context: FaultArchiveKey {
                event_ready: 1,
                ..FaultArchiveKey::default()
            },
            max_duration: NonZeroU64::new(ADAPTIVE_DURATION_MAX_TICKS).unwrap(),
            duration: NonZeroU64::new(256).unwrap(),
        };
        let mut tables =
            DrawTables::<FaultAction>::new(DEFAULT_DRAW_TABLE_PARAMETERS).expect("draw tables");
        let checkpoint = tables.checkpoint().expect("initial checkpoint");
        let mut schedule = DrawVersionSchedule::default();
        schedule.require(checkpoint.records, 0);
        tables.remember_version(&schedule).expect("keep a version");
        let mut waits = 0;
        let mut event_parks = 0;
        for seed in 0..128 {
            let suffix = game
                .expand_suffix_duration(&run, &tables, mixture, seed, None, draw)
                .unwrap();
            let replay = game
                .expand_suffix_recorded_duration(
                    &run,
                    &tables,
                    mixture,
                    Some(&checkpoint),
                    seed,
                    None,
                    Some(draw),
                )
                .unwrap();
            assert_eq!(suffix, replay);
            for action in suffix {
                match action.operation {
                    FaultOperation::Wait(ticks) => {
                        assert_eq!(ticks.get(), 256);
                        assert_eq!(game.duration_of_action(&run, &action), Some(draw.duration));
                        waits += 1;
                    }
                    FaultOperation::EventPark { hold_us, ticks, .. } => {
                        assert_eq!(ticks.get(), 256);
                        assert!((10_000..=2_560_000).contains(&hold_us));
                        assert_eq!(game.duration_of_action(&run, &action), Some(draw.duration));
                        event_parks += 1;
                    }
                    _ => {}
                }
            }
        }
        assert!(waits > 0);
        assert!(event_parks > 0);
        assert!(
            game.expand_suffix_recorded_duration(
                &run,
                &tables,
                mixture,
                Some(&checkpoint),
                1,
                None,
                None
            )
            .is_err()
        );
        let out_of_range = DurationDraw {
            duration: NonZeroU64::new(65536).unwrap(),
            ..draw
        };
        assert!(
            game.expand_suffix_duration(&run, &tables, mixture, 1, None, out_of_range)
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
    fn recorded_policies_require_typed_host_incompleteness() {
        let game = game();
        let mut policies = game.policies(&run(1, vec![1, 2]));
        assert_eq!(
            policies.get(HOST_FAILURE_POLICY_FIELD).unwrap(),
            HOST_FAILURE_POLICY
        );
        policies.remove(HOST_FAILURE_POLICY_FIELD);
        assert!(game.resolve_recorded(&policies).is_err());
        policies.insert(
            HOST_FAILURE_POLICY_FIELD.to_owned(),
            "watchdog_incomplete_v1".into(),
        );
        assert!(game.resolve_recorded(&policies).is_err());
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
        let postgres = fixture_workload(b"kernel", b"postgres-initramfs", &config(&[]));
        assert_ne!(etcd.image_identity(), postgres.image_identity());
        assert_ne!(
            etcd.workload_identity_sha256(),
            postgres.workload_identity_sha256()
        );
    }

    #[test]
    fn a_uml_profile_separates_identity_and_formats_from_the_vm() {
        let uml = |identity: &str| {
            let mut config = config(&[]);
            config.backend = GuestBackend::Uml(Box::new(crate::consonance::UmlGuest {
                profile: "profile".into(),
                profile_identity_sha256: identity.to_owned(),
                host: uml::HostIdentity {
                    architecture: "x86_64".into(),
                    processor: 0,
                    cpu_model: identity.to_owned(),
                    cpu_features: String::new(),
                    boot_cpuinfo: String::new(),
                    auxv: uml::HostAuxv::default(),
                },
                identity: identity.to_owned(),
                work_parent: "work".into(),
            }));
            fixture_workload(b"kernel", b"initramfs", &config)
        };
        let vm = game();
        let first = uml("profile=a;cpu=x");
        let other_cpu = uml("profile=a;cpu=y");
        for (left, right) in [(&vm, &first), (&first, &other_cpu)] {
            assert_ne!(left.image_identity(), right.image_identity());
            assert_ne!(
                left.workload_identity_sha256(),
                right.workload_identity_sha256()
            );
        }
        assert_ne!(vm.stream_format(), first.stream_format());
        assert_ne!(vm.checkpoint_format(), first.checkpoint_format());
    }

    #[test]
    fn the_recorded_policies_pin_the_knobs_and_the_action_format() {
        let game = game();
        let policies = game.policies(&run(1, vec![1, 2]));
        let tuned = fixture_workload(b"kernel", b"initramfs", &config(&["faultlab.puts=20"]));
        assert!(tuned.resolve_recorded(&policies).is_err());
        let mut older = policies.clone();
        for legacy in ["500000000", "fault-action-duration-coverage-v4"] {
            older.insert(ACTION_FORMAT_FIELD.to_owned(), legacy.to_owned());
            assert!(game.resolve_recorded(&older).is_err());
        }
        assert_eq!(
            policies.get(ACTION_FORMAT_FIELD).map(String::as_str),
            Some(ACTION_FORMAT)
        );
    }
}
