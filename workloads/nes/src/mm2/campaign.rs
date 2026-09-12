// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeSet,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    mm2::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MAX_MM2_ACTIONS, Mm2ArchiveKey,
            Mm2ArchiveReport, Mm2MilestoneInputs, Mm2MilestoneTimes, Mm2Milestones,
            Mm2ProgressWatermark, REPLACEMENT_IDENTIFIER, archive_key, chord_time,
            merge_milestones, merge_progress_watermark, milestone_key, milestones,
            progress_watermark, sample_chord,
        },
        target::{
            ButtonChord, Mm2Input, Mm2Observations, Mm2Snapshot, Mm2Stage, Mm2Target,
            power_on_walk, preference_tuple, walk_to_stage_select,
        },
    },
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
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "mm2-quicknes-campaign-stream-v1";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "mm2-quicknes-snapshot-checkpoint-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_start_taps_no_select_v2";
const TERMINAL_POLICY_IDENTIFIER: &str = "death_or_first_boss_defeated";

const VIABILITY_PROBE_MASKS: [u8; 4] = [0, 0x01, 0x80, 0x81];
const VIABILITY_PROBE_FRAMES: u16 = 60;

type Mm2Preference = (u8, u8, u16);
type Mm2ChampionKey = (Mm2ProgressWatermark, Mm2Preference);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Mm2NoTableHeader;

pub struct Mm2Game {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    prefix: Vec<ButtonChord>,
    stage: Mm2Stage,
    identity: String,
    champion_input_path: Option<PathBuf>,
}

impl Mm2Game {
    #[must_use]
    pub fn new_at_stage(rom: &[u8], core_path: &Path, core_sha256: &str, stage: Mm2Stage) -> Self {
        Self::new_at_stage_after(rom, core_path, core_sha256, power_on_walk(), stage)
    }

    #[must_use]
    pub fn new_at_stage_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        prefix: Vec<ButtonChord>,
        stage: Mm2Stage,
    ) -> Self {
        let mut prefix_digest = Sha256::new();
        for chord in &prefix {
            prefix_digest.update([chord.buttons, chord.hold_frames]);
        }
        let identity = format!(
            "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;genesis=mm2-stage-select-v2:{}:prefix-sha256={:x};result_digest=mm2-semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}",
            machine::quicknes::QUICKNES_REVISION,
            machine::quicknes::QUICKNES_BUILD,
            machine::quicknes::QUICKNES_OPTIONS,
            stage.number(),
            prefix_digest.finalize(),
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            prefix,
            stage,
            identity,
            champion_input_path: None,
        }
    }

    #[must_use]
    pub fn with_champion_input_path(mut self, path: PathBuf) -> Self {
        self.champion_input_path = Some(path);
        self
    }

    pub fn walk_to_stage_select(
        &self,
        chords: &[ButtonChord],
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        Ok(walk_to_stage_select(
            &self.rom,
            &self.core_path,
            &self.core_sha256,
            chords,
        )?)
    }

    fn publish_champion(&self, input: &Mm2Input) -> Result<(), Box<dyn Error>> {
        if let Some(path) = &self.champion_input_path {
            std::fs::write(path, serde_json::to_vec_pretty(input)?)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn prefix(&self) -> &[ButtonChord] {
        &self.prefix
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn stage(&self) -> Mm2Stage {
        self.stage
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mm2CampaignRun;

#[derive(Clone, Default)]
pub struct Mm2CampaignEvidence {
    aggregate: Mm2Milestones,
    watermark: Mm2ProgressWatermark,
    first_reached: Mm2MilestoneTimes,
    first_inputs: Mm2MilestoneInputs,
    champion_input: Mm2Input,
    champion_milestones: Mm2Milestones,
    champion_key: Option<Mm2ChampionKey>,
    genesis_screen: Option<u8>,
}

pub type Mm2CampaignOrigin = CampaignOrigin<Mm2Game>;
pub type Mm2CampaignCheckpoint = CampaignCheckpoint<Mm2Snapshot>;
pub type Mm2SnapshotCheckpoint = SnapshotCheckpoint<Mm2Snapshot>;
pub type Mm2CampaignStreamHeader = CampaignStreamHeader<Mm2NoTableHeader>;
pub type Mm2CampaignModeReport = CampaignModeReport<ButtonChord, Mm2ArchiveReport>;
pub type Mm2CampaignProgressRecord = CampaignProgressRecord<Mm2ArchiveKey>;
type Mm2CampaignActionResult = CampaignActionResult<Mm2Game>;
type Mm2CampaignJobResult = CampaignJobResult<Mm2Game>;

#[derive(Serialize)]
struct Mm2ResultCandidate<'a> {
    key: &'a Mm2ArchiveKey,
    viable: bool,
}

#[derive(Serialize)]
struct Mm2ResultAction<'a> {
    action: ButtonChord,
    observations: &'a [Mm2Observations],
    milestones: Mm2Milestones,
    dead: bool,
    victory: bool,
    failed: bool,
    candidate: Option<Mm2ResultCandidate<'a>>,
}

#[derive(Serialize)]
struct Mm2Result<'a> {
    actions: Vec<Mm2ResultAction<'a>>,
}

fn mm2_result_sha256(result: &Mm2CampaignJobResult) -> Result<String, Box<dyn Error>> {
    let actions = result
        .actions
        .iter()
        .map(|action| Mm2ResultAction {
            action: action.action,
            observations: &action.observations,
            milestones: action.milestones,
            dead: action.dead,
            victory: action.victory,
            failed: action.failed,
            candidate: action
                .candidate
                .as_ref()
                .map(|candidate| Mm2ResultCandidate {
                    key: &candidate.key,
                    viable: candidate.viable,
                }),
        })
        .collect();
    postcard_value_sha256(&Mm2Result { actions })
}

pub struct Mm2CampaignConfig {
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

impl Mm2CampaignConfig {
    fn generic(&self) -> CampaignConfig<Mm2Game> {
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
            run: Mm2CampaignRun,
            suffix: self.suffix,
            mixture: self.mixture,
            retention: self.retention,
            selector: self.selector.clone(),
            victory_input_path: self.victory_input_path.clone(),
        }
    }
}

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Mega Man 2 stream is missing {field}").into())
}

