// SPDX-License-Identifier: AGPL-3.0-or-later

//! Temporary sealed runner for the World 8-4 p73 normal-endpoint harvest.

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

const FORMAT: &str = "smb-w8-4-p73-normal-endpoint-harvest-v4";
const PREREGISTRATION_COMMIT: &str = "fbf2afb115f2692ce522b56dea460b065225dc67";
const PREREGISTRATION_DOC_SHA256: &str =
    "8d3b10c32fbb13afe1f1402ae70bc0f591ccc88602a97c53d04c69b55643692c";
const CODE_BASE: &str = "9c2b9fe634990a79112c47176049577dc436838c";
const AUTHORIZING_P0_PREREGISTRATION: &str = "4f0e7549";
const AUTHORIZING_P0_IMPLEMENTATION: &str = "597ea67f";
const AUTHORIZING_P0_RESULT: &str = "6bd11649";
const AUTHORIZING_P0_REPORT_SHA256: &str =
    "255f9b430841303a4e5d9c9d6eb9820c1887ba9c7b3c3f5192d53c2c1eb87e59";
const AUTHORIZING_P61_PREREGISTRATION: &str = "3aaeb783";
const AUTHORIZING_P61_IMPLEMENTATION: &str = "9de5e622";
const AUTHORIZING_P61_RESULT: &str = "9c2b9fe6";
const AUTHORIZING_P61_REPORT_SHA256: &str =
    "6fba3d7d27bd0ab85bc0dc832f4246f7512d3d68d829eb9a48bb971857bec0bb";
const FAILED_V2_PREREGISTRATION: &str = "97b9f4be";
const FAILED_V2_IMPLEMENTATION: &str = "f5bd5c38";
const FAILED_V2_RESULT: &str = "44978d25";
const FAILED_V2_EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const FAILED_V2_STDERR_SHA256: &str =
    "d1d590e5df74a7cbf24970f98b8d5e34f49970adbe399e83a3db1c3308b9241e";
const SOURCE_FILE_SHA256: &str = "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_INPUT_SHA256: &str =
    "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_BYTES: usize = 114_128;
const SOURCE_WRAM_SHA256: &str = "bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734";
const SOURCE_SNAPSHOT_SHA256: &str =
    "3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const SEED_LABEL: &str = "sol-restart-w8-4-p73-normal-endpoint-harvest-v4";
const SEED_LABEL_SHA256: &str = "e90f4c5c70466fd96979600936a65526afce96e4943a3107af5582f2c8cd0ef4";
const MASTER_SEED: u64 = 15_667_819_077_044_015_081;
const EXPECTED_RECIPE_SHA256: &str =
    "2b3fa68177b87af9dc7231f77bcd46b492bc836b71b67a51b25a93af99157954";
const EXPECTED_RECIPE_BYTES: usize = 400_078;
const EXPECTED_PROJECTION_SHA256: [&str; 12] = [
    "6e03f7490150e6d4458f946dd719a6f7b011254ef57a6055d1c1046a78cecaeb",
    "8e4b30861747c9945067681d814511e7f41d32d73cd8199b82c71158a83bfcd7",
    "7eb3b9119264434853fc5c36bd598bf7597a8c54183e89c0f65d5ce1b1f8e3a3",
    "b3506de7dd29ef89deb1b88a4b3aa14d9828b77670d175dadce79f7cf1eedaae",
    "0e58b2dd39826bef4c2584b1b48e9875bf3e71245490b4e2b7b28f8e81a75a6b",
    "c13c8896d0cddf84801cc98689a197e406a3e60da5abc8653f868985a8012468",
    "9adadaae0e1705d78b296d750f161772567bad93eeb313d7a2258eb0c90dd557",
    "3f11494cb1344523e9a43218ddcc464e2948c496bf8e3b7d7b983968eb42f974",
    "595869a0a4e6e24453d2359d51a20947177dae056c33b3e0654793d7d0ada6a9",
    "697fae79ffa0cf95b41eb994a8d1e18bf558ee83847c536a8836197d8a600ada",
    "212780d3e37400269ac72624ed5ed66ead4456ca3dc4f442073049fff4790710",
    "bbf710f5fa1b9243239b4bcc9023f901af277c479966c937d0734d7c4f752499",
];
const EXPECTED_PROJECTION_BYTES: [usize; 12] = [
    32_233, 32_215, 32_178, 32_246, 32_237, 32_265, 32_206, 32_236, 32_269, 32_257, 32_228, 32_207,
];
const SOURCE_ACTIONS: usize = 3_554;
const SOURCE_FRAMES: u64 = 167_340;
const LANES: usize = 12;
const DRAWS: usize = 512;
const ACTION_LIMIT: usize = 4_096;
const ARCHIVE_LIMIT: usize = 513;
const MAX_LINEAGE_ACTIONS: usize = 4_066;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 737_280;
const MAX_PROBE_FRAMES: u64 = 829_440;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 1_738_798;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const PROBE_FRAMES: u16 = 45;
const SOURCE_PROBE_TRANSCRIPT: [(u8, u64, bool, bool); 1] = [(0x00, 45, false, true)];
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 3,
    progress: 73,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 3,
    progress: 73,
    player_y_bucket: 8,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 3,
    progress: 73,
    player_y_bucket: 8,
    player_engine_state: 8,
    state_fingerprint: 60,
    room_x_bucket: 0,
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 0,
    hold_frames: 3,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    lane: usize,
    draw: usize,
    source_index: usize,
    action: ButtonChord,
    selector_seed: u64,
}

