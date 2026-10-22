// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(unix)]
use std::path::Path;
use std::{collections::BTreeMap, error::Error, path::PathBuf};

#[cfg(unix)]
use machine::quicknes::QuickNesMachine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    castlevania::{
        archive::{
            CastlevaniaArchiveKey, CastlevaniaArchiveReport, CastlevaniaChampionKey,
            CastlevaniaMilestoneInputs, CastlevaniaMilestoneTimes, CastlevaniaMilestones,
            CastlevaniaProgressWatermark, DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER,
            REPLACEMENT_IDENTIFIER, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones_from_observation,
        },
        progress::{FirstSeen, NamedProgress, is_route_milestone},
        target::{
            ButtonChord, CastlevaniaInput, CastlevaniaObservations, CastlevaniaSnapshot,
            CastlevaniaTarget, MAX_HOLD_FRAMES,
        },
    },
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
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "castlevania-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "castlevania-quicknes-snapshot-checkpoint-v2";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "castlevania_twenty_held_chords_no_start_select_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "ending_or_lost_life_v1";

pub type CastlevaniaTargets<M> =
    Box<dyn Fn() -> Result<CastlevaniaTarget<M>, String> + Send + Sync>;

pub struct CastlevaniaGame<M: NesBackend<Vec<u8>>> {
    rom_sha256: String,
    identity: String,
    targets: CastlevaniaTargets<M>,
    milestone_input_dir: Option<PathBuf>,
}

