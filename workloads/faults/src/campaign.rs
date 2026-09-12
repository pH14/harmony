// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, io::Write, path::PathBuf, sync::OnceLock};

use searcher::{
    search::{
        archive::{RetentionPolicy, SelectorPolicy},
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignConfig,
            CampaignJobResult, CampaignModeReport, CampaignOrigin, CampaignProgressRecord,
            CampaignStreamHeader, CampaignTypes, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            Evaluation, GamePolicies, InitialDrawState, InputPolicy, Reporting, SnapshotCheckpoint,
            TargetExecution, postcard_result_sha256, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    },
    target::ExitKind,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{
        DURATION_IDENTIFIER, FaultArchiveKey, FaultArchiveReport, FaultBugRecord, FaultInput,
        FaultMilestones, FaultProgressWatermark, KEY_POLICY_IDENTIFIER, MAX_RECORDED_BUGS,
        REPLACEMENT_IDENTIFIER, action_time, archive_key, bug_outcome, merge_milestones,
        merge_progress_watermark, milestone_key, milestones, sample_action,
    },
    bundle::FaultVocabulary,
    consonance::{FaultConfig, FaultTarget, identity, snapshot_memory_charge},
    target::{FaultAction, FaultObservations, FaultSnapshot, MAX_FAULT_ACTIONS},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "faultlab-consonance-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "faultlab-consonance-snapshot-checkpoint-v1";
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

pub struct FaultGame {
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    config: FaultConfig,
    identity: String,
    root_seal: OnceLock<u64>,
}

impl FaultGame {
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
    champion_input: FaultInput,
    champion_milestones: FaultMilestones,
    bugs: Vec<FaultBugRecord>,
}

pub type FaultCampaignOrigin = CampaignOrigin<FaultGame>;
pub type FaultSnapshotCheckpoint = SnapshotCheckpoint<FaultSnapshot>;
pub type FaultCampaignStreamHeader = CampaignStreamHeader<FaultNoTableHeader>;
pub type FaultCampaignModeReport = CampaignModeReport<FaultAction, FaultArchiveReport>;
pub type FaultCampaignProgressRecord = CampaignProgressRecord<FaultArchiveKey>;
type FaultCampaignActionResult = CampaignActionResult<FaultGame>;
type FaultCampaignJobResult = CampaignJobResult<FaultGame>;

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
            continue_after_victory: false,
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
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation));
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
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = FaultNoTableHeader;
}

impl Reporting for FaultGame {
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
            selector: state.selector,
        }
    }
}

impl InputPolicy for FaultGame {
    fn max_action_limit(&self) -> usize {
        MAX_FAULT_ACTIONS
    }

    fn longest_action_time(&self) -> u64 {
        1
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
        ])
        .collect()
    }

    fn resolve_recorded(
        &self,
        policies: &GamePolicies,
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
            |rand| sample_action(rand, &run.vocabulary),
        )
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
        execute_job(
            target,
            origin_snapshot,
            replay,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
        )
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
}
