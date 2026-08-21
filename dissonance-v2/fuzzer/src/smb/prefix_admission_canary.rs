// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the paired observer-prefix archive-admission canary.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
};

use libafl::executors::ExitKind;
use libafl_bolts::rands::StdRand;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    smb::{
        archive::{
            Archive, ArchiveCandidate, SmbArchiveKey, SmbArchiveKeyPolicy,
            SmbArchiveReplacementPolicy, SmbArchiveSelectorPolicy, SmbArchiveWaypointPolicy,
            SmbSelectorDraw, archive_key, merge_action_milestones, merge_progress_watermark,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMechanicalState, SmbMilestones,
            SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
            smb_mechanical_state_from_wram,
        },
    },
    target::Target,
};

const FORMAT: &str = "smb-prefix-admission-canary-v1";
const PREREGISTRATION_COMMIT: &str = "782cbc6f7eaf45f3b4339119b07c4dc19885c3f2";
const PREREGISTRATION_DOC_SHA256: &str =
    "61accce6c83bad292af8cd08995e4f546bfc51cfb9f91f0ef527d2036ff874b6";
const CODE_BASE: &str = "5a33f3ad";
const SOURCE_ARCHIVE_SHA256: &str =
    "d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81";
const SOURCE_STREAM_SHA256: &str =
    "ab869286a526dab104f7846ae0313745de7087e3733e99016218defb42e90201";
const SOURCE_FILE_SHA256: &str = "5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159";
const SOURCE_INPUT_SHA256: &str =
    "584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1";
const SOURCE_TRACE_SHA256: &str =
    "9245f6d42f684a1fcd0a33a762519a51270d1ece2b695ea5a575d83ff64149a1";
const SOURCE_WRAM_SHA256: &str = "936ac08d4c48a2968bec111324fd7ed28628ea89b35baa049b1b5abfffc896ea";
const SOURCE_SNAPSHOT_SHA256: &str =
    "107bab5a4691ca0e43586b3c95849031782d40f2a3013856161ae4f1d997ae66";
const PRIOR_PREFIX_REPORT_SHA256: &str =
    "ad0bfdfe85b08562b7a76425655c44c82c6f7b0d24259f1c212057662ffb394e";
const C119_PRODUCTION_BINARY_SHA256: &str =
    "87fb11f300a7af9386eb06c8b55e7a7353d6cb3654b83ee6a5615806e72e2862";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-c119-observer-prefix-paired-admission-v1";
const SEED_LABEL_SHA256: &str = "e32f651b50c1958a1005c311bd502b8019b48635390e572b17e4dbbee44568f6";
const MASTER_SEED: u64 = 9_986_100_298_565_103_587;
const SOURCE_ACTIONS: usize = 3_297;
const SOURCE_FRAMES: u64 = 155_148;
const PAIRS: usize = 8;
const DRAWS: usize = 128;
const ARMS: usize = PAIRS * 2;
const WORKERS: usize = 12;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 257;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_FULL_ACTION_FRAMES: u64 = 245_760;
const MAX_ENDPOINT_PROBE_FRAMES: u64 = 276_480;
const MAX_PREFIX_ACTION_FRAMES: u64 = 243_712;
const MAX_PREFIX_PROBE_FRAMES: u64 = 276_480;
const MAX_TOTAL_FRAMES: u64 = 1_202_273;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const PROBE_FRAMES: u16 = 45;
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 0,
    progress: 236,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 0,
    progress: 236,
    player_y_bucket: 7,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    pair: usize,
    draw: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
}

