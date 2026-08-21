// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealed, standalone paired rollout-continuity census at the C119 SMB frontier.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
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

const FORMAT: &str = "smb-rollout-horizon-census-v1";
const PREREGISTRATION: &str = "experiments/smb-completion/SOL-ROLLOUT-HORIZON-CENSUS.md@234cf6b1";
const CODE_BASE: &str = "14605677";
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
const SOURCE_ACTIONS: usize = 3_297;
const SOURCE_FRAMES: u64 = 155_148;
const STREAMS: usize = 100;
const ACTIONS_PER_STREAM: usize = 32;
const HORIZONS: [usize; 5] = [2, 4, 8, 16, 32];
const ELIGIBLE_HORIZONS: [usize; 4] = [4, 8, 16, 32];
const MASTER_SEED: u64 = 17_009_187_366_200_191_184;
const RECIPE_DOMAIN: &[u8] = b"rollout-corpus-index";
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
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
struct CensusConfig {
    policy: &'static str,
    master_seed: u64,
    streams: usize,
    actions_per_stream: usize,
    horizons: [usize; HORIZONS.len()],
    execution_schedule: &'static str,
    sampler: &'static str,
    progress_order: &'static str,
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
    config_sha256: &'a str,
    recipe_sha256: &'a str,
    config: &'a CensusConfig,
}

#[derive(Clone, Debug, Serialize)]
struct BaselineRecord {
    record: &'static str,
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

struct Baseline {
    record: BaselineRecord,
    snapshot: SmbSnapshot,
    trace: Sha256,
}

#[derive(Clone, Debug, Serialize)]
struct PairRecipeIdentity {
    horizon: usize,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
struct RecipeStream {
    stream: usize,
    source_indices: Vec<usize>,
    actions: Vec<ButtonChord>,
    pair_recipes: Vec<PairRecipeIdentity>,
}

#[derive(Debug, Serialize)]
struct RecipeRecord<'a> {
    record: &'static str,
    recipe_sha256: &'a str,
    streams: &'a [RecipeStream],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Branch {
    Continuous,
    MidpointReset,
}

#[derive(Clone, Debug, Serialize)]
struct BoundaryRecord {
    record: &'static str,
    branch: Branch,
    stream: usize,
    horizon: Option<usize>,
    midpoint: Option<usize>,
    stream_action: usize,
    source_corpus_index: usize,
    action: ButtonChord,
    candidate_sha256: String,
    suffix_sha256: String,
    requested_branch_hold_frames: u64,
    matched_prefix_hold_frames: u64,
    matched_total_hold_frames: u64,
    actual_action_frames: u64,
    absolute_frames: u64,
    frames_since_restore: u64,
    executed_logical_actions: usize,
    death: bool,
    endpoint: SmbMechanicalState,
    endpoint_watermark: SmbProgressWatermark,
    max_observed_watermark: SmbProgressWatermark,
    observer_deeper_through_boundary: bool,
    live_endpoint_deeper: bool,
    wram_sha256: String,
    snapshot_sha256: String,
    trace_sha256: String,
}

#[derive(Clone, Debug)]
struct StreamEvidence {
    continuous: Vec<BoundaryRecord>,
    resets: BTreeMap<usize, Vec<BoundaryRecord>>,
}

#[derive(Clone, Debug, Serialize)]
struct CreditRecord {
    stream: usize,
    action: usize,
    candidate_sha256: String,
    watermark: SmbProgressWatermark,
    absolute_frames: u64,
    frames_since_restore: u64,
}

#[derive(Clone, Debug, Serialize)]
struct PairOutcome {
    horizon: usize,
    midpoint: usize,
    stream: usize,
    pair_recipe_sha256: String,
    canonical_recipe_owner: bool,
    eligible: bool,
    continuity_live_deeper: bool,
    reset_live_deeper: bool,
    both_live_deeper: bool,
    continuity_win: bool,
    reset_win: bool,
    continuous_alive_through_horizon: bool,
    reset_alive_through_band: bool,
    continuity_evidence: Option<CreditRecord>,
    reset_evidence: Option<CreditRecord>,
}

#[derive(Clone, Debug, Serialize)]
struct HorizonSummary {
    horizon: usize,
    midpoint: usize,
    raw_pairs: usize,
    raw_eligible: usize,
    raw_continuity_live_deeper: usize,
    raw_reset_live_deeper: usize,
    raw_both_live_deeper: usize,
    raw_continuity_wins: usize,
    raw_reset_wins: usize,
    continuous_alive_through_horizon: usize,
    reset_alive_through_band: usize,
    canonical_pair_recipes: usize,
    canonical_eligible: usize,
    continuity_wins: usize,
    reset_wins: usize,
    discordant_pairs: usize,
    sign_tail_numerator: String,
    sign_denominator: String,
    passes_adjusted_gate: bool,
}

#[derive(Debug, Serialize)]
struct SummaryRecord<'a> {
    record: &'static str,
    body_sha256: String,
    decision: &'static str,
    h_star: Option<usize>,
    continuous_work_frames: u64,
    reset_work_frames: u64,
    unique_continuity_candidate_hashes: usize,
    unique_reset_candidate_hashes: usize,
    horizons: Vec<HorizonSummary>,
    pairs: &'a [PairOutcome],
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

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-rollout-horizon-census <input.json> <output.jsonl>")?,
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

