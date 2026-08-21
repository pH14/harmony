// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the World 8-2 p196 paired B1/B2 sibling canary.

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

const FORMAT: &str = "smb-w8-2-p196-paired-b1-b2-sibling-canary-v1";
const PREREGISTRATION_COMMIT: &str = "ee578070f8c53718dbfaf1cc4da9fd4cf5ebf51f";
const PREREGISTRATION_DOC_SHA256: &str =
    "6988acb3162ac2ffcadb191794a5d6c2b85c3a2ff07d3d3d39161ca3b0b0cb76";
const CODE_BASE: &str = "c045412f1575f9921a86347ff8ea75a69d0565f2";
const AUTHORIZING_P183_PREREGISTRATION: &str = "d8ef4322";
const AUTHORIZING_P183_IMPLEMENTATION: &str = "5a4635f9";
const AUTHORIZING_P183_RESULT: &str = "c045412f";
const AUTHORIZING_P183_REPORT_SHA256: &str =
    "7014812f683986c83f246eebd78e8efe9b98ff1576e5760e2fd1e9f269d88203";
const SOURCE_FILE_SHA256: &str = "72f6dc1ed54ef824c73c794e03410b9d64502ede032fc8b787d4ac67763b403d";
const SOURCE_INPUT_SHA256: &str =
    "72f6dc1ed54ef824c73c794e03410b9d64502ede032fc8b787d4ac67763b403d";
const SOURCE_BYTES: usize = 110_605;
const SOURCE_WRAM_SHA256: &str = "49b2721d7533f4c45249d60ce9ec715e2ef2d5d2c1e19776bd6e2ef75d4c2e80";
const SOURCE_SNAPSHOT_SHA256: &str =
    "0627939cc2ca87cbdeea4e74705a09145150f22b7b6d88543a63e4365b201c83";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-w8-2-p196-paired-b1-b2-sibling-burst-v1";
const SEED_LABEL_SHA256: &str = "cc83cf81c4262aa15a68a65b32772b0cb8af2dc0dffd00f8f71929142ef9e958";
const MASTER_SEED: u64 = 11_613_137_214_561_551_308;
const EXPECTED_RECIPE_SHA256: &str =
    "806e5c4d3200d983c7043aded109bd516463e543bba66c2867b24dd3e0484131";
const EXPECTED_RECIPE_BYTES: usize = 132_496;
const EXPECTED_PROJECTION_SHA256: [&str; 16] = [
    "cf1e8b34673bea48574c38aa6847b2e053fb61aafdaeb475f08d2d43279e2c8c",
    "7ba69e41612b223125eb41b1f162533c8b2a6eeacaff058baab8112d289c6ad1",
    "e87356cf9caaba315df2a08996983b268dcd57ccf3e5b1568ad87be4c93fa0e0",
    "a2f4e796a03b594afd186eb4488dcc43289ddf57d352ed7391c4f779b307c4fc",
    "2685aa50a96e50ba49b1fc53c285b4ac165b61051bd55d48af2da9fb2b30e2a3",
    "465cbd6785e8dd4285f29229c53ffabb9e19fe19806181cc21a4a17b7d5abfa8",
    "a0bac85a19bbe649f73e4cc086c69295a1bb5ffc601b33adfb0f383c1d4a09ab",
    "e46cc49b2be21d70624459bcf8fa49866fe4a837ba0691ccba5dfe2c4d90ce5f",
    "a5784588968539cc096b8eef6f6770dc1d3c80609de78579e220e264a89abcf7",
    "5f7ad5e3f879477046210d665cffc24c77a9385f47b861f0aa38c8cc542a4713",
    "b965f6b866ffa16531d125cbda50f8ee4b78673e98801a93e97b7e767beeb1d5",
    "965ae891b652368504bfb648cda91a10f9d0a0b05b72276b0c56a80258dba5c0",
    "d9a3495b41e856f7cd8b1b62fe351cb9865bb2389d1958fdda427d4bb39b5fe9",
    "921806407a58b8465d343e055366c13b3b858122b58e27126b83461b4223a081",
    "72ba5612482d19cc31924bbe705745d9fd3f878c95b6af82df6fcbefa5a8b705",
    "4a55900bd6882355d938b89b96e3f2c74af9a05cf2c9b4877e5e2c44c9b3e40b",
];
const EXPECTED_PROJECTION_BYTES: [usize; 16] = [
    8_003, 7_983, 7_964, 7_970, 7_985, 7_966, 7_986, 7_988, 7_990, 7_952, 7_965, 7_975, 7_971,
    7_997, 7_975, 7_977,
];
const SOURCE_ACTIONS: usize = 3_445;
const SOURCE_FRAMES: u64 = 161_116;
const PAIRS: usize = 16;
const ARMS: usize = 32;
const WORKERS: usize = 12;
const SLOTS: usize = 128;
const BURST_SIZE: usize = 2;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 129;
const MAX_LINEAGE_ACTIONS: usize = 3_573;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 491_520;
const MAX_PROBE_FRAMES: u64 = 552_960;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 1_210_334;
const EXPECTED_B1_SELECTIONS: usize = 2_048;
const EXPECTED_B2_SELECTIONS: usize = 1_024;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const SOURCE_PROBE_MASKS: [u8; 1] = [0x00];
const PROBE_FRAMES: u16 = 45;
const SOURCE_PROBE_TRANSCRIPT: [(u8, u64, bool, bool); 1] = [(0x00, 45, false, true)];
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 1,
    progress: 196,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 1,
    progress: 196,
    player_y_bucket: 6,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 1,
    progress: 196,
    player_y_bucket: 6,
    player_engine_state: 8,
    state_fingerprint: 9,
    room_x_bucket: 0,
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 131,
    hold_frames: 74,
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
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ArmKind {
    B1,
    B2,
}

