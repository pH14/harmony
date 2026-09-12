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
            CampaignStreamHeader, CampaignTypes, Evaluation, GamePolicies, InputPolicy, Reporting,
            SnapshotCheckpoint, TargetExecution, postcard_value_sha256,
            replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    },
    stb::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MAX_STB_ACTIONS, REPLACEMENT_IDENTIFIER,
            StbArchiveKey, StbArchiveReport, StbChampionKey, StbMilestoneInputs, StbMilestoneTimes,
            StbMilestones, StbProgressWatermark, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones_from_observation, sample_chord,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, StbAi, StbInput, StbObservations, StbSnapshot, StbTarget,
        },
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "stb-quicknes-campaign-stream-v3";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "stb-quicknes-snapshot-checkpoint-v3";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_no_start_select_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "local_match_gameover_player_a_win_v2";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StbNoTableHeader;

pub struct StbGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    identity: String,
    ai: StbAi,
}

impl StbGame {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        Self::with_ai(rom, core_path, core_sha256, StbAi::Easy)
    }

    #[must_use]
    pub fn with_ai(rom: &[u8], core_path: &Path, core_sha256: &str, ai: StbAi) -> Self {
        let ai_level = ai.level();
        let identity = format!(
            "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;source=sgadrat/super-tilt-bro@b132fd25add46f816e04be64c434386743b84b8b;rom=tilt_no_network_unrom_E;mode=local;stocks=4;ai={ai_level};stage=0;genesis=stb-local-ai-v1;result_digest=stb-semantic-postcard-1.1.3-sha256-hex-v4;sha256={core_sha256}",
            machine::quicknes::QUICKNES_REVISION,
            machine::quicknes::QUICKNES_BUILD,
            machine::quicknes::QUICKNES_OPTIONS,
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            identity,
            ai,
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
}

impl StbGame {
    #[must_use]
    pub const fn ai(&self) -> StbAi {
        self.ai
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Clone, Copy, Debug)]
pub struct StbCampaignRun;

#[derive(Clone, Default)]
pub struct StbCampaignEvidence {
    aggregate: StbMilestones,
    watermark: StbProgressWatermark,
    first_reached: StbMilestoneTimes,
    first_inputs: StbMilestoneInputs,
    champion_input: StbInput,
    champion_milestones: StbMilestones,
    champion_key: Option<StbChampionKey>,
}

pub type StbCampaignOrigin = CampaignOrigin<StbGame>;
pub type StbCampaignCheckpoint = CampaignCheckpoint<StbSnapshot>;
pub type StbSnapshotCheckpoint = SnapshotCheckpoint<StbSnapshot>;
pub type StbCampaignStreamHeader = CampaignStreamHeader<StbNoTableHeader>;
pub type StbCampaignModeReport = CampaignModeReport<ButtonChord, StbArchiveReport>;
type StbCampaignActionResult = CampaignActionResult<StbGame>;
type StbCampaignJobResult = CampaignJobResult<StbGame>;

#[derive(Serialize)]
struct StbResultCandidate<'a> {
    key: &'a StbArchiveKey,
    viable: bool,
}

#[derive(Serialize)]
struct StbResultAction<'a> {
    action: ButtonChord,
    observations: &'a [StbObservations],
    milestones: StbMilestones,
    dead: bool,
    victory: bool,
    failed: bool,
    candidate: Option<StbResultCandidate<'a>>,
}

#[derive(Serialize)]
struct StbResult<'a> {
    actions: Vec<StbResultAction<'a>>,
}

fn stb_result_sha256(result: &StbCampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|action| StbResultAction {
            action: action.action,
            observations: &action.observations,
            milestones: action.milestones,
            dead: action.dead,
            victory: action.victory,
            failed: action.failed,
            candidate: action
                .candidate
                .as_ref()
                .map(|candidate| StbResultCandidate {
                    key: &candidate.key,
                    viable: candidate.viable,
                }),
        })
        .collect();
    postcard_value_sha256(&StbResult { actions })
}

pub struct StbCampaignConfig {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_victory: bool,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub selector: crate::search::archive::SelectorPolicy,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<PathBuf>,
}

impl StbCampaignConfig {
    fn generic(&self) -> CampaignConfig<StbGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            continue_after_victory: self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker:
                crate::search::campaign::DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: StbCampaignRun,
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
        .ok_or_else(|| format!("Stb stream is missing {field}").into())
}

fn merge_action_milestones(
    aggregate: &mut StbMilestones,
    target: &StbTarget,
) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Ok(());
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones_from_observation(observation));
    }
    Ok(())
}

