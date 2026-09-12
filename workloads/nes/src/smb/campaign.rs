// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    marker::PhantomData,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::target::ExitKind;

use crate::{
    nes_backend::{NesBackend, SnapshotState},
    search::archive::RetentionPolicy,
    search::campaign::{
        CampaignActionResult, CampaignCheckpoint, CampaignJobResult, CampaignModeReport,
        CampaignOrigin, CampaignProgressRecord, CampaignStreamHeader, CampaignTypes, Evaluation,
        InputPolicy, Reporting, SnapshotCheckpoint, TargetExecution, WorkloadPolicies,
        postcard_result_sha256, replay_campaign_checkpointed, run_campaign_checkpointed,
    },
    search::draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    search::empirical_steps::{
        EmpiricalStepCheckpoint, EmpiricalStepParameters, EmpiricalStepTableRef,
        EmpiricalStepTables,
    },
    smb::archive::{
        DOWN_TEN_BUTTON_MASKS, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER, SmbArchiveKey,
        SmbArchiveReport, admission_is_viable, archive_key, chord_time, merge_action_milestones,
        merge_milestones, merge_progress_watermark, milestone_key, stamp_arrival_room,
        stamp_arrival_room_identity, update_first_inputs,
    },
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
        smb_mechanical_state_from_wram,
    },
    target::Target,
};

use machine::{Machine, quicknes::QuickNesMachine};

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
use machine::consonance::{ConsonanceMachine, ConsonancePortable, identity as consonance_identity};

pub use crate::search::campaign::{
    CampaignAdmissionDecision as SmbCampaignAdmissionDecision,
    CampaignConfig as GenericCampaignConfig, CampaignJobRecord as SmbCampaignJobRecord,
    CampaignOriginRecord as SmbCampaignOriginRecord, CampaignSkipRecord as SmbCampaignSkipRecord,
    CampaignStreamRecord as SmbCampaignStreamRecord, RESUME_IDENTIFIER,
    TreeImportCounts as SmbTreeImportCounts, derive_worker_seed,
};

pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-quicknes-campaign-stream-v2";

pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-quicknes-snapshot-checkpoint-v3";
pub const CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-consonance-snapshot-checkpoint-v1";

const DRAW_STATE_MEMORY_RESERVE_BYTES: usize = 2 * 1024 * 1024;

pub const DURATION_IDENTIFIER: &str = "stratified";

pub const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
pub const KEY_POLICY_FIELD: &str = "key_policy";
pub const DURATION_POLICY_FIELD: &str = "duration_policy";
pub const CHORD_POLICY_FIELD: &str = "chord_policy";
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

pub struct SmbGame<M = QuickNesMachine, P = Vec<u8>> {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    identity: String,
    backend: SmbBackend,
    machine: PhantomData<fn() -> (M, P)>,
    #[cfg(test)]
    loopback: bool,
}

enum SmbBackend {
    QuickNes,
    #[cfg(all(
        feature = "consonance",
        target_os = "linux",
        any(target_arch = "x86_64", target_arch = "aarch64"),
        not(miri)
    ))]
    Consonance {
        kernel: Vec<u8>,
        initramfs: Vec<u8>,
    },
}

fn quicknes_identity(core_sha256: &str) -> String {
    format!(
        "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;result_digest=postcard-1.1.3-sha256-hex-v2;sha256={core_sha256}",
        machine::quicknes::QUICKNES_REVISION,
        machine::quicknes::QUICKNES_BUILD,
        machine::quicknes::QUICKNES_OPTIONS,
    )
}

#[doc(hidden)]
pub trait SmbMachineKind<P>: Machine + NesBackend<P> + Sized
where
    P: SnapshotState,
{
    fn new_smb_target(game: &SmbGame<Self, P>) -> Result<SmbTarget<Self, P>, String>;
}

impl SmbMachineKind<Vec<u8>> for QuickNesMachine {
    fn new_smb_target(game: &SmbGame<Self, Vec<u8>>) -> Result<SmbTarget<Self, Vec<u8>>, String> {
        #[cfg(test)]
        if game.loopback {
            return SmbTarget::loopback_for_tests(&game.rom).map_err(|error| error.to_string());
        }
        match &game.backend {
            SmbBackend::QuickNes => SmbTarget::from_smb_rom_bytes_headless(
                &game.rom,
                &game.core_path,
                &game.core_sha256,
            )
            .map_err(|error| error.to_string()),
            #[cfg(all(
                feature = "consonance",
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64"),
                not(miri)
            ))]
            SmbBackend::Consonance { .. } => {
                Err("SMB backend does not match the direct machine type".to_owned())
            }
        }
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl SmbMachineKind<ConsonancePortable> for ConsonanceMachine {
    fn new_smb_target(
        game: &SmbGame<Self, ConsonancePortable>,
    ) -> Result<SmbTarget<Self, ConsonancePortable>, String> {
        match &game.backend {
            SmbBackend::Consonance { kernel, initramfs } => {
                let machine =
                    ConsonanceMachine::new(kernel, initramfs).map_err(|error| error.to_string())?;
                if !machine.starts_at_power_on() {
                    return Err(
                        "SMB Consonance image does not publish a power-on NES guest state"
                            .to_owned(),
                    );
                }
                SmbTarget::from_machine(machine).map_err(|error| error.to_string())
            }
            SmbBackend::QuickNes => Err("SMB backend does not match Consonance".to_owned()),
        }
    }
}

impl SmbGame<QuickNesMachine, Vec<u8>> {
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            identity: quicknes_identity(core_sha256),
            backend: SmbBackend::QuickNes,
            machine: PhantomData,
            #[cfg(test)]
            loopback: false,
        }
    }

    pub fn from_environment(rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let core_path = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned libretro core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        Ok(Self::new(rom, &core_path, &core_sha256))
    }

    #[cfg(test)]
    pub(crate) fn loopback_for_tests(rom: &[u8]) -> Self {
        let core_sha256 = "a".repeat(64);
        Self {
            rom: rom.to_vec(),
            core_path: PathBuf::new(),
            identity: quicknes_identity(&core_sha256),
            core_sha256,
            backend: SmbBackend::QuickNes,
            machine: PhantomData,
            loopback: true,
        }
    }
}

#[cfg(all(
    feature = "consonance",
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64"),
    not(miri)
))]
impl SmbGame<ConsonanceMachine, ConsonancePortable> {
    #[must_use]
    pub fn new_consonance(rom: &[u8], kernel: &[u8], initramfs: &[u8]) -> Self {
        Self {
            rom: rom.to_vec(),
            core_path: PathBuf::new(),
            core_sha256: String::new(),
            identity: format!(
                "{};result_digest=postcard-1.1.3-sha256-hex-v2",
                consonance_identity(kernel, initramfs),
            ),
            backend: SmbBackend::Consonance {
                kernel: kernel.to_vec(),
                initramfs: initramfs.to_vec(),
            },
            machine: PhantomData,
            #[cfg(test)]
            loopback: false,
        }
    }
}

impl<M, P> SmbGame<M, P> {
    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub fn snapshot_checkpoint_format(&self) -> &'static str {
        match &self.backend {
            SmbBackend::QuickNes => SNAPSHOT_CHECKPOINT_FORMAT,
            #[cfg(all(
                feature = "consonance",
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64"),
                not(miri)
            ))]
            SmbBackend::Consonance { .. } => CONSONANCE_SNAPSHOT_CHECKPOINT_FORMAT,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SmbCampaignRun {
    pub chord: SmbCampaignChordPolicy,
    pub vocabulary: SmbButtonVocabulary,
    pub terminal: Option<SmbTerminalPredicate>,
}

pub struct SmbDrawState {
    tables: Option<EmpiricalStepTables<ButtonChord>>,
    versions: BTreeMap<u64, SmbChordTableVersion>,
}

#[derive(Clone, Default)]
pub struct SmbCampaignEvidence {
    aggregate: SmbMilestones,
    watermark: SmbProgressWatermark,
    first_reached: SmbMilestoneTimes,
    first_inputs: SmbMilestoneInputs,
    champion_input: SmbInput,
    champion_milestones: SmbMilestones,
}

pub type SmbCampaignOrigin<M = QuickNesMachine, P = Vec<u8>> = CampaignOrigin<SmbGame<M, P>>;
pub type SmbCampaignCheckpoint<P = Vec<u8>> = CampaignCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpoint<P = Vec<u8>> = SnapshotCheckpoint<SmbSnapshot<P>>;
pub type SmbSnapshotCheckpointEntry<P = Vec<u8>> =
    crate::search::campaign::SnapshotCheckpointEntry<SmbSnapshot<P>>;
pub type SmbCampaignStreamHeader = CampaignStreamHeader<SmbChordTableHeader>;
pub type SmbCampaignModeReport = CampaignModeReport<ButtonChord, SmbArchiveReport>;
pub type SmbCampaignProgressRecord = CampaignProgressRecord<SmbArchiveKey>;
pub(crate) type SmbCampaignActionResult<M = QuickNesMachine, P = Vec<u8>> =
    CampaignActionResult<SmbGame<M, P>>;
