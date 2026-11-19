// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;
use std::error::Error;
#[cfg(unix)]
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use machine::nes::WRAM_SIZE;
#[cfg(unix)]
use machine::quicknes::QuickNesMachine;

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use machine::consonance::{ConsonanceMachine, ConsonancePortable, identity as consonance_identity};

use crate::{
    chord::{
        CHANGE_ONE_CONTROL_IDENTIFIER, CHORD_DRAW_FIELD, ChordVocabulary,
        NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES,
    },
    nes_backend::{CoverageKind, NesBackend, SnapshotState, code_coverage, coverage_site},
    search::campaign::{
        CampaignActionResult, CampaignCheckpoint, CampaignJobResult, CampaignModeReport,
        CampaignOrigin, CampaignProgressRecord, CampaignStreamRecord, CampaignTypes, Evaluation,
        InputPolicy, Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
        postcard_semantic_result_sha256, postcard_value_sha256,
    },
    search::cost::ShrinkCost,
    search::coverage::CoverageRating,
    search::empirical_steps::EmpiricalStepCheckpoint,
    search::rand::RomuDuoJrRand,
    search::rollout::{ExecutionDisposition, Outcome},
    smb::archive::{
        DOWN_TEN_BUTTON_MASKS, LONG_HOLD_FRAMES, REPLACEMENT_IDENTIFIER, SmbArchiveKey,
        SmbArchiveReport, SmbCoverage, SmbCoverageIds, SmbCoverageTerm, admission_is_viable,
        archive_key, chord_time, key_policy_identifier, merge_action_milestones, merge_milestones,
        merge_progress_watermark, milestone_key, stamp_arrival_room, stamp_arrival_room_identity,
        update_first_inputs,
    },
    smb::plan::{Goal, SmbRoute, vine_step},
    smb::shrink::{remaining_frames_bound, simpler_chords},
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
        smb_mechanical_state_from_wram, smb_player_has_control,
    },
    target::{ExitKind, Target},
};

pub use crate::search::campaign::{
    CampaignAdmissionDecision as SmbCampaignAdmissionDecision,
    CampaignConfig as GenericCampaignConfig, CampaignOriginRecord as SmbCampaignOriginRecord,
    RESUME_IDENTIFIER, TreeImportCounts as SmbTreeImportCounts, derive_selection_seed,
};

pub type SmbCampaignStreamRecord = CampaignStreamRecord<EmpiricalStepCheckpoint>;

pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-quicknes-campaign-stream-v3";

pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-quicknes-snapshot-checkpoint-v4";
pub const CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-consonance-snapshot-checkpoint-v3";

pub const DURATION_IDENTIFIER: &str = "stratified_with_wait_for_control_v1";

pub const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
pub const KEY_POLICY_FIELD: &str = "key_policy";
pub const DURATION_POLICY_FIELD: &str = "duration_policy";
pub const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
pub const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
pub const DOOMED_POLICY_FIELD: &str = "doomed_policy";
pub const DOOMED_POLICY_IDENTIFIER: &str =
    "block_below_ground_line_over_open_floor_without_lifts_v2";
pub const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";

pub fn recorded_policy<'a>(
    policies: &'a WorkloadPolicies,
    field: &str,
) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("campaign stream is missing the {field} policy").into())
}

pub type SmbTargets<M, P> = Box<dyn Fn() -> Result<SmbTarget<M, P>, String> + Send + Sync>;

pub struct SmbGame<M, P = Vec<u8>>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    rom_sha256: String,
    identity: String,
    checkpoint_format: &'static str,
    targets: SmbTargets<M, P>,
    route: SmbRoute,
    coverage: Option<SmbCoverage>,
}

#[must_use]
pub fn quicknes_identity(core: &str, core_sha256: &str) -> String {
    format!(
        "{core};state=ppu-unused2-zero-v1;result_digest=semantic-postcard-1.1.3-sha256-hex-v1;sha256={core_sha256}"
    )
}

