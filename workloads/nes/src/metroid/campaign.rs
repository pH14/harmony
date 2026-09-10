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
        progress::NamedProgress,
        target::{
            ButtonChord, MetroidInput, MetroidObservations, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy, power_on_walk, preference_tuple,
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
pub const CAMPAIGN_STREAM_FORMAT: &str = if cfg!(feature = "metroid-retention-progress") {
    "metroid-quicknes-campaign-stream-scoped-progress-v6"
} else if cfg!(feature = "metroid-boss-context-audit") {
    "metroid-quicknes-campaign-stream-endpoint-context-v5"
} else {
    "metroid-quicknes-campaign-stream-v4"
};
/// Snapshot checkpoint format written by Metroid campaigns.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = if cfg!(feature = "metroid-boss-context-audit") {
    "metroid-quicknes-snapshot-checkpoint-endpoint-context-v5"
} else {
    "metroid-quicknes-snapshot-checkpoint-v4"
};

/// First observed live endpoint encounter and its producing searched input.
pub const ENDPOINT_ENCOUNTER_ARTIFACT: &str = "first-endpoint-encounter.json";
/// Format of the optional first-encounter artifact.
pub const ENDPOINT_ENCOUNTER_ARTIFACT_FORMAT: &str = "metroid-endpoint-encounter-input-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_select_taps_no_start_v1";
const RESULT_DIGEST_IDENTIFIER: &str = if cfg!(feature = "metroid-retention-progress") {
    "metroid-semantic-postcard-1.1.3-sha256-hex-motion-endpoint-scoped-progress-v7"
} else if cfg!(feature = "metroid-boss-context-audit") {
    if cfg!(feature = "metroid-motion-context") {
        "metroid-semantic-postcard-1.1.3-sha256-hex-motion-endpoint-context-v6"
    } else {
        "metroid-semantic-postcard-1.1.3-sha256-hex-endpoint-context-v6"
    }
} else if cfg!(feature = "metroid-motion-context") {
    "metroid-semantic-postcard-1.1.3-sha256-hex-motion-v5"
} else {
    "metroid-semantic-postcard-1.1.3-sha256-hex-v4"
};

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
    chord_correlation: crate::chord_correlation::ChordCorrelation,
    local_terminal_retry: bool,
    champion_input_path: Option<PathBuf>,
    milestone_input_dir: Option<PathBuf>,
    #[cfg(feature = "metroid-boss-context-audit")]
    endpoint_encounter_path: Option<PathBuf>,
    terminal_policy: MetroidTerminalPolicy,
    retention_audit: Option<std::sync::Mutex<super::retention_audit::RetentionAudit>>,
    retention_capture: Option<std::sync::Mutex<super::retention_capture::RetentionCapture>>,
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
             result_digest={RESULT_DIGEST_IDENTIFIER};sha256={core_sha256}",
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
            chord_correlation: crate::chord_correlation::ChordCorrelation::Independent,
            local_terminal_retry: false,
            champion_input_path: None,
            milestone_input_dir: None,
            #[cfg(feature = "metroid-boss-context-audit")]
            endpoint_encounter_path: None,
            retention_audit: None,
            retention_capture: None,
            terminal_policy: MetroidTerminalPolicy::Legacy,
        }
    }

    /// Select explicitly versioned terminal observation semantics.
    #[must_use]
    pub fn with_terminal_policy(mut self, policy: MetroidTerminalPolicy) -> Self {
        self.terminal_policy = policy;
        self
    }

    /// Opt in to one ordinary-death retry per live boundary. Attempts consume
    /// the existing pre-drawn suffix; all failed work stays charged and recorded.
    #[must_use]
    pub fn with_local_terminal_retry(mut self, enabled: bool) -> Self {
        self.local_terminal_retry = enabled;
        self
    }

    /// Set an explicitly recorded suffix-local action correlation policy.
    #[must_use]
    pub fn with_chord_correlation(
        mut self,
        policy: crate::chord_correlation::ChordCorrelation,
    ) -> Self {
        self.chord_correlation = policy;
        self
    }

    /// Write the champion input to `path` each time it improves, so a long
    /// run can be filmed at its deepest point before it ends.
    #[must_use]
    pub fn with_champion_input_path(mut self, path: PathBuf) -> Self {
        self.champion_input_path = Some(path);
        self
    }

    /// Export one searched witness per named discovery. This reporting path
    /// never supplies inputs back to the searcher.
    #[must_use]
    pub fn with_milestone_input_dir(mut self, directory: PathBuf) -> Self {
        self.milestone_input_dir = Some(directory);
        self
    }

    /// Export the first classified live endpoint for independent replay.
    /// This artifact never supplies inputs or feedback to the searcher.
    #[cfg(feature = "metroid-boss-context-audit")]
    #[must_use]
    pub fn with_endpoint_encounter_path(mut self, path: PathBuf) -> Self {
        self.endpoint_encounter_path = Some(path);
        self
    }

    #[cfg(feature = "metroid-boss-context-audit")]
    fn publish_endpoint_encounter(
        &self,
        first: &EndpointEncounter,
        input: &MetroidInput,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(path) = &self.endpoint_encounter_path {
            let temporary = path.with_extension("json.tmp");
            let value = serde_json::json!({
                "format": ENDPOINT_ENCOUNTER_ARTIFACT_FORMAT,
                "first": first,
                "input": input,
            });
            std::fs::write(&temporary, serde_json::to_vec(&value)?)?;
            std::fs::rename(temporary, path)?;
        }
        Ok(())
    }

    /// Enable bounded replacement-pair sampling without changing search semantics.
    #[must_use]
    pub fn with_retention_audit(mut self, path: PathBuf) -> Self {
        self.retention_audit = Some(std::sync::Mutex::new(
            super::retention_audit::RetentionAudit::new(path),
        ));
        self
    }

    /// Capture all ordinary single-member competitions for a bounded replay.
    /// The caller pins the actual replay stream/origin and accounts for host I/O.
    /// No emulator work occurs here. Existing output paths are refused.
    pub fn with_retention_capture(
        mut self,
        path: &Path,
        identity: super::retention_capture::CaptureIdentity,
    ) -> Result<Self, Box<dyn Error>> {
        self.retention_capture = Some(std::sync::Mutex::new(
            super::retention_capture::RetentionCapture::create(path, identity)?,
        ));
        Ok(self)
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
    observed_map: MapCoverage,
    underflow: UnderflowDiagnostics,
    named_progress: NamedProgress,
    #[cfg(feature = "metroid-boss-context-audit")]
    endpoint_encounters: EndpointEncounters,
    aggregate: MetroidMilestones,
    watermark: MetroidProgressWatermark,
    first_reached: MetroidMilestoneTimes,
    first_inputs: MetroidMilestoneInputs,
    champion_input: MetroidInput,
    champion_milestones: MetroidMilestones,
    champion_key: Option<MetroidChampionKey>,
    genesis_area: Option<u8>,
}