#[derive(Clone, Debug, Serialize)]
struct Config {
    pairs: usize,
    draws_per_arm: usize,
    workers: usize,
    action_limit: usize,
    archive_limit: usize,
    selector: &'static str,
    retention: &'static str,
    replacement: &'static str,
    key: &'static str,
    prefix_rule: &'static str,
    assignment: &'static str,
    probe_masks: [u8; 3],
    probe_frames: u16,
    max_total_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BaselineRecord {
    record: &'static str,
    setup_frames: u64,
    replay_frames: u64,
    actions: usize,
    endpoint: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    trace_sha256: String,
    wram_sha256: String,
    snapshot_sha256: String,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
}

#[derive(Clone)]
struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum EntryOrigin {
    Source,
    Endpoint { direct_prefix_parent: bool },
    Prefix,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AdmissionOutcome {
    NotCandidate,
    ProbeRefused,
    ObservedOnly,
    Duplicate { id: usize },
    Rejected,
    Retained { id: usize, displaced: bool },
}

impl AdmissionOutcome {
    fn retained_id(&self) -> Option<usize> {
        match self {
            Self::Retained { id, .. } => Some(*id),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProbeAttempt {
    mask: u8,
    work_frames: u64,
    dead: bool,
    survived: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CandidateEvidence {
    action: ButtonChord,
    input_sha256: String,
    observations: Vec<SmbObservations>,
    work_frames: u64,
    dead: bool,
    endpoint: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: Option<String>,
    key: Option<SmbArchiveKey>,
    probe: Vec<ProbeAttempt>,
    probe_survived: bool,
    admission: AdmissionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PrefixEvidence {
    expected: SmbObservations,
    candidate: CandidateEvidence,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct TargetProjection {
    full_action: ButtonChord,
    full_observations: Vec<SmbObservations>,
    full_work_frames: u64,
    full_dead: bool,
    full_endpoint: SmbMechanicalState,
    full_wram_sha256: String,
    full_snapshot_sha256: Option<String>,
    full_probe: Vec<ProbeAttempt>,
    prefix_expected: Option<SmbObservations>,
    prefix_observations: Option<Vec<SmbObservations>>,
    prefix_work_frames: u64,
    prefix_endpoint: Option<SmbMechanicalState>,
    prefix_wram_sha256: Option<String>,
    prefix_snapshot_sha256: Option<String>,
    prefix_probe: Vec<ProbeAttempt>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DirectDescendant {
    parent_id: usize,
    parent_snapshot_sha256: String,
    id: usize,
    input_sha256: String,
    snapshot_sha256: String,
    watermark: SmbProgressWatermark,
    beyond_parent: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StartEvidence {
    observation: SmbObservations,
    wram_sha256: String,
    dead: bool,
    failed: bool,
    instance_work_frames: u64,
    milestones: SmbMilestones,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct DrawRecord {
    draw: usize,
    source_index: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    parent_id: usize,
    parent_origin: EntryOrigin,
    parent_input_sha256: String,
    parent_snapshot_sha256: String,
    start: StartEvidence,
    full: CandidateEvidence,
    prefix: Option<PrefixEvidence>,
    target_projection_sha256: String,
    productive: bool,
    direct_descendant: Option<DirectDescendant>,
    active_endpoint_maximum: FinalMaximum,
    full_action_frames: u64,
    endpoint_probe_frames: u64,
    prefix_action_frames: u64,
    prefix_probe_frames: u64,
    total_work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FinalMaximum {
    watermark: SmbProgressWatermark,
    active_ids: Vec<usize>,
    attained_by_active_direct_descendant: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ArmRecord {
    record: &'static str,
    ordinal: usize,
    worker: usize,
    pair: usize,
    treatment: bool,
    initial_archive_sha256: String,
    draws: Vec<DrawRecord>,
    admitted_prefixes: Vec<(usize, String, String)>,
    selected_prefix_ids: Vec<usize>,
    direct_descendants: Vec<DirectDescendant>,
    final_maximum: FinalMaximum,
    full_action_frames: u64,
    endpoint_probe_frames: u64,
    prefix_action_frames: u64,
    prefix_probe_frames: u64,
    total_work_frames: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Verdict {
    Go,
    Inconclusive,
    Stop,
}

#[derive(Clone, Debug, Serialize)]
struct PairClassification {
    pair: usize,
    control: SmbProgressWatermark,
    treatment: SmbProgressWatermark,
    treatment_win: bool,
    control_win: bool,
    tie: bool,
    treatment_max_direct_descendant: bool,
}

#[derive(Clone, Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    verdict: Verdict,
    treatment_wins: usize,
    control_wins: usize,
    ties: usize,
    sign_tail_numerator: u128,
    sign_tail_denominator: u128,
    directional_gate: bool,
    structural_gate: bool,
    distinct_admitted_prefix_snapshots: usize,
    admitted_prefix_pairs: usize,
    distinct_selected_prefix_snapshots: usize,
    selected_prefix_pairs: usize,
    distinct_descendant_snapshots: usize,
    any_beyond_prefix_descendant: bool,
    winning_max_direct_descendant: bool,
    pair_classifications: Vec<PairClassification>,
    worker_setup_frames: Vec<u64>,
    setup_frames: u64,
    source_replay_frames: u64,
    full_action_frames: u64,
    endpoint_probe_frames: u64,
    prefix_action_frames: u64,
    prefix_probe_frames: u64,
    experimental_frames: u64,
    total_frames: u64,
}

#[derive(Serialize)]
struct HeaderRecord<'a> {
    record: &'static str,
    format: &'static str,
    preregistration_commit: &'static str,
    preregistration_doc_sha256: &'static str,
    code_base: &'static str,
    source_archive_sha256: &'static str,
    source_stream_sha256: &'static str,
    prior_prefix_report_sha256: &'static str,
    c119_production_binary_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    source_entry_id: u64,
    source_parent_id: u64,
    source_created_execution: u64,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    recipe_sha256: &'a str,
    config_sha256: &'a str,
    config: &'a Config,
}

struct NdjsonOutput {
    writer: BufWriter<fs::File>,
    digest: Sha256,
}

impl NdjsonOutput {
    fn new(file: fs::File) -> Self {
        Self {
            writer: BufWriter::new(file),
            digest: Sha256::new(),
        }
    }

    fn write<T: Serialize>(&mut self, value: &T) -> Result<(), Box<dyn Error>> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        self.digest.update(&bytes);
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    fn digest(&self) -> String {
        finish_sha256(self.digest.clone())
    }

    fn finish(mut self) -> Result<String, Box<dyn Error>> {
        self.writer.flush()?;
        self.writer.get_ref().sync_all()?;
        Ok(finish_sha256(self.digest))
    }
}

#[derive(Debug)]
struct ArmReply {
    ordinal: usize,
    worker: usize,
    result: Result<ArmRecord, String>,
}

#[derive(Debug)]
struct SetupReply {
    worker: usize,
    result: Result<u64, String>,
}

enum WorkerState {
    Ready(Box<SmbTarget>),
    Failed(String),
}

/// Run the sealed canary from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-prefix-admission-canary <input.json> <output.jsonl>")?,
    );
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "input JSON")?;
    let source_file_sha256 = sha256_bytes(&source_bytes);
    if source_file_sha256 != SOURCE_FILE_SHA256 {
        return Err("compact source file does not match the preregistration".into());
    }
    let source: SmbInput = serde_json::from_slice(&source_bytes)?;
    validate_source(&source)?;
    let source_input_sha256 = sha256_json(&source)?;
    if source_input_sha256 != SOURCE_INPUT_SHA256 {
        return Err("semantic source input does not match the preregistration".into());
    }

    let recipes = derive_recipes(&source)?;
    let recipe_identity = recipes
        .iter()
        .flat_map(|pair| pair.iter())
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.pair)?,
                u64::try_from(recipe.draw)?,
                u64::try_from(recipe.source_index)?,
                recipe.action,
                recipe.selector_seed,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    let recipe_sha256 = sha256_json(&recipe_identity)?;
    let config = Config {
        pairs: PAIRS,
        draws_per_arm: DRAWS,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        selector: "concentrated_recency_fresh_seed_per_draw_v1",
        retention: "probe_at_admission_45",
        replacement: "fewest_actions",
        key: "frozen",
        prefix_rule: "first_strict_interior_nonterminal_observer_v1",
        assignment: "arm_ordinal_mod_12_persistent_buffered_ascending_v1",
        probe_masks: PROBE_MASKS,
        probe_frames: PROBE_FRAMES,
        max_total_frames: MAX_TOTAL_FRAMES,
    };
    let config_sha256 = sha256_json(&config)?;
    let bin_source_sha256 = sha256_bytes(bin_source);
    let module_source_sha256 = sha256_bytes(module_source);
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = read_bounded(&rom_path, MAX_ROM_BYTES, "ROM")?;
    let rom_sha256 = sha256_bytes(&rom);
    if rom_sha256 != ROM_SHA256 {
        return Err("ROM does not match the preregistration".into());
    }
    let executable_path = env::current_exe()?;
    let executable = read_bounded(&executable_path, MAX_EXECUTABLE_BYTES, "executable")?;
    let executable_sha256 = sha256_bytes(&executable);

    let mut baseline_target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut baseline_target, &source)?;
    let (mut arms, worker_setup_frames) = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    arms.sort_by_key(|arm| arm.ordinal);
    validate_arm_ordinals(&arms)?;
    validate_pure_target_equivalence(&arms)?;
    let classification = classify(&arms)?;
    let work = summarize_work(&arms, baseline.record.setup_frames, &worker_setup_frames)?;

    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration_commit: PREREGISTRATION_COMMIT,
        preregistration_doc_sha256: PREREGISTRATION_DOC_SHA256,
        code_base: CODE_BASE,
        source_archive_sha256: SOURCE_ARCHIVE_SHA256,
        source_stream_sha256: SOURCE_STREAM_SHA256,
        prior_prefix_report_sha256: PRIOR_PREFIX_REPORT_SHA256,
        c119_production_binary_sha256: C119_PRODUCTION_BINARY_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        source_entry_id: 48_076,
        source_parent_id: 29_805,
        source_created_execution: 49_709,
        rom_sha256: &rom_sha256,
        executable_sha256: &executable_sha256,
        bin_source_sha256: &bin_source_sha256,
        module_source_sha256: &module_source_sha256,
        seed_label: SEED_LABEL,
        seed_label_sha256: SEED_LABEL_SHA256,
        recipe_sha256: &recipe_sha256,
        config_sha256: &config_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    #[derive(Serialize)]
    struct RecipeRecord<'a> {
        record: &'static str,
        recipe_sha256: &'a str,
        recipes: &'a [Vec<Recipe>],
    }
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_sha256: &recipe_sha256,
        recipes: &recipes,
    })?;
    for arm in &arms {
        output.write(arm)?;
    }
    #[derive(Serialize)]
    struct ClassificationRecord<'a> {
        record: &'static str,
        pairs: &'a [PairClassification],
    }
    output.write(&ClassificationRecord {
        record: "classification",
        pairs: &classification.pairs,
    })?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        verdict: classification.verdict,
        treatment_wins: classification.treatment_wins,
        control_wins: classification.control_wins,
        ties: classification.ties,
        sign_tail_numerator: classification.sign_tail_numerator,
        sign_tail_denominator: classification.sign_tail_denominator,
        directional_gate: classification.directional_gate,
        structural_gate: classification.structural_gate,
        distinct_admitted_prefix_snapshots: classification.distinct_admitted_prefix_snapshots,
        admitted_prefix_pairs: classification.admitted_prefix_pairs,
        distinct_selected_prefix_snapshots: classification.distinct_selected_prefix_snapshots,
        selected_prefix_pairs: classification.selected_prefix_pairs,
        distinct_descendant_snapshots: classification.distinct_descendant_snapshots,
        any_beyond_prefix_descendant: classification.any_beyond_prefix_descendant,
        winning_max_direct_descendant: classification.winning_max_direct_descendant,
        pair_classifications: classification.pairs,
        worker_setup_frames,
        setup_frames: work.setup,
        source_replay_frames: baseline.record.replay_frames,
        full_action_frames: work.full,
        endpoint_probe_frames: work.endpoint_probe,
        prefix_action_frames: work.prefix,
        prefix_probe_frames: work.prefix_probe,
        experimental_frames: work.experimental,
        total_frames: work.total,
    };
    output.write(&summary)?;
    let report_sha256 = output.finish()?;
    println!(
        "{{\"report_sha256\":\"{report_sha256}\",\"verdict\":{}}}",
        serde_json::to_string(&summary.verdict)?
    );
    Ok(())
}

fn validate_source(source: &SmbInput) -> Result<(), Box<dyn Error>> {
    if source.actions.len() != SOURCE_ACTIONS {
        return Err("source action count does not match the preregistration".into());
    }
    if source
        .actions
        .iter()
        .any(|action| !(2..=MAX_HOLD_FRAMES).contains(&action.hold_frames))
    {
        return Err("source action duration is outside the registered 2..=120 range".into());
    }
    Ok(())
}

fn derive_recipes(source: &SmbInput) -> Result<Vec<Vec<Recipe>>, Box<dyn Error>> {
    let source_len = u64::try_from(source.actions.len())?;
    if source_len == 0 {
        return Err("cannot derive recipes from an empty source".into());
    }
    let mut pairs = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        let mut pair_hasher = Sha256::new();
        pair_hasher.update(MASTER_SEED.to_le_bytes());
        pair_hasher.update(b"paired-admission-pair");
        pair_hasher.update(u64::try_from(pair)?.to_le_bytes());
        let pair_digest = pair_hasher.finalize();
        let pair_seed = u64::from_le_bytes(
            pair_digest[..8]
                .try_into()
                .map_err(|_| "pair digest is shorter than eight bytes")?,
        );
        let mut draws = Vec::with_capacity(DRAWS);
        for draw in 0..DRAWS {
            let draw_u64 = u64::try_from(draw)?;
            let mut action_hasher = Sha256::new();
            action_hasher.update(pair_seed.to_le_bytes());
            action_hasher.update(b"paired-admission-action");
            action_hasher.update(draw_u64.to_le_bytes());
            let action_digest = action_hasher.finalize();
            let source_word = u64::from_le_bytes(
                action_digest[..8]
                    .try_into()
                    .map_err(|_| "action digest is shorter than eight bytes")?,
            );
            let source_index = usize::try_from(source_word % source_len)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived source index is out of bounds")?;

            let mut selector_hasher = Sha256::new();
            selector_hasher.update(pair_seed.to_le_bytes());
            selector_hasher.update(b"paired-admission-parent");
            selector_hasher.update(draw_u64.to_le_bytes());
            let selector_digest = selector_hasher.finalize();
            let selector_seed = u64::from_le_bytes(
                selector_digest[..8]
                    .try_into()
                    .map_err(|_| "selector digest is shorter than eight bytes")?,
            );
            draws.push(Recipe {
                pair,
                draw,
                source_index,
                action,
                selector_seed,
            });
        }
        pairs.push(draws);
    }
    Ok(pairs)
}

fn build_baseline(target: &mut SmbTarget, source: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
    let setup_frames = target.frames_clocked();
    if setup_frames != EXPECTED_SETUP_FRAMES {
        return Err("baseline target setup work does not match the sealed value".into());
    }
    target.reset();
    if target.exit_kind() != ExitKind::Ok || target.is_dead() {
        return Err("SMB gameplay genesis is not live".into());
    }
    let work_before = target.frames_clocked();
    let initial = target.observe();
    let mut trace = Sha256::new();
    trace.update(TRACE_DOMAIN);
    hash_framed_json(&mut trace, &initial)?;
    let mut watermark = watermark(initial.decoded);
    let mut milestones = initial.milestones;
    for (index, action) in source.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok || target.is_dead() {
            return Err("registered source did not replay alive".into());
        }
        trace.update(u64::try_from(index)?.to_le_bytes());
        hash_framed_json(&mut trace, action)?;
        hash_framed_json(&mut trace, target.last_action_observations())?;
        merge_progress_watermark(&mut watermark, target.last_action_observations());
        merge_action_milestones(&mut milestones, target)?;
    }
    let replay_frames = target
        .frames_clocked()
        .checked_sub(work_before)
        .ok_or("baseline work counter moved backwards")?;
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let snapshot = target
        .snapshot()
        .ok_or("failed to snapshot source endpoint")?;
    let record = BaselineRecord {
        record: "baseline",
        setup_frames,
        replay_frames,
        actions: source.actions.len(),
        endpoint,
        watermark,
        trace_sha256: finish_sha256(trace),
        wram_sha256: sha256_bytes(target.wram()),
        snapshot_sha256: sha256_json(&snapshot)?,
        key: archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen),
        milestones,
    };
    if replay_frames != SOURCE_FRAMES
        || target.observe().frame_count != SOURCE_FRAMES
        || record.endpoint != BASELINE_ENDPOINT
        || record.watermark != BASELINE_WATERMARK
        || record.trace_sha256 != SOURCE_TRACE_SHA256
        || record.wram_sha256 != SOURCE_WRAM_SHA256
        || record.snapshot_sha256 != SOURCE_SNAPSHOT_SHA256
    {
        return Err("source replay evidence does not match the preregistration".into());
    }
    Ok(Baseline { record, snapshot })
}

