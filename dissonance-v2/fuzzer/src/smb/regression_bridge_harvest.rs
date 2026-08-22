// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealed World 8-4 p153 regression-bridge H8 harvest.

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
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    smb::{
        archive::{
            SmbArchiveKey, SmbArchiveKeyPolicy, archive_key, merge_action_milestones,
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

const FORMAT: &str = "smb-w8-4-p153-regression-bridge-h8-harvest-v1";
const PREREGISTRATION_COMMIT: &str = "c7b869d1a22d281c2e418739c594b7ccf2918e36";
const PREREGISTRATION_DOC_SHA256: &str =
    "9b7b85c81dd7b6d2ca4e8c5892521a5c93081e05cce5c52bd30ff0044ebcaeb1";
const CODE_BASE: &str = "7312116a5280a7937b18e31c09497d78a18cc955";
const AUTHORIZING_PREREGISTRATION: &str = "3c264bf1aecc49cb6f04db70d41e05f9fac4b9fd";
const AUTHORIZING_IMPLEMENTATION: &str = "d6690276acddd7d48a6f29ee8e1d67778fb8c288";
const AUTHORIZING_RESULT: &str = "7312116a5280a7937b18e31c09497d78a18cc955";
const AUTHORIZING_REPORT_SHA256: &str =
    "c4499e7a8af1e2c2683b0fb40c0923e9ace320fb930fa5597f3bd892128cd26f";
const SOURCE_FILE_SHA256: &str = "14af93bd006ba77cea923ab31cb7aa8ac0ad903a7bc65d5a378c92ccc337300b";
const SOURCE_INPUT_SHA256: &str =
    "14af93bd006ba77cea923ab31cb7aa8ac0ad903a7bc65d5a378c92ccc337300b";
const SOURCE_WRAM_SHA256: &str = "897c7bc0df63a68249b75e81a8bfc8ea3a87a7c872241d4e51a2819ff39689c5";
const SOURCE_SNAPSHOT_SHA256: &str =
    "329594d247d5a97ea59a0e7ec1b0856cfb0388141941f05062e4d6641adf5344";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const EXPECTED_RECIPE_SHA256: &str =
    "aaf2196e37f51ac03eb802417c12e2aadb9100b0ff7dc1ecb4371167aae17060";
const EXPECTED_RECIPE_BYTES: usize = 515_409;
const SOURCE_BYTES: usize = 114_838;
const SOURCE_ACTIONS: usize = 3_576;
const SOURCE_FRAMES: u64 = 168_594;
const STREAMS: usize = 1_680;
const HORIZON: usize = 8;
const MAX_BOUNDARIES: usize = STREAMS * HORIZON;
const WORKERS: usize = 12;
const ACTION_LIMIT: usize = 4_096;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MASTER_SEED: u64 = 16_878_457_775_653_588_938;
const TAIL_INDEX_DOMAIN: &[u8] = b"regression-bridge-source-index";
const MAX_ACTION_FRAMES: u64 = 1_512_840;
const MAX_PROBE_FRAMES: u64 = 1_814_400;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 3_500_572;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const PROBE_FRAMES: u16 = 45;
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const BOUNDARY_TRACE_DOMAIN: &[u8] = b"smb-regression-bridge-h8-v1\0trace\0";
const SOURCE_MASKS: [u8; 14] = [0, 1, 2, 16, 32, 64, 66, 128, 129, 130, 131, 192, 193, 194];
const BASELINE_WATERMARK: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 3,
    progress: 153,
};
const BASELINE_ENDPOINT: SmbMechanicalState = SmbMechanicalState {
    world: 7,
    level: 3,
    progress: 153,
    player_y_bucket: 11,
    player_engine_state: 8,
    dead: false,
    flag_active: false,
};
const BASELINE_KEY: SmbArchiveKey = SmbArchiveKey {
    world: 7,
    level: 3,
    progress: 153,
    player_y_bucket: 11,
    player_engine_state: 8,
    state_fingerprint: 9,
    room_x_bucket: 0,
    rooms: 0,
    room: [0; 3],
};
const BASELINE_MILESTONES: SmbMilestones = SmbMilestones {
    max_1_1_scroll_bucket: 195,
    reached_1_1_flag: true,
    reached_1_2: true,
    reached_onward: true,
};
const BASELINE_FINAL_ACTION: ButtonChord = ButtonChord {
    buttons: 130,
    hold_frames: 104,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    stream: usize,
    first_mask: u8,
    first_duration: u8,
    actions: Vec<ButtonChord>,
    tail_source_indices: Vec<usize>,
    projection_bytes: usize,
    projection_sha256: String,
}

#[derive(Debug, Serialize)]
struct Config {
    streams: usize,
    horizon: usize,
    max_boundaries: usize,
    workers: usize,
    action_limit: usize,
    master_seed: u64,
    tail_index_domain: &'static str,
    source_masks: [u8; 14],
    first_action_order: &'static str,
    execution: &'static str,
    probe_masks: [u8; 3],
    probe_frames: u16,
    max_action_frames: u64,
    max_probe_frames: u64,
    max_total_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProbeAttempt {
    mask: u8,
    work_frames: u64,
    dead: bool,
    survived: bool,
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
    source_probe: ProbeAttempt,
}

#[derive(Clone)]
struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BoundaryRecord {
    record: &'static str,
    stream: usize,
    depth: usize,
    worker: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_setup_frames: Option<u64>,
    action: ButtonChord,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_index: Option<usize>,
    input_actions: usize,
    input_sha256: String,
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    transient_maximum: SmbProgressWatermark,
    action_trace_sha256: String,
    wram_sha256: String,
    snapshot_sha256: Option<String>,
    key: Option<SmbArchiveKey>,
    milestones: SmbMilestones,
    requested_frames: u64,
    action_frames: u64,
    dead: bool,
    failed: bool,
    probe: Vec<ProbeAttempt>,
    probe_survived: bool,
    probe_frames: u64,
    total_work_frames: u64,
    #[serde(skip)]
    input: SmbInput,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ChampionRecord {
    stream: usize,
    depth: usize,
    action: ButtonChord,
    source_index: Option<usize>,
    input: SmbInput,
    input_sha256: String,
    observation: SmbObservations,
    mechanical: SmbMechanicalState,
    watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: String,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
    action_frames: u64,
    probe: Vec<ProbeAttempt>,
    probe_frames: u64,
    total_work_frames: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum AdoptionVerdict {
    Adopt,
    NoAdopt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum BridgeVerdict {
    MultipleRegressionBridges,
    SingleRegressionBridge,
    NoRegressionBridge,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BridgeClassification {
    record: &'static str,
    verdict: BridgeVerdict,
    bridges: usize,
    distinct_first_actions: usize,
    distinct_inputs: usize,
    distinct_snapshots: usize,
    witnesses: Vec<ChampionRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct AdoptionClassification {
    record: &'static str,
    verdict: AdoptionVerdict,
    eligible_candidates: usize,
    champion: Option<ChampionRecord>,
}

#[derive(Debug, Serialize)]
struct HeaderRecord<'a> {
    record: &'static str,
    format: &'static str,
    preregistration_commit: &'static str,
    preregistration_doc_sha256: &'static str,
    code_base: &'static str,
    authorizing_preregistration: &'static str,
    authorizing_implementation: &'static str,
    authorizing_result: &'static str,
    authorizing_report_sha256: &'static str,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    rom_sha256: &'a str,
    executable_sha256: &'a str,
    bin_source_sha256: &'a str,
    module_source_sha256: &'a str,
    recipe_bytes: usize,
    recipe_sha256: &'a str,
    trace_sha256: &'a str,
    config_sha256: &'a str,
    config: &'a Config,
}

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    bridge_verdict: BridgeVerdict,
    adoption_verdict: AdoptionVerdict,
    champion: Option<ChampionRecord>,
    worker_setup_frames: Vec<u64>,
    streams: usize,
    boundaries: usize,
    setup_frames: u64,
    source_replay_frames: u64,
    source_probe_frames: u64,
    action_frames: u64,
    probe_frames: u64,
    experimental_frames: u64,
    total_frames: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StreamCompletionRecord {
    record: &'static str,
    stream: usize,
    worker: usize,
    completed_depths: usize,
    terminated_dead: bool,
}

#[derive(Debug)]
struct StreamExecution {
    boundaries: Vec<BoundaryRecord>,
    completion: StreamCompletionRecord,
}

struct StreamReply {
    stream: usize,
    worker: usize,
    result: Result<StreamExecution, String>,
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

/// Run the sealed harvest from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path =
        PathBuf::from(args.next().ok_or(
            "usage: smb-w8-4-p153-regression-bridge-h8-harvest <input.json> <output.jsonl>",
        )?);
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }

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
    let recipes = derive_recipes(&source)?;
    let recipe_bytes = recipe_identity_bytes(&recipes)?;
    let recipe_sha256 = sha256_bytes(&recipe_bytes);
    if recipe_bytes.len() != EXPECTED_RECIPE_BYTES || recipe_sha256 != EXPECTED_RECIPE_SHA256 {
        return Err("regression-bridge recipe identity does not match the sealed oracle".into());
    }
    let config = Config {
        streams: STREAMS,
        horizon: HORIZON,
        max_boundaries: MAX_BOUNDARIES,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        master_seed: MASTER_SEED,
        tail_index_domain: "regression-bridge-source-index",
        source_masks: SOURCE_MASKS,
        first_action_order: "source_mask_ascending_duration_ascending_v1",
        execution: "independent_source_restore_sequential_h8_strict_probe_v1",
        probe_masks: PROBE_MASKS,
        probe_frames: PROBE_FRAMES,
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
    let streams = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let boundaries = streams
        .iter()
        .flat_map(|stream| stream.boundaries.iter().cloned())
        .collect::<Vec<_>>();
    let bridge = classify_bridges(&boundaries, &recipes, &source)?;
    let adoption = classify_adoption(&boundaries, &recipes, &source)?;
    let work = summarize_work(&streams, &recipes, &source, &baseline.record)?;

    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration_commit: PREREGISTRATION_COMMIT,
        preregistration_doc_sha256: PREREGISTRATION_DOC_SHA256,
        code_base: CODE_BASE,
        authorizing_preregistration: AUTHORIZING_PREREGISTRATION,
        authorizing_implementation: AUTHORIZING_IMPLEMENTATION,
        authorizing_result: AUTHORIZING_RESULT,
        authorizing_report_sha256: AUTHORIZING_REPORT_SHA256,
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        rom_sha256: &rom_sha256,
        executable_sha256: &executable_sha256,
        bin_source_sha256: &bin_source_sha256,
        module_source_sha256: &module_source_sha256,
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
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
        recipes: &'a [Recipe],
    }
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_bytes: recipe_bytes.len(),
        recipe_sha256: &recipe_sha256,
        recipes: &recipes,
    })?;
    for stream in &streams {
        for boundary in &stream.boundaries {
            output.write(boundary)?;
        }
        output.write(&stream.completion)?;
    }
    output.write(&bridge)?;
    output.write(&adoption)?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        bridge_verdict: bridge.verdict,
        adoption_verdict: adoption.verdict,
        champion: adoption.champion.clone(),
        worker_setup_frames: work.worker_setup_frames,
        streams: streams.len(),
        boundaries: boundaries.len(),
        setup_frames: work.setup,
        source_replay_frames: baseline.record.replay_frames,
        source_probe_frames: baseline.record.source_probe.work_frames,
        action_frames: work.action,
        probe_frames: work.probe,
        experimental_frames: work.experimental,
        total_frames: work.total,
    };
    output.write(&summary)?;
    let report_sha256 = output.finish()?;
    println!(
        "{{\"report_sha256\":\"{report_sha256}\",\"bridge_verdict\":{},\"adoption_verdict\":{}}}",
        serde_json::to_string(&summary.bridge_verdict)?,
        serde_json::to_string(&summary.adoption_verdict)?,
    );
    Ok(())
}

fn validate_source(source: &SmbInput) -> Result<(), Box<dyn Error>> {
    if source.actions.len() != SOURCE_ACTIONS
        || source.actions.last() != Some(&BASELINE_FINAL_ACTION)
        || source
            .actions
            .iter()
            .any(|action| !(2..=MAX_HOLD_FRAMES).contains(&action.hold_frames))
    {
        return Err("source action evidence does not match the preregistration".into());
    }
    Ok(())
}

fn derive_recipes(source: &SmbInput) -> Result<Vec<Recipe>, Box<dyn Error>> {
    let masks = source
        .actions
        .iter()
        .map(|action| action.buttons)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if masks.as_slice() != SOURCE_MASKS {
        return Err("source opaque mask support does not match the preregistration".into());
    }
    let mut recipes = Vec::with_capacity(STREAMS);
    let mut projection_bytes = BTreeSet::new();
    for stream in 0..STREAMS {
        let first_mask = *SOURCE_MASKS
            .get(stream / usize::from(MAX_HOLD_FRAMES))
            .ok_or("first-action mask ordinal is out of range")?;
        let first_duration = u8::try_from(
            stream
                .checked_rem(usize::from(MAX_HOLD_FRAMES))
                .and_then(|value| value.checked_add(1))
                .ok_or("first-action duration overflow")?,
        )?;
        let mut actions = Vec::with_capacity(HORIZON);
        let mut tail_source_indices = Vec::with_capacity(HORIZON - 1);
        actions.push(ButtonChord::new(first_mask, first_duration));
        for depth in 1..HORIZON {
            let mut hasher = Sha256::new();
            hasher.update(MASTER_SEED.to_le_bytes());
            hasher.update(TAIL_INDEX_DOMAIN);
            hasher.update(u64::try_from(stream)?.to_le_bytes());
            hasher.update(u64::try_from(depth)?.to_le_bytes());
            let digest = hasher.finalize();
            let first_eight: [u8; 8] = digest
                .get(..8)
                .ok_or("tail-index digest is truncated")?
                .try_into()?;
            let source_index =
                usize::try_from(u64::from_le_bytes(first_eight) % u64::try_from(SOURCE_ACTIONS)?)?;
            let action = *source
                .actions
                .get(source_index)
                .ok_or("derived tail source index is out of range")?;
            tail_source_indices.push(source_index);
            actions.push(action);
        }
        let projection =
            serde_json::to_vec(&(first_mask, first_duration, &actions, &tail_source_indices))?;
        let projection_len = projection.len();
        let projection_sha256 = sha256_bytes(&projection);
        if !projection_bytes.insert(projection) {
            return Err("duplicate stream recipe projection".into());
        }
        recipes.push(Recipe {
            stream,
            first_mask,
            first_duration,
            actions,
            tail_source_indices,
            projection_bytes: projection_len,
            projection_sha256,
        });
    }
    if recipes.len() != STREAMS
        || recipes.iter().enumerate().any(|(stream, recipe)| {
            recipe.stream != stream
                || recipe.actions.len() != HORIZON
                || recipe.tail_source_indices.len() != HORIZON - 1
        })
    {
        return Err("regression-bridge recipe order is not canonical".into());
    }
    Ok(recipes)
}

fn recipe_identity_bytes(recipes: &[Recipe]) -> Result<Vec<u8>, Box<dyn Error>> {
    let identity = recipes
        .iter()
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.stream)?,
                recipe.first_mask,
                recipe.first_duration,
                &recipe.actions,
                &recipe.tail_source_indices,
            ))
        })
        .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
    Ok(serde_json::to_vec(&identity)?)
}

