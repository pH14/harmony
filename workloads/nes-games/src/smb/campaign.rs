// SPDX-License-Identifier: AGPL-3.0-or-later

use std::error::Error;
#[cfg(unix)]
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    nes_backend::{NesBackend, SnapshotState},
    search::campaign::{
        CampaignActionResult, CampaignCheckpoint, CampaignJobRecord, CampaignJobResult,
        CampaignModeReport, CampaignOrigin, CampaignProgressRecord, CampaignSkipRecord,
        CampaignStreamHeader, CampaignStreamRecord, CampaignTypes, Evaluation, InputPolicy,
        Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
        postcard_semantic_result_sha256,
    },
    search::draw_tables::DrawTableHeader,
    search::empirical_steps::EmpiricalStepCheckpoint,
    search::rand::RomuDuoJrRand,
    search::rollout::{ExecutionDisposition, Outcome},
    smb::archive::{
        DOWN_TEN_BUTTON_MASKS, KEY_POLICY_IDENTIFIER, LONG_HOLD_FRAMES, REPLACEMENT_IDENTIFIER,
        SmbArchiveKey, SmbArchiveReport, admission_is_viable, archive_key, chord_time,
        merge_action_milestones, merge_milestones, merge_progress_watermark, milestone_key,
        stamp_arrival_room, stamp_arrival_room_identity, update_first_inputs,
    },
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
        smb_mechanical_state_from_wram,
    },
    target::{ExitKind, Target},
};

pub use crate::search::campaign::{
    CampaignAdmissionDecision as SmbCampaignAdmissionDecision,
    CampaignConfig as GenericCampaignConfig, CampaignOriginRecord as SmbCampaignOriginRecord,
    RESUME_IDENTIFIER, TreeImportCounts as SmbTreeImportCounts, derive_selection_seed,
};

pub type SmbCampaignJobRecord = CampaignJobRecord<EmpiricalStepCheckpoint>;
pub type SmbCampaignSkipRecord = CampaignSkipRecord<EmpiricalStepCheckpoint>;
pub type SmbCampaignStreamRecord = CampaignStreamRecord<EmpiricalStepCheckpoint>;

pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-quicknes-campaign-stream-v3";

pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-quicknes-snapshot-checkpoint-v4";
pub const CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-consonance-snapshot-checkpoint-v3";

pub const DURATION_IDENTIFIER: &str = "selected_symbols_independent_uniform_holds_v1";

pub const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
pub const KEY_POLICY_FIELD: &str = "key_policy";
pub const DURATION_POLICY_FIELD: &str = "duration_policy";
pub const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
pub const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
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
            identity,
            checkpoint_format,
            targets,
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
}

pub type SmbCampaignOrigin<M, P = Vec<u8>> = CampaignOrigin<SmbGame<M, P>>;
pub type SmbCampaignCheckpoint<P = Vec<u8>> = CampaignCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpoint<P = Vec<u8>> = SnapshotCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpointEntry<P = Vec<u8>> =
    crate::search::campaign::SnapshotCheckpointEntry<SmbSnapshot<P>>;
pub type SmbCampaignStreamHeader = CampaignStreamHeader<DrawTableHeader>;
pub type SmbCampaignModeReport = CampaignModeReport<ButtonChord, SmbArchiveReport>;
pub type SmbCampaignProgressRecord = CampaignProgressRecord<SmbArchiveKey>;
pub type SmbCampaignActionResult<M, P = Vec<u8>> = CampaignActionResult<SmbGame<M, P>>;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SmbTerminalPredicate {
    #[default]
    GameVictory,
    LevelTransition {
        world: u8,
        level: u8,
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
        }
    }

    pub fn from_identifier(identifier: &str) -> Result<Self, Box<dyn Error>> {
        if identifier == "game_victory" {
            return Ok(Self::GameVictory);
        }
        let fields = identifier
            .strip_prefix("level_transition:")
            .ok_or("SMB terminal predicate is not recognized")?;
        let (world, level) = fields
            .split_once(',')
            .ok_or("level-transition predicate is missing world or level")?;
        if level.contains(',') {
            return Err("level-transition predicate has extra fields".into());
        }
        Ok(Self::LevelTransition {
            world: world.parse()?,
            level: level.parse()?,
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
        postcard_semantic_result_sha256(result)
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
    fn policies(&self, run: &SmbCampaignRun) -> WorkloadPolicies {
        let mut policies: WorkloadPolicies = [
            (
                CONTROLLER_VOCABULARY_FIELD,
                button_vocabulary_identifier(run.vocabulary).to_owned(),
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER.to_owned()),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER.to_owned()),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER.to_owned()),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER.to_owned()),
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
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER),
            (REPLACEMENT_POLICY_FIELD, REPLACEMENT_IDENTIFIER),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER),
            (CHORD_DRAW_FIELD, CHANGE_ONE_CONTROL_IDENTIFIER),
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

    fn refresh_selected_input_timing(
        &self,
        run: &SmbCampaignRun,
        mutation_seed: u64,
        suffix: &mut [ButtonChord],
    ) -> Result<(), Box<dyn Error>> {
        let mut rand = RomuDuoJrRand::with_seed(mutation_seed ^ u64::from_le_bytes(*b"HOLDTIME"));
        let vocabulary = run.vocabulary.chords();
        for action in suffix {
            action.hold_frames = vocabulary.uniform_hold(&mut rand)?;
        }
        Ok(())
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

    fn current_key(&self, target: &SmbTarget<M, P>) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room(archive_key(&target.wram()), &target.wram())
    }

    fn rollout_key(&self, target: &SmbTarget<M, P>) -> Result<SmbArchiveKey, Box<dyn Error>> {
        Ok(archive_key(&target.wram()))
    }

    fn complete_candidate_key(
        &self,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot<P>,
    ) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room_identity(key, snapshot.room_area())
    }
}
