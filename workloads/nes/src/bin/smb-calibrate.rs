// SPDX-License-Identifier: AGPL-3.0-or-later
#![recursion_limit = "256"]

use std::{
    collections::BTreeMap,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use nes_workload::{
    search::{
        archive::{ArchiveKey, Input, RetentionPolicy},
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCheckpoint, CampaignConfig,
            CampaignExecutionOptions, CampaignJobResult, CampaignOrigin, CampaignTypes, Evaluation,
            InputPolicy, Reporting, SnapshotCheckpoint, SnapshotCheckpointEntry, TargetExecution,
            WorkloadPolicies, postcard_result_sha256, postcard_value_sha256,
            replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
        },
        draw::{DrawMixture, SuffixShape},
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome},
    },
    smb::{
        archive::{KEY_POLICY_IDENTIFIER, SmbArchiveEntryReport, SmbArchiveKey, SmbArchiveReport},
        campaign::{
            CONTROLLER_VOCABULARY_FIELD, KEY_POLICY_FIELD, SmbButtonVocabulary, SmbCampaignRun,
            SmbGame,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes,
            SmbMilestones, SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
            WRAM_SIZE, smb_camera_pixels, smb_mechanical_state_from_wram,
        },
    },
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REQUEST_LIMIT: usize = 64_000;
const PREFIX_FILE_LIMIT: usize = 1_000_000;
const PREFIX_ACTION_LIMIT: usize = 8_192;
const PREFIX_FRAME_LIMIT: u64 = 1_000_000;
const ROM_FILE_LIMIT: usize = 16_000_000;
const CORE_FILE_LIMIT: u64 = 128_000_000;
const CAMPAIGN_STREAM_LIMIT: usize = 32_000_000;
const ACTION_LIMIT: usize = 128;
const ARCHIVE_LIMIT: usize = 512;
const MEMORY_BUDGET_MIB: usize = 128;
const HISTORY_DIAGNOSTIC_LIMIT: usize = 4_096;
const BUTTON_MASKS: [u8; 8] = [0, 128, 129, 130, 1, 2, 64, 65];
const HISTORY_A_OFFSET: usize = 0x06d9;
const HISTORY_B_OFFSET: usize = 0x06da;
const WORLD_OFFSET: usize = 0x075f;
const LEVEL_OFFSET: usize = 0x075c;
const AREA_TYPE_OFFSET: usize = 0x074e;
const AREA_NUMBER_OFFSET: usize = 0x074f;
const PLAYER_X_PAGE_OFFSET: usize = 0x006d;
const PLAYER_X_OFFSET: usize = 0x0086;
const TIMER_OFFSET: usize = 0x07f8;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    History,
    Actions,
}