fn execute_suffix(
    target: &mut StbTarget,
    parent_actions: usize,
    parent_milestones: StbMilestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<StbCampaignJobResult, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.is_match_over() || target.exit_kind() != ExitKind::Ok {
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
        let observations = target.last_action_observations().to_vec();
        let victory = target.player_a_won();
        let dead = target.is_match_over() && !victory;
        let failed = target.exit_kind() != ExitKind::Ok;
        let gameplay_valid = target.mechanical_state().gameplay.is_some();
        let recorded_action = if dead || victory {
            let elapsed = target
                .observe()
                .frame_count
                .saturating_sub(action_start_frame)
                .min(u64::from(MAX_HOLD_FRAMES));
            ButtonChord::new(action.buttons, u8::try_from(elapsed.max(1))?)
        } else {
            *action
        };
        let candidate = if dead || victory || failed || !gameplay_valid {
            None
        } else {
            let snapshot = target.snapshot().ok_or("failed to snapshot Stb suffix")?;
            let viable = match retention {
                RetentionPolicy::AdmitAlive => true,
                RetentionPolicy::ProbeAtAdmission45 => {
                    return Err(
                        "STB deliberately rejects ProbeAtAdmission45: ordinary admission is sufficient".into(),
                    );
                }
            };
            Some(CampaignCandidate {
                key: archive_key(target.mechanical_state())
                    .ok_or("STB attempted to archive a phase-invalid state")?,
                viable,
                snapshot,
            })
        };
        actions.push(CampaignActionResult {
            action: recorded_action,
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

fn update_first_inputs(
    times: &mut StbMilestoneTimes,
    inputs: &mut StbMilestoneInputs,
    value: StbMilestones,
    sequence: u64,
    input: &StbInput,
) {
    if value.opponent_kos > 0 && times.first_opponent_ko.is_none() {
        times.first_opponent_ko = Some(sequence);
        inputs.first_opponent_ko = Some(input.clone());
    }
    if value.victory && times.first_victory.is_none() {
        times.first_victory = Some(sequence);
        inputs.first_victory = Some(input.clone());
    }
    if value.defeat && times.first_defeat.is_none() {
        times.first_defeat = Some(sequence);
        inputs.first_defeat = Some(input.clone());
    }
}

fn action_champion_key(observations: &[StbObservations]) -> Option<StbChampionKey> {
    observations
        .last()
        .and_then(|observation| archive_key(observation.decoded).map(StbArchiveKey::champion_key))
}

impl CampaignTypes for StbGame {
    type Target = StbTarget;
    type Action = ButtonChord;
    type Key = StbArchiveKey;
    type Milestones = StbMilestones;
    type Progress = StbProgressWatermark;
    type Snapshot = StbSnapshot;
    type Observations = StbObservations;
    type Evidence = StbCampaignEvidence;
    type ArchiveReport = StbArchiveReport;
    type Run = StbCampaignRun;
    type DrawState = ();
    type DrawCheckpoint = ();
    type TableHeader = StbNoTableHeader;
}

impl Reporting for StbGame {
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn image_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rom))
    }

    fn result_sha256(&self, result: &StbCampaignJobResult) -> Result<String, Box<dyn Error>> {
        stb_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &StbCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> StbArchiveReport {
        StbArchiveReport {
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
            deaths: state.deaths,
            selector: state.selector,
        }
    }
}

impl InputPolicy for StbGame {
    fn max_action_limit(&self) -> usize {
        MAX_STB_ACTIONS
    }

    fn longest_action_time(&self) -> u64 {
        u64::from(crate::stb::archive::LONGEST_HOLD_FRAMES)
    }

    fn draw_state_memory_reserve_bytes(&self, _run: &StbCampaignRun, _max_actions: usize) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn policies(&self, _run: &StbCampaignRun) -> GamePolicies {
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

    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<StbCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&StbCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Stb stream {field} policy is not recognized").into());
                }
            }
            return Err("Stb stream carries an unknown game policy".into());
        }
        Ok(StbCampaignRun)
    }

    fn initial_draw_state(
        &self,
        _run: &StbCampaignRun,
        _origin: Option<(&str, &StbArchiveReport)>,
    ) -> Result<((), Option<StbNoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(&self, _state: &()) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &StbCampaignRun,
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
        run: &StbCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&()>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Stb stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &StbCampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn retained_inputs_need_full(&self, _run: &StbCampaignRun) -> bool {
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
            Err("Stb stream requires an unsupported draw-table version".into())
        }
    }
}

