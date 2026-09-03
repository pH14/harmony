// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB implementation of the generic campaign, see [`crate::search::campaign`].
//!
//! This module holds everything the generic coordinator asks a game for:
//! target construction and stepping, key and milestone decoding from work
//! RAM, the chord vocabularies a single action is drawn from, the mined
//! chord tables and the source rule that seeds them, and the identifier
//! strings recorded for those policies. The search layer owns the mutation
//! shape, the mixture odds, admission, selection, and the resume rule; none
//! of them is stated here.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::target::ExitKind;

use crate::{
    search::archive::RetentionPolicy,
    search::campaign::{
        CampaignActionResult, CampaignCandidate, CampaignCheckpoint, CampaignJobResult,
        CampaignModeReport, CampaignOrigin, CampaignProgressRecord, CampaignStreamHeader, Game,
        GamePolicies, SnapshotCheckpoint, postcard_result_sha256, replay_campaign_checkpointed,
        run_campaign_checkpointed,
    },
    search::draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
    search::empirical_steps::{
        EmpiricalStepCheckpoint, EmpiricalStepHashRule, EmpiricalStepParameters,
        EmpiricalStepTableRef, EmpiricalStepTables,
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

pub use crate::search::campaign::{
    CampaignAdmissionDecision as SmbCampaignAdmissionDecision,
    CampaignConfig as GenericCampaignConfig, CampaignJobRecord as SmbCampaignJobRecord,
    CampaignOriginRecord as SmbCampaignOriginRecord, CampaignSkipRecord as SmbCampaignSkipRecord,
    CampaignStreamRecord as SmbCampaignStreamRecord, RESUME_IDENTIFIER,
    TreeImportCounts as SmbTreeImportCounts, derive_worker_seed,
};

/// Stream format identifier written as the first line of every campaign stream.
pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-quicknes-campaign-stream-v2";

/// Format tag of the snapshot checkpoint file.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-quicknes-snapshot-checkpoint-v3";

/// Conservative portion of the global campaign budget reserved for the
/// bounded empirical chord tables and their short pending/recent windows.
const DRAW_STATE_MEMORY_RESERVE_BYTES: usize = 2 * 1024 * 1024;

/// Identifier recorded for the hold distribution, see
/// [`crate::smb::archive::sample_chord_from_masks`].
pub const DURATION_IDENTIFIER: &str = "stratified";

/// Stream-header field names SMB records its policies under. These are the
/// recorded names, so they are pinned by every stream already written.
pub const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
/// Header field naming the archive key policy.
pub const KEY_POLICY_FIELD: &str = "key_policy";
/// Header field naming the hold distribution.
pub const DURATION_POLICY_FIELD: &str = "duration_policy";
/// Header field naming the chord policy.
pub const CHORD_POLICY_FIELD: &str = "chord_policy";
/// Header field naming the cell-replacement rule.
pub const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
/// Header field naming the run's success predicate. Absent from legacy streams,
/// whose success predicate was unconditionally whole-game victory.
pub const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
/// Header field pinning the native emulator revision, build, options, and binary.
pub const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";

/// One recorded game policy of a campaign stream header.
///
/// # Errors
///
/// Returns an error when the header records no policy under `field`.
pub fn recorded_policy<'a>(
    policies: &'a GamePolicies,
    field: &str,
) -> Result<&'a str, Box<dyn Error>> {
    policies
        .get(field)
        .map(String::as_str)
        .ok_or_else(|| format!("campaign stream is missing the {field} policy").into())
}

/// The SMB campaign game context: the ROM and everything decoded from it.
pub struct SmbGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    identity: String,
    #[cfg(test)]
    loopback: bool,
}

fn quicknes_identity(core_sha256: &str) -> String {
    format!(
        "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;result_digest=postcard-1.1.3-sha256-hex-v2;sha256={core_sha256}",
        machine::quicknes::QUICKNES_REVISION,
        machine::quicknes::QUICKNES_BUILD,
        machine::quicknes::QUICKNES_OPTIONS,
    )
}

impl SmbGame {
    /// Build a context over the pinned QuickNES execution target.
    ///
    /// The binary identity and all fixed core options are written into the
    /// stream policy. Cross-core streams and checkpoints are rejected.
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            identity: quicknes_identity(core_sha256),
            #[cfg(test)]
            loopback: false,
        }
    }

    /// Build a context from the external core named by `HARMONY_QUICKNES_CORE`.
    ///
    /// # Errors
    ///
    /// Returns an error when the environment variable or core bytes cannot be read.
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
            loopback: true,
        }
    }

    /// Pinned emulator identity recorded in streams and fixture manifests.
    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }

    /// Snapshot checkpoint format for this emulator backend.
    #[must_use]
    pub fn snapshot_checkpoint_format(&self) -> &'static str {
        SNAPSHOT_CHECKPOINT_FORMAT
    }
}

/// Per-run SMB policies recorded in the stream header.
#[derive(Clone, Copy, Debug)]
pub struct SmbCampaignRun {
    /// Chord policy for this run.
    pub chord: SmbCampaignChordPolicy,
    /// Controller vocabulary for this run.
    pub vocabulary: SmbButtonVocabulary,
    /// Recorded terminal predicate. `None` denotes the legacy whole-game
    /// victory policy and preserves byte-exact replay of old streams.
    pub terminal: Option<SmbTerminalPredicate>,
}

/// Live state of the recorded chord-draw policy: the folded tables and, on
/// replay, the remembered versions recorded draws were made against.
pub struct SmbDrawState {
    tables: Option<EmpiricalStepTables<ButtonChord>>,
    versions: BTreeMap<u64, SmbChordTableVersion>,
}

/// Game-owned evidence accumulated outside the archive.
#[derive(Clone, Default)]
pub struct SmbCampaignEvidence {
    aggregate: SmbMilestones,
    watermark: SmbProgressWatermark,
    first_reached: SmbMilestoneTimes,
    first_inputs: SmbMilestoneInputs,
    champion_input: SmbInput,
    champion_milestones: SmbMilestones,
}

