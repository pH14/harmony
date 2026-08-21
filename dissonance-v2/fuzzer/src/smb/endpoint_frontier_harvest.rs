// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the World 8-2 p165 confirmatory L1/L4 phrase canary.

use std::{
    collections::BTreeMap,
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
            SmbSelectorAccounting, SmbSelectorDraw, archive_key, merge_action_milestones,
            merge_progress_watermark,
        },
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMechanicalState, SmbMilestones,
            SmbObservations, SmbProgressWatermark, SmbSnapshot, SmbTarget,
            smb_mechanical_state_from_wram,
        },
    },
    target::Target,
};

const FORMAT: &str = "smb-w8-2-p165-confirmatory-l1-l4-canary-v2";
const PREREGISTRATION_COMMIT: &str = "d64cc8de17cb40eee12f5b84a04a136bd1bc0138";
const PREREGISTRATION_DOC_SHA256: &str =
    "763c56646903c70dc113f7f5eb633b933a5a74a68f02d30bae6bc6ce5347fc6e";
const CODE_BASE: &str = "e8c3eb00dba5d5cf00bb1c2294a3c76d8eb0a494";
const AUTHORIZING_PAIRED_PREREGISTRATION: &str = "e94d5027";
const AUTHORIZING_PAIRED_IMPLEMENTATION: &str = "782081d4";
const AUTHORIZING_PAIRED_RESULT: &str = "e8c3eb00";
const AUTHORIZING_PAIRED_REPORT_SHA256: &str =
    "fa57d9118790b97b81147835aa3caa6a5b88eb8126752aa63658cbdedc010242";
const SOURCE_FILE_SHA256: &str = "42d92ae8b8a4ed47465302c75c5800b79a54a4990d07b8e1306af75217ce7321";
const SOURCE_INPUT_SHA256: &str =
    "42d92ae8b8a4ed47465302c75c5800b79a54a4990d07b8e1306af75217ce7321";
const SOURCE_WRAM_SHA256: &str = "83b7a658bd1c34828204840087b9c125456177155503eb7bfacbf7d3103f4185";
const SOURCE_SNAPSHOT_SHA256: &str =
    "fc69d5f71e7ac1d74c17b42eaa1fbf9bc0230bb109d23019cba8d99e7e853cba";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-w8-2-p165-confirmatory-l1-l4-phrase-canary-v2";
const SEED_LABEL_SHA256: &str = "291d75929cd4d2cd80214aa56eb69062b47fed43e0693c4eab87a218524c7774";
const MASTER_SEED: u64 = 14_831_150_291_821_600_041;
const EXPECTED_RECIPE_SHA256: &str =
    "98c1fa5c8122a7034ed2cd1f39f52e145fd4ca761e90d2300d60303e020ec83b";
const EXPECTED_RECIPE_BYTES: usize = 132_501;
const EXPECTED_PROJECTION_SHA256: [&str; 16] = [
    "f32ab6edb034998ed754b6679c0009c0ce522d01ad0998a63da9bb4ba6be8768",
    "d0cc79ad24201b4a29c83422548cba1331badf18f01ac3717f94047bd7f29800",
    "52de17cadc2d99460c344701f4f732bcf4bfc9e84530fc9a44b210d1998d393a",
    "b677b92d3c0978492b29c77d8a8035373e4987c0fdb654d04d90e2bdfb01a112",
    "930f9edb8d59ddebdc67701916f330adbf1d4025e6140d95a0b8b9e2efd5bf16",
    "7c307eed9d4706c636833083f28290ed5c220b3ff77699a7589a98c3aad50b5a",
    "ff8ea548df99d7de1daa65ecfbbc24087673d2d998532aef6fc8054d6ce8de30",
    "a096e828bbc7063209d864a47431e5d3fbfcfe4fc84c64be794af0393fc8889c",
    "74e55772af57ca0c3ce5fe469d14ba370151fb8d50b2a267ddd4ee955ca9f832",
    "4c8f7ddf8cd1429be6575457d2d29a72f7ae1ff67740d9c27e2d5547d4e7d7d9",
    "8f9d1940b6738ccc4a87e06e853cefb4fcb1551bc87e47d52eade3c18125fa3d",
    "bd804b45207408c3d3d437c5f5d818d833af4cc79e7412504be616e1958974da",
    "e91fee3760fa66717efb950accfd28456ff3c3bb642df78e1bf895bbcd0fb483",
    "17d02a43338861248dd8082545ad57988d8b6594af2ae82ece088308c5c00d0c",
    "6f8e5b8c886671bf0c5d90b457a131d8733f49db75e0fc07d13c04f8c0cf84bf",
    "93e26f3dd991d0dcce85a096d9b29a4e3792312be7610b1f13efff06af612657",
];
const EXPECTED_PROJECTION_BYTES: [usize; 16] = [
    7_976, 7_988, 7_938, 7_952, 8_000, 7_983, 7_989, 8_011, 7_968, 7_976, 8_002, 7_969, 7_983,
    7_970, 7_954, 7_993,
];
const SOURCE_ACTIONS: usize = 3_429;
const SOURCE_FRAMES: u64 = 160_502;
const PAIRS: usize = 16;
const ARMS: usize = 32;
const WORKERS: usize = 12;
const SLOTS: usize = 128;
const PHRASE_LENGTH: usize = 4;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 129;
const MAX_LINEAGE_ACTIONS: usize = 3_557;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 491_520;
const MAX_PROBE_FRAMES: u64 = 552_960;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 1_209_720;
const EXPECTED_L1_SELECTIONS: usize = 2_048;
const EXPECTED_L4_SELECTIONS: usize = 512;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const SOURCE_PROBE_MASKS: [u8; 1] = [0x00];
const PROBE_FRAMES: u16 = 45;
const SOURCE_PROBE_TRANSCRIPT: [(u8, u64, bool, bool); 1] = [(0x00, 45, false, true)];
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 1,
    progress: 165,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 1,
    progress: 165,
    player_y_bucket: 11,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 1,
    progress: 165,
    player_y_bucket: 11,
    player_engine_state: 8,
    state_fingerprint: 3,
    room_x_bucket: 0,
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 16,
    hold_frames: 113,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    pair: usize,
    slot: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
enum ArmKind {
    L1,
    L4,
}

