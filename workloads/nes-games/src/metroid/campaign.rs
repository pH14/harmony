// SPDX-License-Identifier: AGPL-3.0-or-later

use std::cmp::Reverse;
#[cfg(unix)]
use std::path::Path;
use std::{collections::BTreeMap, error::Error, path::PathBuf};

use machine::MachineError;
#[cfg(unix)]
use machine::quicknes::QuickNesMachine;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    chord::{CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD},
    metroid::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MetroidArchiveKey, MetroidArchiveReport,
            MetroidMilestoneInputs, MetroidMilestoneTimes, MetroidMilestones,
            MetroidProgressWatermark, PREFERENCE_IDENTIFIER, REPLACEMENT_IDENTIFIER, archive_key,
            chord_time, merge_milestones, merge_progress_watermark, milestone_key, milestones,
            progress_watermark,
        },
        progress::{FirstSeen, NamedProgress},
        target::{
            ButtonChord, GenesisDepth, MetroidInput, MetroidObservations, MetroidSnapshot,
            MetroidTarget, MetroidTerminalPolicy, preference_tuple,
        },
    },
    nes_backend::{NesBackend, NesMemory},
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignJobResult, CampaignModeReport, CampaignOrigin, CampaignProgressRecord,
            CampaignStreamHeader, CampaignTypes, Evaluation, InputPolicy, PREFERENCE_POLICY_FIELD,
            Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
            postcard_semantic_result_sha256, postcard_value_sha256,
        },
        draw_tables::DrawTableHeader,
        rand::RomuDuoJrRand,
        rollout::{ExecutionDisposition, Outcome},
    },
    target::{ExitKind, Target},
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "metroid-quicknes-campaign-stream-v4";
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "metroid-quicknes-snapshot-checkpoint-v13";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_select_taps_no_start_v1";

type MetroidPreference = (u8, u8, u16, u8);
type MetroidChampionKey = (MetroidProgressWatermark, MetroidPreference);

pub type MetroidMachines<M> = Box<dyn Fn() -> Result<M, MachineError> + Send + Sync>;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetroidObjective {
    #[default]
    GameVictory,
    HighJump,
}

impl MetroidObjective {
    fn identifier(self) -> &'static str {
        match self {
            Self::GameVictory => "game_victory",
            Self::HighJump => "high_jump",
        }
    }

    fn reached<M: NesBackend<Vec<u8>> + NesMemory>(self, target: &MetroidTarget<M>) -> bool {
        target.exit_kind() == ExitKind::Ok
            && match self {
                Self::GameVictory => target.is_victory(),
                Self::HighJump => NamedProgress::reached(&target.observe()).contains(&"high_jump"),
            }
    }
}

pub struct MetroidGame<M: NesBackend<Vec<u8>> + NesMemory> {
    rom_sha256: String,
    machines: MetroidMachines<M>,
    prefix: Vec<ButtonChord>,
    identity: String,
    champion_input_path: Option<PathBuf>,
    milestone_input_dir: Option<PathBuf>,
    terminal_policy: MetroidTerminalPolicy,
    objective: MetroidObjective,
    depth: GenesisDepth,
}

#[cfg(unix)]
impl MetroidGame<QuickNesMachine> {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str, execution_sha256: &str) -> Self {
        Self::new_after(
            rom,
            core_path,
            core_sha256,
            execution_sha256,
            crate::metroid::target::power_on_walk(),
        )
    }

    #[must_use]
    pub fn new_after(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
        prefix: Vec<ButtonChord>,
    ) -> Self {
        Self::new_rooted(
            rom,
            core_path,
            core_sha256,
            execution_sha256,
            prefix,
            GenesisDepth::NewGame,
        )
    }

    #[must_use]
    pub fn new_rooted(
        rom: &[u8],
        core_path: &Path,
        core_sha256: &str,
        execution_sha256: &str,
        prefix: Vec<ButtonChord>,
        depth: GenesisDepth,
    ) -> Self {
        let core = machine::quicknes::core_identity();
        let (image, core_path, sha256, execution) = (
            machine::nes::with_cartridge_ram(rom),
            core_path.to_path_buf(),
            core_sha256.to_owned(),
            execution_sha256.to_owned(),
        );
        Self::with_machines(
            rom,
            &core,
            &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            prefix,
            depth,
            Box::new(move || {
                QuickNesMachine::from_rom_bytes_with_identity(
                    image.as_ref().map_err(Clone::clone)?,
                    &core_path,
                    &sha256,
                    &execution,
                )
            }),
        )
    }
}