impl<M: NesBackend<Vec<u8>>> CastlevaniaGame<M> {
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        targets: CastlevaniaTargets<M>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!(
                "{core};state=ppu-unused2-zero-v1;genesis=castlevania-courtyard-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
            ),
            targets,
            milestone_input_dir: None,
        }
    }

    #[must_use]
    pub fn with_milestone_input_dir(mut self, directory: PathBuf) -> Self {
        self.milestone_input_dir = Some(directory);
        self
    }

    fn publish_inputs(
        &self,
        names: impl IntoIterator<Item = String>,
        input: &CastlevaniaInput,
    ) -> Result<(), Box<dyn Error>> {
        let Some(directory) = &self.milestone_input_dir else {
            return Ok(());
        };
        let mut bytes = None;
        for name in names {
            std::fs::create_dir_all(directory)?;
            let path = directory.join(format!("{name}.json"));
            let temporary = path.with_extension("json.tmp");
            if bytes.is_none() {
                bytes = Some(serde_json::to_vec(input)?);
            }
            std::fs::write(&temporary, bytes.as_deref().unwrap_or_default())?;
            std::fs::rename(temporary, path)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

#[cfg(unix)]
impl CastlevaniaGame<QuickNesMachine> {
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
                CastlevaniaTarget::from_rom_bytes_headless_with_identity(
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
pub struct CastlevaniaCampaignRun;

#[derive(Clone, Default)]
pub struct CastlevaniaCampaignEvidence {
    named_progress: NamedProgress,
    aggregate: CastlevaniaMilestones,
    watermark: CastlevaniaProgressWatermark,
    first_reached: CastlevaniaMilestoneTimes,
    first_inputs: CastlevaniaMilestoneInputs,
    champion_input: CastlevaniaInput,
    champion_milestones: CastlevaniaMilestones,
    champion_key: Option<CastlevaniaChampionKey>,
}

#[derive(Deserialize, Serialize)]
struct CastlevaniaEvidenceCheckpoint {
    first_seen: Vec<(String, Option<FirstSeen>)>,
    aggregate: CastlevaniaMilestones,
    watermark: CastlevaniaProgressWatermark,
    first_reached: CastlevaniaMilestoneTimes,
    first_inputs: CastlevaniaMilestoneInputs,
    champion_input: CastlevaniaInput,
    champion_milestones: CastlevaniaMilestones,
    champion_key: Option<CastlevaniaChampionKey>,
}

pub type CastlevaniaCampaignCheckpoint = CampaignCheckpoint<CastlevaniaSnapshot>;
pub type CastlevaniaSnapshotCheckpoint = SnapshotCheckpoint<CastlevaniaSnapshot>;
pub type CastlevaniaCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type CastlevaniaCampaignModeReport = CampaignModeReport<ButtonChord, CastlevaniaArchiveReport>;
type CastlevaniaCampaignActionResult<M> = CampaignActionResult<CastlevaniaGame<M>>;
type CastlevaniaCampaignJobResult<M> = CampaignJobResult<CastlevaniaGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Castlevania stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>>>(
    aggregate: &mut CastlevaniaMilestones,
    target: &CastlevaniaTarget<M>,
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
    target: &mut CastlevaniaTarget<M>,
    parent_milestones: CastlevaniaMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<CastlevaniaCampaignJobResult<M>, Box<dyn Error>> {
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
        let candidate =
            if !matches!(outcome.disposition, ExecutionDisposition::Runnable) || !archivable {
                None
            } else {
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot Castlevania suffix")?;
                let viable = match retention {
                    RetentionPolicy::Unprobed => true,
                    RetentionPolicy::ProbeAtAdmission => {
                        return Err(
                        "Castlevania rejects ProbeAtAdmission: ordinary admission is sufficient"
                            .into(),
                    );
                    }
                };
                Some(CampaignCandidate {
                    key: archive_key(target.mechanical_state())
                        .ok_or("Castlevania attempted to archive a state outside gameplay")?,
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
    times: &mut CastlevaniaMilestoneTimes,
    inputs: &mut CastlevaniaMilestoneInputs,
    value: CastlevaniaMilestones,
    sequence: u64,
    input: &CastlevaniaInput,
) {
    let reached = usize::from(value.stage).min(times.first_stage.len().saturating_sub(1));
    if inputs.first_stage.len() < times.first_stage.len() {
        inputs.first_stage.resize(times.first_stage.len(), None);
    }
    for stage in 1..=reached {
        if times.first_stage[stage].is_none() {
            times.first_stage[stage] = Some(sequence);
            inputs.first_stage[stage] = Some(input.clone());
        }
    }
    if value.victory && times.first_victory.is_none() {
        times.first_victory = Some(sequence);
        inputs.first_victory = Some(input.clone());
    }
    if value.death && times.first_death.is_none() {
        times.first_death = Some(sequence);
        inputs.first_death = Some(input.clone());
    }
}

fn first_input_needed(inputs: &CastlevaniaMilestoneInputs, value: CastlevaniaMilestones) -> bool {
    (1..=usize::from(value.stage))
        .any(|stage| inputs.first_stage.get(stage).is_none_or(Option::is_none))
        || (value.victory && inputs.first_victory.is_none())
        || (value.death && inputs.first_death.is_none())
}

fn action_champion_key(observations: &[CastlevaniaObservations]) -> Option<CastlevaniaChampionKey> {
    observations.last().and_then(|observation| {
        archive_key(observation.decoded).map(CastlevaniaArchiveKey::champion_key)
    })
}

impl<M: NesBackend<Vec<u8>>> CampaignTypes for CastlevaniaGame<M> {
    type JobObservation = ();
    type Target = CastlevaniaTarget<M>;
    type Action = ButtonChord;
    type Key = CastlevaniaArchiveKey;
    type Milestones = CastlevaniaMilestones;
    type Progress = CastlevaniaProgressWatermark;
    type Snapshot = CastlevaniaSnapshot;
    type Observations = CastlevaniaObservations;
    type Evidence = CastlevaniaCampaignEvidence;
    type ArchiveReport = CastlevaniaArchiveReport;
    type Run = CastlevaniaCampaignRun;
}

impl<M: NesBackend<Vec<u8>>> Reporting for CastlevaniaGame<M> {
    fn write_evidence_checkpoint(
        evidence: &CastlevaniaCampaignEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        use searcher::search::checkpoint::image::Sequence;
        postcard::to_io(
            &(
                Sequence(evidence.named_progress.first_seen.iter()),
                evidence.aggregate,
                evidence.watermark,
                evidence.first_reached,
                &evidence.first_inputs,
                &evidence.champion_input,
                evidence.champion_milestones,
                evidence.champion_key,
            ),
            out,
        )?;
        Ok(())
    }

    fn evidence_from_checkpoint(
        bytes: &[u8],
    ) -> Result<CastlevaniaCampaignEvidence, Box<dyn Error>> {
        let checkpoint: CastlevaniaEvidenceCheckpoint = postcard::from_bytes(bytes)?;
        let mut named_progress = NamedProgress::default();
        named_progress.restore(checkpoint.first_seen)?;
        Ok(CastlevaniaCampaignEvidence {
            named_progress,
            aggregate: checkpoint.aggregate,
            watermark: checkpoint.watermark,
            first_reached: checkpoint.first_reached,
            first_inputs: checkpoint.first_inputs,
            champion_input: checkpoint.champion_input,
            champion_milestones: checkpoint.champion_milestones,
            champion_key: checkpoint.champion_key,
        })
    }

    fn checkpoint_marks(evidence: &CastlevaniaCampaignEvidence) -> usize {
        evidence
            .named_progress
            .first_seen
            .iter()
            .filter(|(name, seen)| seen.is_some() && is_route_milestone(name))
            .count()
    }

    fn diagnostics(evidence: &CastlevaniaCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "named_progress": evidence.named_progress }))
    }

    fn merge_witness_diagnostics(
        evidence: &mut CastlevaniaCampaignEvidence,
        observations: &[CastlevaniaObservations],
        sequence: u64,
    ) {
        let action_end = observations
            .last()
            .map_or(0, |observation| observation.frame_count);
        for observation in observations {
            evidence
                .named_progress
                .observe(observation, sequence, action_end);
        }
    }

    fn retained_diagnostics<'a>(
        snapshots: impl Iterator<Item = (Option<&'a CastlevaniaSnapshot>, u64)>,
    ) -> Option<serde_json::Value> {
        let mut sections = BTreeMap::<String, [u64; 3]>::new();
        let (mut active, mut missing) = (0_u64, 0_u64);
        for (snapshot, selections) in snapshots {
            active += 1;
            let Some(snapshot) = snapshot else {
                missing += 1;
                continue;
            };
            let state = snapshot.state();
            let entry = sections
                .entry(format!("{}:{}", state.stage, state.section))
                .or_default();
            entry[0] += 1;
            entry[1] = entry[1].max(u64::from(state.health));
            entry[2] += selections;
        }
        Some(serde_json::json!({
            "active_entries": active,
            "missing_snapshots": missing,
            "live_entries_by_section": sections,
        }))
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
        result: &CastlevaniaCampaignJobResult<M>,
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
        evidence: &CastlevaniaCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> CastlevaniaArchiveReport {
        CastlevaniaArchiveReport {
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

impl<M: NesBackend<Vec<u8>>> InputPolicy for CastlevaniaGame<M> {
    fn policies(&self, _run: &CastlevaniaCampaignRun) -> WorkloadPolicies {
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
    ) -> Result<CastlevaniaCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&CastlevaniaCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("Castlevania stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("Castlevania stream carries an unknown game policy".into());
        }
        Ok(CastlevaniaCampaignRun)
    }

    fn sample_alphabet(
        &self,
        _run: &CastlevaniaCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::castlevania::archive::CHORDS.draw(rand, previous)
    }
}

impl<M: NesBackend<Vec<u8>>> TargetExecution for CastlevaniaGame<M> {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &CastlevaniaSnapshot) -> usize {
        std::mem::size_of::<CastlevaniaSnapshot>()
            .saturating_add(snapshot.emulator_state.memory_charge())
    }

    fn new_target(&self) -> Result<CastlevaniaTarget<M>, String> {
        (self.targets)()
    }

    fn reset(&self, target: &mut CastlevaniaTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut CastlevaniaTarget<M>,
        snapshot: &CastlevaniaSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &CastlevaniaTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut CastlevaniaTarget<M>,
        action: &ButtonChord,
        aggregate: &mut CastlevaniaMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(
        &self,
        target: &mut CastlevaniaTarget<M>,
    ) -> Result<CastlevaniaSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Castlevania".into())
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

impl<M: NesBackend<Vec<u8>>> Evaluation for CastlevaniaGame<M> {
    fn execution_disposition(&self, target: &CastlevaniaTarget<M>) -> ExecutionDisposition {
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
        _run: &CastlevaniaCampaignRun,
        target: &CastlevaniaTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.won())
    }

    fn current_key(
        &self,
        target: &CastlevaniaTarget<M>,
    ) -> Result<CastlevaniaArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "Castlevania current state has no gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: CastlevaniaArchiveKey,
        _snapshot: &CastlevaniaSnapshot,
    ) -> Result<CastlevaniaArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut CastlevaniaMilestones, from: CastlevaniaMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &CastlevaniaCampaignEvidence) -> CastlevaniaMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &CastlevaniaCampaignEvidence) -> CastlevaniaProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut CastlevaniaCampaignEvidence,
        source: &CastlevaniaArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut CastlevaniaCampaignEvidence,
        target: &CastlevaniaTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let observation = CastlevaniaObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            terminal: target.is_terminal(),
        };
        merge_progress_watermark(&mut evidence.watermark, std::slice::from_ref(&observation));
        evidence
            .named_progress
            .observe(&observation, 0, observation.frame_count);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut CastlevaniaCampaignEvidence,
        value: CastlevaniaMilestones,
        input: &CastlevaniaInput,
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
        evidence: &mut CastlevaniaCampaignEvidence,
        action: &CastlevaniaCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<CastlevaniaInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let action_end_frame = action
            .observations
            .last()
            .map_or(0, |observation| observation.frame_count);
        let mut discoveries = Vec::new();
        for observation in &action.observations {
            discoveries.extend(
                evidence
                    .named_progress
                    .observe(observation, sequence, action_end_frame)
                    .into_iter()
                    .filter(|name| is_route_milestone(name)),
            );
        }
        let first_input_needed = first_input_needed(&evidence.first_inputs, action.milestones);
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed || champion.is_some() || !discoveries.is_empty() {
            let input = input()?;
            self.publish_inputs(discoveries, &input)?;
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
        source: &'a CastlevaniaArchiveReport,
    ) -> &'a [crate::castlevania::archive::CastlevaniaArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(
        &self,
        source: &CastlevaniaArchiveReport,
    ) -> Result<CastlevaniaInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Castlevania source archive has no retained entries".into())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = CastlevaniaGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = CastlevaniaGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&CastlevaniaCampaignRun))
                .is_err()
        );
    }
}
