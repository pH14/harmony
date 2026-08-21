// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealed observer-event action-prefix salvage canary at the C119 SMB frontier.

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

use fuzzer::{
    smb::target::{
        ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMechanicalState, SmbObservations,
        SmbProgressWatermark, SmbSnapshot, SmbTarget, smb_mechanical_state_from_wram,
    },
    target::Target,
};
use libafl::executors::ExitKind;
use serde::Serialize;
use sha2::{Digest, Sha256};

const FORMAT: &str = "smb-observer-prefix-salvage-canary-v1";
const PREREGISTRATION: &str =
    "experiments/smb-completion/SOL-ACTION-PREFIX-SALVAGE-CANARY.md@24895d4a";
const CODE_BASE: &str = "91919a81";
const C119_PRODUCTION_BINARY_SHA256: &str =
    "87fb11f300a7af9386eb06c8b55e7a7353d6cb3654b83ee6a5615806e72e2862";
const SEED_LABEL: &str = "sol-restart-c119-continuous-rollout-v1";
const SEED_LABEL_SHA256: &str = "d0a86c80cac50cec33f1a6a55db713f93468796f14f554cdcc040f5d000a9d60";
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
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const ROLLOUT_ACTION_RECIPE_SHA256: &str =
    "a000be41bdc99f2b7c0fd1b35d8a35770c0f377b446d17ea724735d8454800a2";
const SOURCE_ACTIONS: usize = 3_297;
const SOURCE_FRAMES: u64 = 155_148;
const STREAMS: usize = 100;
const ACTIONS_PER_STREAM: usize = 32;
const WORKERS: usize = 12;
const MASTER_SEED: u64 = 17_009_187_366_200_191_184;
const RECIPE_DOMAIN: &[u8] = b"rollout-corpus-index";
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const VIABILITY_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const VIABILITY_FRAMES: u16 = 45;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_FULL_ACTION_WORK: u64 = 12_000;
const MAX_CANDIDATE_WORK: u64 = 714_000;
const MAX_PROBE_WORK: u64 = 1_606_500;
const EXPECTED_SETUP_WORK_FRAMES: u64 = 361;
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

#[derive(Clone, Debug, Serialize)]
struct CanaryConfig {
    policy: &'static str,
    master_seed: u64,
    streams: usize,
    rollout_actions_per_stream: usize,
    workers: usize,
    assignment: &'static str,
    event_filter: &'static str,
    viability_masks: [u8; VIABILITY_MASKS.len()],
    viability_frames: u16,
    full_action_work_cap: u64,
    candidate_work_cap: u64,
    probe_work_cap: u64,
    gate: &'static str,
}

#[derive(Debug, Serialize)]
struct HeaderRecord<'a> {
    record: &'static str,
    format: &'static str,
    preregistration: &'static str,
    code_base: &'static str,
    c119_production_binary_sha256: &'static str,
    seed_label: &'static str,
    seed_label_sha256: &'static str,
    source_archive_sha256: &'static str,
    source_stream_sha256: &'static str,
    source_entry_id: u64,
    source_parent_id: u64,
    source_created_execution: u64,
    source_path: String,
    source_file_sha256: &'a str,
    source_input_sha256: &'a str,
    source_actions: usize,
    rom_path: String,
    rom_sha256: &'a str,
    executable_path: String,
    executable_sha256: &'a str,
    rollout_action_recipe_sha256: &'a str,
    recipe_sha256: &'a str,
    config_sha256: &'a str,
    config: &'a CanaryConfig,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct BaselineRecord {
    record: &'static str,
    setup_work_frames: u64,
    absolute_frames: u64,
    replay_work_frames: u64,
    actions: usize,
    death: bool,
    failure: bool,
    endpoint: SmbMechanicalState,
    max_watermark: SmbProgressWatermark,
    trace_sha256: String,
    final_wram_sha256: String,
    final_snapshot_sha256: String,
}

#[derive(Clone)]
struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
    trace: Sha256,
}

type RecipeIdentity = (u64, u64, ButtonChord);

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StreamRecipe {
    stream: usize,
    source_index: usize,
    full_chord: ButtonChord,
    full_chord_sha256: String,
}

type RecipeDerivation = (Vec<Vec<ButtonChord>>, Vec<Vec<usize>>, Vec<StreamRecipe>);

