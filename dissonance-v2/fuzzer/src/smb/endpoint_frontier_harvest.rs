// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the World 8-2 p183 paired FULL/TAIL256 canary.

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

const FORMAT: &str = "smb-w8-2-p183-paired-full-tail256-canary-v1";
const PREREGISTRATION_COMMIT: &str = "d8ef4322a3d19ac2dd9a704417bdc7f7b909cc89";
const PREREGISTRATION_DOC_SHA256: &str =
    "2f9e57ffb03211d8959f606fb05f046c06cede0b8df2c5d8dabe30e834ce5a0e";
const CODE_BASE: &str = "734191b103a4106282349bd286afa1eabbf1d48a";
const AUTHORIZING_CONFIRMATION_PREREGISTRATION: &str = "d64cc8de";
const AUTHORIZING_CONFIRMATION_IMPLEMENTATION: &str = "98aa20a5";
const AUTHORIZING_CONFIRMATION_RESULT: &str = "734191b1";
const AUTHORIZING_CONFIRMATION_REPORT_SHA256: &str =
    "9fa87e073313acfa571c56f9b6004dc7e18de1fef5edab7c24030470a4a15230";
const SOURCE_FILE_SHA256: &str = "c56360d445ece8c6df51153943c7ab593a5639a92f9057f31907618b35cc0112";
const SOURCE_INPUT_SHA256: &str =
    "c56360d445ece8c6df51153943c7ab593a5639a92f9057f31907618b35cc0112";
const SOURCE_BYTES: usize = 110_445;
const SOURCE_WRAM_SHA256: &str = "37a3fe9b0285edf6ec9ac6ff23c3d6c1d4da64f12a5f280f6aed89737f47d160";
const SOURCE_SNAPSHOT_SHA256: &str =
    "dfb7d4a391a00f8340294887ca30b7abff3e200d3b7130fd8cf0042641af1098";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-w8-2-p183-paired-full-tail256-action-marginal-v1";
const SEED_LABEL_SHA256: &str = "864ef8e409a480588b1cd8629996ced6f651fc8443177dd7569049285e79ce02";
const MASTER_SEED: u64 = 6_377_277_434_759_761_542;
const EXPECTED_RECIPE_SHA256: &str =
    "039cfd75d3aee68251b3a20dae93b467dac3b5d794ec12b9ca69b8081f4933e0";
const EXPECTED_RECIPE_BYTES: usize = 250_741;
const EXPECTED_PROJECTION_SHA256: [&str; 16] = [
    "5b281ae12599811441b2d5cd869ee3587910f88175e6b6954e21939db0da1662",
    "5032716ce13364d01ea7de208a91af478e9b71986ad202bc23465f490bf413fc",
    "ea036424f333574f5b022163ce7e791214f69755915fd0aac94e4158b96e603d",
    "6869e0a520c486c9dd96e93e4343cba4da1e6c77bb9660f31f18bf78b11688be",
    "4a1ffeb740db1e6128e8a512795a6751a08273c6c83b9485578a450d7c04e28e",
    "978f4a4257175cc98b76a30c2c98ab5c18a710c8d3b07105e25fb31f83bdffd6",
    "60e74ddb4b664799f22846595a9f57fcfbe36f0c9696e85241e6f55e812b43b8",
    "1da288de32802080a4bf8a0b1b9580a08d4c9d9a42c7d8b873dcc8c46e187257",
    "9493d76c15606ecd89f7a71ab611f236031fde9b924da03113fa5f26364d05bf",
    "f8b2828a4363f65b0fdfe077ecf943b0823f96da7e72e639832106a357cc09f6",
    "22a80ab39649b173117a47f2036e6b30db162de7433e04a92f6f084e0cf30319",
    "dcb00c5be713674f81c08e004444dc9b8b2059e347dfdb4edbe36f6ad7d974f2",
    "0cc7fc639068c09c42802747465ac8c6b25b24157d148af723c066fd3315338e",
    "ffe36ebd1991a59f2d56d38f749aceb6516f0b0019a7b0a62498b4dcd0c876b0",
    "ee32bcbd774cb351a29dd9cb66d5fa75fbb22606fc94736c89128cfe7f316aad",
    "17141f20267f28ef793c54a1b45a0c33774a0753d39af7d1c37ce10d18fb12e8",
];
const EXPECTED_PROJECTION_BYTES: [usize; 16] = [
    15_390, 15_371, 15_356, 15_393, 15_388, 15_402, 15_356, 15_362, 15_344, 15_375, 15_379, 15_367,
    15_337, 15_384, 15_342, 15_346,
];
const SOURCE_ACTIONS: usize = 3_440;
const SOURCE_FRAMES: u64 = 160_902;
const PAIRS: usize = 16;
const ARMS: usize = 32;
const WORKERS: usize = 12;
const SLOTS: usize = 128;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 129;
const MAX_LINEAGE_ACTIONS: usize = 3_568;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 491_520;
const MAX_PROBE_FRAMES: u64 = 552_960;
const SOURCE_PROBE_FRAMES: u64 = 71;
const MAX_TOTAL_FRAMES: u64 = 1_210_146;
const EXPECTED_SELECTIONS: usize = 4_096;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const SOURCE_PROBE_MASKS: [u8; 2] = [0x00, 0x01];
const PROBE_FRAMES: u16 = 45;
const SOURCE_PROBE_TRANSCRIPT: [(u8, u64, bool, bool); 2] =
    [(0x00, 26, true, false), (0x01, 45, false, true)];
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 1,
    progress: 183,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 1,
    progress: 183,
    player_y_bucket: 9,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 1,
    progress: 183,
    player_y_bucket: 9,
    player_engine_state: 8,
    state_fingerprint: 55,
    room_x_bucket: 0,
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 129,
    hold_frames: 9,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    pair: usize,
    slot: usize,
    rank_word: u64,
    full_index: usize,
    full_action: ButtonChord,
    tail_index: usize,
    tail_action: ButtonChord,
    selector_seed: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ArmKind {
    Full,
    Tail256,
}