fn evaluate_parallel(
    rom: &[u8],
    source: &SmbInput,
    recipes: &[Vec<Recipe>],
    baseline: &Baseline,
) -> Result<(Vec<ArmRecord>, Vec<u64>), Box<dyn Error>> {
    if recipes.len() != PAIRS || recipes.iter().any(|pair| pair.len() != DRAWS) {
        return Err("recipe shape does not match the preregistration".into());
    }
    let (arm_sender, arm_receiver) = mpsc::channel();
    let (setup_sender, setup_receiver) = mpsc::channel();
    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let assigned = (0..ARMS)
                .filter(|ordinal| ordinal % WORKERS == worker)
                .collect::<Vec<_>>();
            let arm_sender = arm_sender.clone();
            let setup_sender = setup_sender.clone();
            let source = source.clone();
            let recipes = recipes.to_vec();
            let baseline = baseline.clone();
            let handle = thread::Builder::new()
                .name(format!("prefix-admission-{worker}"))
                .spawn_scoped(scope, move || {
                    let (mut state, setup) = match SmbTarget::from_smb_rom_bytes_headless(rom) {
                        Ok(target) => {
                            let frames = target.frames_clocked();
                            (WorkerState::Ready(Box::new(target)), Ok(frames))
                        }
                        Err(error) => {
                            let message = error.to_string();
                            (WorkerState::Failed(message.clone()), Err(message))
                        }
                    };
                    if setup_sender
                        .send(SetupReply {
                            worker,
                            result: setup,
                        })
                        .is_err()
                    {
                        return;
                    }
                    for ordinal in assigned {
                        let pair = ordinal / 2;
                        let treatment = ordinal % 2 == 1;
                        let result = match &mut state {
                            WorkerState::Ready(target) => run_arm(
                                target,
                                &source,
                                &recipes[pair],
                                &baseline,
                                ordinal,
                                worker,
                                pair,
                                treatment,
                            )
                            .map_err(|error| error.to_string()),
                            WorkerState::Failed(error) => Err(error.clone()),
                        };
                        if arm_sender
                            .send(ArmReply {
                                ordinal,
                                worker,
                                result,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })?;
            handles.push(handle);
        }
        drop(arm_sender);
        drop(setup_sender);
        for handle in handles {
            handle
                .join()
                .map_err(|_| "prefix-admission worker panicked")?;
        }
        Ok(())
    })?;
    let arms = consume_arm_replies(arm_receiver.into_iter().collect())?;
    let setups = consume_setup_replies(setup_receiver.into_iter().collect())?;
    Ok((arms, setups))
}

fn consume_arm_replies(replies: Vec<ArmReply>) -> Result<Vec<ArmRecord>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    for reply in replies {
        if reply.ordinal >= ARMS || reply.worker != reply.ordinal % WORKERS {
            return Err("arm reply has an invalid ordinal or worker".into());
        }
        if buffered.insert(reply.ordinal, reply.result).is_some() {
            return Err("duplicate arm reply".into());
        }
    }
    let mut arms = Vec::with_capacity(ARMS);
    for ordinal in 0..ARMS {
        let result = buffered.remove(&ordinal).ok_or("missing arm reply")?;
        arms.push(result.map_err(|error| format!("arm {ordinal}: {error}"))?);
    }
    Ok(arms)
}

fn consume_setup_replies(replies: Vec<SetupReply>) -> Result<Vec<u64>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    for reply in replies {
        if reply.worker >= WORKERS || buffered.insert(reply.worker, reply.result).is_some() {
            return Err("invalid or duplicate setup reply".into());
        }
    }
    let mut setups = Vec::with_capacity(WORKERS);
    for worker in 0..WORKERS {
        let frames = buffered
            .remove(&worker)
            .ok_or("missing worker setup reply")?
            .map_err(|error| format!("worker {worker}: {error}"))?;
        if frames != EXPECTED_SETUP_FRAMES {
            return Err("worker setup work does not match the sealed value".into());
        }
        setups.push(frames);
    }
    Ok(setups)
}