impl TargetExecution for StbGame {
    fn action_time_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &StbSnapshot) -> usize {
        machine::quicknes::QuickNesMachine::portable_memory_charge(&snapshot.emulator_state)
    }

    fn new_target(&self) -> Result<StbTarget, String> {
        StbTarget::from_rom_bytes_headless_with_ai(
            &self.rom,
            &self.core_path,
            &self.core_sha256,
            self.ai,
        )
        .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut StbTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut StbTarget,
        snapshot: &StbSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn frames_clocked(&self, target: &StbTarget) -> u64 {
        target.frames_clocked()
    }

    fn apply_action(
        &self,
        target: &mut StbTarget,
        action: &ButtonChord,
        aggregate: &mut StbMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn snapshot(&self, target: &mut StbTarget) -> Result<StbSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Stb".into())
    }

    fn execute_job(
        &self,
        _run: &StbCampaignRun,
        target: &mut StbTarget,
        origin_snapshot: &StbSnapshot,
        replay: &[ButtonChord],
        parent_actions: usize,
        parent_milestones: StbMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<StbCampaignJobResult, Box<dyn Error>> {
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
        )
    }
}

impl Evaluation for StbGame {
    fn is_terminal(&self, target: &StbTarget) -> bool {
        target.is_match_over() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        _run: &StbCampaignRun,
        target: &StbTarget,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok {
            return Err("Stb terminal predicate cannot inspect a failed emulator".into());
        }
        Ok(target.player_a_won())
    }

    fn current_key(&self, target: &StbTarget) -> Result<StbArchiveKey, Box<dyn Error>> {
        archive_key(target.mechanical_state())
            .ok_or_else(|| "STB current state has no valid gameplay archive key".into())
    }

    fn complete_candidate_key(
        &self,
        key: StbArchiveKey,
        _snapshot: &StbSnapshot,
    ) -> Result<StbArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut StbMilestones, from: StbMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &StbCampaignEvidence) -> StbMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &StbCampaignEvidence) -> StbProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(&self, evidence: &mut StbCampaignEvidence, source: &StbArchiveReport) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut StbCampaignEvidence,
        target: &StbTarget,
    ) -> Result<(), Box<dyn Error>> {
        let observation = StbObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            changed_indices: Vec::new(),
            player_a_ko: false,
            player_b_ko: false,
            player_a_ko_count: 0,
            player_b_ko_count: 0,
            terminal: target.is_match_over(),
            log_line: String::new(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut StbCampaignEvidence,
        value: StbMilestones,
        input: &StbInput,
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
        evidence: &mut StbCampaignEvidence,
        action: &StbCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<StbInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let first_input_needed = (action.milestones.opponent_kos > 0
            && evidence.first_inputs.first_opponent_ko.is_none())
            || (action.milestones.victory && evidence.first_inputs.first_victory.is_none())
            || (action.milestones.defeat && evidence.first_inputs.first_defeat.is_none());
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
        source: &'a StbArchiveReport,
    ) -> &'a [crate::stb::archive::StbArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &StbArchiveReport) -> Result<StbInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Stb source archive has no retained entries".into())
    }
}