impl<M, P> SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    #[must_use]
    pub fn with_targets(
        rom: &[u8],
        identity: String,
        checkpoint_format: &'static str,
        targets: SmbTargets<M, P>,
    ) -> Self {
        Self {
            rom_sha256: format!("{:x}", Sha256::digest(rom)),
            identity: format!("{identity};{}", crate::smb::target::boot_identifier()),
            checkpoint_format,
            targets,
            route: SmbRoute::AllLevels,
            coverage: None,
        }
    }

    #[must_use]
    pub fn with_root(self, root: SmbInput) -> Self
    where
        M: 'static,
        P: 'static,
    {
        if root.actions.is_empty() {
            return self;
        }
        let digest = Sha256::digest(serde_json::to_vec(&root).unwrap_or_default());
        let targets = self.targets;
        let actions = root.actions;
        Self {
            identity: format!("{};genesis=rooted:prefix-sha256={digest:x}", self.identity),
            targets: Box::new(move || {
                let mut target = targets()?;
                target
                    .root_after(&actions)
                    .map_err(|error| error.to_string())?;
                Ok(target)
            }),
            ..self
        }
    }

    #[must_use]
    pub fn with_route(self, route: SmbRoute) -> Self {
        Self { route, ..self }
    }

    #[must_use]
    pub fn route(&self) -> SmbRoute {
        self.route
    }

    #[must_use]
    pub fn with_coverage(self, coverage: SmbCoverage) -> Self
    where
        M: 'static,
        P: 'static,
    {
        let targets = self.targets;
        Self {
            targets: Box::new(move || {
                let mut target = targets()?;
                target
                    .record_coverage()
                    .map_err(|error| error.to_string())?;
                Ok(target)
            }),
            coverage: Some(coverage),
            ..self
        }
    }

    fn key_policy(&self) -> String {
        key_policy_identifier(self.route, self.coverage)
    }

    fn doomed_policy(&self) -> String {
        match self.route {
            SmbRoute::AllLevels => DOOMED_POLICY_IDENTIFIER.to_owned(),
            SmbRoute::WarpsWithoutVine => format!(
                "{DOOMED_POLICY_IDENTIFIER}_or_off_route_{}",
                self.route.identifier()
            ),
            route => format!(
                "{DOOMED_POLICY_IDENTIFIER}_or_off_route_{}_or_vine_brick_left_of_screen_without_the_vine_and_its_area_pointer",
                route.identifier()
            ),
        }
    }

    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn snapshot_checkpoint_format(&self) -> &'static str {
        self.checkpoint_format
    }
}