#[allow(clippy::too_many_arguments)]
fn run_arm(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipes: &[Recipe],
    baseline: &Baseline,
    ordinal: usize,
    worker: usize,
    pair: usize,
    treatment: bool,
) -> Result<ArmRecord, Box<dyn Error>> {
    let mut archive = Archive::new();
    archive.max_entries = ARCHIVE_LIMIT;
    archive.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
    archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Absent);
    archive.set_replacement_policy(SmbArchiveReplacementPolicy::FewestActions);
    let origin_id = archive.insert(
        None,
        0,
        ArchiveCandidate {
            input: source.clone(),
            key: baseline.record.key,
            milestones: baseline.record.milestones,
        },
        baseline.snapshot.clone(),
    )?;
    if origin_id != Some(0)
        || archive.entries.len() != 1
        || archive.active.as_slice() != [true]
        || archive.input_ids.get(source) != Some(&0)
    {
        return Err("arm origin archive did not initialize exactly".into());
    }
    let initial_archive_sha256 = sha256_json(&(
        &archive.entries[0].report,
        &archive.entries[0].snapshot,
        archive.max_entries,
        archive.active[0],
        "concentrated_recency",
        "fewest_actions",
        "absent_waypoint",
    ))?;
    let mut origins = vec![EntryOrigin::Source];
    let mut snapshot_hashes = vec![baseline.record.snapshot_sha256.clone()];
    let mut draws = Vec::with_capacity(DRAWS);
    let mut admitted_prefixes = Vec::new();
    let mut selected_prefix_ids = BTreeSet::new();
    let mut direct_descendants = Vec::new();
    let arm_work_before = target.frames_clocked();
    let mut totals = WorkTotals::default();

    for recipe in recipes {
        if recipe.pair != pair || recipe.draw != draws.len() {
            return Err("arm recipe order is not canonical".into());
        }
        let mut rand = StdRand::with_seed(recipe.selector_seed);
        let (parent_id, selector) = archive.select_parent(&mut rand, ACTION_LIMIT)?;
        let selector = selector.ok_or("normal selector omitted its draw record")?;
        let parent = archive
            .entries
            .get(parent_id)
            .ok_or("selector returned a missing parent")?;
        let parent_report = parent.report.clone();
        let parent_snapshot = parent.snapshot.clone();
        let parent_origin = *origins.get(parent_id).ok_or("missing parent provenance")?;
        let parent_snapshot_sha256 = snapshot_hashes
            .get(parent_id)
            .ok_or("missing parent snapshot identity")?
            .clone();
        if parent_origin == EntryOrigin::Prefix {
            selected_prefix_ids.insert(parent_id);
        }
        target.restore(&parent_snapshot)?;
        verify_snapshot(target, &parent_snapshot)?;
        let start_observation = target.observe();
        let start = StartEvidence {
            observation: start_observation.clone(),
            wram_sha256: sha256_bytes(target.wram()),
            dead: target.is_dead(),
            failed: target.exit_kind() != ExitKind::Ok,
            instance_work_frames: target.frames_clocked(),
            milestones: parent_report.milestones,
        };
        let draw_work_before = target.frames_clocked();

        let full_apply_before = target.frames_clocked();
        target.apply(&recipe.action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a full action".into());
        }
        let full_action_frames = target
            .frames_clocked()
            .checked_sub(full_apply_before)
            .ok_or("full-action work counter moved backwards")?;
        if full_action_frames > u64::from(recipe.action.bounded_hold_frames()) {
            return Err("full action exceeded its bounded duration".into());
        }
        if !target.is_dead() && full_action_frames != u64::from(recipe.action.bounded_hold_frames())
        {
            return Err("live full action did not execute its requested duration".into());
        }
        let full_observations = target.last_action_observations().to_vec();
        let full_endpoint_frame = target.observe().frame_count;
        let full_dead = target.is_dead();
        let full_endpoint = smb_mechanical_state_from_wram(target.wram());
        let full_wram_sha256 = sha256_bytes(target.wram());
        let mut full_milestones = parent_report.milestones;
        merge_action_milestones(&mut full_milestones, target)?;
        let full_input = appended_input(&parent_report.input, recipe.action)?;
        let full_input_sha256 = sha256_json(&full_input)?;

        let mut full_snapshot = None;
        let mut full_snapshot_sha256 = None;
        let mut full_key = None;
        let mut full_probe = Vec::new();
        let mut full_probe_survived = false;
        let mut full_admission = AdmissionOutcome::NotCandidate;
        let mut endpoint_probe_frames = 0_u64;
        let mut direct_descendant = None;
        if !full_dead {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot full endpoint")?;
            let snapshot_sha256 = sha256_json(&snapshot)?;
            let key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
            let (attempts, survived, work) = run_probe(target, &snapshot)?;
            endpoint_probe_frames = work;
            full_probe = attempts;
            full_probe_survived = survived;
            if survived {
                let outcome = insert_candidate(
                    &mut archive,
                    Some(parent_id),
                    u64::try_from(recipe.draw.checked_add(1).ok_or("execution overflow")?)?,
                    ArchiveCandidate {
                        input: full_input,
                        key,
                        milestones: full_milestones,
                    },
                    snapshot.clone(),
                    EntryOrigin::Endpoint {
                        direct_prefix_parent: parent_origin == EntryOrigin::Prefix,
                    },
                    snapshot_sha256.clone(),
                    &mut origins,
                    &mut snapshot_hashes,
                )?;
                if let Some(id) = outcome.retained_id()
                    && parent_origin == EntryOrigin::Prefix
                {
                    let parent_key = parent_report.key;
                    let descendant = DirectDescendant {
                        parent_id,
                        parent_snapshot_sha256: parent_snapshot_sha256.clone(),
                        id,
                        input_sha256: full_input_sha256.clone(),
                        snapshot_sha256: snapshot_sha256.clone(),
                        watermark: watermark_from_key(key),
                        beyond_parent: watermark_from_key(key) > watermark_from_key(parent_key),
                    };
                    direct_descendants.push(descendant.clone());
                    direct_descendant = Some(descendant);
                }
                full_admission = outcome;
            } else {
                full_admission = AdmissionOutcome::ProbeRefused;
            }
            full_snapshot = Some(snapshot);
            full_snapshot_sha256 = Some(snapshot_sha256);
            full_key = Some(key);
        }
        let full = CandidateEvidence {
            action: recipe.action,
            input_sha256: full_input_sha256.clone(),
            observations: full_observations.clone(),
            work_frames: full_action_frames,
            dead: full_dead,
            endpoint: full_endpoint,
            watermark: watermark(full_endpoint),
            wram_sha256: full_wram_sha256.clone(),
            snapshot_sha256: full_snapshot_sha256.clone(),
            key: full_key,
            probe: full_probe.clone(),
            probe_survived: full_probe_survived,
            admission: full_admission,
        };

        let prefix_event =
            first_strict_prefix_event(&start_observation, full_endpoint_frame, &full_observations);
        let mut prefix_action_frames = 0_u64;
        let mut prefix_probe_frames = 0_u64;
        let mut prefix = None;
        if let Some(expected) = prefix_event {
            let offset = expected
                .frame_count
                .checked_sub(start_observation.frame_count)
                .ok_or("prefix event precedes action start")?;
            let duration = u8::try_from(offset)?;
            if duration == 0 || duration >= recipe.action.bounded_hold_frames() {
                return Err("selected prefix duration is not strict interior".into());
            }
            let action = ButtonChord::new(recipe.action.buttons, duration);
            let prefix_input = appended_input(&parent_report.input, action)?;
            let prefix_input_sha256 = sha256_json(&prefix_input)?;
            target.restore(&parent_snapshot)?;
            verify_snapshot(target, &parent_snapshot)?;
            let prefix_before = target.frames_clocked();
            target.apply(&action);
            if target.exit_kind() != ExitKind::Ok || target.is_dead() {
                return Err("registered nonterminal prefix reconstructed terminally".into());
            }
            prefix_action_frames = target
                .frames_clocked()
                .checked_sub(prefix_before)
                .ok_or("prefix work counter moved backwards")?;
            if prefix_action_frames != offset
                || target.observe() != expected
                || target.wram().as_slice() != expected.wram.as_slice()
                || smb_mechanical_state_from_wram(target.wram()) != expected.decoded
            {
                return Err("shortened action did not reconstruct the emitted event".into());
            }
            let observations = target.last_action_observations().to_vec();
            let endpoint = smb_mechanical_state_from_wram(target.wram());
            let wram_sha256 = sha256_bytes(target.wram());
            let snapshot = target.snapshot().ok_or("failed to snapshot prefix")?;
            let snapshot_sha256 = sha256_json(&snapshot)?;
            let key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
            let mut milestones = parent_report.milestones;
            merge_action_milestones(&mut milestones, target)?;
            let (probe, survived, probe_work) = run_probe(target, &snapshot)?;
            prefix_probe_frames = probe_work;
            let admission = if !survived {
                AdmissionOutcome::ProbeRefused
            } else if treatment {
                let outcome = insert_candidate(
                    &mut archive,
                    Some(parent_id),
                    u64::try_from(recipe.draw.checked_add(1).ok_or("execution overflow")?)?,
                    ArchiveCandidate {
                        input: prefix_input.clone(),
                        key,
                        milestones,
                    },
                    snapshot.clone(),
                    EntryOrigin::Prefix,
                    snapshot_sha256.clone(),
                    &mut origins,
                    &mut snapshot_hashes,
                )?;
                if let Some(id) = outcome.retained_id() {
                    admitted_prefixes.push((
                        id,
                        prefix_input_sha256.clone(),
                        snapshot_sha256.clone(),
                    ));
                }
                outcome
            } else {
                AdmissionOutcome::ObservedOnly
            };
            prefix = Some(PrefixEvidence {
                expected: expected.clone(),
                candidate: CandidateEvidence {
                    action,
                    input_sha256: prefix_input_sha256,
                    observations,
                    work_frames: prefix_action_frames,
                    dead: false,
                    endpoint,
                    watermark: watermark(endpoint),
                    wram_sha256,
                    snapshot_sha256: Some(snapshot_sha256),
                    key: Some(key),
                    probe,
                    probe_survived: survived,
                    admission,
                },
            });
        }

        let draw_work = target
            .frames_clocked()
            .checked_sub(draw_work_before)
            .ok_or("draw work counter moved backwards")?;
        let component_work = full_action_frames
            .checked_add(endpoint_probe_frames)
            .and_then(|value| value.checked_add(prefix_action_frames))
            .and_then(|value| value.checked_add(prefix_probe_frames))
            .ok_or("draw component work overflow")?;
        if draw_work != component_work {
            return Err("draw work does not reconcile with its components".into());
        }
        let productive = matches!(full.admission, AdmissionOutcome::Retained { .. })
            || prefix.as_ref().is_some_and(|evidence| {
                matches!(
                    evidence.candidate.admission,
                    AdmissionOutcome::Retained { .. }
                )
            });
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, productive, draw_work)?;
        let projection = TargetProjection {
            full_action: recipe.action,
            full_observations,
            full_work_frames: full_action_frames,
            full_dead,
            full_endpoint,
            full_wram_sha256,
            full_snapshot_sha256: full_snapshot.as_ref().map(sha256_json).transpose()?,
            full_probe,
            prefix_expected: prefix.as_ref().map(|value| value.expected.clone()),
            prefix_observations: prefix
                .as_ref()
                .map(|value| value.candidate.observations.clone()),
            prefix_work_frames: prefix_action_frames,
            prefix_endpoint: prefix.as_ref().map(|value| value.candidate.endpoint),
            prefix_wram_sha256: prefix
                .as_ref()
                .map(|value| value.candidate.wram_sha256.clone()),
            prefix_snapshot_sha256: prefix
                .as_ref()
                .and_then(|value| value.candidate.snapshot_sha256.clone()),
            prefix_probe: prefix
                .as_ref()
                .map(|value| value.candidate.probe.clone())
                .unwrap_or_default(),
        };
        let target_projection_sha256 = sha256_json(&projection)?;
        let active_endpoint_maximum = final_maximum(&archive, &origins)?;
        totals.add_draw(
            full_action_frames,
            endpoint_probe_frames,
            prefix_action_frames,
            prefix_probe_frames,
        )?;
        draws.push(DrawRecord {
            draw: recipe.draw,
            source_index: recipe.source_index,
            selector_seed: recipe.selector_seed,
            selector,
            parent_id,
            parent_origin,
            parent_input_sha256: sha256_json(&parent_report.input)?,
            parent_snapshot_sha256,
            start,
            full,
            prefix,
            target_projection_sha256,
            productive,
            direct_descendant,
            active_endpoint_maximum,
            full_action_frames,
            endpoint_probe_frames,
            prefix_action_frames,
            prefix_probe_frames,
            total_work_frames: draw_work,
        });
    }
    let arm_delta = target
        .frames_clocked()
        .checked_sub(arm_work_before)
        .ok_or("arm work counter moved backwards")?;
    if arm_delta != totals.total()? {
        return Err("arm work does not reconcile".into());
    }
    let final_maximum = final_maximum(&archive, &origins)?;
    Ok(ArmRecord {
        record: "arm",
        ordinal,
        worker,
        pair,
        treatment,
        initial_archive_sha256,
        draws,
        admitted_prefixes,
        selected_prefix_ids: selected_prefix_ids.into_iter().collect(),
        direct_descendants,
        final_maximum,
        full_action_frames: totals.full,
        endpoint_probe_frames: totals.endpoint_probe,
        prefix_action_frames: totals.prefix,
        prefix_probe_frames: totals.prefix_probe,
        total_work_frames: totals.total()?,
    })
}

