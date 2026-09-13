// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeSet,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use machine::Machine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignConfig, CampaignJobResult, CampaignModeReport, CampaignOrigin,
            CampaignStreamHeader, CampaignTypes, Evaluation, InputPolicy, Reporting,
            SnapshotCheckpoint, TargetExecution, WorkloadPolicies, postcard_value_sha256,
            replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
        rollout::{ExecutionDisposition, Outcome},
    },
    target::{ExitKind, Target},
    thwaite::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MAX_THWAITE_ACTIONS,
            REPLACEMENT_IDENTIFIER, ThwaiteArchiveKey, ThwaiteArchiveReport, ThwaiteChampionKey,
            ThwaiteMilestoneInputs, ThwaiteMilestoneTimes, ThwaiteMilestones,
            ThwaiteProgressWatermark, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones_from_observation, sample_chord,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, ThwaiteInput, ThwaiteObservations, ThwaiteSnapshot,
            ThwaiteTarget,
        },
    },
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "thwaite-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "thwaite-quicknes-snapshot-checkpoint-v1";
pub const THWAITE_COMMIT: &str = "00e36745188bc165990f60eed6b093c3ce6ad0e3";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_no_start_select_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "one_player_town_gameover_perfect_hour_v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThwaiteNoTableHeader;

pub struct ThwaiteGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    identity: String,
}

impl ThwaiteGame {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        let identity = format!(
            "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;source=pinobatch/thwaite-nes@{THWAITE_COMMIT};rom=thwaite_nrom256;mode=one_player;genesis=thwaite-first-hour-v1;result_digest=thwaite-semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}",
            machine::quicknes::QUICKNES_REVISION,
            machine::quicknes::QUICKNES_BUILD,
            machine::quicknes::QUICKNES_OPTIONS,
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            identity,
        }
    }

    pub fn from_environment(rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let core_path = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned QuickNES core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        Ok(Self::new(rom, &core_path, &core_sha256))
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ThwaiteCampaignRun;

#[derive(Clone, Default)]
pub struct ThwaiteCampaignEvidence {
    aggregate: ThwaiteMilestones,
    watermark: ThwaiteProgressWatermark,
    first_reached: ThwaiteMilestoneTimes,
    first_inputs: ThwaiteMilestoneInputs,
    champion_input: ThwaiteInput,
    champion_milestones: ThwaiteMilestones,
    champion_key: Option<ThwaiteChampionKey>,
}

pub type ThwaiteCampaignOrigin = CampaignOrigin<ThwaiteGame>;
pub type ThwaiteCampaignCheckpoint = CampaignCheckpoint<ThwaiteSnapshot>;
pub type ThwaiteSnapshotCheckpoint = SnapshotCheckpoint<ThwaiteSnapshot>;
pub type ThwaiteCampaignStreamHeader = CampaignStreamHeader<ThwaiteNoTableHeader>;
pub type ThwaiteCampaignModeReport = CampaignModeReport<ButtonChord, ThwaiteArchiveReport>;
type ThwaiteCampaignActionResult = CampaignActionResult<ThwaiteGame>;
type ThwaiteCampaignJobResult = CampaignJobResult<ThwaiteGame>;

#[derive(Serialize)]
struct ThwaiteResultCandidate<'a> {
    key: &'a ThwaiteArchiveKey,
    viable: bool,
}

#[derive(Serialize)]
struct ThwaiteResultAction<'a> {
    action: ButtonChord,
    observations: &'a [ThwaiteObservations],
    milestones: ThwaiteMilestones,
    outcome: Outcome,
    candidate: Option<ThwaiteResultCandidate<'a>>,
}

#[derive(Serialize)]
struct ThwaiteResult<'a> {
    actions: Vec<ThwaiteResultAction<'a>>,
}

fn thwaite_result_sha256(result: &ThwaiteCampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|action| ThwaiteResultAction {
            action: action.action,
            observations: &action.observations,
            milestones: action.milestones,
            outcome: action.outcome,
            candidate: action
                .candidate
                .as_ref()
                .map(|candidate| ThwaiteResultCandidate {
                    key: &candidate.key,
                    viable: candidate.viable,
                }),
        })
        .collect();
    postcard_value_sha256(&ThwaiteResult { actions })
}

pub struct ThwaiteCampaignConfig {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_perfect_hour: bool,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub selector: crate::search::archive::SelectorPolicy,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub perfect_hour_input_path: Option<PathBuf>,
}