impl Mode {
    fn id(self) -> &'static str {
        match self {
            Self::History => "history",
            Self::Actions => "actions",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    rom_path: PathBuf,
    rom_sha256: String,
    core_path: PathBuf,
    core_sha256: String,
    #[serde(default)]
    source_revision: Option<String>,
    prefix_input: PathBuf,
    mode: Mode,
    broken: bool,
    seed: u64,
    work_budget_frames: u64,
    root_expected: RawRoot,
    objective: Objective,
    action_hold_frames: u8,
    #[serde(default)]
    retain_stream: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct RawRoot {
    world: u8,
    level: u8,
    area_type: u8,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Objective {
    world: u8,
    level: u8,
    area_type: u8,
    absolute_player_x_min: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixFile {
    actions: Vec<PrefixAction>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixAction {
    buttons: u8,
    hold_frames: u8,
}

struct CalibrationGame {
    base: SmbGame,
    mode: Mode,
    broken: bool,
    hold_frames: u8,
    objective: Objective,
    rom_sha256: String,
    core_sha256: String,
    source_revision: Option<String>,
}

#[derive(Clone, Default)]
struct Evidence {
    milestones: SmbMilestones,
    progress: SmbProgressWatermark,
    action_attempts: u64,
    actions_by_mask: [u64; BUTTON_MASKS.len()],
    observations: u64,
    history_equal_samples: u64,
    history_unequal_samples: u64,
    history_collision_keys: BTreeMap<LossyHistoryKey, u8>,
    history_collision_count: u64,
    history_collision_set_truncated: bool,
    objective_observations: u64,
    dead_observations: u64,
    malformed_observations: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct CalibrationArchiveReport {
    base: SmbArchiveReport,
    diagnostics: Value,
}

type LossyHistoryKey = (
    <SmbArchiveKey as ArchiveKey>::Progress,
    <SmbArchiveKey as ArchiveKey>::Place,
    <SmbArchiveKey as ArchiveKey>::Identity,
);

struct BoundedStream(Vec<u8>);

impl Write for BoundedStream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > CAMPAIGN_STREAM_LIMIT {
            return Err(io::Error::other("campaign stream exceeded its 32 MB bound"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn read_capped(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit.saturating_add(1))?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{} exceeds the {limit}-byte input bound", path.display()).into());
    }
    Ok(bytes)
}

fn hash_file(path: &Path, limit: u64) -> Result<(String, u64), Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(u64::try_from(read)?);
        if size > limit {
            return Err(format!("{} exceeds the {limit}-byte file bound", path.display()).into());
        }
        hasher.update(&buffer[..read]);
    }
    Ok((format!("{:x}", hasher.finalize()), size))
}

fn check_digest(expected: &str, actual: &str, label: &str) -> Result<(), Box<dyn Error>> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} SHA-256 must be 64 hexadecimal characters").into());
    }
    if !expected.eq_ignore_ascii_case(actual) {
        return Err(format!("{label} SHA-256 does not match the file").into());
    }
    Ok(())
}

fn validate_request(request: &Request) -> Result<(), Box<dyn Error>> {
    if request.work_budget_frames == 0 || request.work_budget_frames > 20_000 {
        return Err("work_budget_frames must be in 1..=20000".into());
    }
    if request.action_hold_frames == 0 || request.action_hold_frames > 16 {
        return Err("action_hold_frames must be in 1..=16".into());
    }
    if let Some(revision) = &request.source_revision
        && (revision.len() != 40 && revision.len() != 64
            || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err("source_revision must be a 40- or 64-character hexadecimal revision".into());
    }
    Ok(())
}

fn read_prefix(path: &Path) -> Result<(SmbInput, u64, String), Box<dyn Error>> {
    let bytes = read_capped(path, PREFIX_FILE_LIMIT)?;
    let prefix: PrefixFile = serde_json::from_slice(&bytes)?;
    if prefix.actions.len() > PREFIX_ACTION_LIMIT {
        return Err("prefix_input exceeds 8192 actions".into());
    }
    let mut frame_count = 0_u64;
    let mut actions = Vec::with_capacity(prefix.actions.len());
    for action in prefix.actions {
        if action.hold_frames == 0 || action.hold_frames > MAX_HOLD_FRAMES {
            return Err("prefix action hold_frames is outside the native 1..=120 bound".into());
        }
        frame_count = frame_count
            .checked_add(u64::from(action.hold_frames))
            .ok_or("prefix frame count overflowed")?;
        if frame_count > PREFIX_FRAME_LIMIT {
            return Err("prefix_input exceeds 1000000 frames".into());
        }
        actions.push(ButtonChord::new(action.buttons, action.hold_frames));
    }
    Ok((
        Input { actions },
        frame_count,
        format!("{:x}", Sha256::digest(bytes)),
    ))
}

fn masks_for(mode: Mode, broken: bool) -> &'static [u8] {
    if mode == Mode::Actions && broken {
        &[128]
    } else {
        &BUTTON_MASKS
    }
}

fn effective_key(mode: Mode, broken: bool, mut key: SmbArchiveKey) -> SmbArchiveKey {
    if mode == Mode::History && broken {
        key.loop_on_path = false;
    }
    key
}

fn root_matches(wram: &[u8; WRAM_SIZE], expected: RawRoot) -> bool {
    wram[WORLD_OFFSET] == expected.world
        && wram[LEVEL_OFFSET] == expected.level
        && wram[AREA_TYPE_OFFSET] == expected.area_type
}

fn objective_matches(wram: &[u8; WRAM_SIZE], objective: Objective) -> bool {
    let absolute_player_x =
        u16::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u16::from(wram[PLAYER_X_OFFSET]);
    !smb_mechanical_state_from_wram(wram).dead
        && wram[WORLD_OFFSET] == objective.world
        && wram[LEVEL_OFFSET] == objective.level
        && wram[AREA_TYPE_OFFSET] == objective.area_type
        && absolute_player_x >= objective.absolute_player_x_min
}

fn raw_archive_key(wram: &[u8; WRAM_SIZE]) -> SmbArchiveKey {
    let decoded = smb_mechanical_state_from_wram(wram);
    let absolute_player_x =
        u32::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u32::from(wram[PLAYER_X_OFFSET]);
    let screen_x = absolute_player_x.saturating_sub(smb_camera_pixels(wram));
    let room_x_bucket = u8::try_from(screen_x.min(255) / 16).unwrap_or(15);
    let clock = wram[TIMER_OFFSET..TIMER_OFFSET + 3]
        .iter()
        .fold(0_u16, |value, digit| {
            value.saturating_mul(10) + u16::from(*digit)
        });
    SmbArchiveKey {
        world: decoded.world,
        level: decoded.level,
        progress: decoded.progress,
        player_y_bucket: decoded.player_y_bucket,
        loop_on_path: wram[HISTORY_A_OFFSET] == wram[HISTORY_B_OFFSET],
        room_x_bucket,
        time_bucket: wram[TIMER_OFFSET],
        clock,
        room: [
            wram[AREA_TYPE_OFFSET],
            wram[AREA_NUMBER_OFFSET],
            u8::try_from(decoded.progress / 16).unwrap_or(u8::MAX),
        ],
    }
}

fn lossy_history_key(mut key: SmbArchiveKey) -> LossyHistoryKey {
    key.loop_on_path = false;
    (key.progress(), key.place(), key.identity())
}

fn note_history_state(evidence: &mut Evidence, key: SmbArchiveKey, on_path: bool) {
    let signature = lossy_history_key(key);
    let flag = if on_path { 1 } else { 2 };
    if let Some(previous) = evidence.history_collision_keys.get_mut(&signature) {
        let updated = *previous | flag;
        if *previous != 3 && updated == 3 {
            evidence.history_collision_count = evidence.history_collision_count.saturating_add(1);
        }
        *previous = updated;
    } else if evidence.history_collision_keys.len() < HISTORY_DIAGNOSTIC_LIMIT {
        evidence.history_collision_keys.insert(signature, flag);
    } else {
        evidence.history_collision_set_truncated = true;
    }
}

fn merge_milestones(into: &mut SmbMilestones, from: SmbMilestones) {
    into.max_1_1_scroll_bucket = into.max_1_1_scroll_bucket.max(from.max_1_1_scroll_bucket);
    into.reached_1_1_flag |= from.reached_1_1_flag;
    into.reached_1_2 |= from.reached_1_2;
    into.reached_onward |= from.reached_onward;
}

fn record_observation(evidence: &mut Evidence, observation: &SmbObservations) {
    evidence.observations = evidence.observations.saturating_add(1);
    evidence.dead_observations = evidence
        .dead_observations
        .saturating_add(u64::from(observation.dead));
    if observation.wram.len() != WRAM_SIZE {
        evidence.malformed_observations = evidence.malformed_observations.saturating_add(1);
        return;
    }
    let Ok(wram) = <&[u8; WRAM_SIZE]>::try_from(observation.wram.as_slice()) else {
        evidence.malformed_observations = evidence.malformed_observations.saturating_add(1);
        return;
    };
    let key = raw_archive_key(wram);
    if key.loop_on_path {
        evidence.history_equal_samples = evidence.history_equal_samples.saturating_add(1);
    } else {
        evidence.history_unequal_samples = evidence.history_unequal_samples.saturating_add(1);
    }
    note_history_state(evidence, key, key.loop_on_path);
    let decoded = observation.decoded;
    evidence.progress = evidence.progress.max(SmbProgressWatermark {
        world: decoded.world,
        level: decoded.level,
        progress: decoded.progress,
    });
    merge_milestones(&mut evidence.milestones, observation.milestones);
}

impl CampaignTypes for CalibrationGame {
    type Target = SmbTarget;
    type Action = ButtonChord;
    type Key = SmbArchiveKey;
    type Milestones = SmbMilestones;
    type Progress = SmbProgressWatermark;
    type Snapshot = SmbSnapshot;
    type Observations = SmbObservations;
    type Evidence = Evidence;
    type ArchiveReport = CalibrationArchiveReport;
    type Run = ();
}

impl Reporting for CalibrationGame {
    fn stream_format(&self) -> &'static str {
        "smb-local-calibration-v1"
    }

