// SPDX-License-Identifier: AGPL-3.0-or-later

//! Nova implementation of the game-neutral campaign interface.

use std::{
    collections::BTreeSet,
    error::Error,
    io::Write,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    nova::{
        archive::{
            DURATION_IDENTIFIER, KEY_POLICY_IDENTIFIER, MAX_NOVA_ACTIONS, NovaArchiveKey,
            NovaArchiveReport, NovaMilestoneInputs, NovaMilestoneTimes, NovaMilestones,
            NovaProgressWatermark, REPLACEMENT_IDENTIFIER, archive_key, chord_time,
            merge_milestones, merge_progress_watermark, milestone_key, milestones, sample_chord,
        },
        target::{
            ButtonChord, NovaInput, NovaObservations, NovaSnapshot, NovaTarget, preference_tuple,
        },
    },
    search::{
        archive::RetentionPolicy,
        campaign::{
            ArchiveReportState, CampaignActionResult, CampaignCandidate, CampaignCheckpoint,
            CampaignConfig, CampaignJobResult, CampaignModeReport, CampaignOrigin,
            CampaignProgressRecord, CampaignStreamHeader, Game, GamePolicies, SnapshotCheckpoint,
            postcard_result_sha256, replay_campaign_checkpointed, run_campaign_checkpointed,
        },
        draw::{DrawMixture, MixtureDraw, SuffixShape, draw_suffix},
        empirical_steps::EmpiricalStepCheckpoint,
    },
    target::{ExitKind, Target},
};

/// Stream format written by Nova campaigns.
pub const CAMPAIGN_STREAM_FORMAT: &str = "nova-quicknes-campaign-stream-v1";
/// Snapshot checkpoint format written by Nova campaigns.
pub const SNAPSHOT_CHECKPOINT_FORMAT: &str = "nova-quicknes-snapshot-checkpoint-v1";

const CONTROLLER_VOCABULARY_FIELD: &str = "controller_vocabulary";
const KEY_POLICY_FIELD: &str = "key_policy";
const DURATION_POLICY_FIELD: &str = "duration_policy";
const REPLACEMENT_POLICY_FIELD: &str = "replacement_policy";
const TERMINAL_POLICY_FIELD: &str = "terminal_policy";
const EMULATOR_BACKEND_FIELD: &str = "emulator_backend";
const CONTROLLER_VOCABULARY_IDENTIFIER: &str = "directions9_times_ab4_no_start_select_v1";
const TERMINAL_POLICY_IDENTIFIER: &str = "first_durable_level_clear";

const VIABILITY_PROBE_MASKS: [u8; 4] = [0, 0x01, 0x80, 0x81];
const VIABILITY_PROBE_FRAMES: u16 = 60;

type NovaPreference = (u8, u8, u8, bool, u8, u8);
type NovaChampionKey = (NovaProgressWatermark, NovaPreference);

/// Header placeholder for a game with no adaptive draw table.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct NovaNoTableHeader;

/// ROM and emulator identity shared by Nova workers.
pub struct NovaGame {
    rom: Vec<u8>,
    core_path: PathBuf,
    core_sha256: String,
    identity: String,
}

impl NovaGame {
    /// Build a game context over a pinned QuickNES core.
    #[must_use]
    pub fn new(rom: &[u8], core_path: &Path, core_sha256: &str) -> Self {
        let identity = format!(
            "quicknes-libretro:{};{};{};state=ppu-unused2-zero-v1;result_digest=postcard-1.1.3-sha256-hex-v2;sha256={core_sha256}",
            machine::quicknes::QUICKNES_REVISION,
            machine::quicknes::QUICKNES_BUILD,
            machine::quicknes::QUICKNES_OPTIONS,
        );
        Self {
            rom: rom.to_vec(),
            core_path: core_path.to_path_buf(),
            core_sha256: core_sha256.to_owned(),
            identity,
        }
    }