#[derive(Debug, Serialize)]
struct Config {
    pairs: usize,
    arms: usize,
    slots_per_arm: usize,
    phrase_length: usize,
    workers: usize,
    action_limit: usize,
    archive_limit: usize,
    max_lineage_actions: usize,
    selector: &'static str,
    retention: &'static str,
    replacement: &'static str,
    key: &'static str,
    waypoint: &'static str,
    snapback: &'static str,
    pinned_window: &'static str,
    empirical_chord_update: &'static str,
    assignment: &'static str,
    probe_masks: [u8; 3],
    probe_frames: u16,
    source_probe_masks: [u8; 1],
    source_probe_frames: u64,
    max_action_frames: u64,
    max_probe_frames: u64,
    max_total_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BaselineRecord {
    record: &'static str,
    setup_frames: u64,
    replay_frames: u64,
    actions: usize,
    endpoint_observation: SmbObservations,
    endpoint: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    trace_sha256: String,
    wram_sha256: String,
    snapshot_sha256: String,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
    final_action: ButtonChord,
    source_probes: Vec<ProbeAttempt>,
}

#[derive(Clone)]
struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AdmissionOutcome {
    Terminal,
    ProbeRefused,
    Duplicate { id: usize },
    Rejected,
    Retained { id: usize, displaced: bool },
}

impl AdmissionOutcome {
    fn newly_retained_id(&self) -> Option<usize> {
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
struct StartEvidence {
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    wram_sha256: String,
    snapshot_sha256: String,
    dead: bool,
    failed: bool,
    milestones: SmbMilestones,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct EndpointEvidence {
    action: ButtonChord,
    input_actions: usize,
    input_sha256: String,
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: Option<String>,
    key: Option<SmbArchiveKey>,
    milestones: SmbMilestones,
    action_frames: u64,
    dead: bool,
    failed: bool,
    probe: Vec<ProbeAttempt>,
    probe_survived: bool,
    probe_frames: u64,
    admission: AdmissionOutcome,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ActiveMaximum {
    watermark: SmbProgressWatermark,
    ids: Vec<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PrefixRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    phrase: usize,
    prefix_depth: usize,
    source_index: usize,
    selector_seed: u64,
    selector_used: bool,
    current_parent_before: usize,
    current_parent_after: usize,
    start: StartEvidence,
    endpoint: EndpointEvidence,
    productive: bool,
    active_ids: Vec<usize>,
    active_maximum: ActiveMaximum,
    total_work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SkippedSlotRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    phrase: usize,
    prefix_depth: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
    selector_used: bool,
    reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct JobRecord {
    pair: usize,
    arm: ArmKind,
    job: usize,
    selection_slot: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    original_parent_id: usize,
    parent_input_sha256: String,
    parent_snapshot_sha256: String,
    start: StartEvidence,
    prefixes: Vec<PrefixRecord>,
    skipped: Vec<SkippedSlotRecord>,
    productive: bool,
    selector_accounting: SmbSelectorAccounting,
    total_work_frames: u64,
}

#[derive(Clone, Debug)]
struct RetainedEvidence {
    endpoint: EndpointEvidence,
    work_frames: u64,
    prefix_depth: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FinalEntryRecord {
    id: usize,
    parent_id: Option<u64>,
    created_execution: u64,
    actions: usize,
    input_sha256: String,
    key: SmbArchiveKey,
    watermark: SmbProgressWatermark,
    milestones: SmbMilestones,
    snapshot_sha256: String,
    probe_survived: bool,
    work_frames: u64,
    prefix_depth: usize,
}

#[derive(Clone, Debug, Serialize)]
struct ArmRecord {
    record: &'static str,
    ordinal: usize,
    pair: usize,
    arm: ArmKind,
    worker: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_setup_frames: Option<u64>,
    initial_archive_sha256: String,
    jobs: Vec<JobRecord>,
    final_active_entries: Vec<FinalEntryRecord>,
    final_maximum: ActiveMaximum,
    maximum_lineage_actions: usize,
    scheduled_slots: usize,
    executed_slots: usize,
    skipped_slots: usize,
    selections: usize,
    selector_accounting: SmbSelectorAccounting,
    action_frames: u64,
    probe_frames: u64,
    total_work_frames: u64,
    #[serde(skip)]
    champion_candidates: Vec<ChampionCandidate>,
}

#[derive(Clone, Debug)]
struct ChampionCandidate {
    pair: usize,
    arm: ArmKind,
    id: usize,
    prefix_depth: usize,
    input: SmbInput,
    input_sha256: String,
    input_sha256_bytes: [u8; 32],
    parent_lineage: Vec<u64>,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ChampionRecord {
    pair: usize,
    arm: ArmKind,
    id: usize,
    prefix_depth: usize,
    parent_lineage: Vec<u64>,
    input: SmbInput,
    input_sha256: String,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Verdict {
    Adopt,
    Stop,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ClassificationRecord {
    record: &'static str,
    verdict: Verdict,
    eligible_entries: usize,
    champion: Option<ChampionRecord>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum StructuralVerdict {
    InconclusiveSparse,
    ConfirmL4,
    RejectL4,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairOutcomeRecord {
    pair: usize,
    l1_maximum: SmbProgressWatermark,
    l4_maximum: SmbProgressWatermark,
    outcome: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StructuralWitness {
    pair: usize,
    l1_maximum: SmbProgressWatermark,
    champion: ChampionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairedClassificationRecord {
    record: &'static str,
    pairs: Vec<PairOutcomeRecord>,
    non_ties: usize,
    l4_wins: usize,
    tail_numerator: u128,
    tail_denominator: u128,
    witnesses: Vec<StructuralWitness>,
    verdict: StructuralVerdict,
}

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    structural_verdict: StructuralVerdict,
    adoption_verdict: Verdict,
    champion: Option<ChampionRecord>,
    worker_setup_frames: Vec<u64>,
    scheduled_slots: usize,
    executed_slots: usize,
    skipped_slots: usize,
    l1_selections: usize,
    l4_selections: usize,
    setup_frames: u64,
    source_replay_frames: u64,
    source_probe_frames: u64,
    action_frames: u64,
    probe_frames: u64,
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
    authorizing_paired_preregistration: &'static str,
    authorizing_paired_implementation: &'static str,
    authorizing_paired_result: &'static str,
    authorizing_paired_report_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    source_paired_pair: u64,
    source_paired_arm: ArmKind,
    source_paired_entry_id: u64,
    source_paired_prefix_depth: u64,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    recipe_sha256: &'a str,
    projection_sha256: &'a [String],
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

/// Run the sealed paired L1/L4 phrase canary from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-w8-2-p165-confirmatory-l1-l4-canary <input.json> <output.jsonl>")?,
    );
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    verify_seed()?;
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

    let config = Config {
        pairs: PAIRS,
        arms: ARMS,
        slots_per_arm: SLOTS,
        phrase_length: PHRASE_LENGTH,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        max_lineage_actions: MAX_LINEAGE_ACTIONS,
        selector: "concentrated_recency_fresh_seed_per_job_v1",
        retention: "probe_at_admission_45",
        replacement: "fewest_actions",
        key: "frozen",
        waypoint: "absent",
        snapback: "absent",
        pinned_window: "absent",
        empirical_chord_update: "absent",
        assignment: "ordinal_modulo_12_persistent_buffered_ascending_v1",
        probe_masks: PROBE_MASKS,
        probe_frames: PROBE_FRAMES,
        source_probe_masks: SOURCE_PROBE_MASKS,
        source_probe_frames: SOURCE_PROBE_FRAMES,
        max_action_frames: MAX_ACTION_FRAMES,
        max_probe_frames: MAX_PROBE_FRAMES,
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
    let executable = read_bounded(&env::current_exe()?, MAX_EXECUTABLE_BYTES, "executable")?;
    let executable_sha256 = sha256_bytes(&executable);

    let mut baseline_target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut baseline_target, &source)?;
    let recipes = derive_recipes(&source)?;
    let recipe_bytes = recipe_identity_bytes(&recipes)?;
    let recipe_sha256 = sha256_bytes(&recipe_bytes);
    if recipe_bytes.len() != EXPECTED_RECIPE_BYTES || recipe_sha256 != EXPECTED_RECIPE_SHA256 {
        return Err("frozen recipe identity does not match the sealed oracle".into());
    }
    let projection_sha256 = projection_sha256(&recipes)?;
    let arms = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let paired = classify_paired(&arms)?;
    let adoption = classify_adoption(&arms)?;
    let work = summarize_work(
        &arms,
        baseline.record.setup_frames,
        source_probe_frames(&baseline.record.source_probes)?,
    )?;

    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration_commit: PREREGISTRATION_COMMIT,
        preregistration_doc_sha256: PREREGISTRATION_DOC_SHA256,
        code_base: CODE_BASE,
        authorizing_paired_preregistration: AUTHORIZING_PAIRED_PREREGISTRATION,
        authorizing_paired_implementation: AUTHORIZING_PAIRED_IMPLEMENTATION,
        authorizing_paired_result: AUTHORIZING_PAIRED_RESULT,
        authorizing_paired_report_sha256: AUTHORIZING_PAIRED_REPORT_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        source_paired_pair: 2,
        source_paired_arm: ArmKind::L4,
        source_paired_entry_id: 121,
        source_paired_prefix_depth: 1,
        rom_sha256: &rom_sha256,
        executable_sha256: &executable_sha256,
        bin_source_sha256: &bin_source_sha256,
        module_source_sha256: &module_source_sha256,
        seed_label: SEED_LABEL,
        seed_label_sha256: SEED_LABEL_SHA256,
        recipe_sha256: &recipe_sha256,
        projection_sha256: &projection_sha256,
        config_sha256: &config_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    #[derive(Serialize)]
    struct RecipeRecord<'a> {
        record: &'static str,
        recipe_sha256: &'a str,
        projection_sha256: &'a [String],
        recipes: &'a [Vec<Recipe>],
    }
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_sha256: &recipe_sha256,
        projection_sha256: &projection_sha256,
        recipes: &recipes,
    })?;
    for arm in &arms {
        output.write(arm)?;
    }
    output.write(&paired)?;
    output.write(&adoption)?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        structural_verdict: paired.verdict,
        adoption_verdict: adoption.verdict,
        champion: adoption.champion.clone(),
        worker_setup_frames: work.worker_setup_frames.clone(),
        scheduled_slots: work.scheduled,
        executed_slots: work.executed,
        skipped_slots: work.skipped,
        l1_selections: work.l1_selections,
        l4_selections: work.l4_selections,
        setup_frames: work.setup,
        source_replay_frames: baseline.record.replay_frames,
        source_probe_frames: work.source_probe,
        action_frames: work.action,
        probe_frames: work.probe,
        experimental_frames: work.experimental,
        total_frames: work.total,
    };
    output.write(&summary)?;
    let report_sha256 = output.finish()?;
    println!(
        "{{\"report_sha256\":\"{report_sha256}\",\"structural_verdict\":{},\"adoption_verdict\":{}}}",
        serde_json::to_string(&summary.structural_verdict)?,
        serde_json::to_string(&summary.adoption_verdict)?
    );
    Ok(())
}

fn verify_seed() -> Result<(), Box<dyn Error>> {
    let digest = Sha256::digest(SEED_LABEL.as_bytes());
    if digest.as_slice() != hex_to_array(SEED_LABEL_SHA256)?.as_slice() {
        return Err("seed label hash does not match the preregistration".into());
    }
    let first = digest
        .get(..8)
        .ok_or("seed digest is shorter than eight bytes")?;
    let seed = u64::from_le_bytes(first.try_into()?);
    if seed != MASTER_SEED {
        return Err("master seed does not match the seed label".into());
    }
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
    if source.actions.last() != Some(&BASELINE_FINAL_ACTION) {
        return Err("source final action does not match the preregistration".into());
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
        let pair_u64 = u64::try_from(pair)?;
        let pair_seed = digest_word(&[
            &MASTER_SEED.to_le_bytes(),
            b"confirm-l1-l4-v2-pair",
            &pair_u64.to_le_bytes(),
        ])?;
        let mut slots = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let slot_u64 = u64::try_from(slot)?;
            let source_word = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"confirm-l1-l4-v2-action",
                &slot_u64.to_le_bytes(),
            ])?;
            let source_index = usize::try_from(source_word % source_len)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived source index is out of bounds")?;
            let selector_seed = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"confirm-l1-l4-v2-parent",
                &slot_u64.to_le_bytes(),
            ])?;
            slots.push(Recipe {
                pair,
                slot,
                source_index,
                action,
                selector_seed,
            });
        }
        pairs.push(slots);
    }
    Ok(pairs)
}

#[cfg(test)]
fn recipe_sha256(recipes: &[Vec<Recipe>]) -> Result<String, Box<dyn Error>> {
    Ok(sha256_bytes(&recipe_identity_bytes(recipes)?))
}

fn recipe_identity_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<u8>, Box<dyn Error>> {
    let identity = recipes
        .iter()
        .flat_map(|pair| pair.iter())
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.pair)?,
                u64::try_from(recipe.slot)?,
                u64::try_from(recipe.source_index)?,
                recipe.action,
                recipe.selector_seed,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(serde_json::to_vec(&identity)?)
}

fn projection_sha256(recipes: &[Vec<Recipe>]) -> Result<Vec<String>, Box<dyn Error>> {
    let identities = projection_bytes(recipes)?;
    if identities
        .iter()
        .map(Vec::len)
        .ne(EXPECTED_PROJECTION_BYTES)
    {
        return Err("pair recipe projection byte lengths do not match the sealed oracle".into());
    }
    let hashes = identities
        .iter()
        .map(|bytes| sha256_bytes(bytes))
        .collect::<Vec<_>>();
    let mut sorted = identities;
    sorted.sort();
    if sorted.windows(2).any(|window| window[0] == window[1]) {
        return Err("pair recipe projections are not pairwise distinct".into());
    }
    if hashes
        .iter()
        .map(String::as_str)
        .ne(EXPECTED_PROJECTION_SHA256)
    {
        return Err("pair recipe projection hashes do not match the sealed oracle".into());
    }
    Ok(hashes)
}

fn projection_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    if recipes.len() != PAIRS {
        return Err("recipe pair count does not match the preregistration".into());
    }
    let mut identities = Vec::with_capacity(PAIRS);
    for (pair, recipes) in recipes.iter().enumerate() {
        if recipes.len() != SLOTS {
            return Err("recipe slot count does not match the preregistration".into());
        }
        let identity = recipes
            .iter()
            .map(|recipe| {
                if recipe.pair != pair {
                    return Err("recipe pair identity is not canonical".into());
                }
                Ok((
                    u64::try_from(recipe.slot)?,
                    u64::try_from(recipe.source_index)?,
                    recipe.action,
                    recipe.selector_seed,
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let bytes = serde_json::to_vec(&identity)?;
        identities.push(bytes);
    }
    Ok(identities)
}

fn digest_word(parts: &[&[u8]]) -> Result<u64, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    let digest = hasher.finalize();
    Ok(u64::from_le_bytes(
        digest
            .get(..8)
            .ok_or("digest is shorter than eight bytes")?
            .try_into()?,
    ))
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
    let replay_before = target.frames_clocked();
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
        .checked_sub(replay_before)
        .ok_or("baseline work counter moved backwards")?;
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let endpoint_observation = target.observe();
    let snapshot = target
        .snapshot()
        .ok_or("failed to snapshot source endpoint")?;
    let wram_sha256 = sha256_bytes(target.wram());
    let snapshot_sha256 = sha256_json(&snapshot)?;
    let key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
    if replay_frames != SOURCE_FRAMES
        || endpoint_observation.frame_count != SOURCE_FRAMES
        || endpoint != BASELINE_ENDPOINT
        || watermark != BASELINE_WATERMARK
        || wram_sha256 != SOURCE_WRAM_SHA256
        || snapshot_sha256 != SOURCE_SNAPSHOT_SHA256
        || key != BASELINE_KEY
        || milestones != BASELINE_MILESTONES
        || source.actions.last() != Some(&BASELINE_FINAL_ACTION)
    {
        return Err("source replay evidence does not match the preregistration".into());
    }
    let source_probes = run_source_probes(
        target,
        &snapshot,
        wram_sha256.as_str(),
        snapshot_sha256.as_str(),
    )?;
    let source_probe_work = source_probe_frames(&source_probes)?;
    let baseline_delta = target
        .frames_clocked()
        .checked_sub(replay_before)
        .ok_or("baseline total work counter moved backwards")?;
    if baseline_delta
        != replay_frames
            .checked_add(source_probe_work)
            .ok_or("baseline component work overflow")?
    {
        return Err("baseline work does not reconcile with replay and source probe".into());
    }
    let record = BaselineRecord {
        record: "baseline",
        setup_frames,
        replay_frames,
        actions: source.actions.len(),
        endpoint_observation,
        endpoint,
        watermark,
        trace_sha256: finish_sha256(trace),
        wram_sha256,
        snapshot_sha256,
        key,
        milestones,
        final_action: BASELINE_FINAL_ACTION,
        source_probes,
    };
    Ok(Baseline { record, snapshot })
}

fn run_source_probes(
    target: &mut SmbTarget,
    snapshot: &SmbSnapshot,
    expected_wram_sha256: &str,
    expected_snapshot_sha256: &str,
) -> Result<Vec<ProbeAttempt>, Box<dyn Error>> {
    let mut attempts = Vec::with_capacity(SOURCE_PROBE_TRANSCRIPT.len());
    for (mask, expected_work, expected_dead, expected_survived) in SOURCE_PROBE_TRANSCRIPT {
        target.restore(snapshot)?;
        verify_snapshot(target, snapshot)?;
        let before = target.frames_clocked();
        let survived = target.survives_probe(mask, PROBE_FRAMES);
        let work_frames = target
            .frames_clocked()
            .checked_sub(before)
            .ok_or("source-probe work counter moved backwards")?;
        let dead = target.is_dead();
        if target.exit_kind() != ExitKind::Ok
            || work_frames != expected_work
            || dead != expected_dead
            || survived != expected_survived
        {
            return Err(
                "source evidence probe transcript does not match the preregistration".into(),
            );
        }
        attempts.push(ProbeAttempt {
            mask,
            work_frames,
            dead,
            survived,
        });
    }
    target.restore(snapshot)?;
    verify_snapshot(target, snapshot)?;
    let restored_snapshot = target
        .snapshot()
        .ok_or("failed to snapshot restored source after probe")?;
    if sha256_bytes(target.wram()) != expected_wram_sha256
        || sha256_json(&restored_snapshot)? != expected_snapshot_sha256
    {
        return Err("source evidence probe did not restore exact source state".into());
    }
    Ok(attempts)
}

fn source_probe_frames(attempts: &[ProbeAttempt]) -> Result<u64, Box<dyn Error>> {
    if attempts.len() != SOURCE_PROBE_TRANSCRIPT.len() {
        return Err("source evidence probe count does not match the preregistration".into());
    }
    let mut total = 0_u64;
    for (attempt, (mask, work_frames, dead, survived)) in
        attempts.iter().zip(SOURCE_PROBE_TRANSCRIPT)
    {
        if (
            attempt.mask,
            attempt.work_frames,
            attempt.dead,
            attempt.survived,
        ) != (mask, work_frames, dead, survived)
        {
            return Err("source evidence probe record is not canonical".into());
        }
        total = total
            .checked_add(attempt.work_frames)
            .ok_or("source evidence probe work overflow")?;
    }
    if total != SOURCE_PROBE_FRAMES {
        return Err("source evidence probe work does not match the preregistration".into());
    }
    Ok(total)
}

fn evaluate_parallel(
    rom: &[u8],
    source: &SmbInput,
    recipes: &[Vec<Recipe>],
    baseline: &Baseline,
) -> Result<Vec<ArmRecord>, Box<dyn Error>> {
    if recipes.len() != PAIRS || recipes.iter().any(|pair| pair.len() != SLOTS) {
        return Err("recipe shape does not match the preregistration".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let sender = sender.clone();
            let source = source.clone();
            let recipes = recipes.to_vec();
            let baseline = baseline.clone();
            let handle = thread::Builder::new()
                .name(format!("paired-phrase-{worker}"))
                .spawn_scoped(scope, move || {
                    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)
                        .map_err(|error| error.to_string());
                    let mut prior_error = target
                        .as_ref()
                        .ok()
                        .and_then(|target| {
                            (target.frames_clocked() != EXPECTED_SETUP_FRAMES).then(|| {
                                format!(
                                    "worker {worker} setup frames: expected {EXPECTED_SETUP_FRAMES}, got {}",
                                    target.frames_clocked()
                                )
                            })
                        });
                    for ordinal in (worker..ARMS).step_by(WORKERS) {
                        let result = if let Some(error) = prior_error.as_ref() {
                            Err(format!("worker unavailable after prior error: {error}"))
                        } else {
                            match target.as_mut() {
                                Ok(target) => {
                                    let pair = ordinal / 2;
                                    let pair_recipes = recipes
                                        .get(pair)
                                        .ok_or_else(|| "missing pair recipes".to_string());
                                    pair_recipes.and_then(|pair_recipes| {
                                        run_arm(
                                            target,
                                            &source,
                                            pair_recipes,
                                            &baseline,
                                            ordinal,
                                            worker,
                                        )
                                        .map_err(|error| error.to_string())
                                    })
                                }
                                Err(error) => Err(error.clone()),
                            }
                        };
                        if let Err(error) = &result {
                            prior_error = Some(error.clone());
                        }
                        let _ = sender.send(ArmReply {
                            ordinal,
                            worker,
                            result,
                        });
                    }
                })?;
            handles.push(handle);
        }
        drop(sender);
        for handle in handles {
            handle.join().map_err(|_| "paired-phrase worker panicked")?;
        }
        Ok(())
    })?;
    consume_arm_replies(receiver.into_iter().collect())
}

fn consume_arm_replies(replies: Vec<ArmReply>) -> Result<Vec<ArmRecord>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    let mut metadata_errors = Vec::new();
    for reply in replies {
        if reply.ordinal >= ARMS || reply.worker != reply.ordinal % WORKERS {
            metadata_errors.push((0_u8, reply.ordinal, reply.worker, "invalid"));
            continue;
        }
        if buffered.insert(reply.ordinal, reply.result).is_some() {
            metadata_errors.push((1_u8, reply.ordinal, reply.worker, "duplicate"));
        }
    }
    for ordinal in 0..ARMS {
        if !buffered.contains_key(&ordinal) {
            metadata_errors.push((2_u8, ordinal, ordinal % WORKERS, "missing"));
        }
    }
    metadata_errors.sort_unstable();
    if let Some((_, ordinal, worker, kind)) = metadata_errors.first() {
        return Err(format!("{kind} arm reply: ordinal={ordinal}, worker={worker}").into());
    }
    let mut arms = Vec::with_capacity(ARMS);
    for ordinal in 0..ARMS {
        arms.push(
            buffered
                .remove(&ordinal)
                .ok_or("missing arm reply")?
                .map_err(|error| format!("arm {ordinal}: {error}"))?,
        );
    }
    Ok(arms)
}

fn run_arm(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipes: &[Recipe],
    baseline: &Baseline,
    ordinal: usize,
    worker: usize,
) -> Result<ArmRecord, Box<dyn Error>> {
    if ordinal >= ARMS || worker != ordinal % WORKERS || recipes.len() != SLOTS {
        return Err("arm identity or recipe count does not match the preregistration".into());
    }
    let pair = ordinal / 2;
    let arm = if ordinal.is_multiple_of(2) {
        ArmKind::L1
    } else {
        ArmKind::L4
    };
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
    let arm_work_before = target.frames_clocked();
    let jobs_expected = if arm == ArmKind::L1 {
        SLOTS
    } else {
        SLOTS / PHRASE_LENGTH
    };
    let mut jobs = Vec::with_capacity(jobs_expected);
    let mut retained: Vec<Option<RetainedEvidence>> = vec![None];
    let mut action_total = 0_u64;
    let mut probe_total = 0_u64;
    let mut maximum_lineage_actions = SOURCE_ACTIONS;
    let mut executed_slots = 0_usize;
    let mut skipped_slots = 0_usize;

    for job in 0..jobs_expected {
        let selection_slot = if arm == ArmKind::L1 {
            job
        } else {
            job.checked_mul(PHRASE_LENGTH)
                .ok_or("phrase slot overflow")?
        };
        let selection_recipe = recipes
            .get(selection_slot)
            .ok_or("missing selection recipe")?;
        if selection_recipe.pair != pair || selection_recipe.slot != selection_slot {
            return Err("selection recipe order is not canonical".into());
        }
        let mut rand = StdRand::with_seed(selection_recipe.selector_seed);
        let (parent_id, selector) = archive.select_parent(&mut rand, ACTION_LIMIT)?;
        let selector = selector.ok_or("normal selector omitted its draw record")?;
        let parent = archive
            .entries
            .get(parent_id)
            .ok_or("selector returned a missing parent")?;
        let parent_report = parent.report.clone();
        let parent_snapshot = parent.snapshot.clone();
        let parent_input_sha256 = sha256_json(&parent_report.input)?;
        let parent_snapshot_sha256 = sha256_json(&parent_snapshot)?;

        target.restore(&parent_snapshot)?;
        verify_snapshot(target, &parent_snapshot)?;
        let job_start = StartEvidence {
            observation: target.observe(),
            mechanical: smb_mechanical_state_from_wram(target.wram()),
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256: parent_snapshot_sha256.clone(),
            dead: target.is_dead(),
            failed: target.exit_kind() != ExitKind::Ok,
            milestones: parent_report.milestones,
        };
        if job_start.dead || job_start.failed {
            return Err("selector returned a terminal or failed parent".into());
        }
        let job_before = target.frames_clocked();
        let mut current_parent = parent_id;
        let mut cumulative_input = parent_report.input.clone();
        let mut milestones = parent_report.milestones;
        let mut prefixes = Vec::with_capacity(PHRASE_LENGTH);
        let mut skipped = Vec::new();
        let mut phrase_productive = false;
        let actions_in_job = if arm == ArmKind::L1 { 1 } else { PHRASE_LENGTH };
        for offset in 0..actions_in_job {
            let slot = selection_slot.checked_add(offset).ok_or("slot overflow")?;
            let recipe = recipes.get(slot).ok_or("missing action recipe")?;
            if recipe.pair != pair || recipe.slot != slot {
                return Err("action recipe order is not canonical".into());
            }
            let prefix_depth = offset.checked_add(1).ok_or("prefix depth overflow")?;
            let current_parent_before = current_parent;
            let start = StartEvidence {
                observation: target.observe(),
                mechanical: smb_mechanical_state_from_wram(target.wram()),
                wram_sha256: sha256_bytes(target.wram()),
                snapshot_sha256: sha256_json(
                    &target.snapshot().ok_or("failed to snapshot prefix start")?,
                )?,
                dead: target.is_dead(),
                failed: target.exit_kind() != ExitKind::Ok,
                milestones,
            };
            if start.dead || start.failed {
                return Err("live phrase prefix started terminal or failed".into());
            }
            let prefix_before = target.frames_clocked();
            let action_before = target.frames_clocked();
            target.apply(&recipe.action);
            let action_frames = target
                .frames_clocked()
                .checked_sub(action_before)
                .ok_or("action work counter moved backwards")?;
            if target.exit_kind() != ExitKind::Ok {
                return Err("emulator failed during a full action".into());
            }
            let dead = target.is_dead();
            if action_frames > u64::from(recipe.action.bounded_hold_frames())
                || (!dead && action_frames != u64::from(recipe.action.bounded_hold_frames()))
            {
                return Err("full action work does not match its bounded duration".into());
            }
            let observation = target.observe();
            let mechanical = smb_mechanical_state_from_wram(target.wram());
            merge_action_milestones(&mut milestones, target)?;
            cumulative_input = appended_input(&cumulative_input, recipe.action)?;
            record_lineage_actions(&mut maximum_lineage_actions, cumulative_input.actions.len())?;
            let input_sha256 = sha256_json(&cumulative_input)?;
            let wram_sha256 = sha256_bytes(target.wram());
            let mut snapshot_sha256 = None;
            let mut key = None;
            let mut probe = Vec::new();
            let mut probe_survived = false;
            let mut probe_frames = 0_u64;
            let admission;
            if dead {
                admission = AdmissionOutcome::Terminal;
            } else {
                let snapshot = target
                    .snapshot()
                    .ok_or("failed to snapshot ordinary prefix endpoint")?;
                let candidate_snapshot_sha256 = sha256_json(&snapshot)?;
                let candidate_key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
                let (attempts, survived, work) = run_probe(target, &snapshot)?;
                probe = attempts;
                probe_survived = survived;
                probe_frames = work;
                snapshot_sha256 = Some(candidate_snapshot_sha256);
                key = Some(candidate_key);
                admission = if survived {
                    insert_candidate(
                        &mut archive,
                        Some(current_parent),
                        u64::try_from(job.checked_add(1).ok_or("execution overflow")?)?,
                        ArchiveCandidate {
                            input: cumulative_input.clone(),
                            key: candidate_key,
                            milestones,
                        },
                        snapshot,
                    )?
                } else {
                    AdmissionOutcome::ProbeRefused
                };
            }
            let endpoint = EndpointEvidence {
                action: recipe.action,
                input_actions: cumulative_input.actions.len(),
                input_sha256,
                observation,
                mechanical,
                watermark: watermark(mechanical),
                wram_sha256,
                snapshot_sha256,
                key,
                milestones,
                action_frames,
                dead,
                failed: false,
                probe,
                probe_survived,
                probe_frames,
                admission,
            };
            let prefix_productive = endpoint.admission.newly_retained_id().is_some();
            if let Some(id) = endpoint.admission.newly_retained_id() {
                if id != retained.len() {
                    return Err("retained evidence is not insertion-order aligned".into());
                }
                retained.push(Some(RetainedEvidence {
                    endpoint: endpoint.clone(),
                    work_frames: action_frames
                        .checked_add(probe_frames)
                        .ok_or("retained work overflow")?,
                    prefix_depth,
                }));
                phrase_productive = true;
            } else {
                if archive.entries.len() != retained.len() {
                    return Err("nonallocating admission changed archive length".into());
                }
            }
            current_parent = next_current_parent(current_parent, &endpoint.admission);
            let prefix_work = target
                .frames_clocked()
                .checked_sub(prefix_before)
                .ok_or("prefix work counter moved backwards")?;
            if prefix_work
                != action_frames
                    .checked_add(probe_frames)
                    .ok_or("prefix component work overflow")?
            {
                return Err("prefix work does not reconcile with components".into());
            }
            action_total = action_total
                .checked_add(action_frames)
                .ok_or("arm action work overflow")?;
            probe_total = probe_total
                .checked_add(probe_frames)
                .ok_or("arm probe work overflow")?;
            executed_slots = executed_slots
                .checked_add(1)
                .ok_or("executed slot overflow")?;
            prefixes.push(PrefixRecord {
                pair,
                arm,
                slot,
                phrase: job,
                prefix_depth,
                source_index: recipe.source_index,
                selector_seed: recipe.selector_seed,
                selector_used: offset == 0,
                current_parent_before,
                current_parent_after: current_parent,
                start,
                endpoint,
                productive: prefix_productive,
                active_ids: active_ids(&archive)?,
                active_maximum: active_maximum(&archive)?,
                total_work_frames: prefix_work,
            });
            if dead {
                for skipped_offset in offset + 1..actions_in_job {
                    let skipped_slot = selection_slot
                        .checked_add(skipped_offset)
                        .ok_or("skipped slot overflow")?;
                    let skipped_recipe =
                        recipes.get(skipped_slot).ok_or("missing skipped recipe")?;
                    skipped.push(SkippedSlotRecord {
                        pair,
                        arm,
                        slot: skipped_slot,
                        phrase: job,
                        prefix_depth: skipped_offset + 1,
                        source_index: skipped_recipe.source_index,
                        action: skipped_recipe.action,
                        selector_seed: skipped_recipe.selector_seed,
                        selector_used: false,
                        reason: "unexecuted_after_death",
                    });
                    skipped_slots = skipped_slots
                        .checked_add(1)
                        .ok_or("skipped slot overflow")?;
                }
                break;
            }
        }
        let job_work = target
            .frames_clocked()
            .checked_sub(job_before)
            .ok_or("job work counter moved backwards")?;
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, phrase_productive, job_work)?;
        jobs.push(JobRecord {
            pair,
            arm,
            job,
            selection_slot,
            selector_seed: selection_recipe.selector_seed,
            selector,
            original_parent_id: parent_id,
            parent_input_sha256,
            parent_snapshot_sha256,
            start: job_start,
            prefixes,
            skipped,
            productive: phrase_productive,
            selector_accounting: archive.selector_report(),
            total_work_frames: job_work,
        });
    }
    let total_work_frames = action_total
        .checked_add(probe_total)
        .ok_or("arm work overflow")?;
    let arm_delta = target
        .frames_clocked()
        .checked_sub(arm_work_before)
        .ok_or("arm work counter moved backwards")?;
    if arm_delta != total_work_frames
        || executed_slots
            .checked_add(skipped_slots)
            .ok_or("slot total overflow")?
            != SLOTS
    {
        return Err("arm work or slot counts do not reconcile".into());
    }
    let (final_active_entries, champion_candidates) =
        final_entries(pair, arm, &archive, &retained)?;
    Ok(ArmRecord {
        record: "arm",
        ordinal,
        pair,
        arm,
        worker,
        worker_setup_frames: (ordinal == worker).then_some(EXPECTED_SETUP_FRAMES),
        initial_archive_sha256,
        jobs,
        final_active_entries,
        final_maximum: active_maximum(&archive)?,
        maximum_lineage_actions,
        scheduled_slots: SLOTS,
        executed_slots,
        skipped_slots,
        selections: jobs_expected,
        selector_accounting: archive.selector_report(),
        action_frames: action_total,
        probe_frames: probe_total,
        total_work_frames,
        champion_candidates,
    })
}