impl<M: NesBackend<Vec<u8>> + NesMemory> MetroidGame<M> {
    #[must_use]
    pub fn with_machines(
        rom: &[u8],
        core: &str,
        core_sha256: &str,
        prefix: Vec<ButtonChord>,
        depth: GenesisDepth,
        machines: MetroidMachines<M>,
    ) -> Self {
        let mut prefix_digest = Sha256::new();
        for chord in &prefix {
            prefix_digest.update([chord.buttons, chord.hold_frames]);
        }
        let genesis = match depth {
            GenesisDepth::NewGame => "metroid-new-game-v1",
            GenesisDepth::Rooted => "metroid-rooted-v1",
        };
        let identity = format!(
            "{core};state=ppu-unused2-zero-v1;\
             genesis={genesis}:prefix-sha256={:x};\
             image=cartridge-ram-declared-v1;\
             result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}",
            prefix_digest.finalize(),
        );
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            machines,
            prefix,
            identity,
            champion_input_path: None,
            milestone_input_dir: None,
            terminal_policy: MetroidTerminalPolicy::default(),
            objective: MetroidObjective::default(),
            depth,
        }
    }

    #[must_use]
    pub fn with_objective(mut self, objective: MetroidObjective) -> Self {
        self.objective = objective;
        self
    }

    #[must_use]
    pub fn with_terminal_policy(mut self, policy: MetroidTerminalPolicy) -> Self {
        self.terminal_policy = policy;
        self
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

    fn publish_milestones(
        &self,
        names: &[&str],
        input: &MetroidInput,
    ) -> Result<(), Box<dyn Error>> {
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

    fn publish_stocked(
        &self,
        improved: &[(&str, &str)],
        input: &MetroidInput,
    ) -> Result<(), Box<dyn Error>> {
        if improved.is_empty() {
            return Ok(());
        }
        if let Some(directory) = &self.milestone_input_dir {
            std::fs::create_dir_all(directory)?;
            for (name, stock) in improved {
                let path = directory.join(format!("{name}-{stock}.json"));
                let temporary = path.with_extension("json.tmp");
                std::fs::write(&temporary, serde_json::to_vec(input)?)?;
                std::fs::rename(temporary, path)?;
            }
        }
        Ok(())
    }

    fn publish_champion(&self, input: &MetroidInput) -> Result<(), Box<dyn Error>> {
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
}

#[derive(Clone, Copy, Debug)]
pub struct MetroidCampaignRun;

#[derive(Clone, Default)]
pub struct MetroidCampaignEvidence {
    observed_map: MapCoverage,
    named_progress: NamedProgress,
    aggregate: MetroidMilestones,
    watermark: MetroidProgressWatermark,
    first_reached: MetroidMilestoneTimes,
    first_inputs: MetroidMilestoneInputs,
    champion_input: MetroidInput,
    champion_milestones: MetroidMilestones,
    champion_key: Option<MetroidChampionKey>,
    genesis_area: Option<u8>,
    best_energy: BTreeMap<&'static str, (u16, u8)>,
    best_missiles: BTreeMap<&'static str, (u8, u16)>,
    best_boss: BTreeMap<&'static str, (Reverse<u16>, u8, u16)>,
}

#[derive(Deserialize, Serialize)]
struct MetroidEvidenceCheckpoint {
    observed_map: Vec<u64>,
    first_seen: Vec<(String, Option<FirstSeen>)>,
    max_missile_capacity: u8,
    max_energy_tanks: u8,
    aggregate: MetroidMilestones,
    watermark: MetroidProgressWatermark,
    first_reached: MetroidMilestoneTimes,
    first_inputs: MetroidMilestoneInputs,
    champion_input: MetroidInput,
    champion_milestones: MetroidMilestones,
    champion_key: Option<MetroidChampionKey>,
    genesis_area: Option<u8>,
    best_energy: Vec<(String, (u16, u8))>,
    best_missiles: Vec<(String, (u8, u16))>,
    best_boss: Vec<(String, (u16, u8, u16))>,
}

fn named_progress_name(name: &str) -> Result<&'static str, Box<dyn Error>> {
    NamedProgress::default()
        .first_seen
        .keys()
        .find(|known| **known == name)
        .copied()
        .ok_or_else(|| format!("search checkpoint names an unknown milestone {name}").into())
}

fn interned_names<V>(
    entries: Vec<(String, V)>,
) -> Result<BTreeMap<&'static str, V>, Box<dyn Error>> {
    entries
        .into_iter()
        .map(|(name, value)| Ok((named_progress_name(&name)?, value)))
        .collect()
}

impl MetroidCampaignEvidence {
    fn from_checkpoint(checkpoint: MetroidEvidenceCheckpoint) -> Result<Self, Box<dyn Error>> {
        let observed_map: Box<[u64; 4096]> = checkpoint
            .observed_map
            .into_boxed_slice()
            .try_into()
            .map_err(|_| "search checkpoint map coverage has the wrong size")?;
        let mut named_progress = NamedProgress::default();
        let first_seen = interned_names(checkpoint.first_seen)?;
        if first_seen.len() != named_progress.first_seen.len() {
            return Err("search checkpoint milestone names differ from this build".into());
        }
        named_progress.first_seen = first_seen;
        named_progress.max_missile_capacity = checkpoint.max_missile_capacity;
        named_progress.max_energy_tanks = checkpoint.max_energy_tanks;
        Ok(Self {
            observed_map: MapCoverage(observed_map),
            named_progress,
            aggregate: checkpoint.aggregate,
            watermark: checkpoint.watermark,
            first_reached: checkpoint.first_reached,
            first_inputs: checkpoint.first_inputs,
            champion_input: checkpoint.champion_input,
            champion_milestones: checkpoint.champion_milestones,
            champion_key: checkpoint.champion_key,
            genesis_area: checkpoint.genesis_area,
            best_energy: interned_names(checkpoint.best_energy)?,
            best_missiles: interned_names(checkpoint.best_missiles)?,
            best_boss: interned_names(checkpoint.best_boss)?
                .into_iter()
                .map(|(name, (health, missiles, energy))| {
                    (name, (Reverse(health), missiles, energy))
                })
                .collect(),
        })
    }
}

#[derive(Clone)]
struct MapCoverage(Box<[u64; 4096]>);

impl Default for MapCoverage {
    fn default() -> Self {
        Self(Box::new([0; 4096]))
    }
}