    fn checkpoint_format(&self) -> &'static str {
        self.base.snapshot_checkpoint_format()
    }

    fn workload_identity_sha256(&self) -> String {
        let policies = self.policies(&());
        let identity = serde_json::to_vec(&(
            &self.rom_sha256,
            &self.core_sha256,
            self.base.emulator_identity(),
            self.mode,
            self.broken,
            self.hold_frames,
            self.objective,
            &self.source_revision,
            policies,
        ))
        .expect("calibration identity is serializable");
        format!("{:x}", Sha256::digest(identity))
    }

    fn action_cost_unit(&self) -> &'static str {
        "frames"
    }

    fn execution_work_unit(&self) -> &'static str {
        "frames"
    }

    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &Evidence,
        state: ArchiveReportState<Self>,
    ) -> CalibrationArchiveReport {
        CalibrationArchiveReport {
            base: SmbArchiveReport {
                seed: state.seed,
                executions: state.executions,
                milestones: evidence.milestones,
                progress_watermark: evidence.progress,
                first_reached: SmbMilestoneTimes::default(),
                first_inputs: SmbMilestoneInputs::default(),
                champion_input: SmbInput::default(),
                entries: state.entries,
                progress_curve: state.progress_curve,
                retained: state.retained,
                rejected: state.rejected,
                deaths: state
                    .terminal_endpoints
                    .saturating_sub(state.terminal_objectives),
                selector: state.selector,
            },
            diagnostics: Self::diagnostics(evidence).unwrap_or(Value::Null),
        }
    }

    fn diagnostics(evidence: &Evidence) -> Option<Value> {
        Some(json!({
            "action_attempts": evidence.action_attempts,
            "actions_by_mask_order": evidence.actions_by_mask,
            "action_masks": BUTTON_MASKS,
            "observations": evidence.observations,
            "raw_history_equal_samples": evidence.history_equal_samples,
            "raw_history_unequal_samples": evidence.history_unequal_samples,
            "raw_history_collision_keys": evidence.history_collision_count,
            "raw_history_collision_key_set_size": evidence.history_collision_keys.len(),
            "raw_history_collision_key_set_truncated": evidence.history_collision_set_truncated,
            "objective_observations": evidence.objective_observations,
            "dead_observations": evidence.dead_observations,
            "malformed_observations": evidence.malformed_observations,
            "progress_watermark": evidence.progress,
            "milestones": evidence.milestones,
        }))
    }
}

