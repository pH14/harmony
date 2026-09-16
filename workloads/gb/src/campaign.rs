// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use searcher::{
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignConfig, CampaignJobResult, CampaignModeReport, CampaignOrigin,
            CampaignProgressRecord, CampaignStreamHeader, CampaignTypes, Evaluation, InputPolicy,
            Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
            postcard_value_sha256, replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
        rollout::{ExecutionDisposition, Outcome},
    },
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    archive::{
        BlueArchiveEntryReport, BlueArchiveKey, BlueArchiveReport, BlueInput, BlueMilestoneInputs,
        BlueMilestoneTimes, BlueMilestones, BlueProgressWatermark, DURATION_IDENTIFIER,
        KEY_POLICY_IDENTIFIER, MAX_BLUE_ACTIONS, REPLACEMENT_IDENTIFIER, action_time, archive_key,
        longest_action_frames, merge_milestones, merge_progress_watermark, milestone_key,
        milestones, progress_watermark, sample_action,
    },
    progress::NamedProgress,
    target::{BlueAction, BlueObservation, BlueSnapshot, BlueState, BlueTarget},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "blue-gambatte-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "blue-gambatte-snapshot-checkpoint-v1";

const MACRO_VOCABULARY_FIELD: &str = "macro_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const ADVISER_FIELD: &str = "alphabet_adviser";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const MACRO_VOCABULARY_IDENTIFIER: &str =
    "walk_interact_battle_move_item_switch_advance_slot256_v1";
const ADVISER_NONE: &str = "none";

type BluePreference = (u32, u32);
type BlueChampionKey = (BlueProgressWatermark, BluePreference);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BlueNoTableHeader;

pub struct BlueGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    prefix: Vec<machine::gb::ButtonChord>,
    identity: String,
    champion_input_path: Option<PathBuf>,
    milestone_input_dir: Option<PathBuf>,
}

impl BlueGame {
    #[must_use]
    pub fn new(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: Vec<machine::gb::ButtonChord>,
    ) -> Self {
        let mut prefix_digest = Sha256::new();
        for chord in &prefix {
            prefix_digest.update([chord.buttons, chord.hold_frames]);
        }
        let identity = format!(
            "gambatte-libretro:{};{};state=clock-huc3-palette-zero-v1;\
             genesis=blue-new-game-v1:prefix-sha256={:x};\
             result_digest=blue-semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}",
            machine::gambatte::GAMBATTE_REVISION,
            machine::gambatte::GAMBATTE_OPTIONS,
            prefix_digest.finalize(),
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            prefix,
            identity,
            champion_input_path: None,
            milestone_input_dir: None,
        }
    }

    #[must_use]
    pub fn with_champion_input_path(mut self, path: PathBuf) -> Self {
        self.champion_input_path = Some(path);
        self
    }

    #[must_use]
    pub fn with_milestone_input_dir(mut self, directory: PathBuf) -> Self {
        self.milestone_input_dir = Some(directory);
        self
    }