    /// Build from the external core named by `HARMONY_QUICKNES_CORE`.
    pub fn from_environment(rom: &[u8]) -> Result<Self, Box<dyn Error>> {
        let core_path = PathBuf::from(
            std::env::var_os("HARMONY_QUICKNES_CORE")
                .ok_or("HARMONY_QUICKNES_CORE must name the pinned QuickNES core")?,
        );
        let core_sha256 = format!("{:x}", Sha256::digest(std::fs::read(&core_path)?));
        Ok(Self::new(rom, &core_path, &core_sha256))
    }

    /// Pinned emulator identity recorded in streams.
    #[must_use]
    pub fn emulator_identity(&self) -> &str {
        &self.identity
    }
}

/// Nova's fixed recorded run policy.
#[derive(Clone, Copy, Debug)]
pub struct NovaCampaignRun;

/// Game-owned campaign evidence.
#[derive(Clone, Default)]
pub struct NovaCampaignEvidence {
    aggregate: NovaMilestones,
    watermark: NovaProgressWatermark,
    first_reached: NovaMilestoneTimes,
    first_inputs: NovaMilestoneInputs,
    champion_input: NovaInput,
    champion_milestones: NovaMilestones,
    champion_key: Option<NovaChampionKey>,
}

/// Nova campaign origin.
pub type NovaCampaignOrigin = CampaignOrigin<NovaGame>;
/// Nova resume checkpoint.
pub type NovaCampaignCheckpoint = CampaignCheckpoint<NovaSnapshot>;
/// Nova whole-tree snapshot checkpoint.
pub type NovaSnapshotCheckpoint = SnapshotCheckpoint<NovaSnapshot>;
/// Nova stream header.
pub type NovaCampaignStreamHeader = CampaignStreamHeader<NovaNoTableHeader>;
/// Nova campaign report.
pub type NovaCampaignModeReport = CampaignModeReport<ButtonChord, NovaArchiveReport>;
/// Nova progress sidecar record.
pub type NovaCampaignProgressRecord = CampaignProgressRecord<NovaArchiveKey>;
type NovaCampaignActionResult = CampaignActionResult<NovaGame>;
type NovaCampaignJobResult = CampaignJobResult<NovaGame>;

/// Fixed configuration for one live Nova campaign.
pub struct NovaCampaignConfig {
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
    /// Maximum retained archive entries.
    pub archive_entry_limit: usize,
    /// Admission policy.
    pub retention: RetentionPolicy,
    /// Generic parent selector.
    pub selector: crate::search::archive::SelectorPolicy,
    /// Generic suffix-length shape.
    pub suffix: SuffixShape,
    /// Generic draw mixture.
    pub mixture: DrawMixture,
    /// Live-only path receiving the first level-clearing input.
    pub victory_input_path: Option<PathBuf>,
}

impl NovaCampaignConfig {
    fn generic(&self) -> CampaignConfig<NovaGame> {
        CampaignConfig {
            campaign_seed: self.campaign_seed,
            workers: self.workers,
            execution_budget: self.execution_budget,
            action_limit: self.action_limit,
            host: self.host.clone(),
            wall_budget: self.wall_budget,
            archive_entry_limit: self.archive_entry_limit,
            run: NovaCampaignRun,
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
        .ok_or_else(|| format!("Nova stream is missing {field}").into())
}

fn merge_action_milestones(
    aggregate: &mut NovaMilestones,
    target: &NovaTarget,
) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Err("cannot decode Nova milestones from a failed target".into());
    }
    for observation in target.last_action_observations() {
        merge_milestones(aggregate, milestones(observation.decoded));
    }
    Ok(())
}