impl InputPolicy for CalibrationGame {
    fn max_action_limit(&self) -> usize {
        ACTION_LIMIT
    }

    fn max_action_cost(&self) -> u64 {
        u64::from(self.hold_frames)
    }

    fn policies(&self, _: &()) -> WorkloadPolicies {
        let base_run = SmbCampaignRun {
            vocabulary: SmbButtonVocabulary::NesPressable,
            terminal: None,
        };
        let mut policies = self.base.policies(&base_run);
        policies.insert(
            CONTROLLER_VOCABULARY_FIELD.to_owned(),
            if self.mode == Mode::Actions && self.broken {
                "calibration_frozen_right_v1".to_owned()
            } else {
                "calibration_fixed_eight_v1".to_owned()
            },
        );
        if self.mode == Mode::History && self.broken {
            policies.insert(
                KEY_POLICY_FIELD.to_owned(),
                format!("{KEY_POLICY_IDENTIFIER}+loop_on_path_false_control"),
            );
        }
        policies.insert("calibration_mode".to_owned(), self.mode.id().to_owned());
        policies.insert("calibration_broken".to_owned(), self.broken.to_string());
        policies.insert(
            "calibration_action_hold_frames".to_owned(),
            self.hold_frames.to_string(),
        );
        policies
    }

    fn resolve_recorded(&self, policies: &WorkloadPolicies) -> Result<(), Box<dyn Error>> {
        if policies != &self.policies(&()) {
            return Err("campaign stream calibration policies do not match the request".into());
        }
        Ok(())
    }

    fn sample_alphabet(
        &self,
        _: &(),
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        let masks = masks_for(self.mode, self.broken);
        let index = rand.below(std::num::NonZeroUsize::new(masks.len()).ok_or("empty mask")?);
        Ok(ButtonChord::new(masks[index], self.hold_frames))
    }
}

impl TargetExecution for CalibrationGame {
    fn new_target(&self) -> Result<SmbTarget, String> {
        self.base.new_target()
    }

    fn reset(&self, target: &mut SmbTarget) {
        self.base.reset(target);
    }

    fn restore(
        &self,
        target: &mut SmbTarget,
        snapshot: &SmbSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        self.base.restore(target, snapshot)
    }

    fn execution_work(&self, target: &SmbTarget) -> u64 {
        self.base.execution_work(target)
    }

    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        |action| u64::from(action.bounded_hold_frames())
    }

    fn snapshot_memory_charge(snapshot: &SmbSnapshot) -> usize {
        <SmbGame as TargetExecution>::snapshot_memory_charge(snapshot)
    }

    fn apply_action(
        &self,
        target: &mut SmbTarget,
        action: &ButtonChord,
        milestones: &mut SmbMilestones,
    ) -> Result<(), Box<dyn Error>> {
        self.base.apply_action(target, action, milestones)
    }

    fn rollout_observations(&self, target: &SmbTarget) -> Vec<SmbObservations> {
        self.base.rollout_observations(target)
    }

    fn rollout_probe(
        &self,
        _: &(),
        target: &mut SmbTarget,
        snapshot: &SmbSnapshot,
    ) -> Result<bool, Box<dyn Error>> {
        self.base.rollout_probe(
            &SmbCampaignRun {
                vocabulary: SmbButtonVocabulary::NesPressable,
                terminal: None,
            },
            target,
            snapshot,
        )
    }

    fn snapshot(&self, target: &mut SmbTarget) -> Result<SmbSnapshot, Box<dyn Error>> {
        self.base.snapshot(target)
    }
}

impl Evaluation for CalibrationGame {
    fn execution_disposition(&self, target: &SmbTarget) -> ExecutionDisposition {
        self.base.execution_disposition(target)
    }