impl MapCoverage {
    fn observe(&mut self, area: u8, x: u8, y: u8) {
        if x < 32 && y < 32 {
            let bit = usize::from(area) * 1024 + usize::from(y) * 32 + usize::from(x);
            self.0[bit / 64] |= 1 << (bit % 64);
        }
    }

    fn count(&self) -> u32 {
        self.0.iter().map(|word| word.count_ones()).sum()
    }
}

pub type MetroidCampaignOrigin<M> = CampaignOrigin<MetroidGame<M>>;
pub type MetroidCampaignCheckpoint = CampaignCheckpoint<MetroidSnapshot>;
pub type MetroidSnapshotCheckpoint = SnapshotCheckpoint<MetroidSnapshot>;
pub type MetroidCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type MetroidCampaignModeReport = CampaignModeReport<ButtonChord, MetroidArchiveReport>;
pub type MetroidCampaignProgressRecord = CampaignProgressRecord<MetroidArchiveKey>;
pub type MetroidCampaignActionResult<M> = CampaignActionResult<MetroidGame<M>>;
pub type MetroidCampaignJobResult<M> = CampaignJobResult<MetroidGame<M>>;

fn recorded<'a>(policies: &'a WorkloadPolicies, field: &str) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("Metroid stream is missing {field}").into())
}