    // Recipes are sealed before ROM bytes are read or an emulator is constructed.
    let recipes = derive_recipes(&source)?;
    let recipe_actions = recipes
        .iter()
        .map(|recipe| recipe.actions.clone())
        .collect::<Vec<_>>();
    let recipe_sha256 = sha256_json(&recipe_actions)?;
    let canonical_owners = canonical_recipe_owners(&recipes)?;

    let config = CensusConfig {
        policy: "artifact_marginal_continuous_vs_midpoint_reset_v1",
        master_seed: MASTER_SEED,
        streams: STREAMS,
        actions_per_stream: ACTIONS_PER_STREAM,
        horizons: HORIZONS,
        execution_schedule: "serial_stream_then_continuous_then_ascending_reset_horizons_v1",
        sampler: "sha256_modulo_source_action_count_with_replacement_v1",
        progress_order: "smb_progress_watermark_derived_ord_v1",
        gate: "paired_sign_exact_bonferroni_1_over_80_v1",
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

    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut target, &source)?;

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
        config_sha256: &config_sha256,
        recipe_sha256: &recipe_sha256,
        config: &config,
    })?;
    output.write(&baseline.record)?;
    output.write(&RecipeRecord {
        record: "recipes",
        recipe_sha256: &recipe_sha256,
        streams: &recipes,
    })?;

    let mut all_evidence = Vec::with_capacity(STREAMS);
    let mut continuous_work_frames = 0_u64;
    let mut reset_work_frames = 0_u64;
    for recipe in &recipes {
        let continuous = execute_branch(
            &mut target,
            &source,
            recipe,
            &baseline,
            BranchSpec::continuous(),
            &mut output,
        )?;
        continuous_work_frames = continuous_work_frames
            .checked_add(branch_work(&continuous))
            .ok_or("continuous work total overflow")?;

        let mut resets = BTreeMap::new();
        for horizon in HORIZONS {
            let reset = execute_branch(
                &mut target,
                &source,
                recipe,
                &baseline,
                BranchSpec::reset(horizon)?,
                &mut output,
            )?;
            reset_work_frames = reset_work_frames
                .checked_add(branch_work(&reset))
                .ok_or("reset work total overflow")?;
            if resets.insert(horizon, reset).is_some() {
                return Err("duplicate reset horizon".into());
            }
        }
        all_evidence.push(StreamEvidence { continuous, resets });
    }

    let pairs = classify_pairs(&recipes, &all_evidence, &canonical_owners)?;
    let horizons = summarize_horizons(&pairs)?;
    let h_star = horizons
        .iter()
        .find(|summary| {
            ELIGIBLE_HORIZONS.contains(&summary.horizon) && summary.passes_adjusted_gate
        })
        .map(|summary| summary.horizon);
    let unique_continuity_candidate_hashes = unique_candidate_hashes(&pairs, true);
    let unique_reset_candidate_hashes = unique_candidate_hashes(&pairs, false);
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        decision: if h_star.is_some() { "GO" } else { "STOP" },
        h_star,
        continuous_work_frames,
        reset_work_frames,
        unique_continuity_candidate_hashes,
        unique_reset_candidate_hashes,
        horizons,
        pairs: &pairs,
    };
    output.write(&summary)?;
    let report_sha256 = output.finish()?;
    println!(
        "{{\"report_sha256\":\"{report_sha256}\",\"decision\":\"{}\",\"h_star\":{}}}",
        summary.decision,
        summary
            .h_star
            .map_or_else(|| "null".to_owned(), |horizon| horizon.to_string())
    );
    Ok(())
}

#[derive(Clone, Copy)]
struct BranchSpec {
    branch: Branch,
    start: usize,
    end: usize,
    horizon: Option<usize>,
    midpoint: Option<usize>,
}