#[derive(Debug, Serialize)]
struct Config {
    lanes: usize,
    draws_per_lane: usize,
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
struct DrawRecord {
    draw: usize,
    recipe: Recipe,
    source_index: usize,
    selector_seed: u64,
    selector: SmbSelectorDraw,
    parent_id: usize,
    parent_input_sha256: String,
    parent_snapshot_sha256: String,
    start: StartEvidence,
    input: SmbInput,
    endpoint: EndpointEvidence,
    productive: bool,
    active_ids: Vec<usize>,
    active_maximum: ActiveMaximum,
    selector_accounting: SmbSelectorAccounting,
    total_work_frames: u64,
}

#[derive(Clone, Debug)]
struct RetainedEvidence {
    draw: usize,
    recipe: Recipe,
    parent_id: usize,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FinalEntryRecord {
    id: usize,
    parent_id: Option<u64>,
    created_execution: u64,
    draw: usize,
    recipe: Recipe,
    actions: usize,
    input_sha256: String,
    key: SmbArchiveKey,
    watermark: SmbProgressWatermark,
    milestones: SmbMilestones,
    snapshot_sha256: String,
    probe_survived: bool,
    work_frames: u64,
}

#[derive(Clone, Debug, Serialize)]
struct LaneRecord {
    record: &'static str,
    lane: usize,
    worker: usize,
    setup_frames: u64,
    initial_archive_sha256: String,
    draws: Vec<DrawRecord>,
    final_active_entries: Vec<FinalEntryRecord>,
    final_maximum: ActiveMaximum,
    maximum_lineage_actions: usize,
    scheduled_draws: usize,
    executed_draws: usize,
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
    lane: usize,
    id: usize,
    draw: usize,
    recipe: Recipe,
    parent_id: usize,
    input: SmbInput,
    input_sha256: String,
    input_sha256_bytes: [u8; 32],
    parent_lineage: Vec<u64>,
    endpoint: EndpointEvidence,
    work_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ChampionRecord {
    lane: usize,
    id: usize,
    draw: usize,
    recipe: Recipe,
    parent_id: usize,
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

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    verdict: Verdict,
    champion: Option<ChampionRecord>,
    lane_setup_frames: Vec<u64>,
    scheduled_candidates: usize,
    executed_candidates: usize,
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
    authorizing_p0_preregistration: &'static str,
    authorizing_p0_implementation: &'static str,
    authorizing_p0_result: &'static str,
    authorizing_p0_report_sha256: &'static str,
    authorizing_p61_preregistration: &'static str,
    authorizing_p61_implementation: &'static str,
    authorizing_p61_result: &'static str,
    authorizing_p61_report_sha256: &'static str,
    failed_v2_preregistration: &'static str,
    failed_v2_implementation: &'static str,
    failed_v2_result: &'static str,
    failed_v2_empty_sha256: &'static str,
    failed_v2_stderr_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    recipe_bytes: usize,
    recipe_sha256: &'a str,
    projection_bytes: &'static [usize; LANES],
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
struct LaneReply {
    lane: usize,
    worker: usize,
    result: Result<LaneRecord, String>,
}

/// Run the sealed World 8-4 p73 endpoint harvest from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-w8-4-p73-normal-endpoint-harvest-v4 <input.json> <output.jsonl>")?,
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
        lanes: LANES,
        draws_per_lane: DRAWS,
        workers: LANES,
        action_limit: ACTION_LIMIT,
        archive_limit: ARCHIVE_LIMIT,
        max_lineage_actions: MAX_LINEAGE_ACTIONS,
        selector: "concentrated_recency_fresh_seed_per_draw_v1",
        retention: "probe_at_admission_45",
        replacement: "fewest_actions",
        key: "frozen",
        waypoint: "absent",
        snapback: "absent",
        pinned_window: "absent",
        empirical_chord_update: "absent",
        assignment: "one_lane_per_persistent_worker_buffered_ascending_v1",
        probe_masks: PROBE_MASKS,
        probe_frames: PROBE_FRAMES,
        source_probe_masks: [0x00],
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
    let lanes = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let classification = classify(&lanes)?;
    let work = summarize_work(
        &lanes,
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
        authorizing_p0_preregistration: AUTHORIZING_P0_PREREGISTRATION,
        authorizing_p0_implementation: AUTHORIZING_P0_IMPLEMENTATION,
        authorizing_p0_result: AUTHORIZING_P0_RESULT,
        authorizing_p0_report_sha256: AUTHORIZING_P0_REPORT_SHA256,
        authorizing_p61_preregistration: AUTHORIZING_P61_PREREGISTRATION,
        authorizing_p61_implementation: AUTHORIZING_P61_IMPLEMENTATION,
        authorizing_p61_result: AUTHORIZING_P61_RESULT,
        authorizing_p61_report_sha256: AUTHORIZING_P61_REPORT_SHA256,
        failed_v2_preregistration: FAILED_V2_PREREGISTRATION,
        failed_v2_implementation: FAILED_V2_IMPLEMENTATION,
        failed_v2_result: FAILED_V2_RESULT,
        failed_v2_empty_sha256: FAILED_V2_EMPTY_SHA256,
        failed_v2_stderr_sha256: FAILED_V2_STDERR_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
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
        projection_bytes: &'static [usize; LANES],
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
    for lane in &lanes {
        output.write(lane)?;
    }
    output.write(&classification)?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        verdict: classification.verdict,
        champion: classification.champion.clone(),
        lane_setup_frames: lanes.iter().map(|lane| lane.setup_frames).collect(),
        scheduled_candidates: work.scheduled,
        executed_candidates: work.executed,
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
        "{{\"report_sha256\":\"{report_sha256}\",\"verdict\":{}}}",
        serde_json::to_string(&summary.verdict)?
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
    let mut lanes = Vec::with_capacity(LANES);
    for lane in 0..LANES {
        let lane_u64 = u64::try_from(lane)?;
        let lane_seed = digest_word(&[
            &MASTER_SEED.to_le_bytes(),
            b"w8-4-p73-v4-lane",
            &lane_u64.to_le_bytes(),
        ])?;
        let mut draws = Vec::with_capacity(DRAWS);
        for draw in 0..DRAWS {
            let draw_u64 = u64::try_from(draw)?;
            let source_word = digest_word(&[
                &lane_seed.to_le_bytes(),
                b"w8-4-p73-v4-action",
                &draw_u64.to_le_bytes(),
            ])?;
            let source_index = usize::try_from(source_word % source_len)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived source index is out of bounds")?;
            let selector_seed = digest_word(&[
                &lane_seed.to_le_bytes(),
                b"w8-4-p73-v4-parent",
                &draw_u64.to_le_bytes(),
            ])?;
            draws.push(Recipe {
                lane,
                draw,
                source_index,
                action,
                selector_seed,
            });
        }
        lanes.push(draws);
    }
    Ok(lanes)
}

fn recipe_identity_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<u8>, Box<dyn Error>> {
    let identity = recipes
        .iter()
        .flat_map(|lane| lane.iter())
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.lane)?,
                u64::try_from(recipe.draw)?,
                u64::try_from(recipe.source_index)?,
                recipe.action,
                recipe.selector_seed,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(serde_json::to_vec(&identity)?)
}

fn projection_bytes(recipes: &[Vec<Recipe>]) -> Result<Vec<Vec<u8>>, Box<dyn Error>> {
    if recipes.len() != LANES || recipes.iter().any(|lane| lane.len() != DRAWS) {
        return Err("recipe projection shape does not match the preregistration".into());
    }
    recipes
        .iter()
        .map(|lane| {
            let projection = lane
                .iter()
                .map(|recipe| {
                    Ok((
                        u64::try_from(recipe.draw)?,
                        u64::try_from(recipe.source_index)?,
                        recipe.action,
                        recipe.selector_seed,
                    ))
                })
                .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
            Ok(serde_json::to_vec(&projection)?)
        })
        .collect()
}

fn projection_sha256(recipes: &[Vec<Recipe>]) -> Result<Vec<String>, Box<dyn Error>> {
    let projections = projection_bytes(recipes)?;
    let lengths = projections.iter().map(Vec::len).collect::<Vec<_>>();
    if lengths.as_slice() != EXPECTED_PROJECTION_BYTES {
        return Err("lane recipe projection byte lengths do not match the sealed oracle".into());
    }
    let mut ordered = projections.clone();
    ordered.sort();
    if ordered.windows(2).any(|window| window[0] == window[1]) {
        return Err("lane recipe projections are not pairwise distinct".into());
    }
    let hashes = projections
        .iter()
        .map(|projection| sha256_bytes(projection))
        .collect::<Vec<_>>();
    if hashes
        .iter()
        .map(String::as_str)
        .ne(EXPECTED_PROJECTION_SHA256)
    {
        return Err("lane recipe projection hashes do not match the sealed oracle".into());
    }
    Ok(hashes)
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
) -> Result<Vec<LaneRecord>, Box<dyn Error>> {
    if recipes.len() != LANES || recipes.iter().any(|lane| lane.len() != DRAWS) {
        return Err("recipe shape does not match the preregistration".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let mut handles = Vec::with_capacity(LANES);
        for lane in 0..LANES {
            let sender = sender.clone();
            let source = source.clone();
            let lane_recipes = recipes.get(lane).ok_or("missing lane recipes")?.clone();
            let baseline = baseline.clone();
            let handle = thread::Builder::new()
                .name(format!("endpoint-harvest-{lane}"))
                .spawn_scoped(scope, move || {
                    let result = SmbTarget::from_smb_rom_bytes_headless(rom)
                        .map_err(|error| error.to_string())
                        .and_then(|mut target| {
                            run_lane(&mut target, &source, &lane_recipes, &baseline, lane)
                                .map_err(|error| error.to_string())
                        });
                    let _ = sender.send(LaneReply {
                        lane,
                        worker: lane,
                        result,
                    });
                })?;
            handles.push(handle);
        }
        drop(sender);
        for handle in handles {
            handle
                .join()
                .map_err(|_| "endpoint-harvest worker panicked")?;
        }
        Ok(())
    })?;
    consume_lane_replies(receiver.into_iter().collect())
}

fn consume_lane_replies(replies: Vec<LaneReply>) -> Result<Vec<LaneRecord>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    let mut metadata_errors = Vec::new();
    for reply in replies {
        if reply.lane >= LANES || reply.worker != reply.lane {
            metadata_errors.push((0_u8, reply.lane, reply.worker, "invalid"));
            continue;
        }
        if buffered.insert(reply.lane, reply.result).is_some() {
            metadata_errors.push((1_u8, reply.lane, reply.worker, "duplicate"));
        }
    }
    for lane in 0..LANES {
        if !buffered.contains_key(&lane) {
            metadata_errors.push((2_u8, lane, lane, "missing"));
        }
    }
    metadata_errors.sort_unstable();
    if let Some((_, lane, worker, kind)) = metadata_errors.first() {
        return Err(format!("{kind} lane reply: lane={lane}, worker={worker}").into());
    }
    let mut lanes = Vec::with_capacity(LANES);
    for lane in 0..LANES {
        lanes.push(
            buffered
                .remove(&lane)
                .ok_or("missing lane reply")?
                .map_err(|error| format!("lane {lane}: {error}"))?,
        );
    }
    Ok(lanes)
}

fn run_lane(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipes: &[Recipe],
    baseline: &Baseline,
    lane: usize,
) -> Result<LaneRecord, Box<dyn Error>> {
    let setup_frames = target.frames_clocked();
    if setup_frames != EXPECTED_SETUP_FRAMES {
        return Err("worker target setup work does not match the sealed value".into());
    }
    if recipes.len() != DRAWS {
        return Err("lane recipe count does not match the preregistration".into());
    }
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
        return Err("lane origin archive did not initialize exactly".into());
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
    let lane_work_before = target.frames_clocked();
    let mut draws = Vec::with_capacity(DRAWS);
    let mut retained: Vec<Option<RetainedEvidence>> = vec![None];
    let mut action_total = 0_u64;
    let mut probe_total = 0_u64;
    let mut maximum_lineage_actions = SOURCE_ACTIONS;

    for recipe in recipes {
        if recipe.lane != lane || recipe.draw != draws.len() {
            return Err("lane recipe order is not canonical".into());
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
        let draw_before = target.frames_clocked();
        let action_before = target.frames_clocked();
        target.apply(&recipe.action);
        let action_frames = target
            .frames_clocked()
            .checked_sub(action_before)
            .ok_or("action work counter moved backwards")?;
        let failed = target.exit_kind() != ExitKind::Ok;
        if failed {
            return Err("emulator failed during a full action".into());
        }
        let dead = target.is_dead();
        if action_frames > u64::from(recipe.action.bounded_hold_frames()) {
            return Err("full action exceeded its bounded duration".into());
        }
        if !dead && action_frames != u64::from(recipe.action.bounded_hold_frames()) {
            return Err("live full action did not execute its requested duration".into());
        }
        let observation = target.observe();
        let mechanical = smb_mechanical_state_from_wram(target.wram());
        let mut milestones = parent_report.milestones;
        merge_action_milestones(&mut milestones, target)?;
        let input = appended_input(&parent_report.input, recipe.action)?;
        record_lineage_actions(&mut maximum_lineage_actions, input.actions.len())?;
        let input_sha256 = sha256_json(&input)?;
        let wram_sha256 = sha256_bytes(target.wram());

        let mut snapshot_sha256 = None;
        let mut key = None;
        let mut probe = Vec::new();
        let mut probe_survived = false;
        let mut probe_frames = 0_u64;
        let admission;
        let mut retained_snapshot = None;
        if dead {
            admission = AdmissionOutcome::Terminal;
        } else {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot ordinary endpoint")?;
            let candidate_snapshot_sha256 = sha256_json(&snapshot)?;
            let candidate_key = archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen);
            let (attempts, survived, work) = run_probe(target, &snapshot)?;
            probe = attempts;
            probe_survived = survived;
            probe_frames = work;
            snapshot_sha256 = Some(candidate_snapshot_sha256.clone());
            key = Some(candidate_key);
            admission = if survived {
                let outcome = insert_candidate(
                    &mut archive,
                    Some(parent_id),
                    u64::try_from(recipe.draw.checked_add(1).ok_or("execution overflow")?)?,
                    ArchiveCandidate {
                        input: input.clone(),
                        key: candidate_key,
                        milestones,
                    },
                    snapshot.clone(),
                )?;
                if outcome.newly_retained_id().is_some() {
                    retained_snapshot = Some(snapshot);
                }
                outcome
            } else {
                AdmissionOutcome::ProbeRefused
            };
        }
        let endpoint = EndpointEvidence {
            action: recipe.action,
            input_actions: input.actions.len(),
            input_sha256: input_sha256.clone(),
            observation,
            mechanical,
            watermark: watermark(mechanical),
            wram_sha256,
            snapshot_sha256,
            key,
            milestones,
            action_frames,
            dead,
            failed,
            probe,
            probe_survived,
            probe_frames,
            admission,
        };
        if let Some(id) = endpoint.admission.newly_retained_id() {
            if id != retained.len() || retained_snapshot.is_none() {
                return Err("retained evidence is not insertion-order aligned".into());
            }
            retained.push(Some(RetainedEvidence {
                draw: recipe.draw,
                recipe: *recipe,
                parent_id,
                endpoint: endpoint.clone(),
                work_frames: action_frames
                    .checked_add(probe_frames)
                    .ok_or("retained work overflow")?,
            }));
        } else if archive.entries.len() != retained.len() {
            return Err("nonallocating admission changed archive length".into());
        }
        let productive = endpoint.admission.newly_retained_id().is_some();
        let draw_work = target
            .frames_clocked()
            .checked_sub(draw_before)
            .ok_or("draw work counter moved backwards")?;
        if draw_work
            != action_frames
                .checked_add(probe_frames)
                .ok_or("draw component work overflow")?
        {
            return Err("draw work does not reconcile with components".into());
        }
        archive.record_selection(parent_id, &selector);
        archive.record_selection_outcome(parent_id, productive, draw_work)?;
        action_total = action_total
            .checked_add(action_frames)
            .ok_or("lane action work overflow")?;
        probe_total = probe_total
            .checked_add(probe_frames)
            .ok_or("lane probe work overflow")?;
        let active_ids = active_ids(&archive)?;
        let active_maximum = active_maximum(&archive)?;
        draws.push(DrawRecord {
            draw: recipe.draw,
            recipe: *recipe,
            source_index: recipe.source_index,
            selector_seed: recipe.selector_seed,
            selector,
            parent_id,
            parent_input_sha256,
            parent_snapshot_sha256,
            start,
            input,
            endpoint,
            productive,
            active_ids,
            active_maximum,
            selector_accounting: archive.selector_report(),
            total_work_frames: draw_work,
        });
    }
    let total_work_frames = action_total
        .checked_add(probe_total)
        .ok_or("lane work overflow")?;
    let lane_delta = target
        .frames_clocked()
        .checked_sub(lane_work_before)
        .ok_or("lane work counter moved backwards")?;
    if lane_delta != total_work_frames {
        return Err("lane work does not reconcile".into());
    }
    let (final_active_entries, champion_candidates) = final_entries(lane, &archive, &retained)?;
    Ok(LaneRecord {
        record: "lane",
        lane,
        worker: lane,
        setup_frames,
        initial_archive_sha256,
        draws,
        final_active_entries,
        final_maximum: active_maximum(&archive)?,
        maximum_lineage_actions,
        scheduled_draws: DRAWS,
        executed_draws: DRAWS,
        selections: DRAWS,
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
    lane: usize,
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
            || evidence.recipe.lane != lane
            || evidence.recipe.draw != evidence.draw
            || evidence.recipe.action != evidence.endpoint.action
            || entry.report.parent_id != Some(u64::try_from(evidence.parent_id)?)
            || entry.report.created_execution
                != u64::try_from(evidence.draw.checked_add(1).ok_or("draw overflow")?)?
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
            draw: evidence.draw,
            recipe: evidence.recipe,
            actions: entry.report.input.actions.len(),
            input_sha256: input_sha256.clone(),
            key: entry.report.key,
            watermark: watermark_from_key(entry.report.key),
            milestones: entry.report.milestones,
            snapshot_sha256,
            probe_survived: evidence.endpoint.probe_survived,
            work_frames: evidence.work_frames,
        });
        candidates.push(ChampionCandidate {
            lane,
            id,
            draw: evidence.draw,
            recipe: evidence.recipe,
            parent_id: evidence.parent_id,
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

fn classify(lanes: &[LaneRecord]) -> Result<ClassificationRecord, Box<dyn Error>> {
    if lanes.len() != LANES {
        return Err("lane count does not match the preregistration".into());
    }
    for (lane, record) in lanes.iter().enumerate() {
        if record.lane != lane
            || record.worker != lane
            || record.draws.len() != DRAWS
            || record.scheduled_draws != DRAWS
            || record.executed_draws != DRAWS
            || record.selections != DRAWS
            || !(SOURCE_ACTIONS..=MAX_LINEAGE_ACTIONS).contains(&record.maximum_lineage_actions)
        {
            return Err("lane record order or shape is not canonical".into());
        }
    }
    let candidates = lanes
        .iter()
        .flat_map(|lane| lane.champion_candidates.iter().cloned())
        .collect::<Vec<_>>();
    let eligible_entries = candidates.len();
    let champion = rank_champion(candidates);
    let verdict = verdict_for(champion.as_ref());
    Ok(ClassificationRecord {
        record: "classification",
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
            .then_with(|| left.lane.cmp(&right.lane))
            .then_with(|| left.id.cmp(&right.id))
    });
    candidates.first().map(|candidate| ChampionRecord {
        lane: candidate.lane,
        id: candidate.id,
        draw: candidate.draw,
        recipe: candidate.recipe,
        parent_id: candidate.parent_id,
        parent_lineage: candidate.parent_lineage.clone(),
        input: candidate.input.clone(),
        input_sha256: candidate.input_sha256.clone(),
        endpoint: candidate.endpoint.clone(),
        work_frames: candidate.work_frames,
    })
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct WorkSummary {
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
    lanes: &[LaneRecord],
    baseline_setup: u64,
    source_probe: u64,
) -> Result<WorkSummary, Box<dyn Error>> {
    if baseline_setup != EXPECTED_SETUP_FRAMES
        || source_probe != SOURCE_PROBE_FRAMES
        || lanes.len() != LANES
    {
        return Err("setup evidence does not match the preregistration".into());
    }
    let mut setup = baseline_setup;
    let mut scheduled = 0_usize;
    let mut executed = 0_usize;
    let mut selections = 0_usize;
    let mut action = 0_u64;
    let mut probe = 0_u64;
    for (lane, record) in lanes.iter().enumerate() {
        if record.lane != lane
            || record.setup_frames != EXPECTED_SETUP_FRAMES
            || record.scheduled_draws != DRAWS
            || record.executed_draws != DRAWS
            || record.selections != DRAWS
        {
            return Err("lane setup evidence is not canonical".into());
        }
        scheduled = scheduled
            .checked_add(record.scheduled_draws)
            .ok_or("scheduled candidate count overflow")?;
        executed = executed
            .checked_add(record.executed_draws)
            .ok_or("executed candidate count overflow")?;
        selections = selections
            .checked_add(record.selections)
            .ok_or("selection count overflow")?;
        setup = setup
            .checked_add(record.setup_frames)
            .ok_or("setup work overflow")?;
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
            return Err("lane work does not reconcile in summary".into());
        }
    }
    let expected_setup = EXPECTED_SETUP_FRAMES
        .checked_mul(u64::try_from(
            LANES.checked_add(1).ok_or("target count overflow")?,
        )?)
        .ok_or("expected setup work overflow")?;
    let expected_candidates = LANES
        .checked_mul(DRAWS)
        .ok_or("expected candidate count overflow")?;
    if scheduled != expected_candidates
        || executed != expected_candidates
        || selections != expected_candidates
        || setup != expected_setup
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
    use std::io::Read as _;

    use super::*;

    fn synthetic_source() -> SmbInput {
        SmbInput {
            actions: (0..SOURCE_ACTIONS)
                .map(|index| {
                    ButtonChord::new(
                        u8::try_from(index % 256).expect("synthetic button fits u8"),
                        u8::try_from(2 + index % 119).expect("synthetic duration fits u8"),
                    )
                })
                .collect(),
        }
    }

    fn observation(progress: u16) -> SmbObservations {
        let decoded = SmbMechanicalState {
            world: 7,
            level: 0,
            progress,
            ..SmbMechanicalState::default()
        };
        SmbObservations {
            frame_count: 1,
            wram: Vec::new(),
            decoded,
            milestones: SmbMilestones::default(),
            changed_indices: Vec::new(),
            dead: false,
            log_line: String::new(),
        }
    }

    fn candidate(
        lane: usize,
        id: usize,
        progress: u16,
        actions: usize,
        hash_byte: u8,
    ) -> ChampionCandidate {
        candidate_at(lane, id, 7, 3, progress, actions, hash_byte)
    }

    fn candidate_at(
        lane: usize,
        id: usize,
        world: u8,
        level: u8,
        progress: u16,
        actions: usize,
        hash_byte: u8,
    ) -> ChampionCandidate {
        let input = SmbInput {
            actions: vec![ButtonChord::new(0, 2); actions],
        };
        let input_sha256_bytes = [hash_byte; 32];
        let input_sha256 = input_sha256_bytes
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let mut candidate_observation = observation(progress);
        candidate_observation.decoded.world = world;
        candidate_observation.decoded.level = level;
        let mechanical = candidate_observation.decoded;
        let recipe = Recipe {
            lane,
            draw: id,
            source_index: 0,
            action: ButtonChord::new(0, 2),
            selector_seed: 0,
        };
        ChampionCandidate {
            lane,
            id,
            draw: id,
            recipe,
            parent_id: 0,
            input,
            input_sha256,
            input_sha256_bytes,
            parent_lineage: vec![0, u64::try_from(id).expect("id fits u64")],
            endpoint: EndpointEvidence {
                action: ButtonChord::new(0, 2),
                input_actions: actions,
                input_sha256: String::new(),
                observation: candidate_observation,
                mechanical,
                watermark: watermark(mechanical),
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

    fn key(progress: u16) -> SmbArchiveKey {
        SmbArchiveKey {
            world: 7,
            level: 0,
            progress,
            player_y_bucket: 1,
            player_engine_state: 8,
            state_fingerprint: u8::try_from(progress % 64).expect("fingerprint fits u8"),
            room_x_bucket: 0,
        }
    }

    fn fake_snapshot(progress: u16) -> SmbSnapshot {
        serde_json::from_value(serde_json::json!({
            "emulator_state": [],
            "observation": observation(progress),
            "dead": false,
            "failed": false
        }))
        .expect("deserialize synthetic snapshot")
    }

    fn retained_evidence(
        input: &SmbInput,
        key: SmbArchiveKey,
        snapshot: &SmbSnapshot,
        admission: AdmissionOutcome,
    ) -> RetainedEvidence {
        let mechanical = observation(key.progress).decoded;
        let action = *input.actions.last().expect("candidate has an action");
        let draw = admission
            .newly_retained_id()
            .expect("retained evidence has allocated id")
            .checked_sub(1)
            .expect("source is not retained evidence");
        RetainedEvidence {
            draw,
            recipe: Recipe {
                lane: 5,
                draw,
                source_index: 0,
                action,
                selector_seed: 0,
            },
            parent_id: 0,
            endpoint: EndpointEvidence {
                action,
                input_actions: input.actions.len(),
                input_sha256: sha256_json(input).expect("hash candidate input"),
                observation: observation(key.progress),
                mechanical,
                watermark: watermark_from_key(key),
                wram_sha256: sha256_bytes(&[]),
                snapshot_sha256: Some(sha256_json(snapshot).expect("hash candidate snapshot")),
                key: Some(key),
                milestones: SmbMilestones::default(),
                action_frames: 2,
                dead: false,
                failed: false,
                probe: vec![ProbeAttempt {
                    mask: 0,
                    work_frames: 45,
                    dead: false,
                    survived: true,
                }],
                probe_survived: true,
                probe_frames: 45,
                admission,
            },
            work_frames: 47,
        }
    }

    fn minimal_lane(lane: usize, action_frames: u64, probe_frames: u64) -> LaneRecord {
        LaneRecord {
            record: "lane",
            lane,
            worker: lane,
            setup_frames: EXPECTED_SETUP_FRAMES,
            initial_archive_sha256: String::new(),
            draws: Vec::new(),
            final_active_entries: Vec::new(),
            final_maximum: ActiveMaximum {
                watermark: BASELINE_WATERMARK,
                ids: vec![0],
            },
            maximum_lineage_actions: SOURCE_ACTIONS,
            scheduled_draws: DRAWS,
            executed_draws: DRAWS,
            selections: DRAWS,
            selector_accounting: SmbSelectorAccounting::default(),
            action_frames,
            probe_frames,
            total_work_frames: action_frames + probe_frames,
            champion_candidates: Vec::new(),
        }
    }

    #[test]
    fn seed_and_frozen_recipe_bytes_are_exact() {
        verify_seed().expect("sealed seed is self-consistent");
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "2b3fa68177b87af9dc7231f77bcd46b492bc836b71b67a51b25a93af99157954"
        );
        assert_eq!(EXPECTED_RECIPE_BYTES, 400_078);
        let mut source = synthetic_source();
        source.actions[1_996] = ButtonChord::new(128, 10);
        source.actions[2_054] = ButtonChord::new(2, 120);
        let recipes = derive_recipes(&source).expect("derive recipes");
        assert_eq!(recipes.len(), LANES);
        assert!(recipes.iter().all(|lane| lane.len() == DRAWS));
        assert_eq!(
            (
                recipes[0][0].source_index,
                recipes[0][0].action,
                recipes[0][0].selector_seed,
            ),
            (1_996, ButtonChord::new(128, 10), 5_296_170_292_050_619_932)
        );
        assert_eq!(
            (
                recipes[11][511].source_index,
                recipes[11][511].action,
                recipes[11][511].selector_seed,
            ),
            (2_054, ButtonChord::new(2, 120), 11_491_391_464_025_855_751)
        );
        assert_eq!(
            serde_json::to_vec(&(
                0_u64,
                0_u64,
                1_996_u64,
                recipes[0][0].action,
                recipes[0][0].selector_seed,
            ))
            .expect("serialize first recipe"),
            br#"[0,0,1996,{"buttons":128,"hold_frames":10},5296170292050619932]"#
        );
        let identity = recipe_identity_bytes(&recipes).expect("serialize recipes");
        assert_eq!(identity.len(), 403_709);
        assert_eq!(
            sha256_bytes(&identity),
            "580495dc6772b1374700f3a518404f7d5ecae58667edb1fa4947a0893e7774a5"
        );
        let mut projections = projection_bytes(&recipes).expect("serialize projections");
        assert_eq!(projections.len(), LANES);
        projections.sort();
        assert!(projections.windows(2).all(|window| window[0] != window[1]));
        assert!(projection_sha256(&recipes).is_err());
    }

    #[test]
    fn maximum_lineage_actions_are_bounded_and_reported() {
        assert_eq!(SOURCE_ACTIONS.checked_add(DRAWS), Some(MAX_LINEAGE_ACTIONS));
        assert!(MAX_LINEAGE_ACTIONS < ACTION_LIMIT);
        let mut maximum = SOURCE_ACTIONS;
        record_lineage_actions(&mut maximum, MAX_LINEAGE_ACTIONS)
            .expect("registered maximum is allowed");
        assert_eq!(maximum, MAX_LINEAGE_ACTIONS);
        assert!(record_lineage_actions(&mut maximum, MAX_LINEAGE_ACTIONS + 1).is_err());
        assert_eq!(maximum, MAX_LINEAGE_ACTIONS);
    }

    #[test]
    fn source_probe_transcript_and_work_are_exact() {
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
            source_probe_frames(&attempts).expect("registered transcript"),
            45
        );
        let mut malformed = attempts;
        malformed[0].work_frames = 44;
        assert!(source_probe_frames(&malformed).is_err());
    }

    #[test]
    fn source_shape_requires_the_registered_final_action() {
        let mut source = synthetic_source();
        *source.actions.last_mut().expect("source has actions") = BASELINE_FINAL_ACTION;
        validate_source(&source).expect("registered source shape is accepted");
        *source.actions.last_mut().expect("source has actions") = ButtonChord::new(0, 2);
        assert!(validate_source(&source).is_err());
    }

    #[test]
    fn champion_ranking_is_total_and_verdict_is_strict() {
        let ranked = rank_champion(vec![
            candidate(0, 9, 74, 8, 0x10),
            candidate(1, 8, 75, 12, 0xff),
            candidate(2, 7, 75, 10, 0xff),
            candidate(3, 6, 75, 10, 0x20),
            candidate(4, 5, 75, 10, 0x20),
            candidate(4, 4, 75, 10, 0x20),
        ])
        .expect("champion exists");
        assert_eq!((ranked.lane, ranked.id), (3, 6));
        assert_eq!(verdict_for(Some(&ranked)), Verdict::Adopt);

        let equal = rank_champion(vec![candidate(0, 1, 73, 1, 0)]).expect("candidate exists");
        assert_eq!(verdict_for(Some(&equal)), Verdict::Stop);
        assert_eq!(verdict_for(None), Verdict::Stop);
    }

    #[test]
    fn full_watermark_ranking_dominates_cross_level_progress() {
        let old_level = candidate_at(0, 1, 7, 2, u16::MAX, 1, 0x00);
        assert_eq!(
            verdict_for(Some(
                &rank_champion(vec![old_level.clone()]).expect("candidate")
            )),
            Verdict::Stop
        );
        let current_level = candidate_at(1, 2, 7, 3, 74, 1, 0xff);
        let later_world = candidate_at(2, 3, 8, 0, 0, 1, 0xff);
        let ranked = rank_champion(vec![later_world, current_level, old_level])
            .expect("cross-level candidate exists");
        assert_eq!((ranked.lane, ranked.id), (2, 3));
        assert_eq!(verdict_for(Some(&ranked)), Verdict::Adopt);
    }

    #[test]
    fn real_archive_final_active_eligibility_excludes_nonallocations() {
        let archive_key = key(237);
        let source = SmbInput {
            actions: vec![ButtonChord::new(1, 2); 2],
        };
        let mut archive = Archive::new();
        archive.max_entries = 4;
        archive.set_selector_policy(SmbArchiveSelectorPolicy::ConcentratedRecency);
        archive.set_waypoint_policy(SmbArchiveWaypointPolicy::Absent);
        archive.set_replacement_policy(SmbArchiveReplacementPolicy::FewestActions);
        assert_eq!(
            archive
                .insert(
                    None,
                    0,
                    ArchiveCandidate {
                        input: source.clone(),
                        key: archive_key,
                        milestones: SmbMilestones::default(),
                    },
                    fake_snapshot(237),
                )
                .expect("insert source"),
            Some(0)
        );
        let mut retained = vec![None];

        let displaced_input = SmbInput {
            actions: vec![ButtonChord::new(2, 2); 3],
        };
        let displaced_snapshot = fake_snapshot(237);
        let displaced_outcome = insert_candidate(
            &mut archive,
            Some(0),
            1,
            ArchiveCandidate {
                input: displaced_input.clone(),
                key: archive_key,
                milestones: SmbMilestones::default(),
            },
            displaced_snapshot.clone(),
        )
        .expect("insert first endpoint");
        assert_eq!(
            displaced_outcome,
            AdmissionOutcome::Retained {
                id: 1,
                displaced: false
            }
        );
        retained.push(Some(retained_evidence(
            &displaced_input,
            archive_key,
            &displaced_snapshot,
            displaced_outcome,
        )));

        let winning_input = SmbInput {
            actions: vec![ButtonChord::new(3, 2)],
        };
        let winning_snapshot = fake_snapshot(237);
        let winning_outcome = insert_candidate(
            &mut archive,
            Some(0),
            2,
            ArchiveCandidate {
                input: winning_input.clone(),
                key: archive_key,
                milestones: SmbMilestones::default(),
            },
            winning_snapshot.clone(),
        )
        .expect("insert replacing endpoint");
        assert_eq!(
            winning_outcome,
            AdmissionOutcome::Retained {
                id: 2,
                displaced: true
            }
        );
        retained.push(Some(retained_evidence(
            &winning_input,
            archive_key,
            &winning_snapshot,
            winning_outcome,
        )));
        assert!(!archive.active[1]);
        assert!(archive.active[0] && archive.active[2]);

        let duplicate = insert_candidate(
            &mut archive,
            Some(0),
            3,
            ArchiveCandidate {
                input: displaced_input,
                key: archive_key,
                milestones: SmbMilestones::default(),
            },
            fake_snapshot(237),
        )
        .expect("duplicate old id");
        assert_eq!(duplicate, AdmissionOutcome::Duplicate { id: 1 });

        let rejected = insert_candidate(
            &mut archive,
            Some(0),
            4,
            ArchiveCandidate {
                input: SmbInput {
                    actions: vec![ButtonChord::new(4, 2); 4],
                },
                key: archive_key,
                milestones: SmbMilestones::default(),
            },
            fake_snapshot(237),
        )
        .expect("reject costlier endpoint");
        assert_eq!(rejected, AdmissionOutcome::Rejected);

        let (entries, candidates) =
            final_entries(5, &archive, &retained).expect("derive final active eligible endpoints");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, 2);
        assert_eq!(candidates.len(), 1);
        let champion = rank_champion(candidates).expect("rank eligible endpoint");
        assert_eq!((champion.lane, champion.id), (5, 2));
        assert_eq!((champion.draw, champion.parent_id), (1, 0));
        assert_eq!(
            champion.recipe.action,
            *winning_input.actions.last().expect("action")
        );
        assert_eq!(champion.input, winning_input);
        assert_eq!(champion.parent_lineage, vec![0, 2]);
    }

    #[test]
    fn malformed_worker_reply_sets_and_error_order_are_rejected() {
        let mut missing = (0..LANES - 1)
            .map(|lane| LaneReply {
                lane,
                worker: lane,
                result: Ok(minimal_lane(lane, 0, 0)),
            })
            .collect::<Vec<_>>();
        missing.reverse();
        assert!(consume_lane_replies(missing).is_err());

        let duplicate = vec![
            LaneReply {
                lane: 0,
                worker: 0,
                result: Ok(minimal_lane(0, 0, 0)),
            },
            LaneReply {
                lane: 0,
                worker: 0,
                result: Ok(minimal_lane(0, 0, 0)),
            },
        ];
        assert!(consume_lane_replies(duplicate).is_err());

        let mut errors = (0..LANES)
            .map(|lane| LaneReply {
                lane,
                worker: lane,
                result: Err(format!("failure-{lane}")),
            })
            .collect::<Vec<_>>();
        errors.reverse();
        let error = consume_lane_replies(errors).expect_err("inner failures must surface");
        assert!(error.to_string().contains("lane 0: failure-0"));

        let mixed = vec![
            LaneReply {
                lane: 7,
                worker: 6,
                result: Err("wrong-worker".to_owned()),
            },
            LaneReply {
                lane: LANES,
                worker: LANES,
                result: Err("out-of-range".to_owned()),
            },
            LaneReply {
                lane: 2,
                worker: 2,
                result: Err("first".to_owned()),
            },
            LaneReply {
                lane: 2,
                worker: 2,
                result: Err("duplicate".to_owned()),
            },
        ];
        let first = consume_lane_replies(mixed).expect_err("mixed metadata must fail");
        let reversed = vec![
            LaneReply {
                lane: 2,
                worker: 2,
                result: Err("duplicate".to_owned()),
            },
            LaneReply {
                lane: 2,
                worker: 2,
                result: Err("first".to_owned()),
            },
            LaneReply {
                lane: LANES,
                worker: LANES,
                result: Err("out-of-range".to_owned()),
            },
            LaneReply {
                lane: 7,
                worker: 6,
                result: Err("wrong-worker".to_owned()),
            },
        ];
        let second = consume_lane_replies(reversed).expect_err("shuffled metadata must fail");
        assert_eq!(first.to_string(), second.to_string());
        assert_eq!(first.to_string(), "invalid lane reply: lane=7, worker=6");
    }

    #[test]
    fn registered_work_cap_reconciles_and_rejects_overage() {
        let per_lane_action = MAX_ACTION_FRAMES / u64::try_from(LANES).expect("lanes fit u64");
        let per_lane_probe = MAX_PROBE_FRAMES / u64::try_from(LANES).expect("lanes fit u64");
        let lanes = (0..LANES)
            .map(|lane| minimal_lane(lane, per_lane_action, per_lane_probe))
            .collect::<Vec<_>>();
        let summary = summarize_work(&lanes, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES)
            .expect("cap reconciles");
        assert_eq!(summary.action, MAX_ACTION_FRAMES);
        assert_eq!(summary.probe, MAX_PROBE_FRAMES);
        assert_eq!(summary.source_probe, SOURCE_PROBE_FRAMES);
        assert_eq!(summary.total, MAX_TOTAL_FRAMES);
        assert!(summarize_work(&lanes, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES - 1).is_err());

        let mut over = lanes;
        over[0].action_frames += 1;
        over[0].total_work_frames += 1;
        assert!(summarize_work(&over, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES).is_err());

        let mut wrong_count = (0..LANES)
            .map(|lane| minimal_lane(lane, per_lane_action, per_lane_probe))
            .collect::<Vec<_>>();
        wrong_count[0].executed_draws -= 1;
        assert!(summarize_work(&wrong_count, EXPECTED_SETUP_FRAMES, SOURCE_PROBE_FRAMES).is_err());
    }

    #[test]
    fn verdict_and_ndjson_digest_bytes_are_frozen() {
        assert_eq!(
            serde_json::to_string(&Verdict::Adopt).expect("serialize verdict"),
            "\"ADOPT\""
        );
        assert_eq!(
            serde_json::to_string(&Verdict::Stop).expect("serialize verdict"),
            "\"STOP\""
        );
        #[derive(Serialize)]
        struct Record {
            record: &'static str,
            value: u8,
        }
        let file = tempfile::NamedTempFile::new().expect("create temporary output");
        let path = file.path().to_owned();
        let mut output = NdjsonOutput::new(file.reopen().expect("reopen temporary output"));
        output
            .write(&Record {
                record: "x",
                value: 7,
            })
            .expect("write record");
        let expected = b"{\"record\":\"x\",\"value\":7}\n";
        assert_eq!(output.digest(), sha256_bytes(expected));
        assert_eq!(
            output.finish().expect("finish output"),
            sha256_bytes(expected)
        );
        let mut actual = Vec::new();
        fs::File::open(path)
            .expect("open output")
            .read_to_end(&mut actual)
            .expect("read output");
        assert_eq!(actual, expected);
    }
}
