// SPDX-License-Identifier: AGPL-3.0-or-later

//! Deterministic paired canary for generic, same-input SMB trace replacement.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    env,
    error::Error,
    fs::{self, OpenOptions},
    io::{BufWriter, Read, Write},
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use fuzzer::{
    search::sequence_edit::{ReplacementParameters, ReplacementRecipe, apply_replacement},
    smb::{
        archive::MAX_SMB_COMPLETION_ACTIONS,
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbInput, SmbMechanicalState, SmbObservations,
            SmbProgressWatermark, SmbSnapshot, SmbTarget, smb_mechanical_state_from_wram,
        },
    },
    target::Target,
};
use libafl::executors::ExitKind;
use serde::Serialize;
use sha2::{Digest, Sha256};

const FORMAT: &str = "smb-trace-canary-v1";
const DEFAULT_EDIT_HORIZON: usize = 256;
const SNAPSHOT_INTERVAL: usize = 32;
const LENGTH_ARMS: [usize; 5] = [4, 8, 16, 32, 64];
const MAX_PAIRED_DRAWS: usize = 100_000;
const MAX_INPUT_JSON_BYTES: usize = 2 * 1_024 * 1_024;
const MAX_ROM_BYTES: usize = 16 * 1_024 * 1_024;
const MAX_EXECUTABLE_BYTES: usize = 256 * 1_024 * 1_024;
const MAX_RECIPE_RETRIES: u64 = 256;

const REGISTERED_SEED: u64 = 9_829_488_526_003_250_479;
const REGISTERED_PAIRED_DRAWS: usize = 100;
const REGISTERED_PREREGISTRATION: &str =
    "experiments/smb-completion/SOL-COHERENT-REPLACEMENT-CANARY.md@363c3728";
const REGISTERED_SOURCE_ARCHIVE_SHA256: &str =
    "d9038c97f5a818f7c58e828e3621e1327a62d981f17d4a9246cd3238c3021c81";
const REGISTERED_SOURCE_ENTRY_ID: u64 = 48_076;
const REGISTERED_SOURCE_FILE_SHA256: &str =
    "5ae42e26a438ff03cbab449480ad4c26c929d6be7fbcee6787cd641601ed3159";
const REGISTERED_INPUT_SHA256: &str =
    "584de68aba576f0b20ebbfa8c03e520553dda308a1c0d6a2e876c924840d6fa1";
const REGISTERED_INPUT_ACTIONS: usize = 3_297;
const REGISTERED_ROM_SHA256: &str =
    "0b3d9e1f01ed1668205bab34d6c82b0e281456e137352e4f36a9b2cfa3b66dea";
const REGISTERED_BASELINE_FRAMES: u64 = 155_148;
const REGISTERED_SEGMENT_START: usize = 3_208;
const REGISTERED_SEGMENT_ACTIONS: usize = 89;
const REGISTERED_SEGMENT_FRAMES: u64 = 3_837;
const REGISTERED_BASELINE_FRONTIER: SmbProgressWatermark = SmbProgressWatermark {
    world: 7,
    level: 0,
    progress: 236,
};

