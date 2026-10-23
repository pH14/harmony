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
    zelda::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER, ZeldaArchiveKey,
            ZeldaArchiveReport, ZeldaMilestoneInputs, ZeldaMilestoneTimes, ZeldaMilestones,
            ZeldaProgressWatermark, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones_from_observation, progress_point,
        },
        target::{
            ButtonChord, ITEMS, MAX_HOLD_FRAMES, SWORDS, ZeldaInput, ZeldaObservations,
            ZeldaSnapshot, ZeldaTarget,
        },
    },
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "zelda-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "zelda-quicknes-snapshot-checkpoint-v1";
pub const NAMED_PROGRESS_FORMAT: &str = "zelda-named-progress-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "pressable_masks_with_start_taps_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "ending_mode_or_death_mode_v1";

pub type ZeldaTargets<M> = Box<dyn Fn() -> Result<ZeldaTarget<M>, String> + Send + Sync>;

pub struct ZeldaGame<M: NesBackend<Vec<u8>>> {
    rom_sha256: String,
    identity: String,
    targets: ZeldaTargets<M>,
}

impl<M: NesBackend<Vec<u8>>> ZeldaGame<M> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        targets: ZeldaTargets<M>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;genesis=zelda-first-overworld-screen-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
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
impl ZeldaGame<QuickNesMachine> {
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
                ZeldaTarget::from_rom_bytes_headless_with_identity(
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
pub struct ZeldaCampaignRun;

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct ZeldaCampaignEvidence {
    aggregate: ZeldaMilestones,
    watermark: ZeldaProgressWatermark,
    first_reached: ZeldaMilestoneTimes,
    first_inputs: ZeldaMilestoneInputs,
    champion_input: ZeldaInput,
    champion_milestones: ZeldaMilestones,
    champion_key: Option<ZeldaProgressWatermark>,
}

pub type ZeldaCampaignCheckpoint = CampaignCheckpoint<ZeldaSnapshot>;
pub type ZeldaSnapshotCheckpoint = SnapshotCheckpoint<ZeldaSnapshot>;
pub type ZeldaCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type ZeldaCampaignModeReport = CampaignModeReport<ButtonChord, ZeldaArchiveReport>;
type ZeldaCampaignActionResult<M> = CampaignActionResult<ZeldaGame<M>>;
type ZeldaCampaignJobResult<M> = CampaignJobResult<ZeldaGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Zelda stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut ZeldaMilestones,
    target: &ZeldaTarget<M>,
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
    target: &mut ZeldaTarget<M>,
    parent_milestones: ZeldaMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<ZeldaCampaignJobResult<M>, Box<dyn Error>> {
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
        let candidate = if !matches!(outcome.disposition, ExecutionDisposition::Runnable)
            || !archivable
        {
            None
        } else {
            let snapshot = target.snapshot().ok_or("failed to snapshot Zelda suffix")?;
            let viable = match retention {
                RetentionPolicy::Unprobed => true,
                RetentionPolicy::ProbeAtAdmission => {
                    return Err(
                        "Zelda rejects ProbeAtAdmission: ordinary admission is sufficient".into(),
                    );
                }
            };
            Some(CampaignCandidate {
                key: archive_key(target.mechanical_state())
                    .ok_or("Zelda attempted to archive a state outside gameplay")?,
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

fn reached(value: ZeldaMilestones) -> impl Iterator<Item = (usize, usize)> {
    let triforce = (0..8).filter(move |bit| value.triforce & (1 << bit) != 0);
    let swords = 0..usize::from(value.sword).min(SWORDS.len());
    let items = (0..ITEMS.len()).filter(move |bit| value.items & (1 << bit) != 0);
    triforce
        .map(|bit| (0, bit))
        .chain(swords.map(|index| (1, index)))
        .chain(items.map(|bit| (2, bit)))
        .chain(value.victory.then_some((3, 0)))
        .chain(value.death.then_some((4, 0)))
}

fn time_slot(times: &mut ZeldaMilestoneTimes, (kind, index): (usize, usize)) -> &mut Option<u64> {
    match kind {
        0 => &mut times.first_triforce[index],
        1 => &mut times.first_swords[index],
        2 => &mut times.first_items[index],
        3 => &mut times.first_victory,
        _ => &mut times.first_death,
    }
}

fn input_slot(
    inputs: &mut ZeldaMilestoneInputs,
    (kind, index): (usize, usize),
) -> &mut Option<ZeldaInput> {
    match kind {
        0 => &mut inputs.first_triforce[index],
        1 => &mut inputs.first_swords[index],
        2 => &mut inputs.first_items[index],
        3 => &mut inputs.first_victory,
        _ => &mut inputs.first_death,
    }
}

fn update_first_inputs(
    times: &mut ZeldaMilestoneTimes,
    inputs: &mut ZeldaMilestoneInputs,
    value: ZeldaMilestones,
    sequence: u64,
    input: &ZeldaInput,
) {
    for slot in reached(value) {
        let time = time_slot(times, slot);
        if time.is_none() {
            *time = Some(sequence);
            *input_slot(inputs, slot) = Some(input.clone());
        }
    }
}

fn first_input_needed(inputs: &ZeldaMilestoneInputs, value: ZeldaMilestones) -> bool {
    reached(value).any(|(kind, index)| match kind {
        0 => inputs.first_triforce[index].is_none(),
        1 => inputs.first_swords[index].is_none(),
        2 => inputs.first_items[index].is_none(),
        3 => inputs.first_victory.is_none(),
        _ => inputs.first_death.is_none(),
    })
}

fn action_champion_key(observations: &[ZeldaObservations]) -> Option<ZeldaProgressWatermark> {
    observations
        .last()
        .filter(|observation| archive_key(observation.decoded).is_some())
        .map(|observation| progress_point(observation.decoded))
}

impl<M: NesBackend<Vec<u8>>> CampaignTypes for ZeldaGame<M> {
    type JobObservation = ();
    type Target = ZeldaTarget<M>;
    type Action = ButtonChord;
    type Key = ZeldaArchiveKey;
    type Milestones = ZeldaMilestones;
    type Progress = ZeldaProgressWatermark;
    type Snapshot = ZeldaSnapshot;
    type Observations = ZeldaObservations;
    type Evidence = ZeldaCampaignEvidence;
    type ArchiveReport = ZeldaArchiveReport;
    type Run = ZeldaCampaignRun;
}

fn named_progress(evidence: &ZeldaCampaignEvidence) -> serde_json::Value {
    let times = &evidence.first_reached;
    let seen = |seen: &Option<u64>| {
        seen.map_or(
            serde_json::Value::Null,
            |execution| serde_json::json!({ "execution": execution }),
        )
    };
    let mut first_seen = serde_json::Map::new();
    for (index, time) in times.first_triforce.iter().enumerate() {
        first_seen.insert(format!("triforce_{}", index + 1), seen(time));
    }
    for (name, time) in SWORDS.iter().zip(&times.first_swords) {
        first_seen.insert((*name).to_owned(), seen(time));
    }
    for (name, time) in ITEMS.iter().zip(&times.first_items) {
        first_seen.insert((*name).to_owned(), seen(time));
    }
    first_seen.insert("ending".to_owned(), seen(&times.first_victory));
    serde_json::json!({ "format": NAMED_PROGRESS_FORMAT, "first_seen": first_seen })
}

impl<M: NesBackend<Vec<u8>>> Reporting for ZeldaGame<M> {
    fn diagnostics(evidence: &ZeldaCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "named_progress": named_progress(evidence) }))
    }

    fn write_evidence_checkpoint(
        evidence: &ZeldaCampaignEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        postcard::to_io(evidence, out)?;
        Ok(())
    }

    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<ZeldaCampaignEvidence, Box<dyn Error>> {
        Ok(postcard::from_bytes(bytes)?)
    }

    fn checkpoint_marks(evidence: &ZeldaCampaignEvidence) -> usize {
        let times = &evidence.first_reached;
        times
            .first_triforce
            .iter()
            .chain(&times.first_swords)
            .chain(&times.first_items)
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

    fn result_sha256(&self, result: &ZeldaCampaignJobResult<M>) -> Result<String, Box<dyn Error>> {
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
        evidence: &ZeldaCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> ZeldaArchiveReport {
        ZeldaArchiveReport {
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

impl<M: NesBackend<Vec<u8>>> InputPolicy for ZeldaGame<M> {
    fn policies(&self, _run: &ZeldaCampaignRun) -> WorkloadPolicies {
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
    ) -> Result<ZeldaCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&ZeldaCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Zelda stream {field} policy is not recognized").into());
                }
            }
            return Err("Zelda stream carries an unknown game policy".into());
        }
        Ok(ZeldaCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &ZeldaCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::zelda::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>> TargetExecution for ZeldaGame<M> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &ZeldaSnapshot) -> usize {
        std::mem::size_of::<ZeldaSnapshot>().saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<ZeldaTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut ZeldaTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut ZeldaTarget<M>,
        snapshot: &ZeldaSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &ZeldaTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut ZeldaTarget<M>,
        action: &ButtonChord,
        aggregate: &mut ZeldaMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(&self, target: &mut ZeldaTarget<M>) -> Result<ZeldaSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Zelda".into())
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

impl<M: NesBackend<Vec<u8>>> Evaluation for ZeldaGame<M> {
    fn execution_disposition(&self, target: &ZeldaTarget<M>) -> ExecutionDisposition {
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
        _run: &ZeldaCampaignRun,
        target: &ZeldaTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.won())
    }

    fn current_key(&self, target: &ZeldaTarget<M>) -> Result<ZeldaArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "Zelda current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: ZeldaArchiveKey,
        _snapshot: &ZeldaSnapshot,
    ) -> Result<ZeldaArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut ZeldaMilestones, from: ZeldaMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &ZeldaCampaignEvidence) -> ZeldaMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &ZeldaCampaignEvidence) -> ZeldaProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut ZeldaCampaignEvidence,
        source: &ZeldaArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut ZeldaCampaignEvidence,
        target: &ZeldaTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = ZeldaObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut ZeldaCampaignEvidence,
        value: ZeldaMilestones,
        input: &ZeldaInput,
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
        evidence: &mut ZeldaCampaignEvidence,
        action: &ZeldaCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<ZeldaInput, Box<dyn Error>>,
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
        source: &'a ZeldaArchiveReport,
    ) -> &'a [crate::zelda::archive::ZeldaArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &ZeldaArchiveReport) -> Result<ZeldaInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Zelda source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = ZeldaGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = ZeldaGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&ZeldaCampaignRun))
                .is_err()
        );
    }
}