    fn objective_reached(&self, _: &(), target: &SmbTarget) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok || target.is_dead() {
            return Ok(false);
        }
        Ok(objective_matches(&target.wram(), self.objective))
    }

    fn rollout_outcome(&self, run: &(), target: &SmbTarget) -> Result<Outcome, Box<dyn Error>> {
        let reached = self.objective_reached(run, target)?;
        Ok(Outcome {
            objective_reached: reached,
            disposition: if reached {
                ExecutionDisposition::Terminal
            } else {
                self.execution_disposition(target)
            },
        })
    }

    fn current_key(&self, target: &SmbTarget) -> Result<SmbArchiveKey, Box<dyn Error>> {
        Ok(effective_key(
            self.mode,
            self.broken,
            self.base.current_key(target)?,
        ))
    }

    fn rollout_key(&self, target: &SmbTarget) -> Result<SmbArchiveKey, Box<dyn Error>> {
        Ok(effective_key(
            self.mode,
            self.broken,
            self.base.rollout_key(target)?,
        ))
    }

    fn complete_candidate_key(
        &self,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot,
    ) -> Result<SmbArchiveKey, Box<dyn Error>> {
        Ok(effective_key(
            self.mode,
            self.broken,
            self.base.complete_candidate_key(key, snapshot)?,
        ))
    }

    fn merge_milestones(&self, into: &mut SmbMilestones, from: SmbMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &Evidence) -> SmbMilestones {
        evidence.milestones
    }

    fn aggregate_progress(evidence: &Evidence) -> SmbProgressWatermark {
        evidence.progress
    }

    fn merge_origin_evidence(&self, evidence: &mut Evidence, source: &CalibrationArchiveReport) {
        merge_milestones(&mut evidence.milestones, source.base.milestones);
        evidence.progress = evidence.progress.max(source.base.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Evidence,
        target: &SmbTarget,
    ) -> Result<(), Box<dyn Error>> {
        let wram = target.wram();
        let state = target.observe();
        record_observation(evidence, &state);
        evidence.objective_observations = evidence
            .objective_observations
            .saturating_add(u64::from(objective_matches(&wram, self.objective)));
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut Evidence,
        milestones: SmbMilestones,
        input: &Input<ButtonChord>,
    ) {
        merge_milestones(&mut evidence.milestones, milestones);
        evidence.action_attempts = evidence
            .action_attempts
            .saturating_add(u64::try_from(input.actions.len()).unwrap_or(u64::MAX));
    }

    fn merge_action_evidence<F>(
        &self,
        evidence: &mut Evidence,
        action: &CampaignActionResult<Self>,
        _: u64,
        _: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Input<ButtonChord>, Box<dyn Error>>,
    {
        evidence.action_attempts = evidence.action_attempts.saturating_add(1);
        if let Some(index) = BUTTON_MASKS
            .iter()
            .position(|mask| *mask == action.action.buttons)
        {
            evidence.actions_by_mask[index] = evidence.actions_by_mask[index].saturating_add(1);
        }
        merge_milestones(&mut evidence.milestones, action.milestones);
        evidence.objective_observations = evidence
            .objective_observations
            .saturating_add(u64::from(action.outcome.objective_reached));
        for observation in &action.observations {
            record_observation(evidence, observation);
        }
        if self.mode == Mode::History
            && self.broken
            && let (Some(candidate), Some(observation)) =
                (&action.candidate, action.observations.last())
            && observation.wram.len() == WRAM_SIZE
            && let Ok(wram) = <&[u8; WRAM_SIZE]>::try_from(observation.wram.as_slice())
        {
            note_history_state(
                evidence,
                candidate.key,
                wram[HISTORY_A_OFFSET] == wram[HISTORY_B_OFFSET],
            );
        }
        Ok(())
    }

    fn source_entries<'a>(
        &self,
        source: &'a CalibrationArchiveReport,
    ) -> &'a [SmbArchiveEntryReport] {
        &source.base.entries
    }

    fn resume_input(
        &self,
        source: &CalibrationArchiveReport,
    ) -> Result<Input<ButtonChord>, Box<dyn Error>> {
        self.base.resume_input(&source.base)
    }
}

impl CalibrationGame {
    fn campaign_config(&self, seed: u64, work_budget: u64) -> CampaignConfig<Self> {
        CampaignConfig {
            campaign_seed: seed,
            workers: 1,
            execution_budget: work_budget,
            action_limit: ACTION_LIMIT,
            host: "smb-local-calibration".to_owned(),
            wall_budget: None,
            stop_rollout_on_objective: true,
            stop_campaign_on_objective: true,
            archive_entry_limit: ARCHIVE_LIMIT,
            reservations_per_worker: 1,
            memory_budget_mib: Some(MEMORY_BUDGET_MIB),
            materialize_final_artifacts: true,
            run: (),
            suffix: SuffixShape::OneToSixBounded,
            mixture: DrawMixture::BiasedHalf,
            retention: RetentionPolicy::Unprobed,
            objective_witness_path: None,
        }
    }
}

#[derive(Default)]
struct RetainedHistorySummary {
    entries: u64,
    on_path: u64,
    off_path: u64,
    collision_keys: u64,
    missing_snapshots: u64,
}