#[derive(Clone, Debug, Serialize)]
struct CanaryConfig {
    policy: &'static str,
    trailing_edit_horizon: usize,
    snapshot_interval: usize,
    length_arms: [usize; LENGTH_ARMS.len()],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct EvaluationSummary {
    deterministic_frames: u64,
    evaluated_frames: u64,
    executed_actions: usize,
    snapshot_action: usize,
    snapshot_absolute_frames: u64,
    death: bool,
    failure: bool,
    max_mechanical_frontier: SmbProgressWatermark,
    endpoint: SmbMechanicalState,
}

#[derive(Clone, Debug, Serialize)]
struct BaselineSummary {
    trace_sha256: String,
    final_wram_sha256: String,
    final_snapshot_sha256: String,
    registered_segment: SegmentSummary,
    evaluation: EvaluationSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct SegmentSummary {
    first_action: usize,
    actions: usize,
    deterministic_frames: u64,
    death: bool,
    max_mechanical_frontier: SmbProgressWatermark,
}

#[derive(Debug, Serialize)]
struct HeaderRecord<'a> {
    record: &'static str,
    format: &'static str,
    seed: u64,
    paired_draws: usize,
    preregistration: &'static str,
    source_archive_sha256: &'static str,
    source_entry_id: u64,
    source_archive_identity: &'static str,
    source_path: String,
    source_file_sha256: &'a str,
    input_sha256: &'a str,
    input_actions: usize,
    rom_path: String,
    rom_sha256: &'a str,
    executable_path: String,
    executable_sha256: &'a str,
    config_sha256: &'a str,
    config: &'a CanaryConfig,
    arm_window_positions: Vec<(usize, usize)>,
    baseline: &'a BaselineSummary,
}

#[derive(Clone, Debug, Serialize)]
struct PairRecipe {
    retry: u64,
    length_arm: usize,
    recipient_offset: usize,
    donor_offset: usize,
    recipient_range: (usize, usize),
    donor_range: (usize, usize),
    donor_semantic_sha256: String,
    action_multiset_sha256: String,
    donor_total_hold_frames: u64,
    control_permutation: Vec<usize>,
    control_replacement_sha256: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct Discovery {
    eligible: bool,
    new_mechanical_cell: bool,
    cost_improvement: bool,
    prior_best_frames: Option<u64>,
}

#[derive(Clone, Debug, Serialize)]
struct CandidateRecord {
    candidate_sha256: String,
    trace_sha256: String,
    final_wram_sha256: String,
    final_snapshot_sha256: String,
    source_changed: bool,
    useful: bool,
    evaluation: EvaluationSummary,
    discovery: Discovery,
}

#[derive(Debug, Serialize)]
struct ArmRecord<'a> {
    record: &'static str,
    draw_index: usize,
    arm: &'static str,
    recipe: &'a PairRecipe,
    candidate: CandidateRecord,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
struct TreatmentSummary {
    evaluations: u64,
    deaths: u64,
    new_mechanical_cells: u64,
    cost_improvements: u64,
    useful: u64,
    final_mechanical_cells: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
struct ArmSummary {
    length_arm: usize,
    challenger: TreatmentSummary,
    control: TreatmentSummary,
}

#[derive(Debug, Serialize)]
struct SummaryRecord {
    record: &'static str,
    paired_draws: usize,
    body_sha256: String,
    arms: Vec<ArmSummary>,
}

struct PrefixCheckpoint {
    action_count: usize,
    snapshot: SmbSnapshot,
    trace: Sha256,
    max_frontier: SmbProgressWatermark,
    absolute_frames: u64,
}

struct Baseline {
    checkpoints: Vec<PrefixCheckpoint>,
    summary: BaselineSummary,
}

struct CandidateEvaluation {
    input_sha256: String,
    trace_sha256: String,
    final_wram_sha256: String,
    final_snapshot_sha256: String,
    source_changed: bool,
    useful: bool,
    summary: EvaluationSummary,
}

#[derive(Default)]
struct TreatmentState {
    best_frames: BTreeMap<SmbMechanicalState, u64>,
    summary: TreatmentSummary,
}

impl TreatmentState {
    fn with_baseline(baseline: &BaselineSummary) -> Self {
        let mut state = Self::default();
        if !baseline.evaluation.death && !baseline.evaluation.failure {
            state.best_frames.insert(
                baseline.evaluation.endpoint,
                baseline.evaluation.deterministic_frames,
            );
        }
        state
    }

    fn observe(&mut self, evaluation: &CandidateEvaluation) -> Discovery {
        self.summary.evaluations = self.summary.evaluations.saturating_add(1);
        self.summary.useful = self
            .summary
            .useful
            .saturating_add(u64::from(evaluation.useful));
        if evaluation.summary.death || evaluation.summary.failure {
            self.summary.deaths = self.summary.deaths.saturating_add(1);
            return Discovery::default();
        }
        let frames = evaluation.summary.deterministic_frames;
        match self.best_frames.entry(evaluation.summary.endpoint) {
            Entry::Vacant(slot) => {
                slot.insert(frames);
                self.summary.new_mechanical_cells =
                    self.summary.new_mechanical_cells.saturating_add(1);
                Discovery {
                    eligible: true,
                    new_mechanical_cell: true,
                    ..Discovery::default()
                }
            }
            Entry::Occupied(mut slot) => {
                let prior = *slot.get();
                let improved = frames < prior;
                if improved {
                    slot.insert(frames);
                    self.summary.cost_improvements =
                        self.summary.cost_improvements.saturating_add(1);
                }
                Discovery {
                    eligible: true,
                    cost_improvement: improved,
                    prior_best_frames: Some(prior),
                    ..Discovery::default()
                }
            }
        }
    }

    fn finish(mut self) -> TreatmentSummary {
        self.summary.final_mechanical_cells = self.best_frames.len();
        self.summary
    }
}

struct ArmState {
    length: usize,
    challenger: TreatmentState,
    control: TreatmentState,
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
    let source_path = PathBuf::from(args.next().ok_or(
        "usage: smb-trace-canary <input.json> <output.jsonl> <seed> <paired-draws> [trailing-edit-horizon]",
    )?);
    let output_path = PathBuf::from(args.next().ok_or("missing output NDJSON path")?);
    let seed = parse_u64(&args.next().ok_or("missing seed")?.to_string_lossy())?;
    let paired_draws = usize::try_from(parse_u64(
        &args
            .next()
            .ok_or("missing paired draw count")?
            .to_string_lossy(),
    )?)?;
    let trailing_edit_horizon = args
        .next()
        .map(|value| parse_u64(&value.to_string_lossy()))
        .transpose()?
        .map(usize::try_from)
        .transpose()?
        .unwrap_or(DEFAULT_EDIT_HORIZON);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    if paired_draws == 0 || paired_draws > MAX_PAIRED_DRAWS {
        return Err("paired draw count is outside its bounded range".into());
    }