impl BranchSpec {
    fn continuous() -> Self {
        Self {
            branch: Branch::Continuous,
            start: 0,
            end: ACTIONS_PER_STREAM,
            horizon: None,
            midpoint: None,
        }
    }

    fn reset(horizon: usize) -> Result<Self, Box<dyn Error>> {
        if !HORIZONS.contains(&horizon) || !horizon.is_multiple_of(2) {
            return Err("invalid registered reset horizon".into());
        }
        let midpoint = horizon / 2;
        Ok(Self {
            branch: Branch::MidpointReset,
            start: midpoint,
            end: horizon,
            horizon: Some(horizon),
            midpoint: Some(midpoint),
        })
    }
}

fn derive_recipes(source: &SmbInput) -> Result<Vec<RecipeStream>, Box<dyn Error>> {
    let source_len_u64 = u64::try_from(source.actions.len())?;
    if source_len_u64 == 0 {
        return Err("cannot sample an empty source input".into());
    }
    let mut recipes = Vec::with_capacity(STREAMS);
    for stream in 0..STREAMS {
        let mut indices = Vec::with_capacity(ACTIONS_PER_STREAM);
        let mut actions = Vec::with_capacity(ACTIONS_PER_STREAM);
        for action in 0..ACTIONS_PER_STREAM {
            let mut hasher = Sha256::new();
            hasher.update(MASTER_SEED.to_le_bytes());
            hasher.update(RECIPE_DOMAIN);
            hasher.update(u64::try_from(stream)?.to_le_bytes());
            hasher.update(u64::try_from(action)?.to_le_bytes());
            let digest = hasher.finalize();
            let first = digest
                .get(..8)
                .ok_or("SHA-256 digest is unexpectedly short")?;
            let bytes: [u8; 8] = first.try_into()?;
            let index = usize::try_from(u64::from_le_bytes(bytes) % source_len_u64)?;
            let chord = *source
                .actions
                .get(index)
                .ok_or("derived source index is out of bounds")?;
            indices.push(index);
            actions.push(chord);
        }
        let pair_recipes = HORIZONS
            .into_iter()
            .map(|horizon| {
                let prefix = actions
                    .get(..horizon)
                    .ok_or("registered horizon exceeds a recipe")?;
                Ok(PairRecipeIdentity {
                    horizon,
                    sha256: sha256_json(prefix)?,
                })
            })
            .collect::<Result<Vec<_>, Box<dyn Error>>>()?;
        recipes.push(RecipeStream {
            stream,
            source_indices: indices,
            actions,
            pair_recipes,
        });
    }
    Ok(recipes)
}

fn canonical_recipe_owners(
    recipes: &[RecipeStream],
) -> Result<BTreeMap<usize, BTreeMap<String, usize>>, Box<dyn Error>> {
    let mut by_horizon = HORIZONS
        .into_iter()
        .map(|horizon| (horizon, BTreeMap::new()))
        .collect::<BTreeMap<_, _>>();
    for recipe in recipes {
        for pair in &recipe.pair_recipes {
            by_horizon
                .get_mut(&pair.horizon)
                .ok_or("recipe names an unknown horizon")?
                .entry(pair.sha256.clone())
                .or_insert(recipe.stream);
        }
    }
    Ok(by_horizon)
}