fn retained_history_summary(
    game: &CalibrationGame,
    target: &mut SmbTarget,
    report: &CalibrationArchiveReport,
    checkpoint: &SnapshotCheckpoint<SmbSnapshot>,
) -> Result<RetainedHistorySummary, Box<dyn Error>> {
    let snapshots = checkpoint
        .entries
        .iter()
        .map(|entry| (entry.id, &entry.snapshot))
        .collect::<BTreeMap<_, _>>();
    let mut keys = BTreeMap::<LossyHistoryKey, u8>::new();
    let mut summary = RetainedHistorySummary::default();
    for entry in &report.base.entries {
        summary.entries = summary.entries.saturating_add(1);
        let Some(snapshot) = snapshots.get(&entry.id) else {
            summary.missing_snapshots = summary.missing_snapshots.saturating_add(1);
            continue;
        };
        game.restore(target, snapshot)?;
        let wram = target.wram();
        let on_path = wram[HISTORY_A_OFFSET] == wram[HISTORY_B_OFFSET];
        if on_path {
            summary.on_path = summary.on_path.saturating_add(1);
        } else {
            summary.off_path = summary.off_path.saturating_add(1);
        }
        let signature = lossy_history_key(entry.key);
        let flag = if on_path { 1 } else { 2 };
        if let Some(previous) = keys.get_mut(&signature) {
            let updated = *previous | flag;
            if *previous != 3 && updated == 3 {
                summary.collision_keys = summary.collision_keys.saturating_add(1);
            }
            *previous = updated;
        } else {
            keys.insert(signature, flag);
        }
    }
    Ok(summary)
}

fn sum_stream_work(bytes: &[u8]) -> Result<u64, Box<dyn Error>> {
    let mut work = 0_u64;
    for line in bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let value: Value = serde_json::from_slice(line)?;
        if value.get("event").and_then(Value::as_str) == Some("job") {
            work = work
                .checked_add(
                    value
                        .get("execution_work")
                        .and_then(Value::as_u64)
                        .ok_or("campaign job record is missing execution_work")?,
                )
                .ok_or("campaign stream work total overflowed")?;
        }
    }
    Ok(work)
}

fn verify_witness(
    game: &CalibrationGame,
    target: &mut SmbTarget,
    root: &SmbSnapshot,
    witness: &Input<ButtonChord>,
) -> Result<u64, Box<dyn Error>> {
    game.restore(target, root)?;
    let mut milestones = SmbMilestones::default();
    let mut frames = 0_u64;
    for action in &witness.actions {
        frames = frames
            .checked_add(u64::from(action.bounded_hold_frames()))
            .ok_or("witness frame count overflowed")?;
        game.apply_action(target, action, &mut milestones)?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("objective witness caused an emulator failure".into());
        }
    }
    if !game.objective_reached(&(), target)? {
        return Err("objective witness does not reach the raw-RAM objective from its root".into());
    }
    Ok(frames)
}