fn first_strict_prefix_event(
    start: &SmbObservations,
    endpoint_frame: u64,
    observations: &[SmbObservations],
) -> Option<SmbObservations> {
    observations
        .iter()
        .find(|observation| {
            observation.frame_count > start.frame_count
                && observation.frame_count < endpoint_frame
                && !observation.dead
        })
        .cloned()
}

fn appended_input(parent: &SmbInput, action: ButtonChord) -> Result<SmbInput, Box<dyn Error>> {
    let capacity = parent
        .actions
        .len()
        .checked_add(1)
        .ok_or("candidate input length overflow")?;
    let mut actions = Vec::with_capacity(capacity);
    actions.extend_from_slice(&parent.actions);
    actions.push(action);
    Ok(SmbInput { actions })
}

#[allow(clippy::too_many_arguments)]
fn insert_candidate(
    archive: &mut Archive,
    parent_id: Option<usize>,
    execution: u64,
    candidate: ArchiveCandidate,
    snapshot: SmbSnapshot,
    origin: EntryOrigin,
    snapshot_sha256: String,
    origins: &mut Vec<EntryOrigin>,
    snapshot_hashes: &mut Vec<String>,
) -> Result<AdmissionOutcome, Box<dyn Error>> {
    let before_len = archive.entries.len();
    let before_active = archive.active.iter().filter(|active| **active).count();
    let result = archive.insert(parent_id, execution, candidate, snapshot)?;
    match result {
        Some(id) if id < before_len => {
            if archive.entries.len() != before_len {
                return Err("duplicate insertion changed archive length".into());
            }
            Ok(AdmissionOutcome::Duplicate { id })
        }
        Some(id) if id == before_len => {
            if archive.entries.len() != before_len.checked_add(1).ok_or("archive overflow")?
                || origins.len() != before_len
                || snapshot_hashes.len() != before_len
            {
                return Err("retained insertion did not append exactly one entry".into());
            }
            origins.push(origin);
            snapshot_hashes.push(snapshot_sha256);
            let after_active = archive.active.iter().filter(|active| **active).count();
            let displaced = after_active == before_active;
            if after_active != before_active
                && after_active != before_active.checked_add(1).ok_or("active overflow")?
            {
                return Err("retained insertion changed active count unexpectedly".into());
            }
            Ok(AdmissionOutcome::Retained { id, displaced })
        }
        Some(_) => Err("archive returned a noncanonical retained id".into()),
        None => {
            if archive.entries.len() != before_len {
                return Err("rejected insertion changed archive length".into());
            }
            Ok(AdmissionOutcome::Rejected)
        }
    }
}

fn run_probe(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
) -> Result<(Vec<ProbeAttempt>, bool, u64), Box<dyn Error>> {
    let before = target.frames_clocked();
    let mut attempts = Vec::with_capacity(PROBE_MASKS.len());
    let mut survived = false;
    for mask in PROBE_MASKS {
        target.restore(snapshot)?;
        verify_snapshot(target, snapshot)?;
        let attempt_before = target.frames_clocked();
        let this_survived = target.survives_probe(mask, PROBE_FRAMES);
        let work_frames = target
            .frames_clocked()
            .checked_sub(attempt_before)
            .ok_or("probe work counter moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a viability probe".into());
        }
        attempts.push(ProbeAttempt {
            mask,
            work_frames,
            dead: target.is_dead(),
            survived: this_survived,
        });
        if this_survived {
            survived = true;
            break;
        }
    }
    target.restore(snapshot)?;
    verify_snapshot(target, snapshot)?;
    let total = target
        .frames_clocked()
        .checked_sub(before)
        .ok_or("probe total moved backwards")?;
    let summed = attempts.iter().try_fold(0_u64, |sum, attempt| {
        sum.checked_add(attempt.work_frames)
            .ok_or("probe attempt work overflow")
    })?;
    if total != summed || total > u64::from(PROBE_FRAMES) * 3 {
        return Err("probe work does not reconcile".into());
    }
    Ok((attempts, survived, total))
}