#[derive(Debug, Serialize)]
struct Config {
    pairs: usize,
    arms: usize,
    slots_per_arm: usize,
    burst_size: usize,
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
struct CandidateRecord {
    pair: usize,
    arm: ArmKind,
    slot: usize,
    burst: usize,
    sibling_offset: usize,
    recipe: Recipe,
    selector_used: bool,
    original_parent_id: usize,
    original_parent_input_sha256: String,
    original_parent_snapshot_sha256: String,
    start: StartEvidence,
    input: SmbInput,
    endpoint: EndpointEvidence,
    productive: bool,
    active_ids: Vec<usize>,
    active_maximum: ActiveMaximum,
    total_work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SelectionRecord {
    pair: usize,
    arm: ArmKind,
    burst: usize,
    selection_slot: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    original_parent_id: usize,
    original_parent_input_sha256: String,
    original_parent_snapshot_sha256: String,
    start: StartEvidence,
    candidates: Vec<CandidateRecord>,
    productive: bool,
    selector_accounting: SmbSelectorAccounting,
    total_work_frames: u64,
}

#[derive(Clone, Debug)]
struct RetainedEvidence {
    endpoint: EndpointEvidence,
    work_frames: u64,
    slot: usize,
    burst: usize,
    sibling_offset: usize,
    recipe: Recipe,
    original_parent_id: usize,
    burst_recipes: Vec<Recipe>,
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
    burst: usize,
    sibling_offset: usize,
    original_parent_id: usize,
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
    selection_records: Vec<SelectionRecord>,
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
    burst: usize,
    sibling_offset: usize,
    recipe: Recipe,
    original_parent_id: usize,
    burst_recipes: Vec<Recipe>,
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
    burst: usize,
    sibling_offset: usize,
    recipe: Recipe,
    original_parent_id: usize,
    burst_recipes: Vec<Recipe>,
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
    PromoteB2,
    RetainB1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairOutcomeRecord {
    pair: usize,
    b1_maximum: SmbProgressWatermark,
    b2_maximum: SmbProgressWatermark,
    outcome: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StructuralWitness {
    pair: usize,
    b1_maximum: SmbProgressWatermark,
    champion: ChampionRecord,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct PairedClassificationRecord {
    record: &'static str,
    pairs: Vec<PairOutcomeRecord>,
    non_ties: usize,
    b2_wins: usize,
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
    b1_selections: usize,
    b2_selections: usize,
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
    authorizing_p183_preregistration: &'static str,
    authorizing_p183_implementation: &'static str,
    authorizing_p183_result: &'static str,
    authorizing_p183_report_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    source_p183_pair: u64,
    source_p183_arm: ArmKind,
    source_p183_entry_id: u64,
    source_p183_slot: u64,
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

/// Run the sealed paired B1/B2 sibling canary from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path =
        PathBuf::from(args.next().ok_or(
            "usage: smb-w8-2-p196-paired-b1-b2-sibling-canary <input.json> <output.jsonl>",
        )?);
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
        burst_size: BURST_SIZE,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        max_lineage_actions: MAX_LINEAGE_ACTIONS,
        selector: "concentrated_recency_b1_per_slot_b2_even_slot_per_burst_v1",
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
        authorizing_p183_preregistration: AUTHORIZING_P183_PREREGISTRATION,
        authorizing_p183_implementation: AUTHORIZING_P183_IMPLEMENTATION,
        authorizing_p183_result: AUTHORIZING_P183_RESULT,
        authorizing_p183_report_sha256: AUTHORIZING_P183_REPORT_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        source_p183_pair: 5,
        source_p183_arm: ArmKind::B1,
        source_p183_entry_id: 58,
        source_p183_slot: 105,
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
        b1_selections: work.b1_selections,
        b2_selections: work.b2_selections,
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
            b"p196-b1-b2-pair",
            &pair_u64.to_le_bytes(),
        ])?;
        let mut slots = Vec::with_capacity(SLOTS);
        for slot in 0..SLOTS {
            let slot_u64 = u64::try_from(slot)?;
            let action_word = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"p196-b1-b2-action",
                &slot_u64.to_le_bytes(),
            ])?;
            let source_index = usize::try_from(action_word % source_len)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived source index is out of bounds")?;
            let selector_seed = digest_word(&[
                &pair_seed.to_le_bytes(),
                b"p196-b1-b2-parent",
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
                .name(format!("paired-sibling-{worker}"))
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
            handle
                .join()
                .map_err(|_| "paired-sibling worker panicked")?;
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
        ArmKind::B1
    } else {
        ArmKind::B2
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
    let selections = if arm == ArmKind::B1 {
        SLOTS
    } else {
        SLOTS / BURST_SIZE
    };
    let mut selection_records = Vec::with_capacity(selections);
    let mut retained: Vec<Option<RetainedEvidence>> = vec![None];
    let mut action_total = 0_u64;
    let mut probe_total = 0_u64;
    let mut maximum_lineage_actions = SOURCE_ACTIONS;

    for burst in 0..selections {
        let selection_slot = if arm == ArmKind::B1 {
            burst
        } else {
            burst.checked_mul(BURST_SIZE).ok_or("burst slot overflow")?
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
        let original_parent_input_sha256 = sha256_json(&parent_report.input)?;
        let original_parent_snapshot_sha256 = sha256_json(&parent_snapshot)?;

        target.restore(&parent_snapshot)?;
        verify_snapshot(target, &parent_snapshot)?;
        let start = StartEvidence {
            observation: target.observe(),
            mechanical: smb_mechanical_state_from_wram(target.wram()),
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256: original_parent_snapshot_sha256.clone(),
            dead: target.is_dead(),
            failed: target.exit_kind() != ExitKind::Ok,
            milestones: parent_report.milestones,
        };
        if start.dead || start.failed {
            return Err("selector returned a terminal or failed parent".into());
        }

        let candidate_count = if arm == ArmKind::B1 { 1 } else { BURST_SIZE };
        let burst_recipes = recipes
            .get(
                selection_slot
                    ..selection_slot
                        .checked_add(candidate_count)
                        .ok_or("burst end overflow")?,
            )
            .ok_or("missing burst recipes")?
            .to_vec();
        let selection_before = target.frames_clocked();
        let mut candidates = Vec::with_capacity(candidate_count);
        let mut selection_productive = false;
        for (sibling_offset, recipe) in burst_recipes.iter().enumerate() {
            let slot = selection_slot
                .checked_add(sibling_offset)
                .ok_or("candidate slot overflow")?;
            if recipe.pair != pair || recipe.slot != slot {
                return Err("candidate recipe order is not canonical".into());
            }
            let action = recipe.action;
            target.restore(&parent_snapshot)?;
            verify_snapshot(target, &parent_snapshot)?;
            let candidate_start = StartEvidence {
                observation: target.observe(),
                mechanical: smb_mechanical_state_from_wram(target.wram()),
                wram_sha256: sha256_bytes(target.wram()),
                snapshot_sha256: original_parent_snapshot_sha256.clone(),
                dead: target.is_dead(),
                failed: target.exit_kind() != ExitKind::Ok,
                milestones: parent_report.milestones,
            };
            if candidate_start != start {
                return Err("sibling start differs from frozen selected parent".into());
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
                    let execution = if arm == ArmKind::B1 {
                        slot.checked_add(1).ok_or("execution overflow")?
                    } else {
                        burst.checked_add(1).ok_or("execution overflow")?
                    };
                    insert_candidate(
                        &mut archive,
                        Some(parent_id),
                        u64::try_from(execution)?,
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
                    burst,
                    sibling_offset,
                    recipe: *recipe,
                    original_parent_id: parent_id,
                    burst_recipes: burst_recipes.clone(),
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
            selection_productive |= productive;
            candidates.push(CandidateRecord {
                pair,
                arm,
                slot,
                burst,
                sibling_offset,
                recipe: *recipe,
                selector_used: sibling_offset == 0,
                original_parent_id: parent_id,
                original_parent_input_sha256: original_parent_input_sha256.clone(),
                original_parent_snapshot_sha256: original_parent_snapshot_sha256.clone(),
                start: candidate_start,
                input: candidate_input,
                endpoint,
                productive,
                active_ids: active_ids(&archive)?,
                active_maximum: active_maximum(&archive)?,
                total_work_frames: slot_work,
            });
        }
        let selection_work = target
            .frames_clocked()
            .checked_sub(selection_before)
            .ok_or("selection work counter moved backwards")?;
        let candidate_work = candidates.iter().try_fold(0_u64, |sum, candidate| {
            sum.checked_add(candidate.total_work_frames)
                .ok_or("candidate work overflow")
        })?;
        if selection_work != candidate_work || candidates.len() != candidate_count {
            return Err("selection work or candidate count does not reconcile".into());
        }
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, selection_productive, selection_work)?;
        selection_records.push(SelectionRecord {
            pair,
            arm,
            burst,
            selection_slot,
            selector_seed: selection_recipe.selector_seed,
            selector,
            original_parent_id: parent_id,
            original_parent_input_sha256,
            original_parent_snapshot_sha256,
            start,
            candidates,
            productive: selection_productive,
            selector_accounting: archive.selector_report(),
            total_work_frames: selection_work,
        });
    }

    let total_work_frames = action_total
        .checked_add(probe_total)
        .ok_or("arm work overflow")?;
    let arm_delta = target
        .frames_clocked()
        .checked_sub(arm_work_before)
        .ok_or("arm work counter moved backwards")?;
    if arm_delta != total_work_frames || selection_records.len() != selections {
        return Err("arm work or selection counts do not reconcile".into());
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
        selection_records,
        final_active_entries,
        final_maximum: active_maximum(&archive)?,
        maximum_lineage_actions,
        scheduled_slots: SLOTS,
        executed_slots: SLOTS,
        selections,
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
        let expected_execution = if arm == ArmKind::B1 {
            evidence.slot.checked_add(1).ok_or("execution overflow")?
        } else {
            evidence.burst.checked_add(1).ok_or("execution overflow")?
        };
        if evidence.endpoint.admission.newly_retained_id() != Some(id)
            || evidence.endpoint.dead
            || evidence.endpoint.failed
            || !evidence.endpoint.probe_survived
            || evidence.endpoint.key != Some(entry.report.key)
            || entry.report.parent_id != Some(u64::try_from(evidence.original_parent_id)?)
            || entry.report.created_execution != u64::try_from(expected_execution)?
            || evidence.burst_recipes.len() != if arm == ArmKind::B1 { 1 } else { BURST_SIZE }
            || evidence.burst_recipes.get(evidence.sibling_offset) != Some(&evidence.recipe)
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
            burst: evidence.burst,
            sibling_offset: evidence.sibling_offset,
            original_parent_id: evidence.original_parent_id,
        });
        candidates.push(ChampionCandidate {
            pair,
            arm,
            id,
            slot: evidence.slot,
            burst: evidence.burst,
            sibling_offset: evidence.sibling_offset,
            recipe: evidence.recipe,
            original_parent_id: evidence.original_parent_id,
            burst_recipes: evidence.burst_recipes.clone(),
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
            ArmKind::B1
        } else {
            ArmKind::B2
        };
        let expected_selections = if expected_arm == ArmKind::B1 {
            SLOTS
        } else {
            SLOTS / BURST_SIZE
        };
        let accounted_selections = selector_selections(record.selector_accounting)?;
        if record.ordinal != ordinal
            || record.pair != ordinal / 2
            || record.arm != expected_arm
            || record.worker != ordinal % WORKERS
            || record.worker_setup_frames != (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES)
            || record.selection_records.len() != expected_selections
            || record.selections != expected_selections
            || accounted_selections != u64::try_from(expected_selections)?
            || record.selector_accounting.policy != SmbArchiveSelectorPolicy::ConcentratedRecency
            || record.selector_accounting.waypoint_selections != 0
            || record.scheduled_slots != SLOTS
            || record.executed_slots != SLOTS
            || !(SOURCE_ACTIONS..=MAX_LINEAGE_ACTIONS).contains(&record.maximum_lineage_actions)
        {
            return Err("arm record order or shape is not canonical".into());
        }
        for (burst, selection) in record.selection_records.iter().enumerate() {
            let selection_slot = if expected_arm == ArmKind::B1 {
                burst
            } else {
                burst.checked_mul(BURST_SIZE).ok_or("burst overflow")?
            };
            let expected_candidates = if expected_arm == ArmKind::B1 {
                1
            } else {
                BURST_SIZE
            };
            let candidate_work =
                selection
                    .candidates
                    .iter()
                    .try_fold(0_u64, |sum, candidate| {
                        sum.checked_add(candidate.total_work_frames)
                            .ok_or("candidate work overflow")
                    })?;
            if selection.pair != record.pair
                || selection.arm != record.arm
                || selection.burst != burst
                || selection.selection_slot != selection_slot
                || selection.candidates.len() != expected_candidates
                || selection.selector_seed
                    != selection
                        .candidates
                        .first()
                        .ok_or("selection has no candidate")?
                        .recipe
                        .selector_seed
                || selection.candidates.iter().any(|candidate| {
                    candidate.original_parent_id != selection.original_parent_id
                        || candidate.original_parent_input_sha256
                            != selection.original_parent_input_sha256
                        || candidate.original_parent_snapshot_sha256
                            != selection.original_parent_snapshot_sha256
                        || candidate.start != selection.start
                })
                || candidate_work != selection.total_work_frames
                || selection.productive
                    != selection
                        .candidates
                        .iter()
                        .any(|candidate| candidate.productive)
                || selector_selections(selection.selector_accounting)?
                    != u64::try_from(burst.checked_add(1).ok_or("selection count overflow")?)?
            {
                return Err("selection record order or accounting is not canonical".into());
            }
            for (sibling_offset, candidate) in selection.candidates.iter().enumerate() {
                let slot = selection_slot
                    .checked_add(sibling_offset)
                    .ok_or("candidate slot overflow")?;
                let candidate_input_sha256 = sha256_json(&candidate.input)?;
                if candidate.pair != record.pair
                    || candidate.arm != record.arm
                    || candidate.slot != slot
                    || candidate.burst != burst
                    || candidate.sibling_offset != sibling_offset
                    || candidate.recipe.pair != record.pair
                    || candidate.recipe.slot != slot
                    || candidate.selector_used != (sibling_offset == 0)
                    || candidate.input.actions.last() != Some(&candidate.recipe.action)
                    || candidate.input.actions.len() != candidate.endpoint.input_actions
                    || candidate_input_sha256 != candidate.endpoint.input_sha256
                    || candidate.endpoint.action != candidate.recipe.action
                    || candidate.endpoint.admission.newly_retained_id().is_some()
                        != candidate.productive
                {
                    return Err("candidate record order or identity is not canonical".into());
                }
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
        burst: candidate.burst,
        sibling_offset: candidate.sibling_offset,
        recipe: candidate.recipe,
        original_parent_id: candidate.original_parent_id,
        burst_recipes: candidate.burst_recipes.clone(),
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
    let mut b2_wins = 0_usize;
    let mut witnesses = Vec::new();
    for pair in 0..PAIRS {
        let b1 = arms.get(pair * 2).ok_or("missing B1 arm")?;
        let b2 = arms
            .get(
                pair.checked_mul(2)
                    .and_then(|value| value.checked_add(1))
                    .ok_or("arm index overflow")?,
            )
            .ok_or("missing B2 arm")?;
        let outcome = match b2.final_maximum.watermark.cmp(&b1.final_maximum.watermark) {
            std::cmp::Ordering::Greater => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                b2_wins = b2_wins.checked_add(1).ok_or("B2 win count overflow")?;
                "B2_WIN"
            }
            std::cmp::Ordering::Less => {
                non_ties = non_ties.checked_add(1).ok_or("non-tie count overflow")?;
                "B1_WIN"
            }
            std::cmp::Ordering::Equal => "TIE",
        };
        pairs.push(PairOutcomeRecord {
            pair,
            b1_maximum: b1.final_maximum.watermark,
            b2_maximum: b2.final_maximum.watermark,
            outcome,
        });
        witnesses.extend(structural_witnesses(pair, b1, b2));
    }
    witnesses.sort_by_key(|witness| (witness.pair, witness.champion.id));
    let tail_numerator = sign_tail_numerator(non_ties, b2_wins)?;
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
        b2_wins,
        tail_numerator,
        tail_denominator,
        witnesses,
        verdict,
    })
}

fn structural_witnesses(pair: usize, b1: &ArmRecord, b2: &ArmRecord) -> Vec<StructuralWitness> {
    b2.champion_candidates
        .iter()
        .filter(|candidate| is_b2_witness(candidate, b1.final_maximum.watermark))
        .map(|candidate| StructuralWitness {
            pair,
            b1_maximum: b1.final_maximum.watermark,
            champion: champion_record(candidate),
        })
        .collect()
}

fn is_b2_witness(candidate: &ChampionCandidate, b1_maximum: SmbProgressWatermark) -> bool {
    candidate.arm == ArmKind::B2
        && candidate.sibling_offset == 1
        && candidate.burst_recipes.len() == BURST_SIZE
        && candidate.endpoint.admission.newly_retained_id() == Some(candidate.id)
        && !candidate.endpoint.dead
        && !candidate.endpoint.failed
        && candidate.endpoint.probe_survived
        && candidate.endpoint.watermark > BASELINE_WATERMARK
        && candidate.endpoint.watermark > b1_maximum
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
        StructuralVerdict::PromoteB2
    } else {
        StructuralVerdict::RetainB1
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
    b1_selections: usize,
    b2_selections: usize,
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
    let mut b1_selections = 0_usize;
    let mut b2_selections = 0_usize;
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
        let arm_selections = match record.arm {
            ArmKind::B1 => &mut b1_selections,
            ArmKind::B2 => &mut b2_selections,
        };
        *arm_selections = arm_selections
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
        || b1_selections != EXPECTED_B1_SELECTIONS
        || b2_selections != EXPECTED_B2_SELECTIONS
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
        b1_selections,
        b2_selections,
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
            burst: if arm == ArmKind::B1 {
                slot
            } else {
                slot / BURST_SIZE
            },
            sibling_offset: if arm == ArmKind::B1 {
                0
            } else {
                slot % BURST_SIZE
            },
            recipe: Recipe {
                pair,
                slot,
                source_index: slot,
                action: ButtonChord::new(0, 2),
                selector_seed: u64::try_from(slot).expect("slot fits u64"),
            },
            original_parent_id: 0,
            burst_recipes: if arm == ArmKind::B1 {
                vec![Recipe {
                    pair,
                    slot,
                    source_index: slot,
                    action: ButtonChord::new(0, 2),
                    selector_seed: u64::try_from(slot).expect("slot fits u64"),
                }]
            } else {
                let first = slot - slot % BURST_SIZE;
                (first..first + BURST_SIZE)
                    .map(|recipe_slot| Recipe {
                        pair,
                        slot: recipe_slot,
                        source_index: recipe_slot,
                        action: ButtonChord::new(0, 2),
                        selector_seed: u64::try_from(recipe_slot).expect("slot fits u64"),
                    })
                    .collect()
            },
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

    fn synthetic_selection(pair: usize, arm: ArmKind, burst: usize) -> SelectionRecord {
        let start = start_evidence(BASELINE_WATERMARK);
        let selection_slot = if arm == ArmKind::B1 {
            burst
        } else {
            burst * BURST_SIZE
        };
        let candidate_count = if arm == ArmKind::B1 { 1 } else { BURST_SIZE };
        let candidates = (0..candidate_count)
            .map(|sibling_offset| {
                let slot = selection_slot + sibling_offset;
                let recipe = Recipe {
                    pair,
                    slot,
                    source_index: slot,
                    action: ButtonChord::new(0, 2),
                    selector_seed: u64::try_from(slot).expect("slot fits u64"),
                };
                let mut endpoint =
                    candidate(pair, arm, slot + 1, BASELINE_WATERMARK, 1, 0, slot).endpoint;
                let input = SmbInput {
                    actions: vec![recipe.action],
                };
                endpoint.input_actions = input.actions.len();
                endpoint.input_sha256 = sha256_json(&input).expect("hash synthetic input");
                endpoint.admission = AdmissionOutcome::Rejected;
                CandidateRecord {
                    pair,
                    arm,
                    slot,
                    burst,
                    sibling_offset,
                    recipe,
                    selector_used: sibling_offset == 0,
                    original_parent_id: 0,
                    original_parent_input_sha256: String::new(),
                    original_parent_snapshot_sha256: String::new(),
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
                }
            })
            .collect::<Vec<_>>();
        SelectionRecord {
            pair,
            arm,
            burst,
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
            original_parent_input_sha256: String::new(),
            original_parent_snapshot_sha256: String::new(),
            start,
            candidates,
            productive: false,
            selector_accounting: selector_accounting(burst + 1),
            total_work_frames: u64::try_from(candidate_count * 2).expect("work fits u64"),
        }
    }

    fn synthetic_arm(
        pair: usize,
        arm: ArmKind,
        maximum: SmbProgressWatermark,
        champion_candidates: Vec<ChampionCandidate>,
    ) -> ArmRecord {
        let ordinal = pair * 2 + usize::from(arm == ArmKind::B2);
        let selections = if arm == ArmKind::B1 {
            SLOTS
        } else {
            SLOTS / BURST_SIZE
        };
        let selection_records = (0..selections)
            .map(|burst| synthetic_selection(pair, arm, burst))
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
            selection_records,
            final_active_entries: Vec::new(),
            final_maximum: ActiveMaximum {
                watermark: maximum,
                ids: vec![0],
            },
            maximum_lineage_actions: SOURCE_ACTIONS,
            scheduled_slots: SLOTS,
            executed_slots: SLOTS,
            selections,
            selector_accounting: selector_accounting(selections),
            action_frames,
            probe_frames: 0,
            total_work_frames: action_frames,
            champion_candidates,
        }
    }

    fn paired_arms_with_boundary_candidates() -> Vec<ArmRecord> {
        let b1_maximum = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 197,
        };
        let mut arms = Vec::with_capacity(ARMS);
        for pair in 0..PAIRS {
            arms.push(synthetic_arm(pair, ArmKind::B1, b1_maximum, Vec::new()));
            let b2_maximum = SmbProgressWatermark {
                progress: 198,
                ..b1_maximum
            };
            let candidate = candidate(
                pair,
                ArmKind::B2,
                1,
                b2_maximum,
                SOURCE_ACTIONS + 1,
                u8::try_from(pair).expect("pair fits u8"),
                1,
            );
            arms.push(synthetic_arm(
                pair,
                ArmKind::B2,
                b2_maximum,
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
                source_index: 1_382,
                action: ButtonChord::new(102, 75),
                selector_seed: 11_257_089_157_767_927_824,
            }
        );
        assert_eq!(
            recipes[15][127],
            Recipe {
                pair: 15,
                slot: 127,
                source_index: 2_036,
                action: ButtonChord::new(244, 15),
                selector_seed: 18_108_817_186_768_306_067,
            }
        );
        assert_eq!(
            recipe_sha256(&recipes).expect("hash synthetic recipe"),
            "e077671400610fb087c9156c18f6ea58cbd27176c815ccfa1ef67f3de55a5424"
        );
        let mut projections = projection_bytes(&recipes).expect("serialize projections");
        assert_eq!(projections.len(), PAIRS);
        projections.sort();
        assert!(projections.windows(2).all(|window| window[0] != window[1]));
        assert!(projection_sha256(&recipes).is_err());
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "806e5c4d3200d983c7043aded109bd516463e543bba66c2867b24dd3e0484131"
        );
        assert_eq!(EXPECTED_RECIPE_BYTES, 132_496);
    }

    #[test]
    fn paired_sign_gate_requires_exact_tail_and_second_sibling_witness() {
        assert_eq!(sign_tail_numerator(16, 16).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 8).expect("tail"), 1);
        assert_eq!(sign_tail_numerator(8, 7).expect("tail"), 9);
        let arms = paired_arms_with_boundary_candidates();
        let classified = classify_paired(&arms).expect("classify paired arms");
        assert_eq!(classified.non_ties, 16);
        assert_eq!(classified.b2_wins, 16);
        assert!(
            classified
                .pairs
                .iter()
                .all(|outcome| outcome.outcome == "B2_WIN")
        );
        assert_eq!(
            (classified.tail_numerator, classified.tail_denominator),
            (1, 65_536)
        );
        assert_eq!(classified.verdict, StructuralVerdict::PromoteB2);
        assert_eq!(classified.witnesses.len(), PAIRS);
        assert_eq!(
            (
                classified.witnesses[0].pair,
                classified.witnesses[0].champion.id,
                classified.witnesses[0].champion.slot,
                classified.witnesses[0].champion.endpoint.watermark.progress,
            ),
            (0, 1, 1, 198)
        );
        let work = summarize_work(&arms, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES)
            .expect("reconcile paired work");
        assert_eq!(
            work.worker_setup_frames,
            vec![EXPECTED_SETUP_FRAMES; WORKERS]
        );
        assert_eq!(work.b1_selections, EXPECTED_B1_SELECTIONS);
        assert_eq!(work.b2_selections, EXPECTED_B2_SELECTIONS);
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
        assert_eq!(classified.verdict, StructuralVerdict::RetainB1);

        let mut sparse = paired_arms_with_boundary_candidates();
        for pair in 7..PAIRS {
            let b1_maximum = sparse[pair * 2].final_maximum.watermark;
            let b2 = &mut sparse[pair * 2 + 1];
            b2.final_maximum.watermark = b1_maximum;
            b2.champion_candidates.clear();
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
            serde_json::to_string(&StructuralVerdict::PromoteB2)
                .expect("serialize promotion verdict"),
            r#""PROMOTE_B2""#
        );
        assert_eq!(
            serde_json::to_string(&StructuralVerdict::RetainB1)
                .expect("serialize rejection verdict"),
            r#""RETAIN_B1""#
        );
        assert_eq!(
            serde_json::to_string(&ArmKind::B1).expect("serialize B1 arm"),
            r#""B1""#
        );
        assert_eq!(
            serde_json::to_string(&ArmKind::B2).expect("serialize B2 arm"),
            r#""B2""#
        );
    }