fn build_baseline(target: &mut SmbTarget, source: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
    let setup_frames = target.frames_clocked();
    if setup_frames != EXPECTED_SETUP_FRAMES {
        return Err("baseline setup work does not match the sealed value".into());
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
    let mut maximum = watermark(initial.decoded);
    let mut milestones = initial.milestones;
    for (index, action) in source.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok || target.is_dead() {
            return Err("registered source did not replay alive".into());
        }
        trace.update(u64::try_from(index)?.to_le_bytes());
        hash_framed_json(&mut trace, action)?;
        hash_framed_json(&mut trace, target.last_action_observations())?;
        merge_progress_watermark(&mut maximum, target.last_action_observations());
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
        || maximum != BASELINE_WATERMARK
        || wram_sha256 != SOURCE_WRAM_SHA256
        || snapshot_sha256 != SOURCE_SNAPSHOT_SHA256
        || key != BASELINE_KEY
        || milestones != BASELINE_MILESTONES
    {
        return Err("source replay evidence does not match the preregistration".into());
    }
    target.restore(&snapshot)?;
    verify_snapshot(target, &snapshot)?;
    let before = target.frames_clocked();
    let survived = target.survives_probe(0, PROBE_FRAMES);
    let work_frames = target
        .frames_clocked()
        .checked_sub(before)
        .ok_or("source probe work moved backwards")?;
    let source_probe = ProbeAttempt {
        mask: 0,
        work_frames,
        dead: target.is_dead(),
        survived,
    };
    if target.exit_kind() != ExitKind::Ok
        || source_probe
            != (ProbeAttempt {
                mask: 0,
                work_frames: SOURCE_PROBE_FRAMES,
                dead: false,
                survived: true,
            })
    {
        return Err("source probe evidence does not match the preregistration".into());
    }
    target.restore(&snapshot)?;
    verify_snapshot(target, &snapshot)?;
    if sha256_bytes(target.wram()) != SOURCE_WRAM_SHA256
        || sha256_json(&target.snapshot().ok_or("source resnapshot failed")?)?
            != SOURCE_SNAPSHOT_SHA256
    {
        return Err("source probe did not restore the exact source".into());
    }
    let baseline_work = target
        .frames_clocked()
        .checked_sub(replay_before)
        .ok_or("baseline total work moved backwards")?;
    if baseline_work
        != replay_frames
            .checked_add(source_probe.work_frames)
            .ok_or("baseline work overflow")?
    {
        return Err("baseline replay and source-probe work do not reconcile".into());
    }
    Ok(Baseline {
        record: BaselineRecord {
            record: "baseline",
            setup_frames,
            replay_frames,
            actions: source.actions.len(),
            endpoint_observation,
            endpoint,
            watermark: maximum,
            trace_sha256: finish_sha256(trace),
            wram_sha256,
            snapshot_sha256,
            key,
            milestones,
            final_action: BASELINE_FINAL_ACTION,
            source_probe,
        },
        snapshot,
    })
}

fn evaluate_parallel(
    rom: &[u8],
    source: &SmbInput,
    recipes: &[Recipe],
    baseline: &Baseline,
) -> Result<Vec<StreamExecution>, Box<dyn Error>> {
    if recipes.len() != STREAMS {
        return Err("recipe count does not match the preregistration".into());
    }
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| -> Result<(), Box<dyn Error>> {
        let mut handles = Vec::with_capacity(WORKERS);
        for worker in 0..WORKERS {
            let sender = sender.clone();
            let source = source.clone();
            let recipes = recipes.to_vec();
            let baseline = baseline.clone();
            handles.push(
                thread::Builder::new()
                    .name(format!("regression-bridge-{worker}"))
                    .spawn_scoped(scope, move || {
                        let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)
                            .map_err(|error| error.to_string());
                        let mut prior_error = target.as_ref().ok().and_then(|target| {
                            (target.frames_clocked() != EXPECTED_SETUP_FRAMES).then(|| {
                                format!(
                                    "worker {worker} setup frames: expected {EXPECTED_SETUP_FRAMES}, got {}",
                                    target.frames_clocked()
                                )
                            })
                        });
                        for stream in (worker..STREAMS).step_by(WORKERS) {
                            let result = if let Some(error) = prior_error.as_ref() {
                                Err(format!("worker unavailable after prior error: {error}"))
                            } else {
                                match target.as_mut() {
                                    Ok(target) => match recipes.get(stream) {
                                        Some(recipe) => run_stream(
                                            target,
                                            &source,
                                            recipe,
                                            &baseline,
                                            worker,
                                        )
                                        .map_err(|error| error.to_string()),
                                        None => Err("missing recipe".to_owned()),
                                    },
                                    Err(error) => Err(error.clone()),
                                }
                            };
                            if let Err(error) = &result {
                                prior_error = Some(error.clone());
                            }
                            let _ = sender.send(StreamReply {
                                stream,
                                worker,
                                result,
                            });
                        }
                    })?,
            );
        }
        drop(sender);
        for handle in handles {
            handle
                .join()
                .map_err(|_| "regression-bridge worker panicked")?;
        }
        Ok(())
    })?;
    consume_replies(receiver.into_iter().collect())
}

