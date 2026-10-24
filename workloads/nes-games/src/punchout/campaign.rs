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
    nes_backend::{NesBackend, SnapshotState},
    punchout::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, PunchOutArchiveKey, PunchOutArchiveReport,
            PunchOutChampionKey, PunchOutMilestoneInputs, PunchOutMilestoneTimes,
            PunchOutMilestones, PunchOutProgressWatermark, REPLACEMENT_IDENTIFIER, archive_key,
            chord_time, merge_milestones, merge_progress_watermark, milestone_key,
            milestones_from_observation,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, OPPONENTS, PunchOutInput, PunchOutObservations,
            PunchOutSnapshot, PunchOutTarget,
        },
    },
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

pub const CAMPAIGN_STREAM_FORMAT: &str = "punchout-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "punchout-quicknes-snapshot-checkpoint-v2";
pub const NAMED_PROGRESS_FORMAT: &str = "punchout-named-progress-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str =
    "idle_body_face_punches_dodges_block_with_start_taps_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "final_opponent_defeated_or_mac_knocked_down_or_lost_v1";

pub type PunchOutTargets<M> = Box<dyn Fn() -> Result<PunchOutTarget<M>, String> + Send + Sync>;

pub struct PunchOutGame<M: NesBackend<Vec<u8>>> {
    rom_sha256: String,
    identity: String,
    targets: PunchOutTargets<M>,
}