#[derive(Debug, Serialize)]
struct RecipeRecord<'a> {
    record: &'static str,
    rollout_action_recipe_sha256: &'a str,
    recipe_sha256: &'a str,
    rollout_source_indices: &'a [Vec<usize>],
    streams: &'a [StreamRecipe],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ProbeAttempt {
    mask: u8,
    work_frames: u64,
    death: bool,
    failure: bool,
    survived: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct FullActionEvidence {
    action: ButtonChord,
    candidate_sha256: String,
    suffix_sha256: String,
    action_observations: Vec<SmbObservations>,
    requested_frames: u64,
    actual_frames: u64,
    absolute_frames: u64,
    frames_since_restore: u64,
    work_frames: u64,
    death: bool,
    failure: bool,
    endpoint: SmbMechanicalState,
    endpoint_watermark: SmbProgressWatermark,
    max_watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: String,
    trace_sha256: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CandidateEvidence {
    event_offset: u64,
    full_action: ButtonChord,
    full_chord_sha256: String,
    shortened_action: ButtonChord,
    matched_observation: SmbObservations,
    candidate_sha256: String,
    suffix_sha256: String,
    requested_frames: u64,
    actual_frames: u64,
    absolute_frames: u64,
    frames_since_restore: u64,
    work_frames: u64,
    death: bool,
    failure: bool,
    endpoint: SmbMechanicalState,
    endpoint_watermark: SmbProgressWatermark,
    max_watermark: SmbProgressWatermark,
    wram_sha256: String,
    snapshot_sha256: String,
    trace_sha256: String,
    probe_attempts: Vec<ProbeAttempt>,
    probe_survived: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StreamEvidence {
    record: &'static str,
    stream: usize,
    source_index: usize,
    full_chord_sha256: String,
    full: FullActionEvidence,
    candidates: Vec<CandidateEvidence>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct CreditEvidence {
    event_offset: u64,
    candidate_sha256: String,
    snapshot_sha256: String,
    watermark: SmbProgressWatermark,
    absolute_frames: u64,
    frames_since_restore: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct StreamClassification {
    stream: usize,
    source_index: usize,
    full_chord_sha256: String,
    canonical_owner: bool,
    atomic_live_deeper: bool,
    atomic_transient_deeper: bool,
    event_prefix_salvage: bool,
    probe_surviving_salvage: bool,
    probe_refusal: bool,
    credited: Option<CreditEvidence>,
}

#[derive(Debug, Serialize)]
struct ClassificationRecord<'a> {
    record: &'static str,
    streams: &'a [StreamClassification],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct ClassificationCounts {
    atomic_live_deeper: usize,
    atomic_transient_deeper: usize,
    event_prefix_salvage: usize,
    probe_surviving_salvage: usize,
    probe_refusal: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum Verdict {
    Go,
    Inconclusive,
    Stop,
}

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    body_sha256: String,
    verdict: Verdict,
    raw: ClassificationCounts,
    canonical: ClassificationCounts,
    distinct_full_chord_hashes: usize,
    distinct_candidate_hashes: usize,
    distinct_snapshot_hashes: usize,
    credited: Vec<CreditEvidence>,
    full_action_requested_frames: u64,
    full_action_work_frames: u64,
    candidate_requested_frames: u64,
    candidate_work_frames: u64,
    probe_work_frames: u64,
    setup_work_frames_per_target: u64,
    setup_target_count: usize,
    total_setup_work_frames: u64,
    worker_setup_work_frames: Vec<u64>,
    source_replay_work_frames: u64,
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

    fn write<T: Serialize>(&mut self, record: &T) -> Result<(), Box<dyn Error>> {
        let mut bytes = serde_json::to_vec(record)?;
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
struct EvaluationReply<T> {
    stream: usize,
    evaluation: Result<T, String>,
}

#[derive(Debug)]
struct WorkerSetupReply {
    worker: usize,
    setup_work_frames: Result<u64, String>,
}

struct ParallelEvaluation<T> {
    evaluations: Vec<EvaluationReply<T>>,
    worker_setups: Vec<WorkerSetupReply>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-observer-prefix-salvage-canary <input.json> <output.jsonl>")?,
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

    // Seal every action recipe byte before reading the ROM or constructing a target.
    let (rollout_actions, rollout_source_indices, recipes) = derive_recipes(&source)?;
    let rollout_action_recipe_sha256 = sha256_json(&rollout_actions)?;
    if rollout_action_recipe_sha256 != ROLLOUT_ACTION_RECIPE_SHA256 {
        return Err("rollout action recipe does not match the sealed census".into());
    }
    let recipe_identities = recipes
        .iter()
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.stream)?,
                u64::try_from(recipe.source_index)?,
                recipe.full_chord,
            ))
        })
        .collect::<Result<Vec<RecipeIdentity>, Box<dyn Error>>>()?;
    let recipe_sha256 = sha256_json(&recipe_identities)?;
    let canonical_owners = canonical_chord_owners(&recipes);

    let config = CanaryConfig {
        policy: "observer_event_action_prefix_salvage_v1",
        master_seed: MASTER_SEED,
        streams: STREAMS,
        rollout_actions_per_stream: ACTIONS_PER_STREAM,
        workers: WORKERS,
        assignment: "stream_mod_12_persistent_buffered_ascending_v1",
        event_filter: "interior_alive_strictly_deeper_first_at_offset_v1",
        viability_masks: VIABILITY_MASKS,
        viability_frames: VIABILITY_FRAMES,
        full_action_work_cap: MAX_FULL_ACTION_WORK,
        candidate_work_cap: MAX_CANDIDATE_WORK,
        probe_work_cap: MAX_PROBE_WORK,
        gate: "two_distinct_chords_candidates_and_snapshots_v1",
    };
    let config_sha256 = sha256_json(&config)?;

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
    let executable = read_bounded(&executable_path, MAX_EXECUTABLE_BYTES, "current executable")?;
    let executable_sha256 = sha256_bytes(&executable);

    let mut baseline_target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut baseline_target, &source)?;

    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration: PREREGISTRATION,
        code_base: CODE_BASE,
        c119_production_binary_sha256: C119_PRODUCTION_BINARY_SHA256,
        seed_label: SEED_LABEL,
        seed_label_sha256: SEED_LABEL_SHA256,
        source_archive_sha256: SOURCE_ARCHIVE_SHA256,
        source_stream_sha256: SOURCE_STREAM_SHA256,
        source_entry_id: 48_076,
        source_parent_id: 29_805,
        source_created_execution: 49_709,
        source_path: source_path.to_string_lossy().into_owned(),
        source_file_sha256: &source_file_sha256,
        source_input_sha256: &source_input_sha256,
        source_actions: source.actions.len(),
        rom_path: rom_path.to_string_lossy().into_owned(),
        rom_sha256: &rom_sha256,
        executable_path: executable_path.to_string_lossy().into_owned(),
        executable_sha256: &executable_sha256,
        rollout_action_recipe_sha256: &rollout_action_recipe_sha256,
        recipe_sha256: &recipe_sha256,
        config_sha256: &config_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    output.write(&RecipeRecord {
        record: "recipes",
        rollout_action_recipe_sha256: &rollout_action_recipe_sha256,
        recipe_sha256: &recipe_sha256,
        rollout_source_indices: &rollout_source_indices,
        streams: &recipes,
    })?;

    let parallel = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let evidence =
        consume_replies(parallel.evaluations, STREAMS).map_err(|error| -> Box<dyn Error> {
            format!("canonical worker result failed: {error}").into()
        })?;
    let worker_setup_work_frames = consume_worker_setups(parallel.worker_setups, WORKERS).map_err(
        |error| -> Box<dyn Error> { format!("canonical worker setup failed: {error}").into() },
    )?;
    let setup_accounting =
        validate_setup_work_frames(baseline.record.setup_work_frames, &worker_setup_work_frames)?;
    let classifications = classify_all(&evidence, &canonical_owners)?;
    validate_duplicate_equivalence(&evidence, &classifications)?;
    let summary_data = summarize(&evidence, &classifications)?;
    for stream in &evidence {
        output.write(stream)?;
    }
    output.write(&ClassificationRecord {
        record: "classification",
        streams: &classifications,
    })?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        verdict: summary_data.verdict,
        raw: summary_data.raw,
        canonical: summary_data.canonical,
        distinct_full_chord_hashes: summary_data.distinct_full_chord_hashes,
        distinct_candidate_hashes: summary_data.distinct_candidate_hashes,
        distinct_snapshot_hashes: summary_data.distinct_snapshot_hashes,
        credited: summary_data.credited,
        full_action_requested_frames: summary_data.full_action_requested_frames,
        full_action_work_frames: summary_data.full_action_work_frames,
        candidate_requested_frames: summary_data.candidate_requested_frames,
        candidate_work_frames: summary_data.candidate_work_frames,
        probe_work_frames: summary_data.probe_work_frames,
        setup_work_frames_per_target: setup_accounting.per_target,
        setup_target_count: setup_accounting.target_count,
        total_setup_work_frames: setup_accounting.total,
        worker_setup_work_frames,
        source_replay_work_frames: baseline.record.replay_work_frames,
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
        .any(|action| !(1..=MAX_HOLD_FRAMES).contains(&action.hold_frames))
    {
        return Err("source contains an out-of-domain hold duration".into());
    }
    Ok(())
}

fn derive_recipes(source: &SmbInput) -> Result<RecipeDerivation, Box<dyn Error>> {
    let source_len = u64::try_from(source.actions.len())?;
    if source_len == 0 {
        return Err("cannot derive recipes from an empty source".into());
    }
    let mut action_recipes = Vec::with_capacity(STREAMS);
    let mut source_index_recipes = Vec::with_capacity(STREAMS);
    let mut recipes = Vec::with_capacity(STREAMS);
    for stream in 0..STREAMS {
        let mut actions = Vec::with_capacity(ACTIONS_PER_STREAM);
        let mut indices = Vec::with_capacity(ACTIONS_PER_STREAM);
        for action in 0..ACTIONS_PER_STREAM {
            let index = derived_source_index(source_len, stream, action)?;
            let chord = *source
                .actions
                .get(index)
                .ok_or("derived source index is out of bounds")?;
            indices.push(index);
            actions.push(chord);
        }
        let full_chord = *actions.first().ok_or("derived recipe is empty")?;
        let source_index = *indices
            .first()
            .ok_or("derived source-index recipe is empty")?;
        recipes.push(StreamRecipe {
            stream,
            source_index,
            full_chord,
            full_chord_sha256: sha256_json(&full_chord)?,
        });
        action_recipes.push(actions);
        source_index_recipes.push(indices);
    }
    Ok((action_recipes, source_index_recipes, recipes))
}