    #[test]
    fn structural_sign_boundary_is_exact_and_sparse_takes_precedence() {
        assert_eq!(
            structural_verdict(8, 9, 256, true).expect("classify 9/256 tail"),
            StructuralVerdict::RetainB1
        );
        assert_eq!(
            structural_verdict(8, 1, 256, true).expect("classify 1/256 tail"),
            StructuralVerdict::PromoteB2
        );
        assert_eq!(
            structural_verdict(7, 1, 128, true).expect("classify sparse tail"),
            StructuralVerdict::InconclusiveSparse
        );
    }

    #[test]
    fn b2_records_one_aggregate_selection_for_two_same_parent_siblings() {
        let mut arms = paired_arms_with_boundary_candidates();
        let selection = &arms[1].selection_records[0];
        assert_eq!(arms[0].selection_records.len(), SLOTS);
        assert_eq!(arms[1].selection_records.len(), SLOTS / BURST_SIZE);
        assert_eq!(selection.candidates.len(), BURST_SIZE);
        assert_eq!(
            selection
                .candidates
                .iter()
                .map(|candidate| candidate.original_parent_id)
                .collect::<Vec<_>>(),
            vec![selection.original_parent_id; BURST_SIZE]
        );
        assert!(selection.candidates[0].selector_used);
        assert!(!selection.candidates[1].selector_used);
        assert_eq!(selection.candidates[0].start, selection.candidates[1].start);
        assert_eq!(
            selector_selections(selection.selector_accounting).expect("count"),
            1
        );

        arms[1].selection_records[0].candidates[1].original_parent_id = 1;
        assert!(validate_arms(&arms).is_err());
        arms[1].selection_records[0].candidates[1].original_parent_id = 0;
        arms[1].selection_records[0].candidates[1].start.wram_sha256 = "different".to_owned();
        assert!(validate_arms(&arms).is_err());
        arms[1].selection_records[0].candidates[1]
            .start
            .wram_sha256
            .clear();
        arms[1].selection_records[0].productive = true;
        assert!(validate_arms(&arms).is_err());
    }

