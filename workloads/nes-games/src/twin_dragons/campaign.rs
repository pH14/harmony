// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use sha2::{Digest, Sha256};

use crate::{
    chord::{CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD},
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
    twin_dragons::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER,
            TwinDragonsArchiveKey, TwinDragonsArchiveReport, TwinDragonsChampionKey,
            TwinDragonsMilestoneInputs, TwinDragonsMilestoneTimes, TwinDragonsMilestones,
            TwinDragonsProgressWatermark, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones_from_observation,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, TwinDragonsInput, TwinDragonsObservations,
            TwinDragonsSnapshot, TwinDragonsTarget,
        },
    },
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "twin-dragons-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "twin-dragons-quicknes-snapshot-checkpoint-v2";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_no_start_select_v2";
const SOURCE_COMMIT: &str = "bcf27d0c5117a2aac19e6c33bdb3c776ada069fb";
const TERMINAL_POLICY_IDENTIFIER: &str = "final_goal_win_or_zero_health_death_v1";

pub type TwinDragonsTargets<M> =
    Box<dyn Fn() -> Result<TwinDragonsTarget<M>, String> + Send + Sync>;

pub struct TwinDragonsGame<M: NesBackend<Vec<u8>>> {
    rom_sha256: String,
    identity: String,
    targets: TwinDragonsTargets<M>,
}