fn derived_source_index(
    source_len: u64,
    stream: usize,
    action: usize,
) -> Result<usize, Box<dyn Error>> {
    if source_len == 0 {
        return Err("cannot reduce a recipe digest modulo zero".into());
    }
    let mut hasher = Sha256::new();
    hasher.update(MASTER_SEED.to_le_bytes());
    hasher.update(RECIPE_DOMAIN);
    hasher.update(u64::try_from(stream)?.to_le_bytes());
    hasher.update(u64::try_from(action)?.to_le_bytes());
    let digest = hasher.finalize();
    let bytes: [u8; 8] = digest
        .get(..8)
        .ok_or("SHA-256 digest is unexpectedly short")?
        .try_into()?;
    Ok(usize::try_from(u64::from_le_bytes(bytes) % source_len)?)
}

fn canonical_chord_owners(recipes: &[StreamRecipe]) -> BTreeMap<String, usize> {
    let mut owners = BTreeMap::new();
    for recipe in recipes {
        owners
            .entry(recipe.full_chord_sha256.clone())
            .or_insert(recipe.stream);
    }
    owners
}

fn build_baseline(target: &mut SmbTarget, source: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
    let setup_work_frames = target.frames_clocked();
    if setup_work_frames != EXPECTED_SETUP_WORK_FRAMES {
        return Err("baseline target setup work does not match the sealed implementation".into());
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
    let mut max_watermark = watermark(initial.decoded);
    for (index, action) in source.actions.iter().copied().enumerate() {
        target.apply(&action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during registered source replay".into());
        }
        hash_action_trace(&mut trace, index, action, target.last_action_observations())?;
        merge_watermark(&mut max_watermark, target.last_action_observations());
        if target.is_dead() {
            return Err("registered source reached death".into());
        }
    }
    let absolute_frames = target.observe().frame_count;
    let replay_work_frames = target
        .frames_clocked()
        .checked_sub(work_before)
        .ok_or("baseline work counter moved backwards")?;
    if absolute_frames != replay_work_frames {
        return Err("baseline observation and work frames do not reconcile".into());
    }
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let wram_sha256 = sha256_bytes(target.wram());
    let snapshot = target
        .snapshot()
        .ok_or("failed to snapshot C119 endpoint")?;
    let snapshot_sha256 = sha256_json(&snapshot)?;
    let trace_sha256 = finish_sha256(trace.clone());
    if absolute_frames != SOURCE_FRAMES
        || source.actions.len() != SOURCE_ACTIONS
        || endpoint != BASELINE_ENDPOINT
        || max_watermark != BASELINE_WATERMARK
        || target.is_dead()
        || target.exit_kind() != ExitKind::Ok
        || wram_sha256 != SOURCE_WRAM_SHA256
        || snapshot_sha256 != SOURCE_SNAPSHOT_SHA256
        || trace_sha256 != SOURCE_TRACE_SHA256
    {
        return Err("source replay evidence does not match the preregistration".into());
    }
    Ok(Baseline {
        record: BaselineRecord {
            record: "baseline",
            setup_work_frames,
            absolute_frames,
            replay_work_frames,
            actions: source.actions.len(),
            death: target.is_dead(),
            failure: target.exit_kind() != ExitKind::Ok,
            endpoint,
            max_watermark,
            trace_sha256,
            final_wram_sha256: wram_sha256,
            final_snapshot_sha256: snapshot_sha256,
        },
        snapshot,
        trace,
    })
}

fn evaluate_parallel(
    rom: &[u8],
    source: &SmbInput,
    recipes: &[StreamRecipe],
    baseline: &Baseline,
) -> Result<ParallelEvaluation<StreamEvidence>, Box<dyn Error>> {
    if recipes.len() != STREAMS {
        return Err("recipe count does not match the registered stream count".into());
    }
    let (sender, receiver) = mpsc::channel();
    let (setup_sender, setup_receiver) = mpsc::channel();
    thread::scope(
        |scope| -> Result<ParallelEvaluation<StreamEvidence>, Box<dyn Error>> {
            let mut handles = Vec::with_capacity(WORKERS);
            for worker in 0..WORKERS {
                let worker_recipes = recipes
                    .iter()
                    .filter(|recipe| worker_for_stream(recipe.stream) == worker)
                    .cloned()
                    .collect::<Vec<_>>();
                let sender = sender.clone();
                let setup_sender = setup_sender.clone();
                let snapshot = baseline.snapshot.clone();
                let trace = baseline.trace.clone();
                let source = source.clone();
                let handle = thread::Builder::new()
                    .name(format!("prefix-salvage-{worker}"))
                    .spawn_scoped(scope, move || {
                        let (mut state, setup_work_frames) =
                            match SmbTarget::from_smb_rom_bytes_headless(rom) {
                                Ok(target) => {
                                    let setup_work_frames = target.frames_clocked();
                                    (WorkerState::Ready(Box::new(target)), Ok(setup_work_frames))
                                }
                                Err(error) => {
                                    let error = error.to_string();
                                    (WorkerState::Failed(error.clone()), Err(error))
                                }
                            };
                        if setup_sender
                            .send(WorkerSetupReply {
                                worker,
                                setup_work_frames,
                            })
                            .is_err()
                        {
                            return;
                        }
                        for recipe in worker_recipes {
                            let evaluation = match &mut state {
                                WorkerState::Ready(target) => {
                                    evaluate_stream(target, &source, &snapshot, &trace, &recipe)
                                        .map_err(|error| error.to_string())
                                }
                                WorkerState::Failed(error) => Err(error.clone()),
                            };
                            if sender
                                .send(EvaluationReply {
                                    stream: recipe.stream,
                                    evaluation,
                                })
                                .is_err()
                            {
                                break;
                            }
                        }
                    })?;
                handles.push(handle);
            }
            drop(sender);
            drop(setup_sender);
            for handle in handles {
                handle
                    .join()
                    .map_err(|_| "observer-prefix worker panicked")?;
            }
            Ok(ParallelEvaluation {
                evaluations: receiver.into_iter().collect(),
                worker_setups: setup_receiver.into_iter().collect(),
            })
        },
    )
}

enum WorkerState {
    Ready(Box<SmbTarget>),
    Failed(String),
}

fn worker_for_stream(stream: usize) -> usize {
    stream % WORKERS
}

fn consume_replies<T>(
    replies: impl IntoIterator<Item = EvaluationReply<T>>,
    expected: usize,
) -> Result<Vec<T>, String> {
    consume_ordered(
        replies
            .into_iter()
            .map(|reply| (reply.stream, reply.evaluation)),
        expected,
        "stream",
    )
}

fn consume_worker_setups(
    replies: impl IntoIterator<Item = WorkerSetupReply>,
    expected: usize,
) -> Result<Vec<u64>, String> {
    consume_ordered(
        replies
            .into_iter()
            .map(|reply| (reply.worker, reply.setup_work_frames)),
        expected,
        "worker",
    )
}

fn consume_ordered<T>(
    replies: impl IntoIterator<Item = (usize, Result<T, String>)>,
    expected: usize,
    label: &str,
) -> Result<Vec<T>, String> {
    let mut buffered = BTreeMap::new();
    for (index, evaluation) in replies {
        if index >= expected {
            return Err(format!("{label} reply index {index} is out of range"));
        }
        if buffered.insert(index, evaluation).is_some() {
            return Err(format!("duplicate reply for {label} {index}"));
        }
    }
    let mut ordered = Vec::with_capacity(expected);
    for index in 0..expected {
        let evaluation = buffered
            .remove(&index)
            .ok_or_else(|| format!("missing reply for {label} {index}"))?;
        ordered.push(evaluation.map_err(|error| format!("{label} {index}: {error}"))?);
    }
    Ok(ordered)
}

struct SetupAccounting {
    per_target: u64,
    target_count: usize,
    total: u64,
}

fn validate_setup_work_frames(
    baseline_setup_work_frames: u64,
    worker_setup_work_frames: &[u64],
) -> Result<SetupAccounting, Box<dyn Error>> {
    if baseline_setup_work_frames != EXPECTED_SETUP_WORK_FRAMES
        || worker_setup_work_frames.len() != WORKERS
        || worker_setup_work_frames
            .iter()
            .any(|frames| *frames != baseline_setup_work_frames)
    {
        return Err("target setup work is not identical across all canary instances".into());
    }
    let target_count = worker_setup_work_frames
        .len()
        .checked_add(1)
        .ok_or("setup target count overflow")?;
    let target_count_u64 = u64::try_from(target_count)?;
    let total = baseline_setup_work_frames
        .checked_mul(target_count_u64)
        .ok_or("total setup work overflow")?;
    Ok(SetupAccounting {
        per_target: baseline_setup_work_frames,
        target_count,
        total,
    })
}

fn evaluate_stream(
    target: &mut SmbTarget,
    source: &SmbInput,
    source_snapshot: &SmbSnapshot,
    source_trace: &Sha256,
    recipe: &StreamRecipe,
) -> Result<StreamEvidence, Box<dyn Error>> {
    target.restore(source_snapshot)?;
    validate_restore(target)?;
    let full_work_before = target.frames_clocked();
    let prior_frame = target.observe().frame_count;
    target.apply(&recipe.full_chord);
    if target.exit_kind() != ExitKind::Ok {
        return Err("emulator failed during a full canary action".into());
    }
    let full_work_frames = target
        .frames_clocked()
        .checked_sub(full_work_before)
        .ok_or("full-action work counter moved backwards")?;
    let full_absolute_frames = target.observe().frame_count;
    let full_actual_frames = full_absolute_frames
        .checked_sub(prior_frame)
        .ok_or("full-action frame count moved backwards")?;
    let full_since_restore = full_absolute_frames
        .checked_sub(SOURCE_FRAMES)
        .ok_or("full-action endpoint precedes the restored source")?;
    if full_actual_frames != full_work_frames || full_since_restore != full_work_frames {
        return Err("full-action frame accounting does not reconcile".into());
    }
    if !target.is_dead() && full_actual_frames != u64::from(recipe.full_chord.hold_frames) {
        return Err("live full action did not execute its requested duration".into());
    }
    let full_observations = target.last_action_observations().to_vec();
    let mut full_max_watermark = BASELINE_WATERMARK;
    merge_watermark(&mut full_max_watermark, &full_observations);
    let full_endpoint = smb_mechanical_state_from_wram(target.wram());
    let full_endpoint_watermark = watermark(full_endpoint);
    let full_snapshot = target
        .snapshot()
        .ok_or("failed to snapshot full-action endpoint")?;
    let full_snapshot_sha256 = sha256_json(&full_snapshot)?;
    let mut full_trace = source_trace.clone();
    hash_action_trace(
        &mut full_trace,
        SOURCE_ACTIONS,
        recipe.full_chord,
        &full_observations,
    )?;
    let full_suffix = [recipe.full_chord];
    let full = FullActionEvidence {
        action: recipe.full_chord,
        candidate_sha256: candidate_sha256(source, &full_suffix)?,
        suffix_sha256: sha256_json(&full_suffix)?,
        action_observations: full_observations.clone(),
        requested_frames: u64::from(recipe.full_chord.hold_frames),
        actual_frames: full_actual_frames,
        absolute_frames: full_absolute_frames,
        frames_since_restore: full_since_restore,
        work_frames: full_work_frames,
        death: target.is_dead(),
        failure: false,
        endpoint: full_endpoint,
        endpoint_watermark: full_endpoint_watermark,
        max_watermark: full_max_watermark,
        wram_sha256: sha256_bytes(target.wram()),
        snapshot_sha256: full_snapshot_sha256,
        trace_sha256: finish_sha256(full_trace),
    };

    let event_offsets = qualifying_event_offsets(
        &full_observations,
        full_absolute_frames,
        recipe.full_chord.hold_frames,
    )?;
    let mut candidates = Vec::with_capacity(event_offsets.len());
    for (event_offset, observation) in event_offsets {
        let duration = u8::try_from(event_offset)?;
        let shortened_action = ButtonChord::new(recipe.full_chord.buttons, duration);
        target.restore(source_snapshot)?;
        validate_restore(target)?;
        let work_before = target.frames_clocked();
        target.apply(&shortened_action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during a shortened canary action".into());
        }
        let work_frames = target
            .frames_clocked()
            .checked_sub(work_before)
            .ok_or("shortened-action work counter moved backwards")?;
        let absolute_frames = target.observe().frame_count;
        let actual_frames = absolute_frames
            .checked_sub(SOURCE_FRAMES)
            .ok_or("shortened-action endpoint precedes the restored source")?;
        if work_frames != actual_frames || actual_frames != event_offset {
            return Err("shortened-action frame accounting does not reconcile".into());
        }
        let endpoint = smb_mechanical_state_from_wram(target.wram());
        if absolute_frames != observation.frame_count
            || target.wram().as_slice() != observation.wram.as_slice()
            || endpoint != observation.decoded
            || target.is_dead() != observation.dead
        {
            return Err("shortened action did not reconstruct its full-action observation".into());
        }
        let action_observations = target.last_action_observations().to_vec();
        let mut max_watermark = BASELINE_WATERMARK;
        merge_watermark(&mut max_watermark, &action_observations);
        let candidate_snapshot = target
            .snapshot()
            .ok_or("failed to snapshot a reconstructed event prefix")?;
        let snapshot_sha256 = sha256_json(&candidate_snapshot)?;
        let mut candidate_trace = source_trace.clone();
        hash_action_trace(
            &mut candidate_trace,
            SOURCE_ACTIONS,
            shortened_action,
            &action_observations,
        )?;
        let (probe_attempts, probe_survived) = run_viability_probe(target, &candidate_snapshot)?;
        target.restore(&candidate_snapshot)?;
        let restored_observation = target.observe();
        let restored_endpoint = smb_mechanical_state_from_wram(target.wram());
        if restored_observation.frame_count != absolute_frames
            || restored_observation.wram != observation.wram
            || restored_observation.decoded != observation.decoded
            || target.wram().as_slice() != observation.wram.as_slice()
            || restored_endpoint != observation.decoded
            || target.is_dead() != observation.dead
            || target.exit_kind() != ExitKind::Ok
        {
            return Err("candidate restore after viability probing did not reconcile".into());
        }
        let suffix = [shortened_action];
        candidates.push(CandidateEvidence {
            event_offset,
            full_action: recipe.full_chord,
            full_chord_sha256: recipe.full_chord_sha256.clone(),
            shortened_action,
            matched_observation: observation,
            candidate_sha256: candidate_sha256(source, &suffix)?,
            suffix_sha256: sha256_json(&suffix)?,
            requested_frames: event_offset,
            actual_frames,
            absolute_frames,
            frames_since_restore: actual_frames,
            work_frames,
            death: false,
            failure: false,
            endpoint,
            endpoint_watermark: watermark(endpoint),
            max_watermark,
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256,
            trace_sha256: finish_sha256(candidate_trace),
            probe_attempts,
            probe_survived,
        });
    }
    Ok(StreamEvidence {
        record: "stream_evidence",
        stream: recipe.stream,
        source_index: recipe.source_index,
        full_chord_sha256: recipe.full_chord_sha256.clone(),
        full,
        candidates,
    })
}

fn validate_restore(target: &SmbTarget) -> Result<(), Box<dyn Error>> {
    if target.observe().frame_count != SOURCE_FRAMES
        || target.is_dead()
        || target.exit_kind() != ExitKind::Ok
    {
        return Err("restored target does not match the registered C119 endpoint".into());
    }
    Ok(())
}

fn qualifying_event_offsets(
    observations: &[SmbObservations],
    full_endpoint_frame: u64,
    requested_hold: u8,
) -> Result<Vec<(u64, SmbObservations)>, Box<dyn Error>> {
    let mut first_at_offset = BTreeMap::new();
    for observation in observations {
        if observation.frame_count <= SOURCE_FRAMES
            || observation.frame_count >= full_endpoint_frame
            || observation.dead
            || watermark(observation.decoded) <= BASELINE_WATERMARK
        {
            continue;
        }
        let offset = observation
            .frame_count
            .checked_sub(SOURCE_FRAMES)
            .ok_or("observer event precedes the registered source")?;
        if offset == 0 || offset >= u64::from(requested_hold) {
            return Err("qualified observer offset is outside the full action".into());
        }
        first_at_offset
            .entry(offset)
            .or_insert_with(|| observation.clone());
    }
    Ok(first_at_offset.into_iter().collect())
}

fn run_viability_probe(
    target: &mut SmbTarget,
    candidate: &SmbSnapshot,
) -> Result<(Vec<ProbeAttempt>, bool), Box<dyn Error>> {
    let mut attempts = Vec::with_capacity(VIABILITY_MASKS.len());
    for mask in VIABILITY_MASKS {
        target.restore(candidate)?;
        let work_before = target.frames_clocked();
        let survived = target.survives_probe(mask, VIABILITY_FRAMES);
        let work_frames = target
            .frames_clocked()
            .checked_sub(work_before)
            .ok_or("probe work counter moved backwards")?;
        let failure = target.exit_kind() != ExitKind::Ok;
        if failure {
            return Err("emulator failed during a viability probe".into());
        }
        if survived && work_frames != u64::from(VIABILITY_FRAMES) {
            return Err("surviving viability probe did not execute its full horizon".into());
        }
        attempts.push(ProbeAttempt {
            mask,
            work_frames,
            death: target.is_dead(),
            failure,
            survived,
        });
        if survived {
            return Ok((attempts, true));
        }
    }
    Ok((attempts, false))
}

fn classify_all(
    evidence: &[StreamEvidence],
    owners: &BTreeMap<String, usize>,
) -> Result<Vec<StreamClassification>, Box<dyn Error>> {
    if evidence.len() != STREAMS {
        return Err("evidence count does not match the registered streams".into());
    }
    evidence
        .iter()
        .map(|stream| {
            let canonical_owner = owners
                .get(&stream.full_chord_sha256)
                .is_some_and(|owner| *owner == stream.stream);
            classify_stream(stream, canonical_owner)
        })
        .collect()
}

fn classify_stream(
    stream: &StreamEvidence,
    canonical_owner: bool,
) -> Result<StreamClassification, Box<dyn Error>> {
    if stream
        .candidates
        .windows(2)
        .any(|pair| pair[0].event_offset >= pair[1].event_offset)
    {
        return Err("candidate offsets are not strictly increasing".into());
    }
    let atomic_live_deeper =
        !stream.full.death && stream.full.endpoint_watermark > BASELINE_WATERMARK;
    let atomic_transient_deeper = !stream.candidates.is_empty();
    let event_prefix_salvage = !atomic_live_deeper && atomic_transient_deeper;
    let credited_candidate = event_prefix_salvage
        .then(|| {
            stream
                .candidates
                .iter()
                .filter(|candidate| candidate.probe_survived)
                .min_by_key(|candidate| candidate.event_offset)
        })
        .flatten();
    let credited = credited_candidate.map(|candidate| CreditEvidence {
        event_offset: candidate.event_offset,
        candidate_sha256: candidate.candidate_sha256.clone(),
        snapshot_sha256: candidate.snapshot_sha256.clone(),
        watermark: candidate.endpoint_watermark,
        absolute_frames: candidate.absolute_frames,
        frames_since_restore: candidate.frames_since_restore,
    });
    Ok(StreamClassification {
        stream: stream.stream,
        source_index: stream.source_index,
        full_chord_sha256: stream.full_chord_sha256.clone(),
        canonical_owner,
        atomic_live_deeper,
        atomic_transient_deeper,
        event_prefix_salvage,
        probe_surviving_salvage: credited.is_some(),
        probe_refusal: event_prefix_salvage && credited.is_none(),
        credited,
    })
}

#[derive(Serialize)]
struct ClassificationCore<'a> {
    atomic_live_deeper: bool,
    atomic_transient_deeper: bool,
    event_prefix_salvage: bool,
    probe_surviving_salvage: bool,
    probe_refusal: bool,
    credited: &'a Option<CreditEvidence>,
}