    #[test]
    fn b2_witness_requires_second_sibling_and_strict_progress() {
        let b1_maximum = SmbProgressWatermark {
            progress: 197,
            ..BASELINE_WATERMARK
        };
        let strict = SmbProgressWatermark {
            progress: 198,
            ..BASELINE_WATERMARK
        };
        let mut witness = candidate(0, ArmKind::B2, 1, strict, SOURCE_ACTIONS + 1, 1, 1);
        assert!(is_b2_witness(&witness, b1_maximum));

        witness.sibling_offset = 0;
        assert!(!is_b2_witness(&witness, b1_maximum));
        witness.sibling_offset = 1;
        witness.endpoint.watermark = b1_maximum;
        assert!(!is_b2_witness(&witness, b1_maximum));
        witness.endpoint.watermark = BASELINE_WATERMARK;
        assert!(!is_b2_witness(&witness, BASELINE_WATERMARK));
        witness.endpoint.watermark = strict;
        witness.endpoint.probe_survived = false;
        assert!(!is_b2_witness(&witness, b1_maximum));
        witness.endpoint.probe_survived = true;
        witness.endpoint.admission = AdmissionOutcome::Duplicate { id: witness.id };
        assert!(!is_b2_witness(&witness, b1_maximum));
        witness.endpoint.admission = AdmissionOutcome::Retained {
            id: witness.id,
            displaced: false,
        };
        witness.burst_recipes.truncate(1);
        assert!(!is_b2_witness(&witness, b1_maximum));
    }

    #[test]
    fn champion_ranking_uses_full_watermark_then_registered_ties() {
        let base = SmbProgressWatermark {
            world: 7,
            level: 1,
            progress: 197,
        };
        let champion = rank_champion(vec![
            candidate(0, ArmKind::B1, 9, base, 9, 0x10, 1),
            candidate(1, ArmKind::B2, 8, base, 8, 0x20, 5),
            candidate(1, ArmKind::B1, 7, base, 8, 0x20, 1),
            candidate(0, ArmKind::B2, 6, base, 8, 0x20, 3),
            candidate(0, ArmKind::B1, 5, base, 8, 0x20, 1),
        ])
        .expect("champion exists");
        assert_eq!(
            (champion.pair, champion.arm, champion.id),
            (0, ArmKind::B1, 5)
        );
        assert_eq!(verdict_for(Some(&champion)), Verdict::Adopt);

        let later_level = SmbProgressWatermark {
            world: 7,
            level: 2,
            progress: 0,
        };
        let cross_level = rank_champion(vec![
            candidate(0, ArmKind::B1, 1, base, 1, 0, 1),
            candidate(7, ArmKind::B2, 2, later_level, 20, 0xff, 5),
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