impl ThwaiteCampaignConfig {
    fn generic(&self) -> CampaignConfig<ThwaiteGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: !self.continue_after_perfect_hour,
            stop_campaign_on_objective: !self.continue_after_perfect_hour,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker:
                crate::search::campaign::DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: ThwaiteCampaignRun,
            suffix: self.suffix,
            mixture: self.mixture,
            retention: self.retention,
            selector: self.selector.clone(),
            objective_witness_path: self.perfect_hour_input_path.clone(),
        }
    }
}

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Thwaite stream is missing {field}").into())
}

fn merge_action_milestones<M: Machine>(
    aggregate: &mut ThwaiteMilestones,
    target: &ThwaiteTarget<M>,
) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Ok(());
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones_from_observation(observation));
    }
    Ok(())
}

pub(super) fn execute_suffix<M: Machine<Portable = machine::SharedState>>(
    target: &mut ThwaiteTarget<M>,
    parent_actions: usize,
    parent_milestones: ThwaiteMilestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<ThwaiteCampaignJobResult, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = Outcome {
        objective_reached: target.exit_kind() == ExitKind::Ok && target.defended_a_perfect_hour(),
        disposition: if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_game_over() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        },
    };
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        let action_start_frame = target.observe().frame_count;
        target.apply(action);
        merge_action_milestones(&mut aggregate, target)?;
        let observations = if target.exit_kind() != ExitKind::Ok {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        };
        let raw_objective = target.exit_kind() == ExitKind::Ok && target.defended_a_perfect_hour();
        let objective_reached = raw_objective && !objective_seen;
        objective_seen |= raw_objective;
        let disposition = if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_game_over() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        };
        let outcome = Outcome {
            objective_reached,
            disposition,
        };
        let town_valid = target.mechanical_state().town_valid();
        let recorded_action = if matches!(outcome.disposition, ExecutionDisposition::Terminal) {
            let elapsed = target
                .observe()
                .frame_count
                .saturating_sub(action_start_frame)
                .min(u64::from(MAX_HOLD_FRAMES));
            ButtonChord::new(action.buttons, u8::try_from(elapsed.max(1))?)
        } else {
            *action
        };
        let candidate = if !matches!(outcome.disposition, ExecutionDisposition::Runnable)
            || !town_valid
        {
            None
        } else {
            let state = target.mechanical_state();
            let evidence = target.defence_evidence();
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot Thwaite suffix")?;
            let viable = match retention {
                RetentionPolicy::Unprobed => true,
                RetentionPolicy::ProbeAtAdmission => {
                    return Err(
                        "Thwaite deliberately rejects ProbeAtAdmission: ordinary admission is sufficient".into(),
                    );
                }
            };
            Some(CampaignCandidate {
                key: archive_key(state, evidence)
                    .ok_or("Thwaite attempted to archive a phase-invalid state")?,
                viable,
                snapshot,
            })
        };
        actions.push(CampaignActionResult {
            action: recorded_action,
            observations,
            milestones: aggregate,
            outcome,
            candidate,
        });
        if outcome.should_stop(stop_rollout_on_objective) {
            break;
        }
    }
    Ok(CampaignJobResult { actions })
}

fn update_first_inputs(
    times: &mut ThwaiteMilestoneTimes,
    inputs: &mut ThwaiteMilestoneInputs,
    value: ThwaiteMilestones,
    sequence: u64,
    input: &ThwaiteInput,
) {
    if value.levels_cleared > 0 && times.first_level_cleared.is_none() {
        times.first_level_cleared = Some(sequence);
        inputs.first_level_cleared = Some(input.clone());
    }
    if value.perfect_levels > 0 && times.first_perfect_level.is_none() {
        times.first_perfect_level = Some(sequence);
        inputs.first_perfect_level = Some(input.clone());
    }
    if value.game_over && times.first_game_over.is_none() {
        times.first_game_over = Some(sequence);
        inputs.first_game_over = Some(input.clone());
    }
}

fn action_champion_key(observations: &[ThwaiteObservations]) -> Option<ThwaiteChampionKey> {
    observations.last().and_then(|observation| {
        archive_key(observation.decoded, observation.evidence).map(ThwaiteArchiveKey::champion_key)
    })
}