#[derive(Serialize)]
struct FullChordNormalized<'a> {
    full: &'a FullActionEvidence,
    candidates: &'a [CandidateEvidence],
    classification: ClassificationCore<'a>,
}

#[derive(Serialize)]
struct CandidateNormalized<'a> {
    event_offset: u64,
    shortened_action: ButtonChord,
    matched_observation: &'a SmbObservations,
    candidate_sha256: &'a str,
    suffix_sha256: &'a str,
    requested_frames: u64,
    actual_frames: u64,
    absolute_frames: u64,
    frames_since_restore: u64,
    work_frames: u64,
    death: bool,
    failure: bool,
    endpoint: SmbMechanicalState,
    endpoint_watermark: SmbProgressWatermark,
    max_watermark: SmbProgressWatermark,
    wram_sha256: &'a str,
    snapshot_sha256: &'a str,
    trace_sha256: &'a str,
    probe_attempts: &'a [ProbeAttempt],
    probe_survived: bool,
}

fn classification_core(classification: &StreamClassification) -> ClassificationCore<'_> {
    ClassificationCore {
        atomic_live_deeper: classification.atomic_live_deeper,
        atomic_transient_deeper: classification.atomic_transient_deeper,
        event_prefix_salvage: classification.event_prefix_salvage,
        probe_surviving_salvage: classification.probe_surviving_salvage,
        probe_refusal: classification.probe_refusal,
        credited: &classification.credited,
    }
}

