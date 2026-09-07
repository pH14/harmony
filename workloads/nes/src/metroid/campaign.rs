// SPDX-License-Identifier: AGPL-3.0-or-later

//! Metroid implementation of the game-neutral campaign interface.

use std::{
    collections::BTreeSet,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    metroid::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MAX_METROID_ACTIONS, MetroidArchiveKey,
            MetroidArchiveReport, MetroidMilestoneInputs, MetroidMilestoneTimes, MetroidMilestones,
            MetroidProgressWatermark, REPLACEMENT_IDENTIFIER, archive_key, chord_time,
            merge_milestones, merge_progress_watermark, milestone_key, milestones,
            progress_watermark, sample_chord,
        },
        target::{
            ButtonChord, MetroidInput, MetroidObservations, MetroidSnapshot, MetroidTarget,
            power_on_walk, preference_tuple,
        },
    },
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignConfig, CampaignJobResult, CampaignModeReport, CampaignOrigin,
            CampaignProgressRecord, CampaignStreamHeader, CampaignTypes, Evaluation, GamePolicies,
            InputPolicy, Reporting, SnapshotCheckpoint, TargetExecution, postcard_value_sha256,
            replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    },
    target::{ExitKind, Target},
};

/// Stream format written by Metroid campaigns.
pub const CAMPAIGN_STREAM_FORMAT: &str = "metroid-quicknes-campaign-stream-v1";
/// Snapshot checkpoint format written by Metroid campaigns.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "metroid-quicknes-snapshot-checkpoint-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_select_taps_no_start_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "death_or_ending_v2";

type MetroidPreference = (u8, u8, u16, u8);
type MetroidChampionKey = (MetroidProgressWatermark, MetroidPreference);

/// Header placeholder for a game with no adaptive draw table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MetroidNoTableHeader;

/// ROM and emulator identity shared by Metroid workers.
pub struct MetroidGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    prefix: Vec<ButtonChord>,
    identity: String,
    champion_input_path: Option<PathBuf>,
}

impl MetroidGame {
    /// Build a game context whose sealed genesis is a new game from
    /// power-on.
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        Self::new_after(rom, core_path, core_sha256, power_on_walk())
    }

    /// Build a game context whose sealed genesis follows `prefix` from
    /// power-on.
    #[must_use]
    pub fn new_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: Vec<ButtonChord>,
    ) -> Self {
        let mut prefix_digest = Sha256::new();
        for chord in &prefix {
            prefix_digest.update([chord.buttons, chord.hold_frames]);
        }
        let identity = format!(
            "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;\
             genesis=metroid-new-game-v1:prefix-sha256={:x};\
             image=cartridge-ram-declared-v1;\
             result_digest=metroid-semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}",
            machine::quicknes::QUICKNES_REVISION,
            machine::quicknes::QUICKNES_BUILD,
            machine::quicknes::QUICKNES_OPTIONS,
            prefix_digest.finalize(),
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            prefix,
            identity,
            champion_input_path: None,
        }
    }

    /// Write the champion input to `path` each time it improves, so a long
    /// run can be filmed at its deepest point before it ends.
    #[must_use]
    pub fn with_champion_input_path(mut self, path: PathBuf) -> Self {
        self.champion_input_path = Some(path);
        self
    }

    fn publish_champion(&self, input: &MetroidInput) -> Result<(), Box<dyn Error>> {
        if let Some(path) = &self.champion_input_path {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }
        Ok(())
    }

    /// Inputs run from power-on before genesis is sealed.
    #[must_use]
    pub fn prefix(&self) -> &[ButtonChord] {
        &self.prefix
    }

    /// Pinned emulator identity recorded in streams.
    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

/// Fixed recorded run policy.
#[derive(Clone, Copy, Debug)]
pub struct MetroidCampaignRun;

/// Game-owned campaign evidence.
#[derive(Clone, Default)]
pub struct MetroidCampaignEvidence {
    aggregate: MetroidMilestones,
    watermark: MetroidProgressWatermark,
    first_reached: MetroidMilestoneTimes,
    first_inputs: MetroidMilestoneInputs,
    champion_input: MetroidInput,
    champion_milestones: MetroidMilestones,
    champion_key: Option<MetroidChampionKey>,
    genesis_area: Option<u8>,
}