fn consume_replies(replies: Vec<StreamReply>) -> Result<Vec<StreamExecution>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    let mut metadata_errors = Vec::new();
    for reply in replies {
        if reply.stream >= STREAMS || reply.worker != reply.stream % WORKERS {
            metadata_errors.push((0_u8, reply.stream, reply.worker, "invalid"));
        } else if buffered.insert(reply.stream, reply.result).is_some() {
            metadata_errors.push((1_u8, reply.stream, reply.worker, "duplicate"));
        }
    }
    for stream in 0..STREAMS {
        if !buffered.contains_key(&stream) {
            metadata_errors.push((2_u8, stream, stream % WORKERS, "missing"));
        }
    }
    metadata_errors.sort_unstable();
    if let Some((_, stream, worker, kind)) = metadata_errors.first() {
        return Err(format!("{kind} stream reply: stream={stream}, worker={worker}").into());
    }
    let mut streams = Vec::with_capacity(STREAMS);
    for stream in 0..STREAMS {
        streams.push(
            buffered
                .remove(&stream)
                .ok_or("missing stream reply")?
                .map_err(|error| format!("stream {stream}: {error}"))?,
        );
    }
    Ok(streams)
}

fn run_stream(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipe: &Recipe,
    baseline: &Baseline,
    worker: usize,
) -> Result<StreamExecution, Box<dyn Error>> {
    if recipe.stream >= STREAMS || worker != recipe.stream % WORKERS {
        return Err("stream worker ownership is not canonical".into());
    }
    target.restore(&baseline.snapshot)?;
    verify_snapshot(target, &baseline.snapshot)?;
    let mut input = source.clone();
    let mut boundaries = Vec::with_capacity(HORIZON);
    for (action_index, action) in recipe.actions.iter().copied().enumerate() {
        let depth = action_index.checked_add(1).ok_or("stream depth overflow")?;
        let pre_action = smb_mechanical_state_from_wram(target.wram());
        let before = target.frames_clocked();
        target.apply(&action);
        let action_frames = target
            .frames_clocked()
            .checked_sub(before)
            .ok_or("boundary action work moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during regression-bridge action".into());
        }
        let dead = target.is_dead();
        let requested_frames = u64::from(action.bounded_hold_frames());
        if action_frames > requested_frames || (!dead && action_frames != requested_frames) {
            return Err("boundary action work does not match its duration".into());
        }
        input.actions.push(action);
        if input.actions.len() > ACTION_LIMIT {
            return Err("stream input exceeds the action limit".into());
        }
        let observation = target.observe();
        let mechanical = smb_mechanical_state_from_wram(target.wram());
        let endpoint_watermark = watermark(mechanical);
        let mut transient_maximum = watermark(pre_action);
        merge_progress_watermark(&mut transient_maximum, target.last_action_observations());
        let mut milestones = BASELINE_MILESTONES;
        merge_action_milestones(&mut milestones, target)?;
        let input_sha256 = sha256_json(&input)?;
        let wram_sha256 = sha256_bytes(target.wram());
        let mut trace = Sha256::new();
        trace.update(BOUNDARY_TRACE_DOMAIN);
        trace.update(u64::try_from(recipe.stream)?.to_le_bytes());
        trace.update(u64::try_from(depth)?.to_le_bytes());
        hash_framed_json(&mut trace, &action)?;
        hash_framed_json(&mut trace, target.last_action_observations())?;
        let mut snapshot_sha256 = None;
        let mut key = None;
        let mut probe = Vec::new();
        let mut probe_survived = false;
        let mut probe_frames = 0_u64;
        if !dead {
            let snapshot = target
                .snapshot()
                .ok_or("failed to snapshot live boundary")?;
            snapshot_sha256 = Some(sha256_json(&snapshot)?);
            key = Some(archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen));
            if endpoint_watermark > BASELINE_WATERMARK {
                let result = run_probe(target, &snapshot)?;
                probe = result.0;
                probe_survived = result.1;
                probe_frames = result.2;
            }
        }
        let total_work_frames = target
            .frames_clocked()
            .checked_sub(before)
            .ok_or("boundary total work moved backwards")?;
        if total_work_frames
            != action_frames
                .checked_add(probe_frames)
                .ok_or("boundary work overflow")?
        {
            return Err("boundary work does not reconcile".into());
        }
        boundaries.push(BoundaryRecord {
            record: "boundary",
            stream: recipe.stream,
            depth,
            worker,
            worker_setup_frames: (recipe.stream < WORKERS && depth == 1)
                .then_some(EXPECTED_SETUP_FRAMES),
            action,
            source_index: depth
                .checked_sub(2)
                .and_then(|index| recipe.tail_source_indices.get(index).copied()),
            input_actions: input.actions.len(),
            input_sha256,
            observation,
            mechanical,
            watermark: endpoint_watermark,
            transient_maximum,
            action_trace_sha256: finish_sha256(trace),
            wram_sha256,
            snapshot_sha256,
            key,
            milestones,
            requested_frames,
            action_frames,
            dead,
            failed: false,
            probe,
            probe_survived,
            probe_frames,
            total_work_frames,
            input: input.clone(),
        });
        if dead {
            break;
        }
    }
    let completed_depths = boundaries.len();
    let terminated_dead = boundaries.last().is_some_and(|boundary| boundary.dead);
    Ok(StreamExecution {
        boundaries,
        completion: StreamCompletionRecord {
            record: "stream_completion",
            stream: recipe.stream,
            worker,
            completed_depths,
            terminated_dead,
        },
    })
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
            .ok_or("probe attempt work moved backwards")?;
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during candidate probe".into());
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
        .ok_or("probe total work moved backwards")?;
    let summed = attempts.iter().try_fold(0_u64, |sum, attempt| {
        sum.checked_add(attempt.work_frames)
            .ok_or("probe work overflow")
    })?;
    if total != summed || total > u64::from(PROBE_FRAMES) * 3 {
        return Err("probe work does not reconcile".into());
    }
    Ok((attempts, survived, total))
}