fn candidate_normalized(candidate: &CandidateEvidence) -> CandidateNormalized<'_> {
    CandidateNormalized {
        event_offset: candidate.event_offset,
        shortened_action: candidate.shortened_action,
        matched_observation: &candidate.matched_observation,
        candidate_sha256: &candidate.candidate_sha256,
        suffix_sha256: &candidate.suffix_sha256,
        requested_frames: candidate.requested_frames,
        actual_frames: candidate.actual_frames,
        absolute_frames: candidate.absolute_frames,
        frames_since_restore: candidate.frames_since_restore,
        work_frames: candidate.work_frames,
        death: candidate.death,
        failure: candidate.failure,
        endpoint: candidate.endpoint,
        endpoint_watermark: candidate.endpoint_watermark,
        max_watermark: candidate.max_watermark,
        wram_sha256: &candidate.wram_sha256,
        snapshot_sha256: &candidate.snapshot_sha256,
        trace_sha256: &candidate.trace_sha256,
        probe_attempts: &candidate.probe_attempts,
        probe_survived: candidate.probe_survived,
    }
}

fn validate_duplicate_equivalence(
    evidence: &[StreamEvidence],
    classifications: &[StreamClassification],
) -> Result<(), Box<dyn Error>> {
    if evidence.len() != classifications.len() {
        return Err("evidence and classification lengths differ".into());
    }
    let mut full_groups: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for (stream, classification) in evidence.iter().zip(classifications) {
        if stream.stream != classification.stream {
            return Err("evidence and classification stream order differs".into());
        }
        let normalized = serde_json::to_vec(&FullChordNormalized {
            full: &stream.full,
            candidates: &stream.candidates,
            classification: classification_core(classification),
        })?;
        match full_groups.entry(&stream.full_chord_sha256) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(normalized);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                if entry.get() != &normalized {
                    return Err(
                        "duplicate full chords produced different normalized evidence".into(),
                    );
                }
            }
        }
    }

    let mut candidate_groups: BTreeMap<&str, Vec<u8>> = BTreeMap::new();
    for candidate in evidence.iter().flat_map(|stream| &stream.candidates) {
        let normalized = serde_json::to_vec(&candidate_normalized(candidate))?;
        match candidate_groups.entry(&candidate.candidate_sha256) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(normalized);
            }
            std::collections::btree_map::Entry::Occupied(entry) => {
                if entry.get() != &normalized {
                    return Err(
                        "identical candidate hashes produced different normalized evidence".into(),
                    );
                }
            }
        }
    }
    Ok(())
}