/// Fixed-size reporting counters; no search decisions or snapshot storage.
#[derive(Clone, Default)]
struct UnderflowDiagnostics {
    endpoints: u64,
    eligible: u64,
    first_execution: Option<u64>,
    max_health: u16,
}

#[cfg(feature = "metroid-boss-context-audit")]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct EndpointEncounter {
    execution: u64,
    route_action_end_frame: u64,
    area: u8,
    boss_slots: u8,
}

/// Constant-size endpoint counts. Campaigns merge admitted actions; witness
/// replay merges replayed actions. Origin/restored observations are excluded.
#[cfg(feature = "metroid-boss-context-audit")]
#[derive(Clone, Default, Serialize)]
struct EndpointEncounters {
    actions_observed: u64,
    live_endpoints: u64,
    classified_endpoints: u64,
    first: Option<EndpointEncounter>,
}

#[cfg(feature = "metroid-boss-context-audit")]
impl EndpointEncounters {
    fn observe(&mut self, observations: &[MetroidObservations], sequence: u64) -> bool {
        if sequence == 0 {
            return false;
        }
        self.actions_observed += 1;
        let Some(endpoint) = observations.last().filter(|endpoint| !endpoint.dead) else {
            return false;
        };
        let Some(boss_slots) = endpoint.endpoint_boss_slots else {
            return false;
        };
        self.live_endpoints += 1;
        if boss_slots == 0 {
            return false;
        }
        self.classified_endpoints += 1;
        if self.first.is_some() {
            return false;
        }
        self.first = Some(EndpointEncounter {
            execution: sequence,
            route_action_end_frame: endpoint.frame_count,
            area: endpoint.decoded.area,
            boss_slots,
        });
        true
    }
}