fn merge_action_milestones(aggregate: &mut Mm2Milestones, target: &Mm2Target, genesis_weapons: u8) {
    if target.exit_kind() != ExitKind::Ok {
        return;
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation.decoded, genesis_weapons));
    }
}

fn admission_is_viable(
    target: &mut Mm2Target,
    snapshot: &Mm2Snapshot,
) -> Result<bool, Box<dyn Error>> {
    let mut viable = false;
    for mask in VIABILITY_PROBE_MASKS {
        target.restore(snapshot)?;
        if target.survives_probe(mask, VIABILITY_PROBE_FRAMES) {
            viable = true;
            break;
        }
    }
    target.restore(snapshot)?;
    Ok(viable)
}

fn execute_suffix(
    target: &mut Mm2Target,
    genesis_weapons: u8,
    parent_actions: usize,
    parent_milestones: Mm2Milestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<Mm2CampaignJobResult, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.is_dead() || target.defeated_a_boss() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut aggregate, target, genesis_weapons);
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = target.defeated_a_boss();
        let failed = target.exit_kind() != ExitKind::Ok;
        let candidate = if dead || victory || failed {
            None
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot Mega Man 2 suffix")?;
            let viable = match retention {
                RetentionPolicy::ProbeAtAdmission => admission_is_viable(target, &snapshot)?,
                RetentionPolicy::Unprobed => true,
            };
            Some(CampaignCandidate {
                key: archive_key(target.mechanical_state()),
                viable,
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
    times: &mut Mm2MilestoneTimes,
    inputs: &mut Mm2MilestoneInputs,
    value: Mm2Milestones,
    genesis_screen: u8,
    sequence: u64,
    input: &Mm2Input,
) {
    if value.max_screen > genesis_screen && times.first_new_screen.is_none() {
        times.first_new_screen = Some(sequence);
        inputs.first_new_screen = Some(input.clone());
    }
    if value.reached_boss && times.first_boss.is_none() {
        times.first_boss = Some(sequence);
        inputs.first_boss = Some(input.clone());
    }
    if value.defeated_boss && times.first_clear.is_none() {
        times.first_clear = Some(sequence);
        inputs.first_clear = Some(input.clone());
    }
}

fn action_champion_key(observations: &[Mm2Observations]) -> Option<Mm2ChampionKey> {
    observations
        .last()
        .filter(|observation| !observation.dead)
        .map(|observation| {
            let state = observation.decoded;
            (progress_watermark(state), preference_tuple(state))
        })
}

impl CampaignTypes for Mm2Game {
    type Target = Mm2Target;
    type Action = ButtonChord;
    type Key = Mm2ArchiveKey;
    type Milestones = Mm2Milestones;
    type Progress = Mm2ProgressWatermark;
    type Snapshot = Mm2Snapshot;
    type Observations = Mm2Observations;
    type Evidence = Mm2CampaignEvidence;
    type ArchiveReport = Mm2ArchiveReport;
    type Run = Mm2CampaignRun;
    type DrawState = ();
    type DrawHeader = Mm2NoTableHeader;
    type DrawCheckpoint = ();
}

impl Reporting for Mm2Game {
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

    fn result_sha256(&self, result: &Mm2CampaignJobResult) -> Result<String, Box<dyn Error>> {
        mm2_result_sha256(result)
    }

    fn archive_report(
        &self,
        evidence: &Mm2CampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> Mm2ArchiveReport {
        Mm2ArchiveReport {
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

impl InputPolicy for Mm2Game {
    fn max_action_limit(&self) -> usize {
        MAX_MM2_ACTIONS
    }

    fn max_action_cost(&self) -> u64 {
        u64::from(crate::mm2::archive::LONGEST_HOLD_FRAMES)
    }

    fn draw_state_memory_reserve_bytes(&self, _run: &Mm2CampaignRun, _max_actions: usize) -> usize {
        0
    }

    fn draw_state_memory_bytes(&self, _state: &()) -> usize {
        0
    }

    fn policies(&self, _run: &Mm2CampaignRun) -> WorkloadPolicies {
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
        policies: &WorkloadPolicies,
    ) -> Result<Mm2CampaignRun, Box<dyn Error>> {
        let expected = self.policies(&Mm2CampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(
                        format!("Mega Man 2 stream {field} policy is not recognized").into(),
                    );
                }
            }
            return Err("Mega Man 2 stream carries an unknown game policy".into());
        }
        Ok(Mm2CampaignRun)
    }

    fn initial_draw_state(
        &self,
        _run: &Mm2CampaignRun,
        _origin: Option<(&str, &Mm2ArchiveReport)>,
    ) -> Result<((), Option<Mm2NoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(&self, _state: &()) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &Mm2CampaignRun,
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
        run: &Mm2CampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&()>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Mega Man 2 stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &Mm2CampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<()>, Box<dyn Error>> {
        Ok(None)
    }

    fn retained_inputs_need_full(&self, _run: &Mm2CampaignRun) -> bool {
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
            Err("Mega Man 2 stream requires an unsupported draw-table version".into())
        }
    }
}

impl TargetExecution for Mm2Game {
    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &Mm2Snapshot) -> usize {
        std::mem::size_of::<Mm2Snapshot>().saturating_add(snapshot.emulator_state_bytes_len())
    }

    fn new_target(&self) -> Result<Mm2Target, String> {
        Mm2Target::from_rom_bytes_after(
            &self.rom,
            &self.core_path,
            &self.core_sha256,
            &self.prefix,
            self.stage,
        )
        .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut Mm2Target) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut Mm2Target,
        snapshot: &Mm2Snapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &Mm2Target) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut Mm2Target,
        action: &ButtonChord,
        aggregate: &mut Mm2Milestones,
    ) -> Result<(), Box<dyn Error>> {
        let genesis_weapons = target.genesis_weapons();
        target.apply(action);
        merge_action_milestones(aggregate, target, genesis_weapons);
        Ok(())
    }

    fn snapshot(&self, target: &mut Mm2Target) -> Result<Mm2Snapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Mega Man 2".into())
    }

    fn execute_job(
        &self,
        _run: &Mm2CampaignRun,
        target: &mut Mm2Target,
        origin_snapshot: &Mm2Snapshot,
        replay: &[ButtonChord],
        parent_actions: usize,
        parent_milestones: Mm2Milestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<Mm2CampaignJobResult, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
        }
        execute_suffix(
            target,
            target.genesis_weapons(),
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            retention,
        )
    }
}