pub struct SmbCampaignConfig {
    pub campaign_seed: u64,
    pub workers: u32,
    pub execution_budget: u64,
    pub action_limit: usize,
    pub host: String,
    pub wall_budget: Option<std::time::Duration>,
    pub continue_after_victory: bool,
    pub archive_entry_limit: usize,
    pub reservations_per_worker: usize,
    pub memory_budget_mib: Option<usize>,
    pub materialize_final_artifacts: bool,
    pub chord: SmbCampaignChordPolicy,
    pub vocabulary: SmbButtonVocabulary,
    pub terminal: SmbTerminalPredicate,
    pub retention: RetentionPolicy,
    pub selector: crate::search::archive::SelectorPolicy,
    pub suffix: SuffixShape,
    pub mixture: DrawMixture,
    pub victory_input_path: Option<std::path::PathBuf>,
}

impl SmbCampaignConfig {
    fn generic<M, P>(&self) -> GenericCampaignConfig<SmbGame<M, P>>
    where
        M: SmbMachineKind<P>,
        P: SnapshotState,
    {
        GenericCampaignConfig {
            suffix: self.suffix,
            mixture: self.mixture,
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            continue_after_victory: self.continue_after_victory,
            archive_entry_limit: self.archive_entry_limit,
            reservations_per_worker: self.reservations_per_worker,
            memory_budget_mib: self.memory_budget_mib,
            materialize_final_artifacts: self.materialize_final_artifacts,
            run: SmbCampaignRun {
                chord: self.chord,
                vocabulary: self.vocabulary,
                terminal: Some(self.terminal),
            },
            retention: self.retention,
            selector: self.selector.clone(),
            victory_input_path: self.victory_input_path.clone(),
        }
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

    fn reached<M, P>(self, target: &SmbTarget<M, P>) -> Result<bool, Box<dyn Error>>
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
            Self::NesPressable => &crate::smb::archive::NES_PRESSABLE_BUTTON_MASKS,
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

pub fn derive_suffix(
    mutation_seed: u64,
    shape: SuffixShape,
    mixture: DrawMixture,
    mixture_weight: u8,
    chord_policy: SmbCampaignChordPolicy,
    vocabulary: SmbButtonVocabulary,
    chord_tables: Option<EmpiricalStepTableRef<'_, ButtonChord>>,
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(_) = chord_policy;
    draw_suffix(
        shape,
        mixture,
        mixture_weight,
        mutation_seed,
        |rand| {
            let tables = chord_tables.ok_or("derived chord policy has no folded tables")?;
            let length = tables.mixed_len()?;
            Ok(NonZeroUsize::new(length)
                .and_then(|length| tables.mixed_step(rand.below(length)))
                .copied())
        },
        |rand| crate::smb::archive::sample_chord_from_masks(rand, vocabulary.masks()),
    )
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordSourceFilter {
    pub world: u8,
    pub level: u8,
    pub minimum_progress: u16,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SmbChordSource {
    Level(SmbChordSourceFilter),
    All(SmbChordSourceAll),
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordSourceAll {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableDerivation {
    pub source_filter: SmbChordSource,
    pub parameters: EmpiricalStepParameters,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableHeader {
    pub source_sha256: String,
    pub derivation: SmbChordTableDerivation,
    pub initial: EmpiricalStepCheckpoint,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignChordPolicy {
    DerivedHalf(SmbChordTableDerivation),
}

impl Default for SmbCampaignChordPolicy {
    fn default() -> Self {
        Self::DerivedHalf(SmbChordTableDerivation {
            source_filter: SmbChordSource::All(SmbChordSourceAll {}),
            parameters: EmpiricalStepParameters {
                prefix_steps: 0,
                recent_successes: 128,
                recent_weight: 3,
                all_history_weight: 1,
                update_every_records: 64,
                hash_every_records: 1024,
            },
        })
    }
}

#[must_use]
pub fn chord_policy_identifier(policy: SmbCampaignChordPolicy) -> String {
    match policy {
        SmbCampaignChordPolicy::DerivedHalf(derivation) => {
            let parameters = derivation.parameters;
            let source = match derivation.source_filter {
                SmbChordSource::All(_) => "all".to_owned(),
                SmbChordSource::Level(filter) => {
                    format!(
                        "{},{},{}",
                        filter.world, filter.level, filter.minimum_progress
                    )
                }
            };
            format!(
                "chord_draw_recorded_53:{source},{},{},{},{},{},{}",
                parameters.prefix_steps,
                parameters.recent_successes,
                parameters.recent_weight,
                parameters.all_history_weight,
                parameters.update_every_records,
                parameters.hash_every_records
            )
        }
    }
}

pub fn chord_policy_from_identifier(
    identifier: &str,
) -> Result<SmbCampaignChordPolicy, Box<dyn Error>> {
    if let Some(fields) = identifier.strip_prefix("chord_draw_recorded_53:") {
        let mut fields = fields.split(',').peekable();
        let source_filter = if fields.peek() == Some(&"all") {
            fields.next();
            SmbChordSource::All(SmbChordSourceAll {})
        } else {
            SmbChordSource::Level(SmbChordSourceFilter {
                world: parse_chord_field(&mut fields, "world")?,
                level: parse_chord_field(&mut fields, "level")?,
                minimum_progress: parse_chord_field(&mut fields, "minimum progress")?,
            })
        };
        let parameters = EmpiricalStepParameters {
            prefix_steps: parse_chord_field(&mut fields, "prefix steps")?,
            recent_successes: parse_chord_field(&mut fields, "recent successes")?,
            recent_weight: parse_chord_field(&mut fields, "recent weight")?,
            all_history_weight: parse_chord_field(&mut fields, "all-history weight")?,
            update_every_records: parse_chord_field(&mut fields, "update interval")?,
            hash_every_records: parse_chord_field(&mut fields, "hash interval")?,
        };
        if fields.next().is_some() {
            return Err("derived chord policy carries extra fields".into());
        }
        parameters.validate()?;
        return Ok(SmbCampaignChordPolicy::DerivedHalf(
            SmbChordTableDerivation {
                source_filter,
                parameters,
            },
        ));
    }
    Err("campaign stream chord policy is not recognized".into())
}

fn parse_chord_field<'a, T>(
    fields: &mut impl Iterator<Item = &'a str>,
    name: &str,
) -> Result<T, Box<dyn Error>>
where
    T: std::str::FromStr,
    T::Err: Error + 'static,
{
    Ok(fields
        .next()
        .ok_or_else(|| format!("derived chord policy is missing {name}"))?
        .parse()?)
}

type InitialChordTables = (
    Option<EmpiricalStepTables<ButtonChord>>,
    Option<SmbChordTableHeader>,
);

fn source_batch_ready(pending: u64, batch: u64) -> bool {
    pending >= batch
}

fn initial_chord_tables(
    policy: SmbCampaignChordPolicy,
    origin: Option<(&str, &SmbArchiveReport)>,
) -> Result<InitialChordTables, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(derivation) = policy;
    let mut tables = EmpiricalStepTables::new(derivation.parameters)?;
    let source_sha256 = match origin {
        None => format!("{:x}", Sha256::digest([])),
        Some((file_sha256, report)) => {
            let parent_len: BTreeMap<u64, usize> = report
                .entries
                .iter()
                .map(|entry| (entry.id, entry.input.actions.len()))
                .collect();
            let mut source_pending = 0_u64;
            for entry in &report.entries {
                if source_filter_matches(derivation.source_filter, entry) {
                    let prefix = entry
                        .parent_id
                        .and_then(|parent| parent_len.get(&parent).copied())
                        .unwrap_or(0);
                    let folded = entry.input.actions.get(prefix..).unwrap_or(&[]);
                    tables.fold_retained(folded)?;
                    source_pending = source_pending.saturating_add(1);
                    if source_batch_ready(
                        source_pending,
                        derivation.parameters.update_every_records,
                    ) {
                        tables.flush()?;
                        source_pending = 0;
                    }
                }
            }
            file_sha256.to_owned()
        }
    };
    tables.flush()?;
    let initial = tables.checkpoint()?;
    let header = SmbChordTableHeader {
        source_sha256,
        derivation,
        initial,
    };
    Ok((Some(tables), Some(header)))
}

fn source_filter_matches(
    source: SmbChordSource,
    entry: &crate::smb::archive::SmbArchiveEntryReport,
) -> bool {
    match source {
        SmbChordSource::All(_) => true,
        SmbChordSource::Level(filter) => {
            (entry.key.world, entry.key.level) == (filter.world, filter.level)
                && entry.key.progress >= filter.minimum_progress
        }
    }
}

fn current_chord_checkpoint(
    tables: Option<&EmpiricalStepTables<ButtonChord>>,
) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
    tables
        .map(EmpiricalStepTables::checkpoint)
        .transpose()
        .map_err(Into::into)
}

struct SmbChordTableVersion {
    checkpoint: EmpiricalStepCheckpoint,
    history_len: usize,
    history_counts: std::rc::Rc<BTreeMap<ButtonChord, usize>>,
    recent: std::rc::Rc<Vec<ButtonChord>>,
}

fn recorded_chord_tables<'a>(
    policy: SmbCampaignChordPolicy,
    before: Option<&EmpiricalStepCheckpoint>,
    versions: &'a BTreeMap<u64, SmbChordTableVersion>,
    tables: Option<&'a EmpiricalStepTables<ButtonChord>>,
) -> Result<Option<EmpiricalStepTableRef<'a, ButtonChord>>, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(_) = policy;
    let before = before.ok_or("derived chord draw is missing its table version")?;
    let version = versions
        .get(&before.records)
        .ok_or("derived chord draw names an unknown table version")?;
    if version.checkpoint != *before {
        return Err("derived chord draw table hash does not match replay".into());
    }
    let tables = tables.ok_or("derived chord policy has no folded tables")?;
    Ok(Some(EmpiricalStepTableRef::from_counts(
        tables.parameters(),
        &version.recent,
        &version.history_counts,
        version.history_len,
    )))
}

fn remember_chord_version(
    tables: Option<&EmpiricalStepTables<ButtonChord>>,
    required: &BTreeSet<u64>,
    versions: &mut BTreeMap<u64, SmbChordTableVersion>,
) -> Result<(), Box<dyn Error>> {
    let Some(tables) = tables else {
        return Ok(());
    };
    if !required.contains(&tables.records()) {
        return Ok(());
    }
    let checkpoint = tables.checkpoint()?;
    let history_len = tables.history_len();
    let reusable = versions.last_key_value().filter(|(_, last)| {
        last.checkpoint.table_sha256 == checkpoint.table_sha256 && last.history_len == history_len
    });
    let recent = reusable
        .map(|(_, last)| std::rc::Rc::clone(&last.recent))
        .unwrap_or_else(|| std::rc::Rc::new(tables.recent().to_vec()));
    let history_counts = reusable
        .map(|(_, last)| std::rc::Rc::clone(&last.history_counts))
        .unwrap_or_else(|| std::rc::Rc::new(tables.compact_history().clone()));
    versions.insert(
        tables.records(),
        SmbChordTableVersion {
            checkpoint,
            history_len,
            history_counts,
            recent,
        },
    );
    Ok(())
}
impl<M, P> CampaignTypes for SmbGame<M, P>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
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
    type DrawState = SmbDrawState;
    type DrawCheckpoint = EmpiricalStepCheckpoint;
    type TableHeader = SmbChordTableHeader;
}

impl<M, P> Reporting for SmbGame<M, P>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }
    fn checkpoint_format(&self) -> &'static str {
        self.snapshot_checkpoint_format()
    }
    fn image_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rom))
    }
    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
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
            deaths: state.deaths,
            selector: state.selector,
        }
    }
}