impl CampaignTypes for ThwaiteGame {
    type Target = ThwaiteTarget;
    type Action = ButtonChord;
    type Key = ThwaiteArchiveKey;
    type Milestones = ThwaiteMilestones;
    type Progress = ThwaiteProgressWatermark;
    type Snapshot = ThwaiteSnapshot;
    type Observations = ThwaiteObservations;
    type Evidence = ThwaiteCampaignEvidence;
    type ArchiveReport = ThwaiteArchiveReport;
    type Run = ThwaiteCampaignRun;
    type DrawState = ();
    type DrawCheckpoint = ();
    type DrawHeader = ThwaiteNoTableHeader;
}

impl Reporting for ThwaiteGame {
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn workload_identity_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rom))
    }

    fn action_cost_unit(&self) -> &'static str {
        "frames"
    }

    fn execution_work_unit(&self) -> &'static str {
        "frames"
    }

    fn result_sha256(&self, result: &ThwaiteCampaignJobResult) -> Result<String, Box<dyn Error>> {
        thwaite_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &ThwaiteCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> ThwaiteArchiveReport {
        ThwaiteArchiveReport {
            seed: state.seed,
            executions: state.executions,
            milestones: evidence.aggregate,
            progress_watermark: evidence.watermark,
            first_reached: evidence.first_reached,
            first_inputs: evidence.first_inputs.clone(),
            champion_input: evidence.champion_input.clone(),
            entries: state.entries,
            progress_curve: state.progress_curve,
            retained: state.retained,
            rejected: state.rejected,
            deaths: state
                .terminal_endpoints
                .saturating_sub(state.terminal_objectives),
            selector: state.selector,
        }
    }
}

impl InputPolicy for ThwaiteGame {
    fn max_action_limit(&self) -> usize {
        MAX_THWAITE_ACTIONS
    }

    fn max_action_cost(&self) -> u64 {
        u64::from(crate::thwaite::archive::LONGEST_HOLD_FRAMES)
    }

    fn draw_state_memory_reserve_bytes(
        &self,
        _run: &ThwaiteCampaignRun,
        _max_actions: usize,
    ) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn policies(&self, _run: &ThwaiteCampaignRun) -> WorkloadPolicies {
        [
            (
                CONTROLLER_VOCABULARY_FIELD,
                CONTROLLER_VOCABULARY_IDENTIFIER,
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, TERMINAL_POLICY_IDENTIFIER),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .chain(std::iter::once((
            EMULATOR_BACKEND_FIELD.to_owned(),
            self.identity.clone(),
        )))
        .collect()
    }

    fn resolve_recorded(
        &self,
        policies: &WorkloadPolicies,
    ) -> Result<ThwaiteCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&ThwaiteCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Thwaite stream {field} policy is not recognized").into());
                }
            }
            return Err("Thwaite stream carries an unknown game policy".into());
        }
        Ok(ThwaiteCampaignRun)
    }

    fn initial_draw_state(
        &self,
        _run: &ThwaiteCampaignRun,
        _origin: Option<(&str, &ThwaiteArchiveReport)>,
    ) -> Result<((), Option<ThwaiteNoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(&self, _state: &()) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &ThwaiteCampaignRun,
        _state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            mutation_seed,
            |_| Ok(None),
            sample_chord,
        )
    }

    fn expand_suffix_recorded(
        &self,
        run: &ThwaiteCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&()>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Thwaite stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &ThwaiteCampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn retained_inputs_need_full(&self, _run: &ThwaiteCampaignRun) -> bool {
        false
    }

    fn remember_draw_version(
        &self,
        _state: &mut (),
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        if required.is_empty() {
            Ok(())
        } else {
            Err("Thwaite stream requires an unsupported draw-table version".into())
        }
    }
}

impl TargetExecution for ThwaiteGame {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &ThwaiteSnapshot) -> usize {
        machine::quicknes::QuickNesMachine::portable_memory_charge(&snapshot.emulator_state)
    }

    fn new_target(&self) -> Result<ThwaiteTarget, String> {
        ThwaiteTarget::from_rom_bytes_headless(&self.rom, &self.core_path, &self.core_sha256)
            .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut ThwaiteTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut ThwaiteTarget,
        snapshot: &ThwaiteSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &ThwaiteTarget) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut ThwaiteTarget,
        action: &ButtonChord,
        aggregate: &mut ThwaiteMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(&self, target: &mut ThwaiteTarget) -> Result<ThwaiteSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Thwaite".into())
    }

    fn execute_job(
        &self,
        _run: &ThwaiteCampaignRun,
        target: &mut ThwaiteTarget,
        origin_snapshot: &ThwaiteSnapshot,
        replay: &[ButtonChord],
        parent_actions: usize,
        parent_milestones: ThwaiteMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<ThwaiteCampaignJobResult, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(
            target,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            retention,
            stop_rollout_on_objective,
        )
    }
}