fn build_baseline(target: &mut SmbTarget, source: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
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

fn execute_branch(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipe: &RecipeStream,
    baseline: &Baseline,
    spec: BranchSpec,
    output: &mut NdjsonOutput,
) -> Result<Vec<BoundaryRecord>, Box<dyn Error>> {
    let actions = recipe
        .actions
        .get(spec.start..spec.end)
        .ok_or("branch range exceeds recipe")?;
    let indices = recipe
        .source_indices
        .get(spec.start..spec.end)
        .ok_or("branch source-index range exceeds recipe")?;
    target.restore(&baseline.snapshot)?;
    if target.observe().frame_count != SOURCE_FRAMES
        || target.is_dead()
        || target.exit_kind() != ExitKind::Ok
    {
        return Err("restored endpoint does not match the registered live frame".into());
    }
    let work_before = target.frames_clocked();
    let matched_prefix_hold_frames = recipe
        .actions
        .get(..spec.start)
        .ok_or("matched prefix exceeds recipe")?
        .iter()
        .try_fold(0_u64, |total, action| {
            total
                .checked_add(u64::from(action.hold_frames))
                .ok_or("matched prefix hold sum overflow")
        })?;
    let mut requested_branch_hold_frames = 0_u64;
    let mut max_observed_watermark = BASELINE_WATERMARK;
    let mut observer_deeper = false;
    let mut trace = baseline.trace.clone();
    let mut records = Vec::with_capacity(actions.len());

    for (offset, (action, source_index)) in actions.iter().zip(indices).enumerate() {
        let stream_index = spec
            .start
            .checked_add(offset)
            .ok_or("stream action index overflow")?;
        let stream_action = stream_index
            .checked_add(1)
            .ok_or("one-based stream action overflow")?;
        let prior_absolute_frames = target.observe().frame_count;
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed during rollout branch".into());
        }
        let logical_offset = stream_index
            .checked_sub(spec.start)
            .ok_or("logical branch offset moved backwards")?;
        let logical_index = source
            .actions
            .len()
            .checked_add(logical_offset)
            .ok_or("logical candidate index overflow")?;
        hash_action_trace(
            &mut trace,
            logical_index,
            *action,
            target.last_action_observations(),
        )?;
        merge_watermark(
            &mut max_observed_watermark,
            target.last_action_observations(),
        );
        observer_deeper |= target
            .last_action_observations()
            .iter()
            .any(|observation| watermark(observation.decoded) > BASELINE_WATERMARK);
        requested_branch_hold_frames = requested_branch_hold_frames
            .checked_add(u64::from(action.hold_frames))
            .ok_or("branch hold sum overflow")?;
        let matched_total_hold_frames = matched_prefix_hold_frames
            .checked_add(requested_branch_hold_frames)
            .ok_or("matched hold sum overflow")?;
        let absolute_frames = target.observe().frame_count;
        let actual_action_frames = absolute_frames
            .checked_sub(prior_absolute_frames)
            .ok_or("action observation frame moved backwards")?;
        let frames_since_restore = absolute_frames
            .checked_sub(SOURCE_FRAMES)
            .ok_or("branch observation frame precedes restored source")?;
        let work_frames = target
            .frames_clocked()
            .checked_sub(work_before)
            .ok_or("branch work counter moved backwards")?;
        if frames_since_restore != work_frames
            || absolute_frames
                != SOURCE_FRAMES
                    .checked_add(frames_since_restore)
                    .ok_or("absolute branch frame overflow")?
        {
            return Err("branch frame accounting does not reconcile".into());
        }
        let suffix = recipe
            .actions
            .get(spec.start..=stream_index)
            .ok_or("candidate suffix range exceeds recipe")?;
        let endpoint = smb_mechanical_state_from_wram(target.wram());
        let endpoint_watermark = watermark(endpoint);
        let snapshot = target
            .snapshot()
            .ok_or("failed to snapshot rollout boundary")?;
        let death = target.is_dead();
        let record = BoundaryRecord {
            record: "boundary",
            branch: spec.branch,
            stream: recipe.stream,
            horizon: spec.horizon,
            midpoint: spec.midpoint,
            stream_action,
            source_corpus_index: *source_index,
            action: *action,
            candidate_sha256: candidate_sha256(source, suffix)?,
            suffix_sha256: sha256_json(suffix)?,
            requested_branch_hold_frames,
            matched_prefix_hold_frames,
            matched_total_hold_frames,
            actual_action_frames,
            absolute_frames,
            frames_since_restore,
            executed_logical_actions: source
                .actions
                .len()
                .checked_add(suffix.len())
                .ok_or("executed logical action count overflow")?,
            death,
            endpoint,
            endpoint_watermark,
            max_observed_watermark,
            observer_deeper_through_boundary: observer_deeper,
            live_endpoint_deeper: !death && endpoint_watermark > BASELINE_WATERMARK,
            wram_sha256: sha256_bytes(target.wram()),
            snapshot_sha256: sha256_json(&snapshot)?,
            trace_sha256: finish_sha256(trace.clone()),
        };
        output.write(&record)?;
        records.push(record);
        if death {
            break;
        }
    }
    Ok(records)
}