fn eligible(boundary: &BoundaryRecord) -> bool {
    !boundary.dead
        && !boundary.failed
        && boundary.probe_survived
        && boundary.watermark > BASELINE_WATERMARK
        && boundary.snapshot_sha256.is_some()
        && boundary.key.is_some()
}

fn rank(mut boundaries: Vec<&BoundaryRecord>) -> Result<Option<ChampionRecord>, Box<dyn Error>> {
    boundaries.sort_by(|left, right| {
        right
            .watermark
            .cmp(&left.watermark)
            .then_with(|| left.input_actions.cmp(&right.input_actions))
            .then_with(|| left.input_sha256.cmp(&right.input_sha256))
            .then_with(|| left.stream.cmp(&right.stream))
            .then_with(|| left.depth.cmp(&right.depth))
    });
    boundaries
        .first()
        .map(|boundary| champion(boundary))
        .transpose()
}

fn champion(boundary: &BoundaryRecord) -> Result<ChampionRecord, Box<dyn Error>> {
    Ok(ChampionRecord {
        stream: boundary.stream,
        depth: boundary.depth,
        action: boundary.action,
        source_index: boundary.source_index,
        input: boundary.input.clone(),
        input_sha256: boundary.input_sha256.clone(),
        observation: boundary.observation.clone(),
        mechanical: boundary.mechanical,
        watermark: boundary.watermark,
        wram_sha256: boundary.wram_sha256.clone(),
        snapshot_sha256: boundary
            .snapshot_sha256
            .clone()
            .ok_or("eligible champion lacks snapshot identity")?,
        key: boundary.key.ok_or("eligible champion lacks Frozen key")?,
        milestones: boundary.milestones,
        action_frames: boundary.action_frames,
        probe: boundary.probe.clone(),
        probe_frames: boundary.probe_frames,
        total_work_frames: boundary.total_work_frames,
    })
}