fn next_current_parent(current: usize, admission: &AdmissionOutcome) -> usize {
    match admission {
        AdmissionOutcome::Duplicate { id } | AdmissionOutcome::Retained { id, .. } => *id,
        AdmissionOutcome::Terminal
        | AdmissionOutcome::ProbeRefused
        | AdmissionOutcome::Rejected => current,
    }
}

fn record_lineage_actions(
    maximum_lineage_actions: &mut usize,
    candidate_actions: usize,
) -> Result<(), Box<dyn Error>> {
    if candidate_actions > MAX_LINEAGE_ACTIONS {
        return Err("candidate lineage exceeds the registered maximum".into());
    }
    *maximum_lineage_actions = (*maximum_lineage_actions).max(candidate_actions);
    Ok(())
}

fn appended_input(parent: &SmbInput, action: ButtonChord) -> Result<SmbInput, Box<dyn Error>> {
    let capacity = parent
        .actions
        .len()
        .checked_add(1)
        .ok_or("candidate input length overflow")?;
    if capacity > ACTION_LIMIT {
        return Err("candidate input exceeds the registered action limit".into());
    }
    let mut actions = Vec::with_capacity(capacity);
    actions.extend_from_slice(&parent.actions);
    actions.push(action);
    Ok(SmbInput { actions })
}

