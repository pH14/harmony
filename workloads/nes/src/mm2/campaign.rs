// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    chord::{CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD},
    mm2::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, Mm2ArchiveKey, Mm2ArchiveReport,
            Mm2MilestoneInputs, Mm2MilestoneTimes, Mm2Milestones, Mm2ProgressWatermark,
            REPLACEMENT_IDENTIFIER, archive_key, chord_time, merge_milestones,
            merge_progress_watermark, milestone_key, milestones, progress_watermark,
        },
        progress::{FirstSeen, NamedProgress, is_route_milestone},
        target::{
            ButtonChord, Mm2Input, Mm2Observations, Mm2Route, Mm2Snapshot, Mm2Stage, Mm2Target,
            Mm2Tier, ROBOT_MASTER_ORDER, power_on_walk, preference_tuple, walk_to_stage_select,
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
        draw::DrawMixture,
        draw_tables::DrawTableHeader,
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome},
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "mm2-quicknes-campaign-stream-v2";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "mm2-quicknes-snapshot-checkpoint-v3";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_start_taps_no_select_v2";
const STAGE_TERMINAL_POLICY_IDENTIFIER: &str = "death_or_first_boss_defeated";
const WHOLE_GAME_TERMINAL_POLICY_IDENTIFIER: &str = "death_or_ending";

const VIABILITY_PROBE_MASKS: [u8; 4] = [0, 0x01, 0x80, 0x81];
const VIABILITY_PROBE_FRAMES: u16 = 60;

type Mm2Preference = (Mm2Tier, u8, u16);
type Mm2ChampionKey = (Mm2ProgressWatermark, Mm2Preference);

pub struct Mm2Game {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    prefix: Vec<ButtonChord>,
    route: Mm2Route,
    identity: String,
    champion_input_path: Option<PathBuf>,
    milestone_input_dir: Option<PathBuf>,
}

fn chords_sha256(chords: &[ButtonChord]) -> String {
    let mut digest = Sha256::new();
    for chord in chords {
        digest.update([chord.buttons, chord.hold_frames]);
    }
    format!("{:x}", digest.finalize())
}