#[allow(clippy::disallowed_methods)]
fn calibration(request_path: &Path, output_dir: &Path) -> Result<Value, Box<dyn Error>> {
    let total_started = Instant::now();
    let request: Request = serde_json::from_slice(&read_capped(request_path, REQUEST_LIMIT)?)?;
    validate_request(&request)?;
    let (prefix, prefix_frames, prefix_sha256) = read_prefix(&request.prefix_input)?;
    let rom = read_capped(&request.rom_path, ROM_FILE_LIMIT)?;
    let rom_sha256 = format!("{:x}", Sha256::digest(&rom));
    let rom_size = u64::try_from(rom.len())?;
    check_digest(&request.rom_sha256, &rom_sha256, "ROM")?;
    let (core_sha256, core_size) = hash_file(&request.core_path, CORE_FILE_LIMIT)?;
    check_digest(&request.core_sha256, &core_sha256, "emulator core")?;

    let base = SmbGame::new(&rom, &request.core_path, &core_sha256);
    let game = CalibrationGame {
        base,
        mode: request.mode,
        broken: request.broken,
        hold_frames: request.action_hold_frames,
        objective: request.objective,
        rom_sha256: rom_sha256.clone(),
        core_sha256: core_sha256.clone(),
        source_revision: request.source_revision.clone(),
    };
    let mut root_target = game
        .new_target()
        .map_err(|error| -> Box<dyn Error> { error.into() })?;
    let mut prefix_milestones = SmbMilestones::default();
    for action in &prefix.actions {
        game.apply_action(&mut root_target, action, &mut prefix_milestones)?;
        if root_target.exit_kind() != ExitKind::Ok || root_target.is_dead() {
            return Err("recorded prefix reached death or emulator failure before its root".into());
        }
    }
    let root_wram = root_target.wram();
    if !root_matches(&root_wram, request.root_expected) {
        return Err("recorded prefix does not match root_expected raw RAM bytes".into());
    }
    let prefix_replayed_frames = game.execution_work(&root_target);
    let root_snapshot = game.snapshot(&mut root_target)?;
    let root_snapshots = SnapshotCheckpoint {
        format: game.checkpoint_format().to_owned(),
        entries: vec![SnapshotCheckpointEntry {
            id: 0,
            snapshot: root_snapshot.clone(),
        }],
    };
    let root_checkpoint = CampaignCheckpoint {
        path: "private-calibration-prefix-root".to_owned(),
        file_sha256: postcard_value_sha256(&root_snapshots)?,
        snapshots: root_snapshots,
    };
    let origin = CampaignOrigin::SnapshotRoot {
        checkpoint: root_checkpoint.clone(),
    };

    let mut stream = BoundedStream(Vec::new());
    let campaign_started = Instant::now();
    let outcome = run_campaign_checkpointed_with_options(
        &game,
        &game.campaign_config(request.seed, request.work_budget_frames),
        &origin,
        &mut stream,
        None,
        CampaignExecutionOptions {
            work_budget: Some(request.work_budget_frames),
            ..Default::default()
        },
    )?;
    let campaign_seconds = campaign_started.elapsed().as_secs_f64();
    let campaign_work = sum_stream_work(&stream.0)?;
    let report = &outcome.0;
    if campaign_work != report.execution_work {
        return Err("independent campaign stream work sum does not match the engine report".into());
    }
    let overshoot = campaign_work.saturating_sub(request.work_budget_frames);
    let overshoot_limit = u64::from(request.action_hold_frames) * 127;
    if overshoot > overshoot_limit {
        return Err(
            format!("campaign work overshoot {overshoot} exceeds {overshoot_limit}").into(),
        );
    }
    let replay_started = Instant::now();
    let replay = replay_campaign_checkpointed(&game, &stream.0, None, Some(&root_checkpoint))?;
    let replay_seconds = replay_started.elapsed().as_secs_f64();
    if replay != outcome {
        return Err("campaign replay from the checked prefix root diverged".into());
    }
    let witness_frames = if let Some(witness) = &report.objective_witness {
        Some(verify_witness(
            &game,
            &mut root_target,
            &root_snapshot,
            witness,
        )?)
    } else {
        None
    };
    if report.work_to_first_objective.is_some() != witness_frames.is_some() {
        return Err("campaign first-objective work and witness presence disagree".into());
    }
    let retained = retained_history_summary(&game, &mut root_target, &report.archive, &outcome.1)?;
    let stream_sha256 = format!("{:x}", Sha256::digest(&stream.0));

    fs::create_dir_all(output_dir)?;
    let summary_path = output_dir.join("summary.json");
    let witness_path = output_dir.join("witness-input.json");
    let stream_path = output_dir.join("campaign.jsonl");
    for path in [&summary_path, &witness_path, &stream_path] {
        if path.exists() {
            return Err("an output file with the requested name already exists".into());
        }
    }
    if request.retain_stream {
        fs::write(&stream_path, &stream.0)?;
    }
    if let Some(witness) = &report.objective_witness {
        let witness_bytes = serde_json::to_vec_pretty(witness)?;
        if witness_bytes.len() > 64_000 {
            return Err("objective witness input exceeds its output bound".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&witness_path)?;
        file.write_all(&witness_bytes)?;
        file.write_all(b"\n")?;
    }
    let first_objective_work = report.work_to_first_objective;
    let summary = json!({
        "format": "smb-local-calibration-v1",
        "source_revision_claimed": request.source_revision,
        "rom_sha256": rom_sha256,
        "rom_bytes": rom_size,
        "core_sha256": core_sha256,
        "core_bytes": core_size,
        "emulator_identity": game.base.emulator_identity(),
        "mode": request.mode,
        "broken": request.broken,
        "seed": request.seed,
        "work_budget_frames": request.work_budget_frames,
        "action_hold_frames": request.action_hold_frames,
        "action_masks": masks_for(request.mode, request.broken),
        "mixture": "biased_half",
        "workers": 1,
        "admission_window": 1,
        "action_limit": ACTION_LIMIT,
        "archive_entry_limit": ARCHIVE_LIMIT,
        "memory_budget_mib": MEMORY_BUDGET_MIB,
        "prefix_sha256": prefix_sha256,
        "prefix_actions": prefix.actions.len(),
        "prefix_input_frames": prefix_frames,
        "prefix_replayed_frames": prefix_replayed_frames,
        "root_raw": request.root_expected,
        "objective": request.objective,
        "campaign_frames": campaign_work,
        "work_overshoot_frames": overshoot,
        "work_overshoot_limit_frames": overshoot_limit,
        "first_objective_work_frames": first_objective_work,
        "objective_found": first_objective_work.is_some(),
        "success_censored_by_budget": first_objective_work.is_some_and(|work| work <= request.work_budget_frames),
        "witness_frames_from_prefix_root": witness_frames,
        "campaign_executions": report.executions_completed,
        "campaign_objectives": report.objectives_reached,
        "archive_live_entries": report.live_entries,
        "archive_snapshot_evictions": report.snapshot_evictions,
        "exported_snapshot_history": {
            "entries": retained.entries,
            "raw_loop_on_path": retained.on_path,
            "raw_loop_off_path": retained.off_path,
            "lossy_identity_collision_keys": retained.collision_keys,
            "missing_exported_snapshots": retained.missing_snapshots,
            "scope": "exported archive entries joined to snapshots, including nonselectable ancestry; not active portfolio retention"
        },
        "workload_identity_sha256": game.workload_identity_sha256(),
        "campaign_diagnostics": report.archive.diagnostics,
        "campaign_stream_sha256": stream_sha256,
        "campaign_stream_bytes": stream.0.len(),
        "campaign_stream_retained": request.retain_stream,
        "campaign_replay_verified": true,
        "witness_verified_from_prefix_root": witness_frames.is_some(),
        "campaign_seconds": campaign_seconds,
        "replay_seconds": replay_seconds,
        "elapsed_seconds": total_started.elapsed().as_secs_f64(),
        "private_paths_emitted": false
    });
    let mut summary_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&summary_path)?;
    serde_json::to_writer_pretty(&mut summary_file, &summary)?;
    summary_file.write_all(b"\n")?;
    Ok(summary)
}