fn classify_pairs(
    recipes: &[RecipeStream],
    evidence: &[StreamEvidence],
    owners: &BTreeMap<usize, BTreeMap<String, usize>>,
) -> Result<Vec<PairOutcome>, Box<dyn Error>> {
    let mut outcomes = Vec::with_capacity(
        recipes
            .len()
            .checked_mul(HORIZONS.len())
            .ok_or("pair outcome capacity overflow")?,
    );
    for horizon in HORIZONS {
        let midpoint = horizon / 2;
        for recipe in recipes {
            let stream = evidence
                .get(recipe.stream)
                .ok_or("missing stream evidence")?;
            let reset = stream
                .resets
                .get(&horizon)
                .ok_or("missing reset evidence")?;
            let pair_hash = pair_recipe_hash(recipe, horizon)?;
            let canonical_recipe_owner = owners
                .get(&horizon)
                .and_then(|by_hash| by_hash.get(&pair_hash))
                .is_some_and(|owner| *owner == recipe.stream);
            let midpoint_boundary = stream
                .continuous
                .iter()
                .find(|boundary| boundary.stream_action == midpoint);
            let earlier_live_deeper = stream.continuous.iter().any(|boundary| {
                boundary.stream_action <= midpoint && boundary.live_endpoint_deeper
            });
            let eligible =
                midpoint_boundary.is_some_and(|boundary| !boundary.death) && !earlier_live_deeper;
            let continuity_evidence = eligible
                .then(|| first_live_deeper(&stream.continuous, midpoint, horizon))
                .flatten();
            let reset_evidence = eligible
                .then(|| first_live_deeper(reset, midpoint, horizon))
                .flatten();
            let continuity_live_deeper = continuity_evidence.is_some();
            let reset_live_deeper = reset_evidence.is_some();
            outcomes.push(PairOutcome {
                horizon,
                midpoint,
                stream: recipe.stream,
                pair_recipe_sha256: pair_hash,
                canonical_recipe_owner,
                eligible,
                continuity_live_deeper,
                reset_live_deeper,
                both_live_deeper: continuity_live_deeper && reset_live_deeper,
                continuity_win: continuity_live_deeper && !reset_live_deeper,
                reset_win: reset_live_deeper && !continuity_live_deeper,
                continuous_alive_through_horizon: boundary_alive_at(&stream.continuous, horizon),
                reset_alive_through_band: boundary_alive_at(reset, horizon),
                continuity_evidence,
                reset_evidence,
            });
        }
    }
    Ok(outcomes)
}

fn summarize_horizons(pairs: &[PairOutcome]) -> Result<Vec<HorizonSummary>, Box<dyn Error>> {
    HORIZONS
        .into_iter()
        .map(|horizon| {
            let selected = pairs
                .iter()
                .filter(|pair| pair.horizon == horizon)
                .collect::<Vec<_>>();
            let canonical = selected
                .iter()
                .copied()
                .filter(|pair| pair.canonical_recipe_owner)
                .collect::<Vec<_>>();
            let continuity_wins = canonical
                .iter()
                .filter(|pair| pair.eligible && pair.continuity_win)
                .count();
            let reset_wins = canonical
                .iter()
                .filter(|pair| pair.eligible && pair.reset_win)
                .count();
            let (tail, denominator) = exact_sign_tail(continuity_wins, reset_wins)?;
            let passes_adjusted_gate = ELIGIBLE_HORIZONS.contains(&horizon)
                && continuity_wins > reset_wins
                && tail
                    .checked_mul(80)
                    .ok_or("adjusted sign-test numerator overflow")?
                    <= denominator;
            Ok(HorizonSummary {
                horizon,
                midpoint: horizon / 2,
                raw_pairs: selected.len(),
                raw_eligible: selected.iter().filter(|pair| pair.eligible).count(),
                raw_continuity_live_deeper: selected
                    .iter()
                    .filter(|pair| pair.continuity_live_deeper)
                    .count(),
                raw_reset_live_deeper: selected
                    .iter()
                    .filter(|pair| pair.reset_live_deeper)
                    .count(),
                raw_both_live_deeper: selected.iter().filter(|pair| pair.both_live_deeper).count(),
                raw_continuity_wins: selected.iter().filter(|pair| pair.continuity_win).count(),
                raw_reset_wins: selected.iter().filter(|pair| pair.reset_win).count(),
                continuous_alive_through_horizon: selected
                    .iter()
                    .filter(|pair| pair.continuous_alive_through_horizon)
                    .count(),
                reset_alive_through_band: selected
                    .iter()
                    .filter(|pair| pair.reset_alive_through_band)
                    .count(),
                canonical_pair_recipes: canonical.len(),
                canonical_eligible: canonical.iter().filter(|pair| pair.eligible).count(),
                continuity_wins,
                reset_wins,
                discordant_pairs: continuity_wins
                    .checked_add(reset_wins)
                    .ok_or("discordant-pair count overflow")?,
                sign_tail_numerator: tail.to_string(),
                sign_denominator: denominator.to_string(),
                passes_adjusted_gate,
            })
        })
        .collect()
}