/// The SMB origin instantiation.
pub type SmbCampaignOrigin = CampaignOrigin<SmbGame>;
/// The SMB checkpoint instantiation.
pub type SmbCampaignCheckpoint = CampaignCheckpoint<SmbSnapshot>;
/// The SMB snapshot checkpoint instantiation.
pub type SmbSnapshotCheckpoint = SnapshotCheckpoint<SmbSnapshot>;
/// One SMB archive entry's snapshot.
pub type SmbSnapshotCheckpointEntry = crate::search::campaign::SnapshotCheckpointEntry<SmbSnapshot>;
/// The SMB stream header instantiation.
pub type SmbCampaignStreamHeader = CampaignStreamHeader<SmbChordTableHeader>;
/// The SMB campaign report instantiation.
pub type SmbCampaignModeReport = CampaignModeReport<ButtonChord, SmbArchiveReport>;
/// The SMB sidecar progress record instantiation.
pub type SmbCampaignProgressRecord = CampaignProgressRecord<SmbArchiveKey>;
/// One executed SMB action inside a job result.
pub(crate) type SmbCampaignActionResult = CampaignActionResult<SmbGame>;
/// Complete result of one executed SMB job.
pub(crate) type SmbCampaignJobResult = CampaignJobResult<SmbGame>;

/// Fixed configuration for one live campaign run.
pub struct SmbCampaignConfig {
    /// Campaign seed from which every worker stream derives.
    pub campaign_seed: u64,
    /// Number of worker threads.
    pub workers: u32,
    /// Number of executed jobs the campaign admits, unless stopped by wall budget.
    pub execution_budget: u64,
    /// Bounded clean-reset action horizon.
    pub action_limit: usize,
    /// Operator-supplied host name recorded in the header; never probed.
    pub host: String,
    /// Optional live-only wall cutoff that stops issuing new reservations.
    pub wall_budget: Option<std::time::Duration>,
    /// Archive entry bound for this run, recorded in the header and report.
    pub archive_entry_limit: usize,
    /// Reservations held ahead of ordered admission per worker, recorded in
    /// the header's schedule policy.
    pub reservations_per_worker: usize,
    /// Deterministic logical-memory budget for the live search structures.
    pub memory_budget_mib: Option<usize>,
    /// Live-only: materialize full archive inputs and snapshots at completion.
    pub materialize_final_artifacts: bool,
    /// Chord policy for this run, recorded in the header and report.
    pub chord: SmbCampaignChordPolicy,
    /// Controller vocabulary for this run, recorded in the header and report.
    pub vocabulary: SmbButtonVocabulary,
    /// Success predicate for this run, recorded in the stream header.
    pub terminal: SmbTerminalPredicate,
    /// Admission rule for this run, recorded in the header and report.
    pub retention: RetentionPolicy,
    /// Parent selector for this run, recorded in the header and report.
    pub selector: crate::search::archive::SelectorPolicy,
    /// Suffix shape for this run, recorded in the header and report.
    pub suffix: SuffixShape,
    /// Draw mixture for this run, recorded in the header and report.
    pub mixture: DrawMixture,
    /// Live-only: where the first winning input is written the moment it is
    /// admitted, before the in-flight jobs drain. Never recorded.
    pub victory_input_path: Option<std::path::PathBuf>,
}

impl SmbCampaignConfig {
    fn generic(&self) -> GenericCampaignConfig<SmbGame> {
        GenericCampaignConfig {
            suffix: self.suffix,
            mixture: self.mixture,
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
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

/// Mechanically recorded success predicate for an SMB campaign.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SmbTerminalPredicate {
    /// Stop only on ordinary whole-game victory.
    #[default]
    GameVictory,
    /// Stop when execution leaves the named zero-based world/level pair, or
    /// reaches ordinary whole-game victory.
    LevelTransition {
        /// Zero-based source world.
        world: u8,
        /// Zero-based source level.
        level: u8,
    },
}

impl SmbTerminalPredicate {
    /// Stable stream-header identifier for this predicate.
    #[must_use]
    pub fn identifier(self) -> String {
        match self {
            Self::GameVictory => "game_victory".to_owned(),
            Self::LevelTransition { world, level } => {
                format!("level_transition:{world},{level}")
            }
        }
    }

    /// Parse one stable stream-header identifier.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown predicate or malformed world/level.
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

    fn reached(self, target: &SmbTarget) -> Result<bool, Box<dyn Error>> {
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

/// Controller vocabulary a campaign draws button masks from, recorded in
/// the stream header per run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbButtonVocabulary {
    /// Masks written in the SMB-disassembly bit order the emulator reads
    /// reversed; kept so its recordings replay byte-exact.
    DownTenMask,
    /// The emulator-order chord set without B+direction chords; kept so its
    /// recordings replay byte-exact.
    NesDownTen,
    /// The emulator-order chord set with each direction alone and with A, B,
    /// and A+B; kept so its recordings replay byte-exact.
    NesRunThirteen,
    /// Every physically pressable chord: nine direction sets times none, A,
    /// B, and A+B.
    #[default]
    NesPressable,
}

impl SmbButtonVocabulary {
    /// Button masks this vocabulary draws from.
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

/// Header identifier for a controller vocabulary.
#[must_use]
pub fn button_vocabulary_identifier(vocabulary: SmbButtonVocabulary) -> &'static str {
    match vocabulary {
        SmbButtonVocabulary::DownTenMask => "down_ten_mask",
        SmbButtonVocabulary::NesDownTen => "nes_down_ten",
        SmbButtonVocabulary::NesRunThirteen => "nes_run_thirteen",
        SmbButtonVocabulary::NesPressable => "nes_pressable_36",
    }
}

/// Controller vocabulary named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known vocabulary.
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

/// A source archive's frontier identity: the shortest input among the entries
/// at the deepest recorded `(world, level, progress)`, earliest id on ties.
///
/// # Errors
///
/// Returns an error when the source archive has no retained entries.
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

/// Expand one mutation seed into its complete suffix.
///
/// The search layer owns the shape and the mixture odds; SMB supplies only
/// the two draws they compose — one chord from the run's vocabulary, and one
/// chord offered by the run's mined tables. Public so recorded-artifact
/// diagnostics can re-derive the actions a stream's jobs executed.
///
/// # Errors
///
/// Returns an error when a draw bound is invalid or a recorded chord policy
/// is missing its folded tables.
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

/// SMB-only filter preserving the existing deep-lineage extraction semantics.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordSourceFilter {
    /// Zero-based source world.
    pub world: u8,
    /// Zero-based source level.
    pub level: u8,
    /// Minimum retained source progress.
    pub minimum_progress: u16,
}

/// Which retained source entries seed the chord tables.
///
/// Live folding always consumes every retained input; this source rule only
/// selects the entries folded from a source archive at start-up. Serde stays
/// untagged so headers recorded before the all-levels rule existed, which
/// serialized the bare filter fields, still deserialize as `Level`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SmbChordSource {
    /// One `(world, level)` pair at or past a progress floor.
    Level(SmbChordSourceFilter),
    /// Every retained entry, so the rule carries no level knowledge.
    All(SmbChordSourceAll),
}

/// Marker for the level-neutral source rule.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordSourceAll {}

/// Complete registered derivation for one pair of mined chord tables.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableDerivation {
    /// Thin SMB source rule.
    pub source_filter: SmbChordSource,
    /// Game-neutral extraction, mixture, update, and hash parameters.
    pub parameters: EmpiricalStepParameters,
    /// Table-hash rule, bound to the policy identifier so historical
    /// recordings keep verifying under the rule they were made with.
    #[serde(default)]
    pub hash_rule: EmpiricalStepHashRule,
    /// What part of each retained input the fold consumes, bound to the
    /// policy identifier for the same reason.
    #[serde(default)]
    pub fold: ChordFoldSource,
}