struct SummaryData {
    verdict: Verdict,
    raw: ClassificationCounts,
    canonical: ClassificationCounts,
    distinct_full_chord_hashes: usize,
    distinct_candidate_hashes: usize,
    distinct_snapshot_hashes: usize,
    credited: Vec<CreditEvidence>,
    full_action_requested_frames: u64,
    full_action_work_frames: u64,
    candidate_requested_frames: u64,
    candidate_work_frames: u64,
    probe_work_frames: u64,
}

fn summarize(
    evidence: &[StreamEvidence],
    classifications: &[StreamClassification],
) -> Result<SummaryData, Box<dyn Error>> {
    let raw = count_classifications(classifications.iter())?;
    let canonical = count_classifications(
        classifications
            .iter()
            .filter(|classification| classification.canonical_owner),
    )?;
    let canonical_salvages = classifications
        .iter()
        .filter(|classification| {
            classification.canonical_owner && classification.probe_surviving_salvage
        })
        .collect::<Vec<_>>();
    let full_chord_hashes = canonical_salvages
        .iter()
        .map(|classification| classification.full_chord_sha256.clone())
        .collect::<BTreeSet<_>>();
    let candidate_hashes = canonical_salvages
        .iter()
        .filter_map(|classification| {
            classification
                .credited
                .as_ref()
                .map(|credit| credit.candidate_sha256.clone())
        })
        .collect::<BTreeSet<_>>();
    let snapshot_hashes = canonical_salvages
        .iter()
        .filter_map(|classification| {
            classification
                .credited
                .as_ref()
                .map(|credit| credit.snapshot_sha256.clone())
        })
        .collect::<BTreeSet<_>>();
    let credited = canonical_salvages
        .iter()
        .filter_map(|classification| classification.credited.clone())
        .collect::<Vec<_>>();
    let verdict = decide_verdict(
        canonical_salvages.len(),
        full_chord_hashes.len(),
        candidate_hashes.len(),
        snapshot_hashes.len(),
    );

    let mut full_action_requested_frames = 0_u64;
    let mut full_action_work_frames = 0_u64;
    let mut candidate_requested_frames = 0_u64;
    let mut candidate_work_frames = 0_u64;
    let mut probe_work_frames = 0_u64;
    for stream in evidence {
        full_action_requested_frames = full_action_requested_frames
            .checked_add(stream.full.requested_frames)
            .ok_or("full-action requested-frame total overflow")?;
        full_action_work_frames = full_action_work_frames
            .checked_add(stream.full.work_frames)
            .ok_or("full-action work total overflow")?;
        for candidate in &stream.candidates {
            candidate_requested_frames = candidate_requested_frames
                .checked_add(candidate.requested_frames)
                .ok_or("candidate requested-frame total overflow")?;
            candidate_work_frames = candidate_work_frames
                .checked_add(candidate.work_frames)
                .ok_or("candidate work total overflow")?;
            for probe in &candidate.probe_attempts {
                probe_work_frames = probe_work_frames
                    .checked_add(probe.work_frames)
                    .ok_or("probe work total overflow")?;
            }
        }
    }
    if full_action_work_frames > MAX_FULL_ACTION_WORK
        || candidate_requested_frames > MAX_CANDIDATE_WORK
        || candidate_work_frames > MAX_CANDIDATE_WORK
        || probe_work_frames > MAX_PROBE_WORK
    {
        return Err("canary emulator work exceeded a preregistered cap".into());
    }
    Ok(SummaryData {
        verdict,
        raw,
        canonical,
        distinct_full_chord_hashes: full_chord_hashes.len(),
        distinct_candidate_hashes: candidate_hashes.len(),
        distinct_snapshot_hashes: snapshot_hashes.len(),
        credited,
        full_action_requested_frames,
        full_action_work_frames,
        candidate_requested_frames,
        candidate_work_frames,
        probe_work_frames,
    })
}

fn count_classifications<'a>(
    classifications: impl Iterator<Item = &'a StreamClassification>,
) -> Result<ClassificationCounts, Box<dyn Error>> {
    let mut counts = ClassificationCounts::default();
    for classification in classifications {
        checked_increment(
            &mut counts.atomic_live_deeper,
            classification.atomic_live_deeper,
        )?;
        checked_increment(
            &mut counts.atomic_transient_deeper,
            classification.atomic_transient_deeper,
        )?;
        checked_increment(
            &mut counts.event_prefix_salvage,
            classification.event_prefix_salvage,
        )?;
        checked_increment(
            &mut counts.probe_surviving_salvage,
            classification.probe_surviving_salvage,
        )?;
        checked_increment(&mut counts.probe_refusal, classification.probe_refusal)?;
    }
    Ok(counts)
}

fn checked_increment(count: &mut usize, condition: bool) -> Result<(), Box<dyn Error>> {
    *count = count
        .checked_add(usize::from(condition))
        .ok_or("classification count overflow")?;
    Ok(())
}

fn decide_verdict(
    canonical_salvages: usize,
    distinct_full_chords: usize,
    distinct_candidates: usize,
    distinct_snapshots: usize,
) -> Verdict {
    if canonical_salvages == 0 {
        Verdict::Stop
    } else if distinct_full_chords >= 2 && distinct_candidates >= 2 && distinct_snapshots >= 2 {
        Verdict::Go
    } else {
        Verdict::Inconclusive
    }
}

fn candidate_sha256(source: &SmbInput, suffix: &[ButtonChord]) -> Result<String, Box<dyn Error>> {
    let capacity = source
        .actions
        .len()
        .checked_add(suffix.len())
        .ok_or("candidate action capacity overflow")?;
    let mut actions = Vec::new();
    actions.try_reserve_exact(capacity)?;
    actions.extend_from_slice(&source.actions);
    actions.extend_from_slice(suffix);
    sha256_json(&SmbInput { actions })
}

fn watermark(state: SmbMechanicalState) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: state.world,
        level: state.level,
        progress: state.progress,
    }
}

fn merge_watermark(maximum: &mut SmbProgressWatermark, observations: &[SmbObservations]) {
    for observation in observations {
        *maximum = (*maximum).max(watermark(observation.decoded));
    }
}