    #[must_use]
    pub fn prefix(&self) -> &[machine::gb::ButtonChord] {
        &self.prefix
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    fn publish_milestones(&self, names: &[&str], input: &BlueInput) -> Result<(), Box<dyn Error>> {
        if names.is_empty() {
            return Ok(());
        }
        if let Some(directory) = &self.milestone_input_dir {
            std::fs::create_dir_all(directory)?;
            for name in names {
                let path = directory.join(format!("{name}.json"));
                let temporary = path.with_extension("json.tmp");
                std::fs::write(&temporary, serde_json::to_vec(input)?)?;
                std::fs::rename(temporary, path)?;
            }
        }
        Ok(())
    }

    fn publish_champion(&self, input: &BlueInput) -> Result<(), Box<dyn Error>> {
        if let Some(path) = &self.champion_input_path {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct BlueCampaignRun;

#[derive(Clone, Default)]
pub struct MapCoverage {
    maps: [u64; 4],
}

impl MapCoverage {
    pub fn observe(&mut self, map: u8) {
        let bit = usize::from(map);
        self.maps[bit / 64] |= 1 << (bit % 64);
    }

    #[must_use]
    pub fn count(&self) -> u32 {
        self.maps.iter().map(|word| word.count_ones()).sum()
    }
}

#[derive(Clone, Default)]
pub struct BlueCampaignEvidence {
    observed_maps: MapCoverage,
    named_progress: NamedProgress,
    aggregate: BlueMilestones,
    watermark: BlueProgressWatermark,
    first_reached: BlueMilestoneTimes,
    first_inputs: BlueMilestoneInputs,
    champion_input: BlueInput,
    champion_milestones: BlueMilestones,
    champion_key: Option<BlueChampionKey>,
    whiteouts: u64,
}

pub type BlueCampaignOrigin = CampaignOrigin<BlueGame>;
pub type BlueCampaignCheckpoint = CampaignCheckpoint<BlueSnapshot>;
pub type BlueSnapshotCheckpoint = SnapshotCheckpoint<BlueSnapshot>;
pub type BlueCampaignStreamHeader = CampaignStreamHeader<BlueNoTableHeader>;
pub type BlueCampaignModeReport = CampaignModeReport<BlueAction, BlueArchiveReport>;
pub type BlueCampaignProgressRecord = CampaignProgressRecord<BlueArchiveKey>;
type BlueCampaignActionResult = CampaignActionResult<BlueGame>;
type BlueCampaignJobResult = CampaignJobResult<BlueGame>;

#[derive(Serialize)]
struct BlueResultCandidate<'a> {
    key: &'a BlueArchiveKey,
    viable: bool,
}

#[derive(Serialize)]
struct BlueResultAction<'a> {
    action: BlueAction,
    observations: &'a [BlueObservation],
    milestones: BlueMilestones,
    outcome: Outcome,
    candidate: Option<BlueResultCandidate<'a>>,
}

#[derive(Serialize)]
struct BlueResult<'a> {
    actions: Vec<BlueResultAction<'a>>,
}

fn blue_result_sha256(result: &BlueCampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|action| BlueResultAction {
            action: action.action,
            observations: &action.observations,
            milestones: action.milestones,
            outcome: action.outcome,
            candidate: action
                .candidate
                .as_ref()
                .map(|candidate| BlueResultCandidate {
                    key: &candidate.key,
                    viable: candidate.viable,
                }),
        })
        .collect();
    postcard_value_sha256(&BlueResult { actions })
}

pub struct BlueCampaignConfig {
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
    pub selector: searcher::search::archive::SelectorPolicy,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<PathBuf>,
}

impl BlueCampaignConfig {
    fn generic(&self) -> CampaignConfig<BlueGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: !self.continue_after_victory,
            stop_campaign_on_objective: !self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker:
                searcher::search::campaign::DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: BlueCampaignRun,
            suffix: self.suffix,
            mixture: self.mixture,
            retention: self.retention,
            selector: self.selector.clone(),
            objective_witness_path: self.victory_input_path.clone(),
        }
    }
}

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Blue stream is missing {field}").into())
}

fn merge_action_milestones(aggregate: &mut BlueMilestones, target: &BlueTarget) {
    if target.exit_kind() != ExitKind::Ok {
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation.state));
    }
}

fn disposition_of(target: &BlueTarget) -> ExecutionDisposition {
    if target.exit_kind() != ExitKind::Ok {
        ExecutionDisposition::Failed
    } else if target.is_dead() {
        ExecutionDisposition::Terminal
    } else {
        ExecutionDisposition::Runnable
    }
}