/// Observation-only bitmap over raw area bytes and the engine's 32x32 map.
/// Its fixed 32 KiB allocation cannot grow with campaign history. It has no
/// route semantics and is never exposed to archive keys or input selection.
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    discard_previous_dead_actions: Vec<usize>,
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
    postcard_value_sha256(&MetroidResult {
        actions,
        discard_previous_dead_actions: result.discarded_attempt_positions(),
    })
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

fn target_archive_key(target: &MetroidTarget) -> Result<MetroidArchiveKey, Box<dyn Error>> {
    let key = archive_key(target.mechanical_state());
    #[cfg(feature = "metroid-motion-context")]
    let key = MetroidArchiveKey {
        motion_context: Some(target.cached_motion_context()),
        ..key
    };
    #[cfg(feature = "metroid-retention-progress")]
    let key = MetroidArchiveKey {
        retention_progress: target.qualified_retention_progress()?,
        ..key
    };
    Ok(key)
}

#[allow(clippy::too_many_arguments)] // Mirrors the existing bounded worker request plus its opt-in retry policy.
fn execute_suffix(
    target: &mut MetroidTarget,
    genesis: (u8, u8),
    parent_actions: usize,
    parent_milestones: MetroidMilestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
    local_terminal_retry: bool,
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
    let live = if local_terminal_retry {
        Some((
            target
                .snapshot()
                .ok_or("local retry parent snapshot failed")?,
            aggregate,
        ))
    } else {
        None
    };
    let mut retry = crate::search::rollout::LocalRetry::new(live);
    for action in suffix {
        if length >= max_actions {
            break;
        }
        // Failed attempts consume this same cap; a retry draws no extra action.
        length = length.saturating_add(1);
        let restored = retry.restore_pending(|snapshot| target.restore(snapshot))?;
        let discard_previous_dead = restored.is_some();
        if let Some(live_milestones) = restored {
            aggregate = live_milestones;
        }
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
                key: target_archive_key(target)?,
                viable: true,
                snapshot,
            })
        };
        let continue_rollout = retry.observe(
            crate::search::rollout::Outcome {
                dead,
                victory,
                failed,
            },
            candidate.as_ref().map(|candidate| &candidate.snapshot),
            aggregate,
        )?;
        actions.push(CampaignActionResult {
            discard_previous_dead,
            action: *action,
            observations,
            milestones: aggregate,
            dead,
            victory,
            failed,
            candidate,
        });
        if !continue_rollout {
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
    fn observe_retention(
        &self,
        event: &crate::search::archive::RetentionObservation<
            '_,
            ButtonChord,
            MetroidArchiveKey,
            MetroidSnapshot,
        >,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(audit) = &self.retention_audit {
            audit
                .lock()
                .map_err(|_| "retention audit lock poisoned")?
                .observe(event)?;
        }
        if let Some(capture) = &self.retention_capture {
            capture
                .lock()
                .map_err(|_| "retention capture lock poisoned")?
                .observe(event)?;
        }
        Ok(())
    }

    fn finish_retention_observation(&self) -> Result<(), Box<dyn Error>> {
        if let Some(audit) = &self.retention_audit {
            audit
                .lock()
                .map_err(|_| "retention audit lock poisoned")?
                .finish()?;
        }
        if let Some(capture) = &self.retention_capture {
            capture
                .lock()
                .map_err(|_| "retention capture lock poisoned")?
                .finish()?;
        }
        Ok(())
    }

    fn retained_diagnostics<'a>(
        snapshots: impl Iterator<Item = Option<&'a MetroidSnapshot>>,
    ) -> Option<serde_json::Value> {
        let (mut active, mut missing) = (0_u64, 0_u64);
        let (mut equipment, mut bosses, mut missiles, mut tanks) = (0, 0, 0, 0);
        let mut maps = MapCoverage::default();
        let (mut underflows, mut health) = (0_u64, 0_u16);
        for snapshot in snapshots {
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
        }
        Some(serde_json::json!({
            "scope": "union/maxima over cached active endpoints; not one trajectory; lower bounds when snapshots are missing",
            "active_entries": active, "missing_snapshots": missing,
            "underflow_endpoints_cached": underflows, "max_health_cached": health,
            "equipment_union": equipment, "max_bosses": bosses,
            "max_missile_capacity": missiles, "max_energy_tanks": tanks,
            "map_cells_retained_cached": maps.count(), "temporary_bitmap_bytes": 32768
        }))
    }

    fn diagnostics(evidence: &MetroidCampaignEvidence) -> Option<serde_json::Value> {
        let value = serde_json::json!({
            "map_cells_observed": evidence.observed_map.count(),
            "coverage_bitmap_bytes": 32768,
            "named_progress": evidence.named_progress,
            "underflow_action_endpoints": evidence.underflow.endpoints,
            "underflow_candidate_eligible_endpoints": evidence.underflow.eligible,
            "first_underflow_execution": evidence.underflow.first_execution,
            "max_observed_endpoint_health": evidence.underflow.max_health,
            "underflow_diagnostic_memory_bytes": std::mem::size_of::<UnderflowDiagnostics>(),
            "observation_filter": "live gameplay observations supplied to this accumulator"
        });
        #[cfg(feature = "metroid-boss-context-audit")]
        let value = {
            let mut value = value;
            value["endpoint_encounters"] = serde_json::json!({
                "format": "metroid-live-endpoint-encounters-v1",
                "sampling": "completed live action endpoints; origins and intermediate observations excluded",
                "counts": evidence.endpoint_encounters,
            });
            value
        };
        Some(value)
    }
    fn merge_witness_diagnostics(
        evidence: &mut MetroidCampaignEvidence,
        observations: &[MetroidObservations],
        sequence: u64,
    ) {
        #[cfg(feature = "metroid-boss-context-audit")]
        evidence.endpoint_encounters.observe(observations, sequence);
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
        let mut policies: GamePolicies = [
            (
                CONTROLLER_VOCABULARY_FIELD,
                CONTROLLER_VOCABULARY_IDENTIFIER,
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (TERMINAL_POLICY_FIELD, self.terminal_policy.identifier()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .chain(std::iter::once((
            EMULATOR_BACKEND_FIELD.to_owned(),
            self.identity.clone(),
        )))
        .collect();
        #[cfg(feature = "metroid-boss-context-audit")]
        policies.insert(
            "endpoint_encounter_observation".to_owned(),
            "metroid-live-endpoint-boss-slots-v1".to_owned(),
        );
        if self.chord_correlation != crate::chord_correlation::ChordCorrelation::Independent {
            policies.insert(
                "action_correlation".to_owned(),
                self.chord_correlation.identifier().to_owned(),
            );
        }
        if self.local_terminal_retry {
            policies.insert(
                "local_terminal_retry".to_owned(),
                "one_per_live_boundary_predrawn_attempts_v1".to_owned(),
            );
        }
        policies
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
        if self.chord_correlation != crate::chord_correlation::ChordCorrelation::Independent
            && mixture.mixture != DrawMixture::AlphabetOnly
        {
            return Err("action correlation currently requires alphabet_only".into());
        }
        if self.local_terminal_retry
            && (mixture.mixture != DrawMixture::AlphabetOnly
                || self.chord_correlation
                    != crate::chord_correlation::ChordCorrelation::Independent)
        {
            return Err("local terminal retry requires independent alphabet_only draws".into());
        }
        let mut suffix = draw_suffix(
            shape,
            mixture.mixture,
            mixture.weight,
            mutation_seed,
            |_| Ok(None),
            sample_chord,
        )?;
        self.chord_correlation
            .apply(&mut suffix, mutation_seed, 4)?;
        Ok(suffix)
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
        .map(|target| target.with_terminal_policy(self.terminal_policy))
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
            self.local_terminal_retry,
        )
    }
}

impl Evaluation for MetroidGame {
    fn named_milestone(&self, name: &str) -> Option<(&'static str, &'static str)> {
        NamedProgress::default()
            .first_seen
            .get_key_value(name)
            .map(|(name, _)| (*name, super::progress::NAMED_PROGRESS_POLICY))
    }

    fn milestone_first_execution(
        &self,
        evidence: &MetroidCampaignEvidence,
        name: &str,
    ) -> Option<u64> {
        evidence
            .named_progress
            .first_seen
            .get(name)?
            .map(|seen| seen.execution)
    }

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
        target_archive_key(target)
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
        action: &MetroidCampaignActionResult,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<MetroidInput, Box<dyn Error>>,
    {
        #[cfg(feature = "metroid-boss-context-audit")]
        let first_encounter_needed = evidence
            .endpoint_encounters
            .observe(&action.observations, sequence);
        #[cfg(not(feature = "metroid-boss-context-audit"))]
        let first_encounter_needed = false;
        if let Some(endpoint) = action.observations.last() {
            evidence.underflow.max_health =
                evidence.underflow.max_health.max(endpoint.decoded.health);
            if endpoint.decoded.health >= 8000 {
                evidence.underflow.endpoints += 1;
                evidence.underflow.eligible += u64::from(action.candidate.is_some());
                evidence.underflow.first_execution.get_or_insert(sequence);
            }
        }
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
        // Reconstruction is counted in deterministic reports. Whether files
        // are published must not change that count when the stream is replayed.
        if first_input_needed
            || champion.is_some()
            || !discoveries.is_empty()
            || first_encounter_needed
        {
            let input = input()?;
            #[cfg(feature = "metroid-boss-context-audit")]
            if first_encounter_needed {
                self.publish_endpoint_encounter(
                    evidence
                        .endpoint_encounters
                        .first
                        .as_ref()
                        .expect("new encounter"),
                    &input,
                )?;
            }
            self.publish_milestones(&discoveries, &input)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_retry_identity_draws_and_semantic_digest_are_explicit() {
        let ordinary = MetroidGame::new(&[0], Path::new("unused"), "test");
        let old = ordinary.policies(&MetroidCampaignRun);
        let candidate =
            MetroidGame::new(&[0], Path::new("unused"), "test").with_local_terminal_retry(true);
        let new = candidate.policies(&MetroidCampaignRun);
        assert!(!old.contains_key("local_terminal_retry"));
        assert_eq!(new.len(), old.len() + 1);
        assert!(ordinary.resolve_recorded(&new).is_err());
        assert!(candidate.resolve_recorded(&old).is_err());
        assert!(candidate.resolve_recorded(&new).is_ok());
        let mix = MixtureDraw {
            mixture: DrawMixture::AlphabetOnly,
            weight: 0,
            splice_weight: 0,
        };
        for seed in 0..32 {
            assert_eq!(
                ordinary
                    .expand_suffix(&MetroidCampaignRun, &(), SuffixShape::OneToSix, mix, seed)
                    .unwrap(),
                candidate
                    .expand_suffix(&MetroidCampaignRun, &(), SuffixShape::OneToSix, mix, seed)
                    .unwrap()
            );
        }
        let mut result = CampaignJobResult::<MetroidGame> {
            actions: vec![CampaignActionResult {
                discard_previous_dead: false,
                action: ButtonChord::new(1, 1),
                observations: vec![],
                milestones: MetroidMilestones::default(),
                dead: true,
                victory: false,
                failed: false,
                candidate: None,
            }],
        };
        let old_hash = metroid_result_sha256(&result).unwrap();
        result.actions[0].discard_previous_dead = true;
        assert_ne!(old_hash, metroid_result_sha256(&result).unwrap());
    }

    #[test]
    fn correlation_identity_is_strict_and_suffix_local() {
        use crate::chord_correlation::ChordCorrelation;
        let ordinary = MetroidGame::new(&[0], Path::new("unused"), "test");
        let old = ordinary.policies(&MetroidCampaignRun);
        assert!(!old.contains_key("action_correlation"));
        for policy in [
            ChordCorrelation::ComponentHalf,
            ChordCorrelation::MatchedWholeRepeat,
        ] {
            let candidate =
                MetroidGame::new(&[0], Path::new("unused"), "test").with_chord_correlation(policy);
            let new = candidate.policies(&MetroidCampaignRun);
            assert_eq!(new.len(), old.len() + 1);
            assert_eq!(new["action_correlation"], policy.identifier());
            assert!(old.iter().all(|(key, value)| new.get(key) == Some(value)));
            assert!(candidate.resolve_recorded(&new).is_ok());
            assert!(ordinary.resolve_recorded(&new).is_err());
            assert!(candidate.resolve_recorded(&old).is_err());
            let mix = MixtureDraw {
                mixture: DrawMixture::AlphabetOnly,
                weight: 0,
                splice_weight: 0,
            };
            let first = candidate
                .expand_suffix(&MetroidCampaignRun, &(), SuffixShape::OneToSix, mix, 123)
                .unwrap();
            candidate
                .expand_suffix(&MetroidCampaignRun, &(), SuffixShape::OneToSix, mix, 789)
                .unwrap();
            assert_eq!(
                first,
                candidate
                    .expand_suffix_recorded(
                        &MetroidCampaignRun,
                        &(),
                        SuffixShape::OneToSix,
                        mix,
                        None,
                        123
                    )
                    .unwrap()
            );
            assert!(
                candidate
                    .expand_suffix(
                        &MetroidCampaignRun,
                        &(),
                        SuffixShape::OneToSix,
                        MixtureDraw {
                            mixture: DrawMixture::BiasedHalf,
                            weight: 0,
                            splice_weight: 0,
                        },
                        123
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn terminal_semantics_require_a_matching_replay_context() {
        let legacy = MetroidGame::new(&[0], Path::new("unused"), "test");
        let corrected = MetroidGame::new(&[0], Path::new("unused"), "test")
            .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
        let old = legacy.policies(&MetroidCampaignRun);
        let new = corrected.policies(&MetroidCampaignRun);
        assert!(legacy.resolve_recorded(&old).is_ok());
        assert!(corrected.resolve_recorded(&new).is_ok());
        assert!(legacy.resolve_recorded(&new).is_err());
        assert!(corrected.resolve_recorded(&old).is_err());
        assert_eq!(
            old.iter().filter(|(k, v)| new.get(*k) != Some(*v)).count(),
            1
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
            boss_defeats: BossDefeats::default(),
            mother_brain_status: 0,
            tourian_events: TourianEvents::default(),
            #[cfg(feature = "metroid-boss-context-audit")]
            endpoint_boss_slots: None,
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        };
        let action = MetroidCampaignActionResult {
            discard_previous_dead: false,
            action: ButtonChord::new(0, 1),
            observations: vec![observation],
            milestones: MetroidMilestones::default(),
            dead: false,
            victory: false,
            failed: false,
            candidate: None,
        };
        for publish in [false, true] {
            let mut game = MetroidGame::new(&[], Path::new("unused"), "unused");
            if publish {
                game = game.with_milestone_input_dir(directory.clone());
            }
            let mut evidence = MetroidCampaignEvidence {
                // Suppress the scalar champion/first-gain paths: this second
                // area is solely a new named discovery.
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
            // Repeated discoveries must stay bounded.
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
        std::fs::remove_dir_all(directory).unwrap();
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

    #[cfg(feature = "metroid-boss-context-audit")]
    fn endpoint_observation(slots: Option<u8>) -> MetroidObservations {
        let mut wram = [0; 2048];
        wram[0x1e] = 3;
        wram[0x107] = 3;
        wram[0x74] = 0x14;
        MetroidObservations {
            frame_count: 99,
            decoded: super::super::target::decode_state(&wram, &[0; 8192]).unwrap(),
            boss_defeats: Default::default(),
            mother_brain_status: 0,
            tourian_events: Default::default(),
            endpoint_boss_slots: slots,
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        }
    }

    #[cfg(feature = "metroid-boss-context-audit")]
    #[test]
    fn encounter_counts_exclude_origins_intermediate_and_unavailable_samples() {
        let positive = endpoint_observation(Some(1));
        let mut counts = EndpointEncounters::default();
        assert!(!counts.observe(std::slice::from_ref(&positive), 0));
        assert_eq!(counts.actions_observed, 0);
        assert!(!counts.observe(&[], 1));
        assert!(!counts.observe(&[endpoint_observation(None)], 2));
        assert!(!counts.observe(&[positive.clone(), endpoint_observation(Some(0))], 3));
        assert!(counts.observe(std::slice::from_ref(&positive), 4));
        assert!(!counts.observe(std::slice::from_ref(&positive), 5));
        let mut dead = positive;
        dead.dead = true;
        assert!(!counts.observe(&[dead], 6));
        assert_eq!(counts.actions_observed, 6);
        assert_eq!(counts.live_endpoints, 3);
        assert_eq!(counts.classified_endpoints, 2);
        assert_eq!(counts.first.unwrap().execution, 4);
    }

    #[cfg(feature = "metroid-boss-context-audit")]
    #[test]
    fn first_encounter_is_recorded_before_retention_and_only_reconstructed_once() {
        let directory =
            std::env::temp_dir().join(format!("metroid-endpoint-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let action = MetroidCampaignActionResult {
            discard_previous_dead: false,
            action: ButtonChord::new(0, 1),
            observations: vec![endpoint_observation(Some(1))],
            milestones: MetroidMilestones::default(),
            dead: false,
            victory: false,
            failed: false,
            candidate: None, // A rejected endpoint still contributes observed evidence.
        };
        let input = MetroidInput {
            actions: vec![ButtonChord::new(0, 1)],
        };
        for publish in [false, true] {
            let mut game = MetroidGame::new(&[], Path::new("unused"), "unused");
            if publish {
                game =
                    game.with_endpoint_encounter_path(directory.join(ENDPOINT_ENCOUNTER_ARTIFACT));
            }
            let mut evidence = MetroidCampaignEvidence {
                champion_key: action_champion_key(&action.observations),
                ..Default::default()
            };
            // Isolate the encounter from already-known ordinary named progress.
            evidence
                .named_progress
                .observe(&action.observations[0], 1, 99);
            let mut reconstructions = 0;
            game.merge_action_evidence(&mut evidence, &action, 12, || {
                reconstructions += 1;
                Ok(input.clone())
            })
            .unwrap();
            assert_eq!(reconstructions, 1);
            game.merge_action_evidence(&mut evidence, &action, 13, || {
                panic!("the first encounter must not reconstruct twice")
            })
            .unwrap();
            assert_eq!(evidence.endpoint_encounters.first.unwrap().execution, 12);
            assert_eq!(evidence.endpoint_encounters.classified_endpoints, 2);
            assert_eq!(evidence.aggregate, MetroidMilestones::default());
        }
        let artifact: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.join(ENDPOINT_ENCOUNTER_ARTIFACT)).unwrap(),
        )
        .unwrap();
        assert_eq!(artifact["first"]["execution"], 12);
        assert_eq!(artifact["first"]["route_action_end_frame"], 99);
        assert_eq!(artifact["input"], serde_json::to_value(input).unwrap());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