fn classify_adoption(
    boundaries: &[BoundaryRecord],
    recipes: &[Recipe],
    source: &SmbInput,
) -> Result<AdoptionClassification, Box<dyn Error>> {
    validate_boundaries(boundaries, recipes, source)?;
    let eligible = canonical_eligible(boundaries, recipes, source)?;
    let eligible_candidates = eligible.len();
    let champion = rank(eligible)?;
    Ok(AdoptionClassification {
        record: "adoption_classification",
        verdict: if champion.is_some() {
            AdoptionVerdict::Adopt
        } else {
            AdoptionVerdict::NoAdopt
        },
        eligible_candidates,
        champion,
    })
}

fn classify_bridges(
    boundaries: &[BoundaryRecord],
    recipes: &[Recipe],
    source: &SmbInput,
) -> Result<BridgeClassification, Box<dyn Error>> {
    validate_boundaries(boundaries, recipes, source)?;
    let eligible = canonical_eligible(boundaries, recipes, source)?;
    let first_by_stream = boundaries
        .iter()
        .filter(|boundary| boundary.depth == 1)
        .map(|boundary| (boundary.stream, boundary))
        .collect::<BTreeMap<_, _>>();
    let bridges = eligible
        .into_iter()
        .filter(|boundary| {
            boundary.depth >= 2
                && first_by_stream.get(&boundary.stream).is_some_and(|first| {
                    !first.dead && !first.failed && first.watermark < BASELINE_WATERMARK
                })
        })
        .collect::<Vec<_>>();
    let distinct_first_actions = bridges
        .iter()
        .filter_map(|boundary| {
            recipes
                .get(boundary.stream)
                .and_then(|recipe| recipe.actions.first())
                .copied()
        })
        .collect::<BTreeSet<_>>()
        .len();
    let distinct_inputs = bridges
        .iter()
        .map(|boundary| boundary.input_sha256.as_str())
        .collect::<BTreeSet<_>>()
        .len();
    let distinct_snapshots = bridges
        .iter()
        .filter_map(|boundary| boundary.snapshot_sha256.as_deref())
        .collect::<BTreeSet<_>>()
        .len();
    let bridge_count = bridges.len();
    let verdict = if bridge_count >= 2
        && distinct_first_actions >= 2
        && distinct_inputs >= 2
        && distinct_snapshots >= 2
    {
        BridgeVerdict::MultipleRegressionBridges
    } else if bridge_count == 0 {
        BridgeVerdict::NoRegressionBridge
    } else {
        BridgeVerdict::SingleRegressionBridge
    };
    let mut ranked = bridges;
    ranked.sort_by_key(|boundary| (boundary.stream, boundary.depth));
    let witnesses = ranked
        .into_iter()
        .take(2)
        .map(champion)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(BridgeClassification {
        record: "bridge_classification",
        verdict,
        bridges: bridge_count,
        distinct_first_actions,
        distinct_inputs,
        distinct_snapshots,
        witnesses,
    })
}

