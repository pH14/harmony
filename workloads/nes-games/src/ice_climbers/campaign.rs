// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    chord::{CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD},
    ice_climbers::{
        archive::{
            DURATION_IDENTIFIER, IceClimbersArchiveKey, IceClimbersArchiveReport,
            IceClimbersMilestoneInputs, IceClimbersMilestoneTimes, IceClimbersMilestones,
            IceClimbersProgressWatermark, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER,
            archive_key, chord_time, merge_milestones, merge_progress_watermark, milestone_key,
            milestones_from_observation, progress_point,
        },
        target::{
            ButtonChord, FLOORS, IceClimbersInput, IceClimbersObservations, IceClimbersSnapshot,
            IceClimbersTarget, MAX_HOLD_FRAMES, MOUNTAINS,
        },
    },
    nes_backend::{NesBackend, SnapshotState},
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignJobResult, CampaignModeReport, CampaignStreamHeader, CampaignTypes, Evaluation,
            InputPolicy, Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
            postcard_semantic_result_sha256, postcard_value_sha256,
        },
        draw_tables::DrawTableHeader,
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome},
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "ice-climbers-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "ice-climbers-quicknes-snapshot-checkpoint-v1";
pub const NAMED_PROGRESS_FORMAT: &str = "ice-climbers-named-progress-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "pressable_masks_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "last_mountain_or_lost_life_v1";

pub type IceClimbersTargets<M> =
    Box<dyn Fn() -> Result<IceClimbersTarget<M>, String> + Send + Sync>;

pub struct IceClimbersGame<M: NesBackend<Vec<u8>>> {
    rom_sha256: String,
    identity: String,
    targets: IceClimbersTargets<M>,
}

impl<M: NesBackend<Vec<u8>>> IceClimbersGame<M> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        targets: IceClimbersTargets<M>,
    ) -> Self {
        Self::with_genesis(
            rom,
            core,
            core_sha256,
            "ice-climbers-first-mountain-v1",
            targets,
        )
    }

    #[must_use]
    pub fn with_rooted_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        prefix: &[ButtonChord],
        targets: IceClimbersTargets<M>,
    ) -> Self {
        let mut digest = Sha256::new();
        for chord in prefix {
            digest.update([chord.buttons, chord.hold_frames]);
        }
        let genesis = format!(
            "ice-climbers-rooted-v1:prefix-sha256={:x}",
            digest.finalize()
        );
        Self::with_genesis(rom, core, core_sha256, &genesis, targets)
    }

    fn with_genesis(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        genesis: &str,
        targets: IceClimbersTargets<M>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;genesis={genesis};result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
            ),
            targets,
        }
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

#[cfg(unix)]
impl IceClimbersGame<QuickNesMachine> {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str, execution_sha256: &str) -> Self {
        let core = machine::quicknes::core_identity();
        let target_rom = rom.to_vec();
        let target_core = core_path.to_path_buf();
        let target_sha256 = core_sha256.to_owned();
        let target_execution = execution_sha256.to_owned();
        Self::with_targets(
            rom,
            &core,
            &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            Box::new(move || {
                IceClimbersTarget::from_rom_bytes_headless_with_identity(
                    &target_rom,
                    &target_core,
                    &target_sha256,
                    &target_execution,
                )
                .map_err(|error| error.to_string())
            }),
        )
    }

    #[must_use]
    pub fn new_rooted(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
        prefix: Vec<ButtonChord>,
    ) -> Self {
        let core = machine::quicknes::core_identity();
        let target_rom = rom.to_vec();
        let target_core = core_path.to_path_buf();
        let target_sha256 = core_sha256.to_owned();
        let target_execution = execution_sha256.to_owned();
        let target_prefix = prefix.clone();
        Self::with_rooted_targets(
            rom,
            &core,
            &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            &prefix,
            Box::new(move || {
                IceClimbersTarget::from_rom_bytes_headless_rooted_with_identity(
                    &target_rom,
                    &target_core,
                    &target_sha256,
                    &target_execution,
                    &target_prefix,
                )
                .map_err(|error| error.to_string())
            }),
        )
    }

    pub fn from_environment(rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let core_path = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned QuickNES core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        let execution_sha256 =
            machine::quicknes::prepared_execution_sha256(&core_path, &core_sha256)?;
        Ok(Self::new(rom, &core_path, &core_sha256, &execution_sha256))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct IceClimbersCampaignRun;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct IceClimbersCampaignEvidence {
    aggregate: IceClimbersMilestones,
    watermark: IceClimbersProgressWatermark,
    first_reached: IceClimbersMilestoneTimes,
    first_inputs: IceClimbersMilestoneInputs,
    champion_input: IceClimbersInput,
    champion_milestones: IceClimbersMilestones,
    champion_key: Option<IceClimbersProgressWatermark>,
}

pub type IceClimbersCampaignCheckpoint = CampaignCheckpoint<IceClimbersSnapshot>;
pub type IceClimbersSnapshotCheckpoint = SnapshotCheckpoint<IceClimbersSnapshot>;
pub type IceClimbersCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type IceClimbersCampaignModeReport = CampaignModeReport<ButtonChord, IceClimbersArchiveReport>;
type IceClimbersCampaignActionResult<M> = CampaignActionResult<IceClimbersGame<M>>;
type IceClimbersCampaignJobResult<M> = CampaignJobResult<IceClimbersGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("IceClimbers stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut IceClimbersMilestones,
    target: &IceClimbersTarget<M>,
) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Ok(());
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones_from_observation(observation));
    }
    Ok(())
}