fn execute_suffix(
    target: &mut BlueTarget,
    parent_actions: usize,
    parent_milestones: BlueMilestones,
    suffix: &[BlueAction],
    max_actions: usize,
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<BlueCampaignJobResult, Box<dyn Error>> {
    if retention != RetentionPolicy::Unprobed {
        return Err("Blue campaigns admit every live candidate".into());
    }
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_disposition = disposition_of(target);
    let mut objective_seen =
        parent_disposition != ExecutionDisposition::Failed && target.is_victory();
    if parent_disposition.is_terminal() {
        return Ok(CampaignJobResult {
            preparation_failure: None,
            actions,
        });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut aggregate, target);
        let disposition = disposition_of(target);
        let failed = disposition == ExecutionDisposition::Failed;
        let raw_objective = !failed && target.is_victory();
        let objective_reached = raw_objective && !objective_seen;
        objective_seen |= raw_objective;
        let outcome = Outcome {
            objective_reached,
            disposition,
        };
        let observations = if failed {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        };
        let candidate = if matches!(disposition, ExecutionDisposition::Runnable) {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot the Blue suffix")?;
            Some(CampaignCandidate {
                key: archive_key(target.state()),
                viable: true,
                snapshot,
            })
        } else {
            None
        };
        actions.push(CampaignActionResult {
            action: *action,
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
    times: &mut BlueMilestoneTimes,
    inputs: &mut BlueMilestoneInputs,
    value: BlueMilestones,
    sequence: u64,
    input: &BlueInput,
) {
    if value.flags & 0x7f != 0 && times.first_route_flag.is_none() {
        times.first_route_flag = Some(sequence);
        inputs.first_route_flag = Some(input.clone());
    }
    if value.flags & 0x80 != 0 && times.first_badge.is_none() {
        times.first_badge = Some(sequence);
        inputs.first_badge = Some(input.clone());
    }
}

fn action_champion_key(observations: &[BlueObservation]) -> Option<BlueChampionKey> {
    observations
        .last()
        .filter(|observation| !observation.dead)
        .map(|observation| {
            let state = observation.state;
            (
                progress_watermark(state),
                (state.party_levels(), state.party_hp()),
            )
        })
}

impl CampaignTypes for BlueGame {
    type Target = BlueTarget;
    type Action = BlueAction;
    type Key = BlueArchiveKey;
    type Milestones = BlueMilestones;
    type Progress = BlueProgressWatermark;
    type Snapshot = BlueSnapshot;
    type Observations = BlueObservation;
    type Evidence = BlueCampaignEvidence;
    type ArchiveReport = BlueArchiveReport;
    type Run = BlueCampaignRun;
    type DrawState = ();
    type DrawHeader = BlueNoTableHeader;
    type DrawCheckpoint = ();
}

impl Reporting for BlueGame {
    fn diagnostics(evidence: &BlueCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "maps_visited": evidence.observed_maps.count(),
            "milestones_reached": evidence.named_progress.reached(),
            "named_progress": evidence.named_progress,
            "whiteouts": evidence.whiteouts,
            "observation_filter": "live gameplay observations supplied to this accumulator"
        }))
    }

    fn retained_diagnostics<'a>(
        snapshots: impl Iterator<Item = (Option<&'a BlueSnapshot>, u64)>,
    ) -> Option<serde_json::Value> {
        let (mut active, mut missing) = (0_u64, 0_u64);
        let mut maps = MapCoverage::default();
        let mut places = BTreeMap::<(u8, u8, u8), [u64; 4]>::new();
        let mut best_flags = 0_u8;
        for (snapshot, selections) in snapshots {
            active += 1;
            let Some(snapshot) = snapshot else {
                missing += 1;
                continue;
            };
            let state = snapshot.state();
            best_flags |= state.milestone_flags();
            maps.observe(state.map);
            let place = places
                .entry((state.map, state.x / 2, state.y / 2))
                .or_default();
            *place = [
                place[0] + 1,
                place[1].max(u64::from(state.party_hp())),
                place[2].max(u64::from(state.party_levels())),
                place[3].saturating_add(selections),
            ];
        }
        Some(serde_json::json!({
            "scope": "union over cached active endpoints; not one trajectory; lower bounds when snapshots are missing",
            "active_entries": active,
            "missing_snapshots": missing,
            "milestone_flag_union": best_flags,
            "maps_retained_cached": maps.count(),
            "live_entries_by_place": places
                .iter()
                .map(|((map, x, y), best)| (format!("{map}:{x}:{y}"), *best))
                .collect::<BTreeMap<_, _>>(),
            "live_entries_by_place_format": "map:cell_x:cell_y -> [entries, max party hp, max party levels, selections]"
        }))
    }

    fn merge_witness_diagnostics(
        evidence: &mut BlueCampaignEvidence,
        observations: &[BlueObservation],
        sequence: u64,
    ) {
        let action_end = observations.last().map_or(0, |value| value.frame_count);
        for observation in observations {
            evidence
                .named_progress
                .observe(observation, sequence, action_end);
            if !observation.dead {
                evidence.observed_maps.observe(observation.state.map);
            }
        }
    }

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

    fn result_sha256(&self, result: &BlueCampaignJobResult) -> Result<String, Box<dyn Error>> {
        blue_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &BlueCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> BlueArchiveReport {
        BlueArchiveReport {
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
            whiteouts: state
                .terminal_endpoints
                .saturating_sub(state.terminal_objectives),
            selector: state.selector,
        }
    }
}

impl InputPolicy for BlueGame {
    fn max_action_limit(&self) -> usize {
        MAX_BLUE_ACTIONS
    }

    fn max_action_cost(&self) -> u64 {
        longest_action_frames()
    }

    fn draw_state_memory_reserve_bytes(&self, _run: &BlueCampaignRun, _actions: usize) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn policies(&self, _run: &BlueCampaignRun) -> WorkloadPolicies {
        [
            (MACRO_VOCABULARY_FIELD, MACRO_VOCABULARY_IDENTIFIER),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (ADVISER_FIELD, ADVISER_NONE),
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
    ) -> Result<BlueCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&BlueCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Blue stream {field} policy is not recognized").into());
                }
            }
            return Err("Blue stream carries an unknown game policy".into());
        }
        Ok(BlueCampaignRun)
    }

    fn initial_draw_state(
        &self,
        _run: &BlueCampaignRun,
        _origin: Option<(&str, &BlueArchiveReport)>,
    ) -> Result<((), Option<BlueNoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(&self, _state: &()) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &BlueCampaignRun,
        _state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<BlueAction>, Box<dyn Error>> {
        draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            mutation_seed,
            |_| Ok(None),
            sample_action,
        )
    }

    fn expand_suffix_recorded(
        &self,
        run: &BlueCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&()>,
        mutation_seed: u64,
    ) -> Result<Vec<BlueAction>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Blue stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &BlueCampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[BlueAction])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn retained_inputs_need_full(&self, _run: &BlueCampaignRun) -> bool {
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
            Err("Blue stream requires an unsupported draw-table version".into())
        }
    }
}