fn canonical_eligible<'a>(
    boundaries: &'a [BoundaryRecord],
    recipes: &[Recipe],
    _source: &SmbInput,
) -> Result<Vec<&'a BoundaryRecord>, Box<dyn Error>> {
    let mut owners = BTreeMap::new();
    for recipe in recipes {
        let mut suffix = Vec::with_capacity(HORIZON);
        for (action_index, action) in recipe.actions.iter().copied().enumerate() {
            suffix.push(action);
            let depth = action_index.checked_add(1).ok_or("owner depth overflow")?;
            owners
                .entry(suffix.clone())
                .or_insert((recipe.stream, depth));
        }
    }
    Ok(boundaries
        .iter()
        .filter(|boundary| {
            let suffix = boundary
                .input
                .actions
                .get(SOURCE_ACTIONS..)
                .unwrap_or_default();
            eligible(boundary) && owners.get(suffix) == Some(&(boundary.stream, boundary.depth))
        })
        .collect())
}

fn validate_boundaries(
    boundaries: &[BoundaryRecord],
    recipes: &[Recipe],
    source: &SmbInput,
) -> Result<(), Box<dyn Error>> {
    if boundaries.len() > MAX_BOUNDARIES || recipes.len() != STREAMS {
        return Err("boundary or recipe count exceeds the preregistration".into());
    }
    let mut expected_stream = 0_usize;
    let mut expected_depth = 1_usize;
    let mut previous_dead = false;
    for boundary in boundaries {
        while expected_stream < boundary.stream {
            expected_stream = expected_stream
                .checked_add(1)
                .ok_or("expected stream overflow")?;
            expected_depth = 1;
            previous_dead = false;
        }
        let recipe = recipes
            .get(boundary.stream)
            .ok_or("boundary stream has no recipe")?;
        let action_index = boundary.depth.checked_sub(1).ok_or("zero boundary depth")?;
        let expected_action = *recipe
            .actions
            .get(action_index)
            .ok_or("boundary depth exceeds its recipe")?;
        let expected_source_index = action_index
            .checked_sub(1)
            .and_then(|index| recipe.tail_source_indices.get(index).copied());
        let probe_sum = boundary.probe.iter().try_fold(0_u64, |sum, attempt| {
            sum.checked_add(attempt.work_frames)
                .ok_or("boundary probe work overflow")
        })?;
        let expected_input_actions = SOURCE_ACTIONS
            .checked_add(boundary.depth)
            .ok_or("boundary input length overflow")?;
        if previous_dead
            || boundary.stream != expected_stream
            || boundary.depth != expected_depth
            || boundary.worker != boundary.stream % WORKERS
            || boundary.worker_setup_frames
                != (boundary.stream < WORKERS && boundary.depth == 1)
                    .then_some(EXPECTED_SETUP_FRAMES)
            || boundary.action != expected_action
            || boundary.source_index != expected_source_index
            || boundary.input_actions != expected_input_actions
            || boundary.input.actions.len() != boundary.input_actions
            || boundary.input.actions.get(..SOURCE_ACTIONS) != Some(source.actions.as_slice())
            || boundary.input.actions.get(SOURCE_ACTIONS..)
                != Some(&recipe.actions[..boundary.depth])
            || sha256_json(&boundary.input)? != boundary.input_sha256
            || boundary.requested_frames != u64::from(expected_action.hold_frames)
            || boundary.action_frames > boundary.requested_frames
            || (!boundary.dead && boundary.action_frames != boundary.requested_frames)
            || boundary.probe_frames != probe_sum
            || boundary.total_work_frames
                != boundary
                    .action_frames
                    .checked_add(boundary.probe_frames)
                    .ok_or("boundary work overflow")?
            || boundary.failed
        {
            return Err("boundary identity or work is not canonical".into());
        }
        if boundary.dead {
            if boundary.snapshot_sha256.is_some()
                || boundary.key.is_some()
                || !boundary.probe.is_empty()
                || boundary.probe_survived
                || boundary.probe_frames != 0
            {
                return Err("terminal boundary contains live endpoint evidence".into());
            }
        } else {
            if boundary.snapshot_sha256.is_none() || boundary.key.is_none() {
                return Err("live boundary lacks endpoint identity".into());
            }
            let strict = boundary.watermark > BASELINE_WATERMARK;
            if strict == boundary.probe.is_empty()
                || (!strict && (boundary.probe_survived || boundary.probe_frames != 0))
                || boundary.probe.len() > PROBE_MASKS.len()
            {
                return Err("boundary probe eligibility is not canonical".into());
            }
            for (attempt, expected_mask) in boundary.probe.iter().zip(PROBE_MASKS) {
                if attempt.mask != expected_mask || attempt.work_frames > u64::from(PROBE_FRAMES) {
                    return Err("boundary probe attempt is not canonical".into());
                }
            }
            if boundary
                .probe
                .iter()
                .take(boundary.probe.len().saturating_sub(1))
                .any(|attempt| attempt.survived)
                || (!boundary.probe.is_empty()
                    && boundary.probe.last().map(|attempt| attempt.survived)
                        != Some(boundary.probe_survived))
            {
                return Err("boundary probe short-circuit is not canonical".into());
            }
        }
        previous_dead = boundary.dead;
        expected_depth = expected_depth
            .checked_add(1)
            .ok_or("expected depth overflow")?;
    }
    Ok(())
}

