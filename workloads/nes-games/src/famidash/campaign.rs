// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::path::Path;

#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use sha2::{Digest, Sha256};

use crate::{
    chord::{CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD},
    famidash::{
        archive::{
            DURATION_IDENTIFIER, FamidashArchiveKey, FamidashArchiveReport, FamidashChampionKey,
            FamidashMilestoneInputs, FamidashMilestoneTimes, FamidashMilestones,
            FamidashProgressWatermark, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER, archive_key,
            chord_time, merge_milestones, merge_progress_watermark, milestone_key,
            milestones_from_observation,
        },
        levels::Stage,
        target::{
            ButtonChord, FamidashInput, FamidashObservations, FamidashSnapshot, FamidashTarget,
            MAX_HOLD_FRAMES,
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

pub const CAMPAIGN_STREAM_FORMAT: &str = "famidash-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "famidash-quicknes-snapshot-checkpoint-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "a_only_no_start_select_v1";
const RELEASE: &str = "tfdsoft/famidash@1.2.8";
const TERMINAL_POLICY_IDENTIFIER: &str =
    "dash_complete_win_or_death_bit_or_scroll_past_level_end_v2";

pub type FamidashTargets<M> = Box<dyn Fn() -> Result<FamidashTarget<M>, String> + Send + Sync>;

pub struct FamidashGame<M: NesBackend<Vec<u8>>, const GROUP: usize = 0> {
    rom_sha256: String,
    identity: String,
    targets: FamidashTargets<M>,
}

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> FamidashGame<M, GROUP> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        stage: Stage,
        targets: FamidashTargets<M>,
    ) -> Self {
        let genesis = format!(
            "famidash-{}-level-{}-to-{}-v1",
            stage.side.id(),
            stage.level,
            stage.goal
        );
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;release={RELEASE};genesis={genesis};result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
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
impl FamidashGame<QuickNesMachine> {
    #[must_use]
    pub fn new(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
        stage: Stage,
    ) -> Self {
        let core = machine::quicknes::core_identity();
        let target_rom = rom.to_vec();
        let target_core = core_path.to_path_buf();
        let target_sha256 = core_sha256.to_owned();
        let target_execution = execution_sha256.to_owned();
        Self::with_targets(
            rom,
            &core,
            &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            stage,
            Box::new(move || {
                FamidashTarget::from_rom_bytes_headless_with_identity(
                    &target_rom,
                    &target_core,
                    &target_sha256,
                    &target_execution,
                    stage,
                )
                .map_err(|error| error.to_string())
            }),
        )
    }
}

#[derive(Clone, Copy, Debug)]
pub struct FamidashCampaignRun;

#[derive(Clone, Default)]
pub struct FamidashCampaignEvidence {
    aggregate: FamidashMilestones,
    watermark: FamidashProgressWatermark,
    first_reached: FamidashMilestoneTimes,
    first_inputs: FamidashMilestoneInputs,
    champion_input: FamidashInput,
    champion_milestones: FamidashMilestones,
    champion_key: Option<FamidashChampionKey>,
}

pub type FamidashCampaignCheckpoint = CampaignCheckpoint<FamidashSnapshot>;
pub type FamidashSnapshotCheckpoint = SnapshotCheckpoint<FamidashSnapshot>;
pub type FamidashCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type FamidashCampaignModeReport = CampaignModeReport<ButtonChord, FamidashArchiveReport>;
type FamidashCampaignActionResult<M, const GROUP: usize> =
    CampaignActionResult<FamidashGame<M, GROUP>>;
type FamidashCampaignJobResult<M, const GROUP: usize> = CampaignJobResult<FamidashGame<M, GROUP>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Famidash stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut FamidashMilestones,
    target: &FamidashTarget<M>,
) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Ok(());
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones_from_observation(observation));
    }
    Ok(())
}