/// What part of one retained input a chord fold consumes.
///
/// Folding the full input duplicates the whole prefix on every keep, so the
/// table and its fold cost grow with lineage depth; folding only the newly
/// drawn suffix keeps both proportional to the new presses.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum ChordFoldSource {
    /// The complete clean-reset input.
    #[default]
    FullInput,
    /// Only the actions past the retained entry's parent input.
    SuffixOnly,
}

/// Header provenance for a derived chord-table policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SmbChordTableHeader {
    /// SHA-256 of the named resume archive, or SHA-256 of empty bytes at genesis.
    pub source_sha256: String,
    /// Registered source filter and game-neutral fold parameters.
    pub derivation: SmbChordTableDerivation,
    /// Hash after folding the named source and before the first campaign draw.
    pub initial: EmpiricalStepCheckpoint,
}

/// Chord policy a campaign draws chords from, recorded in the stream header.
///
/// The feedback tables are the only draw source; retired policies survive
/// only as stream identifiers for recordings made before their removal, and
/// those streams no longer replay.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignChordPolicy {
    /// Derive recent and all-history tables from the recorded source, mixing
    /// their registered empirical weights into the biased half of each draw.
    DerivedHalf(SmbChordTableDerivation),
}

impl Default for SmbCampaignChordPolicy {
    /// The promoted level-neutral derivation with the registered
    /// head-to-head fold parameters.
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
            hash_rule: EmpiricalStepHashRule::IncrementalCompactHistory,
            fold: ChordFoldSource::SuffixOnly,
        })
    }
}

/// Header identifier for a chord policy.
#[must_use]
pub fn chord_policy_identifier(policy: SmbCampaignChordPolicy) -> String {
    match policy {
        SmbCampaignChordPolicy::DerivedHalf(derivation) => {
            let parameters = derivation.parameters;
            let prefix = match (derivation.fold, derivation.hash_rule) {
                (ChordFoldSource::SuffixOnly, EmpiricalStepHashRule::FullJson) => {
                    "chord_draw_recorded_52"
                }
                (ChordFoldSource::SuffixOnly, EmpiricalStepHashRule::IncrementalHistory) => {
                    "chord_draw_recorded_52"
                }
                (ChordFoldSource::SuffixOnly, EmpiricalStepHashRule::IncrementalCompactHistory) => {
                    "chord_draw_recorded_53"
                }
                (ChordFoldSource::FullInput, EmpiricalStepHashRule::FullJson) => {
                    "chord_draw_recorded_50"
                }
                (ChordFoldSource::FullInput, EmpiricalStepHashRule::IncrementalHistory) => {
                    "chord_draw_recorded_51"
                }
                (ChordFoldSource::FullInput, EmpiricalStepHashRule::IncrementalCompactHistory) => {
                    "chord_draw_recorded_54"
                }
            };
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
                "{prefix}:{source},{},{},{},{},{},{}",
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

/// Chord policy named by a recorded header identifier.
///
/// # Errors
///
/// Returns an error when the identifier names no known chord policy.
pub fn chord_policy_from_identifier(
    identifier: &str,
) -> Result<SmbCampaignChordPolicy, Box<dyn Error>> {
    let (fields, hash_rule, fold) =
        if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_50:") {
            (
                Some(rest),
                EmpiricalStepHashRule::FullJson,
                ChordFoldSource::FullInput,
            )
        } else if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_51:") {
            (
                Some(rest),
                EmpiricalStepHashRule::IncrementalHistory,
                ChordFoldSource::FullInput,
            )
        } else if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_52:") {
            (
                Some(rest),
                EmpiricalStepHashRule::IncrementalHistory,
                ChordFoldSource::SuffixOnly,
            )
        } else if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_53:") {
            (
                Some(rest),
                EmpiricalStepHashRule::IncrementalCompactHistory,
                ChordFoldSource::SuffixOnly,
            )
        } else if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_54:") {
            (
                Some(rest),
                EmpiricalStepHashRule::IncrementalCompactHistory,
                ChordFoldSource::FullInput,
            )
        } else {
            (
                None,
                EmpiricalStepHashRule::FullJson,
                ChordFoldSource::FullInput,
            )
        };
    if let Some(fields) = fields {
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
                hash_rule,
                fold,
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

/// Per-run execution policies a worker applies to every job.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SmbJobPolicies {
    pub(crate) max_actions: usize,
    pub(crate) retention: RetentionPolicy,
    pub(crate) terminal: SmbTerminalPredicate,
}