fn verify_snapshot(target: &mut SmbTarget, expected: &SmbSnapshot) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Err("restored snapshot has a failed exit kind".into());
    }
    let actual = target
        .snapshot()
        .ok_or("failed to resnapshot restored candidate")?;
    if &actual != expected {
        return Err("restored snapshot is not byte-exact".into());
    }
    Ok(())
}

fn final_maximum(
    archive: &Archive,
    origins: &[EntryOrigin],
) -> Result<FinalMaximum, Box<dyn Error>> {
    if archive.entries.len() != origins.len() || archive.active.len() != origins.len() {
        return Err("archive provenance is misaligned".into());
    }
    let mut maximum = None;
    for (id, entry) in archive.entries.iter().enumerate() {
        if !archive.active[id] || origins[id] == EntryOrigin::Prefix {
            continue;
        }
        let candidate = watermark_from_key(entry.report.key);
        maximum = Some(maximum.map_or(candidate, |current: SmbProgressWatermark| {
            current.max(candidate)
        }));
    }
    let watermark = maximum.ok_or("arm has no active endpoint/source entry")?;
    let mut active_ids = Vec::new();
    let mut direct = false;
    for (id, entry) in archive.entries.iter().enumerate() {
        if archive.active[id]
            && origins[id] != EntryOrigin::Prefix
            && watermark_from_key(entry.report.key) == watermark
        {
            active_ids.push(id);
            direct |= matches!(
                origins[id],
                EntryOrigin::Endpoint {
                    direct_prefix_parent: true
                }
            );
        }
    }
    Ok(FinalMaximum {
        watermark,
        active_ids,
        attained_by_active_direct_descendant: direct,
    })
}

fn validate_arm_ordinals(arms: &[ArmRecord]) -> Result<(), Box<dyn Error>> {
    if arms.len() != ARMS {
        return Err("arm count does not match the preregistration".into());
    }
    for (ordinal, arm) in arms.iter().enumerate() {
        if arm.ordinal != ordinal
            || arm.worker != ordinal % WORKERS
            || arm.pair != ordinal / 2
            || arm.treatment != (ordinal % 2 == 1)
            || arm.draws.len() != DRAWS
        {
            return Err("arm identity or draw count is noncanonical".into());
        }
    }
    let origins = arms
        .iter()
        .map(|arm| arm.initial_archive_sha256.as_str())
        .collect::<BTreeSet<_>>();
    if origins.len() != 1 {
        return Err("initial archive identity differs between arms".into());
    }
    Ok(())
}

fn validate_pure_target_equivalence(arms: &[ArmRecord]) -> Result<(), Box<dyn Error>> {
    let mut projections = BTreeMap::<(String, ButtonChord), String>::new();
    for arm in arms {
        for draw in &arm.draws {
            let key = (draw.parent_snapshot_sha256.clone(), draw.full.action);
            if let Some(existing) = projections.insert(key, draw.target_projection_sha256.clone())
                && existing != draw.target_projection_sha256
            {
                return Err("identical target inputs produced different pure evidence".into());
            }
        }
    }
    Ok(())
}

struct Classification {
    verdict: Verdict,
    treatment_wins: usize,
    control_wins: usize,
    ties: usize,
    sign_tail_numerator: u128,
    sign_tail_denominator: u128,
    directional_gate: bool,
    structural_gate: bool,
    distinct_admitted_prefix_snapshots: usize,
    admitted_prefix_pairs: usize,
    distinct_selected_prefix_snapshots: usize,
    selected_prefix_pairs: usize,
    distinct_descendant_snapshots: usize,
    any_beyond_prefix_descendant: bool,
    winning_max_direct_descendant: bool,
    pairs: Vec<PairClassification>,
}

fn classify(arms: &[ArmRecord]) -> Result<Classification, Box<dyn Error>> {
    validate_arm_ordinals(arms)?;
    classify_validated(arms)
}

fn classify_validated(arms: &[ArmRecord]) -> Result<Classification, Box<dyn Error>> {
    if arms.len() != ARMS {
        return Err("classification requires every registered arm".into());
    }
    let mut pair_classifications = Vec::with_capacity(PAIRS);
    let mut treatment_wins = 0_usize;
    let mut control_wins = 0_usize;
    let mut ties = 0_usize;
    for pair in 0..PAIRS {
        let control = &arms[pair * 2];
        let treatment = &arms[pair * 2 + 1];
        let treatment_win = treatment.final_maximum.watermark > control.final_maximum.watermark;
        let control_win = control.final_maximum.watermark > treatment.final_maximum.watermark;
        let tie = !treatment_win && !control_win;
        treatment_wins = treatment_wins
            .checked_add(usize::from(treatment_win))
            .ok_or("treatment win overflow")?;
        control_wins = control_wins
            .checked_add(usize::from(control_win))
            .ok_or("control win overflow")?;
        ties = ties.checked_add(usize::from(tie)).ok_or("tie overflow")?;
        pair_classifications.push(PairClassification {
            pair,
            control: control.final_maximum.watermark,
            treatment: treatment.final_maximum.watermark,
            treatment_win,
            control_win,
            tie,
            treatment_max_direct_descendant: treatment
                .final_maximum
                .attained_by_active_direct_descendant,
        });
    }
    let (sign_tail_numerator, sign_tail_denominator) = sign_tail(treatment_wins, control_wins)?;
    let directional_gate = treatment_wins > control_wins
        && sign_tail_numerator
            .checked_mul(20)
            .ok_or("sign-tail comparison overflow")?
            <= sign_tail_denominator;

    let treatment = arms.iter().filter(|arm| arm.treatment).collect::<Vec<_>>();
    let admitted_snapshots = treatment
        .iter()
        .flat_map(|arm| {
            arm.admitted_prefixes
                .iter()
                .map(|(_, _, snapshot)| snapshot.clone())
        })
        .collect::<BTreeSet<_>>();
    let admitted_pairs = treatment
        .iter()
        .filter(|arm| !arm.admitted_prefixes.is_empty())
        .map(|arm| arm.pair)
        .collect::<BTreeSet<_>>();
    let mut selected_snapshots = BTreeSet::new();
    let mut selected_pairs = BTreeSet::new();
    for arm in &treatment {
        let admitted = arm
            .admitted_prefixes
            .iter()
            .map(|(id, _, snapshot)| (*id, snapshot))
            .collect::<BTreeMap<_, _>>();
        for id in &arm.selected_prefix_ids {
            if let Some(snapshot) = admitted.get(id) {
                selected_snapshots.insert((*snapshot).clone());
                selected_pairs.insert(arm.pair);
            }
        }
    }
    let descendant_snapshots = treatment
        .iter()
        .flat_map(|arm| {
            arm.direct_descendants
                .iter()
                .map(|descendant| descendant.snapshot_sha256.clone())
        })
        .collect::<BTreeSet<_>>();
    let any_beyond_prefix_descendant = treatment.iter().any(|arm| {
        arm.direct_descendants
            .iter()
            .any(|descendant| descendant.beyond_parent)
    });
    let winning_max_direct_descendant = pair_classifications
        .iter()
        .any(|pair| pair.treatment_win && pair.treatment_max_direct_descendant);
    let structural_gate = admitted_snapshots.len() >= 2
        && admitted_pairs.len() >= 2
        && selected_snapshots.len() >= 2
        && selected_pairs.len() >= 2
        && descendant_snapshots.len() >= 2
        && any_beyond_prefix_descendant
        && winning_max_direct_descendant;
    let verdict = if directional_gate && structural_gate {
        Verdict::Go
    } else if admitted_snapshots.is_empty()
        || selected_snapshots.is_empty()
        || !any_beyond_prefix_descendant
    {
        Verdict::Stop
    } else {
        Verdict::Inconclusive
    };
    Ok(Classification {
        verdict,
        treatment_wins,
        control_wins,
        ties,
        sign_tail_numerator,
        sign_tail_denominator,
        directional_gate,
        structural_gate,
        distinct_admitted_prefix_snapshots: admitted_snapshots.len(),
        admitted_prefix_pairs: admitted_pairs.len(),
        distinct_selected_prefix_snapshots: selected_snapshots.len(),
        selected_prefix_pairs: selected_pairs.len(),
        distinct_descendant_snapshots: descendant_snapshots.len(),
        any_beyond_prefix_descendant,
        winning_max_direct_descendant,
        pairs: pair_classifications,
    })
}