impl TargetExecution for BlueGame {
    fn action_cost_fn(&self) -> fn(&BlueAction) -> u64 {
        action_time
    }

    fn snapshot_memory_charge(snapshot: &BlueSnapshot) -> usize {
        std::mem::size_of::<BlueSnapshot>().saturating_add(snapshot.emulator_state_bytes_len())
    }

    fn new_target(&self) -> Result<BlueTarget, String> {
        BlueTarget::from_rom_bytes_after(
            &self.rom,
            &self.core_path,
            &self.core_sha256,
            &self.prefix,
        )
        .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut BlueTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut BlueTarget,
        snapshot: &BlueSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &BlueTarget) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut BlueTarget,
        action: &BlueAction,
        aggregate: &mut BlueMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target);
        Ok(())
    }

    fn snapshot(&self, target: &mut BlueTarget) -> Result<BlueSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Pokemon Blue".into())
    }

    fn execute_job(
        &self,
        _run: &BlueCampaignRun,
        target: &mut BlueTarget,
        origin_snapshot: &BlueSnapshot,
        replay: &[BlueAction],
        parent_actions: usize,
        parent_milestones: BlueMilestones,
        suffix: &[BlueAction],
        max_actions: usize,
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<BlueCampaignJobResult, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
            if disposition_of(target).is_terminal() {
                break;
            }
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

impl Evaluation for BlueGame {
    fn execution_disposition(&self, target: &BlueTarget) -> ExecutionDisposition {
        disposition_of(target)
    }

    fn objective_reached(
        &self,
        _run: &BlueCampaignRun,
        target: &BlueTarget,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.is_victory())
    }

    fn current_key(&self, target: &BlueTarget) -> Result<BlueArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.state()))
    }

    fn complete_candidate_key(
        &self,
        key: BlueArchiveKey,
        _snapshot: &BlueSnapshot,
    ) -> Result<BlueArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut BlueMilestones, from: BlueMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &BlueCampaignEvidence) -> BlueMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &BlueCampaignEvidence) -> BlueProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut BlueCampaignEvidence,
        source: &BlueArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut BlueCampaignEvidence,
        target: &BlueTarget,
    ) -> Result<(), Box<dyn Error>> {
        let state: BlueState = target.state();
        evidence.watermark = evidence.watermark.max(progress_watermark(state));
        evidence.observed_maps.observe(state.map);
        let observation = target.observe();
        evidence
            .named_progress
            .observe(&observation, 0, observation.frame_count);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut BlueCampaignEvidence,
        value: BlueMilestones,
        input: &BlueInput,
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
        evidence: &mut BlueCampaignEvidence,
        action: &BlueCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<BlueInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        let mut discoveries = Vec::new();
        let action_end_frame = action
            .observations
            .last()
            .map_or(0, |value| value.frame_count);
        for observation in &action.observations {
            discoveries.extend(evidence.named_progress.observe(
                observation,
                sequence,
                action_end_frame,
            ));
            if observation.dead {
                evidence.whiteouts = evidence.whiteouts.saturating_add(1);
            } else {
                evidence.observed_maps.observe(observation.state.map);
            }
        }
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let first_input_needed = (action.milestones.flags & 0x7f != 0
            && evidence.first_inputs.first_route_flag.is_none())
            || (action.milestones.flags & 0x80 != 0 && evidence.first_inputs.first_badge.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed || champion.is_some() || !discoveries.is_empty() {
            let input = input()?;
            self.publish_milestones(&discoveries, &input)?;
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
                self.publish_champion(&input)?;
                evidence.champion_input = input;
            }
        }
        Ok(())
    }

    fn source_entries<'a>(&self, source: &'a BlueArchiveReport) -> &'a [BlueArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &BlueArchiveReport) -> Result<BlueInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Blue source archive has no retained entries".into())
    }
}

