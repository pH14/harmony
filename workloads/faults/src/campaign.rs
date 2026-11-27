// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use consonance_client::{cache::CacheIndex, session::WorkerLauncher};

use fault_policy::StandingWindow;
use searcher::{
    search::{
        archive::{KeyShape, RetentionPolicy},
        assertion::{AssertionKey, Compiled, StageFalls},
        campaign::{
            ArchiveFormat, ArchiveReportState, CampaignActionResult, CampaignCandidate,
            CampaignConfig, CampaignExecutionOptions, CampaignJobResult, CampaignModeReport,
            CampaignOrigin, CampaignTypes, Evaluation, InputPolicy, Reporting, SnapshotCheckpoint,
            TargetExecution, ThreadPlacement, WorkloadPolicies, default_window,
            postcard_value_sha256, run_campaign_checkpointed_with_options,
        },
        choice::{ChoicePlan, ChoiceRecord, ChoiceSite},
        cost::ShrinkCost,
        draw::{DrawMixture, MixtureDraw, draw_suffix},
        draw_tables::{DrawTables, biased_step},
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
        DURATION_IDENTIFIER, DrawProfile, FaultArchiveReport, FaultBugRecord, FaultFailureRecord,
        FaultHangRecord, FaultInput, FaultMilestones, FaultProgressWatermark, MAX_RECORDED_BUGS,
        MAX_RECORDED_FAILURES, MAX_RECORDED_HANGS, ParkAims, ParkThresholds,
        REPLACEMENT_IDENTIFIER, UndecidedChecks, action_cost, bug_outcome, merge_milestones,
        merge_progress_watermark, milestone_key, milestones, park_threshold_bucket, parked_pause,
        sample_action, sample_profiled_action,
    },
    assertion::Assertions,
    bundle::FaultVocabulary,
    consonance::{
        FaultConfig, FaultTarget, identity, replay_endpoint_certificate, root_standing,
        snapshot_memory_charge, validate_snapshot_prefix_certificates,
    },
    guest::{GUEST_SITE, GuestChoices},
    key::FaultKeys,
    shrink::{fault_cost, settle_tail, simpler_fault_actions},
    target::{
        ActionWindows, FaultAction, FaultEndpointCertificate, FaultObservations, FaultOperation,
        FaultSnapshot, held_aims_at,
    },
};

pub(crate) mod nodes;
pub(crate) mod settle;

pub const CAMPAIGN_STREAM_FORMAT: &str = "faultlab-consonance-campaign-stream-v16";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-consonance-snapshot-root-v12";
pub const UML_CAMPAIGN_STREAM_FORMAT: &str = "faultlab-uml-campaign-stream-v13";
pub const UML_SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-uml-snapshot-root-v8";
pub const TERMINAL_POLICY_IDENTIFIER: &str = "assertion_or_crash";
const ADAPTIVE_DURATION_MAX_TICKS: u64 = 1_024;

const VOCABULARY_FIELD: &str = "action_vocabulary";
const VIOLATIONS_FIELD: &str = "objective_violations";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const HOST_FAILURE_POLICY_FIELD: &str = "host_failure_policy";
const HOST_FAILURE_POLICY: &str = "host_incomplete_v3";
const IMAGE_FIELD: &str = "image";
const ACTION_FORMAT_FIELD: &str = "action_format";
const ACTION_FORMAT: &str = "fault-action-duration-coverage-v7-parked-pause";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FaultCampaignRun {
    pub vocabulary: FaultVocabulary,
    pub violations: BTreeSet<String>,
}

impl FaultCampaignRun {
    #[must_use]
    pub fn any_bug(vocabulary: FaultVocabulary) -> Self {
        Self {
            vocabulary,
            violations: BTreeSet::new(),
        }
    }

    #[must_use]
    pub fn reproduced_by(&self, observation: &FaultObservations) -> bool {
        self.violations.is_empty()
            || observation
                .violations()
                .iter()
                .any(|violation| self.violations.contains(violation))
    }
}

pub struct FaultWorkload {
    keys: Arc<FaultKeys>,
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: FaultConfig,
    identity: String,
    root_seal: OnceLock<u64>,
    root_windows: OnceLock<Result<Vec<StandingWindow>, String>>,
    snapshot_cache: Option<Arc<dyn CacheIndex>>,
    session_worker: Option<WorkerLauncher>,
}

impl FaultWorkload {
    fn root_windows(&self) -> Result<&[StandingWindow], Box<dyn Error>> {
        self.root_windows
            .get_or_init(|| {
                self.config
                    .root
                    .as_ref()
                    .map(|root| root_standing(root).map_err(|error| error.to_string()))
                    .transpose()
                    .map(Option::unwrap_or_default)
            })
            .as_deref()
            .map_err(|error| error.clone().into())
    }

    fn may_land_held(
        &self,
        evidence: &FaultCampaignEvidence,
        action: &FaultCampaignActionResult,
    ) -> Result<bool, Box<dyn Error>> {
        if !self.config.hold_hooks || self.root_seal().is_none() {
            return Ok(false);
        }
        let windows = self.root_windows()?;
        Ok(action
            .observations
            .iter()
            .flat_map(|observation| &observation.parks)
            .any(|park| {
                evidence.exact_aims.contains(&park.site)
                    || windows.iter().any(|open| {
                        matches!(
                            fault_policy::decode_process_target(&open.target),
                            Some((_, fault_policy::Fault::ProcEventPark {
                                target: Some(target),
                                ..
                            })) if target.end - target.start == 1 && target.start == park.site
                        )
                    })
            }))
    }

    fn held_aims(&self, actions: &[FaultAction]) -> Result<Vec<(u64, u64)>, Box<dyn Error>> {
        let windows = ActionWindows {
            root_seal: self.root_seal().ok_or("the root seal is not known")?,
            hold_hooks: true,
        };
        Ok(held_aims_at(windows, self.root_windows()?, actions)?)
    }

    fn materialize<F>(
        &self,
        evidence: &FaultCampaignEvidence,
        input: Option<F>,
    ) -> Result<FaultInput, Box<dyn Error>>
    where
        F: FnOnce() -> Result<FaultInput, Box<dyn Error>>,
    {
        let mut input = input.ok_or("action input was already materialized")?()?;
        input
            .actions
            .splice(0..0, evidence.root_actions.iter().copied());
        Ok(input)
    }