fn sign_tail(wins: usize, losses: usize) -> Result<(u128, u128), Box<dyn Error>> {
    let n = wins.checked_add(losses).ok_or("sign sample overflow")?;
    if n > PAIRS || wins > n {
        return Err("invalid sign-test counts".into());
    }
    let mut numerator = 0_u128;
    for k in wins..=n {
        numerator = numerator
            .checked_add(binomial(n, k)?)
            .ok_or("sign-tail numerator overflow")?;
    }
    let shift = u32::try_from(n)?;
    let denominator = 1_u128
        .checked_shl(shift)
        .ok_or("sign-tail denominator overflow")?;
    Ok((numerator, denominator))
}

fn binomial(n: usize, k: usize) -> Result<u128, Box<dyn Error>> {
    if k > n {
        return Ok(0);
    }
    let k = k.min(n - k);
    let mut value = 1_u128;
    for index in 0..k {
        value = value
            .checked_mul(u128::try_from(n - index)?)
            .ok_or("binomial multiply overflow")?
            .checked_div(u128::try_from(index + 1)?)
            .ok_or("binomial division failed")?;
    }
    Ok(value)
}

#[derive(Default)]
struct WorkTotals {
    full: u64,
    endpoint_probe: u64,
    prefix: u64,
    prefix_probe: u64,
}

impl WorkTotals {
    fn add_draw(
        &mut self,
        full: u64,
        endpoint_probe: u64,
        prefix: u64,
        prefix_probe: u64,
    ) -> Result<(), Box<dyn Error>> {
        self.full = self.full.checked_add(full).ok_or("full work overflow")?;
        self.endpoint_probe = self
            .endpoint_probe
            .checked_add(endpoint_probe)
            .ok_or("endpoint probe work overflow")?;
        self.prefix = self
            .prefix
            .checked_add(prefix)
            .ok_or("prefix work overflow")?;
        self.prefix_probe = self
            .prefix_probe
            .checked_add(prefix_probe)
            .ok_or("prefix probe work overflow")?;
        Ok(())
    }

    fn total(&self) -> Result<u64, Box<dyn Error>> {
        self.full
            .checked_add(self.endpoint_probe)
            .and_then(|value| value.checked_add(self.prefix))
            .and_then(|value| value.checked_add(self.prefix_probe))
            .ok_or_else(|| "experimental work overflow".into())
    }
}

struct WorkSummary {
    setup: u64,
    full: u64,
    endpoint_probe: u64,
    prefix: u64,
    prefix_probe: u64,
    experimental: u64,
    total: u64,
}

fn summarize_work(
    arms: &[ArmRecord],
    baseline_setup: u64,
    worker_setups: &[u64],
) -> Result<WorkSummary, Box<dyn Error>> {
    if baseline_setup != EXPECTED_SETUP_FRAMES
        || worker_setups.len() != WORKERS
        || worker_setups
            .iter()
            .any(|frames| *frames != EXPECTED_SETUP_FRAMES)
    {
        return Err("setup work does not match the preregistration".into());
    }
    let setup_count = worker_setups
        .len()
        .checked_add(1)
        .ok_or("setup count overflow")?;
    let setup = EXPECTED_SETUP_FRAMES
        .checked_mul(u64::try_from(setup_count)?)
        .ok_or("setup work overflow")?;
    let mut totals = WorkTotals::default();
    for arm in arms {
        totals.add_draw(
            arm.full_action_frames,
            arm.endpoint_probe_frames,
            arm.prefix_action_frames,
            arm.prefix_probe_frames,
        )?;
        if arm.total_work_frames
            != arm
                .full_action_frames
                .checked_add(arm.endpoint_probe_frames)
                .and_then(|value| value.checked_add(arm.prefix_action_frames))
                .and_then(|value| value.checked_add(arm.prefix_probe_frames))
                .ok_or("arm report work overflow")?
        {
            return Err("arm report work does not reconcile".into());
        }
    }
    if totals.full > MAX_FULL_ACTION_FRAMES
        || totals.endpoint_probe > MAX_ENDPOINT_PROBE_FRAMES
        || totals.prefix > MAX_PREFIX_ACTION_FRAMES
        || totals.prefix_probe > MAX_PREFIX_PROBE_FRAMES
    {
        return Err("a registered work component exceeded its cap".into());
    }
    let experimental = totals.total()?;
    let total = setup
        .checked_add(SOURCE_FRAMES)
        .and_then(|value| value.checked_add(experimental))
        .ok_or("total work overflow")?;
    if total > MAX_TOTAL_FRAMES {
        return Err("registered total work exceeded its cap".into());
    }
    Ok(WorkSummary {
        setup,
        full: totals.full,
        endpoint_probe: totals.endpoint_probe,
        prefix: totals.prefix,
        prefix_probe: totals.prefix_probe,
        experimental,
        total,
    })
}

fn watermark(state: SmbMechanicalState) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: state.world,
        level: state.level,
        progress: state.progress,
    }
}

fn watermark_from_key(key: SmbArchiveKey) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: key.world,
        level: key.level,
        progress: key.progress,
    }
}

fn read_bounded(path: &Path, limit: usize, label: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let read_limit = limit.checked_add(1).ok_or("bounded-read limit overflow")?;
    let take_limit = u64::try_from(read_limit)?;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(take_limit)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("{label} exceeds its registered byte bound").into());
    }
    Ok(bytes)
}