fn merge_action_milestones<M: NesBackend<Vec<u8>> + NesMemory>(
    aggregate: &mut MetroidMilestones,
    target: &MetroidTarget<M>,
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

#[allow(clippy::too_many_arguments)]
fn execute_suffix<M: NesBackend<Vec<u8>> + NesMemory>(
    target: &mut MetroidTarget<M>,
    genesis: (u8, u8),
    parent_milestones: MetroidMilestones,
    suffix: &[ButtonChord],
    retention: RetentionPolicy,
    stop_rollout_on_objective: bool,
    objective: MetroidObjective,
) -> Result<MetroidCampaignJobResult<M>, Box<dyn Error>> {
    if retention != RetentionPolicy::Unprobed {
        return Err("Metroid campaigns admit every live candidate".into());
    }
    let (genesis_items, genesis_tanks) = genesis;
    let mut aggregate = parent_milestones;
    let mut actions = Vec::with_capacity(suffix.len());
    let parent_outcome = Outcome {
        objective_reached: objective.reached(target),
        disposition: if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_dead() {
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
        merge_action_milestones(&mut aggregate, target, genesis_items, genesis_tanks);
        let failed = target.exit_kind() != ExitKind::Ok;
        let raw_objective = objective.reached(target);
        let objective_reached = raw_objective && !objective_seen;
        objective_seen |= raw_objective;
        let disposition = if failed {
            ExecutionDisposition::Failed
        } else if target.is_dead() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        };
        let outcome = Outcome {
            objective_reached,
            disposition,
        };
        let observations = if failed {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        };
        let candidate = if matches!(outcome.disposition, ExecutionDisposition::Runnable) {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot Metroid suffix")?;
            Some(CampaignCandidate {
                key: archive_key(target.mechanical_state())
                    .with_boss_health_seen(target.boss_health_seen()),
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

fn action_champion_key(observations: &[MetroidObservations]) -> Option<MetroidChampionKey> {
    observations
        .last()
        .filter(|observation| !observation.dead)
        .map(|observation| {
            let state = observation.decoded;
            (progress_watermark(state), preference_tuple(state))
        })
}

impl<M: NesBackend<Vec<u8>> + NesMemory> CampaignTypes for MetroidGame<M> {
    type JobObservation = ();
    type Target = MetroidTarget<M>;
    type Action = ButtonChord;
    type Key = MetroidArchiveKey;
    type Milestones = MetroidMilestones;
    type Progress = MetroidProgressWatermark;
    type Snapshot = MetroidSnapshot;
    type Observations = MetroidObservations;
    type Evidence = MetroidCampaignEvidence;
    type ArchiveReport = MetroidArchiveReport;
    type Run = MetroidCampaignRun;
}

impl<M: NesBackend<Vec<u8>> + NesMemory> Reporting for MetroidGame<M> {
    fn write_evidence_checkpoint(
        evidence: &MetroidCampaignEvidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        use searcher::search::checkpoint::image::Sequence;
        postcard::to_io(
            &(
                &evidence.observed_map.0[..],
                Sequence(evidence.named_progress.first_seen.iter()),
                evidence.named_progress.max_missile_capacity,
                evidence.named_progress.max_energy_tanks,
                evidence.aggregate,
                evidence.watermark,
                evidence.first_reached,
                &evidence.first_inputs,
                &evidence.champion_input,
                evidence.champion_milestones,
                evidence.champion_key,
                evidence.genesis_area,
                Sequence(evidence.best_energy.iter()),
                Sequence(evidence.best_missiles.iter()),
                Sequence(evidence.best_boss.iter().map(
                    |(name, (Reverse(health), missiles, energy))| {
                        (name, (health, missiles, energy))
                    },
                )),
            ),
            out,
        )?;
        Ok(())
    }
    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<MetroidCampaignEvidence, Box<dyn Error>> {
        MetroidCampaignEvidence::from_checkpoint(postcard::from_bytes(bytes)?)
    }
    fn checkpoint_marks(evidence: &MetroidCampaignEvidence) -> usize {
        evidence
            .named_progress
            .first_seen
            .values()
            .filter(|seen| seen.is_some())
            .count()
    }
    fn diagnostics(evidence: &MetroidCampaignEvidence) -> Option<serde_json::Value> {
        Some(serde_json::json!({
            "map_cells_observed": evidence.observed_map.count(),
            "coverage_bitmap_bytes": 32768,
            "named_progress": evidence.named_progress,
            "observation_filter": "live gameplay observations supplied to this accumulator"
        }))
    }
    fn retained_diagnostics<'a>(
        snapshots: impl Iterator<Item = (Option<&'a MetroidSnapshot>, u64)>,
    ) -> Option<serde_json::Value> {
        let (mut active, mut missing) = (0_u64, 0_u64);
        let (mut equipment, mut bosses, mut missiles, mut tanks) = (0, 0, 0, 0);
        let (mut underflows, mut health) = (0_u64, 0_u16);
        let mut maps = MapCoverage::default();
        let mut areas = [[0_u8; 2]; 256];
        let mut cells = BTreeMap::<(u8, u8, u8), [u64; 5]>::new();
        let mut kit = BTreeMap::<(u8, u8, u8, u8), [u64; 2]>::new();
        for (snapshot, selections) in snapshots {
            active += 1;
            let Some(snapshot) = snapshot else {
                missing += 1;
                continue;
            };
            let state = snapshot.state();
            underflows += u64::from(state.health >= 8000);
            health = health.max(state.health);
            equipment |= state.equipment;
            bosses = bosses.max(state.bosses);
            missiles = missiles.max(state.missile_capacity);
            tanks = tanks.max(state.energy_tanks);
            maps.observe(state.area, state.map_x, state.map_y);
            let area = &mut areas[usize::from(state.area)];
            *area = [area[0].max(state.missiles), area[1].max(state.energy_tanks)];
            let cell = cells
                .entry((state.area, state.map_x, state.map_y))
                .or_default();
            *cell = [
                cell[0] + 1,
                cell[1].max(u64::from(state.health)),
                cell[2].max(u64::from(state.missiles)),
                cell[3] | u64::from(state.equipment),
                cell[4].saturating_add(selections),
            ];
            let held = kit
                .entry((state.area, state.map_x, state.map_y, state.equipment))
                .or_default();
            *held = [held[0] + 1, held[1].saturating_add(selections)];
        }
        Some(serde_json::json!({
            "scope": "union/maxima over cached active endpoints; not one trajectory; lower bounds when snapshots are missing",
            "active_entries": active, "missing_snapshots": missing,
            "underflow_endpoints_cached": underflows, "max_health_cached": health,
            "equipment_union": equipment, "max_bosses": bosses,
            "max_missile_capacity": missiles, "max_energy_tanks": tanks,
            "max_missiles_and_tanks_held_by_area": areas
                .iter()
                .enumerate()
                .filter(|(_, best)| *best != &[0, 0])
                .map(|(area, best)| (area.to_string(), *best))
                .collect::<BTreeMap<_, _>>(),
            "map_cells_retained_cached": maps.count(), "temporary_bitmap_bytes": 32768,
            "live_entries_by_map_cell": cells
                .iter()
                .map(|((area, x, y), best)| (format!("{area}:{x}:{y}"), *best))
                .collect::<BTreeMap<_, _>>(),
            "live_entries_by_map_cell_format": "area:map_x:map_y -> [entries, max health, max missiles, equipment union, selections]",
            "live_entries_by_map_cell_and_equipment": kit
                .iter()
                .map(|((area, x, y, equipment), best)| {
                    (format!("{area}:{x}:{y}:{equipment}"), *best)
                })
                .collect::<BTreeMap<_, _>>(),
            "live_entries_by_map_cell_and_equipment_format": "area:map_x:map_y:equipment bits -> [entries, selections]"
        }))
    }
    fn merge_witness_diagnostics(
        evidence: &mut MetroidCampaignEvidence,
        observations: &[MetroidObservations],
        sequence: u64,
    ) {
        let action_end = observations.last().map_or(0, |obs| obs.frame_count);
        for observation in observations {
            evidence
                .named_progress
                .observe(observation, sequence, action_end);
            let state = observation.decoded;
            if !observation.dead && state.in_play() {
                evidence
                    .observed_map
                    .observe(state.area, state.map_x, state.map_y);
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
        result: &MetroidCampaignJobResult<M>,
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
            deaths: state
                .terminal_endpoints
                .saturating_sub(state.terminal_objectives),
            selector: state.selector,
        }
    }
}

impl<M: NesBackend<Vec<u8>> + NesMemory> InputPolicy for MetroidGame<M> {
    fn policies(&self, _run: &MetroidCampaignRun) -> WorkloadPolicies {
        [
            (
                CONTROLLER_VOCABULARY_FIELD,
                CONTROLLER_VOCABULARY_IDENTIFIER,
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (PREFERENCE_POLICY_FIELD, PREFERENCE_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, self.terminal_policy.identifier()),
            ("objective", self.objective.identifier()),
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

    fn sample_alphabet(
        &self,
        _run: &MetroidCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        crate::metroid::archive::CHORDS.draw(rand, previous)
    }
}

fn continuation_observation(observation: &MetroidObservations) -> Vec<u64> {
    vec![
        u64::from(observation.decoded.area),
        u64::from(observation.decoded.map_x),
        u64::from(observation.decoded.map_y),
        u64::from(observation.decoded.x),
        u64::from(observation.decoded.y),
        u64::from(observation.decoded.pose),
        u64::from(observation.decoded.door),
        u64::from(observation.decoded.mode),
        u64::from(observation.decoded.health),
        u64::from(observation.decoded.equipment),
        u64::from(observation.decoded.missiles),
        u64::from(observation.decoded.missile_capacity),
        u64::from(observation.decoded.energy_tanks),
        u64::from(observation.decoded.boss_health),
        u64::from(observation.decoded.zebetites_destroyed),
        u64::from(observation.decoded.zebetite_hits_left),
        u64::from(observation.decoded.bosses),
        u64::from(observation.decoded.statues),
        u64::from(observation.decoded.mother_brain_defeated),
        u64::from(observation.decoded.ending),
        u64::from(observation.boss_health_seen),
        u64::from(observation.boss_defeats.kraid),
        u64::from(observation.boss_defeats.ridley),
        u64::from(observation.mother_brain_status),
        u64::from(observation.tourian_events.mother_brain_defeated),
        u64::from(observation.tourian_events.escape_started),
        u64::from(observation.dead),
    ]
}

impl<M: NesBackend<Vec<u8>> + NesMemory> TargetExecution for MetroidGame<M> {
    fn continuation_state(&self, target: &MetroidTarget<M>) -> Vec<u64> {
        continuation_observation(&target.observe())
    }

    fn continuation_block_work(&self, _run: &MetroidCampaignRun) -> std::num::NonZeroU64 {
        std::num::NonZeroU64::new(u64::from(crate::metroid::archive::CHORDS.longest_hold()))
            .unwrap()
    }

    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &MetroidSnapshot) -> usize {
        std::mem::size_of::<MetroidSnapshot>().saturating_add(snapshot.emulator_state_bytes_len())
    }

    fn new_target(&self) -> Result<MetroidTarget<M>, String> {
        let machine = (self.machines)().map_err(|error| error.to_string())?;
        MetroidTarget::from_machine(machine, &self.prefix, self.depth)
            .map(|target| target.with_terminal_policy(self.terminal_policy))
            .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut MetroidTarget<M>) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut MetroidTarget<M>,
        snapshot: &MetroidSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn execution_work(&self, target: &MetroidTarget<M>) -> u64 {
        target.execution_work()
    }

    fn apply_action(
        &self,
        target: &mut MetroidTarget<M>,
        action: &ButtonChord,
        aggregate: &mut MetroidMilestones,
    ) -> Result<(), Box<dyn Error>> {
        let (genesis_items, genesis_tanks) = target.genesis_holdings();
        target.apply(action);
        merge_action_milestones(aggregate, target, genesis_items, genesis_tanks);
        Ok(())
    }

    fn snapshot(&self, target: &mut MetroidTarget<M>) -> Result<MetroidSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Metroid".into())
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
    ) -> Result<MetroidCampaignJobResult<M>, Box<dyn Error>> {
        target.restore(origin_snapshot)?;
        for action in replay {
            target.apply(action);
            if self.execution_disposition(target).is_terminal() {
                break;
            }
        }
        execute_suffix(
            target,
            target.genesis_holdings(),
            parent_milestones,
            suffix,
            retention,
            stop_rollout_on_objective,
            self.objective,
        )
    }
}

impl<M: NesBackend<Vec<u8>> + NesMemory> Evaluation for MetroidGame<M> {
    fn execution_disposition(&self, target: &MetroidTarget<M>) -> ExecutionDisposition {
        if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_dead() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        _run: &MetroidCampaignRun,
        target: &MetroidTarget<M>,
    ) -> Result<bool, Box<dyn Error>> {
        Ok(self.objective.reached(target))
    }

    fn current_key(&self, target: &MetroidTarget<M>) -> Result<MetroidArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.mechanical_state()).with_boss_health_seen(target.boss_health_seen()))
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
        target: &MetroidTarget<M>,
    ) -> Result<(), Box<dyn Error>> {
        let state = target.mechanical_state();
        evidence.watermark = evidence.watermark.max(progress_watermark(state));
        evidence
            .observed_map
            .observe(state.area, state.map_x, state.map_y);
        let observation = target.observe();
        evidence
            .named_progress
            .observe(&observation, 0, observation.frame_count);
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
        action: &MetroidCampaignActionResult<M>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<MetroidInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        let mut discoveries = Vec::new();
        let action_end_frame = action.observations.last().map_or(0, |obs| obs.frame_count);
        for observation in &action.observations {
            discoveries.extend(evidence.named_progress.observe(
                observation,
                sequence,
                action_end_frame,
            ));
            let state = observation.decoded;
            if !observation.dead && state.in_play() {
                evidence
                    .observed_map
                    .observe(state.area, state.map_x, state.map_y);
            }
        }
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let genesis_area = evidence.genesis_area.unwrap_or(0);
        let left_starting_area = action.milestones.areas & !(1_u8 << (genesis_area % 8)) != 0;
        let first_input_needed = (left_starting_area
            && evidence.first_inputs.first_new_area.is_none())
            || (action.milestones.gained && evidence.first_inputs.first_gain.is_none());
        let champion = action_champion_key(&action.observations)
            .filter(|key| evidence.champion_key.is_none_or(|current| *key > current));
        let mut improved = Vec::new();
        if let Some(last) = action.observations.last().filter(|last| !last.dead) {
            let state = last.decoded;
            for name in NamedProgress::reached(last) {
                let by_energy = (state.health, state.missiles);
                if evidence
                    .best_energy
                    .get(name)
                    .is_none_or(|best| by_energy > *best)
                {
                    evidence.best_energy.insert(name, by_energy);
                    improved.push((name, "energy"));
                }
                let by_missiles = (state.missiles, state.health);
                if evidence
                    .best_missiles
                    .get(name)
                    .is_none_or(|best| by_missiles > *best)
                {
                    evidence.best_missiles.insert(name, by_missiles);
                    improved.push((name, "missiles"));
                }
                if state.boss_health > 0 {
                    let by_boss = (Reverse(state.boss_health), state.missiles, state.health);
                    if evidence
                        .best_boss
                        .get(name)
                        .is_none_or(|best| by_boss > *best)
                    {
                        evidence.best_boss.insert(name, by_boss);
                        improved.push((name, "boss"));
                    }
                }
            }
        }
        if first_input_needed
            || champion.is_some()
            || !discoveries.is_empty()
            || !improved.is_empty()
        {
            let input = input()?;
            self.publish_milestones(&discoveries, &input)?;
            self.publish_stocked(&improved, &input)?;
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

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn streamed_evidence_preserves_the_owned_reference_bytes_and_resume_fields() {
        let mut evidence = MetroidCampaignEvidence::default();
        evidence.observed_map.0[17] = 19;
        evidence.champion_input.actions.push(ButtonChord {
            buttons: 3,
            hold_frames: 7,
        });
        evidence.best_energy.insert("ridley_area", (3, 5));
        evidence.best_missiles.insert("ridley_area", (9, 11));
        evidence
            .best_boss
            .insert("ridley_area", (Reverse(13), 15, 17));
        evidence.genesis_area = Some(4);
        let names = |values: &BTreeMap<&'static str, _>| {
            values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect()
        };
        let reference = MetroidEvidenceCheckpoint {
            observed_map: evidence.observed_map.0.to_vec(),
            first_seen: names(&evidence.named_progress.first_seen),
            max_missile_capacity: evidence.named_progress.max_missile_capacity,
            max_energy_tanks: evidence.named_progress.max_energy_tanks,
            aggregate: evidence.aggregate,
            watermark: evidence.watermark,
            first_reached: evidence.first_reached,
            first_inputs: evidence.first_inputs.clone(),
            champion_input: evidence.champion_input.clone(),
            champion_milestones: evidence.champion_milestones,
            champion_key: evidence.champion_key,
            genesis_area: evidence.genesis_area,
            best_energy: evidence
                .best_energy
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
            best_missiles: evidence
                .best_missiles
                .iter()
                .map(|(name, value)| ((*name).to_owned(), *value))
                .collect(),
            best_boss: evidence
                .best_boss
                .iter()
                .map(|(name, (Reverse(health), missiles, energy))| {
                    ((*name).to_owned(), (*health, *missiles, *energy))
                })
                .collect(),
        };
        let bytes = MetroidGame::<QuickNesMachine>::evidence_checkpoint(&evidence).unwrap();
        assert_eq!(bytes, postcard::to_allocvec(&reference).unwrap());
        let restored = MetroidGame::<QuickNesMachine>::evidence_from_checkpoint(&bytes).unwrap();
        assert_eq!(
            MetroidGame::<QuickNesMachine>::evidence_checkpoint(&restored).unwrap(),
            bytes
        );
    }

    #[test]
    fn named_discovery_reconstructs_input_independently_of_output_configuration() {
        use crate::metroid::{
            progress::{BossDefeats, TourianEvents},
            target::decode_state,
        };
        let directory =
            std::env::temp_dir().join(format!("metroid-discovery-test-{}", std::process::id()));
        let mut wram = [0; 2048];
        wram[0x1e] = 3;
        wram[0x107] = 3;
        wram[0x74] = 0x14;
        let observation = MetroidObservations {
            frame_count: 99,
            decoded: decode_state(&wram, &[0; 8192]).unwrap(),
            boss_health_seen: 0,
            boss_defeats: BossDefeats::default(),
            mother_brain_status: 0,
            tourian_events: TourianEvents::default(),
            dead: false,
        };
        let action = MetroidCampaignActionResult {
            action: ButtonChord::new(0, 1),
            observations: vec![observation],
            milestones: MetroidMilestones::default(),
            outcome: Outcome::default(),
            candidate: None,
        };
        for publish in [false, true] {
            let mut game = MetroidGame::new(&[], Path::new("unused"), "unused", "test-execution");
            if publish {
                game = game.with_milestone_input_dir(directory.clone());
            }
            let mut evidence = MetroidCampaignEvidence {
                champion_key: action_champion_key(&action.observations),
                ..Default::default()
            };
            let mut reconstructions = 0;
            game.merge_action_evidence(&mut evidence, &action, 12, || {
                reconstructions += 1;
                Ok(MetroidInput::default())
            })
            .unwrap();
            assert_eq!(reconstructions, 1);
            game.merge_action_evidence(&mut evidence, &action, 13, || {
                panic!("an unchanged discovery must not reconstruct again")
            })
            .unwrap();
            assert_eq!(
                evidence.named_progress.first_seen["ridley_area"]
                    .unwrap()
                    .execution,
                12
            );
        }
        assert!(directory.join("ridley_area.json").is_file());
        assert!(directory.join("ridley_area-energy.json").is_file());
        assert!(directory.join("ridley_area-missiles.json").is_file());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_better_stocked_arrival_at_a_milestone_republishes_its_tape() {
        use crate::metroid::{
            progress::{BossDefeats, TourianEvents},
            target::decode_state,
        };
        let directory =
            std::env::temp_dir().join(format!("metroid-stocked-test-{}", std::process::id()));
        let mut wram = [0; 2048];
        let mut cartridge = [0; 8192];
        wram[0x1e] = 3;
        wram[0x107] = 3;
        wram[0x74] = 0x14;
        let observe = |wram: &[u8; 2048], cartridge: &[u8; 8192]| MetroidObservations {
            frame_count: 99,
            decoded: decode_state(wram, cartridge).unwrap(),
            boss_health_seen: 0,
            boss_defeats: BossDefeats::default(),
            mother_brain_status: 0,
            tourian_events: TourianEvents::default(),
            dead: false,
        };
        let action_with = |observation: MetroidObservations, actions: usize| {
            (
                MetroidCampaignActionResult {
                    action: ButtonChord::new(0, 1),
                    observations: vec![observation],
                    milestones: MetroidMilestones::default(),
                    outcome: Outcome::default(),
                    candidate: None,
                },
                MetroidInput {
                    actions: vec![ButtonChord::new(0, 1); actions],
                },
            )
        };
        let game = MetroidGame::new(&[], Path::new("unused"), "unused", "test-execution")
            .with_milestone_input_dir(directory.clone());
        let mut evidence = MetroidCampaignEvidence::default();
        let (first, first_input) = action_with(observe(&wram, &cartridge), 1);
        game.merge_action_evidence(&mut evidence, &first, 1, || Ok(first_input.clone()))
            .unwrap();
        let first_health = first.observations[0].decoded.health;
        let first_missiles = first.observations[0].decoded.missiles;
        wram[0x107] = 4;
        let (healthier, healthier_input) = action_with(observe(&wram, &cartridge), 2);
        assert!(healthier.observations[0].decoded.health > first_health);
        game.merge_action_evidence(&mut evidence, &healthier, 2, || Ok(healthier_input.clone()))
            .unwrap();
        wram[0x107] = 3;
        cartridge[0x879] = 9;
        let (stocked, stocked_input) = action_with(observe(&wram, &cartridge), 3);
        assert!(stocked.observations[0].decoded.missiles > first_missiles);
        game.merge_action_evidence(&mut evidence, &stocked, 3, || Ok(stocked_input.clone()))
            .unwrap();
        game.merge_action_evidence(&mut evidence, &first, 4, || {
            panic!("a worse-stocked arrival must not reconstruct")
        })
        .unwrap();
        let read = |name: &str| -> MetroidInput {
            serde_json::from_slice(&std::fs::read(directory.join(name)).unwrap()).unwrap()
        };
        assert_eq!(read("ridley_area.json").actions.len(), 1);
        assert_eq!(read("ridley_area-energy.json").actions.len(), 2);
        assert_eq!(read("ridley_area-missiles.json").actions.len(), 3);
        assert!(!directory.join("ridley_area-boss.json").is_file());
        wram[0x40b] = 40;
        wram[0x40f] = 0x40;
        cartridge[0xaf4] = 0x01;
        cartridge[0xb02] = 0x09;
        let (fighting, fighting_input) = action_with(observe(&wram, &cartridge), 4);
        assert_eq!(fighting.observations[0].decoded.boss_health, 40);
        game.merge_action_evidence(&mut evidence, &fighting, 5, || Ok(fighting_input.clone()))
            .unwrap();
        wram[0x40b] = 20;
        let (hurting, hurting_input) = action_with(observe(&wram, &cartridge), 5);
        game.merge_action_evidence(&mut evidence, &hurting, 6, || Ok(hurting_input.clone()))
            .unwrap();
        game.merge_action_evidence(&mut evidence, &fighting, 7, || {
            panic!("a boss with more health left must not reconstruct")
        })
        .unwrap();
        assert_eq!(read("ridley_area-boss.json").actions.len(), 5);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_checkpoint_from_the_previous_snapshot_layout_is_rejected_by_its_format() {
        use crate::{
            metroid::target::MetroidMechanicalState, search::campaign::SnapshotCheckpointEntry,
        };

        let checkpoint = |format: &str| MetroidSnapshotCheckpoint {
            format: format.to_owned(),
            entries: vec![SnapshotCheckpointEntry {
                id: 0,
                snapshot: MetroidSnapshot::for_census_tests(MetroidMechanicalState::default()),
            }],
        };
        let current = checkpoint(SNAPSHOT_CHECKPOINT_FORMAT).to_bytes().unwrap();
        assert_eq!(
            MetroidSnapshotCheckpoint::from_bytes(&current, SNAPSHOT_CHECKPOINT_FORMAT).unwrap(),
            checkpoint(SNAPSHOT_CHECKPOINT_FORMAT)
        );
        let previous = checkpoint("metroid-quicknes-snapshot-checkpoint-v12")
            .to_bytes()
            .unwrap();
        let error = MetroidSnapshotCheckpoint::from_bytes(&previous, SNAPSHOT_CHECKPOINT_FORMAT)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "snapshot checkpoint format is not recognized"
        );
    }

    #[test]
    fn continuation_observation_ignores_clock_and_preserves_mechanical_changes() {
        let baseline = MetroidObservations {
            frame_count: 0,
            decoded: Default::default(),
            boss_health_seen: 0,
            boss_defeats: Default::default(),
            mother_brain_status: 0,
            tourian_events: Default::default(),
            dead: false,
        };
        let expected = super::continuation_observation(&baseline);
        let mut clock_only = baseline.clone();
        clock_only.frame_count = u64::MAX;
        assert_eq!(expected, super::continuation_observation(&clock_only));
        let mut changed = baseline.clone();
        changed.decoded.area = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.map_x = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.map_y = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.x = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.y = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.pose = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.door = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.mode = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.health = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.equipment = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.missiles = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.missile_capacity = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.energy_tanks = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.boss_health = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.zebetites_destroyed = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.zebetite_hits_left = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.bosses = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.statues = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.mother_brain_defeated = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.decoded.ending = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.boss_health_seen = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.boss_defeats.kraid = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.boss_defeats.ridley = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.mother_brain_status = 1;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.tourian_events.mother_brain_defeated = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.tourian_events.escape_started = true;
        assert_ne!(expected, super::continuation_observation(&changed));
        let mut changed = baseline.clone();
        changed.dead = true;
        assert_ne!(expected, super::continuation_observation(&changed));
    }

    #[test]
    fn the_census_separates_a_cell_never_drawn_from_one_drawn_without_result() {
        use crate::metroid::target::MetroidMechanicalState;

        let at = |map_x, map_y, equipment, health| MetroidMechanicalState {
            area: 0x11,
            map_x,
            map_y,
            equipment,
            health,
            ..MetroidMechanicalState::default()
        };
        let cached = [
            (at(3, 4, 0b0001, 100), 7),
            (at(3, 4, 0b0100, 250), 0),
            (at(9, 1, 0b0001, 300), 0),
        ]
        .map(|(state, selections)| (MetroidSnapshot::for_census_tests(state), selections));
        let census = <MetroidGame<QuickNesMachine>>::retained_diagnostics(
            cached
                .iter()
                .map(|(snapshot, selections)| (Some(snapshot), *selections))
                .chain(std::iter::once((None, 5))),
        )
        .expect("census");

        assert_eq!(census["active_entries"], 4);
        assert_eq!(census["missing_snapshots"], 1);
        let cells = &census["live_entries_by_map_cell"];
        assert_eq!(cells["17:3:4"], serde_json::json!([2, 250, 0, 0b0101, 7]));
        assert_eq!(cells["17:9:1"], serde_json::json!([1, 300, 0, 0b0001, 0]));
        let kit = &census["live_entries_by_map_cell_and_equipment"];
        assert_eq!(kit["17:3:4:1"], serde_json::json!([1, 7]));
        assert_eq!(kit["17:3:4:4"], serde_json::json!([1, 0]));
    }

    #[test]
    fn milestone_objective_requires_matching_replay_policy() {
        let ending = MetroidGame::new(&[0], Path::new("unused"), "test", "test-execution");
        let high_jump = MetroidGame::new(&[0], Path::new("unused"), "test", "test-execution")
            .with_objective(MetroidObjective::HighJump);
        let ending_policy = ending.policies(&MetroidCampaignRun);
        let high_jump_policy = high_jump.policies(&MetroidCampaignRun);
        assert!(high_jump.resolve_recorded(&high_jump_policy).is_ok());
        assert!(ending.resolve_recorded(&ending_policy).is_ok());
        assert!(ending.resolve_recorded(&high_jump_policy).is_err());
        assert!(high_jump.resolve_recorded(&ending_policy).is_err());
        assert_eq!(high_jump_policy["objective"], "high_jump");
        assert_eq!(ending_policy["objective"], "game_victory");
        assert_eq!(
            high_jump.continuation_block_work(&MetroidCampaignRun).get(),
            u64::from(crate::metroid::archive::CHORDS.longest_hold())
        );
    }

    #[test]
    fn terminal_semantics_require_a_matching_replay_context() {
        let legacy = MetroidGame::new(&[0], Path::new("unused"), "test", "test-execution")
            .with_terminal_policy(MetroidTerminalPolicy::Legacy);
        let corrected = MetroidGame::new(&[0], Path::new("unused"), "test", "test-execution")
            .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
        let old = legacy.policies(&MetroidCampaignRun);
        let new = corrected.policies(&MetroidCampaignRun);
        assert!(legacy.resolve_recorded(&old).is_ok());
        assert!(corrected.resolve_recorded(&new).is_ok());
        assert!(corrected.resolve_recorded(&old).is_err());
        assert!(legacy.resolve_recorded(&new).is_err());
        assert_eq!(
            old.iter().filter(|(k, v)| new.get(*k) != Some(*v)).count(),
            1
        );
        let unset = MetroidGame::new(&[0], Path::new("unused"), "test", "test-execution");
        assert!(
            corrected
                .resolve_recorded(&unset.policies(&MetroidCampaignRun))
                .is_ok()
        );
    }

    #[test]
    fn native_policy_rejects_a_changed_prepared_execution() {
        let first = MetroidGame::new(&[0], Path::new("core.so"), "core", "first");
        let second = MetroidGame::new(&[0], Path::new("core.so"), "core", "second");
        assert!(
            first
                .emulator_identity()
                .contains("prepared-execution-sha256=first")
        );
        assert!(
            second
                .resolve_recorded(&first.policies(&MetroidCampaignRun))
                .is_err()
        );
    }

    #[test]
    fn observed_map_union_deduplicates_and_keeps_area_identity() {
        let mut coverage = MapCoverage::default();
        for _ in 0..100 {
            coverage.observe(16, 3, 14);
            coverage.observe(17, 3, 14);
        }
        coverage.observe(255, 31, 31);
        coverage.observe(16, 32, 0);
        coverage.observe(16, 0, 255);
        assert_eq!(coverage.count(), 3);
        assert_eq!(coverage.0.len() * size_of::<u64>(), 32768);
    }
}