    let source_bytes = read_bounded(&source_path, MAX_INPUT_JSON_BYTES, "input JSON")?;
    let source_file_sha256 = sha256_bytes(&source_bytes);
    let input: SmbInput = serde_json::from_slice(&source_bytes)?;
    validate_input(&input, trailing_edit_horizon)?;
    let input_sha256 = sha256_json(&input)?;

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = read_bounded(&rom_path, MAX_ROM_BYTES, "ROM")?;
    let rom_sha256 = sha256_bytes(&rom);
    let config = CanaryConfig {
        policy: "trailing_same_input_donor_vs_nonidentity_donor_multiset_shuffle",
        trailing_edit_horizon,
        snapshot_interval: SNAPSHOT_INTERVAL,
        length_arms: LENGTH_ARMS,
    };
    let config_sha256 = sha256_json(&config)?;
    let executable_path = env::current_exe()?;
    let executable = read_bounded(&executable_path, MAX_EXECUTABLE_BYTES, "current executable")?;
    let executable_sha256 = sha256_bytes(&executable);

    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let baseline = build_baseline(&mut target, &input)?;
    validate_registered_integrity(
        seed,
        paired_draws,
        trailing_edit_horizon,
        &source_file_sha256,
        &input_sha256,
        input.actions.len(),
        &rom_sha256,
        &baseline.summary,
    )?;
    let horizon_start = input
        .actions
        .len()
        .checked_sub(trailing_edit_horizon)
        .ok_or("edit horizon exceeds the input")?;
    let mut arms = build_arm_states(&baseline.summary);
    let replacement_parameters = replacement_parameters()?;

    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output_path)?;
    let mut output = NdjsonOutput::new(output_file);
    let header = HeaderRecord {
        record: "header",
        format: FORMAT,
        seed,
        paired_draws,
        preregistration: REGISTERED_PREREGISTRATION,
        source_archive_sha256: REGISTERED_SOURCE_ARCHIVE_SHA256,
        source_entry_id: REGISTERED_SOURCE_ENTRY_ID,
        source_archive_identity: "preregistered_provenance_not_reread_by_canary",
        source_path: source_path.to_string_lossy().into_owned(),
        source_file_sha256: &source_file_sha256,
        input_sha256: &input_sha256,
        input_actions: input.actions.len(),
        rom_path: rom_path.to_string_lossy().into_owned(),
        rom_sha256: &rom_sha256,
        executable_path: executable_path.to_string_lossy().into_owned(),
        executable_sha256: &executable_sha256,
        config_sha256: &config_sha256,
        config: &config,
        arm_window_positions: arms
            .iter()
            .map(|arm| {
                let positions = trailing_edit_horizon
                    .checked_sub(arm.length)
                    .and_then(|remaining| remaining.checked_add(1))
                    .unwrap_or(0);
                (arm.length, positions)
            })
            .collect(),
        baseline: &baseline.summary,
    };
    write_record(&mut output, &header)?;

    for draw in 0..paired_draws {
        let arm_index = draw % arms.len();
        let arm = arms
            .get_mut(arm_index)
            .ok_or("paired draw selected an unknown length arm")?;
        let recipe = draw_recipe(&input.actions, seed, draw, horizon_start, arm.length)?;
        let donor = checked_slice(&input.actions, recipe.donor_range)?.to_vec();
        let control_replacement = permute(&donor, &recipe.control_permutation)?;
        let challenger_input = apply_replacement(
            &input.actions,
            &[&input.actions],
            &ReplacementRecipe {
                length_index: arm_index,
                input_start: recipe.recipient_range.0,
                donor_index: 0,
                donor_start: recipe.donor_range.0,
            },
            &replacement_parameters,
        )?;
        let control_input = apply_replacement(
            &input.actions,
            &[&control_replacement],
            &ReplacementRecipe {
                length_index: arm_index,
                input_start: recipe.recipient_range.0,
                donor_index: 0,
                donor_start: 0,
            },
            &replacement_parameters,
        )?;
        validate_pair_materialization(
            &input.actions,
            &donor,
            &control_replacement,
            &recipe,
            &challenger_input,
            &control_input,
        )?;

        let challenger_evaluation = evaluate_candidate(
            &mut target,
            &baseline.checkpoints,
            &input.actions,
            &challenger_input,
            recipe.recipient_range.0,
            &baseline.summary,
        )?;
        let challenger_discovery = arm.challenger.observe(&challenger_evaluation);
        let challenger_record = ArmRecord {
            record: "candidate",
            draw_index: draw,
            arm: "coherent",
            recipe: &recipe,
            candidate: candidate_record(challenger_evaluation, challenger_discovery),
        };
        write_record(&mut output, &challenger_record)?;

        let control_evaluation = evaluate_candidate(
            &mut target,
            &baseline.checkpoints,
            &input.actions,
            &control_input,
            recipe.recipient_range.0,
            &baseline.summary,
        )?;
        let control_discovery = arm.control.observe(&control_evaluation);
        let control_record = ArmRecord {
            record: "candidate",
            draw_index: draw,
            arm: "shuffled_control",
            recipe: &recipe,
            candidate: candidate_record(control_evaluation, control_discovery),
        };
        write_record(&mut output, &control_record)?;
    }

