// SPDX-License-Identifier: AGPL-3.0-or-later

//! SMB implementation of the generic campaign, see [`crate::search::campaign`].
//!
//! This module holds everything the generic coordinator asks a game for:
//! target construction and stepping, key and milestone decoding from work
//! RAM, chord vocabularies and the recorded chord-table policy, and the
//! identifier strings recorded for the game-owned policies.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    io::Write,
    num::NonZeroUsize,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::search::rand::RomuDuoJrRand;
use crate::target::ExitKind;

use crate::{
    search::archive::RetentionPolicy,
    search::campaign::{
        CampaignActionResult, CampaignCandidate, CampaignCheckpoint, CampaignJobResult,
        CampaignModeReport, CampaignOrigin, CampaignProgressRecord, CampaignStreamHeader, Game,
        GameIdentifiers, SnapshotCheckpoint, replay_campaign_checkpointed,
        run_campaign_checkpointed,
    },
    search::empirical_steps::{
        EmpiricalStepCheckpoint, EmpiricalStepHashRule, EmpiricalStepParameters,
        EmpiricalStepTableRef, EmpiricalStepTables,
    },
    smb::archive::{
        DOWN_TEN_BUTTON_MASKS, KEY_POLICY_IDENTIFIER, REPLACEMENT_IDENTIFIER, SmbArchiveKey,
        SmbArchiveReport, admission_is_viable, archive_key, chord_time, merge_action_milestones,
        merge_milestones, merge_progress_watermark, milestone_key, stamp_arrival_room,
        update_first_inputs,
    },
    smb::target::{
        ButtonChord, SmbInput, SmbMilestoneInputs, SmbMilestoneTimes, SmbMilestones,
        SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
    },
    target::Target,
};

pub use crate::search::campaign::{
    CampaignAdmissionDecision as SmbCampaignAdmissionDecision,
    CampaignConfig as GenericCampaignConfig, CampaignJobRecord as SmbCampaignJobRecord,
    CampaignOriginRecord as SmbCampaignOriginRecord, CampaignSkipRecord as SmbCampaignSkipRecord,
    CampaignStreamRecord as SmbCampaignStreamRecord, LIVE_CHECKPOINT_INTERVAL, RESUME_IDENTIFIER,
    TreeImportCounts as SmbTreeImportCounts, derive_worker_seed,
};

/// Stream format identifier written as the first line of every campaign stream.
pub const CAMPAIGN_STREAM_FORMAT: &str = "smb-campaign-stream-v1";

/// Format tag of the snapshot checkpoint file.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "smb-snapshot-checkpoint-v1";

/// Identifier recorded for the suffix shape: one action, or two at
/// one-in-four odds.
pub const SUFFIX_IDENTIFIER: &str = "one_or_two";

/// Identifier recorded for the hold distribution, see
/// [`crate::smb::archive::sample_chord_from_masks`].
pub const DURATION_IDENTIFIER: &str = "stratified";

/// The SMB campaign game context: the ROM and everything decoded from it.
pub struct SmbGame {
    rom: Vec<u8>,
}

impl SmbGame {
    /// Build the context over one ROM image.
    #[must_use]
    pub fn new(rom: &[u8]) -> Self {
        Self { rom: rom.to_vec() }
    }
}

/// Per-run SMB policies recorded in the stream header.
#[derive(Clone, Copy, Debug)]
pub struct SmbCampaignRun {
    /// Chord policy for this run.
    pub chord: SmbCampaignChordPolicy,
    /// Controller vocabulary for this run.
    pub vocabulary: SmbButtonVocabulary,
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
    /// Chord policy for this run, recorded in the header and report.
    pub chord: SmbCampaignChordPolicy,
    /// Controller vocabulary for this run, recorded in the header and report.
    pub vocabulary: SmbButtonVocabulary,
    /// Admission rule for this run, recorded in the header and report.
    pub retention: RetentionPolicy,
    /// Parent selector for this run, recorded in the header and report.
    pub selector: crate::search::archive::SelectorPolicy,
    /// Live-only: where the first winning input is written the moment it is
    /// admitted, before the in-flight jobs drain. Never recorded.
    pub victory_input_path: Option<std::path::PathBuf>,
    /// Live-only: directory receiving a whole-tree checkpoint every
    /// [`LIVE_CHECKPOINT_INTERVAL`] executions. Never recorded.
    pub checkpoint_dir: Option<std::path::PathBuf>,
}