/// Campaign origin.
pub type MetroidCampaignOrigin = CampaignOrigin<MetroidGame>;
/// Resume checkpoint.
pub type MetroidCampaignCheckpoint = CampaignCheckpoint<MetroidSnapshot>;
/// Whole-tree snapshot checkpoint.
pub type MetroidSnapshotCheckpoint = SnapshotCheckpoint<MetroidSnapshot>;
/// Stream header.
pub type MetroidCampaignStreamHeader = CampaignStreamHeader<MetroidNoTableHeader>;
/// Campaign report.
pub type MetroidCampaignModeReport = CampaignModeReport<ButtonChord, MetroidArchiveReport>;
/// Progress sidecar record.
pub type MetroidCampaignProgressRecord = CampaignProgressRecord<MetroidArchiveKey>;
type MetroidCampaignActionResult = CampaignActionResult<MetroidGame>;
type MetroidCampaignJobResult = CampaignJobResult<MetroidGame>;

#[derive(Serialize)]
struct MetroidResultCandidate<'a> {
    key: &'a MetroidArchiveKey,
    viable: bool,
}

#[derive(Serialize)]
struct MetroidResultAction<'a> {
    action: ButtonChord,
    observations: &'a [MetroidObservations],
    milestones: MetroidMilestones,
    dead: bool,
    victory: bool,
    failed: bool,
    candidate: Option<MetroidResultCandidate<'a>>,
}

#[derive(Serialize)]
struct MetroidResult<'a> {
    actions: Vec<MetroidResultAction<'a>>,
}

fn metroid_result_sha256(result: &MetroidCampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|action| MetroidResultAction {
            action: action.action,
            observations: &action.observations,
            milestones: action.milestones,
            dead: action.dead,
            victory: action.victory,
            failed: action.failed,
            candidate: action
                .candidate
                .as_ref()
                .map(|candidate| MetroidResultCandidate {
                    key: &candidate.key,
                    viable: candidate.viable,
                }),
        })
        .collect();
    postcard_value_sha256(&MetroidResult { actions })
}

/// Fixed configuration for one live campaign.
pub struct MetroidCampaignConfig {
    /// Campaign seed.
    pub campaign_seed: u64,
    /// Worker thread count.
    pub workers: u32,
    /// Admitted execution budget.
    pub execution_budget: u64,
    /// Maximum actions in one clean-reset input.
    pub action_limit: usize,
    /// Operator-supplied host label.
    pub host: String,
    /// Optional live-only wall cutoff.
    pub wall_budget: Option<std::time::Duration>,
    /// Live-only: continue issuing reservations after the first victory.
    pub continue_after_victory: bool,
    /// Maximum retained archive entries.
    pub archive_entry_limit: usize,
    /// Deterministic logical-memory budget for live search structures.
    pub memory_budget_mib: Option<usize>,
    /// Live-only: materialize full archive inputs and snapshots at completion.
    pub materialize_final_artifacts: bool,
    /// Admission policy.
    pub retention: RetentionPolicy,
    /// Generic parent selector.
    pub selector: crate::search::archive::SelectorPolicy,
    /// Generic suffix-length shape.
    pub suffix: SuffixShape,
    /// Generic draw mixture.
    pub mixture: DrawMixture,
    /// Live-only path receiving the first item-gaining input.
    pub victory_input_path: Option<PathBuf>,
}