    let summary = SummaryRecord {
        record: "summary",
        paired_draws,
        body_sha256: output.digest(),
        arms: arms
            .into_iter()
            .map(|arm| ArmSummary {
                length_arm: arm.length,
                challenger: arm.challenger.finish(),
                control: arm.control.finish(),
            })
            .collect(),
    };
    write_record(&mut output, &summary)?;
    let report_sha256 = output.finish()?;
    println!("{{\"report_sha256\":\"{report_sha256}\"}}");
    Ok(())
}

fn validate_input(input: &SmbInput, horizon: usize) -> Result<(), Box<dyn Error>> {
    let maximum_arm = *LENGTH_ARMS.last().ok_or("length arm table is empty")?;
    if input.actions.len() > MAX_SMB_COMPLETION_ACTIONS {
        return Err("input exceeds the compiled SMB action bound".into());
    }
    if horizon < maximum_arm || horizon > input.actions.len() {
        return Err("trailing edit horizon cannot fit every registered length arm".into());
    }
    if input
        .actions
        .iter()
        .any(|action| !(1..=MAX_HOLD_FRAMES).contains(&action.hold_frames))
    {
        return Err("input contains an out-of-range hold duration".into());
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_registered_integrity(
    seed: u64,
    paired_draws: usize,
    horizon: usize,
    source_file_sha256: &str,
    input_sha256: &str,
    input_actions: usize,
    rom_sha256: &str,
    baseline: &BaselineSummary,
) -> Result<(), Box<dyn Error>> {
    if seed != REGISTERED_SEED {
        return Err("seed does not match the registered v1 canary".into());
    }
    if paired_draws != REGISTERED_PAIRED_DRAWS {
        return Err("paired draw count does not match the registered v1 canary".into());
    }
    if horizon != DEFAULT_EDIT_HORIZON {
        return Err("edit horizon does not match the registered v1 canary".into());
    }
    if source_file_sha256 != REGISTERED_SOURCE_FILE_SHA256 {
        return Err("compact source file does not match the registered v1 canary".into());
    }
    if input_sha256 != REGISTERED_INPUT_SHA256 || input_actions != REGISTERED_INPUT_ACTIONS {
        return Err("semantic input identity does not match the registered v1 canary".into());
    }
    if rom_sha256 != REGISTERED_ROM_SHA256 {
        return Err("ROM identity does not match the registered v1 canary".into());
    }
    if baseline.evaluation.deterministic_frames != REGISTERED_BASELINE_FRAMES
        || baseline.evaluation.max_mechanical_frontier != REGISTERED_BASELINE_FRONTIER
        || baseline.evaluation.death
        || baseline.evaluation.failure
    {
        return Err("baseline outcome does not match the registered v1 canary".into());
    }
    let segment = baseline.registered_segment;
    if segment.first_action != REGISTERED_SEGMENT_START
        || segment.actions != REGISTERED_SEGMENT_ACTIONS
        || segment.deterministic_frames != REGISTERED_SEGMENT_FRAMES
        || segment.death
        || segment.max_mechanical_frontier != REGISTERED_BASELINE_FRONTIER
    {
        return Err("registered segment context does not match the v1 canary".into());
    }
    Ok(())
}

fn replacement_parameters() -> Result<ReplacementParameters, Box<dyn Error>> {
    Ok(ReplacementParameters {
        lengths: LENGTH_ARMS
            .into_iter()
            .map(|length| NonZeroUsize::new(length).ok_or("zero replacement length"))
            .collect::<Result<Vec<_>, _>>()?,
        maximum_input_steps: NonZeroUsize::new(MAX_SMB_COMPLETION_ACTIONS)
            .ok_or("invalid input bound")?,
    })
}

fn build_baseline(target: &mut SmbTarget, input: &SmbInput) -> Result<Baseline, Box<dyn Error>> {
    target.reset();
    if target.exit_kind() != ExitKind::Ok || target.is_dead() {
        return Err("SMB genesis is not a live executable state".into());
    }
    let initial = target.observe();
    let mut trace = Sha256::new();
    trace.update(b"smb-trace-canary-v1\0trace\0");
    hash_framed_json(&mut trace, &initial)?;
    let mut max_frontier = frontier(initial.decoded);
    let mut segment_start_frames = None;
    let mut segment_max_frontier = frontier(initial.decoded);
    let mut segment_death = false;
    let mut checkpoints = vec![PrefixCheckpoint {
        action_count: 0,
        snapshot: target
            .snapshot()
            .ok_or("failed to snapshot SMB canary genesis")?,
        trace: trace.clone(),
        max_frontier,
        absolute_frames: initial.frame_count,
    }];

    for (index, action) in input.actions.iter().enumerate() {
        if index == REGISTERED_SEGMENT_START {
            segment_start_frames = Some(target.observe().frame_count);
            segment_max_frontier = frontier(target.observe().decoded);
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed while validating the source input".into());
        }
        hash_action_trace(
            &mut trace,
            index,
            *action,
            target.last_action_observations(),
        )?;
        merge_frontier(&mut max_frontier, target.last_action_observations());
        if index >= REGISTERED_SEGMENT_START {
            merge_frontier(&mut segment_max_frontier, target.last_action_observations());
            segment_death |= target.is_dead();
        }
        if target.is_dead() {
            return Err("source input reaches death before its declared endpoint".into());
        }
        let boundary = index
            .checked_add(1)
            .ok_or("source action boundary overflow")?;
        if boundary.is_multiple_of(SNAPSHOT_INTERVAL) || boundary == input.actions.len() {
            checkpoints.push(PrefixCheckpoint {
                action_count: boundary,
                snapshot: target
                    .snapshot()
                    .ok_or("failed to snapshot a source action boundary")?,
                trace: trace.clone(),
                max_frontier,
                absolute_frames: target.observe().frame_count,
            });
        }
    }
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let endpoint_checkpoint = checkpoints
        .last()
        .filter(|checkpoint| checkpoint.action_count == input.actions.len())
        .ok_or("source endpoint checkpoint is missing")?;
    let final_wram_sha256 = sha256_bytes(target.wram());
    let final_snapshot_sha256 = sha256_json(&endpoint_checkpoint.snapshot)?;
    let endpoint_frames = target.observe().frame_count;
    let registered_segment = SegmentSummary {
        first_action: REGISTERED_SEGMENT_START,
        actions: input
            .actions
            .len()
            .checked_sub(REGISTERED_SEGMENT_START)
            .ok_or("registered segment starts beyond the input")?,
        deterministic_frames: endpoint_frames
            .checked_sub(segment_start_frames.ok_or("registered segment start was not replayed")?)
            .ok_or("registered segment frame count moved backwards")?,
        death: segment_death,
        max_mechanical_frontier: segment_max_frontier,
    };
    let evaluation = EvaluationSummary {
        deterministic_frames: endpoint_frames,
        evaluated_frames: endpoint_frames,
        executed_actions: input.actions.len(),
        snapshot_action: 0,
        snapshot_absolute_frames: 0,
        death: target.is_dead(),
        failure: target.exit_kind() != ExitKind::Ok,
        max_mechanical_frontier: max_frontier,
        endpoint,
    };
    Ok(Baseline {
        checkpoints,
        summary: BaselineSummary {
            trace_sha256: finish_sha256(trace),
            final_wram_sha256,
            final_snapshot_sha256,
            registered_segment,
            evaluation,
        },
    })
}

fn build_arm_states(baseline: &BaselineSummary) -> Vec<ArmState> {
    LENGTH_ARMS
        .into_iter()
        .map(|length| ArmState {
            length,
            challenger: TreatmentState::with_baseline(baseline),
            control: TreatmentState::with_baseline(baseline),
        })
        .collect()
}

fn draw_recipe(
    actions: &[ButtonChord],
    seed: u64,
    draw_index: usize,
    horizon_start: usize,
    length: usize,
) -> Result<PairRecipe, Box<dyn Error>> {
    let range_count = actions
        .len()
        .checked_sub(horizon_start)
        .and_then(|horizon| horizon.checked_sub(length))
        .and_then(|remaining| remaining.checked_add(1))
        .ok_or("length arm does not fit inside the trailing edit horizon")?;
    for retry in 0..=MAX_RECIPE_RETRIES {
        let donor_offset = derived_modulo(seed, b"donor", draw_index, retry, 0, range_count)?;
        let recipient_offset =
            derived_modulo(seed, b"recipient", draw_index, retry, 0, range_count)?;
        let donor_start = horizon_start
            .checked_add(donor_offset)
            .ok_or("donor start overflow")?;
        let recipient_start = horizon_start
            .checked_add(recipient_offset)
            .ok_or("recipient start overflow")?;
        let donor_end = donor_start
            .checked_add(length)
            .ok_or("donor end overflow")?;
        let recipient_end = recipient_start
            .checked_add(length)
            .ok_or("recipient end overflow")?;
        if donor_end > actions.len()
            || recipient_end > actions.len()
            || ranges_overlap((donor_start, donor_end), (recipient_start, recipient_end))
        {
            continue;
        }
        let donor = checked_slice(actions, (donor_start, donor_end))?;
        let recipient = checked_slice(actions, (recipient_start, recipient_end))?;
        if donor == recipient {
            continue;
        }
        let control_permutation = shuffled_permutation(seed, draw_index, retry, donor.len())?;
        let control = permute(donor, &control_permutation)?;
        if control == donor || control == recipient {
            continue;
        }
        let mut multiset = donor.to_vec();
        multiset.sort_unstable();
        let donor_total_hold_frames = donor.iter().try_fold(0_u64, |total, action| {
            total.checked_add(u64::from(action.hold_frames))
        });
        return Ok(PairRecipe {
            retry,
            length_arm: length,
            recipient_offset,
            donor_offset,
            recipient_range: (recipient_start, recipient_end),
            donor_range: (donor_start, donor_end),
            donor_semantic_sha256: sha256_json(&donor)?,
            action_multiset_sha256: sha256_json(&multiset)?,
            donor_total_hold_frames: donor_total_hold_frames
                .ok_or("donor hold-frame total overflow")?,
            control_permutation,
            control_replacement_sha256: sha256_json(&control)?,
        });
    }
    Err("recipe construction exceeded the registered retry bound".into())
}

fn derived_modulo(
    seed: u64,
    domain: &[u8],
    draw_index: usize,
    retry: u64,
    ordinal: usize,
    modulus: usize,
) -> Result<usize, Box<dyn Error>> {
    if modulus == 0 {
        return Err("recipe modulus is zero".into());
    }
    let mut digest = Sha256::new();
    digest.update(seed.to_le_bytes());
    digest.update(domain);
    digest.update(u64::try_from(draw_index)?.to_le_bytes());
    digest.update(retry.to_le_bytes());
    digest.update(u64::try_from(ordinal)?.to_le_bytes());
    let bytes: [u8; 8] = digest.finalize()[..8]
        .try_into()
        .map_err(|_| "recipe digest is too short")?;
    let reduced = u64::from_le_bytes(bytes) % u64::try_from(modulus)?;
    Ok(usize::try_from(reduced)?)
}

fn shuffled_permutation(
    seed: u64,
    draw_index: usize,
    retry: u64,
    length: usize,
) -> Result<Vec<usize>, Box<dyn Error>> {
    let mut permutation = (0..length).collect::<Vec<_>>();
    for upper in (1..permutation.len()).rev() {
        let width = upper.checked_add(1).ok_or("shuffle width overflow")?;
        let other = derived_modulo(seed, b"shuffle", draw_index, retry, upper, width)?;
        permutation.swap(upper, other);
    }
    Ok(permutation)
}

fn ranges_overlap(left: (usize, usize), right: (usize, usize)) -> bool {
    left.0 < right.1 && right.0 < left.1
}

fn permute(
    donor: &[ButtonChord],
    permutation: &[usize],
) -> Result<Vec<ButtonChord>, Box<dyn Error>> {
    if donor.len() != permutation.len() {
        return Err("control permutation length differs from its donor".into());
    }
    let mut seen = vec![false; donor.len()];
    let mut result = Vec::with_capacity(donor.len());
    for &index in permutation {
        let value = donor
            .get(index)
            .copied()
            .ok_or("control permutation index is out of range")?;
        let slot = seen
            .get_mut(index)
            .ok_or("control permutation index is out of range")?;
        if *slot {
            return Err("control permutation repeats an index".into());
        }
        *slot = true;
        result.push(value);
    }
    Ok(result)
}

fn checked_slice<T>(source: &[T], range: (usize, usize)) -> Result<&[T], Box<dyn Error>> {
    if range.0 > range.1 {
        return Err("range is inverted".into());
    }
    source
        .get(range.0..range.1)
        .ok_or_else(|| "range is out of bounds".into())
}

fn validate_pair_materialization(
    source: &[ButtonChord],
    donor: &[ButtonChord],
    control_replacement: &[ButtonChord],
    recipe: &PairRecipe,
    challenger: &[ButtonChord],
    control: &[ButtonChord],
) -> Result<(), Box<dyn Error>> {
    if challenger.len() != source.len() || control.len() != source.len() {
        return Err("materialized candidate changed the source action count".into());
    }
    let prefix = source
        .get(..recipe.recipient_range.0)
        .ok_or("recipient prefix is out of bounds")?;
    let suffix = source
        .get(recipe.recipient_range.1..)
        .ok_or("recipient suffix is out of bounds")?;
    if challenger.get(..recipe.recipient_range.0) != Some(prefix)
        || control.get(..recipe.recipient_range.0) != Some(prefix)
        || challenger.get(recipe.recipient_range.1..) != Some(suffix)
        || control.get(recipe.recipient_range.1..) != Some(suffix)
    {
        return Err("materialized candidate changed the preserved prefix or tail".into());
    }
    if checked_slice(challenger, recipe.recipient_range)? != donor
        || checked_slice(control, recipe.recipient_range)? != control_replacement
    {
        return Err("materialized candidate does not contain its registered replacement".into());
    }
    if challenger == source || control == source {
        return Err("materialized candidate left the source input unchanged".into());
    }
    let mut donor_multiset = donor.to_vec();
    donor_multiset.sort_unstable();
    let mut control_multiset = control_replacement.to_vec();
    control_multiset.sort_unstable();
    if donor_multiset != control_multiset {
        return Err("paired replacement action multisets differ".into());
    }
    let donor_frames = donor.iter().try_fold(0_u64, |total, action| {
        total.checked_add(u64::from(action.hold_frames))
    });
    let control_frames = control_replacement.iter().try_fold(0_u64, |total, action| {
        total.checked_add(u64::from(action.hold_frames))
    });
    if donor_frames.is_none() || donor_frames != control_frames {
        return Err("paired replacement hold-frame totals differ or overflow".into());
    }
    Ok(())
}

fn evaluate_candidate(
    target: &mut SmbTarget,
    checkpoints: &[PrefixCheckpoint],
    source: &[ButtonChord],
    candidate: &[ButtonChord],
    edit_start: usize,
    baseline: &BaselineSummary,
) -> Result<CandidateEvaluation, Box<dyn Error>> {
    if candidate.len() != source.len() || edit_start > candidate.len() {
        return Err("candidate shape differs from the registered source input".into());
    }
    let checkpoint = checkpoints
        .iter()
        .rev()
        .find(|checkpoint| checkpoint.action_count <= edit_start)
        .ok_or("candidate has no pre-edit checkpoint")?;
    if source.get(..checkpoint.action_count) != candidate.get(..checkpoint.action_count) {
        return Err("candidate changed actions before its restored checkpoint".into());
    }
    target.restore(&checkpoint.snapshot)?;
    if target.observe().frame_count != checkpoint.absolute_frames {
        return Err("restored snapshot frame does not match its absolute prefix count".into());
    }
    let work_before = target.frames_clocked();
    let mut trace = checkpoint.trace.clone();
    let mut max_frontier = checkpoint.max_frontier;
    let mut executed_actions = checkpoint.action_count;
    for (index, action) in candidate.iter().enumerate().skip(checkpoint.action_count) {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("emulator failed while evaluating a trace edit".into());
        }
        hash_action_trace(
            &mut trace,
            index,
            *action,
            target.last_action_observations(),
        )?;
        merge_frontier(&mut max_frontier, target.last_action_observations());
        executed_actions = index
            .checked_add(1)
            .ok_or("executed action count overflow")?;
        if target.is_dead() {
            break;
        }
    }
    let endpoint = smb_mechanical_state_from_wram(target.wram());
    let failure = target.exit_kind() != ExitKind::Ok;
    let evaluated_frames = target
        .frames_clocked()
        .checked_sub(work_before)
        .ok_or("candidate work counter moved backwards")?;
    let expected_absolute_frames = checkpoint
        .absolute_frames
        .checked_add(evaluated_frames)
        .ok_or("candidate absolute frame count overflow")?;
    if target.observe().frame_count != expected_absolute_frames {
        return Err("candidate absolute frame count does not reconcile with its snapshot".into());
    }
    let final_wram_sha256 = sha256_bytes(target.wram());
    let final_snapshot = target
        .snapshot()
        .ok_or("failed to snapshot a trace-edit endpoint")?;
    let final_snapshot_sha256 = sha256_json(&final_snapshot)?;
    let summary = EvaluationSummary {
        deterministic_frames: target.observe().frame_count,
        evaluated_frames,
        executed_actions,
        snapshot_action: checkpoint.action_count,
        snapshot_absolute_frames: checkpoint.absolute_frames,
        death: target.is_dead(),
        failure,
        max_mechanical_frontier: max_frontier,
        endpoint,
    };
    Ok(CandidateEvaluation {
        input_sha256: sha256_json(&SmbInput {
            actions: candidate.to_vec(),
        })?,
        trace_sha256: finish_sha256(trace),
        final_wram_sha256,
        final_snapshot_sha256,
        source_changed: source != candidate,
        useful: is_useful(&summary, &baseline.evaluation),
        summary,
    })
}

fn is_useful(candidate: &EvaluationSummary, baseline: &EvaluationSummary) -> bool {
    if candidate.death || candidate.failure {
        return false;
    }
    (candidate.max_mechanical_frontier > baseline.max_mechanical_frontier
        && candidate.deterministic_frames <= baseline.deterministic_frames)
        || (candidate.max_mechanical_frontier == baseline.max_mechanical_frontier
            && candidate.deterministic_frames < baseline.deterministic_frames)
}

fn candidate_record(evaluation: CandidateEvaluation, discovery: Discovery) -> CandidateRecord {
    CandidateRecord {
        candidate_sha256: evaluation.input_sha256,
        trace_sha256: evaluation.trace_sha256,
        final_wram_sha256: evaluation.final_wram_sha256,
        final_snapshot_sha256: evaluation.final_snapshot_sha256,
        source_changed: evaluation.source_changed,
        useful: evaluation.useful,
        evaluation: evaluation.summary,
        discovery,
    }
}

fn frontier(state: SmbMechanicalState) -> SmbProgressWatermark {
    SmbProgressWatermark {
        world: state.world,
        level: state.level,
        progress: state.progress,
    }
}

fn merge_frontier(frontier_max: &mut SmbProgressWatermark, observations: &[SmbObservations]) {
    for observation in observations {
        *frontier_max = (*frontier_max).max(frontier(observation.decoded));
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

fn write_record<T: Serialize>(output: &mut NdjsonOutput, record: &T) -> Result<(), Box<dyn Error>> {
    output.write(record)
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

fn parse_u64(value: &str) -> Result<u64, Box<dyn Error>> {
    if let Some(hex) = value.strip_prefix("0x") {
        Ok(u64::from_str_radix(hex, 16)?)
    } else {
        Ok(value.parse()?)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ButtonChord, EvaluationSummary, PairRecipe, SmbProgressWatermark, derived_modulo,
        draw_recipe, is_useful, permute, ranges_overlap, validate_pair_materialization,
    };

    #[test]
    fn registered_recipe_is_reproducible_nonoverlapping_and_nonvacuous() {
        let actions = (0..256)
            .map(|index| ButtonChord::new(u8::try_from(index).unwrap_or(0), 1))
            .collect::<Vec<_>>();
        let first = draw_recipe(&actions, 91, 17, 0, 16).expect("first recipe");
        let second = draw_recipe(&actions, 91, 17, 0, 16).expect("second recipe");
        assert_eq!(first.retry, second.retry);
        assert_eq!(first.recipient_range, second.recipient_range);
        assert_eq!(first.donor_range, second.donor_range);
        assert_eq!(first.control_permutation, second.control_permutation);
        assert!(!ranges_overlap(first.recipient_range, first.donor_range));
        assert_ne!(
            &actions[first.recipient_range.0..first.recipient_range.1],
            &actions[first.donor_range.0..first.donor_range.1]
        );
        let donor = &actions[first.donor_range.0..first.donor_range.1];
        let shuffled = permute(donor, &first.control_permutation).expect("shuffle donor");
        assert_ne!(shuffled, donor);
        let mut expected = donor.to_vec();
        expected.sort_unstable();
        let mut actual = shuffled;
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    #[test]
    fn hash_domains_and_ordinals_are_state_independent() {
        let first = derived_modulo(3, b"recipient", 4, 5, 0, 97).expect("derive recipient");
        assert_eq!(
            first,
            derived_modulo(3, b"recipient", 4, 5, 0, 97).expect("repeat recipient")
        );
        assert_ne!(
            first,
            derived_modulo(3, b"donor", 4, 5, 0, 97).expect("derive donor")
        );
        assert!(derived_modulo(3, b"donor", 4, 5, 0, 0).is_err());
    }

    #[test]
    fn useful_gate_matches_the_registered_order_and_cost_rule() {
        let baseline = EvaluationSummary {
            deterministic_frames: 100,
            evaluated_frames: 100,
            executed_actions: 5,
            snapshot_action: 0,
            snapshot_absolute_frames: 0,
            death: false,
            failure: false,
            max_mechanical_frontier: SmbProgressWatermark {
                world: 1,
                level: 2,
                progress: 3,
            },
            endpoint: Default::default(),
        };
        let mut candidate = baseline;
        candidate.deterministic_frames = 99;
        assert!(is_useful(&candidate, &baseline));
        candidate.deterministic_frames = 101;
        candidate.max_mechanical_frontier.progress = 4;
        assert!(!is_useful(&candidate, &baseline));
        candidate.deterministic_frames = 100;
        assert!(is_useful(&candidate, &baseline));
        candidate.death = true;
        assert!(!is_useful(&candidate, &baseline));
    }

    #[test]
    fn range_overlap_treats_touching_windows_as_disjoint() {
        assert!(!ranges_overlap((4, 8), (8, 12)));
        assert!(ranges_overlap((4, 9), (8, 12)));
    }

    #[test]
    fn runtime_pair_validation_rejects_a_changed_tail() {
        let source = (0..10)
            .map(|value| ButtonChord::new(value, 1))
            .collect::<Vec<_>>();
        let donor = source[0..2].to_vec();
        let control_replacement = vec![donor[1], donor[0]];
        let mut challenger = source.clone();
        challenger[4..6].copy_from_slice(&donor);
        let mut control = source.clone();
        control[4..6].copy_from_slice(&control_replacement);
        let recipe = PairRecipe {
            retry: 0,
            length_arm: 2,
            recipient_offset: 4,
            donor_offset: 0,
            recipient_range: (4, 6),
            donor_range: (0, 2),
            donor_semantic_sha256: String::new(),
            action_multiset_sha256: String::new(),
            donor_total_hold_frames: 2,
            control_permutation: vec![1, 0],
            control_replacement_sha256: String::new(),
        };
        validate_pair_materialization(
            &source,
            &donor,
            &control_replacement,
            &recipe,
            &challenger,
            &control,
        )
        .expect("valid materialization");
        control[9] = ButtonChord::new(99, 1);
        assert!(
            validate_pair_materialization(
                &source,
                &donor,
                &control_replacement,
                &recipe,
                &challenger,
                &control,
            )
            .is_err()
        );
    }
}