impl Evaluation for ThwaiteGame {
    fn execution_disposition(&self, target: &ThwaiteTarget) -> ExecutionDisposition {
        if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_game_over() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        _run: &ThwaiteCampaignRun,
        target: &ThwaiteTarget,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.defended_a_perfect_hour())
    }

    fn current_key(&self, target: &ThwaiteTarget) -> Result<ThwaiteArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state(), target.defence_evidence())
            .ok_or_else(|| "Thwaite current state has no valid town archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: ThwaiteArchiveKey,
        _snapshot: &ThwaiteSnapshot,
    ) -> Result<ThwaiteArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut ThwaiteMilestones, from: ThwaiteMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &ThwaiteCampaignEvidence) -> ThwaiteMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &ThwaiteCampaignEvidence) -> ThwaiteProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut ThwaiteCampaignEvidence,
        source: &ThwaiteArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut ThwaiteCampaignEvidence,
        target: &ThwaiteTarget,
    ) -> Result<(), Box<dyn Error>> {
        let observation = ThwaiteObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            changed_indices: Vec::new(),
            evidence: target.defence_evidence(),
            terminal: target.is_game_over(),
            log_line: String::new(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut ThwaiteCampaignEvidence,
        value: ThwaiteMilestones,
        input: &ThwaiteInput,
    ) {
        merge_milestones(&mut evidence.aggregate, value);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            value,
            0,
            input,
        );
        if milestone_key(value) > milestone_key(evidence.champion_milestones) {
            evidence.champion_milestones = value;
            evidence.champion_input = input.clone();
        }
    }

    fn merge_action_evidence<F>(
        &self,
        evidence: &mut ThwaiteCampaignEvidence,
        action: &ThwaiteCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<ThwaiteInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let first_input_needed = (action.milestones.levels_cleared > 0
            && evidence.first_inputs.first_level_cleared.is_none())
            || (action.milestones.perfect_levels > 0
                && evidence.first_inputs.first_perfect_level.is_none())
            || (action.milestones.game_over && evidence.first_inputs.first_game_over.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed || champion.is_some() {
            let input = input()?;
            update_first_inputs(
                &mut evidence.first_reached,
                &mut evidence.first_inputs,
                action.milestones,
                sequence,
                &input,
            );
            if let Some(key) = champion {
                evidence.champion_key = Some(key);
                evidence.champion_milestones = action.milestones;
                evidence.champion_input = input;
            }
        }
        Ok(())
    }

    fn source_entries<'a>(
        &self,
        source: &'a ThwaiteArchiveReport,
    ) -> &'a [crate::thwaite::archive::ThwaiteArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &ThwaiteArchiveReport) -> Result<ThwaiteInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Thwaite source archive has no retained entries".into())
    }
}