#[derive(Debug, Serialize)]
struct Config {
    pairs: usize,
    arms: usize,
    slots_per_arm: usize,
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
    source_probe_masks: [u8; 2],
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
struct CandidateRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    rank_word: u64,
    full_index: usize,
    full_action: ButtonChord,
    tail_index: usize,
    tail_action: ButtonChord,
    selector_seed: u64,
    parent_id: usize,
    start: StartEvidence,
    input: SmbInput,
    endpoint: EndpointEvidence,
    productive: bool,
    active_ids: Vec<usize>,
    active_maximum: ActiveMaximum,
    total_work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SlotRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    parent_id: usize,
    parent_input_sha256: String,
    parent_snapshot_sha256: String,
    start: StartEvidence,
    candidate: CandidateRecord,
    productive: bool,
    selector_accounting: SmbSelectorAccounting,
    total_work_frames: u64,
}

#[derive(Clone, Debug)]
struct RetainedEvidence {
    endpoint: EndpointEvidence,
    work_frames: u64,
    slot: usize,
    rank_word: u64,
    full_index: usize,
    full_action: ButtonChord,
    tail_index: usize,
    tail_action: ButtonChord,
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
    slot: usize,
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
    slots: Vec<SlotRecord>,
    final_active_entries: Vec<FinalEntryRecord>,
    final_maximum: ActiveMaximum,
    maximum_lineage_actions: usize,
    scheduled_slots: usize,
    executed_slots: usize,
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
    slot: usize,
    rank_word: u64,
    full_index: usize,
    full_action: ButtonChord,
    tail_index: usize,
    tail_action: ButtonChord,
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
    slot: usize,
    rank_word: u64,
    full_index: usize,
    full_action: ButtonChord,
    tail_index: usize,
    tail_action: ButtonChord,
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
    PromoteTail256,
    RetainFull,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairOutcomeRecord {
    pair: usize,
    full_maximum: SmbProgressWatermark,
    tail256_maximum: SmbProgressWatermark,
    outcome: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StructuralWitness {
    pair: usize,
    full_maximum: SmbProgressWatermark,
    champion: ChampionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairedClassificationRecord {
    record: &'static str,
    pairs: Vec<PairOutcomeRecord>,
    non_ties: usize,
    tail256_wins: usize,
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
    selections: usize,
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
    authorizing_confirmation_preregistration: &'static str,
    authorizing_confirmation_implementation: &'static str,
    authorizing_confirmation_result: &'static str,
    authorizing_confirmation_report_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    source_confirmation_pair: u64,
    source_confirmation_arm: &'static str,
    source_confirmation_entry_id: u64,
    source_confirmation_prefix_depth: u64,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    recipe_bytes: usize,
    recipe_sha256: &'a str,
    projection_bytes: &'static [usize; PAIRS],
    projection_sha256: &'a [String],
    trace_sha256: &'a str,
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

/// Run the sealed paired FULL/TAIL256 canary from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-w8-2-p183-paired-full-tail256-canary <input.json> <output.jsonl>")?,
    );
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

    verify_seed()?;
    let source_bytes = read_bounded(&source_path, MAX_SOURCE_BYTES, "input JSON")?;
    let source_file_sha256 = sha256_bytes(&source_bytes);
    if source_bytes.len() != SOURCE_BYTES || source_file_sha256 != SOURCE_FILE_SHA256 {
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
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        max_lineage_actions: MAX_LINEAGE_ACTIONS,
        selector: "concentrated_recency_fresh_seed_per_slot_v1",
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
        authorizing_confirmation_preregistration: AUTHORIZING_CONFIRMATION_PREREGISTRATION,
        authorizing_confirmation_implementation: AUTHORIZING_CONFIRMATION_IMPLEMENTATION,
        authorizing_confirmation_result: AUTHORIZING_CONFIRMATION_RESULT,
        authorizing_confirmation_report_sha256: AUTHORIZING_CONFIRMATION_REPORT_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        source_confirmation_pair: 2,
        source_confirmation_arm: "L1",
        source_confirmation_entry_id: 93,
        source_confirmation_prefix_depth: 1,
        rom_sha256: &rom_sha256,
        executable_sha256: &executable_sha256,
        bin_source_sha256: &bin_source_sha256,
        module_source_sha256: &module_source_sha256,
        seed_label: SEED_LABEL,
        seed_label_sha256: SEED_LABEL_SHA256,
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
        projection_bytes: &EXPECTED_PROJECTION_BYTES,
        projection_sha256: &projection_sha256,
        trace_sha256: &baseline.record.trace_sha256,
        config_sha256: &config_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    #[derive(Serialize)]
    struct RecipeRecord<'a> {
        record: &'static str,
        recipe_bytes: usize,
        recipe_sha256: &'a str,
        projection_bytes: &'static [usize; PAIRS],
        projection_sha256: &'a [String],
        recipes: &'a [Vec<Recipe>],
    }
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
        projection_bytes: &EXPECTED_PROJECTION_BYTES,
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
        selections: work.selections,
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
    let tail_start = source_len
        .checked_sub(256)
        .ok_or("source is shorter than TAIL256")?;
    let mut pairs = Vec::with_capacity(PAIRS);
    for pair in 0..PAIRS {
        let pair_u64 = u64::try_from(pair)?;
        let pair_seed = digest_word(&[
            &MASTER_SEED.to_le_bytes(),
            b"p183-full-tail256-pair",
            &pair_u64.to_le_bytes(),
        ])?;
        let mut slots = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let slot_u64 = u64::try_from(slot)?;
            let rank_word = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"p183-full-tail256-rank",
                &slot_u64.to_le_bytes(),
            ])?;
            let full_index = usize::try_from(rank_word % source_len)?;
            let tail_index = usize::try_from(
                tail_start
                    .checked_add(rank_word % 256)
                    .ok_or("tail index overflow")?,
            )?;
            let full_action = *source
                .actions
                .get(full_index)
                .ok_or("derived FULL index is out of bounds")?;
            let tail_action = *source
                .actions
                .get(tail_index)
                .ok_or("derived TAIL256 index is out of bounds")?;
            let selector_seed = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"p183-full-tail256-parent",
                &slot_u64.to_le_bytes(),
            ])?;
            slots.push(Recipe {
                pair,
                slot,
                rank_word,
                full_index,
                full_action,
                tail_index,
                tail_action,
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
                recipe.rank_word,
                u64::try_from(recipe.full_index)?,
                recipe.full_action,
                u64::try_from(recipe.tail_index)?,
                recipe.tail_action,
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
                    recipe.rank_word,
                    u64::try_from(recipe.full_index)?,
                    recipe.full_action,
                    u64::try_from(recipe.tail_index)?,
                    recipe.tail_action,
                    recipe.selector_seed,
                ))
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        let bytes = serde_json::to_vec(&identity)?;
        if recipes
            .iter()
            .map(|recipe| recipe.full_action)
            .eq(recipes.iter().map(|recipe| recipe.tail_action))
        {
            return Err("FULL and TAIL256 action vectors are identical within a pair".into());
        }
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
                .name(format!("paired-action-{worker}"))
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
            handle.join().map_err(|_| "paired-action worker panicked")?;
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
        ArmKind::Full
    } else {
        ArmKind::Tail256
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
    let mut slots = Vec::with_capacity(SLOTS);
    let mut retained: Vec<Option<RetainedEvidence>> = vec![None];
    let mut action_total = 0_u64;
    let mut probe_total = 0_u64;
    let mut maximum_lineage_actions = SOURCE_ACTIONS;

    for slot in 0..SLOTS {
        let recipe = recipes.get(slot).ok_or("missing slot recipe")?;
        if recipe.pair != pair || recipe.slot != slot {
            return Err("slot recipe order is not canonical".into());
        }
        let action = match arm {
            ArmKind::Full => recipe.full_action,
            ArmKind::Tail256 => recipe.tail_action,
        };
        let mut rand = StdRand::with_seed(recipe.selector_seed);
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
        let start = StartEvidence {
            observation: target.observe(),
            mechanical: smb_mechanical_state_from_wram(target.wram()),
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256: parent_snapshot_sha256.clone(),
            dead: target.is_dead(),
            failed: target.exit_kind() != ExitKind::Ok,
            milestones: parent_report.milestones,
        };
        if start.dead || start.failed {
            return Err("selector returned a terminal or failed parent".into());
        }

        let slot_before = target.frames_clocked();
        target.apply(&action);
        let action_frames = target
            .frames_clocked()
            .checked_sub(slot_before)
            .ok_or("action work counter moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a full action".into());
        }
        let dead = target.is_dead();
        if action_frames > u64::from(action.bounded_hold_frames())
            || (!dead && action_frames != u64::from(action.bounded_hold_frames()))
        {
            return Err("full action work does not match its bounded duration".into());
        }
        let observation = target.observe();
        let mechanical = smb_mechanical_state_from_wram(target.wram());
        let mut milestones = parent_report.milestones;
        merge_action_milestones(&mut milestones, target)?;
        let candidate_input = appended_input(&parent_report.input, action)?;
        record_lineage_actions(&mut maximum_lineage_actions, candidate_input.actions.len())?;
        let input_sha256 = sha256_json(&candidate_input)?;
        let wram_sha256 = sha256_bytes(target.wram());
        let mut snapshot_sha256 = None;
        let mut key = None;
        let mut probe = Vec::new();
        let mut probe_survived = false;
        let mut probe_frames = 0_u64;
        let admission = if dead {
            AdmissionOutcome::Terminal
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot ordinary slot endpoint")?;
            let candidate_snapshot_sha256 = sha256_json(&snapshot)?;
            let candidate_key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
            let (attempts, survived, work) = run_probe(target, &snapshot)?;
            probe = attempts;
            probe_survived = survived;
            probe_frames = work;
            snapshot_sha256 = Some(candidate_snapshot_sha256);
            key = Some(candidate_key);
            if survived {
                insert_candidate(
                    &mut archive,
                    Some(parent_id),
                    u64::try_from(slot.checked_add(1).ok_or("execution overflow")?)?,
                    ArchiveCandidate {
                        input: candidate_input.clone(),
                        key: candidate_key,
                        milestones,
                    },
                    snapshot,
                )?
            } else {
                AdmissionOutcome::ProbeRefused
            }
        };
        let endpoint = EndpointEvidence {
            action,
            input_actions: parent_report
                .input
                .actions
                .len()
                .checked_add(1)
                .ok_or("candidate action count overflow")?,
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
        let productive = endpoint.admission.newly_retained_id().is_some();
        if let Some(id) = endpoint.admission.newly_retained_id() {
            if id != retained.len() {
                return Err("retained evidence is not insertion-order aligned".into());
            }
            retained.push(Some(RetainedEvidence {
                endpoint: endpoint.clone(),
                work_frames: action_frames
                    .checked_add(probe_frames)
                    .ok_or("retained work overflow")?,
                slot,
                rank_word: recipe.rank_word,
                full_index: recipe.full_index,
                full_action: recipe.full_action,
                tail_index: recipe.tail_index,
                tail_action: recipe.tail_action,
            }));
        } else if archive.entries.len() != retained.len() {
            return Err("nonallocating admission changed archive length".into());
        }
        let slot_work = target
            .frames_clocked()
            .checked_sub(slot_before)
            .ok_or("slot work counter moved backwards")?;
        if slot_work
            != action_frames
                .checked_add(probe_frames)
                .ok_or("slot component work overflow")?
        {
            return Err("slot work does not reconcile with components".into());
        }
        action_total = action_total
            .checked_add(action_frames)
            .ok_or("arm action work overflow")?;
        probe_total = probe_total
            .checked_add(probe_frames)
            .ok_or("arm probe work overflow")?;
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, productive, slot_work)?;
        slots.push(SlotRecord {
            pair,
            arm,
            slot,
            selector_seed: recipe.selector_seed,
            selector,
            parent_id,
            parent_input_sha256,
            parent_snapshot_sha256,
            start: start.clone(),
            candidate: CandidateRecord {
                pair,
                arm,
                slot,
                rank_word: recipe.rank_word,
                full_index: recipe.full_index,
                full_action: recipe.full_action,
                tail_index: recipe.tail_index,
                tail_action: recipe.tail_action,
                selector_seed: recipe.selector_seed,
                parent_id,
                start,
                input: candidate_input,
                endpoint,
                productive,
                active_ids: active_ids(&archive)?,
                active_maximum: active_maximum(&archive)?,
                total_work_frames: slot_work,
            },
            productive,
            selector_accounting: archive.selector_report(),
            total_work_frames: slot_work,
        });
    }

    let total_work_frames = action_total
        .checked_add(probe_total)
        .ok_or("arm work overflow")?;
    let arm_delta = target
        .frames_clocked()
        .checked_sub(arm_work_before)
        .ok_or("arm work counter moved backwards")?;
    if arm_delta != total_work_frames || slots.len() != SLOTS {
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
        slots,
        final_active_entries,
        final_maximum: active_maximum(&archive)?,
        maximum_lineage_actions,
        scheduled_slots: SLOTS,
        executed_slots: SLOTS,
        selections: SLOTS,
        selector_accounting: archive.selector_report(),
        action_frames: action_total,
        probe_frames: probe_total,
        total_work_frames,
        champion_candidates,
    })
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
            slot: evidence.slot,
        });
        candidates.push(ChampionCandidate {
            pair,
            arm,
            id,
            slot: evidence.slot,
            rank_word: evidence.rank_word,
            full_index: evidence.full_index,
            full_action: evidence.full_action,
            tail_index: evidence.tail_index,
            tail_action: evidence.tail_action,
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
            ArmKind::Full
        } else {
            ArmKind::Tail256
        };
        let accounted_selections = selector_selections(record.selector_accounting)?;
        if record.ordinal != ordinal
            || record.pair != ordinal / 2
            || record.arm != expected_arm
            || record.worker != ordinal % WORKERS
            || record.worker_setup_frames != (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES)
            || record.slots.len() != SLOTS
            || record.selections != SLOTS
            || accounted_selections != u64::try_from(SLOTS)?
            || record.selector_accounting.policy != SmbArchiveSelectorPolicy::ConcentratedRecency
            || record.selector_accounting.waypoint_selections != 0
            || record.scheduled_slots != SLOTS
            || record.executed_slots != SLOTS
            || !(SOURCE_ACTIONS..=MAX_LINEAGE_ACTIONS).contains(&record.maximum_lineage_actions)
        {
            return Err("arm record order or shape is not canonical".into());
        }
        for (slot, slot_record) in record.slots.iter().enumerate() {
            let expected_action = match expected_arm {
                ArmKind::Full => slot_record.candidate.full_action,
                ArmKind::Tail256 => slot_record.candidate.tail_action,
            };
            let candidate_input_sha256 = sha256_json(&slot_record.candidate.input)?;
            if slot_record.pair != record.pair
                || slot_record.arm != record.arm
                || slot_record.slot != slot
                || slot_record.candidate.pair != record.pair
                || slot_record.candidate.arm != record.arm
                || slot_record.candidate.slot != slot
                || slot_record.candidate.selector_seed != slot_record.selector_seed
                || slot_record.candidate.parent_id != slot_record.parent_id
                || slot_record.candidate.start != slot_record.start
                || slot_record.candidate.input.actions.last() != Some(&expected_action)
                || slot_record.candidate.input.actions.len()
                    != slot_record.candidate.endpoint.input_actions
                || candidate_input_sha256 != slot_record.candidate.endpoint.input_sha256
                || slot_record.candidate.endpoint.action != expected_action
                || slot_record.candidate.productive != slot_record.productive
                || slot_record.total_work_frames != slot_record.candidate.total_work_frames
                || selector_selections(slot_record.selector_accounting)?
                    != u64::try_from(slot.checked_add(1).ok_or("slot count overflow")?)?
                || slot_record
                    .candidate
                    .endpoint
                    .admission
                    .newly_retained_id()
                    .is_some()
                    != slot_record.productive
            {
                return Err("slot record order or accounting is not canonical".into());
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
        slot: candidate.slot,
        rank_word: candidate.rank_word,
        full_index: candidate.full_index,
        full_action: candidate.full_action,
        tail_index: candidate.tail_index,
        tail_action: candidate.tail_action,
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
    let mut tail256_wins = 0_usize;
    let mut witnesses = Vec::new();
    for pair in 0..PAIRS {
        let full = arms.get(pair * 2).ok_or("missing FULL arm")?;
        let tail256 = arms
            .get(
                pair.checked_mul(2)
                    .and_then(|value| value.checked_add(1))
                    .ok_or("arm index overflow")?,
            )
            .ok_or("missing TAIL256 arm")?;
        let outcome = match tail256
            .final_maximum
            .watermark
            .cmp(&full.final_maximum.watermark)
        {
            std::cmp::Ordering::Greater => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                tail256_wins = tail256_wins
                    .checked_add(1)
                    .ok_or("TAIL256 win count overflow")?;
                "TAIL256_WIN"
            }
            std::cmp::Ordering::Less => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                "FULL_WIN"
            }
            std::cmp::Ordering::Equal => "TIE",
        };
        pairs.push(PairOutcomeRecord {
            pair,
            full_maximum: full.final_maximum.watermark,
            tail256_maximum: tail256.final_maximum.watermark,
            outcome,
        });
        witnesses.extend(structural_witnesses(pair, full, tail256));
    }
    witnesses.sort_by_key(|witness| (witness.pair, witness.champion.id));
    let tail_numerator = sign_tail_numerator(non_ties, tail256_wins)?;
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
        tail256_wins,
        tail_numerator,
        tail_denominator,
        witnesses,
        verdict,
    })
}