impl SmbCampaignConfig {
    fn generic(&self) -> GenericCampaignConfig<SmbGame> {
        GenericCampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            archive_entry_limit: self.archive_entry_limit,
            run: SmbCampaignRun {
                chord: self.chord,
                vocabulary: self.vocabulary,
            },
            retention: self.retention,
            selector: self.selector.clone(),
            victory_input_path: self.victory_input_path.clone(),
            checkpoint_dir: self.checkpoint_dir.clone(),
        }
    }
}

/// Controller vocabulary a campaign draws button masks from, recorded in
/// the stream header per run.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbButtonVocabulary {
    /// Masks written in the SMB-disassembly bit order the emulator reads
    /// reversed; kept so its recordings replay byte-exact.
    DownTenMask,
    /// The intended chord set in the emulator's bit order.
    #[default]
    NesDownTen,
}

impl SmbButtonVocabulary {
    /// Button masks this vocabulary draws from.
    #[must_use]
    pub fn masks(self) -> &'static [u8; 10] {
        match self {
            Self::DownTenMask => &DOWN_TEN_BUTTON_MASKS,
            Self::NesDownTen => &crate::smb::archive::NES_DOWN_TEN_BUTTON_MASKS,
        }
    }
}

/// Header identifier for a controller vocabulary.
#[must_use]
pub fn button_vocabulary_identifier(vocabulary: SmbButtonVocabulary) -> &'static str {
    match vocabulary {
        SmbButtonVocabulary::DownTenMask => "down_ten_mask",
        SmbButtonVocabulary::NesDownTen => "nes_down_ten",
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
/// The suffix is sampled from a fresh RNG seeded with the mutation seed
/// alone, so a job is a pure function of (parent snapshot, mutation seed).
/// Under a recorded chord policy each chord comes from the table at even
/// odds with the uniform draw. Public so recorded-artifact diagnostics can
/// re-derive the actions a stream's jobs executed.
///
/// # Errors
///
/// Returns an error when a draw bound is invalid or a recorded chord policy
/// is missing its folded tables.
pub fn derive_suffix(
    mutation_seed: u64,
    chord_policy: SmbCampaignChordPolicy,
    vocabulary: SmbButtonVocabulary,
    chord_tables: Option<EmpiricalStepTableRef<'_, ButtonChord>>,
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    let mut rand = RomuDuoJrRand::with_seed(mutation_seed);
    let suffix_len = if rand.below(NonZeroUsize::new(4).ok_or("invalid suffix odds")?) == 0 {
        2
    } else {
        1
    };
    let mut suffix = Vec::with_capacity(suffix_len);
    for _ in 0..suffix_len {
        let recorded = chord_policy != SmbCampaignChordPolicy::Uniform
            && rand.below(NonZeroUsize::new(2).ok_or("invalid chord odds")?) == 0;
        if recorded {
            let mined = match chord_policy {
                SmbCampaignChordPolicy::Uniform => None,
                SmbCampaignChordPolicy::DerivedHalf(_) => {
                    let tables = chord_tables.ok_or("derived chord policy has no folded tables")?;
                    let length = tables.mixed_len()?;
                    NonZeroUsize::new(length)
                        .and_then(|length| tables.mixed_step(rand.below(length)))
                        .copied()
                }
            };
            if let Some(chord) = mined {
                suffix.push(chord);
                continue;
            }
        }
        suffix.push(crate::smb::archive::sample_chord_from_masks(
            &mut rand,
            vocabulary.masks(),
        )?);
    }
    Ok(suffix)
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
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum SmbCampaignChordPolicy {
    /// Every chord drawn uniformly from the vocabulary.
    #[default]
    Uniform,
    /// Derive recent and all-history tables from the recorded source, mixing
    /// their registered empirical weights into the biased half of each draw.
    DerivedHalf(SmbChordTableDerivation),
}

/// Header identifier for a chord policy.
#[must_use]
pub fn chord_policy_identifier(policy: SmbCampaignChordPolicy) -> String {
    match policy {
        SmbCampaignChordPolicy::Uniform => "chord_uniform".to_owned(),
        SmbCampaignChordPolicy::DerivedHalf(derivation) => {
            let parameters = derivation.parameters;
            let prefix = match derivation.hash_rule {
                EmpiricalStepHashRule::FullJson => "chord_draw_recorded_50",
                EmpiricalStepHashRule::IncrementalHistory => "chord_draw_recorded_51",
            };
            let source = match derivation.source_filter {
                SmbChordSource::All(_) => "all".to_owned(),
                SmbChordSource::Level(filter) => {
                    format!("{},{},{}", filter.world, filter.level, filter.minimum_progress)
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
    if identifier == "chord_uniform" {
        return Ok(SmbCampaignChordPolicy::Uniform);
    }
    let (fields, hash_rule) = if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_50:")
    {
        (Some(rest), EmpiricalStepHashRule::FullJson)
    } else if let Some(rest) = identifier.strip_prefix("chord_draw_recorded_51:") {
        (Some(rest), EmpiricalStepHashRule::IncrementalHistory)
    } else {
        (None, EmpiricalStepHashRule::FullJson)
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
}

/// Execute one job: restore the parent snapshot and apply the suffix exactly as
/// the campaign suffix loop does, collecting per-boundary candidates
/// with worker-side probe verdicts.
pub(crate) fn execute_job(
    target: &mut SmbTarget,
    parent_snapshot: &SmbSnapshot,
    parent_actions: usize,
    parent_milestones: SmbMilestones,
    suffix: &[ButtonChord],
    policies: SmbJobPolicies,
) -> Result<SmbCampaignJobResult, Box<dyn Error>> {
    target.restore(parent_snapshot)?;
    let mut milestones = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    for action in suffix {
        if target.is_dead() || target.is_victory() || length >= policies.max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut milestones, target)?;
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = target.is_victory();
        let failed = target.exit_kind() != ExitKind::Ok;
        // A won game is terminal: nothing past it is searched, so no
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

fn initial_chord_tables(
    policy: SmbCampaignChordPolicy,
    origin: Option<(&str, &SmbArchiveReport)>,
) -> Result<InitialChordTables, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(derivation) = policy else {
        return Ok((None, None));
    };
    let mut tables =
        EmpiricalStepTables::with_hash_rule(derivation.parameters, derivation.hash_rule)?;
    let source_sha256 = match origin {
        None => format!("{:x}", Sha256::digest([])),
        Some((file_sha256, report)) => {
            for entry in &report.entries {
                if source_filter_matches(derivation.source_filter, entry) {
                    tables.fold_retained(&entry.input.actions)?;
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
    recent: std::rc::Rc<Vec<ButtonChord>>,
}

fn recorded_chord_tables<'a>(
    policy: SmbCampaignChordPolicy,
    before: Option<&EmpiricalStepCheckpoint>,
    versions: &'a BTreeMap<u64, SmbChordTableVersion>,
    tables: Option<&'a EmpiricalStepTables<ButtonChord>>,
) -> Result<Option<EmpiricalStepTableRef<'a, ButtonChord>>, Box<dyn Error>> {
    let SmbCampaignChordPolicy::DerivedHalf(_) = policy else {
        if before.is_some() {
            return Err("non-derived chord draw carries a table version".into());
        }
        return Ok(None);
    };
    let before = before.ok_or("derived chord draw is missing its table version")?;
    let version = versions
        .get(&before.records)
        .ok_or("derived chord draw names an unknown table version")?;
    if version.checkpoint != *before {
        return Err("derived chord draw table hash does not match replay".into());
    }
    let tables = tables.ok_or("derived chord policy has no folded tables")?;
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
    let recent = versions
        .last_key_value()
        .filter(|(_, last)| {
            last.checkpoint.table_sha256 == checkpoint.table_sha256
                && last.history_len == tables.all_history().len()
        })
        .map(|(_, last)| std::rc::Rc::clone(&last.recent))
        .unwrap_or_else(|| std::rc::Rc::new(tables.recent().to_vec()));
    versions.insert(
        tables.records(),
        SmbChordTableVersion {
            checkpoint,
            history_len: tables.all_history().len(),
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
        SNAPSHOT_CHECKPOINT_FORMAT
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

    fn identifiers(&self, run: &SmbCampaignRun) -> GameIdentifiers {
        GameIdentifiers {
            controller_vocabulary: button_vocabulary_identifier(run.vocabulary).to_owned(),
            key_policy: KEY_POLICY_IDENTIFIER.to_owned(),
            duration_policy: DURATION_IDENTIFIER.to_owned(),
            suffix_policy: SUFFIX_IDENTIFIER.to_owned(),
            chord_policy: chord_policy_identifier(run.chord),
            replacement_policy: REPLACEMENT_IDENTIFIER.to_owned(),
            resume_policy: RESUME_IDENTIFIER.to_owned(),
        }
    }

    fn resolve_recorded(
        &self,
        identifiers: &GameIdentifiers,
    ) -> Result<SmbCampaignRun, Box<dyn Error>> {
        let expected = [
            (
                identifiers.key_policy.as_str(),
                KEY_POLICY_IDENTIFIER,
                "key policy",
            ),
            (
                identifiers.replacement_policy.as_str(),
                REPLACEMENT_IDENTIFIER,
                "replacement policy",
            ),
            (
                identifiers.suffix_policy.as_str(),
                SUFFIX_IDENTIFIER,
                "suffix policy",
            ),
            (
                identifiers.duration_policy.as_str(),
                DURATION_IDENTIFIER,
                "duration policy",
            ),
        ];
        for (recorded, compiled, name) in expected {
            if recorded != compiled {
                return Err(format!("campaign stream {name} is not recognized").into());
            }
        }
        Ok(SmbCampaignRun {
            chord: chord_policy_from_identifier(&identifiers.chord_policy)?,
            vocabulary: button_vocabulary_from_identifier(&identifiers.controller_vocabulary)?,
        })
    }

    fn new_target(&self) -> Result<SmbTarget, String> {
        SmbTarget::from_smb_rom_bytes_headless(&self.rom).map_err(|error| error.to_string())
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
        stamp_arrival_room(key, snapshot.wram())
    }

    fn execute_job(
        &self,
        target: &mut SmbTarget,
        parent_snapshot: &SmbSnapshot,
        parent_actions: usize,
        parent_milestones: SmbMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<SmbCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            parent_snapshot,
            parent_actions,
            parent_milestones,
            suffix,
            SmbJobPolicies {
                max_actions,
                retention,
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
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        derive_suffix(
            mutation_seed,
            run.chord,
            run.vocabulary,
            state.tables.as_ref().map(EmpiricalStepTables::view),
        )
    }

    fn expand_suffix_recorded(
        &self,
        run: &SmbCampaignRun,
        state: &SmbDrawState,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        let tables =
            recorded_chord_tables(run.chord, before, &state.versions, state.tables.as_ref())?;
        derive_suffix(mutation_seed, run.chord, run.vocabulary, tables)
    }

    fn finish_stream_record(
        &self,
        run: &SmbCampaignRun,
        state: &mut SmbDrawState,
        retained_inputs: &[&[ButtonChord]],
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        let SmbCampaignChordPolicy::DerivedHalf(_) = run.chord else {
            return Ok(None);
        };
        let tables = state
            .tables
            .as_mut()
            .ok_or("derived chord policy has no folded tables")?;
        for input in retained_inputs {
            tables.fold_retained(input)?;
        }
        Ok(tables.finish_record()?)
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

    fn merge_origin_evidence(&self, evidence: &mut SmbCampaignEvidence, source: &SmbArchiveReport) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
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

    fn merge_action_evidence(
        &self,
        evidence: &mut SmbCampaignEvidence,
        action: &SmbCampaignActionResult,
        sequence: u64,
        input: &SmbInput,
    ) {
        merge_progress_watermark(&mut evidence.watermark, &action.observations);
        merge_milestones(&mut evidence.aggregate, action.milestones);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            action.milestones,
            sequence,
            input,
        );
        if milestone_key(action.milestones) > milestone_key(evidence.champion_milestones) {
            evidence.champion_milestones = action.milestones;
            evidence.champion_input = input.clone();
        }
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
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_with_progress(rom, config, origin, stream, None)
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
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    run_smb_campaign_checkpointed(rom, config, origin, stream, progress).map(|(report, _)| report)
}

/// Run a campaign, also returning every retained entry's snapshot so a later
/// whole-tree resume can restore the population instead of re-emulating it.
///
/// # Errors
///
/// Returns an error when the origin is unusable, a worker fails, emulation or
/// snapshotting fails, or the stream cannot be written.
pub fn run_smb_campaign_checkpointed(
    rom: &[u8],
    config: &SmbCampaignConfig,
    origin: &SmbCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
    let game = SmbGame::new(rom);
    run_campaign_checkpointed(&game, &config.generic(), origin, stream, progress)
}

/// Replay a recorded campaign stream serially and rebuild its report.
///
/// # Errors
///
/// Returns an error under the same conditions as
/// [`replay_smb_campaign_checkpointed`].
pub fn replay_smb_campaign(
    rom: &[u8],
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
) -> Result<SmbCampaignModeReport, Box<dyn Error>> {
    replay_smb_campaign_checkpointed(rom, stream_bytes, origin_report, None)
        .map(|(report, _)| report)
}

/// Replay a recorded campaign, also returning the rebuilt snapshot checkpoint.
///
/// # Errors
///
/// Returns an error when the stream is malformed, the origin does not match
/// the header, or any recomputed value differs from the recorded one.
pub fn replay_smb_campaign_checkpointed(
    rom: &[u8],
    stream_bytes: &[u8],
    origin_report: Option<&SmbArchiveReport>,
    origin_checkpoint: Option<&SmbCampaignCheckpoint>,
) -> Result<(SmbCampaignModeReport, SmbSnapshotCheckpoint), Box<dyn Error>> {
    let game = SmbGame::new(rom);
    replay_campaign_checkpointed(&game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::{
        SNAPSHOT_CHECKPOINT_FORMAT, SmbButtonVocabulary, SmbCampaignActionResult,
        SmbCampaignAdmissionDecision, SmbCampaignChordPolicy, SmbCampaignConfig,
        SmbCampaignJobResult, SmbCampaignOrigin, SmbCampaignStreamRecord, SmbGame,
        SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry, chord_policy_from_identifier,
        chord_policy_identifier, derive_suffix, derive_worker_seed, execute_job,
        replay_smb_campaign, run_smb_campaign, run_smb_campaign_with_progress,
    };
    use crate::search::campaign::{CoordinatorCore, write_live_checkpoint};
    use crate::search::empirical_steps::EmpiricalStepHashRule;
    use crate::{
        search::empirical_steps::EmpiricalStepParameters,
        smb::archive::{SmbArchiveEntryReport, SmbArchiveKey, SmbArchiveReport},
        smb::target::{ButtonChord, SmbInput, SmbMilestones, SmbTarget},
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

    fn genesis_config(
        campaign_seed: u64,
        workers: u32,
        execution_budget: u64,
    ) -> SmbCampaignConfig {
        SmbCampaignConfig {
            vocabulary: SmbButtonVocabulary::default(),
            campaign_seed,
            workers,
            execution_budget,
            action_limit: 96,
            host: "unit-test".to_owned(),
            wall_budget: None,
            archive_entry_limit: 32_768,
            chord: SmbCampaignChordPolicy::Uniform,
            retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            selector: crate::search::archive::SelectorPolicy::GroupUniform,
            victory_input_path: None,
            checkpoint_dir: None,
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
            let first = derive_suffix(
                seed,
                SmbCampaignChordPolicy::Uniform,
                SmbButtonVocabulary::default(),
                None,
            )
            .expect("derive suffix");
            let second = derive_suffix(
                seed,
                SmbCampaignChordPolicy::Uniform,
                SmbButtonVocabulary::default(),
                None,
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
        })
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

        let SmbCampaignChordPolicy::DerivedHalf(mut derivation) = policy else {
            panic!("derived policy expected");
        };
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
        let mut first = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load first target");
        let mut second = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load second target");
        first.reset();
        first.apply(&ButtonChord::new(0x81, 12));
        let snapshot = first.snapshot().expect("snapshot prefix");
        let suffix = derive_suffix(
            0x5eed_ca02,
            SmbCampaignChordPolicy::Uniform,
            SmbButtonVocabulary::default(),
            None,
        )
        .expect("derive suffix");
        // Disturb the first instance so the job must depend on the snapshot alone.
        first.apply(&ButtonChord::new(0x02, 30));
        let policies = super::SmbJobPolicies {
            max_actions: 96,
            retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
        };
        let on_first = execute_job(
            &mut first,
            &snapshot,
            1,
            SmbMilestones::default(),
            &suffix,
            policies,
        )
        .expect("execute job on first instance");
        let on_second = execute_job(
            &mut second,
            &snapshot,
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
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        target.reset();
        target.poke_wram(0x0770, 2);
        target.poke_wram(0x075f, 7);
        let won = target.snapshot().expect("snapshot won state");
        let result = execute_job(
            &mut target,
            &won,
            0,
            SmbMilestones::default(),
            &[ButtonChord::new(0x01, 4)],
            super::SmbJobPolicies {
                max_actions: 96,
                retention: crate::search::archive::RetentionPolicy::ProbeAtAdmission45,
            },
        )
        .expect("execute job");
        assert!(result.actions.is_empty());
    }

    #[test]
    fn admission_counts_a_victory_and_keeps_the_first_winning_input() {
        let rom = synthetic_nrom();
        let game = SmbGame::new(&rom);
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        let mut core = CoordinatorCore::new(&game, 96, 32_768);
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
        let (sequence, decisions) = core
            .admit_job(&game, 0, &result)
            .expect("admit winning job");
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
                ..result.actions[0].clone()
            }],
        };
        core.admit_job(&game, 0, &later)
            .expect("admit a second winning job");
        assert_eq!(core.victories, 2);
        assert_eq!(
            core.victory_input,
            Some(SmbInput {
                actions: vec![winning]
            })
        );
        let report = core.into_archive_report(&game, 0);
        assert_eq!(report.entries.len(), 1, "a won lineage is not extended");
    }

    #[test]
    fn live_checkpoint_files_round_trip() {
        let rom = synthetic_nrom();
        let game = SmbGame::new(&rom);
        let mut core = CoordinatorCore::new(&game, 96, 32_768);
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("load target");
        core.bootstrap(&game, &mut target)
            .expect("bootstrap genesis");
        let directory =
            std::env::temp_dir().join(format!("smb-live-checkpoint-{}", std::process::id()));
        std::fs::create_dir_all(&directory).expect("create checkpoint directory");
        write_live_checkpoint(&game, &core, 7, &directory).expect("write live checkpoint");
        let report: SmbArchiveReport = serde_json::from_slice(
            &std::fs::read(directory.join("checkpoint-archive.json")).expect("read archive"),
        )
        .expect("parse archive report");
        assert_eq!(report.entries.len(), core.archive.entries.len());
        let decoded = SmbSnapshotCheckpoint::from_bytes(
            &std::fs::read(directory.join("checkpoint-snapshots.bin")).expect("read snapshots"),
            SNAPSHOT_CHECKPOINT_FORMAT,
        )
        .expect("decode snapshot checkpoint");
        let owned = SmbSnapshotCheckpoint {
            format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
            entries: core
                .archive
                .entries
                .iter()
                .map(|entry| SmbSnapshotCheckpointEntry {
                    id: entry.report.id,
                    snapshot: entry.snapshot.clone(),
                })
                .collect(),
        };
        assert_eq!(decoded, owned, "borrowed encoding matches the owned one");
        std::fs::remove_dir_all(&directory).ok();
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
            "nes_down_ten",
            "frozen_area_span",
            "one_or_two",
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
            ("nes_down_ten", "frozen_nine_mask"),
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
            ..genesis_config(0x5eed_ca13, 3, 12)
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
        let retained_successes = text
            .lines()
            .skip(1)
            .map(|line| {
                serde_json::from_str::<SmbCampaignStreamRecord>(line)
                    .expect("parse campaign record")
            })
            .filter_map(|record| match record {
                SmbCampaignStreamRecord::Job(job) => job.chord_table_before,
                SmbCampaignStreamRecord::Skip(skip) => skip.chord_table_before,
            })
            .map(|checkpoint| checkpoint.retained_successes)
            .max()
            .unwrap_or(0);
        assert!(
            retained_successes > 0,
            "from-scratch continuous tables must grow from retained successes"
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
        use super::{
            SmbCampaignCheckpoint, SmbSnapshotCheckpoint, replay_smb_campaign_checkpointed,
            run_smb_campaign_checkpointed,
        };
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
        // independent live runs whose archives are compared for equality;
        // multi-worker draws are schedule-dependent, so one worker keeps
        // the comparison sound.
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
        assert_eq!(counts.rerooted, 0);
        assert_eq!(counts.checkpointed, 0);

        // Restoring the source population from its snapshot checkpoint
        // reaches the same archive as re-emulating it, records the
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
        assert_eq!(restored_live.bootstrap_frames, 0);
        assert_eq!(
            restored_live.origin.checkpoint_sha256.as_deref(),
            Some(checkpoint.file_sha256.as_str())
        );
        let restored_counts = restored_live.tree_import.expect("restored counts");
        assert_eq!(restored_counts.checkpointed, source_retained);
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
        let game = SmbGame::new(&rom);
        let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom).expect("target");
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
                    player_engine_state: 0,
                    state_fingerprint: 0,
                    room_x_bucket: 0,
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
        let mut core = CoordinatorCore::new(&game, 4, 32_768);
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
        // The synthetic ROM never leaves one cell, so the two-entry cell
        // keeps genesis and the one-action entry and refuses the rest.
        assert_eq!((counts.imported, counts.rejected), (1, 3));
        let reports: Vec<_> = core.archive.entries.iter().map(|e| &e.report).collect();
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
        assert_eq!(reports.len(), 2);
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
        // With one worker the schedule is derivable from the seed, so the
        // sidecar run must record byte-identical stream bytes.
        assert_eq!(without, with);
        assert!(!with.is_empty());
        assert!(
            std::str::from_utf8(&with)
                .expect("stream is utf-8")
                .lines()
                .all(|line| !line.contains("unix_time")),
            "no sidecar field reaches the recorded stream"
        );
        let replayed =
            replay_smb_campaign(&rom, &with, None).expect("sidecar run replays byte-exact");
        assert_eq!(replayed.stream_sha256, observed.stream_sha256);
        assert_eq!(replayed.archive, observed.archive);
    }
}