pub fn run_thwaite_campaign_checkpointed(
    game: &ThwaiteGame,
    config: &ThwaiteCampaignConfig,
    origin: &ThwaiteCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(ThwaiteCampaignModeReport, ThwaiteSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_thwaite_campaign_checkpointed(
    game: &ThwaiteGame,
    stream_bytes: &[u8],
    origin_report: Option<&ThwaiteArchiveReport>,
    origin_checkpoint: Option<&ThwaiteCampaignCheckpoint>,
) -> Result<(ThwaiteCampaignModeReport, ThwaiteSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thwaite::target::{
        NUM_BUILDINGS, STATE_ACTIVE, ThwaiteDefenceEvidence, ThwaiteTownState,
    };

    fn town_state() -> crate::thwaite::target::ThwaiteMechanicalState {
        crate::thwaite::target::ThwaiteMechanicalState {
            game_state: STATE_ACTIVE,
            num_players: 1,
            town: Some(ThwaiteTownState {
                buildings: [1; NUM_BUILDINGS],
                buildings_standing: 12,
                silos_standing: 2,
                silo_missiles: [15, 15],
                enemy_missiles_left: 10,
                enemy_missiles_in_flight: 0,
                buildings_destroyed_this_level: 0,
                crosshair_x: 64,
                crosshair_y: 128,
            }),
            ..crate::thwaite::target::ThwaiteMechanicalState::default()
        }
    }

    fn result_with_portable(bytes: Vec<u8>) -> ThwaiteCampaignJobResult {
        let state = town_state();
        let observation = ThwaiteObservations {
            frame_count: 3,
            decoded: state,
            changed_indices: vec![1, 2],
            evidence: ThwaiteDefenceEvidence::default(),
            terminal: false,
            log_line: "frame=3 changed=[1, 2]".to_owned(),
        };
        let portable = serde_json::from_value(serde_json::json!(bytes))
            .expect("shared-state wire representation");
        CampaignJobResult {
            actions: vec![CampaignActionResult {
                action: ButtonChord::new(0x81, 3),
                observations: vec![observation.clone()],
                milestones: ThwaiteMilestones::default(),
                outcome: Outcome::default(),
                candidate: Some(CampaignCandidate {
                    key: archive_key(state, ThwaiteDefenceEvidence::default())
                        .expect("synthetic live archive key"),
                    viable: true,
                    snapshot: ThwaiteSnapshot {
                        emulator_state: portable,
                        observation,
                        wram: vec![0; 2_048],
                        failed: false,
                        evidence: ThwaiteDefenceEvidence::default(),
                    },
                }),
            }],
        }
    }

    #[test]
    fn champion_ranks_defence_and_ignores_aim_diagnostics() {
        let mut intact = result_with_portable(vec![]).actions.remove(0).observations;
        let mut ruined = intact.clone();
        ruined[0].decoded.town.as_mut().unwrap().buildings_standing = 7;
        assert!(action_champion_key(&intact) > action_champion_key(&ruined));

        let mut aimed = intact.clone();
        aimed[0].decoded.town.as_mut().unwrap().crosshair_x = 200;
        aimed[0]
            .decoded
            .town
            .as_mut()
            .unwrap()
            .enemy_missiles_in_flight = 3;
        assert_eq!(action_champion_key(&intact), action_champion_key(&aimed));

        intact[0].evidence.perfect_levels = 1;
        assert!(action_champion_key(&intact) > action_champion_key(&aimed));
    }

    #[test]
    fn source_identity_matches_the_pinned_build_input() {
        let commit = include_str!("../../thwaite-versions.env")
            .lines()
            .find_map(|line| line.strip_prefix("THWAITE_COMMIT="))
            .unwrap();
        assert_eq!(commit, THWAITE_COMMIT);
        let game = ThwaiteGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        assert!(
            game.emulator_identity()
                .contains(&format!("source=pinobatch/thwaite-nes@{commit};"))
        );
    }

    #[test]
    fn previous_key_policy_requires_its_previous_implementation() {
        let game = ThwaiteGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        let mut policies = game.policies(&ThwaiteCampaignRun);
        policies.insert(
            KEY_POLICY_FIELD.to_owned(),
            "thwaite_town_defence_wave_phase_v0".to_owned(),
        );
        assert!(game.resolve_recorded(&policies).is_err());
    }

    #[test]
    fn recorded_policy_set_is_exact_and_game_owned() {
        let game = ThwaiteGame::new(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64));
        let policies = game.policies(&ThwaiteCampaignRun);
        let ThwaiteCampaignRun = game.resolve_recorded(&policies).expect("resolve");
        let mut foreign = policies;
        foreign.insert("level".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
    }

    #[test]
    fn recordings_reject_another_core_build() {
        let game = ThwaiteGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        let other = ThwaiteGame::new(&[], Path::new("core.so"), &"b".repeat(64));
        assert!(
            game.resolve_recorded(&other.policies(&ThwaiteCampaignRun))
                .is_err()
        );
        assert!(
            game.resolve_recorded(&game.policies(&ThwaiteCampaignRun))
                .is_ok()
        );
    }

    #[test]
    fn result_digest_uses_game_visible_candidate_state_not_portable_bytes() {
        let first = result_with_portable(vec![1, 2, 3]);
        let mut second = result_with_portable(vec![9, 8, 7, 6]);
        assert_eq!(
            thwaite_result_sha256(&first).expect("first digest"),
            thwaite_result_sha256(&second).expect("second digest"),
        );

        let mut changed = town_state();
        changed.town.as_mut().unwrap().buildings_standing = 11;
        second.actions[0].candidate.as_mut().expect("candidate").key =
            archive_key(changed, ThwaiteDefenceEvidence::default())
                .expect("synthetic changed live archive key");
        assert_ne!(
            thwaite_result_sha256(&first).expect("first digest"),
            thwaite_result_sha256(&second).expect("changed digest"),
        );
    }
}