    pub fn new(
        kernel: &[u8],
        initramfs: &[u8],
        config: &FaultConfig,
        keys: FaultKeys,
    ) -> Result<Self, String> {
        Ok(Self {
            keys: Arc::new(keys),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            config: config.clone(),
            identity: identity(kernel, initramfs, config)?,
            root_seal: OnceLock::new(),
            root_windows: OnceLock::new(),
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

    #[must_use]
    pub fn keys(&self) -> &FaultKeys {
        &self.keys
    }

    pub fn key_of(&self, observations: &FaultObservations) -> Option<AssertionKey> {
        self.keys.key(observations)
    }

    pub fn fresh_target(&self) -> Result<FaultTarget, String> {
        FaultTarget::fresh(&self.kernel, &self.initramfs, &self.config)
            .map(|target| target.with_keys(Arc::clone(&self.keys)))
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
    #[serde(default)]
    park_aims: ParkAims,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    exact_aims: BTreeSet<u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    site_incidence: BTreeMap<u64, u64>,
    #[serde(default)]
    site_actions: u64,
    undecided_checks: UndecidedChecks,
    undecided_execution: Option<(u64, u64)>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    stage_falls: BTreeMap<u8, u64>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    parked_pauses: BTreeMap<String, u64>,
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

    fn stage_falls(&self, compiled: &Compiled) -> Vec<StageFalls> {
        compiled
            .stage_names()
            .into_iter()
            .zip(0_u8..)
            .map(|(stage, index)| StageFalls {
                stage: stage.to_owned(),
                records: self
                    .stage_falls
                    .get(&index)
                    .map_or(0, |&records| usize::try_from(records).unwrap_or(usize::MAX)),
            })
            .collect()
    }

    fn recordable_bug<'a>(
        &self,
        observations: &'a [FaultObservations],
    ) -> Option<&'a FaultObservations> {
        if self.bugs.len() < MAX_RECORDED_BUGS {
            return observations.iter().find(|observation| observation.is_bug());
        }
        let recorded: BTreeSet<String> = self
            .bugs
            .iter()
            .flat_map(|bug| bug.observations.violations())
            .collect();
        observations.iter().find(|observation| {
            observation.is_bug() && !observation.violations().is_subset(&recorded)
        })
    }
}

pub type FaultCampaignOrigin = CampaignOrigin<FaultWorkload>;
pub type FaultSnapshotCheckpoint = SnapshotCheckpoint<FaultSnapshot>;
pub type FaultCampaignModeReport = CampaignModeReport<FaultAction, FaultArchiveReport>;
type FaultCampaignActionResult = CampaignActionResult<FaultWorkload>;
type FaultCampaignJobResult = CampaignJobResult<FaultWorkload>;

fn snapshot_digest(snapshot: &FaultSnapshot) -> Result<String, Box<dyn Error>> {
    postcard_value_sha256(&(
        "fault-snapshot-identity-v3",
        snapshot.history.receipt().root(),
        &snapshot.observation,
        snapshot.failed,
        snapshot.endpoint,
        snapshot
            .event_history
            .map(|history| (control_proto::EventHistory::VERSION, history)),
        &snapshot.accumulator,
        (
            snapshot.observed.key,
            snapshot.observed.rejected,
            snapshot.observed.goal,
        ),
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
    expected: Option<AssertionKey>,
    fresh_hash: impl FnOnce() -> Result<[u8; 32], String>,
) -> Result<FaultEndpointCertificate, Box<dyn Error>> {
    if let Some(last) = result.actions.last()
        && matches!(last.outcome.disposition, ExecutionDisposition::Runnable)
        && let Some(candidate) = last.candidate.as_ref()
    {
        validate_snapshot_prefix_certificates(&candidate.snapshot, false)?;
        if !candidate.viable
            || candidate.snapshot.failed
            || !candidate.snapshot.history.matches_actions(actions)
            || &candidate.snapshot.observation != observation
            || Some(candidate.key) != expected
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
            run: FaultCampaignRun::any_bug(self.vocabulary.clone()),
            mixture: self.mixture,
            retention: self.retention,
            objective_witness_path: self.objective_witness_path.clone(),
            minimize: None,
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

fn outcome(run: &FaultCampaignRun, target: &FaultTarget) -> Outcome {
    let objective_reached =
        !target.failed() && target.found_bug() && run.reproduced_by(target.observation());
    let disposition = disposition(target);
    let doomed = !disposition.is_failed() && target.rejected() && !objective_reached;
    Outcome {
        doomed,
        objective_reached,
        disposition: if doomed {
            ExecutionDisposition::Terminal
        } else {
            disposition
        },
    }
}

fn disposition(target: &FaultTarget) -> ExecutionDisposition {
    if target.failed() {
        ExecutionDisposition::Failed
    } else if target.observation().stop.is_continuable() {
        ExecutionDisposition::Runnable
    } else {
        ExecutionDisposition::Terminal
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
    let mut snapshot = snapshot.ok_or("cannot search from a terminal point; rewind further")?;
    target.start_accumulator();
    snapshot.accumulator = target.accumulator().clone();
    snapshot.observed = target.observed();
    Ok(snapshot)
}

fn guest_suffix(
    suffix: &[FaultAction],
    plan: Option<&ChoicePlan>,
    mut untried: Option<u32>,
) -> Vec<FaultAction> {
    let Some(plan) = plan else {
        return suffix.to_vec();
    };
    suffix
        .iter()
        .enumerate()
        .map(|(index, action)| {
            if action.guest.is_some() {
                return *action;
            }
            action.with_guest(GuestChoices::from_plan(plan, index, untried.take()))
        })
        .collect()
}

struct JobSteps {
    aggregate: FaultMilestones,
    objective_seen: bool,
    results: Vec<CampaignActionResult<FaultWorkload>>,
}

impl JobSteps {
    fn record(
        &mut self,
        run: &FaultCampaignRun,
        target: &FaultTarget,
        action: FaultAction,
        candidate: impl FnOnce(
            &FaultTarget,
            ExecutionDisposition,
        ) -> Option<CampaignCandidate<FaultWorkload>>,
    ) -> Outcome {
        let raw = outcome(run, target);
        if !raw.doomed {
            merge_action_milestones(&mut self.aggregate, target);
        }
        let outcome = Outcome {
            doomed: raw.doomed,
            objective_reached: raw.objective_reached && !self.objective_seen,
            disposition: raw.disposition,
        };
        self.objective_seen |= raw.objective_reached;
        self.results.push(CampaignActionResult {
            action,
            observations: target.last_action_observations().to_vec(),
            milestones: self.aggregate,
            outcome,
            candidate: candidate(target, outcome.disposition),
            cost: fault_cost(&action),
            bound: ShrinkCost::ZERO,
        });
        outcome
    }

    fn attempted(&self, more: &[FaultAction]) -> Vec<FaultAction> {
        self.results
            .iter()
            .map(|result| result.action)
            .chain(more.iter().copied())
            .collect()
    }
}

fn apply_settled(
    run: &FaultCampaignRun,
    target: &mut FaultTarget,
    steps: &mut JobSteps,
    settled: &[FaultAction],
    stop: &searcher::search::rollout::RolloutStop,
) -> Result<Option<Vec<FaultAction>>, Box<dyn Error>> {
    for action in settled {
        target.apply(*action);
        target.verify_integrity()?;
        if job_hung(target, "settle execution")? {
            return Ok(Some(steps.attempted(std::slice::from_ref(action))));
        }
        if steps
            .record(run, target, *action, |_, _| None)
            .should_stop(stop)
        {
            break;
        }
    }
    if steps.objective_seen || stop.cancelled() {
        Ok(None)
    } else {
        Err(
            "the settle phase's violation did not recur when its actions were applied to the job"
                .into(),
        )
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn execute_job(
    run: &FaultCampaignRun,
    target: &mut FaultTarget,
    origin_snapshot: &FaultSnapshot,
    replay: &[FaultAction],
    replay_certificates: &[Option<[u8; 32]>],
    parent_milestones: FaultMilestones,
    suffix: &[FaultAction],
    stop: searcher::search::rollout::RolloutStop,
    settle_policy: Option<&settle::SettlePolicy>,
) -> Result<FaultCampaignJobResult, Box<dyn Error>> {
    if replay.len() != replay_certificates.len() {
        return Err("fault replay certificate count does not match its action path".into());
    }
    let _cancel = crate::consonance::JobCancel::install(stop.cancellation().cloned());
    let mut settle_start = settle::JobStart {
        work: target.execution_ticks(),
        fault_free_checks_finished: 0,
    };
    target.restore(origin_snapshot)?;
    if job_hung(target, "parent reconstruction")? {
        if let Some(policy) = settle_policy {
            settle::skipped(target, policy, settle_start, 0, settle::Skip::Hung(None));
        }
        return Ok(hung_job(&[]));
    }
    for (action, certificate) in replay.iter().zip(replay_certificates) {
        if outcome(run, target).disposition.is_terminal() {
            return Err("fault replay stopped before its archived parent endpoint".into());
        }
        crate::job_observation::tail(false);
        let checked = target.apply_replayed_certified(*action, *certificate);
        target.verify_integrity()?;
        if job_hung(target, "parent replay")? {
            if let Some(policy) = settle_policy {
                settle::skipped(
                    target,
                    policy,
                    settle_start,
                    0,
                    settle::Skip::Hung(Some(*action)),
                );
            }
            return Ok(hung_job(&[]));
        }
        checked?;
        crate::job_observation::tail(true);
    }
    settle_start.fault_free_checks_finished = target.observation().fault_free_checks_finished;
    let parent_outcome = outcome(run, target);
    if parent_outcome.disposition.is_terminal() {
        return Err("fault parent reconstruction stopped before its archived endpoint".into());
    }
    let mut steps = JobSteps {
        aggregate: parent_milestones,
        objective_seen: parent_outcome.objective_reached,
        results: Vec::with_capacity(suffix.len()),
    };
    let depth = target.actions().len() as u64;
    let (host_untried, guest_untried) = match stop.choices() {
        Some(plan) if plan.untried_site == nodes::NODE_SITE_ID => (plan.untried, None),
        Some(plan) => (None, plan.untried),
        None => (None, None),
    };
    let mut resolved = nodes::resolve(&run.vocabulary, suffix, stop.choices(), host_untried, depth);
    let suffix = guest_suffix(
        &ready_parked_pauses(
            &resolved.suffix,
            target.observation().event_ready,
            run.vocabulary.nodes(),
        ),
        stop.choices(),
        guest_untried,
    );
    for (index, action) in suffix.iter().enumerate() {
        if let Some(mut row) = resolved.rows[index].take() {
            row.moment = target.observation().moment;
            nodes::record(depth, resolved.untried.filter(|_| index == 0), &row);
        }
        let captured = target.apply_and_snapshot(*action);
        target.verify_integrity()?;
        if job_hung(target, "suffix execution")? {
            if let Some(policy) = settle_policy {
                settle::skipped(
                    target,
                    policy,
                    settle_start,
                    index + 1,
                    settle::Skip::Hung(Some(*action)),
                );
            }
            return Ok(hung_job(&suffix[..=index]));
        }
        let captured = captured?;
        let outcome = steps.record(run, target, *action, |target, disposition| match captured {
            Some(snapshot) if matches!(disposition, ExecutionDisposition::Runnable) => {
                target.observed().key.map(|key| CampaignCandidate {
                    key,
                    viable: true,
                    snapshot,
                })
            }
            _ => None,
        });
        if outcome.should_stop(&stop) {
            break;
        }
    }
    if let Some(policy) = settle_policy {
        let suffix_actions = steps.results.len();
        let skip = if steps.objective_seen {
            Some(settle::Skip::Finding)
        } else if !matches!(
            outcome(run, target).disposition,
            ExecutionDisposition::Runnable
        ) {
            Some(settle::Skip::Terminal)
        } else if stop.cancelled() {
            Some(settle::Skip::Cancelled)
        } else {
            None
        };
        match skip {
            Some(skip) => settle::skipped(target, policy, settle_start, suffix_actions, skip),
            None => {
                let settled = settle::settle_execution(
                    run,
                    target,
                    &stop,
                    policy,
                    settle_start,
                    suffix_actions,
                )?;
                if let Some(settled) = settled
                    && let Some(attempted) =
                        apply_settled(run, target, &mut steps, &settled, &stop)?
                {
                    return Ok(hung_job(&attempted));
                }
            }
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        hang: None,
        actions: steps.results,
    })
}

impl CampaignTypes for FaultWorkload {
    type JobObservation = crate::job_observation::FaultJobObservation;
    type Target = FaultTarget;
    type Action = FaultAction;
    type Key = AssertionKey;
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

    fn catalog_signals(&self, _run: &FaultCampaignRun) -> Vec<String> {
        self.keys
            .catalog()
            .signals()
            .iter()
            .map(|signal| signal.name.clone())
            .collect()
    }

    fn state_record(
        &self,
        _run: &FaultCampaignRun,
        snapshot: &FaultSnapshot,
    ) -> Option<searcher::search::assertion::Record> {
        Some(self.keys.record(&snapshot.observation))
    }

    fn observation_record(
        &self,
        _run: &FaultCampaignRun,
        observation: &FaultObservations,
    ) -> Option<searcher::search::assertion::Record> {
        Some(self.keys.record(observation))
    }

    fn choice_records(
        &self,
        run: &FaultCampaignRun,
        plan: Option<&ChoicePlan>,
        index: usize,
        action: &FaultAction,
        _observations: &[FaultObservations],
    ) -> Vec<ChoiceRecord> {
        plan.zip(action.guest)
            .filter(|(plan, guest)| guest.seed == GuestChoices::from_plan(plan, index, None).seed)
            .and_then(|_| nodes::row(&run.vocabulary, 0, 0, &action.operation))
            .map(|row| ChoiceRecord::new(nodes::NODE_SITE_ID, row.offered, row.default, row.taken))
            .into_iter()
            .collect()
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
            kvm_result_endpoint(
                result,
                target.actions(),
                target.observation(),
                target.observed().key,
                || target.state_hash_current(),
            )?
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
            format: ArchiveFormat::default(),
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
            park_aims: evidence.park_aims.clone(),
            site_incidence: evidence.site_incidence.clone(),
            site_actions: evidence.site_actions,
            undecided_checks: evidence.undecided_checks,
            stage_falls: evidence.stage_falls(self.keys.compiled()),
            parked_pauses: evidence.parked_pauses.clone(),
        }
    }
}

impl InputPolicy for FaultWorkload {
    fn simpler_actions(&self, _run: &FaultCampaignRun, action: &FaultAction) -> Vec<FaultAction> {
        simpler_fault_actions(action)
    }
    fn choice_sites(&self, run: &FaultCampaignRun) -> Vec<ChoiceSite> {
        let mut sites = vec![ChoiceSite::new(GUEST_SITE, Vec::new())];
        if run.vocabulary.untried_first_nodes() {
            sites.push(nodes::site(&run.vocabulary));
        }
        sites
    }
    fn untried_site(&self, run: &FaultCampaignRun, suffix: &[FaultAction]) -> u16 {
        if suffix
            .first()
            .is_some_and(|action| nodes::asks(&run.vocabulary, action))
        {
            nodes::NODE_SITE_ID
        } else {
            0
        }
    }
    fn goal_tail(&self, _run: &FaultCampaignRun, last: Option<&FaultAction>) -> Vec<FaultAction> {
        settle_tail(last)
    }
    fn policies(&self, run: &FaultCampaignRun) -> WorkloadPolicies {
        [
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, TERMINAL_POLICY_IDENTIFIER),
            (HOST_FAILURE_POLICY_FIELD, HOST_FAILURE_POLICY),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .chain([
            (
                KEY_POLICY_FIELD.to_owned(),
                self.keys.compiled().policy_identifier().to_owned(),
            ),
            (IMAGE_FIELD.to_owned(), self.identity.clone()),
            (ACTION_FORMAT_FIELD.to_owned(), ACTION_FORMAT.to_owned()),
            (VOCABULARY_FIELD.to_owned(), run.vocabulary.identifier()),
            (
                VIOLATIONS_FIELD.to_owned(),
                serde_json::to_string(&run.violations).unwrap_or_default(),
            ),
        ])
        .collect()
    }

    fn resolve_recorded(
        &self,
        policies: &WorkloadPolicies,
    ) -> Result<FaultCampaignRun, Box<dyn Error>> {
        let run = FaultCampaignRun {
            vocabulary: FaultVocabulary::from_identifier(recorded(policies, VOCABULARY_FIELD)?)?,
            violations: serde_json::from_str(recorded(policies, VIOLATIONS_FIELD)?)?,
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
        parent: AssertionKey,
        remaining_work: Option<NonZeroU64>,
    ) -> Option<DurationRequest<AssertionKey>> {
        Some(DurationRequest {
            context: self.keys.context(parent),
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
        draw: Option<DurationDraw<AssertionKey>>,
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
        draw: DurationDraw<AssertionKey>,
        replay: bool,
    ) -> Result<Vec<FaultAction>, Box<dyn Error>> {
        held_suffix(
            run,
            state,
            mixture,
            before,
            mutation_seed,
            replay,
            DurationDraw {
                context: self.keys.context_event_ready(draw.context),
                max_duration: draw.max_duration,
                duration: draw.duration,
            },
        )
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

const PARK_FEEDBACK_PRIOR_INCIDENCE: u128 = 1;
const SITE_INCIDENCE_LIMIT: usize = 1 << 18;

fn park_feedback(evidence: &FaultCampaignEvidence) -> BTreeMap<u64, u64> {
    let mut weights = park_read_feedback(evidence);
    for (site, weight) in park_incidence_feedback(evidence) {
        let entry = weights.entry(site).or_default();
        *entry = entry.saturating_add(weight);
    }
    weights
}

fn note_site_incidence(site_incidence: &mut BTreeMap<u64, u64>, site: u64) {
    if let Some(count) = site_incidence.get_mut(&site) {
        *count += 1;
    } else if site_incidence.len() < SITE_INCIDENCE_LIMIT {
        site_incidence.insert(site, 1);
    }
}

fn park_incidence_feedback(evidence: &FaultCampaignEvidence) -> BTreeMap<u64, u64> {
    let actions = u128::from(evidence.site_actions).saturating_add(PARK_FEEDBACK_PRIOR_INCIDENCE);
    evidence
        .site_incidence
        .iter()
        .map(|(site, incidence)| {
            let landings = u128::from(evidence.park_sites.get(site).copied().unwrap_or(0));
            let numerator = PARK_FEEDBACK_SCALE
                .saturating_mul(actions)
                .saturating_mul(PARK_FEEDBACK_PRIOR_LANDINGS);
            let denominator = u128::from(*incidence)
                .saturating_add(PARK_FEEDBACK_PRIOR_INCIDENCE)
                .saturating_mul(landings.saturating_add(PARK_FEEDBACK_PRIOR_LANDINGS));
            let weight = u64::try_from(numerator / denominator).unwrap_or(u64::MAX);
            (*site, weight.max(1))
        })
        .collect()
}

fn park_read_feedback(evidence: &FaultCampaignEvidence) -> BTreeMap<u64, u64> {
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
        FaultOperation::EventKill { node, .. } | FaultOperation::EventPark { node, .. } => {
            node_is_ready(*node, event_ready, 0)
        }
        _ => true,
    }
}

fn node_is_ready(node: u16, event_ready: u64, nodes: u16) -> bool {
    if node == process_proto::ROLE_NODE {
        let every = u64::MAX.checked_shr(64 - u32::from(nodes)).unwrap_or(0);
        return every != 0 && event_ready & every == every;
    }
    1_u64
        .checked_shl(u32::from(node))
        .is_some_and(|bit| event_ready & bit != 0)
}

fn unparked_unless_ready(
    operation: FaultOperation,
    event_ready: u64,
    nodes: u16,
) -> FaultOperation {
    match operation {
        FaultOperation::PauseParked { node, ticks, .. }
            if !node_is_ready(node, event_ready, nodes) =>
        {
            FaultOperation::Pause(node, ticks)
        }
        operation => operation,
    }
}

fn ready_parked_pauses(suffix: &[FaultAction], event_ready: u64, nodes: u16) -> Vec<FaultAction> {
    suffix
        .iter()
        .map(|action| FaultAction {
            operation: unparked_unless_ready(action.operation, event_ready, nodes),
            ..*action
        })
        .collect()
}

const PROFILE_SEED_MIX: u64 = 0x5157_4152_4d5f_6b31;

const RESUME_PARK_SEED_MIX: u64 = 0x7265_7375_6d65_7061;

fn with_parked_pauses(
    suffix: Vec<FaultAction>,
    sites: &[u64],
    event_ready: u64,
    nodes: u16,
    ticks: std::num::NonZeroU16,
    mutation_seed: u64,
) -> Result<Vec<FaultAction>, Box<dyn Error>> {
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed ^ RESUME_PARK_SEED_MIX);
    suffix
        .into_iter()
        .map(|action| {
            let operation = match action.operation {
                FaultOperation::Pause(node, _) if node_is_ready(node, event_ready, nodes) => {
                    parked_pause(&mut rand, node, sites, ticks)?
                }
                operation => unparked_unless_ready(operation, event_ready, nodes),
            };
            Ok(FaultAction {
                operation,
                ..action
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn held_suffix(
    run: &FaultCampaignRun,
    state: &DrawTables<FaultAction>,
    mixture: MixtureDraw,
    before: Option<&EmpiricalStepCheckpoint>,
    mutation_seed: u64,
    replay: bool,
    draw: DurationDraw<u64>,
) -> Result<Vec<FaultAction>, Box<dyn Error>> {
    let ticks = std::num::NonZeroU16::new(u16::try_from(draw.duration.get())?)
        .ok_or("action duration must be positive")?;
    let event_ready = draw.context;
    let profile = DrawProfile::draw(
        &mut RomuDuoJrRand::with_seed(mutation_seed ^ PROFILE_SEED_MIX),
        &run.vocabulary,
    )?;
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
    if let Some(sites) = run.vocabulary.resume_park() {
        suffix = with_parked_pauses(
            suffix,
            sites,
            event_ready,
            run.vocabulary.nodes(),
            ticks,
            mutation_seed,
        )?;
    }
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
        )?
        .with_keys(Arc::clone(&self.keys));
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

    fn parent_rebuild_runs(
        &self,
        _target: &FaultTarget,
    ) -> Option<searcher::search::telemetry::HostRuns> {
        Some(crate::consonance::thread_parent_rebuild_runs())
    }

    fn planned_cost_fn(&self) -> fn(&FaultAction) -> u64 {
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
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        replay_certificates: &[Option<[u8; 32]>],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        _retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        execute_job(
            run,
            target,
            origin_snapshot,
            replay,
            replay_certificates,
            parent_milestones,
            suffix,
            stop,
            settle::policy()?,
        )
    }

    fn execute_job(
        &self,
        run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        _retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        execute_job(
            run,
            target,
            origin_snapshot,
            replay,
            &vec![None; replay.len()],
            parent_milestones,
            suffix,
            stop,
            settle::policy()?,
        )
    }
}

impl Evaluation for FaultWorkload {
    fn shrink_cost(
        &self,
        action: &FaultAction,
        _target: &FaultTarget,
        _observations: &[FaultObservations],
    ) -> ShrinkCost {
        fault_cost(action)
    }
    fn execution_disposition(&self, target: &FaultTarget) -> ExecutionDisposition {
        disposition(target)
    }

    fn objective_reached(
        &self,
        run: &FaultCampaignRun,
        target: &FaultTarget,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(self.rollout_outcome(run, target)?.objective_reached)
    }

    fn rollout_outcome(
        &self,
        run: &FaultCampaignRun,
        target: &FaultTarget,
    ) -> Result<Outcome, Box<dyn Error>> {
        require_completed_attempt(target, "bootstrap or rollout evaluation")?;
        Ok(outcome(run, target))
    }

    fn key_shape(&self) -> KeyShape {
        self.keys.compiled().shape()
    }

    fn current_key(&self, target: &FaultTarget) -> Result<AssertionKey, Box<dyn Error>> {
        target
            .observed()
            .key
            .ok_or_else(|| "the faults strategy gives this state no key".into())
    }

    fn complete_candidate_key(
        &self,
        key: AssertionKey,
        _snapshot: &FaultSnapshot,
    ) -> Result<AssertionKey, Box<dyn Error>> {
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
        if self.config.hold_hooks {
            evidence
                .exact_aims
                .extend(
                    evidence
                        .root_actions
                        .iter()
                        .filter_map(|action| match action.operation {
                            FaultOperation::EventPark {
                                target: Some(target),
                                ..
                            } if target.end - target.start == 1 => Some(target.start),
                            _ => None,
                        }),
                );
        }
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
        if !action.outcome.doomed {
            merge_progress_watermark(&mut evidence.watermark, &action.observations);
            merge_milestones(&mut evidence.aggregate, action.milestones);
        }
        let ran = !action.observations.is_empty();
        if let FaultOperation::EventPark { edges, .. } = action.action.operation
            && ran
        {
            evidence
                .park_thresholds
                .entry(park_threshold_bucket(u64::from(edges)))
                .or_default()
                .armed += 1;
        }
        let aimed = ran
            && matches!(
                action.action.operation,
                FaultOperation::EventPark {
                    target: Some(_),
                    ..
                }
            );
        if aimed {
            evidence.park_aims.armed += 1;
        }
        if self.config.hold_hooks
            && ran
            && let FaultOperation::EventPark {
                target: Some(target),
                ..
            } = action.action.operation
            && target.end - target.start == 1
        {
            evidence.exact_aims.insert(target.start);
        }
        let mut input = Some(input);
        let mut prefix = None;
        let held = if ran && self.may_land_held(evidence, action)? {
            let materialized = self.materialize(evidence, input.take())?;
            let held = self.held_aims(&materialized.actions)?;
            prefix = Some(materialized);
            held
        } else {
            Vec::new()
        };
        let parked = matches!(action.action.operation, FaultOperation::PauseParked { .. });
        if ran {
            evidence.site_actions += 1;
            let crossed = action
                .observations
                .iter()
                .flat_map(|observation| observation.edge_sites.iter().map(|entry| entry.site))
                .collect::<BTreeSet<_>>();
            for site in crossed {
                note_site_incidence(&mut evidence.site_incidence, site);
            }
        }
        for observation in &action.observations {
            evidence.assertions.merge(&observation.assertions);
            for park in &observation.parks {
                *evidence.park_sites.entry(park.site).or_default() += 1;
                let later = held
                    .iter()
                    .any(|&(site, end)| site == park.site && park.moment < end);
                if aimed || later {
                    evidence.park_aims.landings += 1;
                    *evidence.park_aims.sites.entry(park.site).or_default() += 1;
                }
                if later {
                    evidence.park_aims.held_landings += 1;
                }
                if !parked {
                    evidence
                        .park_thresholds
                        .entry(park_threshold_bucket(park.edges))
                        .or_default()
                        .fired += 1;
                }
            }
            for read in &observation.park_reads {
                *evidence.park_reads.entry(read.site).or_default() += 1;
            }
            for pause in &observation.parked_pauses {
                *evidence
                    .parked_pauses
                    .entry(pause.outcome.name().to_owned())
                    .or_default() += 1;
            }
            if let Some(stage) = observation.stage_fall {
                *evidence.stage_falls.entry(stage).or_default() += 1;
            }
            evidence.note_undecided_keys(
                sequence,
                observation
                    .check
                    .as_ref()
                    .map_or(0, |check| check.undecided_keys),
            );
        }
        let bug = evidence.recordable_bug(&action.observations);
        let champion = (!action.outcome.doomed
            && milestone_key(action.milestones) > milestone_key(evidence.champion_milestones))
        .then_some(action.milestones);
        let failed = action.outcome.disposition.is_failed()
            && evidence.failures.len() < MAX_RECORDED_FAILURES;
        if bug.is_some() || champion.is_some() || failed {
            let input = match prefix {
                Some(input) => input,
                None => self.materialize(evidence, input.take())?,
            };
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

pub fn run_fault_campaign_with_plan(
    game: &FaultWorkload,
    config: &FaultCampaignConfig,
    origin: &FaultCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
    options: CampaignExecutionOptions,
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
            ..options
        },
    )?;
    Ok((FaultCampaignReport::new(report), snapshot))
}

#[cfg(test)]
pub(crate) fn fixture_run() -> FaultCampaignRun {
    FaultCampaignRun::any_bug(FaultVocabulary::new(1, vec![1]).expect("fixture vocabulary"))
}

#[cfg(test)]
pub(crate) fn fixture_keys() -> FaultKeys {
    FaultKeys::new(crate::key::STRATEGY, 3).expect("the shipped strategy compiles")
}

#[cfg(test)]
impl FaultWorkload {
    pub(crate) fn with_fixture_keys(mut self, keys: Arc<FaultKeys>) -> Self {
        self.keys = keys;
        self
    }
}

#[cfg(test)]
pub(crate) fn fixture_key(observations: &FaultObservations) -> Option<AssertionKey> {
    static KEYS: OnceLock<FaultKeys> = OnceLock::new();
    KEYS.get_or_init(fixture_keys).key(observations)
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
        keys: Arc::new(fixture_keys()),
        kernel: kernel.to_vec(),
        initramfs: initramfs.to_vec(),
        config: config.clone(),
        identity,
        root_seal: OnceLock::new(),
        root_windows: OnceLock::new(),
        snapshot_cache: None,
        session_worker: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consonance::{DEFAULT_RAM_MIB, GuestBackend};
    use crate::target::{ActionWindows, standing_windows};
    use searcher::search::draw_tables::{DEFAULT_DRAW_TABLE_PARAMETERS, DrawVersionSchedule};

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
    fn a_hang_after_recorded_actions_reports_each_attempted_action_once() {
        let wait = |ticks| {
            FaultAction::new(
                FaultOperation::Wait(std::num::NonZeroU16::new(ticks).unwrap()),
                std::num::NonZeroU16::MIN,
            )
        };
        let result = |action| FaultCampaignActionResult {
            action,
            observations: Vec::new(),
            milestones: FaultMilestones::default(),
            outcome: Outcome {
                doomed: false,
                objective_reached: false,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
            cost: ShrinkCost::ZERO,
            bound: ShrinkCost::ZERO,
        };
        let steps = JobSteps {
            aggregate: FaultMilestones::default(),
            objective_seen: false,
            results: vec![result(wait(1)), result(wait(2))],
        };
        assert_eq!(
            steps.attempted(std::slice::from_ref(&wait(3))),
            vec![wait(1), wait(2), wait(3)]
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
                    doomed: false,
                    objective_reached: false,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: Some(CampaignCandidate {
                    key: fixture_key(&FaultObservations::default()).unwrap(),
                    viable: true,
                    snapshot,
                }),
                cost: ShrinkCost::ZERO,
                bound: ShrinkCost::ZERO,
            }],
        };
        let mut fresh_calls = 0;
        let reused = kvm_result_endpoint(
            &result,
            &[action],
            &FaultObservations::default(),
            fixture_key(&FaultObservations::default()),
            || {
                fresh_calls += 1;
                Ok([99; 32])
            },
        )
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
            None,
            || {
                fresh_calls += 1;
                Ok([17; 32])
            },
        )
        .unwrap();
        assert_eq!(fresh_calls, 1);
        assert_eq!(reused, fresh);
        let mut settled = result.clone();
        settled.actions[0].candidate = None;
        let settled_endpoint = kvm_result_endpoint(
            &settled,
            &[action],
            &FaultObservations::default(),
            fixture_key(&FaultObservations::default()),
            || {
                fresh_calls += 1;
                Ok([17; 32])
            },
        )
        .unwrap();
        assert_eq!(fresh_calls, 2);
        assert_eq!(settled_endpoint, fresh);
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
            kvm_result_endpoint(
                &malformed,
                &[action],
                &FaultObservations::default(),
                fixture_key(&FaultObservations::default()),
                || Ok([17; 32])
            )
            .is_err()
        );
        assert!(
            kvm_result_endpoint(
                &result,
                &[],
                &FaultObservations::default(),
                fixture_key(&FaultObservations::default()),
                || Ok([17; 32])
            )
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
            ..Default::default()
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
    fn a_snapshot_checkpoint_from_the_previous_snapshot_layout_is_refused_by_its_format() {
        let encoded = FaultSnapshotCheckpoint {
            format: "faultlab-consonance-snapshot-root-v11".into(),
            entries: vec![searcher::search::campaign::SnapshotCheckpointEntry {
                id: 1,
                snapshot: FaultSnapshot::default(),
            }],
        }
        .to_bytes()
        .unwrap();
        let error = FaultSnapshotCheckpoint::from_bytes(&encoded, SNAPSHOT_CHECKPOINT_FORMAT)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "snapshot checkpoint format is faultlab-consonance-snapshot-root-v11; this build reads faultlab-consonance-snapshot-root-v12"
            ),
            "{error}"
        );
    }

    fn config(knobs: &[&str]) -> FaultConfig {
        FaultConfig {
            knobs: knobs.iter().map(|knob| (*knob).to_owned()).collect(),
            ram_mib: DEFAULT_RAM_MIB,
            backend: GuestBackend::Vm,
            root: None,
            boot_cache: None,
            answers: Vec::new(),
            hold_hooks: false,
        }
    }

    fn game() -> FaultWorkload {
        fixture_workload(b"kernel", b"initramfs", &config(&[]))
    }

    fn run(nodes: u16, hooks: Vec<u32>) -> FaultCampaignRun {
        FaultCampaignRun::any_bug(FaultVocabulary::new(nodes, hooks).expect("vocabulary"))
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
    fn park_feedback_puts_a_site_crossed_once_in_a_thousand_actions_above_one_crossed_by_all() {
        let mut evidence = FaultCampaignEvidence {
            site_actions: 1000,
            site_incidence: BTreeMap::from([(1, 1), (2, 1000), (3, 1)]),
            park_sites: BTreeMap::from([(3, 90)]),
            ..FaultCampaignEvidence::default()
        };
        let weights = park_feedback(&evidence);
        assert_eq!(
            weights,
            BTreeMap::from([(1, 512_512), (2, 1024), (3, 51_251)])
        );
        assert!(weights[&1] > weights[&3] && weights[&3] > weights[&2]);
        evidence.park_reads = BTreeMap::from([(1, 1), (4, 1)]);
        let reads = park_read_feedback(&evidence);
        let combined = park_feedback(&evidence);
        assert_eq!(combined[&1], reads[&1] + 512_512);
        assert_eq!(combined[&4], reads[&4]);
        assert_eq!(combined[&2], 1024);
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
                doomed: false,
                objective_reached: false,
                disposition,
            },
            candidate: None,
            cost: ShrinkCost::ZERO,
            bound: ShrinkCost::ZERO,
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
    fn past_the_bug_bound_only_a_finding_with_a_new_violated_property_is_recorded() {
        let game = game();
        let action = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN));
        let violating = |properties: &[&str]| FaultObservations {
            assertions: crate::assertion::Assertions(
                properties
                    .iter()
                    .map(|property| {
                        (
                            (*property).to_owned(),
                            crate::assertion::AssertionOutcome {
                                kind: crate::assertion::AssertionKind::Always,
                                message: (*property).to_owned(),
                                location: String::new(),
                                passed: false,
                                failed: true,
                                holds: None,
                                failure_details: None,
                            },
                        )
                    })
                    .collect(),
            ),
            ..FaultObservations::default()
        };
        let result = |observations| FaultCampaignActionResult {
            action,
            observations,
            milestones: FaultMilestones::default(),
            outcome: Outcome {
                doomed: false,
                objective_reached: true,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
            cost: ShrinkCost::ZERO,
            bound: ShrinkCost::ZERO,
        };
        let mut evidence = FaultCampaignEvidence::default();
        for sequence in 0..u64::try_from(MAX_RECORDED_BUGS).unwrap() + 3 {
            game.merge_action_evidence(
                &mut evidence,
                &result(vec![violating(&["first"])]),
                sequence,
                || Ok(FaultInput::default()),
            )
            .unwrap();
        }
        assert_eq!(evidence.bugs.len(), MAX_RECORDED_BUGS);
        let input = FaultInput {
            actions: vec![action, action],
        };
        game.merge_action_evidence(
            &mut evidence,
            &result(vec![violating(&["first"]), violating(&["first", "second"])]),
            100,
            || Ok(input.clone()),
        )
        .unwrap();
        assert_eq!(evidence.bugs.len(), MAX_RECORDED_BUGS + 1);
        let recorded = evidence.bugs.last().unwrap();
        assert_eq!(recorded.execution, 100);
        assert_eq!(recorded.input, input);
        assert_eq!(
            recorded.observations.violations(),
            BTreeSet::from(["first".to_owned(), "second".to_owned()])
        );
        game.merge_action_evidence(
            &mut evidence,
            &result(vec![violating(&["second"])]),
            101,
            || panic!("a finding with no new property requested its input"),
        )
        .unwrap();
        assert_eq!(evidence.bugs.len(), MAX_RECORDED_BUGS + 1);
    }

    #[test]
    fn site_incidence_admits_no_new_site_once_full_and_keeps_counting_admitted_ones() {
        let mut incidence = (0..SITE_INCIDENCE_LIMIT as u64)
            .map(|site| (site, 1))
            .collect::<BTreeMap<_, _>>();
        note_site_incidence(&mut incidence, SITE_INCIDENCE_LIMIT as u64);
        note_site_incidence(&mut incidence, 7);
        assert_eq!(incidence.len(), SITE_INCIDENCE_LIMIT);
        assert_eq!(incidence.get(&(SITE_INCIDENCE_LIMIT as u64)), None);
        assert_eq!(incidence.get(&7), Some(&2));
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
                doomed: false,
                objective_reached: false,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
            cost: ShrinkCost::ZERO,
            bound: ShrinkCost::ZERO,
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
            parked_pauses: vec![
                crate::target::ParkedPause {
                    moment: 3,
                    node: 0,
                    outcome: process_proto::events::ParkedOutcome::Fired,
                },
                crate::target::ParkedPause {
                    moment: 4,
                    node: 1,
                    outcome: process_proto::events::ParkedOutcome::Unadopted,
                },
            ],
            edge_sites: vec![
                crate::target::EdgeSite {
                    moment: 1,
                    site: 40,
                },
                crate::target::EdgeSite {
                    moment: 2,
                    site: 41,
                },
            ],
            ..FaultObservations::default()
        };
        let crossed_again = FaultObservations {
            edge_sites: vec![crate::target::EdgeSite {
                moment: 5,
                site: 40,
            }],
            ..FaultObservations::default()
        };
        let cutoff = FaultObservations {
            watchdog_cutoff: true,
            ..FaultObservations::default()
        };
        assert!(
            serde_json::to_value(&evidence)
                .unwrap()
                .get("parked_pauses")
                .is_none()
        );
        for observations in [vec![landed, crossed_again], vec![]] {
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
        assert_eq!(evidence.site_actions, 1);
        assert_eq!(evidence.site_incidence, BTreeMap::from([(40, 1), (41, 1)]));
        assert_eq!(
            evidence.parked_pauses,
            BTreeMap::from([("fired".to_owned(), 1), ("unadopted".to_owned(), 1)])
        );
    }

    #[test]
    fn park_aims_count_only_parks_drawn_with_a_target() {
        let game = game();
        let mut evidence = FaultCampaignEvidence::default();
        let park = |target| FaultOperation::EventPark {
            node: 0,
            edges: 1,
            hold_us: 10_000,
            ticks: std::num::NonZeroU16::MIN,
            target,
        };
        let landing = |site| FaultObservations {
            parks: vec![crate::target::ParkLanding {
                moment: 1,
                site,
                edges: 1,
            }],
            ..FaultObservations::default()
        };
        let aimed = fault_policy::ParkTarget::new(0x40, 0x41);
        for (operation, observations) in [
            (park(aimed), vec![landing(0x40)]),
            (park(aimed), vec![FaultObservations::default()]),
            (park(None), vec![landing(0x80)]),
            (park(aimed), vec![]),
        ] {
            let result = FaultCampaignActionResult {
                action: operation.into(),
                observations,
                milestones: FaultMilestones::default(),
                outcome: Outcome {
                    doomed: false,
                    objective_reached: false,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: None,
                cost: ShrinkCost::ZERO,
                bound: ShrinkCost::ZERO,
            };
            game.merge_action_evidence(&mut evidence, &result, 0, || Err("no input".into()))
                .unwrap();
        }
        assert_eq!(
            evidence.park_aims,
            ParkAims {
                armed: 2,
                landings: 1,
                held_landings: 0,
                sites: BTreeMap::from([(0x40, 1)]),
            }
        );
        assert_eq!(
            evidence.park_thresholds.get(&0),
            Some(&ParkThresholds { armed: 3, fired: 2 })
        );
        assert_eq!(evidence.park_sites, BTreeMap::from([(0x40, 1), (0x80, 1)]));
    }

    fn held_game() -> FaultWorkload {
        let game = fixture_workload(
            b"kernel",
            b"initramfs",
            &FaultConfig {
                hold_hooks: true,
                ..config(&[])
            },
        );
        game.root_seal.set(1_000).unwrap();
        game
    }

    fn held_aim(node: u16, site: u64, ticks: u16, bits: u64) -> FaultAction {
        FaultAction::from(FaultOperation::EventPark {
            node,
            edges: 1,
            hold_us: 10_000,
            ticks: std::num::NonZeroU16::new(ticks).unwrap(),
            target: fault_policy::ParkTarget::new(site, site + 1),
        })
        .with_choice(bits << 61)
    }

    fn merge_held(
        game: &FaultWorkload,
        steps: Vec<(FaultAction, Vec<(u64, u64)>)>,
    ) -> FaultCampaignEvidence {
        let mut evidence = FaultCampaignEvidence::default();
        let mut prefix = Vec::new();
        let mut start = 1_000;
        for (action, landings) in steps {
            prefix.push(action);
            let actions = prefix.clone();
            let observations = vec![FaultObservations {
                parks: landings
                    .into_iter()
                    .map(|(offset, site)| crate::target::ParkLanding {
                        moment: start + offset,
                        site,
                        edges: 1,
                    })
                    .collect(),
                ..FaultObservations::default()
            }];
            start += action.ticks() * crate::target::SUPERVISOR_TICK_NANOS;
            let result = FaultCampaignActionResult {
                action,
                observations,
                milestones: FaultMilestones::default(),
                outcome: Outcome {
                    doomed: false,
                    objective_reached: false,
                    disposition: ExecutionDisposition::Runnable,
                },
                candidate: None,
                cost: ShrinkCost::ZERO,
                bound: ShrinkCost::ZERO,
            };
            game.merge_action_evidence(&mut evidence, &result, 0, || Ok(FaultInput { actions }))
                .unwrap();
        }
        evidence
    }

    #[test]
    fn a_held_aim_landing_in_a_later_action_counts_as_an_aim_landing() {
        let ticks = std::num::NonZeroU16::new(4).unwrap();
        let wait = FaultAction::from(FaultOperation::Wait(ticks));
        let kill = FaultAction::from(FaultOperation::Kill(0, ticks));
        let evidence = merge_held(
            &held_game(),
            vec![
                (held_aim(0, 0x40, 4, 7), vec![]),
                (wait, vec![(1, 0x40)]),
                (wait, vec![(1, 0x80)]),
                (held_aim(1, 0x50, 4, 0), vec![(1, 0x40)]),
                (kill, vec![]),
                (wait, vec![(1, 0x40)]),
            ],
        );
        assert_eq!(
            evidence.park_aims,
            ParkAims {
                armed: 2,
                landings: 2,
                held_landings: 2,
                sites: BTreeMap::from([(0x40, 2)]),
            }
        );
        assert_eq!(evidence.park_sites, BTreeMap::from([(0x40, 3), (0x80, 1)]));
    }

    #[test]
    fn a_landing_after_a_held_aim_expires_mid_action_is_not_counted() {
        let tick = crate::target::SUPERVISOR_TICK_NANOS;
        let wait = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::new(8).unwrap()));
        let evidence = merge_held(
            &held_game(),
            vec![
                (held_aim(0, 0x40, 4, 1), vec![]),
                (wait, vec![(2 * tick, 0x40), (6 * tick, 0x40)]),
            ],
        );
        assert_eq!(evidence.park_aims.held_landings, 1);
        assert_eq!(evidence.park_aims.landings, 1);
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
                doomed: false,
                objective_reached: false,
                disposition: ExecutionDisposition::Runnable,
            },
            candidate: None,
            cost: ShrinkCost::ZERO,
            bound: ShrinkCost::ZERO,
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
    fn a_run_with_violations_is_reproduced_only_by_one_of_them() {
        let violated = |id: &str| {
            let mut assertions = crate::assertion::Assertions::default();
            assertions.record(
                id.to_owned(),
                crate::assertion::AssertionOutcome {
                    kind: crate::assertion::AssertionKind::Always,
                    message: id.to_owned(),
                    location: String::new(),
                    passed: false,
                    failed: true,
                    holds: None,
                    failure_details: None,
                },
            );
            FaultObservations {
                assertions,
                ..FaultObservations::default()
            }
        };
        let any = run(3, vec![]);
        assert!(any.reproduced_by(&violated("lost write")));
        let finding = FaultCampaignRun {
            violations: BTreeSet::from(["lost write".to_owned(), "stale read".to_owned()]),
            ..any
        };
        assert!(finding.reproduced_by(&violated("stale read")));
        assert!(!finding.reproduced_by(&violated("node ended")));
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
        let run = FaultCampaignRun::any_bug(
            FaultVocabulary::new(3, vec![])
                .unwrap()
                .with_instrumented_events(true),
        );
        let key = |alive| {
            game.key_of(&FaultObservations {
                alive,
                ..FaultObservations::default()
            })
            .unwrap()
        };
        let parent = key(7);
        let request = game.duration_request(&run, parent, None).unwrap();
        let down = key(3);
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
        let run = FaultCampaignRun::any_bug(
            FaultVocabulary::new(3, vec![])
                .unwrap()
                .with_instrumented_events(true),
        );
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let draw = DurationDraw {
            context: game.keys().context(
                game.key_of(&FaultObservations {
                    event_ready: 1,
                    ..FaultObservations::default()
                })
                .unwrap(),
            ),
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
    fn a_ready_pause_draws_as_a_parked_pause_only_under_resume_park() {
        let game = game();
        let vocabulary = FaultVocabulary::new(3, vec![])
            .unwrap()
            .with_instrumented_events(true);
        let off = FaultCampaignRun::any_bug(vocabulary.clone());
        let anywhere = FaultCampaignRun::any_bug(
            vocabulary
                .clone()
                .with_resume_park(Some(Vec::new()))
                .unwrap(),
        );
        let sited =
            FaultCampaignRun::any_bug(vocabulary.with_resume_park(Some(vec![228_994])).unwrap());
        let mixture = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let ready = |event_ready| DurationDraw {
            context: game.keys().context(
                game.key_of(&FaultObservations {
                    event_ready,
                    ..FaultObservations::default()
                })
                .unwrap(),
            ),
            max_duration: NonZeroU64::new(ADAPTIVE_DURATION_MAX_TICKS).unwrap(),
            duration: NonZeroU64::new(256).unwrap(),
        };
        let draw = ready(0b111);
        let mut tables =
            DrawTables::<FaultAction>::new(DEFAULT_DRAW_TABLE_PARAMETERS).expect("draw tables");
        let checkpoint = tables.checkpoint().expect("initial checkpoint");
        let mut schedule = DrawVersionSchedule::default();
        schedule.require(checkpoint.records, 0);
        tables.remember_version(&schedule).expect("keep a version");
        let windows = ActionWindows {
            root_seal: 0,
            hold_hooks: false,
        };
        let mut pauses = 0;
        let mut limited = 0;
        let mut exponents = BTreeSet::new();
        for seed in 0..256 {
            let today = game
                .expand_suffix_duration(&off, &tables, mixture, seed, None, draw)
                .unwrap();
            assert!(
                today
                    .iter()
                    .all(|action| !matches!(action.operation, FaultOperation::PauseParked { .. }))
            );
            for (run, site) in [(&anywhere, None), (&sited, Some(228_994))] {
                let parked = game
                    .expand_suffix_duration(run, &tables, mixture, seed, None, draw)
                    .unwrap();
                assert_eq!(parked.len(), today.len());
                for (drawn, action) in today.iter().zip(&parked) {
                    let FaultOperation::Pause(node, _) = drawn.operation else {
                        assert_eq!(action, drawn);
                        continue;
                    };
                    pauses += 1;
                    let FaultOperation::PauseParked {
                        node: parked_node,
                        edges,
                        hold_us,
                        ticks,
                        target,
                    } = action.operation
                    else {
                        panic!("a ready pause draws as a parked pause");
                    };
                    assert_eq!(
                        FaultAction {
                            operation: drawn.operation,
                            ..*action
                        },
                        *drawn
                    );
                    assert_eq!(parked_node, node);
                    assert_eq!(ticks.get(), 256);
                    assert!((10_000..=2_560_000).contains(&hold_us));
                    match site {
                        Some(site) => {
                            assert_eq!(edges, 1);
                            assert_eq!(target, fault_policy::ParkTarget::new(site, site + 1));
                        }
                        None => {
                            assert!(edges.is_power_of_two() && edges <= 1024);
                            assert_eq!(target, None);
                            exponents.insert(edges.trailing_zeros());
                        }
                    }
                }
                if matches!(parked[0].operation, FaultOperation::PauseParked { .. }) {
                    limited += 1;
                    let standing = standing_windows(windows, &[], &parked[..1]).unwrap();
                    assert_eq!(standing.len(), 2);
                    assert!(matches!(
                        fault_policy::decode_process_target(&standing[0].target),
                        Some((_, fault_policy::Fault::ProcPause(_)))
                    ));
                    assert!(matches!(
                        fault_policy::decode_process_target(&standing[1].target),
                        Some((
                            _,
                            fault_policy::Fault::ProcEventPark {
                                at_resume: true,
                                ..
                            }
                        ))
                    ));
                    assert_eq!(standing[0].end, standing[1].start);
                }
                let replay = game
                    .expand_suffix_recorded_duration(
                        run,
                        &tables,
                        mixture,
                        Some(&checkpoint),
                        seed,
                        None,
                        Some(draw),
                    )
                    .unwrap();
                assert_eq!(replay, parked);
            }
        }
        assert!(pauses > 0);
        assert!(limited > 0);
        assert_eq!(exponents, (0..=10).collect::<BTreeSet<_>>());
        for seed in 0..32 {
            assert_eq!(
                game.expand_suffix_duration(&anywhere, &tables, mixture, seed, None, ready(0))
                    .unwrap(),
                game.expand_suffix_duration(&off, &tables, mixture, seed, None, ready(0))
                    .unwrap()
            );
        }
    }

    #[test]
    fn a_retained_or_executed_parked_pause_keeps_its_park_only_on_a_ready_node() {
        let ticks = std::num::NonZeroU16::new(256).unwrap();
        let parked = |node| {
            FaultAction::from(FaultOperation::PauseParked {
                node,
                edges: 4,
                hold_us: 10_000,
                ticks,
                target: None,
            })
            .with_choice(9)
        };
        let wait = FaultAction::from(FaultOperation::Wait(ticks));
        let suffix = vec![parked(0), wait, parked(1)];
        assert_eq!(
            with_parked_pauses(suffix.clone(), &[], 0b011, 2, ticks, 7).unwrap(),
            suffix
        );
        let unready_first = vec![
            FaultAction::from(FaultOperation::Pause(0, ticks)).with_choice(9),
            wait,
            parked(1),
        ];
        assert_eq!(
            with_parked_pauses(suffix.clone(), &[], 0b010, 2, ticks, 7).unwrap(),
            unready_first
        );
        assert_eq!(ready_parked_pauses(&suffix, 0b011, 2), suffix);
        assert_eq!(ready_parked_pauses(&suffix, 0b010, 2), unready_first);
        let role = vec![parked(process_proto::ROLE_NODE)];
        assert_eq!(ready_parked_pauses(&role, 0b111, 3), role);
        assert_eq!(
            ready_parked_pauses(&role, 0b011, 3),
            [
                FaultAction::from(FaultOperation::Pause(process_proto::ROLE_NODE, ticks))
                    .with_choice(9)
            ]
        );
        let drawn = with_parked_pauses(
            vec![FaultAction::from(FaultOperation::Pause(
                process_proto::ROLE_NODE,
                ticks,
            ))],
            &[],
            0b111,
            3,
            ticks,
            7,
        )
        .unwrap();
        assert!(matches!(
            drawn[..],
            [FaultAction {
                operation: FaultOperation::PauseParked {
                    node: process_proto::ROLE_NODE,
                    ..
                },
                ..
            }]
        ));
    }

    #[test]
    fn default_draws_match_their_recorded_digests() {
        use sha2::{Digest, Sha256};
        const DIGESTS: [&str; 36] = [
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "a80acebf03ab88908fab4344286ddd84d57b2593473ad488921c059658dc52d2",
            "ad57e5ffcd5ed7fd943eba313b6d503705607480a67b975580eb3e9b2b847168",
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "9b209cfc9a715251524b9db65d4a304c7f8e90e1b4544f28c4fd67b121bba396",
            "be708b25e5216ec6c754f496c5648d825812b53119c2cd9af46ba32b638dd753",
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "a80acebf03ab88908fab4344286ddd84d57b2593473ad488921c059658dc52d2",
            "ad57e5ffcd5ed7fd943eba313b6d503705607480a67b975580eb3e9b2b847168",
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "d949bdc09f664f7599c67c6833d9d20d3e13ab6bdcaade568f3a243dd016ea94",
            "905f3184a2d72a5665368471a058593ad8a6230d19e8ef2d5f35e4684be73306",
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "a80acebf03ab88908fab4344286ddd84d57b2593473ad488921c059658dc52d2",
            "ad57e5ffcd5ed7fd943eba313b6d503705607480a67b975580eb3e9b2b847168",
            "8298c2c8d07413710397f2eea761a5458603a3e3be5845d3fa259df3899a5fbb",
            "9b209cfc9a715251524b9db65d4a304c7f8e90e1b4544f28c4fd67b121bba396",
            "be708b25e5216ec6c754f496c5648d825812b53119c2cd9af46ba32b638dd753",
            "11bbfb83a9e710346a3a27a01098937eff45600c84398d59915405bb34c158b8",
            "355da7fe7dacd4ad485d397fdd35fb1292b244b6bc14b8dc13ce83663359e512",
            "a3c1ce73004e1bbc8f45d4003e33fecc8f38a846ff65ba7d23441ae5ec94dd4b",
            "11bbfb83a9e710346a3a27a01098937eff45600c84398d59915405bb34c158b8",
            "40743fba2644319ce6db0d276e1f6f6c23692d2c771cca208ac624491a9d3fbe",
            "c1114d6506347002ce4ec2bade7cf68f7f1a791d8a8b854b1d39b8df2ee527c3",
            "7d4b2200879e87aed23379062c6f4229bba23ea3a0d3790a35436c9d6eba4e70",
            "becae5cdea55cdc80cf873b722cf4cb97a1ff869b4df183c01476895432a96e1",
            "c832e1fd2fc4d1f79b92514e2ead1bdecdffbb130c2270e48f424a9ace64f581",
            "7d4b2200879e87aed23379062c6f4229bba23ea3a0d3790a35436c9d6eba4e70",
            "dd37ce131dffa509919605e2aa3a91e947552f26616a8a219f9dea3c9cc61104",
            "b0ef66c59c160cdbb177d1c800d07ffcfd10db4445a90c1ce7e159b10514bd35",
            "35b8b9ec83bbdfb9319e071c9aa8b0042770095ac6a413fd2364bf1a7d91648b",
            "27fdb01b9ac3e35a0c97acb52c11ce4590173e6f2ca956897267bec68becdec7",
            "0e4d4ca5a28c91ffd2b83057e3b51c059592c5d287238384ad9fe6dcc8620fe2",
            "35b8b9ec83bbdfb9319e071c9aa8b0042770095ac6a413fd2364bf1a7d91648b",
            "3d89b8b48ddde5b273e45d5bcb1b84d6a037399508e5b1908a8da3bc8466911a",
            "f1f39b8efc5a84290815f397ccd14c48d744bd1b577d070d798a6e7958ab00df",
        ];
        let game = game();
        let plain = FaultVocabulary::new(3, vec![]).unwrap();
        let instrumented = plain.clone().with_instrumented_events(true);
        let hooked = FaultVocabulary::new(3, vec![1, 2])
            .unwrap()
            .with_instrumented_events(true);
        let draw = |event_ready| DurationDraw {
            context: game.keys().context(
                game.key_of(&FaultObservations {
                    event_ready,
                    ..FaultObservations::default()
                })
                .unwrap(),
            ),
            max_duration: NonZeroU64::new(ADAPTIVE_DURATION_MAX_TICKS).unwrap(),
            duration: NonZeroU64::new(256).unwrap(),
        };
        let alphabet = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        let empty = DrawTables::<FaultAction>::new(DEFAULT_DRAW_TABLE_PARAMETERS).unwrap();
        let mut folded = DrawTables::<FaultAction>::new(DEFAULT_DRAW_TABLE_PARAMETERS).unwrap();
        let source = FaultCampaignRun::any_bug(instrumented.clone());
        for seed in 1000..1016 {
            let suffix = game
                .expand_suffix_duration(&source, &empty, alphabet, seed, None, draw(0b111))
                .unwrap();
            folded.fold_source(&suffix).unwrap();
        }
        folded.flush().unwrap();
        let mixtures = [
            alphabet,
            MixtureDraw {
                mixture: DrawMixture::BiasedHalf,
                weight: 0,
                splice_weight: 0,
            },
            MixtureDraw {
                mixture: DrawMixture::Energy { scale: 4 },
                weight: 200,
                splice_weight: 0,
            },
        ];
        let mut digests = Vec::new();
        for vocabulary in [plain, instrumented, hooked] {
            let run = FaultCampaignRun::any_bug(vocabulary);
            for ready in [0_u64, 0b111] {
                for tables in [&empty, &folded] {
                    for mixture in mixtures {
                        let mut digest = Sha256::new();
                        for seed in 0..64 {
                            let suffix = game
                                .expand_suffix_duration(
                                    &run,
                                    tables,
                                    mixture,
                                    seed,
                                    None,
                                    draw(ready),
                                )
                                .unwrap();
                            assert_eq!(suffix.len(), 64);
                            digest.update(serde_json::to_string(&suffix).unwrap().as_bytes());
                            digest.update(b"\n");
                        }
                        digests.push(format!("{:x}", digest.finalize()));
                    }
                }
            }
        }
        assert_eq!(digests, DIGESTS);
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
    fn the_stream_pins_the_objective_violations() {
        let game = game();
        let any = run(1, vec![1, 2]);
        let named = FaultCampaignRun {
            violations: BTreeSet::from(["lost write".to_owned()]),
            ..any.clone()
        };
        let policies = game.policies(&named);
        assert_ne!(policies, game.policies(&any));
        assert_eq!(game.resolve_recorded(&policies).expect("resolve"), named);
        let mut missing = policies;
        missing.remove(VIOLATIONS_FIELD);
        assert!(game.resolve_recorded(&missing).is_err());
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
        for legacy in [
            "500000000",
            "fault-action-duration-coverage-v4",
            "fault-action-duration-coverage-v6-guest-choices",
        ] {
            older.insert(ACTION_FORMAT_FIELD.to_owned(), legacy.to_owned());
            assert!(game.resolve_recorded(&older).is_err());
        }
        assert_eq!(
            policies.get(ACTION_FORMAT_FIELD).map(String::as_str),
            Some(ACTION_FORMAT)
        );
    }

    #[test]
    fn a_job_s_plan_stamps_its_fresh_actions_and_leaves_copied_ones() {
        use searcher::search::choice::{CERTAIN, ChoicePolicy, ChoiceProfile, ChoiceProfiles};
        let policy = ChoicePolicy {
            profiles: ChoiceProfiles::Fixed(ChoiceProfile::new(CERTAIN / 2, 0)),
            untried_first: true,
        };
        let plan = ChoicePlan::new(
            &policy,
            ChoiceProfile::default(),
            7,
            Some(2),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(plan.untried, Some(2));
        let wait = FaultAction::from(FaultOperation::Wait(std::num::NonZeroU16::MIN));
        let copied = wait.with_choice(5).with_guest(GuestChoices {
            contrary: 1,
            hold: 2,
            seed: 3,
            untried: Some(0),
        });
        let fresh = wait.with_choice(6);
        let stamped = guest_suffix(&[copied, fresh, fresh], Some(&plan), plan.untried);
        assert_eq!(stamped[0], copied);
        assert_eq!(
            stamped[1],
            fresh.with_guest(GuestChoices::from_plan(&plan, 1, Some(2)))
        );
        assert_eq!(
            stamped[2],
            fresh.with_guest(GuestChoices::from_plan(&plan, 2, None))
        );
        assert_ne!(
            stamped[1].guest.unwrap().seed,
            stamped[2].guest.unwrap().seed
        );
        assert_eq!(stamped[1].guest.unwrap().contrary, CERTAIN / 2);
        assert_eq!(
            guest_suffix(&[copied, fresh], None, None),
            vec![copied, fresh]
        );
    }

    #[test]
    fn a_suffix_names_the_node_site_only_when_its_first_action_asks_it() {
        let game = game();
        let ticks = std::num::NonZeroU16::MIN;
        let pause = FaultAction::from(FaultOperation::Pause(1, ticks)).with_choice(5);
        let wait = FaultAction::from(FaultOperation::Wait(ticks)).with_choice(5);
        let off = run(3, vec![1]);
        let on = FaultCampaignRun::any_bug(off.vocabulary.clone().with_untried_first_nodes(true));
        assert_eq!(game.choice_sites(&off).len(), 1);
        assert_eq!(
            game.choice_sites(&on).len(),
            usize::from(nodes::NODE_SITE_ID) + 1
        );
        assert_eq!(
            ChoiceSite::sites_without_default(&game.choice_sites(&on)),
            [nodes::NODE_SITE_ID]
        );
        assert!(ChoiceSite::sites_without_default(&game.choice_sites(&off)).is_empty());
        assert_eq!(game.untried_site(&off, &[pause, wait]), 0);
        assert_eq!(game.untried_site(&on, &[pause, wait]), nodes::NODE_SITE_ID);
        assert_eq!(game.untried_site(&on, &[wait, pause]), 0);
        assert_eq!(game.untried_site(&on, &[]), 0);
    }

    #[test]
    fn the_stall_report_reads_fault_records() {
        let game = game();
        let off = run(3, vec![1]);
        let names: Vec<String> = game
            .keys()
            .catalog()
            .signals()
            .iter()
            .map(|signal| signal.name.clone())
            .collect();
        assert!(!names.is_empty());
        assert_eq!(game.catalog_signals(&off), names);
        let observation = FaultObservations {
            restarts: 2,
            ..FaultObservations::default()
        };
        assert_eq!(
            game.observation_record(&off, &observation),
            Some(game.keys().record(&observation))
        );
        let snapshot = FaultSnapshot {
            observation: observation.clone(),
            ..FaultSnapshot::default()
        };
        assert_eq!(
            game.state_record(&off, &snapshot),
            Some(game.keys().record(&observation))
        );
    }

    #[test]
    fn the_stall_report_gives_the_fault_node_rows_a_job_writes() {
        use searcher::search::choice::{ChoicePolicy, ChoiceProfile};
        let game = game();
        let off = run(3, vec![1]);
        let on = FaultCampaignRun::any_bug(off.vocabulary.clone().with_untried_first_nodes(true));
        let parked = FaultCampaignRun::any_bug(
            on.vocabulary
                .clone()
                .with_instrumented_events(true)
                .with_resume_park(Some(vec![7]))
                .unwrap(),
        );
        let plan = ChoicePlan::new(
            &ChoicePolicy::default(),
            ChoiceProfile::default(),
            7,
            None,
            Vec::new(),
            Vec::new(),
        );
        let ticks = std::num::NonZeroU16::MIN;
        let pause = FaultAction::from(FaultOperation::Pause(2, ticks));
        let kill = FaultAction::from(FaultOperation::Kill(1, ticks));
        let wait = FaultAction::from(FaultOperation::Wait(ticks));
        let copied = kill.with_guest(GuestChoices {
            contrary: 1,
            hold: 2,
            seed: 3,
            untried: None,
        });
        let suffix = [pause, copied, wait, kill];
        for run in [&off, &on, &parked] {
            let resolved = nodes::resolve(&run.vocabulary, &suffix, Some(&plan), None, 0);
            let executed = guest_suffix(&resolved.suffix, Some(&plan), None);
            let mut asked = 0;
            for (index, (action, row)) in executed.iter().zip(&resolved.rows).enumerate() {
                let expected: Vec<ChoiceRecord> = row
                    .iter()
                    .map(|row| {
                        ChoiceRecord::new(
                            nodes::NODE_SITE_ID,
                            row.offered.clone(),
                            row.default,
                            row.taken,
                        )
                    })
                    .collect();
                asked += expected.len();
                assert_eq!(
                    game.choice_records(run, Some(&plan), index, action, &[]),
                    expected
                );
                assert!(
                    game.choice_records(run, None, index, action, &[])
                        .is_empty()
                );
            }
            assert_eq!(asked, 2);
        }
    }

    #[test]
    fn the_stall_report_counts_the_fault_node_row_of_a_parked_pause() {
        use searcher::search::choice::{ChoicePolicy, ChoiceProfile};
        let game = game();
        let off = run(3, vec![1]);
        let parked = FaultCampaignRun::any_bug(
            off.vocabulary
                .clone()
                .with_untried_first_nodes(true)
                .with_instrumented_events(true)
                .with_resume_park(Some(vec![7]))
                .unwrap(),
        );
        let plan = ChoicePlan::new(
            &ChoicePolicy::default(),
            ChoiceProfile::default(),
            7,
            None,
            Vec::new(),
            Vec::new(),
        );
        let ticks = std::num::NonZeroU16::MIN;
        let parked_pause = FaultAction::from(FaultOperation::PauseParked {
            node: 2,
            edges: 1,
            hold_us: 10_000,
            ticks,
            target: fault_policy::ParkTarget::new(7, 8),
        });
        let wait = FaultAction::from(FaultOperation::Wait(ticks));
        let resolved = nodes::resolve(
            &parked.vocabulary,
            &[parked_pause, wait],
            Some(&plan),
            None,
            0,
        );
        let executed = guest_suffix(&resolved.suffix, Some(&plan), None);
        let records = game.choice_records(&parked, Some(&plan), 0, &executed[0], &[]);
        let row = resolved.rows[0].as_ref().unwrap();
        assert_eq!(
            records,
            [ChoiceRecord::new(
                nodes::NODE_SITE_ID,
                row.offered.clone(),
                row.default,
                row.taken,
            )]
        );
        let ran_plain = FaultAction {
            operation: FaultOperation::Pause(executed[0].operation.target_node().unwrap(), ticks),
            ..executed[0]
        };
        assert_eq!(
            game.choice_records(&parked, Some(&plan), 0, &ran_plain, &[]),
            records
        );
        assert!(
            game.choice_records(&parked, Some(&plan), 1, &executed[1], &[])
                .is_empty()
        );
    }
}
