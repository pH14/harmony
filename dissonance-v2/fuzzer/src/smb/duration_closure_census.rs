// SPDX-License-Identifier: AGPL-3.0-or-later

//! Sealed World 8-4 p73 source-mask duration-closure census.

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

const FORMAT: &str = "smb-w8-4-p73-source-mask-duration-closure-census-v1";
const PREREGISTRATION_COMMIT: &str = "6078a7c781de16b0bc75c152481cf158a5669ee3";
const PREREGISTRATION_DOC_SHA256: &str =
    "c7e8632179aa0c52d0a4a7f1da6b22a506973fa7f0b6fd9ae6d28217f9772e99";
const CODE_BASE: &str = "a6935d4c08dd72a176b1aa295ad73b63c19311c6";
const AUTHORIZING_P73_PREREGISTRATION: &str = "fbf2afb1";
const AUTHORIZING_P73_IMPLEMENTATION: &str = "c3902b4a";
const AUTHORIZING_P73_RESULT: &str = "fc62d470";
const AUTHORIZING_P73_REPORT_SHA256: &str =
    "5fc888c8fcb522b9b1216de9649223cebbddbf87709e68d1236a4e2031ff2e90";
const SOURCE_FILE_SHA256: &str = "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_INPUT_SHA256: &str =
    "d222d9ebc0126c52473a121e4143889ec92ee584cd53837a3461b0c6c2648a7c";
const SOURCE_WRAM_SHA256: &str = "bc051f742198e95efeb2e0392fc2c7cb72f0fd38dc4449247a0082eebe60e734";
const SOURCE_SNAPSHOT_SHA256: &str =
    "3620e6ed58f4853cc059b4daf7f2bc493ee61480abbdf84fb6dff5d26e670927";
const ROM_SHA256: &str = "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const EXPECTED_RECIPE_SHA256: &str =
    "e4c46f7a0f52dd5bcdb6e269f7a5afec18278acaa2d4285a87f5216a92b3b953";