impl Evaluation for Mm2Game {
    fn is_terminal(&self, target: &Mm2Target) -> bool {
        target.is_dead() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        _run: &Mm2CampaignRun,
        target: &Mm2Target,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok {
            return Err("Mega Man 2 terminal predicate cannot inspect a failed emulator".into());
        }
        Ok(target.is_dead() || target.defeated_a_boss())
    }

    fn current_key(&self, target: &Mm2Target) -> Result<Mm2ArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.mechanical_state()))
    }

    fn complete_candidate_key(
        &self,
        key: Mm2ArchiveKey,
        _snapshot: &Mm2Snapshot,
    ) -> Result<Mm2ArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn merge_milestones(&self, into: &mut Mm2Milestones, from: Mm2Milestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &Mm2CampaignEvidence) -> Mm2Milestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &Mm2CampaignEvidence) -> Mm2ProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(&self, evidence: &mut Mm2CampaignEvidence, source: &Mm2ArchiveReport) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut Mm2CampaignEvidence,
        target: &Mm2Target,
    ) -> Result<(), Box<dyn Error>> {
        let state = target.mechanical_state();
        evidence.watermark = evidence.watermark.max(progress_watermark(state));
        evidence.genesis_screen.get_or_insert(state.screen);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut Mm2CampaignEvidence,
        value: Mm2Milestones,
        input: &Mm2Input,
    ) {
        merge_milestones(&mut evidence.aggregate, value);
        let genesis_screen = evidence.genesis_screen.unwrap_or(0);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            value,
            genesis_screen,
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
        evidence: &mut Mm2CampaignEvidence,
        action: &Mm2CampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<Mm2Input, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let genesis_screen = evidence.genesis_screen.unwrap_or(0);
        let first_input_needed = (action.milestones.max_screen > genesis_screen
            && evidence.first_inputs.first_new_screen.is_none())
            || (action.milestones.reached_boss && evidence.first_inputs.first_boss.is_none())
            || (action.milestones.defeated_boss && evidence.first_inputs.first_clear.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed || champion.is_some() {
            let input = input()?;
            update_first_inputs(
                &mut evidence.first_reached,
                &mut evidence.first_inputs,
                action.milestones,
                genesis_screen,
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
        source: &'a Mm2ArchiveReport,
    ) -> &'a [crate::mm2::archive::Mm2ArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &Mm2ArchiveReport) -> Result<Mm2Input, Box<dyn Error>> {
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
            .ok_or_else(|| "Mega Man 2 source archive has no retained entries".into())
    }
}

pub fn run_mm2_campaign_checkpointed(
    game: &Mm2Game,
    config: &Mm2CampaignConfig,
    origin: &Mm2CampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(Mm2CampaignModeReport, Mm2SnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_mm2_campaign_checkpointed(
    game: &Mm2Game,
    stream_bytes: &[u8],
    origin_report: Option<&Mm2ArchiveReport>,
    origin_checkpoint: Option<&Mm2CampaignCheckpoint>,
) -> Result<(Mm2CampaignModeReport, Mm2SnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_policy_set_is_exact_and_game_owned() {
        let game = Mm2Game::new_at_stage(
            &[1, 2, 3],
            Path::new("core.so"),
            &"a".repeat(64),
            Mm2Stage::default(),
        );
        let policies = game.policies(&Mm2CampaignRun);
        let Mm2CampaignRun = game.resolve_recorded(&policies).expect("resolve");
        let mut foreign = policies;
        foreign.insert("stage".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
    }

    #[test]
    fn selected_stage_is_part_of_recorded_machine_identity() {
        let stage = Mm2Stage::from_number(3).expect("stage");
        let game = Mm2Game::new_at_stage(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64), stage);
        assert_eq!(game.stage(), stage);
        assert!(
            game.emulator_identity()
                .contains("genesis=mm2-stage-select-v2:3:prefix-sha256=")
        );
    }
}