pub fn run_stb_campaign_checkpointed(
    game: &StbGame,
    config: &StbCampaignConfig,
    origin: &StbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(StbCampaignModeReport, StbSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_stb_campaign_checkpointed(
    game: &StbGame,
    stream_bytes: &[u8],
    origin_report: Option<&StbArchiveReport>,
    origin_checkpoint: Option<&StbCampaignCheckpoint>,
) -> Result<(StbCampaignModeReport, StbSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result_with_portable(bytes: Vec<u8>) -> StbCampaignJobResult {
        let state = crate::stb::target::StbMechanicalState {
            gameplay: Some(crate::stb::target::StbGameplayState::default()),
            ..crate::stb::target::StbMechanicalState::default()
        };
        let observation = StbObservations {
            frame_count: 3,
            decoded: state,
            changed_indices: vec![1, 2],
            player_a_ko: false,
            player_b_ko: false,
            player_a_ko_count: 0,
            player_b_ko_count: 0,
            terminal: false,
            log_line: "frame=3 changed=[1, 2]".to_owned(),
        };
        let portable = serde_json::from_value(serde_json::json!(bytes))
            .expect("shared-state wire representation");
        CampaignJobResult {
            actions: vec![CampaignActionResult {
                action: ButtonChord::new(0x81, 3),
                observations: vec![observation.clone()],
                milestones: StbMilestones::default(),
                dead: false,
                victory: false,
                failed: false,
                candidate: Some(CampaignCandidate {
                    key: archive_key(state).expect("synthetic live archive key"),
                    viable: true,
                    snapshot: StbSnapshot {
                        emulator_state: portable,
                        observation,
                        wram: vec![0; 2_048],
                        failed: false,
                        last_valid_gameplay: state.gameplay,
                        player_a_ko_count: 0,
                        player_b_ko_count: 0,
                    },
                }),
            }],
        }
    }

    #[test]
    fn champion_preserves_resources_without_ranking_diagnostic_fields() {
        let mut healthy = result_with_portable(vec![]).actions.remove(0).observations;
        let gameplay = healthy[0].decoded.gameplay.as_mut().unwrap();
        gameplay.player_a_stocks = 4;
        gameplay.player_b_stocks = 4;
        let mut wounded = healthy.clone();
        wounded[0]
            .decoded
            .gameplay
            .as_mut()
            .unwrap()
            .player_a_damage = 80;
        assert!(action_champion_key(&healthy) > action_champion_key(&wounded));
        let mut diagnostic = healthy.clone();
        let gameplay = diagnostic[0].decoded.gameplay.as_mut().unwrap();
        gameplay.player_a_hitstun = 20;
        gameplay.player_a_grounded = true;
        gameplay.player_a_x = 200;
        assert_eq!(
            action_champion_key(&healthy),
            action_champion_key(&diagnostic)
        );
    }

    #[test]
    fn source_identity_matches_the_pinned_build_input() {
        let commit = include_str!("../../stb-versions.env")
            .lines()
            .find_map(|line| line.strip_prefix("STB_COMMIT="))
            .unwrap();
        let game = StbGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        assert!(
            game.emulator_identity()
                .contains(&format!("source=sgadrat/super-tilt-bro@{commit};"))
        );
    }

    #[test]
    fn previous_key_policy_requires_its_previous_implementation() {
        let game = StbGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        let mut policies = game.policies(&StbCampaignRun);
        policies.insert(
            KEY_POLICY_FIELD.to_owned(),
            "stb_local_ai_spatial_16_preference_v2".to_owned(),
        );
        assert!(game.resolve_recorded(&policies).is_err());
    }

    #[test]
    fn recorded_policy_set_is_exact_and_game_owned() {
        let game = StbGame::new(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64));
        let policies = game.policies(&StbCampaignRun);
        let StbCampaignRun = game.resolve_recorded(&policies).expect("resolve");
        let mut foreign = policies;
        foreign.insert("level".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
    }

    #[test]
    fn recordings_reject_another_ai_difficulty() {
        let easy = StbGame::new(&[], Path::new("core.so"), &"a".repeat(64));
        for ai in [StbAi::Fair, StbAi::Hard] {
            let harder = StbGame::with_ai(&[], Path::new("core.so"), &"a".repeat(64), ai);
            assert!(
                harder
                    .emulator_identity()
                    .contains(&format!(";ai={};", ai.level()))
            );
            assert!(
                harder
                    .resolve_recorded(&easy.policies(&StbCampaignRun))
                    .is_err()
            );
            assert!(
                easy.resolve_recorded(&harder.policies(&StbCampaignRun))
                    .is_err()
            );
            assert!(
                harder
                    .resolve_recorded(&harder.policies(&StbCampaignRun))
                    .is_ok()
            );
        }
    }

    #[test]
    fn local_ai_mode_is_part_of_recorded_machine_identity() {
        let game = StbGame::new(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64));
        assert!(
            game.emulator_identity()
                .contains("mode=local;stocks=4;ai=1;stage=0")
        );
    }

    #[test]
    fn result_digest_uses_game_visible_candidate_state_not_portable_bytes() {
        let first = result_with_portable(vec![1, 2, 3]);
        let mut second = result_with_portable(vec![9, 8, 7, 6]);
        assert_eq!(
            stb_result_sha256(&first).expect("first digest"),
            stb_result_sha256(&second).expect("second digest"),
        );

        let changed = crate::stb::target::StbMechanicalState {
            gameplay: Some(crate::stb::target::StbGameplayState {
                player_a_x: 64,
                ..crate::stb::target::StbGameplayState::default()
            }),
            ..crate::stb::target::StbMechanicalState::default()
        };
        second.actions[0].candidate.as_mut().expect("candidate").key =
            archive_key(changed).expect("synthetic changed live archive key");
        assert_ne!(
            stb_result_sha256(&first).expect("first digest"),
            stb_result_sha256(&second).expect("changed digest"),
        );
    }
}