fn usage() -> &'static str {
    "usage: smb-calibrate <strict-request.json> <new-or-empty-output-directory>\n\
     Request fields: rom_path, rom_sha256, core_path, core_sha256, prefix_input, mode, broken,\n\
     seed, work_budget_frames, root_expected, objective, action_hold_frames; optional fields:\n\
     source_revision, retain_stream."
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && args[0] == "--help" {
        println!("{}", usage());
        return Ok(());
    }
    if args.len() != 2 {
        return Err(usage().into());
    }
    let summary = calibration(Path::new(&args[0]), Path::new(&args[1]))?;
    println!("{}", serde_json::to_string(&summary)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SmbArchiveKey {
        SmbArchiveKey {
            world: 6,
            level: 3,
            progress: 18,
            player_y_bucket: 2,
            loop_on_path: true,
            room_x_bucket: 4,
            time_bucket: 1,
            clock: 321,
            room: [3, 8, 1],
        }
    }

    fn request_json() -> Value {
        json!({
            "rom_path": "private.nes",
            "rom_sha256": "a".repeat(64),
            "core_path": "private.dylib",
            "core_sha256": "b".repeat(64),
            "prefix_input": "prefix.json",
            "mode": "history",
            "broken": false,
            "seed": 17,
            "work_budget_frames": 20000,
            "root_expected": {"world": 6, "level": 3, "area_type": 3},
            "objective": {"world": 6, "level": 3, "area_type": 3, "absolute_player_x_min": 1280},
            "action_hold_frames": 8
        })
    }

    #[test]
    fn request_schema_rejects_unknown_fields_at_each_level() {
        let bytes = serde_json::to_vec(&request_json()).expect("request JSON");
        let parsed: Request = serde_json::from_slice(&bytes).expect("strict request");
        validate_request(&parsed).expect("bounded request");
        let mut extra = request_json();
        extra["unregistered"] = json!(true);
        assert!(serde_json::from_value::<Request>(extra).is_err());
        let mut nested = request_json();
        nested["objective"]["use_decoded_alias"] = json!(true);
        assert!(serde_json::from_value::<Request>(nested).is_err());
    }

    #[test]
    fn history_control_only_clears_the_existing_loop_path_bit() {
        let original = key();
        assert_eq!(effective_key(Mode::History, false, original), original);
        let broken = effective_key(Mode::History, true, original);
        assert!(!broken.loop_on_path);
        assert_eq!(
            SmbArchiveKey {
                loop_on_path: true,
                ..broken
            },
            original
        );
        assert_eq!(effective_key(Mode::Actions, true, original), original);
    }

    #[test]
    fn action_control_changes_only_the_declared_action_alphabet() {
        assert_eq!(
            masks_for(Mode::History, false),
            masks_for(Mode::History, true)
        );
        assert_eq!(masks_for(Mode::Actions, false), &BUTTON_MASKS);
        assert_eq!(masks_for(Mode::Actions, true), &[128]);
    }

    #[test]
    fn raw_objective_requires_the_declared_ram_fields_and_absolute_x() {
        let mut wram = [0_u8; WRAM_SIZE];
        wram[WORLD_OFFSET] = 6;
        wram[LEVEL_OFFSET] = 3;
        wram[AREA_TYPE_OFFSET] = 3;
        wram[PLAYER_X_PAGE_OFFSET] = 5;
        wram[PLAYER_X_OFFSET] = 0;
        let objective = Objective {
            world: 6,
            level: 3,
            area_type: 3,
            absolute_player_x_min: 1280,
        };
        assert!(objective_matches(&wram, objective));
        wram[0x00b5] = 2;
        assert!(!objective_matches(&wram, objective));
        wram[0x00b5] = 0;
        wram[PLAYER_X_OFFSET] = 255;
        assert!(!objective_matches(
            &wram,
            Objective {
                absolute_player_x_min: 1536,
                ..objective
            }
        ));
        wram[AREA_TYPE_OFFSET] = 4;
        assert!(!objective_matches(&wram, objective));
    }

    #[test]
    fn lossy_collision_diagnostic_normalizes_only_history_flag() {
        let on_path = key();
        let off_path = SmbArchiveKey {
            loop_on_path: false,
            ..on_path
        };
        assert_eq!(lossy_history_key(on_path), lossy_history_key(off_path));
        let shifted = SmbArchiveKey {
            progress: 30,
            ..off_path
        };
        assert_ne!(lossy_history_key(on_path), lossy_history_key(shifted));
    }
}