fn first_live_deeper(
    boundaries: &[BoundaryRecord],
    lower_exclusive: usize,
    upper_inclusive: usize,
) -> Option<CreditRecord> {
    boundaries
        .iter()
        .find(|boundary| {
            boundary.stream_action > lower_exclusive
                && boundary.stream_action <= upper_inclusive
                && boundary.live_endpoint_deeper
        })
        .map(|boundary| CreditRecord {
            stream: boundary.stream,
            action: boundary.stream_action,
            candidate_sha256: boundary.candidate_sha256.clone(),
            watermark: boundary.endpoint_watermark,
            absolute_frames: boundary.absolute_frames,
            frames_since_restore: boundary.frames_since_restore,
        })
}

fn boundary_alive_at(boundaries: &[BoundaryRecord], action: usize) -> bool {
    boundaries
        .iter()
        .find(|boundary| boundary.stream_action == action)
        .is_some_and(|boundary| !boundary.death)
}

fn pair_recipe_hash(recipe: &RecipeStream, horizon: usize) -> Result<String, Box<dyn Error>> {
    recipe
        .pair_recipes
        .iter()
        .find(|pair| pair.horizon == horizon)
        .map(|pair| pair.sha256.clone())
        .ok_or_else(|| "recipe is missing a registered horizon".into())
}

fn unique_candidate_hashes(pairs: &[PairOutcome], continuity: bool) -> usize {
    pairs
        .iter()
        .filter_map(|pair| {
            if continuity {
                pair.continuity_evidence.as_ref()
            } else {
                pair.reset_evidence.as_ref()
            }
        })
        .map(|credit| (credit.candidate_sha256.clone(), ()))
        .collect::<BTreeMap<_, _>>()
        .len()
}

fn exact_sign_tail(
    continuity_wins: usize,
    reset_wins: usize,
) -> Result<(u128, u128), Box<dyn Error>> {
    let trials = continuity_wins
        .checked_add(reset_wins)
        .ok_or("sign-test trial count overflow")?;
    if trials > STREAMS {
        return Err("sign-test trials exceed the frozen pair bound".into());
    }
    let mut row = vec![0_u128; trials.checked_add(1).ok_or("Pascal row overflow")?];
    if let Some(first) = row.first_mut() {
        *first = 1;
    }
    for n in 1..=trials {
        for k in (1..=n).rev() {
            row[k] = row[k]
                .checked_add(row[k - 1])
                .ok_or("binomial coefficient overflow")?;
        }
    }
    let tail = row
        .get(continuity_wins..)
        .ok_or("sign-test success count exceeds trials")?
        .iter()
        .try_fold(0_u128, |sum, value| {
            sum.checked_add(*value).ok_or("sign-test tail overflow")
        })?;
    let shift = u32::try_from(trials)?;
    let denominator = 1_u128
        .checked_shl(shift)
        .ok_or("sign-test denominator overflow")?;
    Ok((tail, denominator))
}