const EXPECTED_RECIPE_BYTES: usize = 75_787;
const SOURCE_BYTES: usize = 114_128;
const SOURCE_ACTIONS: usize = 3_554;
const SOURCE_FRAMES: u64 = 167_340;
const CANDIDATES: usize = 1_680;
const WORKERS: usize = 12;
const ACTION_LIMIT: usize = 4_096;
const EXPECTED_SETUP_FRAMES: u64 = 361;
const MAX_SOURCE_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_ACTION_FRAMES: u64 = 101_640;
const MAX_PROBE_FRAMES: u64 = 226_800;
const SOURCE_PROBE_FRAMES: u64 = 45;
const MAX_TOTAL_FRAMES: u64 = 500_518;
const PROBE_MASKS: [u8; 3] = [0x00, 0x01, 0x81];
const PROBE_FRAMES: u16 = 45;
const TRACE_DOMAIN: &[u8] = b"smb-trace-canary-v1\0trace\0";
const CANDIDATE_TRACE_DOMAIN: &[u8] = b"smb-duration-closure-candidate-v1\0trace\0";
const SOURCE_MASKS: [u8; 14] = [0, 1, 2, 16, 32, 64, 66, 128, 129, 130, 131, 192, 193, 194];
const SOURCE_DURATIONS: [u8; 53] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 23, 26, 29, 36, 37, 44, 47, 49, 53, 54, 57, 62, 74, 79, 88,
    92, 95, 96, 97, 98, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 111, 112, 113,
    114, 115, 116, 117, 118, 119, 120,
];
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
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum SupportLabel {
    EmpiricalOccurrence,
    FactorialClosure,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct Recipe {
    ordinal: usize,
    mask: u8,
    duration: u8,
    action: ButtonChord,
    support: SupportLabel,
    duration_seen_in_source: bool,
}

#[derive(Debug, Serialize)]
struct Config {
    candidates: usize,
    workers: usize,
    action_limit: usize,
    source_masks: [u8; 14],
    source_durations: &'static [u8],
    duration_domain: &'static str,
    order: &'static str,
    support_label: &'static str,
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
struct CandidateRecord {
    record: &'static str,
    ordinal: usize,
    worker: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    worker_setup_frames: Option<u64>,
    mask: u8,
    duration: u8,
    action: ButtonChord,
    support: SupportLabel,
    duration_seen_in_source: bool,
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
    ordinal: usize,
    mask: u8,
    duration: u8,
    action: ButtonChord,
    support: SupportLabel,
    duration_seen_in_source: bool,
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
enum SupportVerdict {
    ExpandFactorialSupport,
    EmpiricalOccurrenceSufficient,
    InsufficientClosureEvidence,
    NoDirectAdvance,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct SupportClassification {
    record: &'static str,
    verdict: SupportVerdict,
    empirical_eligible: usize,
    closure_eligible: usize,
    closure_distinct_inputs: usize,
    closure_distinct_snapshots: usize,
    best_empirical: Option<ChampionRecord>,
    best_closure: Option<ChampionRecord>,
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
    authorizing_p73_preregistration: &'static str,
    authorizing_p73_implementation: &'static str,
    authorizing_p73_result: &'static str,
    authorizing_p73_report_sha256: &'static str,
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
    support_verdict: SupportVerdict,
    adoption_verdict: AdoptionVerdict,
    champion: Option<ChampionRecord>,
    worker_setup_frames: Vec<u64>,
    candidates: usize,
    setup_frames: u64,
    source_replay_frames: u64,
    source_probe_frames: u64,
    action_frames: u64,
    probe_frames: u64,
    experimental_frames: u64,
    total_frames: u64,
}

struct CandidateReply {
    ordinal: usize,
    worker: usize,
    result: Result<CandidateRecord, String>,
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

/// Run the sealed census from process arguments and environment.
pub fn run_from_process(
    bin_source: &'static [u8],
    module_source: &'static [u8],
) -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let source_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-w8-4-p73-duration-closure-census <input.json> <output.jsonl>")?,
    );
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
    let config = Config {
        candidates: CANDIDATES,
        workers: WORKERS,
        action_limit: ACTION_LIMIT,
        source_masks: SOURCE_MASKS,
        source_durations: &SOURCE_DURATIONS,
        duration_domain: "all_u8_hold_durations_1_through_120_v1",
        order: "source_mask_ascending_duration_ascending_v1",
        support_label: "exact_source_button_chord_membership_v1",
        execution: "independent_source_restore_one_action_probe_v1",
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
    let recipes = derive_recipes(&source)?;
    let recipe_bytes = recipe_identity_bytes(&recipes)?;
    let recipe_sha256 = sha256_bytes(&recipe_bytes);
    if recipe_bytes.len() != EXPECTED_RECIPE_BYTES || recipe_sha256 != EXPECTED_RECIPE_SHA256 {
        return Err("duration-closure recipe identity does not match the sealed oracle".into());
    }
    let candidates = evaluate_parallel(&rom, &source, &recipes, &baseline)?;
    let support = classify_support(&candidates)?;
    let adoption = classify_adoption(&candidates)?;
    let work = summarize_work(&candidates, &baseline.record)?;

    let mut output = NdjsonOutput::new(output_file);
    output.write(&HeaderRecord {
        record: "header",
        format: FORMAT,
        preregistration_commit: PREREGISTRATION_COMMIT,
        preregistration_doc_sha256: PREREGISTRATION_DOC_SHA256,
        code_base: CODE_BASE,
        authorizing_p73_preregistration: AUTHORIZING_P73_PREREGISTRATION,
        authorizing_p73_implementation: AUTHORIZING_P73_IMPLEMENTATION,
        authorizing_p73_result: AUTHORIZING_P73_RESULT,
        authorizing_p73_report_sha256: AUTHORIZING_P73_REPORT_SHA256,
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
    for candidate in &candidates {
        output.write(candidate)?;
    }
    output.write(&support)?;
    output.write(&adoption)?;
    let summary = SummaryRecord {
        record: "summary",
        body_sha256: output.digest(),
        support_verdict: support.verdict,
        adoption_verdict: adoption.verdict,
        champion: adoption.champion.clone(),
        worker_setup_frames: work.worker_setup_frames,
        candidates: candidates.len(),
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
        "{{\"report_sha256\":\"{report_sha256}\",\"support_verdict\":{},\"adoption_verdict\":{}}}",
        serde_json::to_string(&summary.support_verdict)?,
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
    let durations = source
        .actions
        .iter()
        .map(|action| action.hold_frames)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if masks.as_slice() != SOURCE_MASKS || durations.as_slice() != SOURCE_DURATIONS {
        return Err("source opaque support does not match the preregistration".into());
    }
    let empirical = source.actions.iter().copied().collect::<BTreeSet<_>>();
    let duration_set = SOURCE_DURATIONS.into_iter().collect::<BTreeSet<_>>();
    let mut recipes = Vec::with_capacity(CANDIDATES);
    for mask in SOURCE_MASKS {
        for duration in 1..=MAX_HOLD_FRAMES {
            let ordinal = recipes.len();
            let action = ButtonChord::new(mask, duration);
            recipes.push(Recipe {
                ordinal,
                mask,
                duration,
                action,
                support: if empirical.contains(&action) {
                    SupportLabel::EmpiricalOccurrence
                } else {
                    SupportLabel::FactorialClosure
                },
                duration_seen_in_source: duration_set.contains(&duration),
            });
        }
    }
    if recipes.len() != CANDIDATES
        || recipes
            .iter()
            .enumerate()
            .any(|(ordinal, recipe)| recipe.ordinal != ordinal)
    {
        return Err("duration-closure recipe order is not canonical".into());
    }
    Ok(recipes)
}

fn recipe_identity_bytes(recipes: &[Recipe]) -> Result<Vec<u8>, Box<dyn Error>> {
    let identity = recipes
        .iter()
        .map(|recipe| {
            Ok((
                u64::try_from(recipe.ordinal)?,
                recipe.mask,
                recipe.duration,
                recipe.action,
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
) -> Result<Vec<CandidateRecord>, Box<dyn Error>> {
    if recipes.len() != CANDIDATES {
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
                    .name(format!("duration-closure-{worker}"))
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
                        for ordinal in (worker..CANDIDATES).step_by(WORKERS) {
                            let result = if let Some(error) = prior_error.as_ref() {
                                Err(format!("worker unavailable after prior error: {error}"))
                            } else {
                                match target.as_mut() {
                                    Ok(target) => match recipes.get(ordinal) {
                                        Some(recipe) => run_candidate(
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
                            let _ = sender.send(CandidateReply {
                                ordinal,
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
                .map_err(|_| "duration-closure worker panicked")?;
        }
        Ok(())
    })?;
    consume_replies(receiver.into_iter().collect())
}

fn consume_replies(replies: Vec<CandidateReply>) -> Result<Vec<CandidateRecord>, Box<dyn Error>> {
    let mut buffered = BTreeMap::new();
    let mut metadata_errors = Vec::new();
    for reply in replies {
        if reply.ordinal >= CANDIDATES || reply.worker != reply.ordinal % WORKERS {
            metadata_errors.push((0_u8, reply.ordinal, reply.worker, "invalid"));
        } else if buffered.insert(reply.ordinal, reply.result).is_some() {
            metadata_errors.push((1_u8, reply.ordinal, reply.worker, "duplicate"));
        }
    }
    for ordinal in 0..CANDIDATES {
        if !buffered.contains_key(&ordinal) {
            metadata_errors.push((2_u8, ordinal, ordinal % WORKERS, "missing"));
        }
    }
    metadata_errors.sort_unstable();
    if let Some((_, ordinal, worker, kind)) = metadata_errors.first() {
        return Err(format!("{kind} candidate reply: ordinal={ordinal}, worker={worker}").into());
    }
    let mut candidates = Vec::with_capacity(CANDIDATES);
    for ordinal in 0..CANDIDATES {
        candidates.push(
            buffered
                .remove(&ordinal)
                .ok_or("missing candidate reply")?
                .map_err(|error| format!("candidate {ordinal}: {error}"))?,
        );
    }
    Ok(candidates)
}

fn run_candidate(
    target: &mut SmbTarget,
    source: &SmbInput,
    recipe: &Recipe,
    baseline: &Baseline,
    worker: usize,
) -> Result<CandidateRecord, Box<dyn Error>> {
    if recipe.ordinal >= CANDIDATES || worker != recipe.ordinal % WORKERS {
        return Err("candidate worker ownership is not canonical".into());
    }
    target.restore(&baseline.snapshot)?;
    verify_snapshot(target, &baseline.snapshot)?;
    let before = target.frames_clocked();
    target.apply(&recipe.action);
    let action_frames = target
        .frames_clocked()
        .checked_sub(before)
        .ok_or("candidate action work moved backwards")?;
    if target.exit_kind() != ExitKind::Ok {
        return Err("emulator failed during duration-closure action".into());
    }
    let dead = target.is_dead();
    let requested_frames = u64::from(recipe.action.bounded_hold_frames());
    if action_frames > requested_frames || (!dead && action_frames != requested_frames) {
        return Err("candidate action work does not match its duration".into());
    }
    let observation = target.observe();
    let mechanical = smb_mechanical_state_from_wram(target.wram());
    let mut transient_maximum = BASELINE_WATERMARK;
    merge_progress_watermark(&mut transient_maximum, target.last_action_observations());
    let mut milestones = BASELINE_MILESTONES;
    merge_action_milestones(&mut milestones, target)?;
    let input = appended_input(source, recipe.action)?;
    let input_sha256 = sha256_json(&input)?;
    let wram_sha256 = sha256_bytes(target.wram());
    let mut trace = Sha256::new();
    trace.update(CANDIDATE_TRACE_DOMAIN);
    trace.update(u64::try_from(recipe.ordinal)?.to_le_bytes());
    hash_framed_json(&mut trace, &recipe.action)?;
    hash_framed_json(&mut trace, target.last_action_observations())?;
    let mut snapshot_sha256 = None;
    let mut key = None;
    let mut probe = Vec::new();
    let mut probe_survived = false;
    let mut probe_frames = 0_u64;
    if !dead {
        let snapshot = target
            .snapshot()
            .ok_or("failed to snapshot candidate endpoint")?;
        snapshot_sha256 = Some(sha256_json(&snapshot)?);
        key = Some(archive_key(target.wram(), SmbArchiveKeyPolicy::Frozen));
        let result = run_probe(target, &snapshot)?;
        probe = result.0;
        probe_survived = result.1;
        probe_frames = result.2;
    }
    let total_work_frames = target
        .frames_clocked()
        .checked_sub(before)
        .ok_or("candidate total work moved backwards")?;
    if total_work_frames
        != action_frames
            .checked_add(probe_frames)
            .ok_or("candidate work overflow")?
    {
        return Err("candidate work does not reconcile".into());
    }
    Ok(CandidateRecord {
        record: "candidate",
        ordinal: recipe.ordinal,
        worker,
        worker_setup_frames: (recipe.ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES),
        mask: recipe.mask,
        duration: recipe.duration,
        action: recipe.action,
        support: recipe.support,
        duration_seen_in_source: recipe.duration_seen_in_source,
        input_actions: input.actions.len(),
        input_sha256,
        observation,
        mechanical,
        watermark: watermark(mechanical),
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
        input,
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

fn eligible(candidate: &CandidateRecord) -> bool {
    !candidate.dead
        && !candidate.failed
        && candidate.probe_survived
        && candidate.watermark > BASELINE_WATERMARK
        && candidate.snapshot_sha256.is_some()
        && candidate.key.is_some()
}

fn rank(mut candidates: Vec<&CandidateRecord>) -> Result<Option<ChampionRecord>, Box<dyn Error>> {
    candidates.sort_by(|left, right| {
        right
            .watermark
            .cmp(&left.watermark)
            .then_with(|| left.input_sha256.cmp(&right.input_sha256))
            .then_with(|| left.ordinal.cmp(&right.ordinal))
    });
    candidates
        .first()
        .map(|candidate| champion(candidate))
        .transpose()
}

fn champion(candidate: &CandidateRecord) -> Result<ChampionRecord, Box<dyn Error>> {
    Ok(ChampionRecord {
        ordinal: candidate.ordinal,
        mask: candidate.mask,
        duration: candidate.duration,
        action: candidate.action,
        support: candidate.support,
        duration_seen_in_source: candidate.duration_seen_in_source,
        input: candidate.input.clone(),
        input_sha256: candidate.input_sha256.clone(),
        observation: candidate.observation.clone(),
        mechanical: candidate.mechanical,
        watermark: candidate.watermark,
        wram_sha256: candidate.wram_sha256.clone(),
        snapshot_sha256: candidate
            .snapshot_sha256
            .clone()
            .ok_or("eligible champion lacks snapshot identity")?,
        key: candidate.key.ok_or("eligible champion lacks Frozen key")?,
        milestones: candidate.milestones,
        action_frames: candidate.action_frames,
        probe: candidate.probe.clone(),
        probe_frames: candidate.probe_frames,
        total_work_frames: candidate.total_work_frames,
    })
}

fn classify_adoption(
    candidates: &[CandidateRecord],
) -> Result<AdoptionClassification, Box<dyn Error>> {
    validate_candidates(candidates)?;
    let eligible_candidates = candidates
        .iter()
        .filter(|candidate| eligible(candidate))
        .count();
    let champion = rank(
        candidates
            .iter()
            .filter(|candidate| eligible(candidate))
            .collect(),
    )?;
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

fn classify_support(
    candidates: &[CandidateRecord],
) -> Result<SupportClassification, Box<dyn Error>> {
    validate_candidates(candidates)?;
    let empirical = candidates
        .iter()
        .filter(|candidate| {
            eligible(candidate) && candidate.support == SupportLabel::EmpiricalOccurrence
        })
        .collect::<Vec<_>>();
    let closure = candidates
        .iter()
        .filter(|candidate| {
            eligible(candidate) && candidate.support == SupportLabel::FactorialClosure
        })
        .collect::<Vec<_>>();
    let closure_inputs = closure
        .iter()
        .map(|candidate| candidate.input_sha256.as_str())
        .collect::<BTreeSet<_>>();
    let closure_snapshots = closure
        .iter()
        .map(|candidate| candidate.snapshot_sha256.as_deref().unwrap_or_default())
        .collect::<BTreeSet<_>>();
    let best_empirical = rank(empirical.clone())?;
    let best_closure = rank(closure.clone())?;
    let verdict = support_verdict(
        empirical.len(),
        closure.len(),
        closure_inputs.len(),
        closure_snapshots.len(),
        best_empirical.as_ref().map(|candidate| candidate.watermark),
        best_closure.as_ref().map(|candidate| candidate.watermark),
    );
    Ok(SupportClassification {
        record: "support_classification",
        verdict,
        empirical_eligible: empirical.len(),
        closure_eligible: closure.len(),
        closure_distinct_inputs: closure_inputs.len(),
        closure_distinct_snapshots: closure_snapshots.len(),
        best_empirical,
        best_closure,
    })
}

fn support_verdict(
    empirical_eligible: usize,
    closure_eligible: usize,
    closure_distinct_inputs: usize,
    closure_distinct_snapshots: usize,
    best_empirical: Option<SmbProgressWatermark>,
    best_closure: Option<SmbProgressWatermark>,
) -> SupportVerdict {
    let empirical_floor = best_empirical.unwrap_or(BASELINE_WATERMARK);
    if closure_eligible >= 2
        && closure_distinct_inputs >= 2
        && closure_distinct_snapshots >= 2
        && best_closure.is_some_and(|watermark| watermark > empirical_floor)
    {
        SupportVerdict::ExpandFactorialSupport
    } else if empirical_eligible > 0 {
        SupportVerdict::EmpiricalOccurrenceSufficient
    } else if closure_eligible > 0 {
        SupportVerdict::InsufficientClosureEvidence
    } else {
        SupportVerdict::NoDirectAdvance
    }
}

fn validate_candidates(candidates: &[CandidateRecord]) -> Result<(), Box<dyn Error>> {
    if candidates.len() != CANDIDATES {
        return Err("candidate count does not match the preregistration".into());
    }
    for (ordinal, candidate) in candidates.iter().enumerate() {
        let mask = *SOURCE_MASKS
            .get(ordinal / usize::from(MAX_HOLD_FRAMES))
            .ok_or("candidate mask ordinal is out of range")?;
        let duration = u8::try_from(
            ordinal
                .checked_rem(usize::from(MAX_HOLD_FRAMES))
                .and_then(|value| value.checked_add(1))
                .ok_or("candidate duration ordinal overflow")?,
        )?;
        let action = ButtonChord::new(mask, duration);
        let support = if candidate
            .input
            .actions
            .get(..SOURCE_ACTIONS)
            .ok_or("candidate source prefix is truncated")?
            .contains(&action)
        {
            SupportLabel::EmpiricalOccurrence
        } else {
            SupportLabel::FactorialClosure
        };
        let probe_sum = candidate.probe.iter().try_fold(0_u64, |sum, attempt| {
            sum.checked_add(attempt.work_frames)
                .ok_or("candidate probe work overflow")
        })?;
        if candidate.ordinal != ordinal
            || candidate.worker != ordinal % WORKERS
            || candidate.worker_setup_frames != (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES)
            || candidate.mask != mask
            || candidate.duration != duration
            || candidate.action != action
            || candidate.support != support
            || candidate.duration_seen_in_source != SOURCE_DURATIONS.contains(&duration)
            || candidate.input_actions != SOURCE_ACTIONS + 1
            || candidate.input.actions.len() != candidate.input_actions
            || candidate.input.actions.last() != Some(&candidate.action)
            || sha256_json(&candidate.input)? != candidate.input_sha256
            || candidate.requested_frames != u64::from(duration)
            || candidate.action_frames > candidate.requested_frames
            || (!candidate.dead && candidate.action_frames != candidate.requested_frames)
            || candidate.probe_frames != probe_sum
            || candidate.total_work_frames
                != candidate
                    .action_frames
                    .checked_add(candidate.probe_frames)
                    .ok_or("candidate work overflow")?
            || candidate.failed
        {
            return Err("candidate identity or work is not canonical".into());
        }
        if candidate.dead {
            if candidate.snapshot_sha256.is_some()
                || candidate.key.is_some()
                || !candidate.probe.is_empty()
                || candidate.probe_survived
                || candidate.probe_frames != 0
            {
                return Err("terminal candidate contains live endpoint evidence".into());
            }
        } else {
            if candidate.snapshot_sha256.is_none()
                || candidate.key.is_none()
                || candidate.probe.is_empty()
                || candidate.probe.len() > PROBE_MASKS.len()
            {
                return Err("live candidate lacks canonical endpoint or probe evidence".into());
            }
            for (attempt, expected_mask) in candidate.probe.iter().zip(PROBE_MASKS) {
                if attempt.mask != expected_mask || attempt.work_frames > u64::from(PROBE_FRAMES) {
                    return Err("candidate probe attempt is not canonical".into());
                }
            }
            if candidate
                .probe
                .iter()
                .take(candidate.probe.len().saturating_sub(1))
                .any(|attempt| attempt.survived)
                || candidate.probe.last().map(|attempt| attempt.survived)
                    != Some(candidate.probe_survived)
            {
                return Err("candidate probe short-circuit is not canonical".into());
            }
        }
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
    candidates: &[CandidateRecord],
    baseline: &BaselineRecord,
) -> Result<WorkSummary, Box<dyn Error>> {
    validate_candidates(candidates)?;
    if baseline.setup_frames != EXPECTED_SETUP_FRAMES
        || baseline.replay_frames != SOURCE_FRAMES
        || baseline.source_probe.work_frames != SOURCE_PROBE_FRAMES
    {
        return Err("baseline work does not match the preregistration".into());
    }
    let action = candidates.iter().try_fold(0_u64, |sum, candidate| {
        sum.checked_add(candidate.action_frames)
            .ok_or("action work overflow")
    })?;
    let probe = candidates.iter().try_fold(0_u64, |sum, candidate| {
        sum.checked_add(candidate.probe_frames)
            .ok_or("probe work overflow")
    })?;
    let worker_setup_frames = candidates
        .iter()
        .filter_map(|candidate| candidate.worker_setup_frames)
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

fn appended_input(source: &SmbInput, action: ButtonChord) -> Result<SmbInput, Box<dyn Error>> {
    let capacity = source
        .actions
        .len()
        .checked_add(1)
        .ok_or("candidate input length overflow")?;
    if capacity > ACTION_LIMIT {
        return Err("candidate input exceeds the action limit".into());
    }
    let mut actions = Vec::with_capacity(capacity);
    actions.extend_from_slice(&source.actions);
    actions.push(action);
    Ok(SmbInput { actions })
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
        let mut actions = Vec::new();
        for mask in SOURCE_MASKS {
            for duration in SOURCE_DURATIONS {
                actions.push(ButtonChord::new(mask, duration));
            }
        }
        actions.resize(SOURCE_ACTIONS, ButtonChord::new(0, 2));
        actions[SOURCE_ACTIONS - 1] = BASELINE_FINAL_ACTION;
        SmbInput { actions }
    }

    fn candidate(
        ordinal: usize,
        support: SupportLabel,
        watermark: SmbProgressWatermark,
        snapshot: &str,
    ) -> CandidateRecord {
        let mechanical = SmbMechanicalState {
            world: watermark.world,
            level: watermark.level,
            progress: watermark.progress,
            ..SmbMechanicalState::default()
        };
        let action = ButtonChord::new(0, u8::try_from(ordinal % 120 + 1).expect("duration"));
        let mut input = synthetic_source();
        input.actions.push(action);
        CandidateRecord {
            record: "candidate",
            ordinal,
            worker: ordinal % WORKERS,
            worker_setup_frames: (ordinal < WORKERS).then_some(EXPECTED_SETUP_FRAMES),
            mask: action.buttons,
            duration: action.hold_frames,
            action,
            support,
            duration_seen_in_source: true,
            input_actions: SOURCE_ACTIONS + 1,
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
            watermark,
            transient_maximum: watermark,
            action_trace_sha256: String::new(),
            wram_sha256: String::new(),
            snapshot_sha256: Some(snapshot.to_owned()),
            key: Some(SmbArchiveKey {
                world: watermark.world,
                level: watermark.level,
                progress: watermark.progress,
                ..BASELINE_KEY
            }),
            milestones: BASELINE_MILESTONES,
            requested_frames: u64::from(action.hold_frames),
            action_frames: u64::from(action.hold_frames),
            dead: false,
            failed: false,
            probe: Vec::new(),
            probe_survived: true,
            probe_frames: 0,
            total_work_frames: u64::from(action.hold_frames),
            input,
        }
    }

    #[test]
    fn recipes_match_sealed_order_and_oracle() {
        let recipes = derive_recipes(&synthetic_source()).expect("recipes");
        assert_eq!(recipes.len(), CANDIDATES);
        assert_eq!(recipes[0].action, ButtonChord::new(0, 1));
        assert_eq!(recipes[0].support, SupportLabel::FactorialClosure);
        assert!(!recipes[0].duration_seen_in_source);
        assert_eq!(recipes[1].action, ButtonChord::new(0, 2));
        assert_eq!(recipes[1].support, SupportLabel::EmpiricalOccurrence);
        assert!(recipes[1].duration_seen_in_source);
        assert_eq!(recipes[12].action, ButtonChord::new(0, 13));
        assert_eq!(recipes[12].support, SupportLabel::FactorialClosure);
        assert!(!recipes[12].duration_seen_in_source);
        assert_eq!(recipes[CANDIDATES - 1].action, ButtonChord::new(194, 120));
        assert_eq!(
            recipe_identity_bytes(&recipes).expect("bytes").len(),
            EXPECTED_RECIPE_BYTES
        );
        assert_eq!(
            sha256_bytes(&recipe_identity_bytes(&recipes).expect("bytes")),
            EXPECTED_RECIPE_SHA256
        );
    }

    #[test]
    fn support_gate_and_adoption_ranking_are_orthogonal() {
        let strict = SmbProgressWatermark {
            progress: 74,
            ..BASELINE_WATERMARK
        };
        assert_eq!(
            support_verdict(0, 2, 2, 2, None, Some(strict)),
            SupportVerdict::ExpandFactorialSupport
        );
        assert_eq!(
            support_verdict(0, 2, 2, 1, None, Some(strict)),
            SupportVerdict::InsufficientClosureEvidence
        );
        assert_eq!(
            support_verdict(1, 2, 2, 2, Some(strict), Some(strict)),
            SupportVerdict::EmpiricalOccurrenceSufficient
        );
        assert_eq!(
            support_verdict(0, 0, 0, 0, None, None),
            SupportVerdict::NoDirectAdvance
        );
        let lower = candidate(
            0,
            SupportLabel::EmpiricalOccurrence,
            BASELINE_WATERMARK,
            "a",
        );
        let higher = candidate(1, SupportLabel::FactorialClosure, strict, "b");
        assert_eq!(
            rank(vec![&lower, &higher])
                .expect("rank")
                .expect("champion")
                .watermark,
            strict
        );
    }

    #[test]
    fn verdict_bytes_and_work_cap_are_frozen() {
        assert_eq!(
            serde_json::to_string(&SupportVerdict::ExpandFactorialSupport).expect("verdict"),
            r#""EXPAND_FACTORIAL_SUPPORT""#
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
        let make = |ordinal: usize| CandidateReply {
            ordinal,
            worker: ordinal % WORKERS,
            result: Err(format!("failure-{ordinal}")),
        };
        let ascending = (0..CANDIDATES).map(make).collect::<Vec<_>>();
        let descending = (0..CANDIDATES).rev().map(make).collect::<Vec<_>>();
        assert_eq!(
            consume_replies(ascending).expect_err("failure").to_string(),
            consume_replies(descending)
                .expect_err("failure")
                .to_string()
        );
    }
}