fn insert_candidate(
    archive: &mut Archive,
    parent_id: Option<usize>,
    execution: u64,
    candidate: ArchiveCandidate,
    snapshot: SmbSnapshot,
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
            if archive.entries.len() != before_len.checked_add(1).ok_or("archive overflow")? {
                return Err("retained insertion did not append exactly one entry".into());
            }
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
    if total != summed || total > u64::from(PROBE_FRAMES) * u64::try_from(PROBE_MASKS.len())? {
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
    let observation = target.observe();
    if &actual != expected
        || target.wram().as_slice() != observation.wram.as_slice()
        || smb_mechanical_state_from_wram(target.wram()) != observation.decoded
    {
        return Err("restored snapshot is not byte-exact".into());
    }
    Ok(())
}

fn active_ids(archive: &Archive) -> Result<Vec<usize>, Box<dyn Error>> {
    if archive.active.len() != archive.entries.len() {
        return Err("archive active bits are misaligned".into());
    }
    Ok(archive
        .active
        .iter()
        .enumerate()
        .filter_map(|(id, active)| active.then_some(id))
        .collect())
}

fn active_maximum(archive: &Archive) -> Result<ActiveMaximum, Box<dyn Error>> {
    let ids = active_ids(archive)?;
    let watermark = ids
        .iter()
        .map(|id| {
            archive
                .entries
                .get(*id)
                .map(|entry| watermark_from_key(entry.report.key))
                .ok_or("active id is missing its archive entry")
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max()
        .ok_or("archive has no active entry")?;
    let ids = ids
        .into_iter()
        .filter(|id| watermark_from_key(archive.entries[*id].report.key) == watermark)
        .collect();
    Ok(ActiveMaximum { watermark, ids })
}

fn final_entries(
    pair: usize,
    arm: ArmKind,
    archive: &Archive,
    retained: &[Option<RetainedEvidence>],
) -> Result<(Vec<FinalEntryRecord>, Vec<ChampionCandidate>), Box<dyn Error>> {
    if archive.entries.len() != archive.active.len() || archive.entries.len() != retained.len() {
        return Err("final archive evidence is misaligned".into());
    }
    let mut records = Vec::new();
    let mut candidates = Vec::new();
    for id in 1..archive.entries.len() {
        if !archive.active[id] {
            continue;
        }
        let entry = archive.entries.get(id).ok_or("final entry is missing")?;
        let evidence = retained
            .get(id)
            .and_then(Option::as_ref)
            .ok_or("active allocated entry lacks retention evidence")?;
        if evidence.endpoint.admission.newly_retained_id() != Some(id)
            || evidence.endpoint.dead
            || evidence.endpoint.failed
            || !evidence.endpoint.probe_survived
            || evidence.endpoint.key != Some(entry.report.key)
        {
            return Err("active entry disagrees with its normal admission evidence".into());
        }
        let input_sha256 = sha256_json(&entry.report.input)?;
        if input_sha256 != evidence.endpoint.input_sha256 {
            return Err("active entry input identity changed after admission".into());
        }
        let snapshot_sha256 = sha256_json(&entry.snapshot)?;
        if evidence.endpoint.snapshot_sha256.as_deref() != Some(snapshot_sha256.as_str()) {
            return Err("active entry snapshot identity changed after admission".into());
        }
        let parent_lineage = parent_lineage(archive, id)?;
        records.push(FinalEntryRecord {
            id,
            parent_id: entry.report.parent_id,
            created_execution: entry.report.created_execution,
            actions: entry.report.input.actions.len(),
            input_sha256: input_sha256.clone(),
            key: entry.report.key,
            watermark: watermark_from_key(entry.report.key),
            milestones: entry.report.milestones,
            snapshot_sha256,
            probe_survived: evidence.endpoint.probe_survived,
            work_frames: evidence.work_frames,
            prefix_depth: evidence.prefix_depth,
        });
        candidates.push(ChampionCandidate {
            pair,
            arm,
            id,
            prefix_depth: evidence.prefix_depth,
            input: entry.report.input.clone(),
            input_sha256_bytes: hex_to_array(&input_sha256)?,
            input_sha256,
            parent_lineage,
            endpoint: evidence.endpoint.clone(),
            work_frames: evidence.work_frames,
        });
    }
    Ok((records, candidates))
}

fn parent_lineage(archive: &Archive, id: usize) -> Result<Vec<u64>, Box<dyn Error>> {
    if id >= archive.entries.len() {
        return Err("lineage starts outside the archive".into());
    }
    let mut lineage = Vec::new();
    let mut current = Some(id);
    while let Some(entry_id) = current {
        if lineage.len() >= archive.entries.len() {
            return Err("archive lineage contains a cycle".into());
        }
        let entry = archive
            .entries
            .get(entry_id)
            .ok_or("archive lineage references a missing entry")?;
        lineage.push(entry.report.id);
        current = entry.report.parent_id.map(usize::try_from).transpose()?;
    }
    lineage.reverse();
    if lineage.first() != Some(&0) || lineage.last() != Some(&u64::try_from(id)?) {
        return Err("archive lineage does not connect source to candidate".into());
    }
    Ok(lineage)
}

fn validate_arms(arms: &[ArmRecord]) -> Result<(), Box<dyn Error>> {
    if arms.len() != ARMS {
        return Err("arm count does not match the preregistration".into());
    }
    for (ordinal, record) in arms.iter().enumerate() {
        let expected_arm = if ordinal.is_multiple_of(2) {
            ArmKind::L1
        } else {
            ArmKind::L4
        };
        let expected_jobs = if expected_arm == ArmKind::L1 {
            SLOTS
        } else {
            SLOTS / PHRASE_LENGTH
        };
        let accounted_selections = selector_selections(record.selector_accounting)?;
        if record.ordinal != ordinal
            || record.pair != ordinal / 2
            || record.arm != expected_arm
            || record.worker != ordinal % WORKERS
            || record.worker_setup_frames != (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES)
            || record.jobs.len() != expected_jobs
            || record.selections != expected_jobs
            || accounted_selections != u64::try_from(expected_jobs)?
            || record.selector_accounting.policy != SmbArchiveSelectorPolicy::ConcentratedRecency
            || record.selector_accounting.waypoint_selections != 0
            || record.scheduled_slots != SLOTS
            || record
                .executed_slots
                .checked_add(record.skipped_slots)
                .ok_or("arm slot count overflow")?
                != SLOTS
            || !(SOURCE_ACTIONS..=MAX_LINEAGE_ACTIONS).contains(&record.maximum_lineage_actions)
        {
            return Err("arm record order or shape is not canonical".into());
        }
        for (job, job_record) in record.jobs.iter().enumerate() {
            let expected_slot = if expected_arm == ArmKind::L1 {
                job
            } else {
                job.checked_mul(PHRASE_LENGTH)
                    .ok_or("validated phrase slot overflow")?
            };
            let expected_slots = if expected_arm == ArmKind::L1 {
                1
            } else {
                PHRASE_LENGTH
            };
            let accounted_slots = job_record
                .prefixes
                .len()
                .checked_add(job_record.skipped.len())
                .ok_or("job slot count overflow")?;
            let prefix_work = job_record.prefixes.iter().try_fold(0_u64, |sum, prefix| {
                sum.checked_add(prefix.total_work_frames)
                    .ok_or("job prefix work overflow")
            })?;
            if job_record.pair != record.pair
                || job_record.arm != record.arm
                || job_record.job != job
                || job_record.selection_slot != expected_slot
                || accounted_slots != expected_slots
                || prefix_work != job_record.total_work_frames
                || selector_selections(job_record.selector_accounting)?
                    != u64::try_from(job.checked_add(1).ok_or("job count overflow")?)?
                || job_record.prefixes.iter().any(|prefix| {
                    prefix.pair != record.pair
                        || prefix.arm != record.arm
                        || prefix.phrase != job
                        || prefix.selector_used != (prefix.prefix_depth == 1)
                })
                || job_record.skipped.iter().any(|skipped| {
                    skipped.pair != record.pair
                        || skipped.arm != record.arm
                        || skipped.phrase != job
                        || skipped.selector_used
                })
            {
                return Err("job record order or accounting is not canonical".into());
            }
        }
    }
    Ok(())
}

fn selector_selections(accounting: SmbSelectorAccounting) -> Result<u64, Box<dyn Error>> {
    accounting
        .uniform_selections
        .checked_add(accounting.tie_class_selections)
        .ok_or_else(|| "selector selection count overflow".into())
}

fn classify_adoption(arms: &[ArmRecord]) -> Result<ClassificationRecord, Box<dyn Error>> {
    validate_arms(arms)?;
    let candidates = arms
        .iter()
        .flat_map(|arm| arm.champion_candidates.iter().cloned())
        .collect::<Vec<_>>();
    let eligible_entries = candidates.len();
    let champion = rank_champion(candidates);
    let verdict = verdict_for(champion.as_ref());
    Ok(ClassificationRecord {
        record: "adoption_classification",
        verdict,
        eligible_entries,
        champion,
    })
}

fn rank_champion(mut candidates: Vec<ChampionCandidate>) -> Option<ChampionRecord> {
    candidates.sort_by(|left, right| {
        right
            .endpoint
            .watermark
            .cmp(&left.endpoint.watermark)
            .then_with(|| left.input.actions.len().cmp(&right.input.actions.len()))
            .then_with(|| left.input_sha256_bytes.cmp(&right.input_sha256_bytes))
            .then_with(|| left.pair.cmp(&right.pair))
            .then_with(|| left.arm.cmp(&right.arm))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.first().map(champion_record)
}

fn champion_record(candidate: &ChampionCandidate) -> ChampionRecord {
    ChampionRecord {
        pair: candidate.pair,
        arm: candidate.arm,
        id: candidate.id,
        prefix_depth: candidate.prefix_depth,
        parent_lineage: candidate.parent_lineage.clone(),
        input: candidate.input.clone(),
        input_sha256: candidate.input_sha256.clone(),
        endpoint: candidate.endpoint.clone(),
        work_frames: candidate.work_frames,
    }
}

fn verdict_for(champion: Option<&ChampionRecord>) -> Verdict {
    if champion.is_some_and(|candidate| {
        !candidate.endpoint.dead
            && !candidate.endpoint.failed
            && candidate.endpoint.probe_survived
            && candidate.endpoint.watermark > BASELINE_WATERMARK
    }) {
        Verdict::Adopt
    } else {
        Verdict::Stop
    }
}

fn classify_paired(arms: &[ArmRecord]) -> Result<PairedClassificationRecord, Box<dyn Error>> {
    validate_arms(arms)?;
    let mut pairs = Vec::with_capacity(PAIRS);
    let mut non_ties = 0_usize;
    let mut l4_wins = 0_usize;
    let mut witnesses = Vec::new();
    for pair in 0..PAIRS {
        let l1 = arms.get(pair * 2).ok_or("missing L1 arm")?;
        let l4 = arms
            .get(
                pair.checked_mul(2)
                    .and_then(|value| value.checked_add(1))
                    .ok_or("arm index overflow")?,
            )
            .ok_or("missing L4 arm")?;
        let outcome = match l4.final_maximum.watermark.cmp(&l1.final_maximum.watermark) {
            std::cmp::Ordering::Greater => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                l4_wins = l4_wins.checked_add(1).ok_or("L4 win count overflow")?;
                "L4_WIN"
            }
            std::cmp::Ordering::Less => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                "L1_WIN"
            }
            std::cmp::Ordering::Equal => "TIE",
        };
        pairs.push(PairOutcomeRecord {
            pair,
            l1_maximum: l1.final_maximum.watermark,
            l4_maximum: l4.final_maximum.watermark,
            outcome,
        });
        witnesses.extend(structural_witnesses(pair, l1, l4));
    }
    witnesses.sort_by_key(|witness| (witness.pair, witness.champion.id));
    let tail_numerator = sign_tail_numerator(non_ties, l4_wins)?;
    let shift = u32::try_from(non_ties)?;
    let tail_denominator = 1_u128
        .checked_shl(shift)
        .ok_or("sign denominator overflow")?;
    let verdict = structural_verdict(
        non_ties,
        tail_numerator,
        tail_denominator,
        !witnesses.is_empty(),
    )?;
    Ok(PairedClassificationRecord {
        record: "paired_classification",
        pairs,
        non_ties,
        l4_wins,
        tail_numerator,
        tail_denominator,
        witnesses,
        verdict,
    })
}

fn structural_witnesses(pair: usize, l1: &ArmRecord, l4: &ArmRecord) -> Vec<StructuralWitness> {
    l4.champion_candidates
        .iter()
        .filter(|candidate| {
            (2..=PHRASE_LENGTH).contains(&candidate.prefix_depth)
                && candidate.endpoint.watermark > BASELINE_WATERMARK
                && candidate.endpoint.watermark > l1.final_maximum.watermark
        })
        .map(|candidate| StructuralWitness {
            pair,
            l1_maximum: l1.final_maximum.watermark,
            champion: champion_record(candidate),
        })
        .collect()
}

fn structural_verdict(
    non_ties: usize,
    tail_numerator: u128,
    tail_denominator: u128,
    has_witness: bool,
) -> Result<StructuralVerdict, Box<dyn Error>> {
    let sign = tail_numerator
        .checked_mul(80)
        .ok_or("sign-tail comparison overflow")?
        <= tail_denominator;
    Ok(if non_ties < 8 {
        StructuralVerdict::InconclusiveSparse
    } else if sign && has_witness {
        StructuralVerdict::ConfirmL4
    } else {
        StructuralVerdict::RejectL4
    })
}

fn sign_tail_numerator(n: usize, wins: usize) -> Result<u128, Box<dyn Error>> {
    if wins > n {
        return Err("sign-tail wins exceed non-ties".into());
    }
    let mut numerator = 0_u128;
    for k in wins..=n {
        numerator = numerator
            .checked_add(choose(n, k)?)
            .ok_or("sign-tail numerator overflow")?;
    }
    Ok(numerator)
}

fn choose(n: usize, k: usize) -> Result<u128, Box<dyn Error>> {
    if k > n {
        return Err("binomial index exceeds population".into());
    }
    let k = k.min(n - k);
    let mut value = 1_u128;
    for index in 0..k {
        value = value
            .checked_mul(u128::try_from(n - index)?)
            .ok_or("binomial multiplication overflow")?
            .checked_div(u128::try_from(index + 1)?)
            .ok_or("binomial division by zero")?;
    }
    Ok(value)
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct WorkSummary {
    worker_setup_frames: Vec<u64>,
    scheduled: usize,
    executed: usize,
    skipped: usize,
    l1_selections: usize,
    l4_selections: usize,
    setup: u64,
    source_probe: u64,
    action: u64,
    probe: u64,
    experimental: u64,
    total: u64,
}

fn summarize_work(
    arms: &[ArmRecord],
    baseline_setup: u64,
    source_probe: u64,
) -> Result<WorkSummary, Box<dyn Error>> {
    if baseline_setup != EXPECTED_SETUP_FRAMES
        || source_probe != SOURCE_PROBE_FRAMES
        || arms.len() != ARMS
    {
        return Err("setup evidence does not match the preregistration".into());
    }
    validate_arms(arms)?;
    let mut setup = baseline_setup;
    let mut action = 0_u64;
    let mut probe = 0_u64;
    let mut scheduled = 0_usize;
    let mut executed = 0_usize;
    let mut skipped = 0_usize;
    let mut l1_selections = 0_usize;
    let mut l4_selections = 0_usize;
    for record in arms {
        action = action
            .checked_add(record.action_frames)
            .ok_or("action work overflow")?;
        probe = probe
            .checked_add(record.probe_frames)
            .ok_or("probe work overflow")?;
        if record.total_work_frames
            != record
                .action_frames
                .checked_add(record.probe_frames)
                .ok_or("lane component work overflow")?
        {
            return Err("arm work does not reconcile in summary".into());
        }
        scheduled = scheduled
            .checked_add(record.scheduled_slots)
            .ok_or("scheduled slot count overflow")?;
        executed = executed
            .checked_add(record.executed_slots)
            .ok_or("executed slot count overflow")?;
        skipped = skipped
            .checked_add(record.skipped_slots)
            .ok_or("skipped slot count overflow")?;
        match record.arm {
            ArmKind::L1 => {
                l1_selections = l1_selections
                    .checked_add(record.selections)
                    .ok_or("L1 selection count overflow")?;
            }
            ArmKind::L4 => {
                l4_selections = l4_selections
                    .checked_add(record.selections)
                    .ok_or("L4 selection count overflow")?;
            }
        }
    }
    setup = setup
        .checked_add(
            EXPECTED_SETUP_FRAMES
                .checked_mul(u64::try_from(WORKERS)?)
                .ok_or("worker setup work overflow")?,
        )
        .ok_or("setup work overflow")?;
    let expected_setup = EXPECTED_SETUP_FRAMES
        .checked_mul(u64::try_from(
            WORKERS.checked_add(1).ok_or("target count overflow")?,
        )?)
        .ok_or("expected setup work overflow")?;
    if setup != expected_setup
        || scheduled
            != PAIRS
                .checked_mul(2)
                .and_then(|value| value.checked_mul(SLOTS))
                .ok_or("scheduled slot bound overflow")?
        || executed.checked_add(skipped).ok_or("slot count overflow")? != scheduled
        || l1_selections != EXPECTED_L1_SELECTIONS
        || l4_selections != EXPECTED_L4_SELECTIONS
        || action > MAX_ACTION_FRAMES
        || probe > MAX_PROBE_FRAMES
    {
        return Err("work component exceeds the preregistered bound".into());
    }
    let experimental = action
        .checked_add(probe)
        .ok_or("experimental work overflow")?;
    let total = setup
        .checked_add(SOURCE_FRAMES)
        .and_then(|value| value.checked_add(source_probe))
        .and_then(|value| value.checked_add(experimental))
        .ok_or("total work overflow")?;
    if total > MAX_TOTAL_FRAMES {
        return Err("total work exceeds the preregistered bound".into());
    }
    Ok(WorkSummary {
        worker_setup_frames: arms
            .iter()
            .filter_map(|record| record.worker_setup_frames)
            .collect(),
        scheduled,
        executed,
        skipped,
        l1_selections,
        l4_selections,
        setup,
        source_probe,
        action,
        probe,
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
    let bound = limit.checked_add(1).ok_or("read bound overflow")?;
    let mut file = fs::File::open(path)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(bound)?)
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
    finish_sha256(Sha256::new_with_prefix(bytes))
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

fn hex_to_array(value: &str) -> Result<[u8; 32], Box<dyn Error>> {
    let bytes = value.as_bytes();
    if bytes.len() != 64 {
        return Err("SHA-256 text must contain exactly 64 hexadecimal bytes".into());
    }
    let mut output = [0_u8; 32];
    for (index, slot) in output.iter_mut().enumerate() {
        let high = hex_nibble(bytes[index * 2]).ok_or("invalid SHA-256 hexadecimal digit")?;
        let low = hex_nibble(bytes[index * 2 + 1]).ok_or("invalid SHA-256 hexadecimal digit")?;
        *slot = (high << 4) | low;
    }
    Ok(output)
}

fn hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smb::archive::SmbSelectorPath;

    fn synthetic_source() -> SmbInput {
        SmbInput {
            actions: (0..SOURCE_ACTIONS)
                .map(|index| {
                    ButtonChord::new(
                        u8::try_from(index % 256).expect("button fits u8"),
                        u8::try_from(2 + index % 119).expect("duration fits u8"),
                    )
                })
                .collect(),
        }
    }

    fn candidate(
        pair: usize,
        arm: ArmKind,
        id: usize,
        watermark: SmbProgressWatermark,
        actions: usize,
        hash_byte: u8,
        prefix_depth: usize,
    ) -> ChampionCandidate {
        let mechanical = SmbMechanicalState {
            world: watermark.world,
            level: watermark.level,
            progress: watermark.progress,
            ..SmbMechanicalState::default()
        };
        let input = SmbInput {
            actions: vec![ButtonChord::new(0, 2); actions],
        };
        let input_sha256_bytes = [hash_byte; 32];
        ChampionCandidate {
            pair,
            arm,
            id,
            prefix_depth,
            input,
            input_sha256: input_sha256_bytes
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect(),
            input_sha256_bytes,
            parent_lineage: vec![0, u64::try_from(id).expect("id fits u64")],
            endpoint: EndpointEvidence {
                action: ButtonChord::new(0, 2),
                input_actions: actions,
                input_sha256: String::new(),
                observation: SmbObservations {
                    frame_count: 0,
                    wram: Vec::new(),
                    decoded: mechanical,
                    milestones: SmbMilestones::default(),
                    changed_indices: Vec::new(),
                    dead: false,
                    log_line: String::new(),
                },
                mechanical,
                watermark,
                wram_sha256: String::new(),
                snapshot_sha256: Some(String::new()),
                key: None,
                milestones: SmbMilestones::default(),
                action_frames: 2,
                dead: false,
                failed: false,
                probe: Vec::new(),
                probe_survived: true,
                probe_frames: 0,
                admission: AdmissionOutcome::Retained {
                    id,
                    displaced: false,
                },
            },
            work_frames: 2,
        }
    }

    fn start_evidence(watermark: SmbProgressWatermark) -> StartEvidence {
        let mechanical = SmbMechanicalState {
            world: watermark.world,
            level: watermark.level,
            progress: watermark.progress,
            ..SmbMechanicalState::default()
        };
        StartEvidence {
            observation: SmbObservations {
                frame_count: 0,
                wram: Vec::new(),
                decoded: mechanical,
                milestones: SmbMilestones::default(),
                changed_indices: Vec::new(),
                dead: false,
                log_line: String::new(),
            },
            mechanical,
            wram_sha256: String::new(),
            snapshot_sha256: String::new(),
            dead: false,
            failed: false,
            milestones: SmbMilestones::default(),
        }
    }

    fn selector_accounting(selections: usize) -> SmbSelectorAccounting {
        SmbSelectorAccounting {
            policy: SmbArchiveSelectorPolicy::ConcentratedRecency,
            uniform_selections: u64::try_from(selections).expect("selection count fits u64"),
            ..SmbSelectorAccounting::default()
        }
    }

    fn synthetic_job(pair: usize, arm: ArmKind, job: usize) -> JobRecord {
        let action_count = if arm == ArmKind::L1 { 1 } else { PHRASE_LENGTH };
        let selection_slot = if arm == ArmKind::L1 {
            job
        } else {
            job * PHRASE_LENGTH
        };
        let prefixes = (0..action_count)
            .map(|offset| {
                let slot = selection_slot + offset;
                let mut endpoint = candidate(
                    pair,
                    arm,
                    slot + 1,
                    BASELINE_WATERMARK,
                    SOURCE_ACTIONS + offset + 1,
                    0,
                    offset + 1,
                )
                .endpoint;
                endpoint.admission = AdmissionOutcome::Rejected;
                PrefixRecord {
                    pair,
                    arm,
                    slot,
                    phrase: job,
                    prefix_depth: offset + 1,
                    source_index: slot,
                    selector_seed: u64::try_from(slot).expect("slot fits u64"),
                    selector_used: offset == 0,
                    current_parent_before: 0,
                    current_parent_after: 0,
                    start: start_evidence(BASELINE_WATERMARK),
                    endpoint,
                    productive: false,
                    active_ids: vec![0],
                    active_maximum: ActiveMaximum {
                        watermark: BASELINE_WATERMARK,
                        ids: vec![0],
                    },
                    total_work_frames: 2,
                }
            })
            .collect::<Vec<_>>();
        JobRecord {
            pair,
            arm,
            job,
            selection_slot,
            selector_seed: u64::try_from(selection_slot).expect("slot fits u64"),
            selector: SmbSelectorDraw {
                path: SmbSelectorPath::Uniform,
                classes_skipped: 0,
                counter_reset: false,
                concentration: None,
                waypoint: false,
            },
            original_parent_id: 0,
            parent_input_sha256: String::new(),
            parent_snapshot_sha256: String::new(),
            start: start_evidence(BASELINE_WATERMARK),
            prefixes,
            skipped: Vec::new(),
            productive: false,
            selector_accounting: selector_accounting(job + 1),
            total_work_frames: u64::try_from(action_count * 2).expect("work fits u64"),
        }
    }

    fn synthetic_arm(
        pair: usize,
        arm: ArmKind,
        maximum: SmbProgressWatermark,
        champion_candidates: Vec<ChampionCandidate>,
    ) -> ArmRecord {
        let ordinal = pair * 2 + usize::from(arm == ArmKind::L4);
        let selections = if arm == ArmKind::L1 {
            SLOTS
        } else {
            SLOTS / PHRASE_LENGTH
        };
        let jobs = (0..selections)
            .map(|job| synthetic_job(pair, arm, job))
            .collect::<Vec<_>>();
        let action_frames = u64::try_from(SLOTS * 2).expect("work fits u64");
        ArmRecord {
            record: "arm",
            ordinal,
            pair,
            arm,
            worker: ordinal % WORKERS,
            worker_setup_frames: (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES),
            initial_archive_sha256: String::new(),
            jobs,
            final_active_entries: Vec::new(),
            final_maximum: ActiveMaximum {
                watermark: maximum,
                ids: vec![0],
            },
            maximum_lineage_actions: SOURCE_ACTIONS,
            scheduled_slots: SLOTS,
            executed_slots: SLOTS,
            skipped_slots: 0,
            selections,
            selector_accounting: selector_accounting(selections),
            action_frames,
            probe_frames: 0,
            total_work_frames: action_frames,
            champion_candidates,
        }
    }

    fn paired_arms_with_boundary_candidates() -> Vec<ArmRecord> {
        let l1_maximum = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 166,
        };
        let mut arms = Vec::with_capacity(ARMS);
        for pair in 0..PAIRS {
            arms.push(synthetic_arm(pair, ArmKind::L1, l1_maximum, Vec::new()));
            let candidates = if pair == 0 {
                vec![
                    candidate(
                        pair,
                        ArmKind::L4,
                        1,
                        SmbProgressWatermark {
                            progress: 168,
                            ..l1_maximum
                        },
                        SOURCE_ACTIONS + 1,
                        1,
                        1,
                    ),
                    candidate(
                        pair,
                        ArmKind::L4,
                        2,
                        BASELINE_WATERMARK,
                        SOURCE_ACTIONS + 2,
                        2,
                        2,
                    ),
                    candidate(pair, ArmKind::L4, 3, l1_maximum, SOURCE_ACTIONS + 2, 3, 2),
                    candidate(
                        pair,
                        ArmKind::L4,
                        4,
                        SmbProgressWatermark {
                            progress: 167,
                            ..l1_maximum
                        },
                        SOURCE_ACTIONS + 2,
                        4,
                        2,
                    ),
                ]
            } else {
                vec![candidate(
                    pair,
                    ArmKind::L4,
                    1,
                    SmbProgressWatermark {
                        progress: 167,
                        ..l1_maximum
                    },
                    SOURCE_ACTIONS + 1,
                    u8::try_from(pair).expect("pair fits u8"),
                    1,
                )]
            };
            let maximum = candidates
                .iter()
                .map(|candidate| candidate.endpoint.watermark)
                .max()
                .expect("L4 candidates are nonempty");
            arms.push(synthetic_arm(pair, ArmKind::L4, maximum, candidates));
        }
        arms
    }

    #[test]
    fn seed_recipe_and_pair_projections_match_sealed_oracles() {
        verify_seed().expect("sealed seed is self-consistent");
        let recipes = derive_recipes(&synthetic_source()).expect("derive recipes");
        assert_eq!(recipes.len(), PAIRS);
        assert!(recipes.iter().all(|pair| pair.len() == SLOTS));
        assert_eq!(
            recipes[0][0],
            Recipe {
                pair: 0,
                slot: 0,
                source_index: 1_177,
                action: ButtonChord::new(153, 108),
                selector_seed: 4_688_544_944_769_344_307,
            }
        );
        assert_eq!(
            (
                recipes[15][127].source_index,
                recipes[15][127].selector_seed
            ),
            (1_524, 8_093_841_264_025_830_477)
        );
        assert_eq!(
            recipe_sha256(&recipes).expect("hash recipes"),
            "201577d284a0edf5d7d92711eaa74cc1c014150cc98cf7fb54bba95961da4c63"
        );
        let mut projections = projection_bytes(&recipes).expect("serialize projections");
        assert_eq!(projections.len(), PAIRS);
        projections.sort();
        assert!(projections.windows(2).all(|window| window[0] != window[1]));
        assert!(projection_sha256(&recipes).is_err());
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "98c1fa5c8122a7034ed2cd1f39f52e145fd4ca761e90d2300d60303e020ec83b"
        );
    }

    #[test]
    fn paired_sign_gate_requires_exact_tail_and_structural_witness() {
        assert_eq!(sign_tail_numerator(16, 16).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 8).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 7).expect("tail"), 9);
        let arms = paired_arms_with_boundary_candidates();
        let classified = classify_paired(&arms).expect("classify paired arms");
        assert_eq!(classified.non_ties, 16);
        assert_eq!(classified.l4_wins, 16);
        assert_eq!(
            (classified.tail_numerator, classified.tail_denominator),
            (1, 65_536)
        );
        assert_eq!(classified.verdict, StructuralVerdict::ConfirmL4);
        assert_eq!(classified.witnesses.len(), 1);
        assert_eq!(
            (
                classified.witnesses[0].pair,
                classified.witnesses[0].champion.id,
                classified.witnesses[0].champion.prefix_depth,
                classified.witnesses[0].champion.endpoint.watermark.progress,
            ),
            (0, 4, 2, 167)
        );
        let work = summarize_work(&arms, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES)
            .expect("reconcile paired work");
        assert_eq!(
            work.worker_setup_frames,
            vec![EXPECTED_SETUP_FRAMES; WORKERS]
        );
        assert_eq!(
            arms.iter()
                .map(|arm| serde_json::to_value(arm).expect("serialize arm"))
                .filter(|arm| arm.get("worker_setup_frames").is_some())
                .count(),
            WORKERS
        );

        let mut without_strict_witness = arms;
        without_strict_witness[1]
            .champion_candidates
            .retain(|candidate| candidate.id != 4);
        let classified =
            classify_paired(&without_strict_witness).expect("classify witness-free arms");
        assert!(classified.witnesses.is_empty());
        assert_eq!(classified.verdict, StructuralVerdict::RejectL4);

        let mut sparse = paired_arms_with_boundary_candidates();
        for pair in 7..PAIRS {
            let l1_maximum = sparse[pair * 2].final_maximum.watermark;
            let l4 = &mut sparse[pair * 2 + 1];
            l4.final_maximum.watermark = l1_maximum;
            l4.champion_candidates.clear();
        }
        let classified = classify_paired(&sparse).expect("classify sparse arms");
        assert_eq!(classified.non_ties, 7);
        assert!(!classified.witnesses.is_empty());
        assert_eq!(classified.verdict, StructuralVerdict::InconclusiveSparse);
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::InconclusiveSparse)
                .expect("serialize sparse verdict"),
            r#""INCONCLUSIVE_SPARSE""#
        );
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::ConfirmL4)
                .expect("serialize confirmation verdict"),
            r#""CONFIRM_L4""#
        );
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::RejectL4)
                .expect("serialize rejection verdict"),
            r#""REJECT_L4""#
        );
    }

    #[test]
    fn structural_sign_boundary_is_exact_and_sparse_takes_precedence() {
        assert_eq!(
            structural_verdict(8, 9, 256, true).expect("classify 9/256 tail"),
            StructuralVerdict::RejectL4
        );
        assert_eq!(
            structural_verdict(8, 1, 256, true).expect("classify 1/256 tail"),
            StructuralVerdict::ConfirmL4
        );
        assert_eq!(
            structural_verdict(7, 1, 128, true).expect("classify sparse tail"),
            StructuralVerdict::InconclusiveSparse
        );
    }

    #[test]
    fn champion_ranking_uses_full_watermark_then_registered_ties() {
        let base = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 166,
        };
        let champion = rank_champion(vec![
            candidate(0, ArmKind::L1, 9, base, 9, 0x10, 1),
            candidate(1, ArmKind::L4, 8, base, 8, 0x20, 4),
            candidate(1, ArmKind::L1, 7, base, 8, 0x20, 1),
            candidate(0, ArmKind::L4, 6, base, 8, 0x20, 3),
            candidate(0, ArmKind::L1, 5, base, 8, 0x20, 1),
        ])
        .expect("champion exists");
        assert_eq!(
            (champion.pair, champion.arm, champion.id),
            (0, ArmKind::L1, 5)
        );
        assert_eq!(verdict_for(Some(&champion)), Verdict::Adopt);

        let later_level = SmbProgressWatermark {
            world: 7,
            level: 2,
            progress: 0,
        };
        let cross_level = rank_champion(vec![
            candidate(0, ArmKind::L1, 1, base, 1, 0, 1),
            candidate(7, ArmKind::L4, 2, later_level, 20, 0xff, 4),
        ])
        .expect("cross-level champion");
        assert_eq!(cross_level.endpoint.watermark, later_level);
    }

    #[test]
    fn reply_errors_are_canonical_under_arrival_reordering() {
        let make = |ordinal: usize| ArmReply {
            ordinal,
            worker: ordinal % WORKERS,
            result: Err(format!("failure-{ordinal}")),
        };
        let ascending = (0..ARMS).map(make).collect::<Vec<_>>();
        let mut descending = (0..ARMS).rev().map(make).collect::<Vec<_>>();
        assert_eq!(
            consume_arm_replies(ascending)
                .expect_err("inner failure")
                .to_string(),
            consume_arm_replies(std::mem::take(&mut descending))
                .expect_err("inner failure")
                .to_string()
        );

        let malformed = vec![
            ArmReply {
                ordinal: ARMS,
                worker: 0,
                result: Err("out-of-range".to_owned()),
            },
            ArmReply {
                ordinal: 7,
                worker: 0,
                result: Err("wrong-worker".to_owned()),
            },
        ];
        let mut reversed = malformed
            .iter()
            .map(|reply| ArmReply {
                ordinal: reply.ordinal,
                worker: reply.worker,
                result: Err("same".to_owned()),
            })
            .collect::<Vec<_>>();
        reversed.reverse();
        let first = consume_arm_replies(malformed)
            .expect_err("malformed replies")
            .to_string();
        let second = consume_arm_replies(reversed)
            .expect_err("malformed replies")
            .to_string();
        assert_eq!(first, second);
        assert_eq!(first, "invalid arm reply: ordinal=7, worker=0");
    }

    #[test]
    fn lineage_and_source_probe_bounds_are_exact() {
        assert_eq!(SOURCE_ACTIONS.checked_add(SLOTS), Some(MAX_LINEAGE_ACTIONS));
        assert!(MAX_LINEAGE_ACTIONS < ACTION_LIMIT);
        let mut maximum = SOURCE_ACTIONS;
        record_lineage_actions(&mut maximum, MAX_LINEAGE_ACTIONS).expect("registered maximum");
        assert!(record_lineage_actions(&mut maximum, MAX_LINEAGE_ACTIONS + 1).is_err());

        let attempts = SOURCE_PROBE_TRANSCRIPT
            .into_iter()
            .map(|(mask, work_frames, dead, survived)| ProbeAttempt {
                mask,
                work_frames,
                dead,
                survived,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            source_probe_frames(&attempts).expect("source transcript"),
            SOURCE_PROBE_FRAMES
        );
        assert_eq!(
            MAX_ACTION_FRAMES
                .checked_add(MAX_PROBE_FRAMES)
                .and_then(|value| value.checked_add(SOURCE_FRAMES))
                .and_then(|value| value.checked_add(SOURCE_PROBE_FRAMES))
                .and_then(|value| {
                    value.checked_add(
                        EXPECTED_SETUP_FRAMES
                            * u64::try_from(WORKERS + 1).expect("target count fits u64"),
                    )
                }),
            Some(MAX_TOTAL_FRAMES)
        );
    }

    #[test]
    fn phrase_parent_updates_only_for_retained_or_duplicate_prefixes() {
        assert_eq!(
            next_current_parent(
                4,
                &AdmissionOutcome::Retained {
                    id: 9,
                    displaced: true,
                }
            ),
            9
        );
        assert_eq!(
            next_current_parent(4, &AdmissionOutcome::Duplicate { id: 7 }),
            7
        );
        for unchanged in [
            AdmissionOutcome::Terminal,
            AdmissionOutcome::ProbeRefused,
            AdmissionOutcome::Rejected,
        ] {
            assert_eq!(next_current_parent(4, &unchanged), 4);
        }
    }
}