#[derive(Debug)]
struct WorkSummary {
    worker_setup_frames: Vec<u64>,
    setup: u64,
    action: u64,
    probe: u64,
    experimental: u64,
    total: u64,
}

fn summarize_work(
    streams: &[StreamExecution],
    recipes: &[Recipe],
    source: &SmbInput,
    baseline: &BaselineRecord,
) -> Result<WorkSummary, Box<dyn Error>> {
    if streams.len() != STREAMS {
        return Err("stream count does not match the preregistration".into());
    }
    let boundaries = streams
        .iter()
        .flat_map(|stream| stream.boundaries.iter().cloned())
        .collect::<Vec<_>>();
    validate_boundaries(&boundaries, recipes, source)?;
    for (stream_index, stream) in streams.iter().enumerate() {
        if stream.completion.stream != stream_index
            || stream.completion.worker != stream_index % WORKERS
            || stream.completion.completed_depths != stream.boundaries.len()
            || stream.completion.completed_depths == 0
            || stream.completion.completed_depths > HORIZON
            || stream.completion.terminated_dead
                != stream
                    .boundaries
                    .last()
                    .is_some_and(|boundary| boundary.dead)
            || (stream.completion.completed_depths < HORIZON && !stream.completion.terminated_dead)
        {
            return Err("stream completion evidence is not canonical".into());
        }
    }
    if baseline.setup_frames != EXPECTED_SETUP_FRAMES
        || baseline.replay_frames != SOURCE_FRAMES
        || baseline.source_probe.work_frames != SOURCE_PROBE_FRAMES
    {
        return Err("baseline work does not match the preregistration".into());
    }
    let action = boundaries.iter().try_fold(0_u64, |sum, boundary| {
        sum.checked_add(boundary.action_frames)
            .ok_or("action work overflow")
    })?;
    let probe = boundaries.iter().try_fold(0_u64, |sum, boundary| {
        sum.checked_add(boundary.probe_frames)
            .ok_or("probe work overflow")
    })?;
    let worker_setup_frames = boundaries
        .iter()
        .filter_map(|boundary| boundary.worker_setup_frames)
        .collect::<Vec<_>>();
    let setup = EXPECTED_SETUP_FRAMES
        .checked_mul(u64::try_from(WORKERS + 1)?)
        .ok_or("setup work overflow")?;
    let experimental = action
        .checked_add(probe)
        .ok_or("experimental work overflow")?;
    let total = setup
        .checked_add(SOURCE_FRAMES)
        .and_then(|value| value.checked_add(SOURCE_PROBE_FRAMES))
        .and_then(|value| value.checked_add(experimental))
        .ok_or("total work overflow")?;
    if worker_setup_frames != vec![EXPECTED_SETUP_FRAMES; WORKERS]
        || action > MAX_ACTION_FRAMES
        || probe > MAX_PROBE_FRAMES
        || total > MAX_TOTAL_FRAMES
    {
        return Err("work exceeds the preregistered bounds".into());
    }
    Ok(WorkSummary {
        worker_setup_frames,
        setup,
        action,
        probe,
        experimental,
        total,
    })
}

fn verify_snapshot(target: &mut SmbTarget, expected: &SmbSnapshot) -> Result<(), Box<dyn Error>> {
    if target.exit_kind() != ExitKind::Ok {
        return Err("restored snapshot has a failed exit kind".into());
    }
    let actual = target
        .snapshot()
        .ok_or("failed to resnapshot restored state")?;
    let observation = target.observe();
    if &actual != expected
        || target.wram().as_slice() != observation.wram.as_slice()
        || smb_mechanical_state_from_wram(target.wram()) != observation.decoded
    {
        return Err("restored snapshot is not byte-exact".into());
    }
    Ok(())
}