fn emulator_identity(genesis: &str, core_sha256: &str) -> String {
    format!(
        "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;genesis={genesis};result_digest=mm2-semantic-postcard-1.1.3-sha256-hex-v2;sha256={core_sha256}",
        machine::quicknes::QUICKNES_REVISION,
        machine::quicknes::QUICKNES_BUILD,
        machine::quicknes::QUICKNES_OPTIONS,
    )
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
        let identity = emulator_identity(
            &format!(
                "mm2-stage-select-v2:{}:prefix-sha256={}",
                stage.number(),
                chords_sha256(&prefix)
            ),
            core_sha256,
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            prefix,
            route: Mm2Route::Stage(stage),
            identity,
            champion_input_path: None,
            milestone_input_dir: None,
        }
    }

    #[must_use]
    pub fn new_whole_game(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        root: Vec<ButtonChord>,
    ) -> Self {
        let order = ROBOT_MASTER_ORDER
            .iter()
            .map(|stage| stage.name())
            .collect::<Vec<_>>()
            .join(",");
        let identity = emulator_identity(
            &format!(
                "mm2-whole-game-v1:order={order}:root-sha256={}",
                chords_sha256(&root)
            ),
            core_sha256,
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            prefix: root,
            route: Mm2Route::WholeGame,
            identity,
            champion_input_path: None,
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
        files: impl IntoIterator<Item = String>,
        input: &Mm2Input,
    ) -> Result<(), Box<dyn Error>> {
        let Some(directory) = &self.milestone_input_dir else {
            return Ok(());
        };
        let mut bytes = None;
        for file in files {
            std::fs::create_dir_all(directory)?;
            let path = directory.join(format!("{file}.json"));
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
    pub fn route(&self) -> Mm2Route {
        self.route
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Mm2CampaignRun;

#[derive(Clone, Default)]
pub struct Mm2CampaignEvidence {
    named_progress: NamedProgress,
    aggregate: Mm2Milestones,
    watermark: Mm2ProgressWatermark,
    first_reached: Mm2MilestoneTimes,
    first_inputs: Mm2MilestoneInputs,
    champion_input: Mm2Input,
    champion_milestones: Mm2Milestones,
    champion_key: Option<Mm2ChampionKey>,
    genesis_screen: Option<u8>,
    best_health: BTreeMap<String, (u8, u16)>,
    best_energy: BTreeMap<String, (u16, u8)>,
}

#[derive(Deserialize, Serialize)]
struct Mm2EvidenceCheckpoint {
    first_seen: Vec<(String, Option<FirstSeen>)>,
    aggregate: Mm2Milestones,
    watermark: Mm2ProgressWatermark,
    first_reached: Mm2MilestoneTimes,
    first_inputs: Mm2MilestoneInputs,
    champion_input: Mm2Input,
    champion_milestones: Mm2Milestones,
    champion_key: Option<Mm2ChampionKey>,
    genesis_screen: Option<u8>,
    best_health: Vec<(String, (u8, u16))>,
    best_energy: Vec<(String, (u16, u8))>,
}

impl Mm2CampaignEvidence {
    fn to_checkpoint(&self) -> Mm2EvidenceCheckpoint {
        Mm2EvidenceCheckpoint {
            first_seen: self.named_progress.first_seen.clone().into_iter().collect(),
            aggregate: self.aggregate,
            watermark: self.watermark,
            first_reached: self.first_reached,
            first_inputs: self.first_inputs.clone(),
            champion_input: self.champion_input.clone(),
            champion_milestones: self.champion_milestones,
            champion_key: self.champion_key,
            genesis_screen: self.genesis_screen,
            best_health: self.best_health.clone().into_iter().collect(),
            best_energy: self.best_energy.clone().into_iter().collect(),
        }
    }

    fn from_checkpoint(checkpoint: Mm2EvidenceCheckpoint) -> Result<Self, Box<dyn Error>> {
        let mut named_progress = NamedProgress::default();
        named_progress.restore(checkpoint.first_seen)?;
        Ok(Self {
            named_progress,
            aggregate: checkpoint.aggregate,
            watermark: checkpoint.watermark,
            first_reached: checkpoint.first_reached,
            first_inputs: checkpoint.first_inputs,
            champion_input: checkpoint.champion_input,
            champion_milestones: checkpoint.champion_milestones,
            champion_key: checkpoint.champion_key,
            genesis_screen: checkpoint.genesis_screen,
            best_health: checkpoint.best_health.into_iter().collect(),
            best_energy: checkpoint.best_energy.into_iter().collect(),
        })
    }

    fn stock(&mut self, observation: &Mm2Observations) -> Vec<String> {
        let state = observation.decoded;
        let mut improved = Vec::new();
        for name in NamedProgress::reached(observation) {
            if !is_route_milestone(&name)
                || name == "ending"
                || name.ends_with("_defeated")
                || name.ends_with("_refight")
            {
                continue;
            }
            let by_health = (state.health, state.weapon_energy);
            if self
                .best_health
                .get(&name)
                .is_none_or(|best| by_health > *best)
            {
                self.best_health.insert(name.clone(), by_health);
                improved.push(format!("{name}-health"));
            }
            let by_energy = (state.weapon_energy, state.health);
            if self
                .best_energy
                .get(&name)
                .is_none_or(|best| by_energy > *best)
            {
                self.best_energy.insert(name.clone(), by_energy);
                improved.push(format!("{name}-energy"));
            }
        }
        improved
    }
}

pub type Mm2CampaignOrigin = CampaignOrigin<Mm2Game>;
pub type Mm2CampaignCheckpoint = CampaignCheckpoint<Mm2Snapshot>;
pub type Mm2SnapshotCheckpoint = SnapshotCheckpoint<Mm2Snapshot>;
pub type Mm2CampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
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
    outcome: Outcome,
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
            outcome: action.outcome,
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
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_victory: bool,
    pub archive_entry_limit: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub retention: RetentionPolicy,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<PathBuf>,
}

impl Mm2CampaignConfig {
    fn generic(&self) -> CampaignConfig<Mm2Game> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            stop_rollout_on_objective: !self.continue_after_victory,
            stop_campaign_on_objective: !self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            window: crate::search::campaign::default_window(self.workers),
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: Mm2CampaignRun,
            mixture: self.mixture,
            retention: self.retention,
            objective_witness_path: self.victory_input_path.clone(),
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

#[allow(clippy::too_many_arguments)]
fn execute_suffix(
    target: &mut Mm2Target,
    genesis_weapons: u8,
    parent_milestones: Mm2Milestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
) -> Result<Mm2CampaignJobResult, Box<dyn Error>> {
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = Outcome {
        objective_reached: target.exit_kind() == ExitKind::Ok && target.objective_reached(),
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
        target.apply(action);
        merge_action_milestones(&mut aggregate, target, genesis_weapons);
        let observations = if target.exit_kind() != ExitKind::Ok {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        };
        let raw_objective = target.exit_kind() == ExitKind::Ok && target.objective_reached();
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
        let candidate = if matches!(outcome.disposition, ExecutionDisposition::Runnable) {
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
}

impl Reporting for Mm2Game {
    fn evidence_checkpoint(evidence: &Mm2CampaignEvidence) -> Result<Vec<u8>, Box<dyn Error>> {
        Ok(postcard::to_allocvec(&evidence.to_checkpoint())?)
    }
    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<Mm2CampaignEvidence, Box<dyn Error>> {
        Mm2CampaignEvidence::from_checkpoint(postcard::from_bytes(bytes)?)
    }
    fn checkpoint_marks(evidence: &Mm2CampaignEvidence) -> usize {
        evidence
            .named_progress
            .first_seen
            .iter()
            .filter(|(name, seen)| seen.is_some() && is_route_milestone(name))
            .count()
    }
    fn diagnostics(evidence: &Mm2CampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({ "named_progress": evidence.named_progress }))
    }
    fn merge_witness_diagnostics(
        evidence: &mut Mm2CampaignEvidence,
        observations: &[Mm2Observations],
        sequence: u64,
    ) {
        let action_end = observations.last().map_or(0, |obs| obs.frame_count);
        for observation in observations {
            evidence
                .named_progress
                .observe(observation, sequence, action_end);
        }
    }
    fn retained_diagnostics<'a>(
        snapshots: impl Iterator<Item = (Option<&'a Mm2Snapshot>, u64)>,
    ) -> Option<serde_json::Value> {
        let (mut active, mut missing) = (0_u64, 0_u64);
        let mut top = Mm2Tier::default();
        let mut screens = BTreeMap::<(u8, u8), [u64; 4]>::new();
        let mut rows = BTreeMap::<(u8, u8, u8), [u64; 2]>::new();
        for (snapshot, selections) in snapshots {
            active += 1;
            let Some(snapshot) = snapshot else {
                missing += 1;
                continue;
            };
            let state = snapshot.state();
            top = top.max(state.tier());
            let screen = screens.entry((state.stage, state.screen)).or_default();
            *screen = [
                screen[0] + 1,
                screen[1].max(u64::from(state.health)),
                screen[2].max(u64::from(state.weapon_energy)),
                screen[3].saturating_add(selections),
            ];
            let row = rows
                .entry((state.stage, state.screen, state.y / 16))
                .or_default();
            *row = [row[0] + 1, row[1].saturating_add(selections)];
        }
        Some(serde_json::json!({
            "scope": "union/maxima over cached active endpoints; not one trajectory; lower bounds when snapshots are missing",
            "active_entries": active, "missing_snapshots": missing,
            "top_tier": top,
            "live_entries_by_screen": screens
                .iter()
                .map(|((stage, screen), best)| (format!("{stage}:{screen}"), *best))
                .collect::<BTreeMap<_, _>>(),
            "live_entries_by_screen_format": "stage:screen -> [entries, max health, max summed energy, selections]",
            "live_entries_by_screen_row": rows
                .iter()
                .map(|((stage, screen, row), best)| (format!("{stage}:{screen}:{row}"), *best))
                .collect::<BTreeMap<_, _>>(),
            "live_entries_by_screen_row_format": "stage:screen:16-pixel row from the top -> [entries, selections]"
        }))
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
            deaths: state
                .terminal_endpoints
                .saturating_sub(state.terminal_objectives),
            selector: state.selector,
        }
    }
}

impl InputPolicy for Mm2Game {
    fn policies(&self, _run: &Mm2CampaignRun) -> WorkloadPolicies {
        [
            (
                CONTROLLER_VOCABULARY_FIELD,
                CONTROLLER_VOCABULARY_IDENTIFIER,
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (
                TERMINAL_POLICY_FIELD,
                match self.route {
                    Mm2Route::Stage(_) => STAGE_TERMINAL_POLICY_IDENTIFIER,
                    Mm2Route::WholeGame => WHOLE_GAME_TERMINAL_POLICY_IDENTIFIER,
                },
            ),
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

    fn sample_alphabet(
        &self,
        _run: &Mm2CampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::mm2::archive::CHORDS.draw(rand, previous)
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
        match self.route {
            Mm2Route::Stage(stage) => Mm2Target::from_rom_bytes_after(
                &self.rom,
                &self.core_path,
                &self.core_sha256,
                &self.prefix,
                stage,
            ),
            Mm2Route::WholeGame => {
                Mm2Target::whole_game(&self.rom, &self.core_path, &self.core_sha256, &self.prefix)
            }
        }
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
        parent_milestones: Mm2Milestones,
        suffix: &[ButtonChord],
        retention: RetentionPolicy,
        stop_rollout_on_objective: bool,
    ) -> Result<Mm2CampaignJobResult, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
            if self.execution_disposition(target).is_terminal() {
                break;
            }
        }
        execute_suffix(
            target,
            target.genesis_weapons(),
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
        )
    }
}

impl Evaluation for Mm2Game {
    fn execution_disposition(&self, target: &Mm2Target) -> ExecutionDisposition {
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
        _run: &Mm2CampaignRun,
        target: &Mm2Target,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(target.exit_kind() == ExitKind::Ok && target.objective_reached())
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
        let observation = target.observe();
        evidence
            .named_progress
            .observe(&observation, 0, observation.frame_count);
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
        let action_end_frame = action.observations.last().map_or(0, |obs| obs.frame_count);
        let mut discoveries = Vec::new();
        let mut improved = Vec::new();
        for observation in &action.observations {
            discoveries.extend(
                evidence
                    .named_progress
                    .observe(observation, sequence, action_end_frame)
                    .into_iter()
                    .filter(|name| is_route_milestone(name)),
            );
        }
        if let Some(last) = action.observations.last() {
            improved = evidence.stock(last);
        }
        let genesis_screen = evidence.genesis_screen.unwrap_or(0);
        let first_input_needed = (action.milestones.max_screen > genesis_screen
            && evidence.first_inputs.first_new_screen.is_none())
            || (action.milestones.reached_boss && evidence.first_inputs.first_boss.is_none())
            || (action.milestones.defeated_boss && evidence.first_inputs.first_clear.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        if first_input_needed
            || champion.is_some()
            || !discoveries.is_empty()
            || !improved.is_empty()
        {
            let input = input()?;
            self.publish_inputs(discoveries.into_iter().chain(improved), &input)?;
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
    fn the_census_reports_where_the_live_archive_sits_and_what_the_selector_drew() {
        use crate::mm2::target::Mm2MechanicalState;

        let at = |stage, screen, y, health| Mm2MechanicalState {
            stage,
            screen,
            y,
            health,
            ..Mm2MechanicalState::default()
        };
        let cached = [
            (at(8, 4, 10, 28), 3),
            (at(8, 4, 200, 8), 0),
            (at(8, 2, 10, 20), 11),
            (at(2, 4, 10, 5), 1),
        ]
        .map(|(state, selections)| (Mm2Snapshot::for_census_tests(state), selections));
        let census = Mm2Game::retained_diagnostics(
            cached
                .iter()
                .map(|(snapshot, selections)| (Some(snapshot), *selections))
                .chain(std::iter::once((None, 9))),
        )
        .expect("census");

        assert_eq!(census["active_entries"], 5);
        assert_eq!(census["missing_snapshots"], 1);
        assert_eq!(census["top_tier"]["castles"], 0);
        let screens = &census["live_entries_by_screen"];
        assert_eq!(screens["8:4"], serde_json::json!([2, 28, 0, 3]));
        assert_eq!(screens["8:2"], serde_json::json!([1, 20, 0, 11]));
        assert_eq!(screens["2:4"], serde_json::json!([1, 5, 0, 1]));
        let rows = &census["live_entries_by_screen_row"];
        assert_eq!(rows["8:4:0"], serde_json::json!([1, 3]));
        assert_eq!(rows["8:4:12"], serde_json::json!([1, 0]));
    }

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
        assert_eq!(game.route(), Mm2Route::Stage(stage));
        assert!(
            game.emulator_identity()
                .contains("genesis=mm2-stage-select-v2:3:prefix-sha256=")
        );
    }
}
