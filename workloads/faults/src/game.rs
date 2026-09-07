// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fault workload adapter over the generic campaign engine.

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use std::error::Error;

use searcher::search::archive::{
    ArchiveEntryReport, ArchiveKey, ProgressPoint, SelectorAccounting,
};
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use searcher::search::{
    archive::Input,
    campaign::{
        ArchiveReportState, CampaignActionResult, CampaignJobResult, CampaignTypes, Evaluation,
        GamePolicies, InputPolicy, Reporting, TargetExecution, postcard_result_sha256,
    },
    draw::{MixtureDraw, SuffixShape, draw_suffix},
    rollout,
};
use serde::{Deserialize, Serialize};

use crate::action::{CatalogAction, FaultAction};
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use crate::spec::WorkloadSpec;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use sha2::{Digest, Sha256};

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use consonance_client::catalog::StateCatalog;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use consonance_client::session::{PortableSnapshot, SdkEvent, Session, SessionConfig};

/// SDK state register ids published by the guest supervisor.
pub const REG_ACTION_COMPLETE: u32 = 1;
pub const REG_ASSERTION: u32 = 2;
pub const REG_PROGRESS: u32 = 3;
pub const REG_PARTITIONED: u32 = 4;
pub const REG_RECOVERIES: u32 = 5;
pub const REG_EXECUTION_ERROR: u32 = 6;
pub const REG_DELAYED: u32 = 7;
pub const REG_PENDING_WORK: u32 = 8;
pub const ASSERTION_POINT: u32 = 1;

/// Names paired with the register ids in the guest SDK catalog.
pub const ACTION_COMPLETE_NAME: &str = "fault.action_complete";
pub const ASSERTION_NAME: &str = "fault.assertion";
pub const PROGRESS_NAME: &str = "fault.progress";
pub const PARTITIONED_NAME: &str = "fault.partitioned";
pub const RECOVERIES_NAME: &str = "fault.recoveries";
pub const EXECUTION_ERROR_NAME: &str = "fault.execution_error";
pub const DELAYED_NAME: &str = "fault.delayed";
pub const PENDING_WORK_NAME: &str = "fault.pending_work";

/// Package-owned launch contract for the one-vCPU fault guest.
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub const FAULT_SESSION_SEED: u64 = 0x4641_554c_5453_4553;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub const FAULT_SESSION_RAM_BYTES: usize = 128 * 1024 * 1024;
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
// Allow the guest recovery window plus bounded work/check ticks to finish.
// The limit is part of SessionConfig and therefore the execution identity.
pub const FAULT_SESSION_RUN_BUDGET: u64 = 5_000_000_000;

/// Return the launch settings selected by this package rather than relying on
/// a workload-neutral client's defaults. The values are part of the session
/// identity recorded in campaign policy.
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
#[must_use]
pub fn fault_session_config() -> SessionConfig {
    let defaults = SessionConfig::default();
    SessionConfig::new(
        FAULT_SESSION_RAM_BYTES,
        FAULT_SESSION_SEED,
        FAULT_SESSION_RUN_BUDGET,
        defaults.cmdline,
    )
}

/// One stopped guest observation.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultObservation {
    pub action_complete: u64,
    pub assertion: u64,
    pub progress: u64,
    pub check_status: u64,
    pub partitioned: bool,
    pub delayed: bool,
    pub pending_work: bool,
    pub recoveries: u64,
    pub execution_error: bool,
}

/// A compact archive key. Pending and assertion bits are deliberately part of
/// the key, so a queued-work or failed-check state cannot collide with an
/// otherwise alive state at the same progress.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct FaultKey {
    pub progress: u16,
    pub partitioned: bool,
    pub delayed: bool,
    pub pending_work: bool,
    pub assertion: bool,
}

impl ArchiveKey for FaultKey {
    type Group = u16;
    type Lineage = ();

    fn groups() -> usize {
        2
    }