#[cfg(unix)]
impl SmbGame<QuickNesMachine, Vec<u8>> {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str, execution_sha256: &str) -> Self {
        let core = machine::quicknes::core_identity();
        let target_rom = rom.to_vec();
        let target_core = core_path.to_path_buf();
        let target_sha256 = core_sha256.to_owned();
        let target_execution = execution_sha256.to_owned();
        Self::with_targets(
            rom,
            quicknes_identity(
                &core,
                &crate::nes_backend::native_identity_sha256(core_sha256, execution_sha256),
            ),
            SNAPSHOT_CHECKPOINT_FORMAT,
            Box::new(move || {
                SmbTarget::from_smb_rom_bytes_headless_with_identity(
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
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        let execution_sha256 =
            machine::quicknes::prepared_execution_sha256(&core_path, &core_sha256)?;
        Ok(Self::new(rom, &core_path, &core_sha256, &execution_sha256))
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn loopback_for_tests(rom: &[u8]) -> Self {
        let mut game = Self::new(rom, Path::new(""), &"a".repeat(64), &"b".repeat(64));
        let target_rom = rom.to_vec();
        game.targets = Box::new(move || {
            SmbTarget::loopback_for_tests(&target_rom).map_err(|error| error.to_string())
        });
        game
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl SmbGame<ConsonanceMachine, ConsonancePortable> {
    pub fn new_consonance(
        rom: &[u8],
        kernel: &[u8],
        initramfs: &[u8],
    ) -> Result<Self, machine::MachineError> {
        let target_kernel = kernel.to_vec();
        let target_initramfs = initramfs.to_vec();
        Ok(Self::with_targets(
            rom,
            format!(
                "{};result_digest=semantic-postcard-1.1.3-sha256-hex-v1",
                consonance_identity(kernel, initramfs)?,
            ),
            CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT,
            Box::new(move || {
                let machine = ConsonanceMachine::new(&target_kernel, &target_initramfs)
                    .map_err(|error| error.to_string())?;
                SmbTarget::from_machine(machine).map_err(|error| error.to_string())
            }),
        ))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SmbCampaignRun {
    pub vocabulary: SmbButtonVocabulary,
    pub terminal: Option<SmbTerminalPredicate>,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SmbCampaignEvidence {
    pub aggregate: SmbMilestones,
    pub watermark: SmbProgressWatermark,
    pub first_reached: SmbMilestoneTimes,
    pub first_inputs: SmbMilestoneInputs,
    pub champion_input: SmbInput,
    pub champion_milestones: SmbMilestones,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vine_steps: BTreeMap<u16, SmbVineStepReach>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub vine_step_starts: BTreeMap<u16, SmbVineStepStarts>,
    #[serde(default, skip_serializing_if = "SmbCoverageEvidence::is_empty")]
    pub coverage: SmbCoverageEvidence,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<SmbRevealEvidence>,
    #[serde(skip)]
    pub job_descends_from_reveal: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmbVineStepReach {
    pub first_job: u64,
    pub actions: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rating: Option<SmbCoverageRating>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmbCoverageRating {
    pub rarity: u8,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<u32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmbRevealEvidence {
    pub discovery: u32,
    pub id: u64,
    pub cell_jobs: u64,
    pub lineage: u32,
    pub descendant_jobs: u64,
    pub descendant_furthest_step: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmbVineStepStarts {
    pub first_job: u64,
    pub jobs: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SmbCoverageEvidence {
    pub action_ids_by_kind: [u64; 3],
    pub capped_by_kind: [u64; 3],
    pub capped_from_f400: u64,
    #[serde(default)]
    pub discovered_by_kind: [u64; 3],
    #[serde(default)]
    pub discovered_from_f400: u64,
}

impl SmbCoverageEvidence {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[must_use]
pub fn smb_cpu_address(site: u32) -> u16 {
    let address = if site & 0x10_0000 == 0 {
        0x8000 | (site & 0x7FFF)
    } else {
        site & 0xFFFF
    };
    u16::try_from(address).unwrap_or(0)
}

fn coverage_id_address(id: u64) -> String {
    format!("${:04X}", smb_cpu_address(coverage_site(id)))
}

fn coverage_rating_json(rating: SmbCoverageRating) -> serde_json::Value {
    serde_json::json!({
        "rarity": rating.rarity,
        "id": rating.id,
        "kind": CoverageKind::of(rating.id).identifier(),
        "address": coverage_id_address(rating.id),
        "discovery": rating.discovery,
    })
}

fn count_coverage_id(by_kind: &mut [u64; 3], from_f400: &mut u64, id: u64) {
    let kind = &mut by_kind[CoverageKind::of(id).index()];
    *kind = kind.saturating_add(1);
    if smb_cpu_address(coverage_site(id)) >= 0xF400 {
        *from_f400 = from_f400.saturating_add(1);
    }
}

fn kinds_json(counts: [u64; 3]) -> serde_json::Value {
    serde_json::json!({
        CoverageKind::Branch.identifier(): counts[CoverageKind::Branch.index()],
        CoverageKind::Jump.identifier(): counts[CoverageKind::Jump.index()],
        CoverageKind::Data.identifier(): counts[CoverageKind::Data.index()],
    })
}

fn action_vine_step<M, P>(action: &SmbCampaignActionResult<M, P>) -> u16
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    action
        .observations
        .iter()
        .filter_map(|observation| <&[u8; WRAM_SIZE]>::try_from(observation.wram.as_slice()).ok())
        .map(vine_step)
        .max()
        .unwrap_or(0)
}

pub type SmbCampaignOrigin<M, P = Vec<u8>> = CampaignOrigin<SmbGame<M, P>>;
pub type SmbCampaignCheckpoint<P = Vec<u8>> = CampaignCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpoint<P = Vec<u8>> = SnapshotCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpointEntry<P = Vec<u8>> =
    crate::search::campaign::SnapshotCheckpointEntry<SmbSnapshot<P>>;
pub type SmbCampaignModeReport = CampaignModeReport<ButtonChord, SmbArchiveReport>;
pub type SmbCampaignProgressRecord = CampaignProgressRecord<SmbArchiveKey>;
pub type SmbCampaignActionResult<M, P = Vec<u8>> = CampaignActionResult<SmbGame<M, P>>;

#[must_use]
pub fn route_stage(route: SmbRoute, wram: &[u8; WRAM_SIZE]) -> u8 {
    route.locate(&archive_key(wram).plan_state()).0
}

fn terminal_stage(route: SmbRoute, terminal: Option<SmbTerminalPredicate>) -> usize {
    let plan = route.plan();
    let last = plan.len().saturating_sub(1);
    let first = |world: u8, level: u8| {
        plan.iter()
            .position(|stage| (stage.world, stage.level) == (world, level))
    };
    match terminal {
        None | Some(SmbTerminalPredicate::GameVictory) => plan
            .iter()
            .position(|stage| stage.goal == Goal::Ending)
            .unwrap_or(plan.len()),
        Some(SmbTerminalPredicate::ControlIn { world, level }) => {
            first(world, level).unwrap_or(last)
        }
        Some(SmbTerminalPredicate::LevelTransition { world, level }) => plan
            .iter()
            .rposition(|stage| (stage.world, stage.level) == (world, level))
            .map_or(last, |index| index + 1),
        Some(SmbTerminalPredicate::Stage { stage, .. }) => usize::from(stage),
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SmbTerminalPredicate {
    #[default]
    GameVictory,
    LevelTransition {
        world: u8,
        level: u8,
    },
    ControlIn {
        world: u8,
        level: u8,
    },
    Stage {
        route: SmbRoute,
        stage: u8,
    },
}

impl SmbTerminalPredicate {
    #[must_use]
    pub fn identifier(self) -> String {
        match self {
            Self::GameVictory => "game_victory".to_owned(),
            Self::LevelTransition { world, level } => {
                format!("level_transition:{world},{level}")
            }
            Self::ControlIn { world, level } => format!("control_in:{world},{level}"),
            Self::Stage { route, stage } => format!("stage:{}:{stage}", route.identifier()),
        }
    }

    pub fn from_identifier(identifier: &str) -> Result<Self, Box<dyn Error>> {
        if identifier == "game_victory" {
            return Ok(Self::GameVictory);
        }
        if let Some(fields) = identifier.strip_prefix("stage:") {
            let (route, stage) = fields
                .rsplit_once(':')
                .ok_or("stage predicate is missing its route or stage")?;
            return Ok(Self::Stage {
                route: SmbRoute::from_identifier(route)?,
                stage: stage.parse()?,
            });
        }
        let (control, fields) = if let Some(fields) = identifier.strip_prefix("level_transition:") {
            (false, fields)
        } else if let Some(fields) = identifier.strip_prefix("control_in:") {
            (true, fields)
        } else {
            return Err("SMB terminal predicate is not recognized".into());
        };
        let (world, level) = fields
            .split_once(',')
            .ok_or("level predicate is missing world or level")?;
        if level.contains(',') {
            return Err("level predicate has extra fields".into());
        }
        let (world, level) = (world.parse()?, level.parse()?);
        Ok(if control {
            Self::ControlIn { world, level }
        } else {
            Self::LevelTransition { world, level }
        })
    }

    pub fn reached<M, P>(self, target: &SmbTarget<M, P>) -> Result<bool, Box<dyn Error>>
    where
        M: NesBackend<P>,
        P: SnapshotState,
    {
        if target.exit_kind() != ExitKind::Ok {
            return Err("SMB terminal predicate cannot inspect a failed emulator".into());
        }
        if target.is_victory() {
            return Ok(true);
        }
        Ok(match self {
            Self::GameVictory => false,
            Self::LevelTransition { world, level } => {
                let state = target.mechanical_state();
                (state.world, state.level) != (world, level)
            }
            Self::ControlIn { world, level } => {
                let state = target.mechanical_state();
                (state.world, state.level) == (world, level)
                    && smb_player_has_control(&target.wram())
            }
            Self::Stage { route, stage } => {
                let wram = target.wram();
                route_stage(route, &wram) >= stage
                    && !route.is_doomed(&wram)
                    && smb_player_has_control(&wram)
            }
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbButtonVocabulary {
    DownTenMask,
    NesDownTen,
    NesRunThirteen,
    #[default]
    NesPressable,
}

impl SmbButtonVocabulary {
    #[must_use]
    pub fn masks(self) -> &'static [u8] {
        match self {
            Self::DownTenMask => &DOWN_TEN_BUTTON_MASKS,
            Self::NesDownTen => &crate::smb::archive::NES_DOWN_TEN_BUTTON_MASKS,
            Self::NesRunThirteen => &crate::smb::archive::NES_RUN_THIRTEEN_BUTTON_MASKS,
            Self::NesPressable => &NES_PRESSABLE_BUTTON_MASKS,
        }
    }

    #[must_use]
    pub fn chords(self) -> ChordVocabulary {
        ChordVocabulary {
            held: self.masks(),
            tap: None,
            short_hold: SHORT_HOLD_FRAMES,
            long_hold: LONG_HOLD_FRAMES,
        }
    }
}

#[must_use]
pub fn button_vocabulary_identifier(vocabulary: SmbButtonVocabulary) -> &'static str {
    match vocabulary {
        SmbButtonVocabulary::DownTenMask => "down_ten_mask",
        SmbButtonVocabulary::NesDownTen => "nes_down_ten",
        SmbButtonVocabulary::NesRunThirteen => "nes_run_thirteen",
        SmbButtonVocabulary::NesPressable => "nes_pressable_36",
    }
}

pub fn button_vocabulary_from_identifier(
    identifier: &str,
) -> Result<SmbButtonVocabulary, Box<dyn Error>> {
    match identifier {
        "down_ten_mask" => Ok(SmbButtonVocabulary::DownTenMask),
        "nes_down_ten" => Ok(SmbButtonVocabulary::NesDownTen),
        "nes_run_thirteen" => Ok(SmbButtonVocabulary::NesRunThirteen),
        "nes_pressable_36" => Ok(SmbButtonVocabulary::NesPressable),
        _ => Err("campaign stream controller vocabulary is not recognized".into()),
    }
}

pub fn select_frontier_resume_input(source: &SmbArchiveReport) -> Result<SmbInput, Box<dyn Error>> {
    let frontier = source
        .entries
        .iter()
        .map(|entry| (entry.key.world, entry.key.level, entry.key.progress))
        .max()
        .ok_or("source archive contains no retained entries")?;
    source
        .entries
        .iter()
        .filter(|entry| (entry.key.world, entry.key.level, entry.key.progress) == frontier)
        .min_by_key(|entry| (entry.input.actions.len(), entry.id))
        .map(|entry| entry.input.clone())
        .ok_or_else(|| "source archive contains no frontier entries".into())
}

impl<M, P> CampaignTypes for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    type JobObservation = ();
    type Target = SmbTarget<M, P>;
    type Action = ButtonChord;
    type Key = SmbArchiveKey;
    type Milestones = SmbMilestones;
    type Progress = SmbProgressWatermark;
    type Snapshot = SmbSnapshot<P>;
    type Observations = SmbObservations;
    type Evidence = SmbCampaignEvidence;
    type ArchiveReport = SmbArchiveReport;
    type Run = SmbCampaignRun;
}

impl<M, P> Reporting for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn diagnostics(evidence: &Self::Evidence) -> Option<serde_json::Value> {
        let mut diagnostics = serde_json::Map::new();
        if !evidence.vine_steps.is_empty() {
            diagnostics.insert(
                "vine_steps".to_owned(),
                evidence
                    .vine_steps
                    .iter()
                    .map(|(step, reach)| {
                        let mut value = serde_json::json!({
                            "first_job": reach.first_job,
                            "actions": reach.actions,
                        });
                        if let Some(rating) = reach.rating {
                            value["rating"] = coverage_rating_json(rating);
                        }
                        (step.to_string(), value)
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into(),
            );
        }
        if !evidence.vine_step_starts.is_empty() {
            diagnostics.insert(
                "vine_step_starts".to_owned(),
                evidence
                    .vine_step_starts
                    .iter()
                    .map(|(step, starts)| {
                        (
                            step.to_string(),
                            serde_json::json!({
                                "first_job": starts.first_job,
                                "jobs": starts.jobs,
                            }),
                        )
                    })
                    .collect::<serde_json::Map<_, _>>()
                    .into(),
            );
        }
        if !evidence.coverage.is_empty() {
            diagnostics.insert(
                "coverage".to_owned(),
                serde_json::json!({
                    "action_ids_by_kind": kinds_json(evidence.coverage.action_ids_by_kind),
                    "capped_by_kind": kinds_json(evidence.coverage.capped_by_kind),
                    "capped_from_f400": evidence.coverage.capped_from_f400,
                    "discovered_by_kind": kinds_json(evidence.coverage.discovered_by_kind),
                    "discovered_from_f400": evidence.coverage.discovered_from_f400,
                }),
            );
        }
        if let Some(reveal) = evidence.reveal {
            diagnostics.insert(
                "reveal".to_owned(),
                serde_json::json!({
                    "discovery": reveal.discovery,
                    "id": reveal.id,
                    "kind": CoverageKind::of(reveal.id).identifier(),
                    "address": coverage_id_address(reveal.id),
                    "cell_jobs": reveal.cell_jobs,
                    "lineage": reveal.lineage,
                    "descendant_jobs": reveal.descendant_jobs,
                    "descendant_furthest_step": reveal.descendant_furthest_step,
                }),
            );
        }
        (!diagnostics.is_empty()).then(|| diagnostics.into())
    }

    fn write_evidence_checkpoint(
        evidence: &Self::Evidence,
        out: &mut dyn std::io::Write,
    ) -> Result<(), Box<dyn Error>> {
        serde_json::to_writer(out, evidence)?;
        Ok(())
    }
    fn evidence_from_checkpoint(bytes: &[u8]) -> Result<Self::Evidence, Box<dyn Error>> {
        Ok(serde_json::from_slice(bytes)?)
    }
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }
    fn checkpoint_format(&self) -> &'static str {
        self.snapshot_checkpoint_format()
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
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        let semantic = postcard_semantic_result_sha256(result)?;
        if self.coverage.is_none() {
            return Ok(semantic);
        }
        let coverage: Vec<&[u64]> = result
            .actions
            .iter()
            .map(|action| self.action_coverage(action))
            .collect();
        postcard_value_sha256(&(semantic, coverage))
    }

    fn snapshot_identity_sha256(
        &self,
        snapshot: &Self::Snapshot,
    ) -> Result<String, Box<dyn Error>> {
        snapshot.snapshot_identity_sha256()
    }

    fn archive_report(
        &self,
        evidence: &SmbCampaignEvidence,
        state: crate::search::campaign::ArchiveReportState<Self>,
    ) -> SmbArchiveReport {
        SmbArchiveReport {
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

impl<M, P> InputPolicy for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn simpler_actions(&self, _run: &SmbCampaignRun, action: &ButtonChord) -> Vec<ButtonChord> {
        simpler_chords(action)
    }
    fn policies(&self, run: &SmbCampaignRun) -> WorkloadPolicies {
        let mut policies: WorkloadPolicies = [
            (
                CONTROLLER_VOCABULARY_FIELD,
                button_vocabulary_identifier(run.vocabulary).to_owned(),
            ),
            (KEY_POLICY_FIELD, self.key_policy()),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER.to_owned()),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER.to_owned()),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER.to_owned()),
            (DOOMED_POLICY_FIELD, self.doomed_policy()),
        ]
        .into_iter()
        .map(|(field, value)| (field.to_owned(), value))
        .collect();
        if let Some(terminal) = run.terminal {
            policies.insert(TERMINAL_POLICY_FIELD.to_owned(), terminal.identifier());
        }
        policies.insert(EMULATOR_BACKEND_FIELD.to_owned(), self.identity.clone());
        policies
    }

    fn resolve_recorded(
        &self,
        policies: &WorkloadPolicies,
    ) -> Result<SmbCampaignRun, Box<dyn Error>> {
        let recorded = |field: &str| recorded_policy(policies, field);
        let pinned = [
            (KEY_POLICY_FIELD, self.key_policy()),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER.to_owned()),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER.to_owned()),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER.to_owned()),
            (DOOMED_POLICY_FIELD, self.doomed_policy()),
        ];
        for (field, compiled) in pinned {
            if recorded(field)? != compiled {
                return Err(format!("campaign stream {field} is not recognized").into());
            }
        }
        if recorded(EMULATOR_BACKEND_FIELD)? != self.identity {
            return Err("campaign stream emulator_backend is not this SMB backend".into());
        }
        let run = SmbCampaignRun {
            vocabulary: button_vocabulary_from_identifier(recorded(CONTROLLER_VOCABULARY_FIELD)?)?,
            terminal: policies
                .get(TERMINAL_POLICY_FIELD)
                .map(|identifier| SmbTerminalPredicate::from_identifier(identifier))
                .transpose()?,
        };
        let unknown = policies
            .keys()
            .find(|field| !self.policies(&run).contains_key(field.as_str()));
        if let Some(field) = unknown {
            return Err(format!("campaign stream carries an unknown {field} policy").into());
        }
        Ok(run)
    }

    fn sample_alphabet(
        &self,
        run: &SmbCampaignRun,
        previous: Option<&ButtonChord>,
        rand: &mut RomuDuoJrRand,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        run.vocabulary.chords().draw(rand, previous)
    }
}

impl<M, P> TargetExecution for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn new_target(&self) -> Result<SmbTarget<M, P>, String> {
        (self.targets)()
    }
    fn reset(&self, target: &mut SmbTarget<M, P>) {
        target.reset();
    }
    fn restore(
        &self,
        target: &mut SmbTarget<M, P>,
        snapshot: &SmbSnapshot<P>,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }
    fn execution_work(&self, target: &SmbTarget<M, P>) -> u64 {
        target.execution_work()
    }
    fn apply_action(
        &self,
        target: &mut SmbTarget<M, P>,
        action: &ButtonChord,
        milestones: &mut SmbMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(milestones, target)
    }
    fn rollout_observations(&self, target: &SmbTarget<M, P>) -> Vec<SmbObservations> {
        if target.exit_kind() != ExitKind::Ok {
            Vec::new()
        } else {
            target.last_action_observations().to_vec()
        }
    }
    fn rollout_probe(
        &self,
        _run: &SmbCampaignRun,
        target: &mut SmbTarget<M, P>,
        snapshot: &SmbSnapshot<P>,
    ) -> Result<bool, Box<dyn Error>> {
        admission_is_viable(target, snapshot)
    }
    fn snapshot(&self, target: &mut SmbTarget<M, P>) -> Result<SmbSnapshot<P>, Box<dyn Error>> {
        target.snapshot().ok_or_else(|| "failed to snapshot".into())
    }

    fn action_cost_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &SmbSnapshot<P>) -> usize {
        snapshot.resident_memory_charge()
    }
}

impl<M, P> Evaluation for SmbGame<M, P>
where
    M: NesBackend<P>,
    P: SnapshotState,
{
    fn shrink_cost(&self, _action: &ButtonChord, target: &SmbTarget<M, P>) -> ShrinkCost {
        ShrinkCost::new(
            target.last_action_frames(),
            target.last_action_level_frames(),
        )
    }
    fn remaining_cost_bound(&self, run: &SmbCampaignRun, target: &SmbTarget<M, P>) -> ShrinkCost {
        if let Some(SmbTerminalPredicate::Stage { route, stage }) = run.terminal {
            let state = target.mechanical_state();
            let here = (state.world, state.level);
            if route
                .plan()
                .get(usize::from(stage))
                .is_none_or(|goal| (goal.world, goal.level) == here)
            {
                return ShrinkCost::ZERO;
            }
        }
        let frames = remaining_frames_bound(target);
        ShrinkCost::new(frames, frames)
    }
    fn stage(&self, _run: &SmbCampaignRun, target: &SmbTarget<M, P>) -> Option<u32> {
        Some(u32::from(route_stage(self.route, &target.wram())))
    }
    fn stage_run(&self, run: &SmbCampaignRun, stage: u32) -> Option<SmbCampaignRun> {
        let stage = u8::try_from(stage).ok()?;
        (usize::from(stage) < terminal_stage(self.route, run.terminal)).then_some(SmbCampaignRun {
            terminal: Some(SmbTerminalPredicate::Stage {
                route: self.route,
                stage,
            }),
            ..*run
        })
    }
    fn merge_milestones(&self, into: &mut SmbMilestones, from: SmbMilestones) {
        merge_milestones(into, from);
    }
    fn aggregate_milestones(evidence: &SmbCampaignEvidence) -> SmbMilestones {
        evidence.aggregate
    }
    fn aggregate_progress(evidence: &SmbCampaignEvidence) -> SmbProgressWatermark {
        evidence.watermark
    }
    fn merge_origin_evidence(&self, evidence: &mut SmbCampaignEvidence, source: &SmbArchiveReport) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }
    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut SmbCampaignEvidence,
        target: &SmbTarget<M, P>,
    ) -> Result<(), Box<dyn Error>> {
        let state = smb_mechanical_state_from_wram(&target.wram());
        evidence.watermark = evidence.watermark.max(SmbProgressWatermark {
            world: state.world,
            level: state.level,
            progress: state.progress,
        });
        Ok(())
    }
    fn merge_import_evidence(
        &self,
        evidence: &mut SmbCampaignEvidence,
        milestones: SmbMilestones,
        input: &SmbInput,
    ) {
        merge_milestones(&mut evidence.aggregate, milestones);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            milestones,
            0,
            input,
        );
        if milestone_key(milestones) > milestone_key(evidence.champion_milestones) {
            evidence.champion_milestones = milestones;
            evidence.champion_input = input.clone();
        }
    }
    fn merge_action_evidence<F>(
        &self,
        evidence: &mut SmbCampaignEvidence,
        action: &SmbCampaignActionResult<M, P>,
        sequence: u64,
        input: F,
    ) -> Result<(), Box<dyn Error>>
    where
        F: FnOnce() -> Result<SmbInput, Box<dyn Error>>,
    {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        let step = action_vine_step(action);
        if step > 0 {
            let reach = evidence.vine_steps.entry(step).or_insert(SmbVineStepReach {
                first_job: sequence,
                ..SmbVineStepReach::default()
            });
            reach.actions = reach.actions.saturating_add(1);
        }
        if evidence.job_descends_from_reveal
            && let Some(reveal) = evidence.reveal.as_mut()
        {
            reveal.descendant_furthest_step = reveal.descendant_furthest_step.max(step);
        }
        if let Some(endpoint) = action.observations.last() {
            let ids = endpoint.coverage.as_slice();
            let mut start = 0;
            for kind in [CoverageKind::Branch, CoverageKind::Jump, CoverageKind::Data] {
                let end = start + ids[start..].partition_point(|&id| CoverageKind::of(id) == kind);
                let count = &mut evidence.coverage.action_ids_by_kind[kind.index()];
                *count = count.saturating_add(u64::try_from(end - start).unwrap_or(u64::MAX));
                start = end;
            }
        }
        let first_input_needed = (action.milestones.max_1_1_scroll_bucket > 0
            && evidence.first_inputs.progress_into_1_1.is_none())
            || (action.milestones.reached_1_1_flag && evidence.first_inputs.flag_1_1.is_none())
            || (action.milestones.reached_1_2 && evidence.first_inputs.level_1_2.is_none())
            || (action.milestones.reached_onward && evidence.first_inputs.onward.is_none());
        let champion =
            milestone_key(action.milestones) > milestone_key(evidence.champion_milestones);
        if first_input_needed || champion {
            let input = input()?;
            update_first_inputs(
                &mut evidence.first_reached,
                &mut evidence.first_inputs,
                action.milestones,
                sequence,
                &input,
            );
            if champion {
                evidence.champion_milestones = action.milestones;
                evidence.champion_input = input;
            }
        }
        Ok(())
    }
    fn source_entries<'a>(
        &self,
        source: &'a SmbArchiveReport,
    ) -> &'a [crate::smb::archive::SmbArchiveEntryReport] {
        &source.entries
    }
    fn resume_input(&self, source: &SmbArchiveReport) -> Result<SmbInput, Box<dyn Error>> {
        select_frontier_resume_input(source)
    }

    fn execution_disposition(&self, target: &SmbTarget<M, P>) -> ExecutionDisposition {
        if target.exit_kind() != ExitKind::Ok {
            ExecutionDisposition::Failed
        } else if target.is_dead() || target.is_victory() {
            ExecutionDisposition::Terminal
        } else {
            ExecutionDisposition::Runnable
        }
    }

    fn objective_reached(
        &self,
        run: &SmbCampaignRun,
        target: &SmbTarget<M, P>,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok {
            return Ok(false);
        }
        run.terminal.unwrap_or_default().reached(target)
    }

    fn rollout_outcome(
        &self,
        run: &SmbCampaignRun,
        target: &SmbTarget<M, P>,
    ) -> Result<Outcome, Box<dyn Error>> {
        let objective_reached = self.objective_reached(run, target)?;
        Ok(Outcome {
            doomed: false,
            objective_reached,
            disposition: if target.exit_kind() != ExitKind::Ok {
                ExecutionDisposition::Failed
            } else if target.is_dead() || objective_reached {
                ExecutionDisposition::Terminal
            } else {
                ExecutionDisposition::Runnable
            },
        })
    }

    fn is_doomed(&self, target: &SmbTarget<M, P>) -> bool {
        target.is_doomed() || (!target.is_dead() && self.route.is_doomed(&target.wram()))
    }

    fn current_key(&self, target: &SmbTarget<M, P>) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room(
            archive_key(&target.wram()).on_route(self.route),
            &target.wram(),
        )
    }

    fn rollout_key(&self, target: &SmbTarget<M, P>) -> Result<SmbArchiveKey, Box<dyn Error>> {
        Ok(archive_key(&target.wram()).on_route(self.route))
    }

    fn complete_candidate_key(
        &self,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot<P>,
    ) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room_identity(key, snapshot.room_area())
    }

    fn action_coverage<'a>(&self, action: &'a CampaignActionResult<Self>) -> &'a [u64] {
        let Some(coverage) = self.coverage else {
            return &[];
        };
        let ids = action
            .observations
            .last()
            .map_or(&[][..], |endpoint| endpoint.coverage.as_slice());
        match coverage.ids {
            SmbCoverageIds::All => ids,
            SmbCoverageIds::Code => code_coverage(ids),
        }
    }

    fn rate_coverage(&self, key: SmbArchiveKey, rating: CoverageRating) -> SmbArchiveKey {
        match self.coverage.map(|coverage| coverage.term) {
            Some(SmbCoverageTerm::Rarity { cap }) => SmbArchiveKey {
                coverage: u32::from(rating.rarity.min(cap)),
                ..key
            },
            Some(SmbCoverageTerm::NewId) => {
                let discovery = rating.discovery.unwrap_or(0);
                SmbArchiveKey {
                    coverage: discovery,
                    vine_discovery: if key.vine >= 2 { discovery } else { 0 },
                    ..key
                }
            }
            None => key,
        }
    }

    fn merge_coverage_evidence(
        &self,
        evidence: &mut SmbCampaignEvidence,
        action: &SmbCampaignActionResult<M, P>,
        sequence: u64,
        rating: CoverageRating,
    ) {
        let step = action_vine_step(action);
        if let Some(reach) = evidence.vine_steps.get_mut(&step)
            && reach.first_job == sequence
            && reach.rating.is_none()
        {
            reach.rating = Some(SmbCoverageRating {
                rarity: rating.rarity,
                id: rating.id,
                discovery: rating.discovery,
            });
            if step == 2
                && evidence.reveal.is_none()
                && action.candidate.is_some()
                && let Some(discovery) = rating.discovery
                && matches!(
                    self.coverage.map(|coverage| coverage.term),
                    Some(SmbCoverageTerm::NewId)
                )
            {
                evidence.reveal = Some(SmbRevealEvidence {
                    discovery,
                    id: rating.id,
                    ..SmbRevealEvidence::default()
                });
            }
        }
        let Some(coverage) = self.coverage else {
            return;
        };
        if action.candidate.is_none() {
            return;
        }
        if let SmbCoverageTerm::Rarity { cap } = coverage.term
            && rating.rarity >= cap
        {
            count_coverage_id(
                &mut evidence.coverage.capped_by_kind,
                &mut evidence.coverage.capped_from_f400,
                rating.id,
            );
        }
        if rating.discovery.is_some() {
            count_coverage_id(
                &mut evidence.coverage.discovered_by_kind,
                &mut evidence.coverage.discovered_from_f400,
                rating.id,
            );
        }
    }

    fn merge_job_start_evidence(
        &self,
        evidence: &mut SmbCampaignEvidence,
        parent: &SmbArchiveKey,
        sequence: u64,
    ) {
        evidence.job_descends_from_reveal = false;
        if let Some(reveal) = evidence.reveal.as_mut() {
            if parent.coverage == reveal.discovery {
                reveal.cell_jobs = reveal.cell_jobs.saturating_add(1);
                if reveal.lineage == 0 {
                    reveal.lineage = parent.vine_discovery;
                }
            }
            if reveal.lineage != 0 && parent.vine_discovery == reveal.lineage {
                reveal.descendant_jobs = reveal.descendant_jobs.saturating_add(1);
                evidence.job_descends_from_reveal = true;
            }
        }
        if parent.vine == 0 {
            return;
        }
        let starts = evidence
            .vine_step_starts
            .entry(parent.vine)
            .or_insert(SmbVineStepStarts {
                first_job: sequence,
                jobs: 0,
            });
        starts.jobs = starts.jobs.saturating_add(1);
    }
}