fn admission_is_viable(
    target: &mut NovaTarget,
    snapshot: &NovaSnapshot,
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

fn execute_job(
    target: &mut NovaTarget,
    parent_snapshot: &NovaSnapshot,
    parent_actions: usize,
    parent_milestones: NovaMilestones,
    suffix: &[ButtonChord],
    max_actions: usize,
    retention: RetentionPolicy,
) -> Result<NovaCampaignJobResult, Box<dyn Error>> {
    target.restore(parent_snapshot)?;
    let mut aggregate = parent_milestones;
    let mut length = parent_actions;
    let mut actions = Vec::with_capacity(suffix.len());
    if target.is_dead() || target.cleared_a_level() {
        return Ok(CampaignJobResult { actions });
    }
    for action in suffix {
        if length >= max_actions {
            break;
        }
        length = length.saturating_add(1);
        target.apply(action);
        merge_action_milestones(&mut aggregate, target)?;
        let observations = target.last_action_observations().to_vec();
        let dead = target.is_dead();
        let victory = target.cleared_a_level();
        let failed = target.exit_kind() != ExitKind::Ok;
        let candidate = if dead || victory || failed {
            None
        } else {
            let snapshot = target.snapshot().ok_or("failed to snapshot Nova suffix")?;
            let viable = match retention {
                RetentionPolicy::ProbeAtAdmission45 => admission_is_viable(target, &snapshot)?,
                RetentionPolicy::AdmitAlive => true,
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
    times: &mut NovaMilestoneTimes,
    inputs: &mut NovaMilestoneInputs,
    value: NovaMilestones,
    sequence: u64,
    input: &NovaInput,
) {
    if value.cleared > 0 && times.first_clear.is_none() {
        times.first_clear = Some(sequence);
        inputs.first_clear = Some(input.clone());
    }
    if value.collectibles > 0 && times.first_collectible.is_none() {
        times.first_collectible = Some(sequence);
        inputs.first_collectible = Some(input.clone());
    }
    if value.acquired_ability && times.first_ability.is_none() {
        times.first_ability = Some(sequence);
        inputs.first_ability = Some(input.clone());
    }
}

fn action_champion_key(observations: &[NovaObservations]) -> Option<NovaChampionKey> {
    observations.last().map(|observation| {
        let state = observation.decoded;
        (
            NovaProgressWatermark {
                cleared: state.cleared_count(),
                collectibles: state.collectible_count(),
                available: state.available_count(),
                started_level: state.started_level,
                level: state.level,
                x: state.x,
                y: state.y,
            },
            preference_tuple(state),
        )
    })
}

impl Game for NovaGame {
    type Target = NovaTarget;
    type Action = ButtonChord;
    type Key = NovaArchiveKey;
    type Milestones = NovaMilestones;
    type Progress = NovaProgressWatermark;
    type Snapshot = NovaSnapshot;
    type Observations = NovaObservations;
    type Evidence = NovaCampaignEvidence;
    type ArchiveReport = NovaArchiveReport;
    type Run = NovaCampaignRun;
    type DrawState = ();
    type TableHeader = NovaNoTableHeader;

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
        MAX_NOVA_ACTIONS
    }

    fn action_time_fn(&self) -> fn(&ButtonChord) -> u64 {
        chord_time
    }

    fn result_sha256(&self, result: &NovaCampaignJobResult) -> Result<String, Box<dyn Error>> {
        postcard_result_sha256(result)
    }

    fn policies(&self, _run: &NovaCampaignRun) -> GamePolicies {
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

    fn resolve_recorded(&self, policies: &GamePolicies) -> Result<NovaCampaignRun, Box<dyn Error>> {
        let expected = self.policies(&NovaCampaignRun);
        if policies != &expected {
            for (field, value) in &expected {
                if recorded(policies, field)? != value {
                    return Err(format!("Nova stream {field} policy is not recognized").into());
                }
            }
            return Err("Nova stream carries an unknown game policy".into());
        }
        Ok(NovaCampaignRun)
    }

    fn new_target(&self) -> Result<NovaTarget, String> {
        NovaTarget::from_rom_bytes_headless(&self.rom, &self.core_path, &self.core_sha256)
            .map_err(|error| error.to_string())
    }

    fn reset(&self, target: &mut NovaTarget) {
        target.reset();
    }

    fn restore(
        &self,
        target: &mut NovaTarget,
        snapshot: &NovaSnapshot,
    ) -> Result<(), Box<dyn Error>> {
        target.restore(snapshot)
    }

    fn frames_clocked(&self, target: &NovaTarget) -> u64 {
        target.frames_clocked()
    }

    fn apply_action(
        &self,
        target: &mut NovaTarget,
        action: &ButtonChord,
        aggregate: &mut NovaMilestones,
    ) -> Result<(), Box<dyn Error>> {
        target.apply(action);
        merge_action_milestones(aggregate, target)
    }

    fn is_terminal(&self, target: &NovaTarget) -> bool {
        target.is_dead() || target.exit_kind() != ExitKind::Ok
    }

    fn is_run_terminal(
        &self,
        _run: &NovaCampaignRun,
        target: &NovaTarget,
    ) -> Result<bool, Box<dyn Error>> {
        if target.exit_kind() != ExitKind::Ok {
            return Err("Nova terminal predicate cannot inspect a failed emulator".into());
        }
        Ok(target.is_dead() || target.cleared_a_level())
    }

    fn snapshot(&self, target: &mut NovaTarget) -> Result<NovaSnapshot, Box<dyn Error>> {
        target
            .snapshot()
            .ok_or_else(|| "failed to snapshot Nova".into())
    }

    fn current_key(&self, target: &NovaTarget) -> Result<NovaArchiveKey, Box<dyn Error>> {
        Ok(archive_key(target.mechanical_state()))
    }

    fn complete_candidate_key(
        &self,
        key: NovaArchiveKey,
        _snapshot: &NovaSnapshot,
    ) -> Result<NovaArchiveKey, Box<dyn Error>> {
        Ok(key)
    }

    fn execute_job(
        &self,
        _run: &NovaCampaignRun,
        target: &mut NovaTarget,
        parent_snapshot: &NovaSnapshot,
        parent_actions: usize,
        parent_milestones: NovaMilestones,
        suffix: &[ButtonChord],
        max_actions: usize,
        retention: RetentionPolicy,
    ) -> Result<NovaCampaignJobResult, Box<dyn Error>> {
        execute_job(
            target,
            parent_snapshot,
            parent_actions,
            parent_milestones,
            suffix,
            max_actions,
            retention,
        )
    }

    fn initial_draw_state(
        &self,
        _run: &NovaCampaignRun,
        _origin: Option<(&str, &NovaArchiveReport)>,
    ) -> Result<((), Option<NovaNoTableHeader>), Box<dyn Error>> {
        Ok(((), None))
    }

    fn draw_checkpoint(
        &self,
        _state: &(),
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        Ok(None)
    }

    fn expand_suffix(
        &self,
        _run: &NovaCampaignRun,
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
        run: &NovaCampaignRun,
        state: &(),
        shape: SuffixShape,
        mixture: MixtureDraw,
        before: Option<&EmpiricalStepCheckpoint>,
        mutation_seed: u64,
    ) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
        if before.is_some() {
            return Err("Nova stream unexpectedly records a draw table".into());
        }
        self.expand_suffix(run, state, shape, mixture, mutation_seed)
    }

    fn finish_stream_record(
        &self,
        _run: &NovaCampaignRun,
        _state: &mut (),
        _retained: &[(usize, &[ButtonChord])],
    ) -> Result<Option<EmpiricalStepCheckpoint>, Box<dyn Error>> {
        Ok(None)
    }

    fn remember_draw_version(
        &self,
        _state: &mut (),
        required: &BTreeSet<u64>,
    ) -> Result<(), Box<dyn Error>> {
        if required.is_empty() {
            Ok(())
        } else {
            Err("Nova stream requires an unsupported draw-table version".into())
        }
    }

    fn merge_milestones(&self, into: &mut NovaMilestones, from: NovaMilestones) {
        merge_milestones(into, from);
    }

    fn aggregate_milestones(evidence: &NovaCampaignEvidence) -> NovaMilestones {
        evidence.aggregate
    }

    fn aggregate_progress(evidence: &NovaCampaignEvidence) -> NovaProgressWatermark {
        evidence.watermark
    }

    fn merge_origin_evidence(
        &self,
        evidence: &mut NovaCampaignEvidence,
        source: &NovaArchiveReport,
    ) {
        evidence.watermark = evidence.watermark.max(source.progress_watermark);
    }

    fn merge_snapshot_root_evidence(
        &self,
        evidence: &mut NovaCampaignEvidence,
        target: &NovaTarget,
    ) -> Result<(), Box<dyn Error>> {
        let observation = NovaObservations {
            frame_count: 0,
            decoded: target.mechanical_state(),
            changed_indices: Vec::new(),
            dead: target.is_dead(),
            log_line: String::new(),
        };
        merge_progress_watermark(&mut evidence.watermark, &[observation]);
        Ok(())
    }

    fn merge_import_evidence(
        &self,
        evidence: &mut NovaCampaignEvidence,
        value: NovaMilestones,
        input: &NovaInput,
    ) {
        merge_milestones(&mut evidence.aggregate, value);
        update_first_inputs(
            &mut evidence.first_reached,
            &mut evidence.first_inputs,
            value,
            0,
            input,
        );
        if milestone_key(value) > milestone_key(evidence.champion_milestones) {
            evidence.champion_milestones = value;
            evidence.champion_input = input.clone();
        }
    }

    fn merge_action_evidence(
        &self,
        evidence: &mut NovaCampaignEvidence,
        action: &NovaCampaignActionResult,
        sequence: u64,
        input: &NovaInput,
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
        if let Some(key) = action_champion_key(&action.observations)
            && evidence.champion_key.is_none_or(|current| key > current)
        {
            evidence.champion_key = Some(key);
            evidence.champion_milestones = action.milestones;
            evidence.champion_input = input.clone();
        }
    }

    fn source_entries<'a>(
        &self,
        source: &'a NovaArchiveReport,
    ) -> &'a [crate::nova::archive::NovaArchiveEntryReport] {
        &source.entries
    }

    fn resume_input(&self, source: &NovaArchiveReport) -> Result<NovaInput, Box<dyn Error>> {
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
            .ok_or_else(|| "Nova source archive has no retained entries".into())
    }

    fn archive_report(
        &self,
        evidence: &NovaCampaignEvidence,
        state: ArchiveReportState<Self>,
    ) -> NovaArchiveReport {
        NovaArchiveReport {
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

/// Run a Nova campaign and return its report plus whole-tree checkpoint.
pub fn run_nova_campaign_checkpointed(
    game: &NovaGame,
    config: &NovaCampaignConfig,
    origin: &NovaCampaignOrigin,
    stream: &mut dyn Write,
    progress: Option<&mut dyn Write>,
) -> Result<(NovaCampaignModeReport, NovaSnapshotCheckpoint), Box<dyn Error>> {
    run_campaign_checkpointed(game, &config.generic(), origin, stream, progress)
}

/// Replay a recorded Nova stream exactly.
pub fn replay_nova_campaign_checkpointed(
    game: &NovaGame,
    stream_bytes: &[u8],
    origin_report: Option<&NovaArchiveReport>,
    origin_checkpoint: Option<&NovaCampaignCheckpoint>,
) -> Result<(NovaCampaignModeReport, NovaSnapshotCheckpoint), Box<dyn Error>> {
    replay_campaign_checkpointed(game, stream_bytes, origin_report, origin_checkpoint)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recorded_policy_set_is_exact_and_game_owned() {
        let game = NovaGame::new(&[1, 2, 3], Path::new("core.so"), &"a".repeat(64));
        let policies = game.policies(&NovaCampaignRun);
        let NovaCampaignRun = game.resolve_recorded(&policies).expect("resolve");
        let mut foreign = policies;
        foreign.insert("level".to_owned(), "understood-by-search".to_owned());
        assert!(game.resolve_recorded(&foreign).is_err());
    }
}