impl<M: NesBackend<Vec<u8>>> PunchOutGame<M> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        targets: PunchOutTargets<M>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;genesis=punchout-glass-joe-round-1-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
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
impl PunchOutGame<QuickNesMachine> {
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
                PunchOutTarget::from_rom_bytes_headless_with_identity(
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
pub struct PunchOutCampaignRun;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct PunchOutCampaignEvidence {
    aggregate: PunchOutMilestones,
    watermark: PunchOutProgressWatermark,
    first_reached: PunchOutMilestoneTimes,
    first_inputs: PunchOutMilestoneInputs,
    champion_input: PunchOutInput,
    champion_milestones: PunchOutMilestones,
    champion_key: Option<PunchOutChampionKey>,
}

pub type PunchOutCampaignCheckpoint = CampaignCheckpoint<PunchOutSnapshot>;
pub type PunchOutSnapshotCheckpoint = SnapshotCheckpoint<PunchOutSnapshot>;
pub type PunchOutCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type PunchOutCampaignModeReport = CampaignModeReport<ButtonChord, PunchOutArchiveReport>;
type PunchOutCampaignActionResult<M> = CampaignActionResult<PunchOutGame<M>>;
type PunchOutCampaignJobResult<M> = CampaignJobResult<PunchOutGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Punch-Out!! stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut PunchOutMilestones,
    target: &PunchOutTarget<M>,
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
    target: &mut PunchOutTarget<M>,
    parent_milestones: PunchOutMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop: searcher::search::rollout::RolloutStop,
) -> Result<PunchOutCampaignJobResult<M>, Box<dyn Error>> {
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
                    .ok_or("failed to snapshot Punch-Out!! suffix")?;
                let viable = match retention {
                    RetentionPolicy::Unprobed => true,
                    RetentionPolicy::ProbeAtAdmission => {
                        return Err(
                        "Punch-Out!! rejects ProbeAtAdmission: ordinary admission is sufficient"
                            .into(),
                    );
                    }
                };
                Some(CampaignCandidate {
                    key: archive_key(target.mechanical_state())
                        .ok_or("Punch-Out!! attempted to archive a state outside gameplay")?,
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

fn update_first_inputs(
    times: &mut PunchOutMilestoneTimes,
    inputs: &mut PunchOutMilestoneInputs,
    value: PunchOutMilestones,
    sequence: u64,
    input: &PunchOutInput,
) {
    for opponent in 0..usize::from(value.defeated).min(times.first_defeats.len()) {
        if times.first_defeats[opponent].is_none() {
            times.first_defeats[opponent] = Some(sequence);
            inputs.first_defeats[opponent] = Some(input.clone());
        }
    }
    if value.death && times.first_death.is_none() {
        times.first_death = Some(sequence);
        inputs.first_death = Some(input.clone());
    }
}

fn first_input_needed(inputs: &PunchOutMilestoneInputs, value: PunchOutMilestones) -> bool {
    inputs
        .first_defeats
        .iter()
        .take(usize::from(value.defeated))
        .any(Option::is_none)
        || (value.death && inputs.first_death.is_none())
}

fn action_champion_key(observations: &[PunchOutObservations]) -> Option<PunchOutChampionKey> {
    observations.last().and_then(|observation| {
        archive_key(observation.decoded).map(PunchOutArchiveKey::champion_key)
    })
}

impl<M: NesBackend<Vec<u8>>> CampaignTypes for PunchOutGame<M> {
    type JobObservation = ();
    type Target = PunchOutTarget<M>;
    type Action = ButtonChord;
    type Key = PunchOutArchiveKey;
    type Milestones = PunchOutMilestones;
    type Progress = PunchOutProgressWatermark;
    type Snapshot = PunchOutSnapshot;
    type Observations = PunchOutObservations;
    type Evidence = PunchOutCampaignEvidence;
    type ArchiveReport = PunchOutArchiveReport;
    type Run = PunchOutCampaignRun;
}

fn named_progress(evidence: &PunchOutCampaignEvidence) -> serde_json::Value {
    let first_seen: serde_json::Map<String, serde_json::Value> = OPPONENTS
        .iter()
        .zip(evidence.first_reached.first_defeats.iter())
        .map(|(name, seen)| {
            (
                format!("{name}_defeated"),
                seen.map_or(
                    serde_json::Value::Null,
                    |execution| serde_json::json!({ "execution": execution }),
                ),
            )
        })
        .collect();
    serde_json::json!({ "format": NAMED_PROGRESS_FORMAT, "first_seen": first_seen })
}

impl<M: NesBackend<Vec<u8>>> Reporting for PunchOutGame<M> {
    fn diagnostics(evidence: &PunchOutCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "named_progress": named_progress(evidence) }))
    }

    fn write_evidence_checkpoint(
        evidence: &PunchOutCampaignEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        postcard::to_io(evidence, out)?;
        Ok(())
    }

    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<PunchOutCampaignEvidence, Box<dyn Error>> {
        Ok(postcard::from_bytes(bytes)?)
    }

    fn checkpoint_marks(evidence: &PunchOutCampaignEvidence) -> usize {
        evidence
            .first_reached
            .first_defeats
            .iter()
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
        result: &PunchOutCampaignJobResult<M>,
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
        evidence: &PunchOutCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> PunchOutArchiveReport {
        PunchOutArchiveReport {
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

impl<M: NesBackend<Vec<u8>>> InputPolicy for PunchOutGame<M> {
    fn policies(&self, _run: &PunchOutCampaignRun) -> WorkloadPolicies {
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
    ) -> Result<PunchOutCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&PunchOutCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("Punch-Out!! stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("Punch-Out!! stream carries an unknown game policy".into());
        }
        Ok(PunchOutCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &PunchOutCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::punchout::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>> TargetExecution for PunchOutGame<M> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &PunchOutSnapshot) -> usize {
        std::mem::size_of::<PunchOutSnapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<PunchOutTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut PunchOutTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut PunchOutTarget<M>,
        snapshot: &PunchOutSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &PunchOutTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut PunchOutTarget<M>,
        action: &ButtonChord,
        aggregate: &mut PunchOutMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(&self, target: &mut PunchOutTarget<M>) -> Result<PunchOutSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Punch-Out!!".into())
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
        execute_suffix(
            target,
            parent_milestones,
            suffix,
            retention,
            stop,
        )
    }
}

impl<M: NesBackend<Vec<u8>>> Evaluation for PunchOutGame<M> {
    fn execution_disposition(&self, target: &PunchOutTarget<M>) -> ExecutionDisposition {
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
        _run: &PunchOutCampaignRun,
        target: &PunchOutTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.won())
    }

    fn current_key(
        &self,
        target: &PunchOutTarget<M>,
    ) -> Result<PunchOutArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "Punch-Out!! current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: PunchOutArchiveKey,
        _snapshot: &PunchOutSnapshot,
    ) -> Result<PunchOutArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut PunchOutMilestones, from: PunchOutMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &PunchOutCampaignEvidence) -> PunchOutMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &PunchOutCampaignEvidence) -> PunchOutProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut PunchOutCampaignEvidence,
        source: &PunchOutArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut PunchOutCampaignEvidence,
        target: &PunchOutTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = PunchOutObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut PunchOutCampaignEvidence,
        value: PunchOutMilestones,
        input: &PunchOutInput,
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
        evidence: &mut PunchOutCampaignEvidence,
        action: &PunchOutCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<PunchOutInput, Box<dyn Error>>,
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
        source: &'a PunchOutArchiveReport,
    ) -> &'a [crate::punchout::archive::PunchOutArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &PunchOutArchiveReport,
    ) -> Result<PunchOutInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Punch-Out!! source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = PunchOutGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = PunchOutGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&PunchOutCampaignRun))
                .is_err()
        );
    }
}