fn watermark(state: SmbMechanicalState) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: state.world,
        level: state.level,
        progress: state.progress,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn synthetic_source() -> SmbInput {
        let mut actions = Vec::with_capacity(SOURCE_ACTIONS);
        for index in 0..SOURCE_ACTIONS - 1 {
            let mask = SOURCE_MASKS[index % SOURCE_MASKS.len()];
            let duration =
                u8::try_from(index % usize::from(MAX_HOLD_FRAMES) + 1).expect("synthetic duration");
            actions.push(ButtonChord::new(mask, duration));
        }
        actions.push(BASELINE_FINAL_ACTION);
        SmbInput { actions }
    }

    fn boundary(
        source: &SmbInput,
        recipe: &Recipe,
        depth: usize,
        boundary_watermark: SmbProgressWatermark,
        snapshot: &str,
    ) -> BoundaryRecord {
        let mechanical = SmbMechanicalState {
            world: boundary_watermark.world,
            level: boundary_watermark.level,
            progress: boundary_watermark.progress,
            ..SmbMechanicalState::default()
        };
        let action = recipe.actions[depth - 1];
        let mut input = source.clone();
        input.actions.extend_from_slice(&recipe.actions[..depth]);
        let strict = boundary_watermark > BASELINE_WATERMARK;
        let probe = strict.then(|| ProbeAttempt {
            mask: PROBE_MASKS[0],
            work_frames: u64::from(PROBE_FRAMES),
            dead: false,
            survived: true,
        });
        BoundaryRecord {
            record: "boundary",
            stream: recipe.stream,
            depth,
            worker: recipe.stream % WORKERS,
            worker_setup_frames: (recipe.stream < WORKERS && depth == 1)
                .then_some(EXPECTED_SETUP_FRAMES),
            action,
            source_index: depth
                .checked_sub(2)
                .and_then(|index| recipe.tail_source_indices.get(index).copied()),
            input_actions: SOURCE_ACTIONS + depth,
            input_sha256: sha256_json(&input).expect("input hash"),
            observation: SmbObservations {
                frame_count: 0,
                wram: Vec::new(),
                decoded: mechanical,
                milestones: BASELINE_MILESTONES,
                changed_indices: Vec::new(),
                dead: false,
                log_line: String::new(),
            },
            mechanical,
            watermark: boundary_watermark,
            transient_maximum: boundary_watermark,
            action_trace_sha256: String::new(),
            wram_sha256: String::new(),
            snapshot_sha256: Some(snapshot.to_owned()),
            key: Some(SmbArchiveKey {
                world: boundary_watermark.world,
                level: boundary_watermark.level,
                progress: boundary_watermark.progress,
                ..BASELINE_KEY
            }),
            milestones: BASELINE_MILESTONES,
            requested_frames: u64::from(action.hold_frames),
            action_frames: u64::from(action.hold_frames),
            dead: false,
            failed: false,
            probe: probe.iter().cloned().collect(),
            probe_survived: strict,
            probe_frames: if strict { u64::from(PROBE_FRAMES) } else { 0 },
            total_work_frames: u64::from(action.hold_frames)
                + if strict { u64::from(PROBE_FRAMES) } else { 0 },
            input,
        }
    }

    #[test]
    fn recipes_freeze_grid_domains_and_projection_distinctness() {
        let recipes = derive_recipes(&synthetic_source()).expect("recipes");
        assert_eq!(recipes.len(), STREAMS);
        assert_eq!(recipes[0].actions[0], ButtonChord::new(0, 1));
        assert_eq!(
            recipes[0].tail_source_indices,
            vec![1477, 187, 1473, 1139, 713, 3111, 1861]
        );
        assert_eq!(recipes[STREAMS - 1].actions[0], ButtonChord::new(194, 120));
        assert_eq!(
            recipes[STREAMS - 1].tail_source_indices,
            vec![2084, 1367, 518, 2486, 2543, 1613, 1444]
        );
        assert_eq!(
            recipes
                .iter()
                .map(|recipe| recipe.projection_sha256.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            STREAMS
        );
        assert_eq!(EXPECTED_RECIPE_BYTES, 515_409);
        assert_eq!(
            EXPECTED_RECIPE_SHA256,
            "aaf2196e37f51ac03eb802417c12e2aadb9100b0ff7dc1ecb4371167aae17060"
        );
    }

    #[test]
    fn real_bridge_classifier_requires_lower_first_step_and_distinct_evidence() {
        let source = synthetic_source();
        let recipes = derive_recipes(&source).expect("recipes");
        let lower = SmbProgressWatermark {
            progress: 9,
            ..BASELINE_WATERMARK
        };
        let strict = SmbProgressWatermark {
            progress: 154,
            ..BASELINE_WATERMARK
        };
        let boundaries = vec![
            boundary(&source, &recipes[0], 1, lower, "lower-0"),
            boundary(&source, &recipes[0], 2, strict, "strict-0"),
            boundary(&source, &recipes[1], 1, lower, "lower-1"),
            boundary(&source, &recipes[1], 2, strict, "strict-1"),
        ];
        let classification =
            classify_bridges(&boundaries, &recipes, &source).expect("classification");
        assert_eq!(
            classification.verdict,
            BridgeVerdict::MultipleRegressionBridges
        );
        assert_eq!(classification.bridges, 2);
        let only_one =
            classify_bridges(&boundaries[..2], &recipes, &source).expect("single classification");
        assert_eq!(only_one.verdict, BridgeVerdict::SingleRegressionBridge);
        let mut not_lower = boundaries.clone();
        not_lower[0].watermark = BASELINE_WATERMARK;
        not_lower[2].watermark = BASELINE_WATERMARK;
        assert_eq!(
            classify_bridges(&not_lower, &recipes, &source)
                .expect("no bridge")
                .verdict,
            BridgeVerdict::NoRegressionBridge
        );
        assert_eq!(
            rank(vec![&boundaries[0], &boundaries[1]])
                .expect("rank")
                .expect("champion")
                .watermark,
            strict
        );
    }

    #[test]
    fn verdict_bytes_and_work_cap_are_frozen() {
        assert_eq!(
            serde_json::to_string(&BridgeVerdict::MultipleRegressionBridges).expect("verdict"),
            r#""MULTIPLE_REGRESSION_BRIDGES""#
        );
        assert_eq!(
            serde_json::to_string(&AdoptionVerdict::NoAdopt).expect("verdict"),
            r#""NO_ADOPT""#
        );
        assert_eq!(
            MAX_ACTION_FRAMES
                + MAX_PROBE_FRAMES
                + SOURCE_FRAMES
                + SOURCE_PROBE_FRAMES
                + EXPECTED_SETUP_FRAMES * u64::try_from(WORKERS + 1).expect("targets"),
            MAX_TOTAL_FRAMES
        );
    }

    #[test]
    fn reply_errors_are_arrival_order_independent() {
        let make = |stream: usize| StreamReply {
            stream,
            worker: stream % WORKERS,
            result: Err(format!("failure-{stream}")),
        };
        let ascending = (0..STREAMS).map(make).collect::<Vec<_>>();
        let descending = (0..STREAMS).rev().map(make).collect::<Vec<_>>();
        assert_eq!(
            consume_replies(ascending).expect_err("failure").to_string(),
            consume_replies(descending)
                .expect_err("failure")
                .to_string()
        );
    }

    #[test]
    fn boundary_validation_rejects_recipe_drift() {
        let source = synthetic_source();
        let recipes = derive_recipes(&source).expect("recipes");
        let mut record = boundary(
            &source,
            &recipes[0],
            1,
            SmbProgressWatermark {
                progress: 9,
                ..BASELINE_WATERMARK
            },
            "snapshot",
        );
        record.action = ButtonChord::new(1, 1);
        assert!(validate_boundaries(&[record], &recipes, &source).is_err());
    }
}