impl MetroidCampaignConfig {
    fn generic(&self) -> CampaignConfig<MetroidGame> {
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
            run: MetroidCampaignRun,
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
        .ok_or_else(|| format!("Metroid stream is missing {field}").into())
}

fn merge_action_milestones(
    aggregate: &mut MetroidMilestones,
    target: &MetroidTarget,
    genesis_items: u8,
    genesis_tanks: u8,
) {
    if target.exit_kind() != ExitKind::Ok {
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(
            aggregate,
            milestones(observation.decoded, genesis_items, genesis_tanks),
        );
    }
}

fn execute_suffix(
    target: &mut MetroidTarget,
    genesis: (u8, u8),
    parent_actions: usize,
    parent_milestones: MetroidMilestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<MetroidCampaignJobResult, Box<dyn Error>> {
    if retention != RetentionPolicy::AdmitAlive {
        return Err("Metroid campaigns admit every live candidate".into());
    }
    let (genesis_items, genesis_tanks) = genesis;
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.is_dead() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut aggregate, target, genesis_items, genesis_tanks);
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = target.is_victory();
        let failed = target.exit_kind() != ExitKind::Ok;
        let candidate = if dead || failed {
            None
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot Metroid suffix")?;
            Some(CampaignCandidate {
                key: archive_key(target.mechanical_state()),
                viable: true,
                snapshot,
            })
        };
        actions.push(CampaignActionResult {
            action: *action,
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
    times: &mut MetroidMilestoneTimes,
    inputs: &mut MetroidMilestoneInputs,
    value: MetroidMilestones,
    genesis_area: u8,
    sequence: u64,
    input: &MetroidInput,
) {
    let left_starting_area = value.areas & !(1_u8 << (genesis_area % 8)) != 0;
    if left_starting_area && times.first_new_area.is_none() {
        times.first_new_area = Some(sequence);
        inputs.first_new_area = Some(input.clone());
    }
    if value.gained && times.first_gain.is_none() {
        times.first_gain = Some(sequence);
        inputs.first_gain = Some(input.clone());
    }
}

/// The action's endpoint as a champion key, or nothing when the endpoint is
/// a death: a dying Samus can reach keys no live input extends, and the
/// champion input exists to be replayed and extended.
fn action_champion_key(observations: &[MetroidObservations]) -> Option<MetroidChampionKey> {
    observations
        .last()
        .filter(|observation| !observation.dead)
        .map(|observation| {
            let state = observation.decoded;
            (progress_watermark(state), preference_tuple(state))
        })
}

impl CampaignTypes for MetroidGame {
    type Target = MetroidTarget;
    type Action = ButtonChord;
    type Key = MetroidArchiveKey;
    type Milestones = MetroidMilestones;
    type Progress = MetroidProgressWatermark;
    type Snapshot = MetroidSnapshot;
    type Observations = MetroidObservations;
    type Evidence = MetroidCampaignEvidence;
    type ArchiveReport = MetroidArchiveReport;
    type Run = MetroidCampaignRun;
    type DrawState = ();
    type TableHeader = MetroidNoTableHeader;
    type DrawCheckpoint = ();
}

impl Reporting for MetroidGame {
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }

    fn image_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rom))
    }

    fn result_sha256(&self, result: &MetroidCampaignJobResult) -> Result<String, Box<dyn Error>> {
        metroid_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &MetroidCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> MetroidArchiveReport {
        MetroidArchiveReport {
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

impl InputPolicy for MetroidGame {
    fn max_action_limit(&self) -> usize {
        MAX_METROID_ACTIONS
    }

    fn longest_action_time(&self) -> u64 {
        u64::from(crate::metroid::archive::LONGEST_HOLD_FRAMES)
    }

    fn draw_state_memory_reserve_bytes(
        &self,
        _run: &MetroidCampaignRun,
        _max_actions: usize,
    ) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn policies(&self, _run: &MetroidCampaignRun) -> GamePolicies {
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
        policies: &GamePolicies,
    ) -> Result<MetroidCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&MetroidCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Metroid stream {field} policy is not recognized").into());
                }
            }
            return Err("Metroid stream carries an unknown game policy".into());
        }
        Ok(MetroidCampaignRun)
    }

    fn initial_draw_state(
        &self,
        _run: &MetroidCampaignRun,
        _origin: Option<(&str, &MetroidArchiveReport)>,
    ) -> Result<((), Option<MetroidNoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(&self, _state: &()) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &MetroidCampaignRun,
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
        run: &MetroidCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&()>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Metroid stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &MetroidCampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn retained_inputs_need_full(&self, _run: &MetroidCampaignRun) -> bool {
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
            Err("Metroid stream requires an unsupported draw-table version".into())
        }
    }
}