fn execute_suffix<M: NesBackend<Vec<u8>>, const GROUP: usize>(
    target: &mut FamidashTarget<M>,
    parent_milestones: FamidashMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<FamidashCampaignJobResult<M, GROUP>, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = Outcome {
        objective_reached: target.exit_kind() == ExitKind::Ok && target.finished(),
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
        let raw_objective = target.exit_kind() == ExitKind::Ok && target.finished();
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
        let gameplay_valid = target.mechanical_state().gameplay.is_some();
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
            if !matches!(outcome.disposition, ExecutionDisposition::Runnable) || !gameplay_valid {
                None
            } else {
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot Famidash suffix")?;
                let viable = match retention {
                    RetentionPolicy::Unprobed => true,
                    RetentionPolicy::ProbeAtAdmission => {
                        return Err(
                            "Famidash rejects ProbeAtAdmission: ordinary admission is sufficient"
                                .into(),
                        );
                    }
                };
                Some(CampaignCandidate {
                    key: archive_key(target.mechanical_state())
                        .ok_or("Famidash attempted to archive a state outside gameplay")?,
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
    Ok(CampaignJobResult {
        preparation_failure: None,
        actions,
    })
}

fn update_first_inputs(
    times: &mut FamidashMilestoneTimes,
    inputs: &mut FamidashMilestoneInputs,
    value: FamidashMilestones,
    sequence: u64,
    input: &FamidashInput,
) {
    if value.complete && times.first_complete.is_none() {
        times.first_complete = Some(sequence);
        inputs.first_complete = Some(input.clone());
    }
    if value.death && times.first_death.is_none() {
        times.first_death = Some(sequence);
        inputs.first_death = Some(input.clone());
    }
}

fn first_input_needed(inputs: &FamidashMilestoneInputs, value: FamidashMilestones) -> bool {
    (value.complete && inputs.first_complete.is_none())
        || (value.death && inputs.first_death.is_none())
}

fn action_champion_key(observations: &[FamidashObservations]) -> Option<FamidashChampionKey> {
    observations.last().and_then(|observation| {
        archive_key(observation.decoded).map(FamidashArchiveKey::champion_key)
    })
}

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> CampaignTypes for FamidashGame<M, GROUP> {
    type JobObservation = ();
    type Target = FamidashTarget<M>;
    type Action = ButtonChord;
    type Key = FamidashArchiveKey;
    type Milestones = FamidashMilestones;
    type Progress = FamidashProgressWatermark;
    type Snapshot = FamidashSnapshot;
    type Observations = FamidashObservations;
    type Evidence = FamidashCampaignEvidence;
    type ArchiveReport = FamidashArchiveReport;
    type Run = FamidashCampaignRun;
}

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> Reporting for FamidashGame<M, GROUP> {
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
        result: &FamidashCampaignJobResult<M, GROUP>,
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
        evidence: &FamidashCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> FamidashArchiveReport {
        FamidashArchiveReport {
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

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> InputPolicy for FamidashGame<M, GROUP> {
    fn policies(&self, _run: &FamidashCampaignRun) -> WorkloadPolicies {
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
    ) -> Result<FamidashCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&FamidashCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Famidash stream {field} policy is not recognized").into());
                }
            }
            return Err("Famidash stream carries an unknown game policy".into());
        }
        Ok(FamidashCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &FamidashCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::famidash::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> TargetExecution for FamidashGame<M, GROUP> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &FamidashSnapshot) -> usize {
        std::mem::size_of::<FamidashSnapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<FamidashTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut FamidashTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut FamidashTarget<M>,
        snapshot: &FamidashSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &FamidashTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut FamidashTarget<M>,
        action: &ButtonChord,
        aggregate: &mut FamidashMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(&self, target: &mut FamidashTarget<M>) -> Result<FamidashSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Famidash".into())
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
        stop_rollout_on_objective: bool,
    ) -> Result<CampaignJobResult<Self>, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(
            target,
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
        )
    }
}

impl<M: NesBackend<Vec<u8>>, const GROUP: usize> Evaluation for FamidashGame<M, GROUP> {
    fn execution_disposition(&self, target: &FamidashTarget<M>) -> ExecutionDisposition {
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
        _run: &FamidashCampaignRun,
        target: &FamidashTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.finished())
    }

    fn current_key(
        &self,
        target: &FamidashTarget<M>,
    ) -> Result<FamidashArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "Famidash current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: FamidashArchiveKey,
        _snapshot: &FamidashSnapshot,
    ) -> Result<FamidashArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut FamidashMilestones, from: FamidashMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &FamidashCampaignEvidence) -> FamidashMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &FamidashCampaignEvidence) -> FamidashProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut FamidashCampaignEvidence,
        source: &FamidashArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut FamidashCampaignEvidence,
        target: &FamidashTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = FamidashObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut FamidashCampaignEvidence,
        value: FamidashMilestones,
        input: &FamidashInput,
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
        evidence: &mut FamidashCampaignEvidence,
        action: &FamidashCampaignActionResult<M, GROUP>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<FamidashInput, Box<dyn Error>>,
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
        source: &'a FamidashArchiveReport,
    ) -> &'a [crate::famidash::archive::FamidashArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &FamidashArchiveReport,
    ) -> Result<FamidashInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Famidash source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::famidash::levels::Side;

    fn game(execution: &str, stage: Stage) -> FamidashGame<QuickNesMachine> {
        FamidashGame::new(&[0], Path::new("core.so"), "core", execution, stage)
    }

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let stage = Stage::through_group(Side::Main, 0).unwrap();
        let first = game("first", stage);
        let second = game("second", stage);
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&FamidashCampaignRun))
                .is_err()
        );
    }

    #[test]
    fn the_policy_names_the_side_start_and_goal() {
        let main = game("first", Stage::through_group(Side::Main, 0).unwrap());
        let b = game("first", Stage::single(Side::B, 4).unwrap());
        assert!(
            main.emulator_identity()
                .contains("genesis=famidash-main-level-0-to-21-v1")
        );
        assert!(
            b.emulator_identity()
                .contains("genesis=famidash-b-level-4-to-4-v1")
        );
        assert!(
            b.resolve_recorded(&main.policies(&FamidashCampaignRun))
                .is_err()
        );
    }
}