/// Execute one job: restore the origin snapshot, replay the actions that
/// lead from it to the parent, and apply the suffix exactly as the campaign
/// suffix loop does, collecting per-boundary candidates with worker-side
/// probe verdicts.
pub(crate) fn execute_job(
    target: &mut SmbTarget,
    origin_snapshot: &SmbSnapshot,
    replay: &[ButtonChord],
    parent_actions: usize,
    parent_milestones: SmbMilestones,
    suffix: &[ButtonChord],
    policies: SmbJobPolicies,
) -> Result<SmbCampaignJobResult, Box<dyn Error>> {
    target.restore(origin_snapshot)?;
    for action in replay {
        target.apply(action);
    }
    let mut milestones = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.is_dead() || policies.terminal.reached(target)? {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= policies.max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut milestones, target)?;
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = policies.terminal.reached(target)?;
        let failed = target.exit_kind() != ExitKind::Ok;
        // Recorded success is terminal: nothing past it is searched, so no
        // candidate is offered.
        let candidate = if dead || victory || failed {
            None
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot campaign suffix")?;
            let key = archive_key(&target.wram());
            let viable = match policies.retention {
                RetentionPolicy::ProbeAtAdmission45 => admission_is_viable(target, &snapshot)?,
                RetentionPolicy::AdmitAlive => true,
            };
            Some(CampaignCandidate {
                key,
                viable,
                snapshot,
            })
        };
        actions.push(CampaignActionResult {
            action: *action,
            observations,
            milestones,
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
    let mut tables =
        EmpiricalStepTables::with_hash_rule(derivation.parameters, derivation.hash_rule)?;
    let source_sha256 = match origin {
        None => format!("{:x}", Sha256::digest([])),
        Some((file_sha256, report)) => {
            let parent_len: BTreeMap<u64, usize> = match derivation.fold {
                ChordFoldSource::FullInput => BTreeMap::new(),
                ChordFoldSource::SuffixOnly => report
                    .entries
                    .iter()
                    .map(|entry| (entry.id, entry.input.actions.len()))
                    .collect(),
            };
            let mut source_pending = 0_u64;
            for entry in &report.entries {
                if source_filter_matches(derivation.source_filter, entry) {
                    let folded = match derivation.fold {
                        ChordFoldSource::FullInput => entry.input.actions.as_slice(),
                        ChordFoldSource::SuffixOnly => {
                            let prefix = entry
                                .parent_id
                                .and_then(|parent| parent_len.get(&parent).copied())
                                .unwrap_or(0);
                            entry.input.actions.get(prefix..).unwrap_or(&[])
                        }
                    };
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

/// One recorded table version, light enough to keep for every dispatch point:
/// the append-only history is shared with the live fold and named by length,
/// and only the bounded recent window is snapshotted (shared between versions
/// whose visible tables did not change).
struct SmbChordTableVersion {
    checkpoint: EmpiricalStepCheckpoint,
    history_len: usize,
    history_counts: Option<std::rc::Rc<BTreeMap<ButtonChord, usize>>>,
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
    if let Some(history_counts) = &version.history_counts {
        return Ok(Some(EmpiricalStepTableRef::from_counts(
            tables.parameters(),
            &version.recent,
            history_counts,
            version.history_len,
        )));
    }
    let history = tables
        .all_history()
        .get(..version.history_len)
        .ok_or("derived chord version names history the fold does not hold")?;
    Ok(Some(EmpiricalStepTableRef::from_parts(
        tables.parameters(),
        &version.recent,
        history,
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
        .and_then(|(_, last)| last.history_counts.as_ref().map(std::rc::Rc::clone))
        .or_else(|| {
            tables
                .compact_history()
                .map(|counts| std::rc::Rc::new(counts.clone()))
        });
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

impl Game for SmbGame {
    type Target = SmbTarget;
    type Action = ButtonChord;
    type Key = SmbArchiveKey;
    type Milestones = SmbMilestones;
    type Progress = SmbProgressWatermark;
    type Snapshot = SmbSnapshot;
    type Observations = SmbObservations;
    type Evidence = SmbCampaignEvidence;
    type ArchiveReport = SmbArchiveReport;
    type Run = SmbCampaignRun;
    type DrawState = SmbDrawState;
    type TableHeader = SmbChordTableHeader;

    fn stream_format(&self) -> &'static str {
        CAMPAIGN_STREAM_FORMAT
    }

    fn checkpoint_format(&self) -> &'static str {
        self.snapshot_checkpoint_format()
    }

    fn image_sha256(&self) -> String {
        format!("{:x}", Sha256::digest(&self.rom))
    }

    fn max_action_limit(&self) -> usize {
        crate::smb::archive::MAX_SMB_COMPLETION_ACTIONS
    }

    fn action_time_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn longest_action_time(&self) -> u64 {
        u64::from(crate::smb::archive::LONG_HOLD_FRAMES.1)
    }

    fn snapshot_memory_charge(snapshot: &SmbSnapshot) -> usize {
        snapshot.resident_memory_charge()
    }

    fn draw_state_memory_reserve_bytes(&self, _run: &SmbCampaignRun, _max_actions: usize) -> usize {
        DRAW_STATE_MEMORY_RESERVE_BYTES
    }

    fn draw_state_memory_bytes(&self, state: &SmbDrawState) -> usize {
        state
            .tables
            .as_ref()
            .map_or(0, EmpiricalStepTables::memory_bytes)
    }

    fn result_sha256(&self, result: &CampaignJobResult<Self>) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
    }

    fn policies(&self, run: &SmbCampaignRun) -> GamePolicies {
        let mut policies: GamePolicies = [
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

    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<SmbCampaignRun, Box<dyn Error>> {
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
            return Err("campaign stream emulator_backend is not this QuickNES build".into());
        }
        let run = SmbCampaignRun {
            chord: chord_policy_from_identifier(recorded(CHORD_POLICY_FIELD)?)?,
            vocabulary: button_vocabulary_from_identifier(recorded(CONTROLLER_VOCABULARY_FIELD)?)?,
            terminal: policies
                .get(TERMINAL_POLICY_FIELD)
                .map(|identifier| SmbTerminalPredicate::from_identifier(identifier))
                .transpose()?,
        };
        // A name SMB does not own would silently survive replay, so the
        // recorded set must be exactly the set this build writes.
        let unknown = policies
            .keys()
            .find(|field| !self.policies(&run).contains_key(field.as_str()));
        if let Some(field) = unknown {
            return Err(format!("campaign stream carries an unknown {field} policy").into());
        }
        Ok(run)
    }

    fn new_target(&self) -> Result<SmbTarget, String> {
        #[cfg(test)]
        if self.loopback {
            return SmbTarget::loopback_for_tests(&self.rom).map_err(|error| error.to_string());
        }
        SmbTarget::from_smb_rom_bytes_headless(&self.rom, &self.core_path, &self.core_sha256)
            .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut SmbTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut SmbTarget,
        snapshot: &SmbSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn frames_clocked(&self, target: &SmbTarget) -> u64 {
        target.frames_clocked()
    }

    fn apply_action(
        &self,
        target: &mut SmbTarget,
        action: &ButtonChord,
        milestones: &mut SmbMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(milestones, target)
    }

    fn is_terminal(&self, target: &SmbTarget) -> bool {
        target.is_dead() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        run: &SmbCampaignRun,
        target: &SmbTarget,
    ) -> Result<bool, Box<dyn Error>> {
        if self.is_terminal(target) {
            return Ok(true);
        }
        run.terminal.unwrap_or_default().reached(target)
    }

    fn snapshot(&self, target: &mut SmbTarget) -> Result<SmbSnapshot, Box<dyn Error>> {
        target.snapshot().ok_or_else(|| "failed to snapshot".into())
    }

    fn current_key(&self, target: &SmbTarget) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room(archive_key(&target.wram()), &target.wram())
    }

    fn complete_candidate_key(
        &self,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot,
    ) -> Result<SmbArchiveKey, Box<dyn Error>> {
        stamp_arrival_room_identity(key, snapshot.room_area())
    }

    fn execute_job(
        &self,
        run: &SmbCampaignRun,
        target: &mut SmbTarget,
        origin_snapshot: &SmbSnapshot,
        replay: &[ButtonChord],
        parent_actions: usize,
        parent_milestones: SmbMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<SmbCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            origin_snapshot,
            replay,
            parent_actions,
            parent_milestones,
            suffix,
            SmbJobPolicies {
                max_actions,
                retention,
                terminal: run.terminal.unwrap_or_default(),
            },
        )
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
        let SmbCampaignChordPolicy::DerivedHalf(derivation) = run.chord;
        let tables = state
            .tables
            .as_mut()
            .ok_or("derived chord policy has no folded tables")?;
        for (parent_actions, input) in retained {
            let folded = match derivation.fold {
                ChordFoldSource::FullInput => input,
                ChordFoldSource::SuffixOnly => input.get(*parent_actions..).unwrap_or(&[]),
            };
            tables.fold_retained(folded)?;
        }
        Ok(tables.finish_record()?)
    }

    fn retained_inputs_need_full(&self, run: &SmbCampaignRun) -> bool {
        let SmbCampaignChordPolicy::DerivedHalf(derivation) = run.chord;
        matches!(derivation.fold, ChordFoldSource::FullInput)
    }

    fn remember_draw_version(
        &self,
        state: &mut SmbDrawState,
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        let SmbDrawState { tables, versions } = state;
        remember_chord_version(tables.as_ref(), required, versions)
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
        target: &SmbTarget,
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
        action: &SmbCampaignActionResult,
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

/// Run one live campaign, writing the stream as it goes.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`run_smb_campaign_checkpointed`].
pub fn run_smb_campaign(
    game: &SmbGame,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_with_progress(game, config, origin, stream, None)
}

/// Run a campaign, optionally emitting periodic progress lines to a sidecar.
///
/// The sidecar is pure observation: it reads archive state that is already
/// settled, consumes no randomness, and writes to a sink separate from the
/// recorded stream. A run with a sidecar and the same run without one record
/// byte-identical streams and archives.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`run_smb_campaign_checkpointed`].
pub fn run_smb_campaign_with_progress(
    game: &SmbGame,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_checkpointed(game, config, origin, stream, progress).map(|(report, _)| report)
}

/// Run a campaign, also returning every retained entry's snapshot so a later
/// whole-tree resume can restore the population instead of re-emulating it.
///
/// # Errors
///
/// Returns an error when the origin is unusable, a worker fails, emulation or
/// snapshotting fails, or the stream cannot be written.
pub fn run_smb_campaign_checkpointed(
    game: &SmbGame,
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

/// Replay a recorded campaign stream serially and rebuild its report.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`replay_smb_campaign_checkpointed`].
pub fn replay_smb_campaign(
    game: &SmbGame,
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    replay_smb_campaign_checkpointed(game, stream_bytes, origin_report, None)
        .map(|(report, _)| report)
}

/// Replay a recorded campaign, also returning the rebuilt snapshot checkpoint.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the origin does not match
/// the header, or any recomputed value differs from the recorded one.
pub fn replay_smb_campaign_checkpointed(
    game: &SmbGame,
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
    origin_checkpoint: Option<&SmbCampaignCheckpoint>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
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
        SmbCampaignAdmissionDecision, SmbCampaignCheckpoint, SmbCampaignChordPolicy,
        SmbCampaignConfig, SmbCampaignJobResult, SmbCampaignOrigin, SmbCampaignProgressRecord,
        SmbCampaignRun, SmbCampaignStreamRecord, SmbChordTableVersion, SmbGame,
        SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry, SmbTerminalPredicate, SuffixShape,
        chord_policy_from_identifier, chord_policy_identifier, derive_suffix, derive_worker_seed,
        execute_job, remember_chord_version, source_batch_ready,
    };
    use crate::search::campaign::{
        CAMPAIGN_SCHEDULE_IDENTITY, CoordinatorCore, DEFAULT_ADMISSION_RESERVATIONS_PER_WORKER,
        Game,
    };
    use crate::search::empirical_steps::{EmpiricalStepHashRule, EmpiricalStepTables};
    use crate::{
        search::empirical_steps::EmpiricalStepParameters,
        smb::archive::{SmbArchiveEntryReport, SmbArchiveKey, SmbArchiveReport},
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
            let empty = crate::search::empirical_steps::EmpiricalStepTableRef::from_parts(
                empty_table_parameters(),
                &[],
                &[],
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
            hash_rule: EmpiricalStepHashRule::default(),
            fold: super::ChordFoldSource::default(),
        })
    }

    #[test]
    fn smb_memory_and_retained_input_contracts_are_exact() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("target");
        target.reset();
        let snapshot = target.snapshot().expect("snapshot");
        let mut full = derived_policy();
        let SmbCampaignChordPolicy::DerivedHalf(mut derivation) = full;
        derivation.fold = super::ChordFoldSource::FullInput;
        full = SmbCampaignChordPolicy::DerivedHalf(derivation);
        let full_run = SmbCampaignRun {
            chord: full,
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::default()),
        };
        derivation.fold = super::ChordFoldSource::SuffixOnly;
        let suffix_run = SmbCampaignRun {
            chord: SmbCampaignChordPolicy::DerivedHalf(derivation),
            ..full_run
        };

        assert_eq!(
            game.draw_state_memory_reserve_bytes(&full_run, 96),
            2 * 1024 * 1024
        );
        assert_eq!(
            <SmbGame as Game>::snapshot_memory_charge(&snapshot),
            snapshot.resident_memory_charge()
        );
        assert!(game.retained_inputs_need_full(&full_run));
        assert!(!game.retained_inputs_need_full(&suffix_run));
        assert!(!source_batch_ready(1, 2));
        assert!(source_batch_ready(2, 2));
        assert!(source_batch_ready(3, 2));

        let (mut draw_state, _) = game
            .initial_draw_state(&full_run, None)
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
        let mut tables =
            EmpiricalStepTables::with_hash_rule(derivation.parameters, derivation.hash_rule)
                .expect("tables");
        let chord = ButtonChord::new(0x01, 4);
        tables.fold_retained(&[chord]).expect("fold chord");
        tables.finish_record().expect("finish record");
        tables.flush().expect("flush table");
        let checkpoint = tables.checkpoint().expect("checkpoint");
        let history_len = tables.history_len();
        let required = BTreeSet::from([tables.records()]);

        let sentinel = Rc::new(vec![ButtonChord::new(0x80, 7)]);
        let exact = SmbChordTableVersion {
            checkpoint: checkpoint.clone(),
            history_len,
            history_counts: None,
            recent: Rc::clone(&sentinel),
        };
        let mut versions = BTreeMap::from([(0, exact)]);
        remember_chord_version(Some(&tables), &required, &mut versions)
            .expect("remember exact version");
        assert!(Rc::ptr_eq(&versions[&tables.records()].recent, &sentinel));

        let wrong_len = SmbChordTableVersion {
            checkpoint: checkpoint.clone(),
            history_len: history_len.saturating_add(1),
            history_counts: None,
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
            history_counts: None,
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
        assert!(chord_policy_from_identifier("chord_draw_recorded_50").is_err());
        assert!(chord_policy_from_identifier("chord_draw_recorded_50:0").is_err());

        let SmbCampaignChordPolicy::DerivedHalf(mut derivation) = policy;
        derivation.hash_rule = EmpiricalStepHashRule::IncrementalHistory;
        let incremental = SmbCampaignChordPolicy::DerivedHalf(derivation);
        let identifier = chord_policy_identifier(incremental);
        assert!(identifier.starts_with("chord_draw_recorded_51:"));
        assert_eq!(
            chord_policy_from_identifier(&identifier).expect("parse incremental policy"),
            incremental
        );

        derivation.source_filter = super::SmbChordSource::All(super::SmbChordSourceAll {});
        let all_levels = SmbCampaignChordPolicy::DerivedHalf(derivation);
        let identifier = chord_policy_identifier(all_levels);
        assert!(identifier.starts_with("chord_draw_recorded_51:all,"));
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
    fn job_execution_is_pure_across_target_instances() {
        let rom = synthetic_nrom();
        let mut first = SmbTarget::loopback_for_tests(&rom).expect("load first target");
        let mut second = SmbTarget::loopback_for_tests(&rom).expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let empty = crate::search::empirical_steps::EmpiricalStepTableRef::from_parts(
            empty_table_parameters(),
            &[],
            &[],
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
        // Disturb the first instance so the job must depend on the snapshot alone.
        first.apply(&ButtonChord::new(0x02, 30));
        let policies = super::SmbJobPolicies {
            max_actions: 96,
            retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            terminal: super::SmbTerminalPredicate::GameVictory,
        };
        let on_first = execute_job(
            &mut first,
            &snapshot,
            &[],
            1,
            SmbMilestones::default(),
            &suffix,
            policies,
        )
        .expect("execute job on first instance");
        let on_second = execute_job(
            &mut second,
            &snapshot,
            &[],
            1,
            SmbMilestones::default(),
            &suffix,
            policies,
        )
        .expect("execute job on second instance");
        assert_eq!(on_first, on_second);
    }

    #[test]
    fn a_job_from_a_won_snapshot_executes_nothing() {
        let rom = synthetic_nrom();
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        target.reset();
        target.poke_wram(0x0770, 2);
        target.poke_wram(0x075f, 7);
        let won = target.snapshot().expect("snapshot won state");
        let result = execute_job(
            &mut target,
            &won,
            &[],
            0,
            SmbMilestones::default(),
            &[ButtonChord::new(0x01, 4)],
            super::SmbJobPolicies {
                max_actions: 96,
                retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
                terminal: super::SmbTerminalPredicate::GameVictory,
            },
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
                .contains("\"schedule_policy\":\"deterministic_window_1_per_worker_v2\"")
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
        // Window 64 in the historical namespace is the legacy identifier
        // verbatim, so a live run at that window must record its own.
        assert!(
            recorded
                .lines()
                .next()
                .expect("stream header")
                .contains("\"schedule_policy\":\"deterministic_window_64_per_worker_v2\""),
            "unexpected header: {}",
            recorded.lines().next().unwrap_or_default()
        );
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, recorded.as_bytes(), None, None)
                .expect("window-64 stream replays off the windowed path");
        assert_eq!(live, replay);
        assert_eq!(live_checkpoint, replay_checkpoint);
        let legacy_tagged = recorded.replacen(
            "deterministic_window_64_per_worker_v2",
            "deterministic_window_64_per_worker_v1",
            1,
        );
        // Rewriting only the namespace claims a run recorded before the
        // budget maintenance was corrected, which this stream is not.
        assert!(
            replay_smb_campaign_checkpointed(&rom, legacy_tagged.as_bytes(), None, None).is_err(),
            "the legacy path must not silently accept a live window-64 stream"
        );
    }

    #[test]
    fn a_budgeted_stream_in_a_historical_namespace_is_refused_rather_than_replayed() {
        // Budgeted runs recorded before the maintenance correction enforced
        // the budget at other stream positions and counted CLOCK work
        // differently, so which entries survive differs. Replay must say so
        // instead of running and diverging.
        let rom = synthetic_nrom();
        let mut config = genesis_config(0x5eed_ca44, 2, 128);
        config.retention = crate::search::archive::RetentionPolicy::AdmitAlive;
        config.memory_budget_mib = Some(4);
        config.archive_entry_limit = 1;
        let mut stream = Vec::new();
        let live = run_smb_campaign(&rom, &config, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("budgeted live campaign");
        assert!(
            live.liveness_anchor_reactivations > 0,
            "the budget must bind for this stream to be maintenance-sensitive"
        );
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        replay_smb_campaign(&rom, recorded.as_bytes(), None).expect("its own namespace replays");

        let refused = "campaign stream recorded a memory budget under superseded maintenance \
                       and cannot be replayed";
        for historical in [
            recorded.replacen("_per_worker_v2", "_per_worker_v1", 1),
            recorded.replacen(
                "\"schedule_policy\":\"deterministic_window_1_per_worker_v2\"",
                "\"schedule_policy\":\"deterministic_window_64_per_worker_v1\"",
                1,
            ),
            recorded.replacen(
                "\"schedule_policy\":\"deterministic_window_1_per_worker_v2\",",
                "",
                1,
            ),
        ] {
            let error = replay_smb_campaign(&rom, historical.as_bytes(), None)
                .expect_err("a budgeted historical stream is refused");
            assert_eq!(error.to_string(), refused);
        }

        // The same rewrite stays replayable without a budget, because the
        // maintenance step does nothing there.
        let unbudgeted = genesis_config(0x5eed_ca45, 2, 32);
        let mut stream = Vec::new();
        run_smb_campaign(&rom, &unbudgeted, &SmbCampaignOrigin::Genesis, &mut stream)
            .expect("unbudgeted live campaign");
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        replay_smb_campaign(
            &rom,
            recorded
                .replacen("_per_worker_v2", "_per_worker_v1", 1)
                .as_bytes(),
            None,
        )
        .expect("an unbudgeted historical stream still replays");
    }

    #[test]
    fn legacy_stream_omits_new_evaluator_payloads_on_replay() {
        let rom = synthetic_nrom();
        let config = genesis_config(0x5eed_ca22, 1, 4);
        let mut stream = Vec::new();
        let (live, live_checkpoint) = run_smb_campaign_checkpointed(
            &rom,
            &config,
            &SmbCampaignOrigin::Genesis,
            &mut stream,
            None,
        )
        .expect("new campaign");
        let recorded = String::from_utf8(stream).expect("stream is utf-8");
        let historical = recorded.replacen(
            "\"schedule_policy\":\"deterministic_window_1_per_worker_v2\"",
            "\"schedule_policy\":\"deterministic_window_64_per_worker_v1\"",
            1,
        );
        let (historical_replay, historical_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, historical.as_bytes(), None, None)
                .expect("deterministic legacy policy replays");
        let (historical_replay_again, historical_checkpoint_again) =
            replay_smb_campaign_checkpointed(&rom, historical.as_bytes(), None, None)
                .expect("legacy policy replays deterministically");
        assert_eq!(historical_replay, historical_replay_again);
        assert_eq!(historical_checkpoint, historical_checkpoint_again);
        // Replacing only the schedule tag changes the stream digest by
        // design; the archive and checkpoint must remain identical.
        assert_eq!(historical_replay.archive, live.archive);
        assert_eq!(historical_checkpoint, live_checkpoint);
        assert_eq!(
            historical_replay.schedule_identity,
            CAMPAIGN_SCHEDULE_IDENTITY
        );
        let legacy = recorded
            .replacen(
                "\"schedule_policy\":\"deterministic_window_1_per_worker_v2\",",
                "",
                1,
            )
            .replacen(
                "\"progress_policy\":\"mechanical_watermark_bounded_1024_v2\",",
                "",
                1,
            )
            .replacen("\"terminal_policy\":\"game_victory\",", "", 1);
        let replay = replay_smb_campaign(&rom, legacy.as_bytes(), None)
            .expect("legacy evaluator stream replays");
        assert!(
            replay
                .schedule_identity
                .starts_with("the live schedule is not derivable")
        );
        assert!(
            !replay
                .game_policies
                .contains_key(super::TERMINAL_POLICY_FIELD)
        );
        let curve = serde_json::to_string(&replay.archive.progress_curve)
            .expect("serialize legacy progress curve");
        assert!(!curve.contains("\"progress\""));
    }

    #[test]
    fn snapshot_root_rejects_malformed_checkpoint_shapes() {
        use sha2::{Digest, Sha256};

        let rom = synthetic_nrom();
        let game = test_game(&rom);
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
            let run = SmbCampaignRun {
                chord: SmbCampaignChordPolicy::default(),
                vocabulary: SmbButtonVocabulary::default(),
                terminal: Some(SmbTerminalPredicate::default()),
            };
            let mut core = CoordinatorCore::new(&game, &run, 96, 32_768, None);
            assert!(
                core.bootstrap_snapshot_root(&game, &run, &mut target, &malformed)
                    .is_err()
            );
        }
    }

    #[test]
    fn admission_counts_a_victory_and_keeps_the_first_winning_input() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("load target");
        let run = SmbCampaignRun {
            chord: SmbCampaignChordPolicy::default(),
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::default()),
        };
        let mut core = CoordinatorCore::new(&game, &run, 96, 32_768, None);
        core.bootstrap(&game, &mut target).expect("retain genesis");
        let winning = ButtonChord::new(0x81, 7);
        let result = SmbCampaignJobResult {
            actions: vec![SmbCampaignActionResult {
                action: winning,
                observations: Vec::new(),
                milestones: SmbMilestones::default(),
                dead: false,
                victory: true,
                failed: false,
                candidate: None,
            }],
        };
        let winning_action = result.actions[0].clone();
        let (sequence, decisions) = core.admit_job(&game, 0, result).expect("admit winning job");
        assert_eq!(sequence, 1);
        assert_eq!(decisions, vec![SmbCampaignAdmissionDecision::Victory]);
        assert_eq!(core.victories, 1);
        assert_eq!(
            core.victory_input,
            Some(SmbInput {
                actions: vec![winning]
            })
        );
        let later = SmbCampaignJobResult {
            actions: vec![SmbCampaignActionResult {
                action: ButtonChord::new(0x01, 9),
                ..winning_action
            }],
        };
        core.admit_job(&game, 0, later)
            .expect("admit a second winning job");
        assert_eq!(core.victories, 2);
        assert_eq!(
            core.victory_input,
            Some(SmbInput {
                actions: vec![winning]
            })
        );
        let (report, _) = core.into_archive_report_and_snapshots(&game, 0, true);
        assert_eq!(report.entries.len(), 1, "a won lineage is not extended");
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
        // This bounded setup drives the active population to the action limit
        // while the 64-entry budget continues admitting replacements.
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
        // Skips account a selection without inserting, so this run exercises
        // the budget maintenance the coordinator spends outside admission.
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
        assert!(live.liveness_anchor_reactivations > 0);
        let (replay, replay_checkpoint) =
            replay_smb_campaign_checkpointed(&rom, &stream, None, None)
                .expect("single-entry bounded replay");
        assert!(live.liveness_anchor_reactivations > 0);
        let mut live_without_test_evidence = live;
        let mut replay_without_test_evidence = replay;
        live_without_test_evidence.liveness_anchor_reactivations = 0;
        replay_without_test_evidence.liveness_anchor_reactivations = 0;
        assert_eq!(live_without_test_evidence, replay_without_test_evidence);
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
        // The scale replays diverged only in end-state retirement counters,
        // so this sweeps seeds under reset-heavy thresholds until a live
        // report and its replay disagree.
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
            ("deterministic_window_1_per_worker_v2", "unknown_order_v9"),
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
            // Cross the reservation window so a later job must draw against
            // a table updated by earlier retained successes.
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
        // Tampering with a recorded skip must fail replay loudly rather than
        // silently reproducing the counters.
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
        // The re-emulated and checkpoint-restored campaigns below are two
        // independent live runs whose archives are compared for equality.
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

        // Restoring the compact source breeding population from its snapshot
        // checkpoint reaches the same archive as re-emulating it, records the
        // checkpoint in the header, and replays with or without the file.
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
    fn whole_tree_import_rebuilds_the_source_population() {
        let rom = synthetic_nrom();
        let game = test_game(&rom);
        let mut target = SmbTarget::loopback_for_tests(&rom).expect("target");
        let chord = |mask: u8| ButtonChord::new(mask, 4);
        let entry =
            |id: u64, parent: Option<u64>, actions: Vec<ButtonChord>| SmbArchiveEntryReport {
                id,
                parent_id: parent,
                created_execution: 0,
                input: SmbInput { actions },
                key: SmbArchiveKey {
                    world: 0,
                    level: 0,
                    progress: 0,
                    player_y_bucket: 0,
                    state_fingerprint: 0,
                    room_x_bucket: 0,
                    time_bucket: 0,
                    room: [0; 3],
                },
                milestones: SmbMilestones::default(),
                selector: None,
            };
        // A trunk of two, a branch off the trunk, a child whose recorded
        // parent is absent from the report, and one past the action limit.
        let entries = vec![
            entry(0, None, Vec::new()),
            entry(1, Some(0), vec![chord(0x01)]),
            entry(2, Some(1), vec![chord(0x01), chord(0x02)]),
            entry(3, Some(1), vec![chord(0x01), chord(0x80)]),
            entry(9, Some(7), vec![chord(0x01), chord(0x02), chord(0x40)]),
            entry(10, Some(2), vec![chord(0x01); 5]),
        ];
        let source = SmbArchiveReport {
            seed: 0,
            executions: 0,
            milestones: SmbMilestones::default(),
            progress_watermark: crate::smb::target::SmbProgressWatermark::default(),
            first_reached: crate::smb::target::SmbMilestoneTimes::default(),
            first_inputs: crate::smb::target::SmbMilestoneInputs::default(),
            champion_input: SmbInput::default(),
            entries,
            progress_curve: Vec::new(),
            retained: 0,
            rejected: 0,
            deaths: 0,
            selector: crate::search::archive::SelectorAccounting::default(),
        };
        let run = SmbCampaignRun {
            chord: SmbCampaignChordPolicy::default(),
            vocabulary: SmbButtonVocabulary::default(),
            terminal: Some(SmbTerminalPredicate::default()),
        };
        let mut core = CoordinatorCore::new(&game, &run, 4, 32_768, None);
        // The report stores each entry's actions past its parent and rebuilds
        // the full inputs on load.
        let suffix_json = serde_json::to_string(&source).expect("serialize");
        assert!(suffix_json.contains("\"input_suffix\""));
        let rebuilt: SmbArchiveReport = serde_json::from_str(&suffix_json).expect("load suffix");
        assert_eq!(rebuilt, source);
        let counts = core
            .import_tree(&game, &mut target, &source, None)
            .expect("import");
        assert_eq!(counts.over_limit, 1);
        assert_eq!(counts.rerooted, 1);
        assert_eq!(counts.terminal, 0);
        assert_eq!(counts.imported + counts.rejected, 4);
        let reports = core.archive.take_entry_reports_and_snapshots().0;
        assert_eq!(reports[0].input.actions.len(), 0);
        for report in &reports[1..] {
            let parent = usize::try_from(report.parent_id.expect("parent")).expect("index");
            let parent_input = &reports[parent].input.actions;
            assert_eq!(
                report.input.actions.get(..parent_input.len()),
                Some(parent_input.as_slice())
            );
            assert_eq!(report.created_execution, 0);
        }
        assert_eq!(
            reports.len(),
            usize::try_from(counts.imported).expect("imported count") + 1
        );
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
        // The deterministic window schedule means the sidecar run must record
        // byte-identical stream bytes.
        assert_eq!(without, with);
        assert!(!with.is_empty());
        assert!(
            std::str::from_utf8(&with)
                .expect("stream is utf-8")
                .lines()
                .all(|line| !line.contains("unix_time")),
            "no sidecar field reaches the recorded stream"
        );
        let progress: SmbCampaignProgressRecord =
            serde_json::from_slice(&sidecar).expect("the short run emits one progress record");
        assert_eq!(progress.executions, 1);
        assert!(progress.frames_emulated > 0);
        let replayed =
            replay_smb_campaign(&rom, &with, None).expect("sidecar run replays byte-exact");
        assert_eq!(replayed.stream_sha256, observed.stream_sha256);
        assert_eq!(replayed.archive, observed.archive);
    }
}