impl TargetExecution for MetroidGame {
    fn action_time_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &MetroidSnapshot) -> usize {
        std::mem::size_of::<MetroidSnapshot>().saturating_add(snapshot.emulator_state_bytes_len())
    }

    fn new_target(&self) -> Result<MetroidTarget, String> {
        MetroidTarget::from_rom_bytes_after(
            &self.rom,
            &self.core_path,
            &self.core_sha256,
            &self.prefix,
        )
        .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut MetroidTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut MetroidTarget,
        snapshot: &MetroidSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn frames_clocked(&self, target: &MetroidTarget) -> u64 {
        target.frames_clocked()
    }

    fn apply_action(
        &self,
        target: &mut MetroidTarget,
        action: &ButtonChord,
        aggregate: &mut MetroidMilestones,
    ) -> Result<(), Box<dyn Error>> {
        let (genesis_items, genesis_tanks) = target.genesis_holdings();
        target.apply(action);
        merge_action_milestones(aggregate, target, genesis_items, genesis_tanks);
        Ok(())
    }

    fn snapshot(&self, target: &mut MetroidTarget) -> Result<MetroidSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Metroid".into())
    }

    fn execute_job(
        &self,
        _run: &MetroidCampaignRun,
        target: &mut MetroidTarget,
        origin_snapshot: &MetroidSnapshot,
        replay: &[ButtonChord],
        parent_actions: usize,
        parent_milestones: MetroidMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<MetroidCampaignJobResult, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(
            target,
            target.genesis_holdings(),
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            retention,
        )
    }
}

impl Evaluation for MetroidGame {
    fn is_terminal(&self, target: &MetroidTarget) -> bool {
        target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        _run: &MetroidCampaignRun,
        target: &MetroidTarget,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok {
            return Err("Metroid terminal predicate cannot inspect a failed emulator".into());
        }
        Ok(target.is_dead() || target.is_victory())
    }

    fn current_key(&self, target: &MetroidTarget) -> Result<MetroidArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.mechanical_state()))
    }

    fn complete_candidate_key(
        &self,
        key: MetroidArchiveKey,
        _snapshot: &MetroidSnapshot,
    ) -> Result<MetroidArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut MetroidMilestones, from: MetroidMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &MetroidCampaignEvidence) -> MetroidMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &MetroidCampaignEvidence) -> MetroidProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut MetroidCampaignEvidence,
        source: &MetroidArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut MetroidCampaignEvidence,
        target: &MetroidTarget,
    ) -> Result<(), Box<dyn Error>> {
        let state = target.mechanical_state();
        evidence.watermark = evidence.watermark.max(progress_watermark(state));
        evidence.genesis_area.get_or_insert(state.area);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut MetroidCampaignEvidence,
        value: MetroidMilestones,
        input: &MetroidInput,
    ) {
        merge_milestones(&mut evidence.aggregate, value);
        let genesis_area = evidence.genesis_area.unwrap_or(0);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            value,
            genesis_area,
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
        evidence: &mut MetroidCampaignEvidence,
        action: &MetroidCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<MetroidInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let genesis_area = evidence.genesis_area.unwrap_or(0);
        let left_starting_area = action.milestones.areas & !(1_u8 << (genesis_area % 8)) != 0;
        let first_input_needed = (left_starting_area
            && evidence.first_inputs.first_new_area.is_none())
            || (action.milestones.gained && evidence.first_inputs.first_gain.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed || champion.is_some() {
            let input = input()?;
            update_first_inputs(
                &mut evidence.first_reached,
                &mut evidence.first_inputs,
                action.milestones,
                genesis_area,
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

    fn source_entries<'a>(
        &self,
        source: &'a MetroidArchiveReport,
    ) -> &'a [crate::metroid::archive::MetroidArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &MetroidArchiveReport) -> Result<MetroidInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Metroid source archive has no retained entries".into())
    }
}

/// Run a campaign and return its report plus whole-tree checkpoint.
pub fn run_metroid_campaign_checkpointed(
    game: &MetroidGame,
    config: &MetroidCampaignConfig,
    origin: &MetroidCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(MetroidCampaignModeReport, MetroidSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

/// Replay a recorded stream exactly.
pub fn replay_metroid_campaign_checkpointed(
    game: &MetroidGame,
    stream_bytes: &[u8],
    origin_report: Option<&MetroidArchiveReport>,
    origin_checkpoint: Option<&MetroidCampaignCheckpoint>,
) -> Result<(MetroidCampaignModeReport, MetroidSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}