impl<M, P> InputPolicy for SmbGame<M, P>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    fn draw_state_memory_reserve_bytes(&self, _run: &SmbCampaignRun, _max_actions: usize) -> usize {
        DRAW_STATE_MEMORY_RESERVE_BYTES
    }
    fn draw_state_memory_bytes(&self, state: &SmbDrawState) -> usize {
        state
            .tables
            .as_ref()
            .map_or(0, EmpiricalStepTables::memory_bytes)
    }
    fn policies(&self, run: &SmbCampaignRun) -> WorkloadPolicies {
        let mut policies: WorkloadPolicies = [
            (
                CONTROLLER_VOCABULARY_FIELD,
                button_vocabulary_identifier(run.vocabulary).to_owned(),
            ),
            (KEY_POLICY_FIELD, KEY_POLICY_IDENTIFIER.to_owned()),
            (DURATION_POLICY_FIELD, DURATION_IDENTIFIER.to_owned()),
            (CHORD_POLICY_FIELD, chord_policy_identifier(run.chord)),
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
            chord: chord_policy_from_identifier(recorded(CHORD_POLICY_FIELD)?)?,
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
    fn initial_draw_state(
        &self,
        run: &SmbCampaignRun,
        origin: Option<(&str, &SmbArchiveReport)>,
    ) -> Result<crate::search::campaign::InitialDrawState<Self>, Box<dyn Error>> {
        let (tables, header) = initial_chord_tables(run.chord, origin)?;
        Ok((
            SmbDrawState {
                tables,
                versions: BTreeMap::new(),
            },
            header,
        ))
    }
    fn draw_checkpoint(
        &self,
        state: &SmbDrawState,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        current_chord_checkpoint(state.tables.as_ref())
    }
    fn draw_checkpoint_to_wire(
        &self,
        checkpoint: &EmpiricalStepCheckpoint,
    ) -> Result<EmpiricalStepCheckpoint, Box<dyn Error>> {
        Ok(checkpoint.clone())
    }
    fn draw_checkpoint_from_wire(
        &self,
        checkpoint: Option<&EmpiricalStepCheckpoint>,
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        Ok(checkpoint.cloned())
    }
    fn expand_suffix(
        &self,
        run: &SmbCampaignRun,
        state: &SmbDrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        derive_suffix(
            mutation_seed,
            shape,
            mixture.mixture,
            mixture.weight,
            run.chord,
            run.vocabulary,
            state.tables.as_ref().map(EmpiricalStepTables::view),
        )
    }
    fn expand_suffix_recorded(
        &self,
        run: &SmbCampaignRun,
        state: &SmbDrawState,
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        let tables =
            recorded_chord_tables(run.chord, before, &state.versions, state.tables.as_ref())?;
        derive_suffix(
            mutation_seed,
            shape,
            mixture.mixture,
            mixture.weight,
            run.chord,
            run.vocabulary,
            tables,
        )
    }
    fn finish_stream_record(
        &self,
        run: &SmbCampaignRun,
        state: &mut SmbDrawState,
        retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        let SmbCampaignChordPolicy::DerivedHalf(_) = run.chord;
        let tables = state
            .tables
            .as_mut()
            .ok_or("derived chord policy has no folded tables")?;
        for (parent_actions, input) in retained {
            let folded = input.get(*parent_actions..).unwrap_or(&[]);
            tables.fold_retained(folded)?;
        }
        Ok(tables.finish_record()?)
    }
    fn retained_inputs_need_full(&self, _run: &SmbCampaignRun) -> bool {
        false
    }
    fn remember_draw_version(
        &self,
        state: &mut SmbDrawState,
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        let SmbDrawState { tables, versions } = state;
        remember_chord_version(tables.as_ref(), required, versions)
    }

    fn max_action_limit(&self) -> usize {
        crate::smb::archive::MAX_SMB_COMPLETION_ACTIONS
    }

    fn longest_action_time(&self) -> u64 {
        u64::from(crate::smb::archive::LONG_HOLD_FRAMES.1)
    }
}

impl<M, P> TargetExecution for SmbGame<M, P>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    fn new_target(&self) -> Result<SmbTarget<M, P>, String> {
        M::new_smb_target(self)
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
    fn frames_clocked(&self, target: &SmbTarget<M, P>) -> u64 {
        target.frames_clocked()
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
        target.last_action_observations().to_vec()
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

    fn action_time_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn snapshot_memory_charge(snapshot: &SmbSnapshot<P>) -> usize {
        snapshot.resident_memory_charge()
    }
}

impl<M, P> Evaluation for SmbGame<M, P>
where
    M: SmbMachineKind<P>,
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

    fn is_terminal(&self, target: &SmbTarget<M, P>) -> bool {
        target.is_dead() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        run: &SmbCampaignRun,
        target: &SmbTarget<M, P>,
    ) -> Result<bool, Box<dyn Error>> {
        if self.is_terminal(target) {
            return Ok(true);
        }
        run.terminal.unwrap_or_default().reached(target)
    }

    fn rollout_outcome(
        &self,
        run: &SmbCampaignRun,
        target: &SmbTarget<M, P>,
    ) -> Result<crate::search::rollout::Outcome, Box<dyn Error>> {
        Ok(crate::search::rollout::Outcome {
            dead: target.is_dead(),
            victory: run.terminal.unwrap_or_default().reached(target)?,
            failed: target.exit_kind() != ExitKind::Ok,
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

pub fn run_smb_campaign<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    run_smb_campaign_with_progress(game, config, origin, stream, None)
}

pub fn run_smb_campaign_with_progress<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    run_smb_campaign_checkpointed(game, config, origin, stream, progress).map(|(report, _)| report)
}

pub fn run_smb_campaign_checkpointed<M, P>(
    game: &SmbGame<M, P>,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin<M, P>,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint<P>), Box<dyn Error>>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

pub fn replay_smb_campaign<M, P>(
    game: &SmbGame<M, P>,
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    replay_smb_campaign_checkpointed(game, stream_bytes, origin_report, None)
        .map(|(report, _)| report)
}

pub fn replay_smb_campaign_checkpointed<M, P>(
    game: &SmbGame<M, P>,
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
    origin_checkpoint: Option<&SmbCampaignCheckpoint<P>>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint<P>), Box<dyn Error>>
where
    M: SmbMachineKind<P>,
    P: SnapshotState,
{
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        collections::{BTreeMap, BTreeSet},
        rc::Rc,
    };

    use super::{
        DrawMixture, SNAPSHOT_CHECKPOINT_FORMAT, SmbButtonVocabulary, SmbCampaignActionResult,
        SmbCampaignCheckpoint, SmbCampaignChordPolicy, SmbCampaignConfig, SmbCampaignOrigin,
        SmbCampaignProgressRecord, SmbCampaignRun, SmbCampaignStreamRecord, SmbChordTableVersion,
        SmbGame, SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry, SmbTerminalPredicate,
        SuffixShape, chord_policy_from_identifier, chord_policy_identifier, derive_suffix,
        derive_worker_seed, remember_chord_version, source_batch_ready,
    };
    use crate::search::campaign::{
        DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER, Evaluation, InputPolicy, TargetExecution,
    };
    use crate::search::empirical_steps::EmpiricalStepTables;
    use crate::{
        search::empirical_steps::EmpiricalStepParameters,
        smb::archive::SmbArchiveReport,
        smb::target::{
            ButtonChord, SmbInput, SmbMechanicalState, SmbMilestones, SmbObservations,
            SmbProgressWatermark, SmbTarget,
        },
        target::Target,
    };

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    fn test_game(rom: &[u8]) -> SmbGame {
        SmbGame::loopback_for_tests(rom)
    }

    fn run_smb_campaign(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::run_smb_campaign(&test_game(rom), config, origin, stream)
    }

    fn run_smb_campaign_with_progress(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
        progress: Option<&mut dyn std::io::Write>,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::run_smb_campaign_with_progress(&test_game(rom), config, origin, stream, progress)
    }

    fn run_smb_campaign_checkpointed(
        rom: &[u8],
        config: &SmbCampaignConfig,
        origin: &SmbCampaignOrigin,
        stream: &mut dyn std::io::Write,
        progress: Option<&mut dyn std::io::Write>,
    ) -> Result<(super::SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn std::error::Error>>
    {
        super::run_smb_campaign_checkpointed(&test_game(rom), config, origin, stream, progress)
    }

    fn replay_smb_campaign(
        rom: &[u8],
        stream_bytes: &[u8],
        origin_report: Option<&SmbArchiveReport>,
    ) -> Result<super::SmbCampaignModeReport, Box<dyn std::error::Error>> {
        super::replay_smb_campaign(&test_game(rom), stream_bytes, origin_report)
    }

    fn replay_smb_campaign_checkpointed(
        rom: &[u8],
        stream_bytes: &[u8],
        origin_report: Option<&SmbArchiveReport>,
        origin_checkpoint: Option<&SmbCampaignCheckpoint>,
    ) -> Result<(super::SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn std::error::Error>>
    {
        super::replay_smb_campaign_checkpointed(
            &test_game(rom),
            stream_bytes,
            origin_report,
            origin_checkpoint,
        )
    }

    fn genesis_config(
        campaign_seed: u64,
        workers: u32,
        execution_budget: u64,
    ) -> SmbCampaignConfig {
        SmbCampaignConfig {
            vocabulary: SmbButtonVocabulary::default(),
            terminal: super::SmbTerminalPredicate::GameVictory,
            campaign_seed,
            workers,
            execution_budget,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            continue_after_victory: false,
            archive_entry_limit: 32_768,
            reservations_per_worker: DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
            memory_budget_mib: None,
            materialize_final_artifacts: true,
            chord: SmbCampaignChordPolicy::default(),
            retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            selector: crate::search::archive::SelectorPolicy::GroupUniform,
            suffix: SuffixShape::default(),
            mixture: DrawMixture::BiasedHalf,
            victory_input_path: None,
        }
    }

    #[test]
    fn worker_seed_derivation_is_stable() {
        let seeds = (0..3)
            .map(|index| derive_worker_seed(0x5eed_ca00, index).expect("derive worker seed"))
            .collect::<Vec<_>>();
        assert_eq!(seeds.len(), 3);
        assert_ne!(seeds[0], seeds[1]);
        assert_ne!(seeds[1], seeds[2]);
        let again = derive_worker_seed(0x5eed_ca00, 0).expect("derive worker seed again");
        assert_eq!(seeds[0], again);
    }

    #[test]
    fn suffix_derivation_is_pure_and_bounded() {
        for seed in [0_u64, 0x5eed_ca01, u64::MAX] {
            let history = BTreeMap::new();
            let empty = crate::search::empirical_steps::EmpiricalStepTableRef::from_counts(
                empty_table_parameters(),
                &[],
                &history,
                0,
            );
            let first = derive_suffix(
                seed,
                SuffixShape::OneOrTwo,
                DrawMixture::BiasedHalf,
                128,
                SmbCampaignChordPolicy::default(),
                SmbButtonVocabulary::default(),
                Some(empty),
            )
            .expect("derive suffix");
            let second = derive_suffix(
                seed,
                SuffixShape::OneOrTwo,
                DrawMixture::BiasedHalf,
                128,
                SmbCampaignChordPolicy::default(),
                SmbButtonVocabulary::default(),
                Some(empty),
            )
            .expect("derive suffix again");
            assert_eq!(first, second);
            assert!((1..=2).contains(&first.len()));
            assert!(
                first
                    .iter()
                    .all(|chord| (2..=12).contains(&chord.hold_frames)
                        || (96..=120).contains(&chord.hold_frames))
            );
        }
    }

    fn empty_table_parameters() -> EmpiricalStepParameters {
        let SmbCampaignChordPolicy::DerivedHalf(derivation) = SmbCampaignChordPolicy::default();
        derivation.parameters
    }

    fn derived_policy() -> SmbCampaignChordPolicy {
        SmbCampaignChordPolicy::DerivedHalf(super::SmbChordTableDerivation {
            source_filter: super::SmbChordSource::Level(super::SmbChordSourceFilter {
                world: 0,
                level: 0,
                minimum_progress: 0,
            }),
            parameters: EmpiricalStepParameters {
                prefix_steps: 0,
                recent_successes: 4,
                recent_weight: 3,
                all_history_weight: 1,
                update_every_records: 2,
                hash_every_records: 2,
            },
        })
    }

    #[test]
    fn smb_memory_and_suffix_input_contracts_are_exact() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot");
        let run = SmbCampaignRun {
            chord: derived_policy(),
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::default()),
        };

        assert_eq!(
            game.draw_state_memory_reserve_bytes(&run, 96),
            2 * 1024 * 1024
        );
        assert_eq!(
            <SmbGame as TargetExecution>::snapshot_memory_charge(&snapshot),
            snapshot.resident_memory_charge()
        );
        assert!(!game.retained_inputs_need_full(&run));
        assert!(!source_batch_ready(1, 2));
        assert!(source_batch_ready(2, 2));
        assert!(source_batch_ready(3, 2));

        let (mut draw_state, _) = game
            .initial_draw_state(&run, None)
            .expect("initial draw state");
        draw_state
            .tables
            .as_mut()
            .expect("derived tables")
            .fold_retained(&[ButtonChord::new(0x01, 4), ButtonChord::new(0x02, 5)])
            .expect("fold retained draw state");
        let expected = draw_state
            .tables
            .as_ref()
            .expect("derived tables")
            .memory_bytes();
        assert!(expected > 1);
        assert_eq!(game.draw_state_memory_bytes(&draw_state), expected);
    }

    #[test]
    fn remembered_chord_versions_reuse_only_an_exact_visible_table() {
        let SmbCampaignChordPolicy::DerivedHalf(derivation) = derived_policy();
        let mut tables = EmpiricalStepTables::new(derivation.parameters).expect("tables");
        let chord = ButtonChord::new(0x01, 4);
        tables.fold_retained(&[chord]).expect("fold chord");
        tables.finish_record().expect("finish record");
        tables.flush().expect("flush table");
        let checkpoint = tables.checkpoint().expect("checkpoint");
        let history_len = tables.history_len();
        let required = BTreeSet::from([tables.records()]);
        let history_counts = Rc::new(tables.compact_history().clone());

        let sentinel = Rc::new(vec![ButtonChord::new(0x80, 7)]);
        let exact = SmbChordTableVersion {
            checkpoint: checkpoint.clone(),
            history_len,
            history_counts: Rc::clone(&history_counts),
            recent: Rc::clone(&sentinel),
        };
        let mut versions = BTreeMap::from([(0, exact)]);
        remember_chord_version(Some(&tables), &required, &mut versions)
            .expect("remember exact version");
        assert!(Rc::ptr_eq(&versions[&tables.records()].recent, &sentinel));

        let wrong_len = SmbChordTableVersion {
            checkpoint: checkpoint.clone(),
            history_len: history_len.saturating_add(1),
            history_counts: Rc::clone(&history_counts),
            recent: Rc::clone(&sentinel),
        };
        let mut versions = BTreeMap::from([(0, wrong_len)]);
        remember_chord_version(Some(&tables), &required, &mut versions)
            .expect("remember after length mismatch");
        assert!(!Rc::ptr_eq(&versions[&tables.records()].recent, &sentinel));

        let mut wrong_hash = checkpoint;
        wrong_hash.table_sha256 = "0".repeat(64);
        let wrong_hash = SmbChordTableVersion {
            checkpoint: wrong_hash,
            history_len,
            history_counts,
            recent: Rc::clone(&sentinel),
        };
        let mut versions = BTreeMap::from([(0, wrong_hash)]);
        remember_chord_version(Some(&tables), &required, &mut versions)
            .expect("remember after hash mismatch");
        assert!(!Rc::ptr_eq(&versions[&tables.records()].recent, &sentinel));
    }

    fn evidence_action(milestones: SmbMilestones) -> SmbCampaignActionResult {
        SmbCampaignActionResult {
            action: ButtonChord::new(0x01, 4),
            observations: vec![SmbObservations {
                frame_count: 1,
                wram: Vec::new(),
                decoded: SmbMechanicalState {
                    world: 1,
                    level: 2,
                    progress: 33,
                    ..SmbMechanicalState::default()
                },
                milestones,
                changed_indices: Vec::new(),
                dead: false,
                log_line: String::new(),
            }],
            milestones,
            dead: false,
            victory: false,
            failed: false,
            candidate: None,
        }
    }

    #[test]
    fn action_evidence_materializes_inputs_only_for_real_discoveries() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let run = |milestones| {
            let mut evidence = super::SmbCampaignEvidence::default();
            let calls = Cell::new(0);
            game.merge_action_evidence(&mut evidence, &evidence_action(milestones), 7, || {
                calls.set(calls.get() + 1);
                Ok(SmbInput {
                    actions: vec![ButtonChord::new(0x01, 4)],
                })
            })
            .expect("merge action evidence");
            (evidence, calls.get())
        };

        let (empty, empty_calls) = run(SmbMilestones::default());
        assert_eq!(empty_calls, 0);
        assert_eq!(
            empty.watermark,
            SmbProgressWatermark {
                world: 1,
                level: 2,
                progress: 33
            }
        );
        assert_eq!(empty.champion_milestones, SmbMilestones::default());

        let first = SmbMilestones {
            max_1_1_scroll_bucket: 1,
            ..SmbMilestones::default()
        };
        let (progress, progress_calls) = run(first);
        assert_eq!(progress_calls, 1);
        assert_eq!(progress.first_reached.progress_into_1_1, Some(7));
        assert_eq!(progress.champion_milestones, first);
        assert_eq!(progress.champion_input.actions.len(), 1);

        for milestones in [
            SmbMilestones {
                reached_1_1_flag: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_2: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_onward: true,
                ..SmbMilestones::default()
            },
        ] {
            let (evidence, calls) = run(milestones);
            assert_eq!(calls, 1);
            assert_eq!(evidence.champion_milestones, milestones);
        }

        let reigning = SmbMilestones {
            max_1_1_scroll_bucket: u16::MAX,
            reached_1_1_flag: true,
            reached_1_2: true,
            reached_onward: true,
        };
        for milestones in [
            SmbMilestones {
                max_1_1_scroll_bucket: 1,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_1_flag: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_1_2: true,
                ..SmbMilestones::default()
            },
            SmbMilestones {
                reached_onward: true,
                ..SmbMilestones::default()
            },
        ] {
            let mut evidence = super::SmbCampaignEvidence {
                champion_milestones: reigning,
                ..super::SmbCampaignEvidence::default()
            };
            let calls = Cell::new(0);
            game.merge_action_evidence(&mut evidence, &evidence_action(milestones), 9, || {
                calls.set(calls.get() + 1);
                Ok(SmbInput {
                    actions: vec![ButtonChord::new(0x02, 4)],
                })
            })
            .expect("merge first-input-only evidence");
            assert_eq!(calls.get(), 1);
            assert_eq!(evidence.champion_milestones, reigning);
        }
    }

    #[test]
    fn derived_chord_policy_identifier_round_trips() {
        let policy = derived_policy();
        let identifier = chord_policy_identifier(policy);
        assert_eq!(
            chord_policy_from_identifier(&identifier).expect("parse derived policy"),
            policy
        );
        assert!(identifier.starts_with("chord_draw_recorded_53:"));
        for version in ["50", "51", "52", "54"] {
            assert!(
                chord_policy_from_identifier(&format!(
                    "chord_draw_recorded_{version}:all,0,4,3,1,2,2"
                ))
                .is_err()
            );
        }
        let SmbCampaignChordPolicy::DerivedHalf(mut derivation) = policy;
        derivation.source_filter = super::SmbChordSource::All(super::SmbChordSourceAll {});
        let all_levels = SmbCampaignChordPolicy::DerivedHalf(derivation);
        let identifier = chord_policy_identifier(all_levels);
        assert!(identifier.starts_with("chord_draw_recorded_53:all,"));
        assert_eq!(
            chord_policy_from_identifier(&identifier).expect("parse all-levels policy"),
            all_levels
        );
        let level_header = serde_json::json!({
            "world": 2, "level": 1, "minimum_progress": 40
        });
        assert_eq!(
            serde_json::from_value::<super::SmbChordSource>(level_header)
                .expect("legacy source deserializes"),
            super::SmbChordSource::Level(super::SmbChordSourceFilter {
                world: 2,
                level: 1,
                minimum_progress: 40
            })
        );
    }

    #[test]
    fn worker_keys_preserve_recorded_room_assignment() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = game.new_target().expect("load target");
        target.reset();
        target.poke_wram(0x074e, 7);
        target.poke_wram(0x074f, 9);
        let origin = target.snapshot().expect("snapshot room");
        let result = game
            .execute_job(
                &SmbCampaignRun {
                    chord: SmbCampaignChordPolicy::default(),
                    vocabulary: SmbButtonVocabulary::default(),
                    terminal: Some(SmbTerminalPredicate::GameVictory),
                },
                &mut target,
                &origin,
                &[],
                0,
                SmbMilestones::default(),
                &[ButtonChord::new(0x01, 1)],
                96,
                crate::search::archive::RetentionPolicy::AdmitAlive,
            )
            .expect("execute room action");
        let candidate = result.actions[0]
            .candidate
            .as_ref()
            .expect("live candidate");
        assert_eq!(candidate.key.room, [0; 3]);
        let completed = game
            .complete_candidate_key(candidate.key, &candidate.snapshot)
            .expect("complete arrival room");
        assert_eq!(completed, game.current_key(&target).expect("current room"));
        assert_eq!(&completed.room[..2], &[7, 9]);
        assert_ne!(
            postcard::to_stdvec(&candidate.key).expect("recorded worker key"),
            postcard::to_stdvec(&completed).expect("completed archive key")
        );
    }

    #[test]
    fn job_execution_is_pure_across_target_instances() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut first = game.new_target().expect("load first target");
        let mut second = game.new_target().expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let history = BTreeMap::new();
        let empty = crate::search::empirical_steps::EmpiricalStepTableRef::from_counts(
            empty_table_parameters(),
            &[],
            &history,
            0,
        );
        let suffix = derive_suffix(
            0x5eed_ca02,
            SuffixShape::OneOrTwo,
            DrawMixture::BiasedHalf,
            128,
            SmbCampaignChordPolicy::default(),
            SmbButtonVocabulary::default(),
            Some(empty),
        )
        .expect("derive suffix");
        first.apply(&ButtonChord::new(0x02, 30));
        let run = SmbCampaignRun {
            chord: SmbCampaignChordPolicy::default(),
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::GameVictory),
        };
        let on_first = game
            .execute_job(
                &run,
                &mut first,
                &snapshot,
                &[],
                1,
                SmbMilestones::default(),
                &suffix,
                96,
                crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            )
            .expect("execute job on first instance");
        let on_second = game
            .execute_job(
                &run,
                &mut second,
                &snapshot,
                &[],
                1,
                SmbMilestones::default(),
                &suffix,
                96,
                crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            )
            .expect("execute job on second instance");
        assert_eq!(on_first, on_second);
    }

    #[test]
    fn a_job_from_a_won_snapshot_executes_nothing() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = game.new_target().expect("load target");
        target.reset();
        target.poke_wram(0x0770, 2);
        target.poke_wram(0x075f, 7);
        let won = target.snapshot().expect("snapshot won state");
        let result = game
            .execute_job(
                &SmbCampaignRun {
                    chord: SmbCampaignChordPolicy::default(),
                    vocabulary: SmbButtonVocabulary::default(),
                    terminal: Some(SmbTerminalPredicate::GameVictory),
                },
                &mut target,
                &won,
                &[],
                0,
                SmbMilestones::default(),
                &[ButtonChord::new(0x01, 4)],
                96,
                crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            )
            .expect("execute job");
        assert!(result.actions.is_empty());
    }

    #[test]
    fn terminal_predicate_is_mechanical_and_round_trips() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let transition = SmbTerminalPredicate::LevelTransition { world: 0, level: 0 };
        assert!(!transition.reached(&target).expect("initial terminal state"));
        target.poke_wram(0x075c, 1);
        assert!(transition.reached(&target).expect("changed terminal state"));
        assert_eq!(
            SmbTerminalPredicate::from_identifier(&transition.identifier())
                .expect("terminal predicate round trip"),
            transition
        );
        assert!(SmbTerminalPredicate::from_identifier("level_transition:0,0,1").is_err());
        assert!(SmbTerminalPredicate::from_identifier("level_transition:256,0").is_err());
        assert!(SmbTerminalPredicate::from_identifier("coordinates:0,0").is_err());
    }

    #[test]
    fn snapshot_root_replays_and_binds_its_identity() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot root");
        let snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry { id: 0, snapshot }],
        };
        let checkpoint_bytes = snapshots.to_bytes().expect("encode root checkpoint");
        let checkpoint = SmbCampaignCheckpoint {
            path: "fixture-neutral-00".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(&checkpoint_bytes)),
            snapshots,
        };
        let config = SmbCampaignConfig {
            terminal: SmbTerminalPredicate::LevelTransition { world: 0, level: 0 },
            ..genesis_config(0x5eed_ca20, 2, 4)
        };
        let origin = SmbCampaignOrigin::SnapshotRoot {
            checkpoint: checkpoint.clone(),
        };
        let mut stream = Vec::new();
        let (live, live_checkpoint) =
            run_smb_campaign_checkpointed(&rom, &config, &origin, &mut stream, None)
                .expect("snapshot-root campaign");
        assert_eq!(live.origin.kind, "snapshot_root");
        assert_eq!(live.origin.resume_actions, 0);
        assert_eq!(live.resume_policy, "snapshot_root");
        assert_eq!(live.archive.entries[0].id, 0);
        assert!(live.archive.entries[0].input.actions.len() <= config.action_limit);
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&checkpoint))
                .expect("replay snapshot-root campaign");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, None).is_err());

        let mut wrong_path = checkpoint.clone();
        wrong_path.path.push_str("-wrong");
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&wrong_path)).is_err());
        let mut wrong_hash = checkpoint.clone();
        let replacement = if wrong_hash.file_sha256.starts_with('0') {
            "1"
        } else {
            "0"
        };
        wrong_hash.file_sha256.replace_range(..1, replacement);
        assert!(replay_smb_campaign_checkpointed(&rom, &stream, None, Some(&wrong_hash)).is_err());
    }

    #[test]
    fn snapshot_root_rejects_the_recorded_terminal_predicate() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        target.poke_wram(0x075c, 1);
        let snapshot = target.snapshot().expect("snapshot terminal root");
        let snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry { id: 0, snapshot }],
        };
        let checkpoint = SmbCampaignCheckpoint {
            path: "fixture-terminal-00".to_owned(),
            file_sha256: format!(
                "{:x}",
                Sha256::digest(snapshots.to_bytes().expect("encode terminal root"))
            ),
            snapshots,
        };
        let config = SmbCampaignConfig {
            terminal: SmbTerminalPredicate::LevelTransition { world: 0, level: 0 },
            ..genesis_config(0x5eed_ca21, 1, 1)
        };
        let origin = SmbCampaignOrigin::SnapshotRoot { checkpoint };
        assert!(
            run_smb_campaign_checkpointed(&rom, &config, &origin, &mut Vec::new(), None).is_err()
        );
    }

    #[test]
    fn twelve_worker_windowed_streams_are_repeatable_and_replay_exactly() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca21, 12, 24);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        let mut first_stream = Vec::new();
        let first = run_smb_campaign(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut first_stream,
        )
        .expect("first 12-worker campaign");
        let mut second_stream = Vec::new();
        let second = run_smb_campaign(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut second_stream,
        )
        .expect("second 12-worker campaign");
        let first_replay =
            replay_smb_campaign(&rom, &first_stream, None).expect("first windowed stream replays");
        let second_replay = replay_smb_campaign(&rom, &second_stream, None)
            .expect("second windowed stream replays");
        assert_eq!(first_stream, second_stream);
        assert_eq!(first, second);
        assert_eq!(first, first_replay);
        assert_eq!(second, second_replay);
        assert!(
            std::str::from_utf8(&first_stream)
                .expect("stream utf-8")
                .lines()
                .next()
                .expect("stream header")
                .contains("\"schedule_policy\":\"deterministic_window_1_per_worker_v3\"")
        );
    }

    #[test]
    fn a_run_without_a_victory_reports_no_first_victory_counters() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca43, 2, 32);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign without a victory");
        assert_eq!(live.victories, 0);
        assert_eq!(live.frames_to_first_victory, None);
        assert_eq!(live.executions_to_first_victory, None);
        assert!(live.executions_completed > 0);
        let json = serde_json::to_string(&live).expect("serialize report");
        for field in ["frames_to_first_victory", "executions_to_first_victory"] {
            assert!(
                !json.contains(field),
                "a run without a victory must not add {field} to its report"
            );
        }
    }

    #[test]
    fn a_zero_reservation_window_is_refused_by_the_public_campaign_entry() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca40, 2, 8);
        config.reservations_per_worker = 0;
        let mut stream = Vec::new();
        let error = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect_err("a zero window is refused");
        assert!(
            error.to_string().contains("reservations per worker"),
            "unexpected error: {error}"
        );
        assert!(
            stream.is_empty(),
            "a refused run must not write a stream header"
        );
    }

    #[test]
    fn a_live_window_of_sixty_four_records_and_replays_as_a_window() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca41, 2, 256);
        config.reservations_per_worker = 64;
        config.memory_budget_mib = Some(4);
        let mut stream = Vec::new();
        let (live, live_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("window-64 live campaign");
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        assert!(
            recorded
                .lines()
                .next()
                .expect("stream header")
                .contains("\"schedule_policy\":\"deterministic_window_64_per_worker_v3\""),
            "unexpected header: {}",
            recorded.lines().next().unwrap_or_default()
        );
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, recorded.as_bytes(), None, None)
                .expect("window-64 stream replays off the windowed path");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        let legacy_tagged = recorded.replacen(
            "deterministic_window_64_per_worker_v3",
            "deterministic_window_64_per_worker_v1",
            1,
        );
        assert!(
            replay_smb_campaign_checkpointed(&rom, legacy_tagged.as_bytes(), None, None).is_err(),
            "the legacy path must not silently accept a live window-64 stream"
        );
    }

    #[test]
    fn obsolete_campaign_policies_are_rejected_before_replay() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca44, 2, 128);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 1;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("budgeted live campaign");
        assert!(
            live.history_compactions > 0 && live.historical_entries_dropped > 0,
            "the budget must bind for this stream to be maintenance-sensitive"
        );
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        replay_smb_campaign(&rom, recorded.as_bytes(), None).expect("its own namespace replays");

        for historical in [
            recorded.replacen("_per_worker_v3", "_per_worker_v1", 1),
            recorded.replacen("_per_worker_v3", "_per_worker_v2", 1),
            recorded.replacen(
                "mechanical_watermark_bounded_1024_v2",
                "mechanical_watermark_v1",
                1,
            ),
        ] {
            let error = replay_smb_campaign(&rom, historical.as_bytes(), None)
                .expect_err("an obsolete campaign policy is refused");
            let message = error.to_string();
            assert!(
                message.contains("schedule policy") || message.contains("progress policy"),
                "unexpected obsolete policy error: {message}"
            );
        }

        let without_schedule = recorded.replacen(
            "\"schedule_policy\":\"deterministic_window_1_per_worker_v3\",",
            "",
            1,
        );
        assert!(
            replay_smb_campaign(&rom, without_schedule.as_bytes(), None).is_err(),
            "a recording without the current schedule policy is refused"
        );
        let without_progress = recorded.replacen(
            "\"progress_policy\":\"mechanical_watermark_bounded_1024_v2\",",
            "",
            1,
        );
        assert!(
            replay_smb_campaign(&rom, without_progress.as_bytes(), None).is_err(),
            "a recording without the current progress policy is refused"
        );
    }

    #[test]
    fn snapshot_root_rejects_malformed_checkpoint_shapes() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot root");
        let valid_snapshots = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: vec![SmbSnapshotCheckpointEntry {
                id: 0,
                snapshot: snapshot.clone(),
            }],
        };
        let valid = SmbCampaignCheckpoint {
            path: "fixture-neutral-01".to_owned(),
            file_sha256: format!(
                "{:x}",
                Sha256::digest(valid_snapshots.to_bytes().expect("encode valid root"))
            ),
            snapshots: valid_snapshots,
        };
        for malformed in [
            SmbCampaignCheckpoint {
                path: String::new(),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                file_sha256: "0".repeat(63),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                file_sha256: "0".repeat(64),
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: "wrong".to_owned(),
                    entries: valid.snapshots.entries.clone(),
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: Vec::new(),
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: vec![SmbSnapshotCheckpointEntry {
                        id: 1,
                        snapshot: snapshot.clone(),
                    }],
                },
                ..valid.clone()
            },
            SmbCampaignCheckpoint {
                snapshots: SmbSnapshotCheckpoint {
                    format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
                    entries: vec![
                        SmbSnapshotCheckpointEntry {
                            id: 0,
                            snapshot: snapshot.clone(),
                        },
                        SmbSnapshotCheckpointEntry { id: 1, snapshot },
                    ],
                },
                ..valid.clone()
            },
        ] {
            let config = genesis_config(0x5eed_ca23, 1, 1);
            let origin = SmbCampaignOrigin::SnapshotRoot {
                checkpoint: malformed,
            };
            assert!(
                run_smb_campaign_checkpointed(&rom, &config, &origin, &mut Vec::new(), None)
                    .is_err()
            );
        }
    }

    #[test]
    fn live_campaign_replays_byte_identically() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca03, 4, 32);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        assert_eq!(live.executions_completed, 32);
        assert_eq!(live.jobs_per_worker.iter().sum::<u64>(), 32);
        assert_eq!(live.victories, 0);
        assert_eq!(live.victory_input, None);
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        for identifier in [
            "room_cell_uniform_128",
            "probe_at_admission_45",
            "fewest_frames_in_level",
            "whole_tree",
            "nes_pressable_36",
            "frozen_area_span",
            "one_to_six",
            "stratified",
        ] {
            assert!(header.contains(identifier), "header lacks {identifier}");
        }
        for line in text.lines().skip(1) {
            assert!(line.contains("\"selector\""));
            assert_eq!(
                line.contains("\"room_cell_uniform\""),
                line.contains("\"concentration\"")
            );
        }
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay recorded campaign");
        assert_eq!(live, replayed);
        let live_bytes = serde_json::to_vec_pretty(&live).expect("serialize live report");
        let replay_bytes = serde_json::to_vec_pretty(&replayed).expect("serialize replayed report");
        assert_eq!(live_bytes, replay_bytes);
        let accounting = live.archive.selector;
        assert_eq!(
            accounting
                .uniform_selections
                .checked_add(accounting.cell_selections),
            live.executions_completed
                .checked_add(live.duplicates_skipped)
        );
        assert_eq!(accounting.concentration.window_cap, 128);
        assert_eq!(
            accounting.concentration.window_draws,
            accounting.cell_selections
        );
    }

    #[test]
    fn budgeted_64_entry_campaign_reactivates_at_action_limit_and_replays_exactly() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca31, 4, 8_192);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 64;
        let mut stream = Vec::new();
        let (live, live_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("budgeted live campaign");
        assert_eq!(live.executions_completed, 8_192);
        assert_eq!(live.memory_budget_mib, Some(4));
        assert!(live.resident_memory_bytes <= 4 * 1024 * 1024);
        assert!(live.duplicates_skipped > 0);
        assert!(live.archive.retained > 1);
        assert!(live.history_compactions > 0);
        assert!(live.historical_entries_dropped > 0);
        assert!(live.live_entries < usize::try_from(live.archive.retained).unwrap_or(usize::MAX));
        assert!(live_checkpoint.entries.len() <= live.archive.entries.len());

        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, None)
                .expect("replay budgeted campaign");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        let anchor = live
            .archive
            .entries
            .iter()
            .find(|entry| entry.id == 0)
            .expect("genesis liveness anchor remains retained");
        assert!(
            anchor
                .selector
                .as_ref()
                .is_some_and(|selector| selector.selected > 0 && selector.productive > 0)
        );
    }

    #[test]
    fn budgeted_single_entry_campaign_reactivates_displaced_anchor() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca32, 1, 128);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 1;
        let mut stream = Vec::new();
        let (live, checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("single-entry bounded campaign");
        assert!(live.history_compactions > 0);
        assert!(live.historical_entries_dropped > 0);
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, None)
                .expect("single-entry bounded replay");
        assert_eq!(live, replay);
        assert_eq!(checkpoint, replay_checkpoint);
    }

    #[test]
    fn recorded_prefix_rebuilds_without_a_live_checkpoint() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca04, 4, 32);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = std::str::from_utf8(&stream).expect("stream is utf-8");
        let prefix_lines = text.lines().take(9).collect::<Vec<_>>();
        let expected_executions = u64::try_from(
            prefix_lines
                .iter()
                .skip(1)
                .filter(|line| {
                    matches!(
                        serde_json::from_str::<SmbCampaignStreamRecord>(line)
                            .expect("decode prefix record"),
                        SmbCampaignStreamRecord::Job(_)
                    )
                })
                .count(),
        )
        .expect("short prefix count fits u64");
        let prefix = format!("{}\n", prefix_lines.join("\n"));
        let (rebuilt, checkpoint) =
            replay_smb_campaign_checkpointed(&rom, prefix.as_bytes(), None, None)
                .expect("rebuild recorded prefix");
        assert!(expected_executions > 0);
        assert_eq!(rebuilt.executions_completed, expected_executions);
        assert!(rebuilt.executions_completed < 32);
        assert!(!checkpoint.entries.is_empty());
        assert!(checkpoint.entries.len() <= rebuilt.archive.entries.len());
        let report_ids = rebuilt
            .archive
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        assert!(
            checkpoint
                .entries
                .iter()
                .all(|snapshot| report_ids.contains(&snapshot.id))
        );
    }

    #[test]
    fn admit_alive_campaign_probes_nothing_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let probing = genesis_config(0x5eed_ca20, 4, 32);
        let mut probing_stream = Vec::new();
        let probed = run_smb_campaign(
            &rom,
            &probing,
            &SmbCampaignOrigin::Genesis,
            &mut probing_stream,
        )
        .expect("probing campaign");
        let mut config = genesis_config(0x5eed_ca20, 4, 32);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("admit-alive campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("\"retention_policy\":\"admit_alive\""));
        assert_eq!(live.probe_refused, 0);
        assert!(
            live.frames_emulated < probed.frames_emulated,
            "skipping the probe must emulate fewer frames: {} against {}",
            live.frames_emulated,
            probed.frames_emulated
        );
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay admit-alive");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn retiring_selector_records_counters_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca21, 4, 48);
        config.selector = crate::search::archive::SelectorPolicy::Retire(
            crate::search::archive::RetireThresholds {
                entry: 2,
                groups: vec![4, 8, 16],
            },
        );
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("retiring campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("room_cell_uniform_128_retire:2,4,8,16"));
        assert!(live.archive.selector.retirement.is_some());
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay retiring");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn energy_selector_records_counters_and_replays_byte_identically() {
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca22, 4, 48);
        config.selector = crate::search::archive::SelectorPolicy::Energy(
            crate::search::archive::RetireThresholds {
                entry: 2,
                groups: vec![4, 8, 16],
            },
        );
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("energy campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        let header = text.lines().next().expect("header");
        assert!(header.contains("room_cell_uniform_128_energy:2,4,8,16"));
        assert!(live.archive.selector.retirement.is_some());
        let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay energy");
        assert_eq!(
            serde_json::to_vec_pretty(&live).expect("serialize live"),
            serde_json::to_vec_pretty(&replayed).expect("serialize replayed")
        );
    }

    #[test]
    fn retiring_selector_reports_survive_a_seed_sweep() {
        let rom = synthetic_nrom();
        for seed in 0..24_u64 {
            let mut config = genesis_config(0x5eed_d000 + seed, 4, 64);
            config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
            config.selector = crate::search::archive::SelectorPolicy::Retire(
                crate::search::archive::RetireThresholds {
                    entry: 1,
                    groups: vec![2, 2, 3],
                },
            );
            let mut stream = Vec::new();
            let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
                .expect("reset-heavy campaign");
            let replayed = replay_smb_campaign(&rom, &stream, None).expect("replay reset-heavy");
            assert_eq!(live, replayed, "seed offset {seed} diverged");
        }
    }

    #[test]
    fn retention_and_selector_identifiers_round_trip() {
        use crate::search::archive::{
            RetentionPolicy, RetireThresholds, SelectorPolicy, retention_policy_from_identifier,
            retention_policy_identifier, selector_policy_identifier,
        };
        use crate::smb::archive::selector_policy_from_identifier;
        for policy in [
            RetentionPolicy::ProbeAtAdmission45,
            RetentionPolicy::AdmitAlive,
        ] {
            assert_eq!(
                retention_policy_from_identifier(retention_policy_identifier(policy))
                    .expect("retention round trip"),
                policy
            );
        }
        for policy in [
            SelectorPolicy::GroupUniform,
            SelectorPolicy::Retire(RetireThresholds {
                entry: 3,
                groups: vec![6, 12, 2],
            }),
            SelectorPolicy::Energy(RetireThresholds {
                entry: 3,
                groups: vec![6, 12, 2],
            }),
            SelectorPolicy::EnergyFrontier(RetireThresholds {
                entry: 3,
                groups: vec![6, 12, 2],
            }),
            SelectorPolicy::EnergyFrontierCheapest(RetireThresholds {
                entry: 3,
                groups: vec![6, 12, 2],
            }),
        ] {
            assert_eq!(
                selector_policy_from_identifier(&selector_policy_identifier(&policy))
                    .expect("selector round trip"),
                policy
            );
        }
        assert!(retention_policy_from_identifier("no_probe").is_err());
        assert!(selector_policy_from_identifier("room_cell_uniform_128_retire:3,6,12").is_err());
        assert!(selector_policy_from_identifier("room_cell_uniform_128_retire:3,6,12,0").is_err());
    }

    #[test]
    fn a_stream_recorded_under_another_rule_is_refused() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca10, 1, 4);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = String::from_utf8(stream).expect("stream is utf-8");
        for (from, to) in [
            ("room_cell_uniform_128", "concentrated_recency_128"),
            ("probe_at_admission_45", "probe_at_admission_45_snapback_16"),
            ("fewest_frames_in_level", "fewest_actions"),
            ("\"whole_tree\"", "\"frontier_shortest\""),
            ("nes_pressable_36", "frozen_nine_mask"),
            ("deterministic_window_1_per_worker_v3", "unknown_order_v9"),
        ] {
            let tampered = text.replacen(from, to, 1);
            assert!(
                replay_smb_campaign(&rom, tampered.as_bytes(), None).is_err(),
                "replay accepted {to}"
            );
        }
    }

    #[test]
    fn continuous_chord_tables_replay_with_recorded_versions() {
        let rom = synthetic_nrom();
        let config = SmbCampaignConfig {
            chord: derived_policy(),
            ..genesis_config(0x5eed_ca13, 1, 20)
        };
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("continuous chord-table campaign");
        let text = std::str::from_utf8(&stream).expect("stream text");
        assert!(
            text.lines()
                .next()
                .expect("header")
                .contains("\"chord_table\"")
        );
        assert!(text.contains("\"chord_table_before\""));
        assert!(text.contains("\"chord_table_after\""));
        let checkpoints = text
            .lines()
            .skip(1)
            .map(|line| {
                serde_json::from_str::<SmbCampaignStreamRecord>(line)
                    .expect("parse campaign record")
            })
            .filter_map(|record| match record {
                SmbCampaignStreamRecord::Job(job) => job.draw_table_before,
                SmbCampaignStreamRecord::Skip(skip) => skip.draw_table_before,
            })
            .collect::<Vec<_>>();
        assert!(
            checkpoints.windows(2).all(|pair| {
                pair[0].records <= pair[1].records
                    && pair[0].retained_successes <= pair[1].retained_successes
            }),
            "recorded table checkpoints must advance monotonically"
        );
        assert!(
            checkpoints.len() > 1,
            "multiple table versions must be recorded"
        );
        let replayed =
            replay_smb_campaign(&rom, &stream, None).expect("replay continuous chord tables");
        assert_eq!(live, replayed);
    }

    #[test]
    fn duplicate_check_requires_every_boundary() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca04, 2, 16);
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("live campaign");
        let text = String::from_utf8(stream.clone()).expect("stream is utf-8");
        if let Some(skip_line) = text.lines().find(|line| line.contains("\"skip\"")) {
            let tampered_line = skip_line.replace("\"mutation_seed\":", "\"mutation_seed\":9");
            let tampered = text.replace(skip_line, &tampered_line);
            let outcome = replay_smb_campaign(&rom, tampered.as_bytes(), None);
            assert!(outcome.is_err());
        }
        assert_eq!(
            live.duplicates_skipped,
            live.skips_per_worker.iter().sum::<u64>()
        );
    }

    #[test]
    fn archive_origin_round_trips_through_replay() {
        use super::{SmbCampaignCheckpoint, SmbSnapshotCheckpoint};
        use sha2::{Digest, Sha256};
        let rom = synthetic_nrom();
        let seed_config = genesis_config(0x5eed_ca05, 2, 12);
        let mut seed_stream = Vec::new();
        let (seed_campaign, seed_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &seed_config,
            &SmbCampaignOrigin::Genesis,
            &mut seed_stream,
            None,
        )
        .expect("seed campaign");
        let source = seed_campaign.archive.clone();
        let source_sha = "0000000000000000000000000000000000000000000000000000000000000000";
        let tree_config = genesis_config(0x5eed_ca06, 1, 16);
        let mut tree_stream = Vec::new();
        let tree_live = run_smb_campaign(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                file_sha256: source_sha.to_owned(),
                report: Box::new(source.clone()),
                checkpoint: None,
            },
            &mut tree_stream,
        )
        .expect("whole-tree campaign");
        let tree_replayed = replay_smb_campaign(&rom, &tree_stream, Some(&source))
            .expect("replay whole-tree campaign");
        assert_eq!(tree_live, tree_replayed);
        assert_eq!(tree_live.origin.kind, "archive");
        assert_eq!(tree_live.resume_policy, "whole_tree");
        let counts = tree_live.tree_import.expect("tree import counts");
        let source_retained = u64::try_from(source.entries.len() - 1).expect("count");
        assert_eq!(
            counts.imported
                + counts.duplicate
                + counts.rejected
                + counts.terminal
                + counts.over_limit,
            source_retained
        );
        assert!(counts.imported >= 1);
        let source_ids = source
            .entries
            .iter()
            .map(|entry| entry.id)
            .collect::<BTreeSet<_>>();
        let sparse_parents = source
            .entries
            .iter()
            .filter(|entry| {
                entry
                    .parent_id
                    .is_some_and(|parent| !source_ids.contains(&parent))
            })
            .count();
        assert_eq!(
            counts.rerooted,
            u64::try_from(sparse_parents).expect("sparse parent count")
        );
        assert_eq!(counts.checkpointed, 0);

        let checkpoint_bytes = seed_checkpoint.to_bytes().expect("encode checkpoint");
        let checkpoint = SmbCampaignCheckpoint {
            path: "seed-snapshots.bin".to_owned(),
            file_sha256: format!("{:x}", Sha256::digest(&checkpoint_bytes)),
            snapshots: SmbSnapshotCheckpoint::from_bytes(
                &checkpoint_bytes,
                super::SNAPSHOT_CHECKPOINT_FORMAT,
            )
            .expect("decode checkpoint"),
        };
        assert_eq!(checkpoint.snapshots.entries.len(), source.entries.len());
        let checkpointed_source_entries = u64::try_from(
            checkpoint
                .snapshots
                .entries
                .iter()
                .filter(|entry| entry.id != 0)
                .count(),
        )
        .expect("checkpoint entry count fits u64");
        let mut restored_stream = Vec::new();
        let (restored_live, restored_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &tree_config,
            &SmbCampaignOrigin::Archive {
                path: "seed-archive.json".to_owned(),
                file_sha256: source_sha.to_owned(),
                report: Box::new(source.clone()),
                checkpoint: Some(checkpoint.clone()),
            },
            &mut restored_stream,
            None,
        )
        .expect("checkpoint-restored campaign");
        assert_eq!(restored_live.archive, tree_live.archive);
        assert!(restored_live.bootstrap_frames < tree_live.bootstrap_frames);
        assert_eq!(
            restored_live.origin.checkpoint_sha256.as_deref(),
            Some(checkpoint.file_sha256.as_str())
        );
        let restored_counts = restored_live.tree_import.expect("restored counts");
        assert_eq!(restored_counts.checkpointed, checkpointed_source_entries);
        assert_eq!(
            (
                restored_counts.imported,
                restored_counts.duplicate,
                restored_counts.rejected
            ),
            (counts.imported, counts.duplicate, counts.rejected)
        );
        let (replayed_with, replayed_checkpoint) = replay_smb_campaign_checkpointed(
            &rom,
            &restored_stream,
            Some(&source),
            Some(&checkpoint),
        )
        .expect("replay with checkpoint");
        assert_eq!(replayed_with, restored_live);
        assert_eq!(replayed_checkpoint, restored_checkpoint);
        let moved_checkpoint = SmbCampaignCheckpoint {
            path: "moved/seed-snapshots.bin".to_owned(),
            ..checkpoint.clone()
        };
        let (replayed_moved, moved_snapshots) = replay_smb_campaign_checkpointed(
            &rom,
            &restored_stream,
            Some(&source),
            Some(&moved_checkpoint),
        )
        .expect("replay with moved archive checkpoint");
        assert_eq!(replayed_moved, restored_live);
        assert_eq!(moved_snapshots, restored_checkpoint);
        let (replayed_without, _) =
            replay_smb_campaign_checkpointed(&rom, &restored_stream, Some(&source), None)
                .expect("replay without checkpoint");
        assert_eq!(replayed_without.archive, restored_live.archive);
        let wrong = SmbCampaignCheckpoint {
            file_sha256: "00".to_owned(),
            ..checkpoint.clone()
        };
        assert!(
            replay_smb_campaign_checkpointed(&rom, &restored_stream, Some(&source), Some(&wrong))
                .is_err()
        );
        let imported_inputs: std::collections::BTreeSet<_> = tree_live
            .archive
            .entries
            .iter()
            .filter(|entry| entry.created_execution == 0)
            .map(|entry| entry.input.clone())
            .collect();
        let source_inputs: std::collections::BTreeSet<_> = source
            .entries
            .iter()
            .map(|entry| entry.input.clone())
            .collect();
        assert!(imported_inputs.is_subset(&source_inputs));
        assert!(imported_inputs.len() > 1);
    }

    #[test]
    fn the_progress_sidecar_changes_no_recorded_bytes() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca0e, 1, 24);
        let mut without = Vec::new();
        run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut without)
            .expect("campaign without a sidecar");
        let mut with = Vec::new();
        let mut sidecar = Vec::new();
        let observed = run_smb_campaign_with_progress(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut with,
            Some(&mut sidecar),
        )
        .expect("campaign with a sidecar");
        assert_eq!(without, with);
        assert!(!with.is_empty());
        assert!(
            std::str::from_utf8(&with)
                .expect("stream is utf-8")
                .lines()
                .all(|line| !line.contains("unix_time")),
            "no sidecar field reaches the recorded stream"
        );
        let records: Vec<SmbCampaignProgressRecord> = std::str::from_utf8(&sidecar)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.first().unwrap().executions, 1);
        let progress = records.last().unwrap();
        assert_eq!(progress.executions, observed.executions_completed);
        assert_eq!(progress.frames_emulated, observed.frames_emulated);
        assert!(progress.progress.is_some());
        assert!(progress.search_elapsed_millis.is_some());
        let replayed =
            replay_smb_campaign(&rom, &with, None).expect("sidecar run replays byte-exact");
        assert_eq!(replayed.stream_sha256, observed.stream_sha256);
        assert_eq!(replayed.archive, observed.archive);
    }
}