fn hash_action_trace(
    trace: &mut Sha256,
    index: usize,
    action: ButtonChord,
    observations: &[SmbObservations],
) -> Result<(), Box<dyn Error>> {
    trace.update(u64::try_from(index)?.to_le_bytes());
    hash_framed_json(trace, &action)?;
    hash_framed_json(trace, &observations)?;
    Ok(())
}

fn hash_framed_json<T: Serialize>(hasher: &mut Sha256, value: &T) -> Result<(), Box<dyn Error>> {
    let bytes = serde_json::to_vec(value)?;
    hasher.update(u64::try_from(bytes.len())?.to_le_bytes());
    hasher.update(bytes);
    Ok(())
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

fn read_bounded(path: &Path, maximum: usize, label: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let limit = u64::try_from(maximum)?
        .checked_add(1)
        .ok_or("bounded read limit overflow")?;
    let mut reader = fs::File::open(path)?.take(limit);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(format!("{label} exceeds its byte bound").into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use fuzzer::smb::target::SmbMilestones;

    use super::*;

    fn synthetic_source() -> SmbInput {
        SmbInput {
            actions: (0..SOURCE_ACTIONS)
                .map(|index| ButtonChord::new((index % 256) as u8, (index % 120 + 1) as u8))
                .collect(),
        }
    }

    fn state(progress: u16) -> SmbMechanicalState {
        SmbMechanicalState {
            world: 7,
            level: 0,
            progress,
            ..SmbMechanicalState::default()
        }
    }

    fn observation(offset: u64, progress: u16, dead: bool, marker: u8) -> SmbObservations {
        SmbObservations {
            frame_count: SOURCE_FRAMES + offset,
            wram: vec![marker; 2 * 1_024],
            decoded: state(progress),
            milestones: SmbMilestones::default(),
            changed_indices: vec![u16::from(marker)],
            dead,
            log_line: format!("observation-{marker}"),
        }
    }

    fn candidate(
        full_action: ButtonChord,
        full_chord_sha256: &str,
        event_offset: u64,
        progress: u16,
        probe_survived: bool,
        candidate_sha256: &str,
        snapshot_sha256: &str,
    ) -> CandidateEvidence {
        let shortened_action = ButtonChord::new(full_action.buttons, event_offset as u8);
        CandidateEvidence {
            event_offset,
            full_action,
            full_chord_sha256: full_chord_sha256.to_owned(),
            shortened_action,
            matched_observation: observation(event_offset, progress, false, event_offset as u8),
            candidate_sha256: candidate_sha256.to_owned(),
            suffix_sha256: format!("suffix-{candidate_sha256}"),
            requested_frames: event_offset,
            actual_frames: event_offset,
            absolute_frames: SOURCE_FRAMES + event_offset,
            frames_since_restore: event_offset,
            work_frames: event_offset,
            death: false,
            failure: false,
            endpoint: state(progress),
            endpoint_watermark: watermark(state(progress)),
            max_watermark: watermark(state(progress)),
            wram_sha256: format!("wram-{candidate_sha256}"),
            snapshot_sha256: snapshot_sha256.to_owned(),
            trace_sha256: format!("trace-{candidate_sha256}"),
            probe_attempts: vec![ProbeAttempt {
                mask: 0,
                work_frames: u64::from(VIABILITY_FRAMES),
                death: !probe_survived,
                failure: false,
                survived: probe_survived,
            }],
            probe_survived,
        }
    }

    fn evidence(
        stream: usize,
        full_action: ButtonChord,
        full_chord_sha256: &str,
        endpoint_progress: u16,
        candidates: Vec<CandidateEvidence>,
    ) -> StreamEvidence {
        StreamEvidence {
            record: "stream_evidence",
            stream,
            source_index: stream + 100,
            full_chord_sha256: full_chord_sha256.to_owned(),
            full: FullActionEvidence {
                action: full_action,
                candidate_sha256: format!("full-candidate-{full_chord_sha256}"),
                suffix_sha256: format!("full-suffix-{full_chord_sha256}"),
                action_observations: Vec::new(),
                requested_frames: u64::from(full_action.hold_frames),
                actual_frames: u64::from(full_action.hold_frames),
                absolute_frames: SOURCE_FRAMES + u64::from(full_action.hold_frames),
                frames_since_restore: u64::from(full_action.hold_frames),
                work_frames: u64::from(full_action.hold_frames),
                death: false,
                failure: false,
                endpoint: state(endpoint_progress),
                endpoint_watermark: watermark(state(endpoint_progress)),
                max_watermark: watermark(state(endpoint_progress)),
                wram_sha256: format!("full-wram-{full_chord_sha256}"),
                snapshot_sha256: format!("full-snapshot-{full_chord_sha256}"),
                trace_sha256: format!("full-trace-{full_chord_sha256}"),
            },
            candidates,
        }
    }

    #[test]
    fn exact_recipe_bytes_are_reproducible_bounded_and_complete() {
        let source = synthetic_source();
        let (first_actions, first_indices, first) =
            derive_recipes(&source).expect("derive first recipe set");
        let (second_actions, second_indices, second) =
            derive_recipes(&source).expect("derive second recipe set");
        assert_eq!(first_actions, second_actions);
        assert_eq!(first_indices, second_indices);
        assert_eq!(first, second);
        assert_eq!(first_actions.len(), STREAMS);
        assert!(
            first_actions
                .iter()
                .all(|actions| actions.len() == ACTIONS_PER_STREAM)
        );
        assert_eq!(
            &first_indices[0][..12],
            [
                2750, 1970, 865, 2554, 3272, 749, 3032, 758, 969, 2363, 299, 1657
            ]
        );
        assert_eq!(&first_indices[1][..6], [3194, 2310, 2742, 1192, 760, 536]);
        assert_eq!(
            sha256_json(&first_actions).expect("hash synthetic recipe set"),
            "378d81cb4778d2f7aac9c2e50b97f49c56baeecdc28310d9f3ddd80182ffdf1f"
        );
        let identities = first
            .iter()
            .map(|recipe| {
                (
                    recipe.stream as u64,
                    recipe.source_index as u64,
                    recipe.full_chord,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            sha256_json(&identities).expect("hash synthetic canary recipes"),
            "fef71f35b890010aa392599f23a61a66655818fae3f19f7d7d12a8d320cfa811"
        );
    }

    #[test]
    fn event_offsets_are_interior_alive_deeper_first_occurrences() {
        let first_two = observation(2, 237, false, 2);
        let duplicate_two = observation(2, 238, false, 22);
        let observations = vec![
            observation(0, 237, false, 0),
            observation(1, 236, false, 1),
            first_two.clone(),
            duplicate_two,
            observation(3, 239, true, 3),
            observation(4, 240, false, 4),
            observation(5, 241, false, 5),
        ];
        let offsets = qualifying_event_offsets(&observations, SOURCE_FRAMES + 5, 6)
            .expect("classify event offsets");
        assert_eq!(
            offsets
                .iter()
                .map(|(offset, _)| *offset)
                .collect::<Vec<_>>(),
            [2, 4]
        );
        assert_eq!(offsets[0].1, first_two);
    }

    #[test]
    fn classification_credits_earliest_probe_surviving_salvage() {
        let full_action = ButtonChord::new(0x81, 8);
        let candidates = vec![
            candidate(
                full_action,
                "chord-a",
                2,
                237,
                false,
                "candidate-2",
                "snapshot-2",
            ),
            candidate(
                full_action,
                "chord-a",
                4,
                240,
                true,
                "candidate-4",
                "snapshot-4",
            ),
        ];
        let classification =
            classify_stream(&evidence(0, full_action, "chord-a", 236, candidates), true)
                .expect("classify salvage");
        assert!(classification.atomic_transient_deeper);
        assert!(classification.event_prefix_salvage);
        assert!(classification.probe_surviving_salvage);
        assert!(!classification.probe_refusal);
        assert_eq!(
            classification
                .credited
                .as_ref()
                .expect("credited candidate")
                .event_offset,
            4
        );
        assert_eq!(
            count_classifications(std::iter::once(&classification)).expect("count classification"),
            ClassificationCounts {
                atomic_live_deeper: 0,
                atomic_transient_deeper: 1,
                event_prefix_salvage: 1,
                probe_surviving_salvage: 1,
                probe_refusal: 0,
            }
        );
        let mut saturated = usize::MAX;
        assert!(checked_increment(&mut saturated, true).is_err());
        assert_eq!(saturated, usize::MAX);

        let atomic = classify_stream(
            &evidence(
                1,
                full_action,
                "chord-b",
                237,
                vec![candidate(
                    full_action,
                    "chord-b",
                    2,
                    237,
                    true,
                    "atomic-candidate",
                    "atomic-snapshot",
                )],
            ),
            true,
        )
        .expect("classify atomic result");
        assert!(atomic.atomic_live_deeper);
        assert!(!atomic.event_prefix_salvage);
        assert!(!atomic.probe_surviving_salvage);
        assert!(atomic.credited.is_none());
    }

    #[test]
    fn duplicate_normalization_checks_full_chords_and_candidate_states() {
        let full_action = ButtonChord::new(0x01, 8);
        let original = evidence(
            0,
            full_action,
            "same-full",
            236,
            vec![candidate(
                full_action,
                "same-full",
                2,
                237,
                true,
                "same-candidate",
                "same-snapshot",
            )],
        );
        let mut duplicate = original.clone();
        duplicate.stream = 1;
        duplicate.source_index = 999;
        let classifications = vec![
            classify_stream(&original, true).expect("classify original"),
            classify_stream(&duplicate, false).expect("classify duplicate"),
        ];
        validate_duplicate_equivalence(&[original.clone(), duplicate], &classifications)
            .expect("equivalent duplicate full chords");

        let other_full_action = ButtonChord::new(0x01, 12);
        let mut same_candidate = evidence(
            2,
            other_full_action,
            "different-full",
            236,
            vec![candidate(
                other_full_action,
                "different-full",
                2,
                237,
                true,
                "same-candidate",
                "same-snapshot",
            )],
        );
        same_candidate.candidates[0].shortened_action = original.candidates[0].shortened_action;
        same_candidate.candidates[0].suffix_sha256 = original.candidates[0].suffix_sha256.clone();
        same_candidate.candidates[0].matched_observation =
            original.candidates[0].matched_observation.clone();
        same_candidate.candidates[0].wram_sha256 = original.candidates[0].wram_sha256.clone();
        same_candidate.candidates[0].trace_sha256 = original.candidates[0].trace_sha256.clone();
        let candidate_group = vec![original.clone(), same_candidate.clone()];
        let candidate_classifications = candidate_group
            .iter()
            .enumerate()
            .map(|(index, stream)| classify_stream(stream, index == 0))
            .collect::<Result<Vec<_>, _>>()
            .expect("classify candidate group");
        validate_duplicate_equivalence(&candidate_group, &candidate_classifications)
            .expect("candidate normalization excludes full-chord identity");

        same_candidate.candidates[0].snapshot_sha256 = "different-snapshot".to_owned();
        let mismatched = vec![original, same_candidate];
        let mismatched_classifications = mismatched
            .iter()
            .enumerate()
            .map(|(index, stream)| classify_stream(stream, index == 0))
            .collect::<Result<Vec<_>, _>>()
            .expect("classify mismatched candidate group");
        assert!(validate_duplicate_equivalence(&mismatched, &mismatched_classifications).is_err());
    }

    #[test]
    fn frozen_gate_distinguishes_stop_inconclusive_and_go() {
        assert_eq!(decide_verdict(0, 0, 0, 0), Verdict::Stop);
        assert_eq!(decide_verdict(1, 1, 1, 1), Verdict::Inconclusive);
        assert_eq!(decide_verdict(2, 2, 2, 1), Verdict::Inconclusive);
        assert_eq!(decide_verdict(2, 2, 2, 2), Verdict::Go);
        assert_eq!(
            serde_json::to_string(&Verdict::Go).expect("serialize GO"),
            "\"GO\""
        );
    }

    #[test]
    fn worker_replies_are_consumed_in_canonical_order() {
        assert_eq!(worker_for_stream(0), 0);
        assert_eq!(worker_for_stream(11), 11);
        assert_eq!(worker_for_stream(12), 0);
        let replies = vec![
            EvaluationReply {
                stream: 2,
                evaluation: Ok("two"),
            },
            EvaluationReply {
                stream: 0,
                evaluation: Ok("zero"),
            },
            EvaluationReply {
                stream: 1,
                evaluation: Ok("one"),
            },
        ];
        assert_eq!(
            consume_replies(replies, 3).expect("consume ordered replies"),
            ["zero", "one", "two"]
        );

        let setups = vec![
            WorkerSetupReply {
                worker: 2,
                setup_work_frames: Ok(EXPECTED_SETUP_WORK_FRAMES),
            },
            WorkerSetupReply {
                worker: 0,
                setup_work_frames: Ok(EXPECTED_SETUP_WORK_FRAMES),
            },
            WorkerSetupReply {
                worker: 1,
                setup_work_frames: Ok(EXPECTED_SETUP_WORK_FRAMES),
            },
        ];
        assert_eq!(
            consume_worker_setups(setups, 3).expect("consume ordered setup replies"),
            [EXPECTED_SETUP_WORK_FRAMES; 3]
        );

        let accounting = validate_setup_work_frames(
            EXPECTED_SETUP_WORK_FRAMES,
            &[EXPECTED_SETUP_WORK_FRAMES; WORKERS],
        )
        .expect("validate identical setup work");
        assert_eq!(accounting.per_target, EXPECTED_SETUP_WORK_FRAMES);
        assert_eq!(accounting.target_count, WORKERS + 1);
        assert_eq!(accounting.total, 4_693);
        assert!(
            validate_setup_work_frames(
                EXPECTED_SETUP_WORK_FRAMES,
                &[EXPECTED_SETUP_WORK_FRAMES - 1; WORKERS]
            )
            .is_err()
        );
    }

    #[test]
    fn worker_errors_surface_in_canonical_stream_order() {
        let replies = vec![
            EvaluationReply::<()> {
                stream: 2,
                evaluation: Err("higher".to_owned()),
            },
            EvaluationReply {
                stream: 1,
                evaluation: Err("lower".to_owned()),
            },
            EvaluationReply {
                stream: 0,
                evaluation: Ok(()),
            },
        ];
        assert_eq!(
            consume_replies(replies, 3).expect_err("lowest failing stream must win"),
            "stream 1: lower"
        );

        let setup_replies = vec![
            WorkerSetupReply {
                worker: 2,
                setup_work_frames: Err("higher".to_owned()),
            },
            WorkerSetupReply {
                worker: 1,
                setup_work_frames: Err("lower".to_owned()),
            },
            WorkerSetupReply {
                worker: 0,
                setup_work_frames: Ok(EXPECTED_SETUP_WORK_FRAMES),
            },
        ];
        assert_eq!(
            consume_worker_setups(setup_replies, 3).expect_err("lowest failing worker must win"),
            "worker 1: lower"
        );
    }
}