fn execute_suffix<M: NesBackend<Vec<u8>>>(
    target: &mut IceClimbersTarget<M>,
    parent_milestones: IceClimbersMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop: searcher::search::rollout::RolloutStop,
) -> Result<IceClimbersCampaignJobResult<M>, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = Outcome {
        objective_reached: target.exit_kind() == ExitKind::Ok && target.won(),
        disposition: if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_terminal() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        },
    };
    let mut objective_seen = parent_outcome.objective_reached;
    if parent_outcome.disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: None,
            hang: None,
            actions,
        });
    }
    for action in suffix {
        let action_start_frame = target.observe().frame_count;
        target.apply(action);
        merge_action_milestones(&mut aggregate, target)?;
        let observations = if target.exit_kind() != ExitKind::Ok {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        };
        let raw_objective = target.exit_kind() == ExitKind::Ok && target.won();
        let objective_reached = raw_objective && !objective_seen;
        objective_seen |= raw_objective;
        let disposition = if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_terminal() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        };
        let outcome = Outcome {
            objective_reached,
            disposition,
        };
        let archivable = archive_key(target.mechanical_state()).is_some();
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
        let candidate =
            if !matches!(outcome.disposition, ExecutionDisposition::Runnable) || !archivable {
                None
            } else {
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot IceClimbers suffix")?;
                let viable = match retention {
                    RetentionPolicy::Unprobed => true,
                    RetentionPolicy::ProbeAtAdmission => {
                        return Err(
                        "IceClimbers rejects ProbeAtAdmission: ordinary admission is sufficient"
                            .into(),
                    );
                    }
                };
                Some(CampaignCandidate {
                    key: archive_key(target.mechanical_state())
                        .ok_or("IceClimbers attempted to archive a state outside gameplay")?,
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

fn reached(value: IceClimbersMilestones) -> impl Iterator<Item = (usize, usize)> {
    let mountains = 0..usize::from(value.level).min(MOUNTAINS as usize);
    let floors = 0..usize::from(value.floor).min(FLOORS as usize);
    mountains
        .map(|index| (0, index))
        .chain(floors.map(|index| (1, index)))
        .chain(value.victory.then_some((2, 0)))
        .chain(value.death.then_some((3, 0)))
}

fn time_slot(
    times: &mut IceClimbersMilestoneTimes,
    (kind, index): (usize, usize),
) -> &mut Option<u64> {
    match kind {
        0 => &mut times.first_mountains[index],
        1 => &mut times.first_floors[index],
        2 => &mut times.first_victory,
        _ => &mut times.first_death,
    }
}

fn input_slot(
    inputs: &mut IceClimbersMilestoneInputs,
    (kind, index): (usize, usize),
) -> &mut Option<IceClimbersInput> {
    match kind {
        0 => &mut inputs.first_mountains[index],
        1 => &mut inputs.first_floors[index],
        2 => &mut inputs.first_victory,
        _ => &mut inputs.first_death,
    }
}

fn update_first_inputs(
    times: &mut IceClimbersMilestoneTimes,
    inputs: &mut IceClimbersMilestoneInputs,
    value: IceClimbersMilestones,
    sequence: u64,
    input: &IceClimbersInput,
) {
    for slot in reached(value) {
        let time = time_slot(times, slot);
        if time.is_none() {
            *time = Some(sequence);
            *input_slot(inputs, slot) = Some(input.clone());
        }
    }
}

fn first_input_needed(inputs: &IceClimbersMilestoneInputs, value: IceClimbersMilestones) -> bool {
    reached(value).any(|(kind, index)| match kind {
        0 => inputs.first_mountains[index].is_none(),
        1 => inputs.first_floors[index].is_none(),
        2 => inputs.first_victory.is_none(),
        _ => inputs.first_death.is_none(),
    })
}

fn action_champion_key(
    observations: &[IceClimbersObservations],
) -> Option<IceClimbersProgressWatermark> {
    observations
        .last()
        .filter(|observation| archive_key(observation.decoded).is_some())
        .map(|observation| progress_point(observation.decoded))
}

impl<M: NesBackend<Vec<u8>>> CampaignTypes for IceClimbersGame<M> {
    type JobObservation = ();
    type Target = IceClimbersTarget<M>;
    type Action = ButtonChord;
    type Key = IceClimbersArchiveKey;
    type Milestones = IceClimbersMilestones;
    type Progress = IceClimbersProgressWatermark;
    type Snapshot = IceClimbersSnapshot;
    type Observations = IceClimbersObservations;
    type Evidence = IceClimbersCampaignEvidence;
    type ArchiveReport = IceClimbersArchiveReport;
    type Run = IceClimbersCampaignRun;
}

fn named_progress(evidence: &IceClimbersCampaignEvidence) -> serde_json::Value {
    let times = &evidence.first_reached;
    let seen = |seen: &Option<u64>| {
        seen.map_or(
            serde_json::Value::Null,
            |execution| serde_json::json!({ "execution": execution }),
        )
    };
    let mut first_seen = serde_json::Map::new();
    for (index, time) in times.first_mountains.iter().enumerate() {
        first_seen.insert(format!("mountain_{}_cleared", index + 1), seen(time));
    }
    for (index, time) in times.first_floors.iter().enumerate() {
        first_seen.insert(format!("floor_{}", index + 1), seen(time));
    }
    first_seen.insert("last_mountain".to_owned(), seen(&times.first_victory));
    serde_json::json!({ "format": NAMED_PROGRESS_FORMAT, "first_seen": first_seen })
}

impl<M: NesBackend<Vec<u8>>> Reporting for IceClimbersGame<M> {
    fn diagnostics(evidence: &IceClimbersCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "named_progress": named_progress(evidence) }))
    }

    fn write_evidence_checkpoint(
        evidence: &IceClimbersCampaignEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        postcard::to_io(evidence, out)?;
        Ok(())
    }

    fn evidence_from_checkpoint(
        bytes: &[u8],
    ) -> Result<IceClimbersCampaignEvidence, Box<dyn Error>> {
        Ok(postcard::from_bytes(bytes)?)
    }

    fn checkpoint_marks(evidence: &IceClimbersCampaignEvidence) -> usize {
        let times = &evidence.first_reached;
        times
            .first_mountains
            .iter()
            .chain(&times.first_floors)
            .chain(std::iter::once(&times.first_victory))
            .filter(|seen| seen.is_some())
            .count()
    }

    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn workload_identity_sha256(&self) -> String {
        self.rom_sha256.clone()
    }
    fn action_cost_unit(&self) -> &'static str {
        "frames"
    }
    fn execution_work_unit(&self) -> &'static str {
        "frames"
    }

    fn result_sha256(
        &self,
        result: &IceClimbersCampaignJobResult<M>,
    ) -> Result<String, Box<dyn Error>> {
        postcard_semantic_result_sha256(result)
    }

    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        postcard_value_sha256(snapshot)
    }

    fn archive_report(
        &self,
        evidence: &IceClimbersCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> IceClimbersArchiveReport {
        IceClimbersArchiveReport {
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

impl<M: NesBackend<Vec<u8>>> InputPolicy for IceClimbersGame<M> {
    fn policies(&self, _run: &IceClimbersCampaignRun) -> WorkloadPolicies {
        [
            (
                CONTROLLER_VOCABULARY_FIELD,
                CONTROLLER_VOCABULARY_IDENTIFIER,
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER),
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
    ) -> Result<IceClimbersCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&IceClimbersCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("IceClimbers stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("IceClimbers stream carries an unknown game policy".into());
        }
        Ok(IceClimbersCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &IceClimbersCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::ice_climbers::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>> TargetExecution for IceClimbersGame<M> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &IceClimbersSnapshot) -> usize {
        std::mem::size_of::<IceClimbersSnapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<IceClimbersTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut IceClimbersTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut IceClimbersTarget<M>,
        snapshot: &IceClimbersSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &IceClimbersTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut IceClimbersTarget<M>,
        action: &ButtonChord,
        aggregate: &mut IceClimbersMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(
        &self,
        target: &mut IceClimbersTarget<M>,
    ) -> Result<IceClimbersSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot IceClimbers".into())
    }

    fn execute_job(
        &self,
        _run: &Self::Run,
        target: &mut Self::Target,
        origin_snapshot: &Self::Snapshot,
        replay: &[Self::Action],
        parent_milestones: Self::Milestones,
        suffix: &[Self::Action],
        retention: RetentionPolicy,
        stop: searcher::search::rollout::RolloutStop,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(target, parent_milestones, suffix, retention, stop)
    }
}

impl<M: NesBackend<Vec<u8>>> Evaluation for IceClimbersGame<M> {
    fn execution_disposition(&self, target: &IceClimbersTarget<M>) -> ExecutionDisposition {
        if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_terminal() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        _run: &IceClimbersCampaignRun,
        target: &IceClimbersTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.won())
    }

    fn current_key(
        &self,
        target: &IceClimbersTarget<M>,
    ) -> Result<IceClimbersArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "IceClimbers current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: IceClimbersArchiveKey,
        _snapshot: &IceClimbersSnapshot,
    ) -> Result<IceClimbersArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut IceClimbersMilestones, from: IceClimbersMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &IceClimbersCampaignEvidence) -> IceClimbersMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &IceClimbersCampaignEvidence) -> IceClimbersProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut IceClimbersCampaignEvidence,
        source: &IceClimbersArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut IceClimbersCampaignEvidence,
        target: &IceClimbersTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = IceClimbersObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut IceClimbersCampaignEvidence,
        value: IceClimbersMilestones,
        input: &IceClimbersInput,
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
        evidence: &mut IceClimbersCampaignEvidence,
        action: &IceClimbersCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<IceClimbersInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let first_input_needed = first_input_needed(&evidence.first_inputs, action.milestones);
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
        source: &'a IceClimbersArchiveReport,
    ) -> &'a [crate::ice_climbers::archive::IceClimbersArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &IceClimbersArchiveReport,
    ) -> Result<IceClimbersInput, Box<dyn Error>> {
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
            .ok_or_else(|| "IceClimbers source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = IceClimbersGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = IceClimbersGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&IceClimbersCampaignRun))
                .is_err()
        );
    }

    #[test]
    fn named_progress_records_the_first_execution_of_each_mountain_and_floor() {
        let mut evidence = IceClimbersCampaignEvidence::default();
        let input = IceClimbersInput {
            actions: vec![ButtonChord::new(0x10, 4)],
        };
        let first = IceClimbersMilestones {
            level: 2,
            floor: 3,
            ..IceClimbersMilestones::default()
        };
        assert!(first_input_needed(&evidence.first_inputs, first));
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            first,
            7,
            &input,
        );
        assert!(!first_input_needed(&evidence.first_inputs, first));
        let later = IceClimbersMilestones { floor: 5, ..first };
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            later,
            9,
            &input,
        );
        let seen = &named_progress(&evidence)["first_seen"];
        assert_eq!(seen["mountain_2_cleared"]["execution"], 7);
        assert!(seen["mountain_3_cleared"].is_null());
        assert_eq!(seen["floor_3"]["execution"], 7);
        assert_eq!(seen["floor_5"]["execution"], 9);
        assert_eq!(
            IceClimbersGame::<QuickNesMachine>::checkpoint_marks(&evidence),
            7
        );
    }
}