fn branch_work(boundaries: &[BoundaryRecord]) -> u64 {
    boundaries
        .last()
        .map_or(0, |boundary| boundary.frames_since_restore)
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
    use std::collections::BTreeMap;

    use super::{
        ACTIONS_PER_STREAM, BASELINE_WATERMARK, BoundaryRecord, Branch, ButtonChord, CreditRecord,
        HORIZONS, PairOutcome, PairRecipeIdentity, RecipeStream, SmbMechanicalState,
        SmbProgressWatermark, StreamEvidence, canonical_recipe_owners, classify_pairs,
        derive_recipes, exact_sign_tail, first_live_deeper, sha256_json, summarize_horizons,
    };

    fn source() -> super::SmbInput {
        super::SmbInput {
            actions: (0..super::SOURCE_ACTIONS)
                .map(|index| ButtonChord::new((index % 256) as u8, (index % 120 + 1) as u8))
                .collect(),
        }
    }

    #[test]
    fn recipes_are_reproducible_bounded_and_complete() {
        let first = derive_recipes(&source()).expect("derive first recipe set");
        let second = derive_recipes(&source()).expect("derive second recipe set");
        assert_eq!(first.len(), super::STREAMS);
        assert_eq!(first[0].actions.len(), ACTIONS_PER_STREAM);
        assert_eq!(
            serde_json::to_vec(&first[0].actions).expect("serialize first"),
            serde_json::to_vec(&second[0].actions).expect("serialize second")
        );
        assert!(
            first
                .iter()
                .flat_map(|recipe| &recipe.source_indices)
                .all(|index| *index < super::SOURCE_ACTIONS)
        );
        assert_eq!(
            &first[0].source_indices[..12],
            &[
                2750, 1970, 865, 2554, 3272, 749, 3032, 758, 969, 2363, 299, 1657
            ]
        );
        assert_eq!(
            &first[1].source_indices[..6],
            &[3194, 2310, 2742, 1192, 760, 536]
        );
        let all_actions = first
            .iter()
            .map(|recipe| recipe.actions.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            sha256_json(&all_actions).expect("hash synthetic recipe set"),
            "378d81cb4778d2f7aac9c2e50b97f49c56baeecdc28310d9f3ddd80182ffdf1f"
        );
    }

    #[test]
    fn canonical_pair_recipe_owner_is_first_stream() {
        let mut recipes = derive_recipes(&source()).expect("derive recipes");
        recipes[1].actions = recipes[0].actions.clone();
        recipes[1].pair_recipes = recipes[0].pair_recipes.clone();
        let owners = canonical_recipe_owners(&recipes).expect("canonical owners");
        for pair in &recipes[0].pair_recipes {
            assert_eq!(owners[&pair.horizon][&pair.sha256], 0);
        }
    }

    #[test]
    fn exact_sign_gate_examples_match_preregistration() {
        let (tail, denominator) = exact_sign_tail(7, 0).expect("7-0 tail");
        assert!(tail * 80 <= denominator);
        let (tail, denominator) = exact_sign_tail(6, 0).expect("6-0 tail");
        assert!(tail * 80 > denominator);
        let (tail, denominator) = exact_sign_tail(9, 1).expect("9-1 tail");
        assert!(tail * 80 <= denominator);
        let (tail, denominator) = exact_sign_tail(3, 2).expect("3-2 tail");
        assert!(tail * 80 > denominator);
    }

    #[test]
    fn branch_names_match_the_frozen_report_surface() {
        assert_eq!(
            serde_json::to_string(&Branch::Continuous).expect("serialize continuous"),
            "\"continuous\""
        );
        assert_eq!(
            serde_json::to_string(&Branch::MidpointReset).expect("serialize reset"),
            "\"midpoint_reset\""
        );
    }

    #[test]
    fn later_death_does_not_erase_first_live_deeper_boundary() {
        let live = boundary(0, Branch::Continuous, 3, false, true);
        let dead = boundary(0, Branch::Continuous, 4, true, false);
        let evidence = first_live_deeper(&[live, dead], 2, 4).expect("credited boundary");
        assert_eq!(evidence.action, 3);
    }

    #[test]
    fn classify_pairs_enforces_midpoint_and_open_closed_band_bounds() {
        let recipes = (0..4).map(recipe).collect::<Vec<_>>();
        let owners = canonical_recipe_owners(&recipes).expect("canonical owners");
        let evidence = vec![
            stream_evidence(0, Scenario::ContinuityOnly),
            stream_evidence(1, Scenario::DeeperAtMidpoint),
            stream_evidence(2, Scenario::DeadAtMidpoint),
            stream_evidence(3, Scenario::BothInBand),
        ];
        let pairs = classify_pairs(&recipes, &evidence, &owners).expect("classify pairs");
        let at = |stream| {
            pairs
                .iter()
                .find(|pair| pair.horizon == 4 && pair.stream == stream)
                .expect("horizon-four pair")
        };
        assert!(at(0).eligible);
        assert!(at(0).continuity_win);
        assert!(!at(0).reset_live_deeper);
        assert!(!at(1).eligible, "progress at M must exclude the band");
        assert!(!at(2).eligible, "death at M must exclude the band");
        assert!(at(3).eligible);
        assert!(at(3).both_live_deeper);
        assert!(!at(3).continuity_win);
        assert!(!at(3).reset_win);
    }

    #[test]
    fn summary_gate_uses_canonical_discordant_pairs_only() {
        let mut pairs = Vec::new();
        for stream in 0..7 {
            pairs.push(pair(stream, true, true, false));
        }
        pairs.push(pair(99, false, false, true));
        let summaries = summarize_horizons(&pairs).expect("summaries");
        let horizon = summaries
            .iter()
            .find(|summary| summary.horizon == 4)
            .expect("horizon four");
        assert_eq!(horizon.continuity_wins, 7);
        assert_eq!(horizon.reset_wins, 0);
        assert!(horizon.passes_adjusted_gate);
    }

    fn pair(stream: usize, canonical: bool, continuity_win: bool, reset_win: bool) -> PairOutcome {
        PairOutcome {
            horizon: 4,
            midpoint: 2,
            stream,
            pair_recipe_sha256: format!("pair-{stream}"),
            canonical_recipe_owner: canonical,
            eligible: true,
            continuity_live_deeper: continuity_win,
            reset_live_deeper: reset_win,
            both_live_deeper: false,
            continuity_win,
            reset_win,
            continuous_alive_through_horizon: true,
            reset_alive_through_band: true,
            continuity_evidence: continuity_win.then(|| credit(stream)),
            reset_evidence: reset_win.then(|| credit(stream)),
        }
    }

    fn credit(stream: usize) -> CreditRecord {
        CreditRecord {
            stream,
            action: 3,
            candidate_sha256: format!("candidate-{stream}"),
            watermark: SmbProgressWatermark {
                progress: BASELINE_WATERMARK.progress + 1,
                ..BASELINE_WATERMARK
            },
            absolute_frames: super::SOURCE_FRAMES + 1,
            frames_since_restore: 1,
        }
    }

    #[derive(Clone, Copy)]
    enum Scenario {
        ContinuityOnly,
        DeeperAtMidpoint,
        DeadAtMidpoint,
        BothInBand,
    }

    fn recipe(stream: usize) -> RecipeStream {
        let actions = vec![ButtonChord::new(stream as u8, 1); ACTIONS_PER_STREAM];
        let pair_recipes = HORIZONS
            .into_iter()
            .map(|horizon| PairRecipeIdentity {
                horizon,
                sha256: sha256_json(&actions[..horizon]).expect("hash pair recipe"),
            })
            .collect();
        RecipeStream {
            stream,
            source_indices: vec![stream; ACTIONS_PER_STREAM],
            actions,
            pair_recipes,
        }
    }

    fn stream_evidence(stream: usize, scenario: Scenario) -> StreamEvidence {
        let mut continuous = Vec::new();
        continuous.push(boundary(stream, Branch::Continuous, 1, false, false));
        continuous.push(boundary(
            stream,
            Branch::Continuous,
            2,
            matches!(scenario, Scenario::DeadAtMidpoint),
            matches!(scenario, Scenario::DeeperAtMidpoint),
        ));
        if !matches!(scenario, Scenario::DeadAtMidpoint) {
            continuous.push(boundary(
                stream,
                Branch::Continuous,
                3,
                false,
                matches!(scenario, Scenario::ContinuityOnly | Scenario::BothInBand),
            ));
            continuous.push(boundary(stream, Branch::Continuous, 4, false, false));
        }

        let mut resets = HORIZONS
            .into_iter()
            .map(|horizon| (horizon, Vec::new()))
            .collect::<BTreeMap<_, _>>();
        let reset_four = resets.get_mut(&4).expect("horizon four reset");
        // A synthetic action exactly at M proves that the lower bound is open.
        reset_four.push(boundary(
            stream,
            Branch::MidpointReset,
            2,
            false,
            matches!(scenario, Scenario::ContinuityOnly),
        ));
        reset_four.push(boundary(
            stream,
            Branch::MidpointReset,
            3,
            false,
            matches!(scenario, Scenario::BothInBand),
        ));
        reset_four.push(boundary(stream, Branch::MidpointReset, 4, false, false));
        StreamEvidence { continuous, resets }
    }

    fn boundary(
        stream: usize,
        branch: Branch,
        action: usize,
        death: bool,
        live_deeper: bool,
    ) -> BoundaryRecord {
        let endpoint_watermark = if live_deeper {
            SmbProgressWatermark {
                progress: BASELINE_WATERMARK.progress + 1,
                ..BASELINE_WATERMARK
            }
        } else {
            BASELINE_WATERMARK
        };
        BoundaryRecord {
            record: "boundary",
            branch,
            stream,
            horizon: None,
            midpoint: None,
            stream_action: action,
            source_corpus_index: 0,
            action: ButtonChord::new(0, 1),
            candidate_sha256: format!("candidate-{action}"),
            suffix_sha256: format!("suffix-{action}"),
            requested_branch_hold_frames: action as u64,
            matched_prefix_hold_frames: 0,
            matched_total_hold_frames: action as u64,
            actual_action_frames: 1,
            absolute_frames: super::SOURCE_FRAMES + action as u64,
            frames_since_restore: action as u64,
            executed_logical_actions: super::SOURCE_ACTIONS + action,
            death,
            endpoint: SmbMechanicalState {
                dead: death,
                progress: endpoint_watermark.progress,
                ..SmbMechanicalState::default()
            },
            endpoint_watermark,
            max_observed_watermark: endpoint_watermark,
            observer_deeper_through_boundary: live_deeper,
            live_endpoint_deeper: live_deeper && !death,
            wram_sha256: String::new(),
            snapshot_sha256: String::new(),
            trace_sha256: String::new(),
        }
    }

    #[allow(dead_code)]
    fn _assert_recipe_is_send(_: RecipeStream) {}
}