pub fn run_blue_campaign_checkpointed(
    game: &BlueGame,
    config: &BlueCampaignConfig,
    origin: &BlueCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(BlueCampaignModeReport, BlueSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_blue_campaign_checkpointed(
    game: &BlueGame,
    stream_bytes: &[u8],
    origin_report: Option<&BlueArchiveReport>,
    origin_checkpoint: Option<&BlueCampaignCheckpoint>,
) -> Result<(BlueCampaignModeReport, BlueSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::ActionKind;

    fn game() -> BlueGame {
        BlueGame::new(&[0], Path::new("unused"), "unused", Vec::new())
    }

    #[test]
    fn a_stream_from_another_policy_is_refused() {
        let game = game();
        let good = game.policies(&BlueCampaignRun);
        assert!(game.resolve_recorded(&good).is_ok());
        let mut altered = good.clone();
        altered.insert(KEY_POLICY_FIELD.to_owned(), "something_else".to_owned());
        assert!(game.resolve_recorded(&altered).is_err());
        let mut missing = good;
        missing.remove(ADVISER_FIELD);
        assert!(game.resolve_recorded(&missing).is_err());
    }

    #[test]
    fn a_discovery_reconstructs_its_input_once() {
        let state = BlueState {
            route_flags: 1,
            ..BlueState::default()
        };
        let observation = BlueObservation {
            frame_count: 99,
            state,
            alphabet_size: 3,
            dead: false,
        };
        let action = BlueCampaignActionResult {
            action: BlueAction::new(ActionKind::WalkTo, 0),
            observations: vec![observation],
            milestones: BlueMilestones { flags: 1 },
            outcome: Outcome::default(),
            candidate: None,
        };
        let game = game();
        let mut evidence = BlueCampaignEvidence {
            champion_key: action_champion_key(&action.observations),
            ..Default::default()
        };
        let mut reconstructions = 0;
        game.merge_action_evidence(&mut evidence, &action, 12, || {
            reconstructions += 1;
            Ok(BlueInput::default())
        })
        .unwrap();
        assert_eq!(reconstructions, 1);
        game.merge_action_evidence(&mut evidence, &action, 13, || {
            panic!("an unchanged discovery must not reconstruct again")
        })
        .unwrap();
        assert_eq!(
            evidence.named_progress.first_seen["got_starter"]
                .unwrap()
                .execution,
            12
        );
        assert_eq!(evidence.first_reached.first_route_flag, Some(12));
        assert_eq!(evidence.first_reached.first_badge, None);
    }

    #[test]
    fn the_map_union_counts_each_map_once() {
        let mut coverage = MapCoverage::default();
        for _ in 0..50 {
            coverage.observe(12);
            coverage.observe(255);
        }
        coverage.observe(0);
        assert_eq!(coverage.count(), 3);
    }
}