    fn group(self, depth: usize) -> Self::Group {
        match depth {
            0 => self.progress,
            1 => {
                (u16::from(self.partitioned) << 3)
                    | (u16::from(self.delayed) << 2)
                    | (u16::from(self.pending_work) << 1)
                    | u16::from(self.assertion)
            }
            _ => panic!("fault key group depth outside the declared policy"),
        }
    }

    fn complete(self, _parent: Option<(Self, &Self::Lineage)>) -> Self {
        self
    }

    fn record(_lineage: &mut Self::Lineage, _key: Self) {}
}

/// Milestones retained in the generic archive.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultMilestones {
    pub actions: u16,
    pub partitions: u16,
    pub delays: u16,
    pub restarts: u16,
    pub recoveries: u16,
    pub max_progress: u64,
}

/// Route-independent progress reported in campaign sidecars.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultProgress {
    pub checks: u64,
    pub recoveries: u64,
    pub bugs: u64,
}

/// Fault campaign policies owned by this package.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultCampaignRun {
    pub catalog_len: u16,
    pub max_work_ticks: u16,
    pub max_recovery_ticks: u16,
}

impl Default for FaultCampaignRun {
    fn default() -> Self {
        Self {
            catalog_len: CatalogAction::CATALOG_LEN,
            max_work_ticks: 8,
            max_recovery_ticks: 8,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultArchiveReport {
    pub seed: u64,
    pub executions: u64,
    #[serde(with = "searcher::search::archive::entries_by_suffix")]
    pub entries: Vec<ArchiveEntryReport<FaultAction, FaultKey, FaultMilestones>>,
    pub progress_curve: Vec<ProgressPoint<FaultMilestones, FaultProgress>>,
    pub retained: u64,
    pub rejected: u64,
    pub deaths: u64,
    pub selector: SelectorAccounting,
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
#[derive(Clone, Default)]
pub struct FaultEvidence {
    milestones: FaultMilestones,
    progress: FaultProgress,
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FaultSnapshot {
    pub state: PortableSnapshot,
    pub observation: FaultObservation,
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub struct FaultTarget {
    session: Session,
    initial: FaultSnapshot,
    current: FaultSnapshot,
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl FaultTarget {
    pub fn new(
        kernel: &[u8],
        initramfs: &[u8],
        config: SessionConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let session = Session::new_with_config(kernel, initramfs, config)?;
        let state = session.setup_snapshot()?;
        let observation = FaultObservation::default();
        let initial = FaultSnapshot { state, observation };
        Ok(Self {
            session,
            initial: initial.clone(),
            current: initial,
        })
    }

    fn apply(&mut self, action: &FaultAction) -> Result<(), Box<dyn Error>> {
        let state = self
            .session
            .apply_payload(&self.current.state, action.encode().to_vec())?;
        let observation = read_observation(&mut self.session)?;
        if observation.execution_error
            && let Ok(console) = self.session.console_tail()
            && !console.is_empty()
        {
            eprintln!(
                "FAULT_TARGET_EXECUTION_ERROR_CONSOLE:\n{}",
                String::from_utf8_lossy(&console)
            );
        }
        self.current = FaultSnapshot { state, observation };
        Ok(())
    }

    /// Apply one package action and advance the target to its stopped result.
    pub fn apply_action(&mut self, action: FaultAction) -> Result<(), Box<dyn Error>> {
        self.apply(&action)
    }

    /// Return the most recently published guest observation.
    #[must_use]
    pub fn observation(&self) -> &FaultObservation {
        &self.current.observation
    }

    /// Copy the current whole-VM checkpoint and observation.
    #[must_use]
    pub fn snapshot(&self) -> FaultSnapshot {
        self.current.clone()
    }

    fn restore_snapshot(&mut self, snapshot: &FaultSnapshot) -> Result<(), Box<dyn Error>> {
        self.session.restore(&snapshot.state)?;
        self.current = snapshot.clone();
        Ok(())
    }

    /// Restore a previously captured whole-VM checkpoint.
    pub fn restore(&mut self, snapshot: &FaultSnapshot) -> Result<(), Box<dyn Error>> {
        self.restore_snapshot(snapshot)
    }

    /// Hash the live whole-VM state at the current stopped point.
    pub fn state_hash(&mut self) -> Result<[u8; 32], Box<dyn Error>> {
        self.session.state_hash()
    }

    fn probe_snapshot(&mut self, snapshot: &FaultSnapshot) -> Result<bool, Box<dyn Error>> {
        let saved = self.current.clone();
        self.restore_snapshot(snapshot)?;
        let viable =
            self.current.observation.assertion == 0 && !self.current.observation.execution_error;
        self.restore_snapshot(&saved)?;
        Ok(viable)
    }

    fn reset(&mut self) -> Result<(), Box<dyn Error>> {
        let initial = self.initial.clone();
        self.restore_snapshot(&initial)
    }

    fn observations(&self) -> Vec<FaultObservation> {
        vec![self.current.observation.clone()]
    }

    fn key(&self) -> FaultKey {
        FaultKey {
            progress: u16::try_from(self.current.observation.progress.min(u64::from(u16::MAX)))
                .unwrap_or(u16::MAX),
            partitioned: self.current.observation.progress > 0
                && self.current.observation.partitioned,
            delayed: self.current.observation.delayed,
            pending_work: self.current.observation.pending_work,
            assertion: self.current.observation.assertion != 0,
        }
    }

    fn is_failed(&self) -> bool {
        self.current.observation.execution_error
    }

    fn is_bug(&self) -> bool {
        self.current.observation.assertion != 0
    }

    fn frames(&self) -> u64 {
        self.current.observation.action_complete
    }
}

/// A game context whose image and spec are already staged in the guest image.
#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
pub struct FaultGame {
    spec: WorkloadSpec,
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    identity: String,
    session_config: SessionConfig,
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl FaultGame {
    pub fn new_consonance(
        spec: &WorkloadSpec,
        kernel: &[u8],
        initramfs: &[u8],
    ) -> Result<Self, Box<dyn Error>> {
        spec.validate()?;
        let session_config = fault_session_config();
        Ok(Self {
            spec: spec.clone(),
            kernel: kernel.to_vec(),
            initramfs: initramfs.to_vec(),
            identity: Session::identity_with_config(kernel, initramfs, &session_config),
            session_config,
        })
    }

    #[must_use]
    pub fn workload_identity(&self) -> &str {
        &self.identity
    }

    /// Construct one real whole-VM target for package acceptance checks.
    pub fn acceptance_target(&self) -> Result<FaultTarget, Box<dyn Error>> {
        FaultTarget::new(&self.kernel, &self.initramfs, self.session_config.clone())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl CampaignTypes for FaultGame {
    type Target = FaultTarget;
    type Action = FaultAction;
    type Key = FaultKey;
    type Milestones = FaultMilestones;
    type Progress = FaultProgress;
    type Snapshot = FaultSnapshot;
    type Observations = FaultObservation;
    type Evidence = FaultEvidence;
    type ArchiveReport = FaultArchiveReport;
    type Run = FaultCampaignRun;
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = ();
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl Reporting for FaultGame {
    fn stream_format(&self) -> &'static str {
        "fault-campaign-stream-v2"
    }
    fn checkpoint_format(&self) -> &'static str {
        "fault-campaign-snapshot-checkpoint-v2"
    }
    fn image_sha256(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(&self.kernel);
        digest.update(&self.initramfs);
        format!("{:x}", digest.finalize())
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
    }

    fn archive_report(
        &self,
        _evidence: &Self::Evidence,
        state: ArchiveReportState<Self>,
    ) -> Self::ArchiveReport {
        FaultArchiveReport {
            seed: state.seed,
            executions: state.executions,
            entries: state.entries,
            progress_curve: state.progress_curve,
            retained: state.retained,
            rejected: state.rejected,
            deaths: state.deaths,
            selector: state.selector,
        }
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl InputPolicy for FaultGame {
    fn draw_state_memory_reserve_bytes(&self, _run: &Self::Run, _max_actions: usize) -> usize {
        0
    }
    fn draw_state_memory_bytes(&self, _state: &Self::DrawState) -> usize {
        0
    }
    fn policies(&self, run: &Self::Run) -> GamePolicies {
        [
            (
                "fault_catalog".to_owned(),
                format!("catalog-v2:{}", run.catalog_len),
            ),
            (
                "fault_runtime_schema".to_owned(),
                format!("runtime-action-v{}", fault_runtime::ACTION_SCHEMA_VERSION),
            ),
            (
                "fault_action_schema".to_owned(),
                format!(
                    "search-action-v{};delay-forward-latency-ms={};pending-work-v1",
                    crate::ACTION_SCHEMA_VERSION,
                    crate::action::DELAY_FORWARD_LATENCY_MS
                ),
            ),
            ("fault_workload".to_owned(), self.spec_identity()),
            ("fault_backend".to_owned(), self.identity.clone()),
        ]
        .into_iter()
        .collect()
    }
    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<Self::Run, Box<dyn Error>> {
        let run = FaultCampaignRun::default();
        let expected = self.policies(&run);
        for (field, value) in expected {
            if policies.get(&field) != Some(&value) {
                return Err(format!("fault policy {field} is not recognized").into());
            }
        }
        if policies
            .keys()
            .any(|field| !self.policies(&run).contains_key(field))
        {
            return Err("fault stream carries an unknown policy".into());
        }
        Ok(run)
    }
    fn initial_draw_state(
        &self,
        _run: &Self::Run,
        _origin: Option<(&str, &Self::ArchiveReport)>,
    ) -> Result<searcher::search::campaign::InitialDrawState<Self>, Box<dyn Error>> {
        Ok(((), None))
    }
    fn expand_suffix(
        &self,
        run: &Self::Run,
        _state: &Self::DrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<Self::Action>, Box<dyn Error>> {
        draw_actions(*run, shape, mixture, mutation_seed)
    }

    fn max_action_limit(&self) -> usize {
        128
    }

    fn longest_action_time(&self) -> u64 {
        1 + u64::from(u16::MAX) * 2
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl TargetExecution for FaultGame {
    fn new_target(&self) -> Result<Self::Target, String> {
        FaultTarget::new(&self.kernel, &self.initramfs, self.session_config.clone())
            .map_err(|error| error.to_string())
    }
    fn reset(&self, target: &mut Self::Target) {
        let _ = target.reset();
    }
    fn restore(
        &self,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore_snapshot(snapshot)
    }
    fn frames_clocked(&self, target: &Self::Target) -> u64 {
        target.frames()
    }
    fn apply_action(
        &self,
        target: &mut Self::Target,
        action: &Self::Action,
        milestones: &mut Self::Milestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action)?;
        milestones.actions = milestones.actions.saturating_add(1);
        milestones.max_progress = milestones
            .max_progress
            .max(target.current.observation.progress);
        match action.kind() {
            Some(CatalogAction::PartitionForward) => {
                milestones.partitions = milestones.partitions.saturating_add(1)
            }
            Some(CatalogAction::DelayForward) => {
                milestones.delays = milestones.delays.saturating_add(1)
            }
            Some(CatalogAction::RestartReplica) => {
                milestones.restarts = milestones.restarts.saturating_add(1)
            }
            Some(CatalogAction::RecoverForward) => {
                milestones.recoveries = milestones.recoveries.saturating_add(1)
            }
            _ => {}
        }
        Ok(())
    }
    fn rollout_observations(&self, target: &Self::Target) -> Vec<Self::Observations> {
        target.observations()
    }
    fn rollout_probe(
        &self,
        _run: &Self::Run,
        target: &mut Self::Target,
        snapshot: &Self::Snapshot,
    ) -> Result<bool, Box<dyn Error>> {
        target.probe_snapshot(snapshot)
    }
    fn snapshot(&self, target: &mut Self::Target) -> Result<Self::Snapshot, Box<dyn Error>> {
        Ok(target.current.clone())
    }

    fn action_time_fn(&self) -> fn(&Self::Action) -> u64 {
        |action| 1 + u64::from(action.work_ticks) + u64::from(action.recovery_ticks)
    }

    fn snapshot_memory_charge(snapshot: &Self::Snapshot) -> usize {
        snapshot
            .state
            .pages
            .iter()
            .map(|(_, page)| page.len())
            .sum::<usize>()
            .saturating_add(snapshot.state.sidecar.len())
            .saturating_add(std::mem::size_of::<FaultObservation>())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl Evaluation for FaultGame {
    fn merge_milestones(&self, into: &mut Self::Milestones, from: Self::Milestones) {
        *into = FaultMilestones {
            actions: into.actions.max(from.actions),
            partitions: into.partitions.max(from.partitions),
            delays: into.delays.max(from.delays),
            restarts: into.restarts.max(from.restarts),
            recoveries: into.recoveries.max(from.recoveries),
            max_progress: into.max_progress.max(from.max_progress),
        };
    }
    fn aggregate_milestones(evidence: &Self::Evidence) -> Self::Milestones {
        evidence.milestones
    }
    fn aggregate_progress(evidence: &Self::Evidence) -> Self::Progress {
        evidence.progress
    }
    fn merge_origin_evidence(&self, evidence: &mut Self::Evidence, source: &Self::ArchiveReport) {
        for entry in &source.entries {
            evidence.milestones = FaultMilestones {
                actions: evidence.milestones.actions.max(entry.milestones.actions),
                partitions: evidence
                    .milestones
                    .partitions
                    .max(entry.milestones.partitions),
                delays: evidence.milestones.delays.max(entry.milestones.delays),
                restarts: evidence.milestones.restarts.max(entry.milestones.restarts),
                recoveries: evidence
                    .milestones
                    .recoveries
                    .max(entry.milestones.recoveries),
                max_progress: evidence
                    .milestones
                    .max_progress
                    .max(entry.milestones.max_progress),
            };
        }
    }
    fn merge_snapshot_root_evidence(
        &self,
        _evidence: &mut Self::Evidence,
        _target: &Self::Target,
    ) -> Result<(), Box<dyn Error>> {
        Ok(())
    }
    fn merge_import_evidence(
        &self,
        evidence: &mut Self::Evidence,
        milestones: Self::Milestones,
        _input: &Input<Self::Action>,
    ) {
        self.merge_milestones(&mut evidence.milestones, milestones);
    }
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Self::Evidence,
        action: &CampaignActionResult<Self>,
        _sequence: u64,
        _input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<Self::Action>, Box<dyn Error>>,
    {
        self.merge_milestones(&mut evidence.milestones, action.milestones);
        for observation in &action.observations {
            evidence.progress.checks = evidence.progress.checks.saturating_add(1);
            evidence.progress.recoveries = evidence
                .progress
                .recoveries
                .saturating_add(u64::from(observation.assertion == 0));
            evidence.progress.bugs = evidence
                .progress
                .bugs
                .saturating_add(u64::from(observation.assertion != 0));
        }
        Ok(())
    }
    fn source_entries<'a>(
        &self,
        source: &'a Self::ArchiveReport,
    ) -> &'a [ArchiveEntryReport<Self::Action, Self::Key, Self::Milestones>] {
        &source.entries
    }
    fn resume_input(
        &self,
        source: &Self::ArchiveReport,
    ) -> Result<Input<Self::Action>, Box<dyn Error>> {
        source
            .entries
            .iter()
            .max_by_key(|entry| (entry.key, entry.id))
            .map(|entry| entry.input.clone())
            .ok_or_else(|| "fault archive has no retained entries".into())
    }

    fn is_terminal(&self, target: &Self::Target) -> bool {
        target.is_failed()
    }

    fn is_run_terminal(
        &self,
        _run: &Self::Run,
        target: &Self::Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.is_failed() || target.is_bug())
    }

    fn rollout_outcome(
        &self,
        _run: &Self::Run,
        target: &Self::Target,
    ) -> Result<rollout::Outcome, Box<dyn Error>> {
        Ok(rollout::Outcome {
            dead: false,
            victory: target.is_bug(),
            failed: target.is_failed(),
        })
    }

    fn current_key(&self, target: &Self::Target) -> Result<Self::Key, Box<dyn Error>> {
        Ok(target.key())
    }

    fn complete_candidate_key(
        &self,
        key: Self::Key,
        _snapshot: &Self::Snapshot,
    ) -> Result<Self::Key, Box<dyn Error>> {
        Ok(key)
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl FaultGame {
    fn spec_identity(&self) -> String {
        self.spec
            .identity_sha256()
            .unwrap_or_else(|_| "invalid".into())
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn draw_actions(
    run: FaultCampaignRun,
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
        |rand| {
            let catalog = rand.below(
                std::num::NonZeroUsize::new(usize::from(run.catalog_len))
                    .ok_or("fault catalog is empty")?,
            );
            let work = rand.below(
                std::num::NonZeroUsize::new(usize::from(run.max_work_ticks).max(1))
                    .ok_or("fault work bound is empty")?,
            );
            let recovery = rand.below(
                std::num::NonZeroUsize::new(usize::from(run.max_recovery_ticks).max(1))
                    .ok_or("fault recovery bound is empty")?,
            );
            Ok(FaultAction {
                catalog: u16::try_from(catalog).map_err(|_| "catalog index overflow")?,
                work_ticks: u16::try_from(work).map_err(|_| "work index overflow")?,
                recovery_ticks: u16::try_from(recovery).map_err(|_| "recovery index overflow")?,
            })
        },
    )
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn read_observation(session: &mut Session) -> Result<FaultObservation, Box<dyn Error>> {
    let events = session.sdk_events()?;
    observation_from_events(&events)
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn observation_from_events(events: &[SdkEvent]) -> Result<FaultObservation, Box<dyn Error>> {
    let mut catalog = StateCatalog::default();
    for (_, event_id, bytes) in events {
        catalog.observe(*event_id, bytes)?;
    }
    observation_from_catalog(&catalog)
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn required_state(catalog: &StateCatalog, name: &str) -> Result<u64, Box<dyn Error>> {
    catalog
        .get(name)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error).into())
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
fn observation_from_catalog(catalog: &StateCatalog) -> Result<FaultObservation, Box<dyn Error>> {
    let assertion = required_state(catalog, ASSERTION_NAME)?;
    Ok(FaultObservation {
        action_complete: required_state(catalog, ACTION_COMPLETE_NAME)?,
        assertion,
        progress: required_state(catalog, PROGRESS_NAME)?,
        check_status: assertion,
        partitioned: required_state(catalog, PARTITIONED_NAME)? != 0,
        delayed: required_state(catalog, DELAYED_NAME)? != 0,
        pending_work: required_state(catalog, PENDING_WORK_NAME)? != 0,
        recoveries: required_state(catalog, RECOVERIES_NAME)?,
        execution_error: required_state(catalog, EXECUTION_ERROR_NAME)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_key_separates_partition_and_assertion_states() {
        let alive = FaultKey {
            progress: 4,
            partitioned: true,
            delayed: false,
            pending_work: false,
            assertion: false,
        };
        let bug = FaultKey {
            assertion: true,
            ..alive
        };
        let pending = FaultKey {
            pending_work: true,
            ..alive
        };
        assert_ne!(alive, bug);
        assert_ne!(alive, pending);
        assert_eq!(alive.group(1), 8);
        assert_eq!(bug.group(1), 9);
        assert_eq!(pending.group(1), 10);
    }

    #[test]
    fn action_time_is_bounded_and_catalog_is_copy_ordered() {
        let a = FaultAction::new(CatalogAction::PartitionForward, 4, 8);
        let b = FaultAction::new(CatalogAction::RecoverForward, 4, 8);
        assert!(a < b);
        assert_eq!(
            1 + u64::from(a.work_ticks) + u64::from(a.recovery_ticks),
            13
        );
    }

    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    mod observation {
        use super::{
            ACTION_COMPLETE_NAME, ASSERTION_NAME, ASSERTION_POINT, DELAYED_NAME,
            EXECUTION_ERROR_NAME, PARTITIONED_NAME, PENDING_WORK_NAME, PROGRESS_NAME,
            RECOVERIES_NAME, REG_ACTION_COMPLETE, REG_ASSERTION, REG_DELAYED, REG_EXECUTION_ERROR,
            REG_PARTITIONED, REG_PENDING_WORK, REG_PROGRESS, REG_RECOVERIES, SdkEvent,
            StateCatalog, observation_from_catalog, observation_from_events,
        };

        const STATE_KIND: u8 = 4;
        const STATE_NAMESPACE: u32 = 2 << 24;

        fn catalog_declaration(values: &[(u32, &str, u64)]) -> Vec<u8> {
            let mut bytes = b"SDKC\x01".to_vec();
            bytes.extend((values.len() as u32).to_le_bytes());
            for (id, name, _value) in values {
                bytes.push(STATE_KIND);
                bytes.extend(id.to_le_bytes());
                bytes.extend((name.len() as u16).to_le_bytes());
                bytes.extend(name.as_bytes());
            }
            bytes
        }

        fn state_event(id: u32, value: u64) -> SdkEvent {
            let mut state = vec![0];
            state.extend(value.to_le_bytes());
            (0, STATE_NAMESPACE | id, state)
        }

        fn events(values: &[(u32, &str, u64)]) -> Vec<SdkEvent> {
            let mut events = vec![(0, 0, catalog_declaration(values))];
            for (id, _name, value) in values {
                events.push(state_event(*id, *value));
            }
            events
        }

        #[test]
        fn current_assertion_register_can_clear_after_recovery() {
            let values = [
                (REG_ACTION_COMPLETE, ACTION_COMPLETE_NAME, 1),
                (REG_ASSERTION, ASSERTION_NAME, 1),
                (REG_PROGRESS, PROGRESS_NAME, 3),
                (REG_PARTITIONED, PARTITIONED_NAME, 1),
                (REG_RECOVERIES, RECOVERIES_NAME, 0),
                (REG_EXECUTION_ERROR, EXECUTION_ERROR_NAME, 0),
                (REG_DELAYED, DELAYED_NAME, 0),
                (REG_PENDING_WORK, PENDING_WORK_NAME, 0),
            ];
            let mut events = events(&values);
            events.insert(1, (1, ASSERTION_POINT, vec![1]));
            events.push(state_event(REG_ASSERTION, 0));
            events.push(state_event(REG_PARTITIONED, 0));
            events.push(state_event(REG_RECOVERIES, 1));

            let observation = observation_from_events(&events).unwrap();
            assert_eq!(observation.assertion, 0);
            assert_eq!(observation.check_status, 0);
            assert!(!observation.partitioned);
            assert!(!observation.delayed);
            assert!(!observation.pending_work);
            assert_eq!(observation.recoveries, 1);
        }

        #[test]
        fn missing_registers_are_execution_errors() {
            let error = observation_from_catalog(&StateCatalog::default()).unwrap_err();
            assert!(error.to_string().contains("not declared"));
        }
    }
}