fn structural_witnesses(
    pair: usize,
    full: &ArmRecord,
    tail256: &ArmRecord,
) -> Vec<StructuralWitness> {
    tail256
        .champion_candidates
        .iter()
        .filter(|candidate| is_tail256_witness(candidate, full.final_maximum.watermark))
        .map(|candidate| StructuralWitness {
            pair,
            full_maximum: full.final_maximum.watermark,
            champion: champion_record(candidate),
        })
        .collect()
}

fn is_tail256_witness(candidate: &ChampionCandidate, full_maximum: SmbProgressWatermark) -> bool {
    candidate.arm == ArmKind::Tail256
        && candidate.endpoint.admission.newly_retained_id() == Some(candidate.id)
        && !candidate.endpoint.dead
        && !candidate.endpoint.failed
        && candidate.endpoint.probe_survived
        && candidate.endpoint.watermark > BASELINE_WATERMARK
        && candidate.endpoint.watermark > full_maximum
        && candidate.tail_action != candidate.full_action
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
        StructuralVerdict::PromoteTail256
    } else {
        StructuralVerdict::RetainFull
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
    selections: usize,
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
    let mut selections = 0_usize;
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
        selections = selections
            .checked_add(record.selections)
            .ok_or("selection count overflow")?;
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
        || executed != scheduled
        || selections != EXPECTED_SELECTIONS
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
        selections,
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
        slot: usize,
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
            slot,
            rank_word: u64::try_from(slot).expect("slot fits u64"),
            full_index: slot,
            full_action: ButtonChord::new(1, 2),
            tail_index: SOURCE_ACTIONS - 1,
            tail_action: ButtonChord::new(2, 2),
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

    fn synthetic_slot(pair: usize, arm: ArmKind, slot: usize) -> SlotRecord {
        let start = start_evidence(BASELINE_WATERMARK);
        let mut endpoint = candidate(
            pair,
            arm,
            slot + 1,
            BASELINE_WATERMARK,
            SOURCE_ACTIONS + 1,
            0,
            slot,
        )
        .endpoint;
        let full_action = ButtonChord::new(1, 2);
        let tail_action = ButtonChord::new(2, 2);
        endpoint.action = match arm {
            ArmKind::Full => full_action,
            ArmKind::Tail256 => tail_action,
        };
        let input = SmbInput {
            actions: vec![endpoint.action],
        };
        endpoint.input_actions = input.actions.len();
        endpoint.input_sha256 = sha256_json(&input).expect("hash synthetic input");
        endpoint.admission = AdmissionOutcome::Rejected;
        let candidate = CandidateRecord {
            pair,
            arm,
            slot,
            rank_word: u64::try_from(slot).expect("slot fits u64"),
            full_index: slot,
            full_action,
            tail_index: SOURCE_ACTIONS - 1,
            tail_action,
            selector_seed: u64::try_from(slot).expect("slot fits u64"),
            parent_id: 0,
            start: start.clone(),
            input,
            endpoint,
            productive: false,
            active_ids: vec![0],
            active_maximum: ActiveMaximum {
                watermark: BASELINE_WATERMARK,
                ids: vec![0],
            },
            total_work_frames: 2,
        };
        SlotRecord {
            pair,
            arm,
            slot,
            selector_seed: u64::try_from(slot).expect("slot fits u64"),
            selector: SmbSelectorDraw {
                path: SmbSelectorPath::Uniform,
                classes_skipped: 0,
                counter_reset: false,
                concentration: None,
                waypoint: false,
            },
            parent_id: 0,
            parent_input_sha256: String::new(),
            parent_snapshot_sha256: String::new(),
            start,
            candidate,
            productive: false,
            selector_accounting: selector_accounting(slot + 1),
            total_work_frames: 2,
        }
    }

    fn synthetic_arm(
        pair: usize,
        arm: ArmKind,
        maximum: SmbProgressWatermark,
        champion_candidates: Vec<ChampionCandidate>,
    ) -> ArmRecord {
        let ordinal = pair * 2 + usize::from(arm == ArmKind::Tail256);
        let slots = (0..SLOTS)
            .map(|slot| synthetic_slot(pair, arm, slot))
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
            slots,
            final_active_entries: Vec::new(),
            final_maximum: ActiveMaximum {
                watermark: maximum,
                ids: vec![0],
            },
            maximum_lineage_actions: SOURCE_ACTIONS,
            scheduled_slots: SLOTS,
            executed_slots: SLOTS,
            selections: SLOTS,
            selector_accounting: selector_accounting(SLOTS),
            action_frames,
            probe_frames: 0,
            total_work_frames: action_frames,
            champion_candidates,
        }
    }

    fn paired_arms_with_boundary_candidates() -> Vec<ArmRecord> {
        let full_maximum = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 184,
        };
        let mut arms = Vec::with_capacity(ARMS);
        for pair in 0..PAIRS {
            arms.push(synthetic_arm(pair, ArmKind::Full, full_maximum, Vec::new()));
            let tail256_maximum = SmbProgressWatermark {
                progress: 185,
                ..full_maximum
            };
            let candidate = candidate(
                pair,
                ArmKind::Tail256,
                1,
                tail256_maximum,
                SOURCE_ACTIONS + 1,
                u8::try_from(pair).expect("pair fits u8"),
                pair,
            );
            arms.push(synthetic_arm(
                pair,
                ArmKind::Tail256,
                tail256_maximum,
                vec![candidate],
            ));
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
                rank_word: 11_285_664_908_963_401_769,
                full_index: 1_609,
                full_action: ButtonChord::new(73, 64),
                tail_index: 3_225,
                tail_action: ButtonChord::new(153, 14),
                selector_seed: 14_881_283_772_109_067_701,
            }
        );
        assert_eq!(
            recipes[15][127],
            Recipe {
                pair: 15,
                slot: 127,
                rank_word: 7_197_191_640_346_007_267,
                full_index: 2_627,
                full_action: ButtonChord::new(67, 11),
                tail_index: 3_411,
                tail_action: ButtonChord::new(83, 81),
                selector_seed: 7_063_694_047_214_560_074,
            }
        );
        assert_eq!(
            recipe_sha256(&recipes).expect("hash synthetic recipe"),
            "affd6ec44508c51d062227349c879fcb94073c7d91438a5fa9cccf1b2b13414f"
        );
        let mut projections = projection_bytes(&recipes).expect("serialize projections");
        assert_eq!(projections.len(), PAIRS);
        projections.sort();
        assert!(projections.windows(2).all(|window| window[0] != window[1]));
        assert!(projection_sha256(&recipes).is_err());
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "039cfd75d3aee68251b3a20dae93b467dac3b5d794ec12b9ca69b8081f4933e0"
        );
        assert_eq!(EXPECTED_RECIPE_BYTES, 250_741);
    }

    #[test]
    fn paired_sign_gate_requires_exact_tail_and_structural_witness() {
        assert_eq!(sign_tail_numerator(16, 16).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 8).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 7).expect("tail"), 9);
        let arms = paired_arms_with_boundary_candidates();
        let classified = classify_paired(&arms).expect("classify paired arms");
        assert_eq!(classified.non_ties, 16);
        assert_eq!(classified.tail256_wins, 16);
        assert!(
            classified
                .pairs
                .iter()
                .all(|outcome| outcome.outcome == "TAIL256_WIN")
        );
        assert_eq!(
            (classified.tail_numerator, classified.tail_denominator),
            (1, 65_536)
        );
        assert_eq!(classified.verdict, StructuralVerdict::PromoteTail256);
        assert_eq!(classified.witnesses.len(), PAIRS);
        assert_eq!(
            (
                classified.witnesses[0].pair,
                classified.witnesses[0].champion.id,
                classified.witnesses[0].champion.slot,
                classified.witnesses[0].champion.endpoint.watermark.progress,
            ),
            (0, 1, 0, 185)
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
        for pair in 0..PAIRS {
            without_strict_witness[pair * 2 + 1]
                .champion_candidates
                .clear();
        }
        let classified =
            classify_paired(&without_strict_witness).expect("classify witness-free arms");
        assert!(classified.witnesses.is_empty());
        assert_eq!(classified.verdict, StructuralVerdict::RetainFull);

        let mut sparse = paired_arms_with_boundary_candidates();
        for pair in 7..PAIRS {
            let full_maximum = sparse[pair * 2].final_maximum.watermark;
            let tail256 = &mut sparse[pair * 2 + 1];
            tail256.final_maximum.watermark = full_maximum;
            tail256.champion_candidates.clear();
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
            serde_json::to_string(&StructuralVerdict::PromoteTail256)
                .expect("serialize confirmation verdict"),
            r#""PROMOTE_TAIL256""#
        );
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::RetainFull)
                .expect("serialize rejection verdict"),
            r#""RETAIN_FULL""#
        );
        assert_eq!(
            serde_json::to_string(&ArmKind::Full).expect("serialize FULL arm"),
            r#""FULL""#
        );
        assert_eq!(
            serde_json::to_string(&ArmKind::Tail256).expect("serialize TAIL256 arm"),
            r#""TAIL256""#
        );
    }

    #[test]
    fn structural_sign_boundary_is_exact_and_sparse_takes_precedence() {
        assert_eq!(
            structural_verdict(8, 9, 256, true).expect("classify 9/256 tail"),
            StructuralVerdict::RetainFull
        );
        assert_eq!(
            structural_verdict(8, 1, 256, true).expect("classify 1/256 tail"),
            StructuralVerdict::PromoteTail256
        );
        assert_eq!(
            structural_verdict(7, 1, 128, true).expect("classify sparse tail"),
            StructuralVerdict::InconclusiveSparse
        );
    }

    #[test]
    fn tail256_witness_requires_strict_progress_and_a_different_paired_chord() {
        let full_maximum = SmbProgressWatermark {
            progress: 184,
            ..BASELINE_WATERMARK
        };
        let strict = SmbProgressWatermark {
            progress: 185,
            ..BASELINE_WATERMARK
        };
        let mut witness = candidate(0, ArmKind::Tail256, 1, strict, SOURCE_ACTIONS + 1, 1, 0);
        assert!(is_tail256_witness(&witness, full_maximum));

        witness.tail_action = witness.full_action;
        assert!(!is_tail256_witness(&witness, full_maximum));
        witness.tail_action = ButtonChord::new(2, 2);
        witness.endpoint.watermark = full_maximum;
        assert!(!is_tail256_witness(&witness, full_maximum));
        witness.endpoint.watermark = BASELINE_WATERMARK;
        assert!(!is_tail256_witness(&witness, BASELINE_WATERMARK));
        witness.endpoint.watermark = strict;
        witness.endpoint.probe_survived = false;
        assert!(!is_tail256_witness(&witness, full_maximum));
        witness.endpoint.probe_survived = true;
        witness.endpoint.admission = AdmissionOutcome::Duplicate { id: witness.id };
        assert!(!is_tail256_witness(&witness, full_maximum));
    }

    #[test]
    fn champion_ranking_uses_full_watermark_then_registered_ties() {
        let base = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 184,
        };
        let champion = rank_champion(vec![
            candidate(0, ArmKind::Full, 9, base, 9, 0x10, 1),
            candidate(1, ArmKind::Tail256, 8, base, 8, 0x20, 4),
            candidate(1, ArmKind::Full, 7, base, 8, 0x20, 1),
            candidate(0, ArmKind::Tail256, 6, base, 8, 0x20, 3),
            candidate(0, ArmKind::Full, 5, base, 8, 0x20, 1),
        ])
        .expect("champion exists");
        assert_eq!(
            (champion.pair, champion.arm, champion.id),
            (0, ArmKind::Full, 5)
        );
        assert_eq!(verdict_for(Some(&champion)), Verdict::Adopt);

        let later_level = SmbProgressWatermark {
            world: 7,
            level: 2,
            progress: 0,
        };
        let cross_level = rank_champion(vec![
            candidate(0, ArmKind::Full, 1, base, 1, 0, 1),
            candidate(7, ArmKind::Tail256, 2, later_level, 20, 0xff, 4),
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
}