impl<M: NesBackend<Vec<u8>>> TwinDragonsGame<M> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        targets: TwinDragonsTargets<M>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;source=Garydos/twin-dragons-nes@{SOURCE_COMMIT};rom=silent-effects;genesis=twin-dragons-level-1-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
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
impl TwinDragonsGame<QuickNesMachine> {
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
                TwinDragonsTarget::from_rom_bytes_headless_with_identity(
                    &target_rom,
                    &target_core,
                    &target_sha256,
                    &target_execution,
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
pub struct TwinDragonsCampaignRun;

#[derive(Clone, Default)]
pub struct TwinDragonsCampaignEvidence {
    aggregate: TwinDragonsMilestones,
    watermark: TwinDragonsProgressWatermark,
    first_reached: TwinDragonsMilestoneTimes,
    first_inputs: TwinDragonsMilestoneInputs,
    champion_input: TwinDragonsInput,
    champion_milestones: TwinDragonsMilestones,
    champion_key: Option<TwinDragonsChampionKey>,
}

pub type TwinDragonsCampaignCheckpoint = CampaignCheckpoint<TwinDragonsSnapshot>;
pub type TwinDragonsSnapshotCheckpoint = SnapshotCheckpoint<TwinDragonsSnapshot>;
pub type TwinDragonsCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type TwinDragonsCampaignModeReport = CampaignModeReport<ButtonChord, TwinDragonsArchiveReport>;
type TwinDragonsCampaignActionResult<M> = CampaignActionResult<TwinDragonsGame<M>>;
type TwinDragonsCampaignJobResult<M> = CampaignJobResult<TwinDragonsGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Twin Dragons stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut TwinDragonsMilestones,
    target: &TwinDragonsTarget<M>,
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
    target: &mut TwinDragonsTarget<M>,
    parent_milestones: TwinDragonsMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop: searcher::search::rollout::RolloutStop,
) -> Result<TwinDragonsCampaignJobResult<M>, Box<dyn Error>> {
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
                    .ok_or("failed to snapshot Twin Dragons suffix")?;
                let viable = match retention {
                    RetentionPolicy::Unprobed => true,
                    RetentionPolicy::ProbeAtAdmission => {
                        return Err(
                        "Twin Dragons rejects ProbeAtAdmission: ordinary admission is sufficient"
                            .into(),
                    );
                    }
                };
                Some(CampaignCandidate {
                    key: archive_key(target.mechanical_state())
                        .ok_or("Twin Dragons attempted to archive a state outside gameplay")?,
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
        if outcome.should_stop(stop) {
            break;
        }
    }
    Ok(CampaignJobResult {
        preparation_failure: None,
        hang: None,
        actions,
    })
}

fn update_first_inputs(
    times: &mut TwinDragonsMilestoneTimes,
    inputs: &mut TwinDragonsMilestoneInputs,
    value: TwinDragonsMilestones,
    sequence: u64,
    input: &TwinDragonsInput,
) {
    for level in 0..usize::from(value.levels_cleared).min(times.first_clears.len()) {
        if times.first_clears[level].is_none() {
            times.first_clears[level] = Some(sequence);
            inputs.first_clears[level] = Some(input.clone());
        }
    }
    if value.death && times.first_death.is_none() {
        times.first_death = Some(sequence);
        inputs.first_death = Some(input.clone());
    }
}

fn first_input_needed(inputs: &TwinDragonsMilestoneInputs, value: TwinDragonsMilestones) -> bool {
    inputs
        .first_clears
        .iter()
        .take(usize::from(value.levels_cleared))
        .any(Option::is_none)
        || (value.death && inputs.first_death.is_none())
}

fn action_champion_key(observations: &[TwinDragonsObservations]) -> Option<TwinDragonsChampionKey> {
    observations.last().and_then(|observation| {
        archive_key(observation.decoded).map(TwinDragonsArchiveKey::champion_key)
    })
}

impl<M: NesBackend<Vec<u8>>> CampaignTypes for TwinDragonsGame<M> {
    type JobObservation = ();
    type Target = TwinDragonsTarget<M>;
    type Action = ButtonChord;
    type Key = TwinDragonsArchiveKey;
    type Milestones = TwinDragonsMilestones;
    type Progress = TwinDragonsProgressWatermark;
    type Snapshot = TwinDragonsSnapshot;
    type Observations = TwinDragonsObservations;
    type Evidence = TwinDragonsCampaignEvidence;
    type ArchiveReport = TwinDragonsArchiveReport;
    type Run = TwinDragonsCampaignRun;
}

impl<M: NesBackend<Vec<u8>>> Reporting for TwinDragonsGame<M> {
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
        result: &TwinDragonsCampaignJobResult<M>,
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
        evidence: &TwinDragonsCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> TwinDragonsArchiveReport {
        TwinDragonsArchiveReport {
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

impl<M: NesBackend<Vec<u8>>> InputPolicy for TwinDragonsGame<M> {
    fn policies(&self, _run: &TwinDragonsCampaignRun) -> WorkloadPolicies {
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
    ) -> Result<TwinDragonsCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&TwinDragonsCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("Twin Dragons stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("Twin Dragons stream carries an unknown game policy".into());
        }
        Ok(TwinDragonsCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &TwinDragonsCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::twin_dragons::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>> TargetExecution for TwinDragonsGame<M> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &TwinDragonsSnapshot) -> usize {
        std::mem::size_of::<TwinDragonsSnapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<TwinDragonsTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut TwinDragonsTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut TwinDragonsTarget<M>,
        snapshot: &TwinDragonsSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &TwinDragonsTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut TwinDragonsTarget<M>,
        action: &ButtonChord,
        aggregate: &mut TwinDragonsMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(
        &self,
        target: &mut TwinDragonsTarget<M>,
    ) -> Result<TwinDragonsSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Twin Dragons".into())
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

impl<M: NesBackend<Vec<u8>>> Evaluation for TwinDragonsGame<M> {
    fn execution_disposition(&self, target: &TwinDragonsTarget<M>) -> ExecutionDisposition {
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
        _run: &TwinDragonsCampaignRun,
        target: &TwinDragonsTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.won())
    }

    fn current_key(
        &self,
        target: &TwinDragonsTarget<M>,
    ) -> Result<TwinDragonsArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "Twin Dragons current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: TwinDragonsArchiveKey,
        _snapshot: &TwinDragonsSnapshot,
    ) -> Result<TwinDragonsArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut TwinDragonsMilestones, from: TwinDragonsMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &TwinDragonsCampaignEvidence) -> TwinDragonsMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &TwinDragonsCampaignEvidence) -> TwinDragonsProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut TwinDragonsCampaignEvidence,
        source: &TwinDragonsArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut TwinDragonsCampaignEvidence,
        target: &TwinDragonsTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = TwinDragonsObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut TwinDragonsCampaignEvidence,
        value: TwinDragonsMilestones,
        input: &TwinDragonsInput,
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
        evidence: &mut TwinDragonsCampaignEvidence,
        action: &TwinDragonsCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<TwinDragonsInput, Box<dyn Error>>,
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
        source: &'a TwinDragonsArchiveReport,
    ) -> &'a [crate::twin_dragons::archive::TwinDragonsArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &TwinDragonsArchiveReport,
    ) -> Result<TwinDragonsInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Twin Dragons source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = TwinDragonsGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = TwinDragonsGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&TwinDragonsCampaignRun))
                .is_err()
        );
    }
}