fn sha256_json<T: Serialize + ?Sized>(value: &T) -> Result<String, Box<dyn Error>> {
    Ok(sha256_bytes(&serde_json::to_vec(value)?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn finish_sha256(hasher: Sha256) -> String {
    format!("{:x}", hasher.finalize())
}

fn hash_framed_json<T: Serialize + ?Sized>(
    hasher: &mut Sha256,
    value: &T,
) -> Result<(), Box<dyn Error>> {
    let bytes = serde_json::to_vec(value)?;
    hasher.update(u64::try_from(bytes.len())?.to_le_bytes());
    hasher.update(bytes);
    Ok(())
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn observation(frame_count: u64, dead: bool) -> SmbObservations {
        SmbObservations {
            frame_count,
            wram: vec![0; 2_048],
            decoded: SmbMechanicalState::default(),
            milestones: SmbMilestones::default(),
            changed_indices: Vec::new(),
            dead,
            log_line: String::new(),
        }
    }

    fn synthetic_source() -> SmbInput {
        SmbInput {
            actions: (0..SOURCE_ACTIONS)
                .map(|index| {
                    ButtonChord::new(
                        u8::try_from(index % 256).expect("button fits"),
                        u8::try_from(2 + index % 119).expect("hold fits"),
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn frozen_recipe_bytes_are_exact() {
        let recipes = derive_recipes(&synthetic_source()).expect("derive recipes");
        assert_eq!(
            (recipes[0][0].source_index, recipes[0][0].selector_seed),
            (1925, 12_988_899_808_458_477_641)
        );
        assert_eq!(
            (recipes[0][1].source_index, recipes[0][1].selector_seed),
            (1596, 7_441_018_120_016_017_410)
        );
        assert_eq!(
            (recipes[1][0].source_index, recipes[1][0].selector_seed),
            (2230, 18_375_488_381_634_031_990)
        );
        assert_eq!(
            (recipes[7][127].source_index, recipes[7][127].selector_seed),
            (2061, 6_082_510_968_939_681_302)
        );
        let identities = recipes
            .iter()
            .flat_map(|pair| pair.iter())
            .map(|recipe| {
                (
                    u64::try_from(recipe.pair).expect("pair fits"),
                    u64::try_from(recipe.draw).expect("draw fits"),
                    u64::try_from(recipe.source_index).expect("index fits"),
                    recipe.action,
                    recipe.selector_seed,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sha256_json(&identities).expect("hash identities"),
            "5c2cccf683e019ce0a0066bc79f556ae6d3be3a19c8a9def5e3a6e3184da5e09"
        );
    }

    #[test]
    fn event_selection_is_strict_first_and_nonterminal() {
        let start = observation(100, false);
        let observations = vec![
            observation(100, false),
            observation(101, true),
            observation(102, false),
            observation(103, false),
            observation(104, false),
        ];
        assert_eq!(
            first_strict_prefix_event(&start, 104, &observations)
                .expect("event")
                .frame_count,
            102
        );
        assert!(first_strict_prefix_event(&start, 101, &observations).is_none());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn event_selection_never_chooses_a_boundary(
            start in 0_u64..10_000,
            hold in 2_u64..=120,
            first in 1_u64..120,
        ) {
            let endpoint = start + hold;
            let offset = first.min(hold - 1);
            let events = vec![
                observation(start, false),
                observation(start + offset, false),
                observation(endpoint, false),
            ];
            let selected = first_strict_prefix_event(&observation(start, false), endpoint, &events)
                .expect("strict event");
            prop_assert!(selected.frame_count > start);
            prop_assert!(selected.frame_count < endpoint);
        }
    }

    #[test]
    fn sign_gate_passing_cases_are_frozen() {
        let mut passing = Vec::new();
        for wins in 0..=PAIRS {
            for losses in 0..=PAIRS - wins {
                let (numerator, denominator) = sign_tail(wins, losses).expect("sign tail");
                if wins > losses && numerator * 20 <= denominator {
                    passing.push((wins, losses));
                }
            }
        }
        assert_eq!(passing, vec![(5, 0), (6, 0), (7, 0), (7, 1), (8, 0)]);
    }

    #[test]
    fn registered_work_cap_reconciles() {
        let total = MAX_FULL_ACTION_FRAMES
            + MAX_ENDPOINT_PROBE_FRAMES
            + MAX_PREFIX_ACTION_FRAMES
            + MAX_PREFIX_PROBE_FRAMES
            + SOURCE_FRAMES
            + EXPECTED_SETUP_FRAMES * 13;
        assert_eq!(total, MAX_TOTAL_FRAMES);
    }

    #[test]
    fn malformed_worker_reply_sets_are_rejected() {
        assert!(consume_arm_replies(Vec::new()).is_err());
        let setups = (0..WORKERS - 1)
            .map(|worker| SetupReply {
                worker,
                result: Ok(EXPECTED_SETUP_FRAMES),
            })
            .collect();
        assert!(consume_setup_replies(setups).is_err());
    }

    #[test]
    fn admission_productivity_excludes_nonallocations() {
        assert_eq!(AdmissionOutcome::Duplicate { id: 4 }.retained_id(), None);
        assert_eq!(AdmissionOutcome::Rejected.retained_id(), None);
        assert_eq!(
            AdmissionOutcome::Retained {
                id: 7,
                displaced: true,
            }
            .retained_id(),
            Some(7)
        );
    }

    fn key(progress: u16) -> SmbArchiveKey {
        SmbArchiveKey {
            world: 7,
            level: 0,
            progress,
            player_y_bucket: 1,
            player_engine_state: 8,
            state_fingerprint: u8::try_from(progress % 64).expect("fingerprint fits"),
            room_x_bucket: 0,
        }
    }

    fn fake_snapshot() -> SmbSnapshot {
        serde_json::from_value(serde_json::json!({
            "emulator_state": [],
            "observation": observation(0, false),
            "dead": false,
            "failed": false
        }))
        .expect("deserialize synthetic snapshot")
    }

    #[test]
    fn real_archive_selector_and_admission_are_exercised() {
        let mut archive = Archive::new();
        archive.max_entries = 3;
        archive.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
        let mut origins = Vec::new();
        let mut snapshots = Vec::new();
        for progress in [236, 237] {
            let action = ButtonChord::new(u8::try_from(progress % 8).expect("button fits"), 2);
            let input = SmbInput {
                actions: vec![action; usize::from(progress - 235)],
            };
            let outcome = insert_candidate(
                &mut archive,
                None,
                u64::from(progress - 235),
                ArchiveCandidate {
                    input,
                    key: key(progress),
                    milestones: SmbMilestones::default(),
                },
                fake_snapshot(),
                EntryOrigin::Endpoint {
                    direct_prefix_parent: false,
                },
                format!("snapshot-{progress}"),
                &mut origins,
                &mut snapshots,
            )
            .expect("insert candidate");
            assert!(matches!(outcome, AdmissionOutcome::Retained { .. }));
        }
        let mut rand = StdRand::with_seed(1234);
        let (selected, draw) = archive
            .select_parent(&mut rand, ACTION_LIMIT)
            .expect("select parent");
        let draw = draw.expect("selector draw");
        archive.record_selection(selected, &draw);
        archive
            .record_selection_outcome(selected, true, 17)
            .expect("record outcome");
        let accounting = archive.selector_report();
        assert_eq!(
            accounting.uniform_selections + accounting.tie_class_selections,
            1
        );
        assert_eq!(accounting.productive_selections, 1);
    }

    fn minimal_arm(ordinal: usize, progress: u16) -> ArmRecord {
        ArmRecord {
            record: "arm",
            ordinal,
            worker: ordinal % WORKERS,
            pair: ordinal / 2,
            treatment: ordinal % 2 == 1,
            initial_archive_sha256: "origin".to_owned(),
            draws: Vec::new(),
            admitted_prefixes: Vec::new(),
            selected_prefix_ids: Vec::new(),
            direct_descendants: Vec::new(),
            final_maximum: FinalMaximum {
                watermark: SmbProgressWatermark {
                    world: 7,
                    level: 0,
                    progress,
                },
                active_ids: vec![0],
                attained_by_active_direct_descendant: false,
            },
            full_action_frames: 0,
            endpoint_probe_frames: 0,
            prefix_action_frames: 0,
            prefix_probe_frames: 0,
            total_work_frames: 0,
        }
    }

    fn structurally_passing_arms(treatment_wins: usize) -> Vec<ArmRecord> {
        let mut arms = (0..ARMS)
            .map(|ordinal| minimal_arm(ordinal, 236))
            .collect::<Vec<_>>();
        for pair in 0..treatment_wins {
            arms[pair * 2 + 1].final_maximum.watermark.progress = 237;
        }
        for pair in 0..2 {
            let arm = &mut arms[pair * 2 + 1];
            let id = pair + 1;
            arm.admitted_prefixes.push((
                id,
                format!("prefix-input-{pair}"),
                format!("prefix-snapshot-{pair}"),
            ));
            arm.selected_prefix_ids.push(id);
            arm.direct_descendants.push(DirectDescendant {
                parent_id: id,
                parent_snapshot_sha256: format!("prefix-snapshot-{pair}"),
                id: pair + 20,
                input_sha256: format!("descendant-input-{pair}"),
                snapshot_sha256: format!("descendant-snapshot-{pair}"),
                watermark: SmbProgressWatermark {
                    world: 7,
                    level: 0,
                    progress: 237,
                },
                beyond_parent: true,
            });
        }
        arms[1].final_maximum.attained_by_active_direct_descendant = true;
        arms
    }

    #[test]
    fn classification_gates_are_nonvacuous_and_exhaustive() {
        let go = classify_validated(&structurally_passing_arms(5)).expect("classify GO");
        assert_eq!(go.verdict, Verdict::Go);
        assert!(go.directional_gate && go.structural_gate);

        let inconclusive =
            classify_validated(&structurally_passing_arms(4)).expect("classify inconclusive");
        assert_eq!(inconclusive.verdict, Verdict::Inconclusive);
        assert!(!inconclusive.directional_gate && inconclusive.structural_gate);

        let stopped = classify_validated(
            &(0..ARMS)
                .map(|ordinal| minimal_arm(ordinal, 236))
                .collect::<Vec<_>>(),
        )
        .expect("classify STOP");
        assert_eq!(stopped.verdict, Verdict::Stop);
    }

    #[test]
    fn verdict_and_ndjson_bytes_are_frozen() {
        assert_eq!(
            serde_json::to_string(&Verdict::Go).expect("GO JSON"),
            "\"GO\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Inconclusive).expect("INCONCLUSIVE JSON"),
            "\"INCONCLUSIVE\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Stop).expect("STOP JSON"),
            "\"STOP\""
        );
        let file = tempfile::tempfile().expect("temporary NDJSON");
        let mut output = NdjsonOutput::new(file);
        output
            .write(&serde_json::json!({"record": "fixture", "value": 1}))
            .expect("write fixture");
        assert_eq!(
            output.digest(),
            sha256_bytes(b"{\"record\":\"fixture\",\"value\":1}\n")
        );
    }
}
