// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    error::Error,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use nes_workload::{
    search::{
        archive::{ArchiveCandidate, ArchiveKey},
        campaign::Evaluation,
        rand::RomuDuoJrRand,
    },
    smb::{
        archive::{MAX_SMB_COMPLETION_ACTIONS, SmbArchive, SmbArchiveKey},
        campaign::SmbGame,
        target::{
            ButtonChord, MAX_HOLD_FRAMES, SmbMilestones, SmbSnapshot, SmbTarget, WRAM_SIZE,
            smb_mechanical_state_from_wram,
        },
    },
    target::{ExitKind, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REQUEST_LIMIT: usize = 64_000;
const INPUT_FILE_LIMIT: usize = 1_000_000;
const PREFIX_ACTION_LIMIT: usize = MAX_SMB_COMPLETION_ACTIONS - 1;
const PREFIX_FRAME_LIMIT: u64 = 1_000_000;
const PREFIX_COUNT: usize = 3;
const TOTAL_PREFIX_ACTION_LIMIT: usize = PREFIX_COUNT * PREFIX_ACTION_LIMIT;
const TOTAL_PREFIX_FRAME_LIMIT: u64 = PREFIX_COUNT as u64 * PREFIX_FRAME_LIMIT;
const CONTINUATION_ACTION_LIMIT: usize = 128;
const CONTINUATION_FRAME_LIMIT: u64 = 15_360;
const ROM_FILE_LIMIT: usize = 16_000_000;
const CORE_FILE_LIMIT: u64 = 128_000_000;
const ARCHIVE_CAPACITY: usize = 512;
const MAX_POPULATION_SAMPLES_PER_PREFIX: usize = 256;
const SELECTOR_SAMPLES: usize = 16;
const HISTORY_CORRECT_OFFSET: usize = 0x06d9;
const HISTORY_PASSES_OFFSET: usize = 0x06da;
const WORLD_OFFSET: usize = 0x075f;
const LEVEL_OFFSET: usize = 0x075c;
const AREA_TYPE_OFFSET: usize = 0x074e;
const PLAYER_X_PAGE_OFFSET: usize = 0x006d;
const PLAYER_X_OFFSET: usize = 0x0086;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    rom_path: PathBuf,
    rom_sha256: String,
    core_path: PathBuf,
    core_sha256: String,
    useful_prefix: PathBuf,
    other_prefix: PathBuf,
    competitor_prefix: PathBuf,
    continuation: PathBuf,
    objective: Objective,
    population_start_action: usize,
    seed: u64,
    broken: bool,
    #[serde(default)]
    mode: Mode,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Mode {
    #[default]
    History,
    Deadline,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct Objective {
    world: u8,
    level: u8,
    area_type: u8,
    absolute_player_x_min: u16,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFile {
    actions: Vec<ChordInput>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChordInput {
    buttons: u8,
    hold_frames: u8,
}

struct ParsedInput {
    actions: Vec<ButtonChord>,
    frames: u64,
    sha256: String,
}

#[derive(Clone, Copy, Debug)]
struct RawState {
    world: u8,
    level: u8,
    area_type: u8,
    area_number: u8,
    absolute_x: u16,
    history_correct: u8,
    history_passes: u8,
    clock: u16,
    alive: bool,
}

#[derive(Clone)]
struct Checkpoint {
    count: usize,
    snapshot: SmbSnapshot,
    key: SmbArchiveKey,
    milestones: SmbMilestones,
    wram: [u8; WRAM_SIZE],
    raw: RawState,
}

struct Replay {
    input: ParsedInput,
    target: SmbTarget,
    population: Vec<Checkpoint>,
    endpoint: Checkpoint,
}

#[derive(Clone)]
struct PopulationCandidate {
    branch: usize,
    checkpoint: Checkpoint,
}

fn chord_time(action: &ButtonChord) -> u64 {
    u64::from(action.bounded_hold_frames())
}

fn read_capped(path: &Path, limit: usize) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit.saturating_add(1))?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(format!("input exceeds the {limit}-byte bound").into());
    }
    Ok(bytes)
}

fn read_hashed_file(path: &Path, limit: u64) -> Result<(Vec<u8>, String), Box<dyn Error>> {
    let bytes = read_capped(path, usize::try_from(limit)?)?;
    let digest = format!("{:x}", Sha256::digest(&bytes));
    Ok((bytes, digest))
}

fn hash_file(path: &Path, limit: u64) -> Result<String, Box<dyn Error>> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size = size.saturating_add(u64::try_from(read)?);
        if size > limit {
            return Err("core exceeds its bounded file size".into());
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn check_digest(expected: &str, actual: &str, label: &str) -> Result<(), Box<dyn Error>> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} SHA-256 must contain 64 hexadecimal characters").into());
    }
    if !expected.eq_ignore_ascii_case(actual) {
        return Err(format!("{label} SHA-256 mismatch").into());
    }
    Ok(())
}

fn read_input(
    path: &Path,
    action_limit: usize,
    frame_limit: u64,
) -> Result<ParsedInput, Box<dyn Error>> {
    let bytes = read_capped(path, INPUT_FILE_LIMIT)?;
    let parsed: InputFile = serde_json::from_slice(&bytes)?;
    if parsed.actions.is_empty() || parsed.actions.len() > action_limit {
        return Err(format!("input must contain 1..={action_limit} actions").into());
    }
    let mut frames = 0_u64;
    let mut actions = Vec::with_capacity(parsed.actions.len());
    for action in parsed.actions {
        if action.hold_frames == 0 || action.hold_frames > MAX_HOLD_FRAMES {
            return Err("hold_frames is outside the native 1..=120 bound".into());
        }
        frames = frames
            .checked_add(u64::from(action.hold_frames))
            .ok_or("input frame count overflowed")?;
        if frames > frame_limit {
            return Err("input exceeds its fixed frame bound".into());
        }
        actions.push(ButtonChord::new(action.buttons, action.hold_frames));
    }
    Ok(ParsedInput {
        actions,
        frames,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn sample_counts(action_count: usize, start: usize) -> Vec<usize> {
    let first = start.max(1);
    let last = action_count.saturating_sub(1);
    if first > last {
        return Vec::new();
    }
    let available = last - first + 1;
    let take = available.min(MAX_POPULATION_SAMPLES_PER_PREFIX);
    (0..take)
        .map(|index| {
            if take == 1 {
                first
            } else {
                first + index * (available - 1) / (take - 1)
            }
        })
        .collect()
}

fn raw_state(wram: &[u8; WRAM_SIZE], alive: bool) -> RawState {
    RawState {
        world: wram[WORLD_OFFSET],
        level: wram[LEVEL_OFFSET],
        area_type: wram[AREA_TYPE_OFFSET],
        area_number: wram[0x074f],
        absolute_x: u16::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u16::from(wram[PLAYER_X_OFFSET]),
        history_correct: wram[HISTORY_CORRECT_OFFSET],
        history_passes: wram[HISTORY_PASSES_OFFSET],
        clock: wram[0x07f8..=0x07fa]
            .iter()
            .fold(0, |clock, digit| clock * 10 + u16::from(*digit)),
        alive,
    }
}

fn raw_objective(wram: &[u8; WRAM_SIZE], objective: Objective) -> bool {
    let state = smb_mechanical_state_from_wram(wram);
    let absolute_x = u16::from(wram[PLAYER_X_PAGE_OFFSET]) * 256 + u16::from(wram[PLAYER_X_OFFSET]);
    !state.dead
        && wram[WORLD_OFFSET] == objective.world
        && wram[LEVEL_OFFSET] == objective.level
        && wram[AREA_TYPE_OFFSET] == objective.area_type
        && absolute_x >= objective.absolute_player_x_min
}

fn effective_key(mut key: SmbArchiveKey, broken: bool, mode: Mode) -> SmbArchiveKey {
    if broken {
        match mode {
            Mode::History => key.loop_on_path = false,
            Mode::Deadline => key.clock = 999 - key.clock,
        }
    }
    key
}

fn checkpoint(
    game: &SmbGame,
    target: &mut SmbTarget,
    count: usize,
) -> Result<Checkpoint, Box<dyn Error>> {
    let snapshot = target
        .snapshot()
        .ok_or("failed to capture exact SMB snapshot")?;
    let wram = target.wram();
    let key = game.complete_candidate_key(game.rollout_key(target)?, &snapshot)?;
    Ok(Checkpoint {
        count,
        snapshot,
        key,
        milestones: target.observe().milestones,
        raw: raw_state(&wram, !target.is_dead()),
        wram,
    })
}

fn replay_prefix(
    game: &SmbGame,
    rom: &[u8],
    core_path: &Path,
    core_sha256: &str,
    input: ParsedInput,
    population_start_action: usize,
) -> Result<Replay, Box<dyn Error>> {
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom, core_path, core_sha256)?;
    let sample_counts = sample_counts(input.actions.len(), population_start_action);
    let sample_set: BTreeSet<usize> = sample_counts.iter().copied().collect();
    let mut population = Vec::with_capacity(sample_counts.len());
    let mut endpoint = None;
    for (index, action) in input.actions.iter().enumerate() {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err(format!("SMB target failed during prefix action {}", index + 1).into());
        }
        if target.is_dead() || target.is_victory() {
            return Err(format!("prefix became terminal at action {}", index + 1).into());
        }
        let count = index + 1;
        if count == input.actions.len() || sample_set.contains(&count) {
            let captured = checkpoint(game, &mut target, count)?;
            if count == input.actions.len() {
                endpoint = Some(captured);
            } else {
                population.push(captured);
            }
        }
    }
    let endpoint = endpoint.ok_or("prefix did not produce an endpoint snapshot")?;
    target.restore(&endpoint.snapshot)?;
    if target.wram() != endpoint.wram || target.is_dead() {
        return Err("prefix endpoint snapshot did not restore its exact live state".into());
    }
    Ok(Replay {
        input,
        target,
        population,
        endpoint,
    })
}

fn same_zone(left: RawState, right: RawState) -> bool {
    (left.world, left.level, left.area_type) == (right.world, right.level, right.area_type)
}

fn normalized_location(mut key: SmbArchiveKey, mode: Mode) -> SmbArchiveKey {
    match mode {
        Mode::History => key.loop_on_path = false,
        Mode::Deadline => key.clock = 0,
    }
    key
}

fn same_normalized_slot(left: SmbArchiveKey, right: SmbArchiveKey, mode: Mode) -> bool {
    let left = normalized_location(left, mode);
    let right = normalized_location(right, mode);
    left.place() == right.place()
        && left.progress() == right.progress()
        && left.identity() == right.identity()
}

fn state_json(checkpoint: &Checkpoint) -> Value {
    json!({
        "actions": checkpoint.count,
        "raw": raw_json(checkpoint.raw),
        "loop_on_path": checkpoint.key.loop_on_path,
        "clock": checkpoint.key.clock,
        "raw_clock": checkpoint.raw.clock,
    })
}

fn population_candidates(
    useful: &Replay,
    other: &Replay,
    competitor: &Replay,
    start: usize,
    mode: Mode,
) -> Vec<PopulationCandidate> {
    let useful_zone = useful.endpoint.raw;
    let other_zone = other.endpoint.raw;
    let common_room = useful.endpoint.key.room;
    let mut candidates = Vec::new();
    let competitor_zone = competitor.endpoint.raw;
    for (branch, replay, endpoint_zone) in [
        (0, useful, useful_zone),
        (1, other, other_zone),
        (2, competitor, competitor_zone),
    ] {
        for checkpoint in &replay.population {
            if checkpoint.count >= start
                && same_zone(checkpoint.raw, endpoint_zone)
                && same_zone(checkpoint.raw, other_zone)
                && same_zone(checkpoint.raw, competitor_zone)
                && checkpoint.key.room == common_room
                && !same_normalized_slot(checkpoint.key, useful.endpoint.key, mode)
            {
                candidates.push(PopulationCandidate {
                    branch,
                    checkpoint: checkpoint.clone(),
                });
            }
        }
    }
    candidates.sort_by_key(|candidate| (candidate.branch, candidate.checkpoint.count));
    candidates.truncate(ARCHIVE_CAPACITY.saturating_sub(3));
    candidates
}

fn insert_candidate(
    archive: &mut SmbArchive,
    parent: Option<usize>,
    actions: &[ButtonChord],
    checkpoint: &Checkpoint,
    broken: bool,
    mode: Mode,
    execution: u64,
) -> Result<Option<usize>, Box<dyn Error>> {
    archive.insert(
        parent,
        execution,
        ArchiveCandidate {
            suffix: actions.to_vec(),
            key: effective_key(checkpoint.key, broken, mode),
            milestones: checkpoint.milestones,
        },
        checkpoint.snapshot.clone(),
    )
}

fn selected_states(
    archive: &mut SmbArchive,
    indices: &[usize],
    target: &mut SmbTarget,
    continuation: &ParsedInput,
    objective: Objective,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let (reports, snapshots) = archive.take_entry_reports_and_snapshots();
    let states: BTreeMap<u64, SmbSnapshot> = snapshots.into_iter().collect();
    indices
        .iter()
        .map(|index| {
            let report = reports
                .get(*index)
                .ok_or("selected archive index has no report")?;
            let snapshot = states
                .get(&report.id)
                .ok_or("selected active archive entry has no actual snapshot")?;
            target.restore(snapshot)?;
            let wram = target.wram();
            let state = smb_mechanical_state_from_wram(&wram);
            let actual = raw_state(&wram, !target.is_dead());
            let witness = run_witness(target, &continuation.actions, objective)?;
            Ok(json!({
                "entry_id": report.id,
                "state": {"world": state.world, "level": state.level, "progress": state.progress,
                    "area_type": actual.area_type, "area_number": actual.area_number,
                    "absolute_player_x": actual.absolute_x, "alive": actual.alive,
                    "game_clock": actual.clock,
                    "dead": state.dead, "history_correct": actual.history_correct,
                    "history_passes": actual.history_passes},
                "key": {"loop_on_path": report.key.loop_on_path, "clock": report.key.clock},
                "shared_witness": witness,
            }))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn archive_trial(
    population: &[PopulationCandidate],
    useful: &mut Replay,
    other: &mut Replay,
    competitor: &Replay,
    seed: u64,
    broken: bool,
    mode: Mode,
    useful_first: bool,
    continuation: &ParsedInput,
    objective: Objective,
) -> Result<Value, Box<dyn Error>> {
    let mut archive = SmbArchive::new(chord_time);
    archive.max_entries = ARCHIVE_CAPACITY;
    let mut execution = 0_u64;
    let mut population_insertions = 0_u64;
    for candidate in population {
        let replay: &Replay = match candidate.branch {
            0 => &*useful,
            1 => &*other,
            _ => competitor,
        };
        let count = candidate.checkpoint.count;
        if insert_candidate(
            &mut archive,
            None,
            &replay.input.actions[..count],
            &candidate.checkpoint,
            broken,
            mode,
            execution,
        )?
        .is_some()
        {
            population_insertions = population_insertions.saturating_add(1);
        }
        execution = execution.saturating_add(1);
    }
    let competitor_id = insert_candidate(
        &mut archive,
        None,
        &competitor.input.actions,
        &competitor.endpoint,
        broken,
        mode,
        execution,
    )?;
    execution = execution.saturating_add(1);
    let (first, second): (&Replay, &Replay) = if useful_first {
        (&*useful, &*other)
    } else {
        (&*other, &*useful)
    };
    let first_id = insert_candidate(
        &mut archive,
        None,
        &first.input.actions,
        &first.endpoint,
        broken,
        mode,
        execution,
    )?;
    execution = execution.saturating_add(1);
    let second_id = insert_candidate(
        &mut archive,
        None,
        &second.input.actions,
        &second.endpoint,
        broken,
        mode,
        execution,
    )?;
    let (useful_id, other_id) = if useful_first {
        (first_id, second_id)
    } else {
        (second_id, first_id)
    };
    let useful_retained =
        useful_id.is_some_and(|id| archive.active.get(id).copied().unwrap_or(false));
    let other_retained =
        other_id.is_some_and(|id| archive.active.get(id).copied().unwrap_or(false));
    let competitor_retained =
        competitor_id.is_some_and(|id| archive.active.get(id).copied().unwrap_or(false));
    let active_entries = archive.active_count();
    let resident_snapshots = archive.resident_snapshot_count();
    let mut random = RomuDuoJrRand::with_seed(seed);
    let mut selected_indices = Vec::with_capacity(SELECTOR_SAMPLES);
    for _ in 0..SELECTOR_SAMPLES {
        let (id, draw) = archive.select_parent(&mut random, MAX_SMB_COMPLETION_ACTIONS)?;
        archive.record_selection(id, &draw);
        selected_indices.push(id);
    }
    let selected = selected_states(
        &mut archive,
        &selected_indices,
        &mut useful.target,
        continuation,
        objective,
    )?;
    Ok(json!({
        "order": if useful_first { "useful_then_other" } else { "other_then_useful" },
        "competitor_inserted_first": true,
        "warmup_candidate_cap": ARCHIVE_CAPACITY - 3,
        "capacity": ARCHIVE_CAPACITY, "per_cell_capacity": SmbArchiveKey::capacity(),
        "population_candidates": population.len(),
        "population_insertions": population_insertions, "active_entries": active_entries,
        "resident_snapshots": resident_snapshots, "useful_index": useful_id, "other_index": other_id,
        "useful_active": useful_retained, "other_active": other_retained,
        "competitor_index": competitor_id, "competitor_active": competitor_retained,
        "seed": seed, "selected_actual_snapshots": selected,
    }))
}

fn run_witness(
    target: &mut SmbTarget,
    actions: &[ButtonChord],
    objective: Objective,
) -> Result<Value, Box<dyn Error>> {
    let mut actions_executed = 0_u64;
    let mut frames_executed = 0_u64;
    let mut requested_frames = 0_u64;
    let mut reached = raw_objective(&target.wram(), objective);
    let mut terminal = if target.is_dead() { "dead" } else { "runnable" };
    for action in actions {
        if reached || terminal != "runnable" {
            break;
        }
        actions_executed = actions_executed.saturating_add(1);
        requested_frames = requested_frames.saturating_add(u64::from(action.bounded_hold_frames()));
        let work_before = target.execution_work();
        target.apply(action);
        frames_executed =
            frames_executed.saturating_add(target.execution_work().saturating_sub(work_before));
        if target.exit_kind() != ExitKind::Ok {
            terminal = "target_error";
            break;
        }
        let wram = target.wram();
        reached = raw_objective(&wram, objective);
        if target.is_dead() {
            terminal = "dead";
        } else if target.is_victory() {
            terminal = "victory";
        }
    }
    let final_wram = target.wram();
    let final_raw = raw_state(&final_wram, !target.is_dead());
    Ok(json!({
        "status": if reached { "objective_reached" } else { terminal },
        "objective_reached": reached,
        "actions_executed": actions_executed,
        "frames_executed": frames_executed,
        "requested_frames": requested_frames,
        "final_state": raw_json(final_raw),
    }))
}

fn continue_from(
    replay: &mut Replay,
    continuation: &ParsedInput,
    objective: Objective,
) -> Result<Value, Box<dyn Error>> {
    replay.target.restore(&replay.endpoint.snapshot)?;
    if replay.target.wram() != replay.endpoint.wram {
        return Err("continuation checkpoint failed exact restore validation".into());
    }
    run_witness(&mut replay.target, &continuation.actions, objective)
}

fn raw_json(raw: RawState) -> Value {
    json!({
        "world": raw.world, "level": raw.level, "area_type": raw.area_type,
        "area_number": raw.area_number, "absolute_player_x": raw.absolute_x, "alive": raw.alive,
        "history_correct_passes": raw.history_correct, "history_passes": raw.history_passes,
        "game_clock": raw.clock,
    })
}

fn run(request_path: &Path, output_dir: &Path) -> Result<(), Box<dyn Error>> {
    let request: Request = serde_json::from_slice(&read_capped(request_path, REQUEST_LIMIT)?)?;
    let (rom, rom_sha256) = read_hashed_file(&request.rom_path, ROM_FILE_LIMIT as u64)?;
    let core_sha256 = hash_file(&request.core_path, CORE_FILE_LIMIT)?;
    check_digest(&request.rom_sha256, &rom_sha256, "ROM")?;
    check_digest(&request.core_sha256, &core_sha256, "core")?;
    let useful_input = read_input(
        &request.useful_prefix,
        PREFIX_ACTION_LIMIT,
        PREFIX_FRAME_LIMIT,
    )?;
    let other_input = read_input(
        &request.other_prefix,
        PREFIX_ACTION_LIMIT,
        PREFIX_FRAME_LIMIT,
    )?;
    let competitor_input = read_input(
        &request.competitor_prefix,
        PREFIX_ACTION_LIMIT,
        PREFIX_FRAME_LIMIT,
    )?;
    let continuation = read_input(
        &request.continuation,
        CONTINUATION_ACTION_LIMIT,
        CONTINUATION_FRAME_LIMIT,
    )?;
    let total_prefix_actions =
        useful_input.actions.len() + other_input.actions.len() + competitor_input.actions.len();
    let total_prefix_frames = useful_input.frames + other_input.frames + competitor_input.frames;
    if total_prefix_actions > TOTAL_PREFIX_ACTION_LIMIT
        || total_prefix_frames > TOTAL_PREFIX_FRAME_LIMIT
    {
        return Err("three-prefix aggregate exceeds its fixed action or frame bound".into());
    }
    if request.population_start_action >= useful_input.actions.len()
        || request.population_start_action >= other_input.actions.len()
        || request.population_start_action >= competitor_input.actions.len()
    {
        return Err("population_start_action must precede all three endpoint states".into());
    }

    let game = SmbGame::new(&rom, &request.core_path, &core_sha256);
    let mut useful = replay_prefix(
        &game,
        &rom,
        &request.core_path,
        &core_sha256,
        useful_input,
        request.population_start_action,
    )?;
    let mut other = replay_prefix(
        &game,
        &rom,
        &request.core_path,
        &core_sha256,
        other_input,
        request.population_start_action,
    )?;
    let mut competitor = replay_prefix(
        &game,
        &rom,
        &request.core_path,
        &core_sha256,
        competitor_input,
        request.population_start_action,
    )?;

    let useful = &mut useful;
    let other = &mut other;
    let competitor = &mut competitor;
    let useful_raw = useful.endpoint.raw;
    let other_raw = other.endpoint.raw;
    let competitor_raw = competitor.endpoint.raw;
    let useful_loop = useful.endpoint.key.loop_on_path;
    let other_loop = other.endpoint.key.loop_on_path;
    let competitor_loop = competitor.endpoint.key.loop_on_path;
    if !useful_raw.alive || !other_raw.alive || !competitor_raw.alive {
        return Err("three prefix endpoints must be alive".into());
    }
    if !same_zone(useful_raw, other_raw) || !same_zone(other_raw, competitor_raw) {
        return Err("three prefix endpoints do not share raw world, level, and area type".into());
    }
    let useful_normalized = normalized_location(useful.endpoint.key, request.mode);
    let other_normalized = normalized_location(other.endpoint.key, request.mode);
    let same_place = useful_normalized.place() == other_normalized.place();
    let same_progress = useful_normalized.progress() == other_normalized.progress();
    let same_identity = useful_normalized.identity() == other_normalized.identity();
    if !(same_place && same_progress && same_identity) {
        return Err(
            "prefix endpoints differ in normalized archive place, progress, or identity".into(),
        );
    }
    if !same_normalized_slot(other.endpoint.key, competitor.endpoint.key, request.mode) {
        return Err("other and competitor endpoints must share the normalized archive slot".into());
    }
    if request.mode == Mode::History {
        if !useful_loop || other_loop || competitor_loop {
            return Err(
                "history mode requires useful history true and both competitor histories false"
                    .into(),
            );
        }
    } else {
        let clocks = [useful_raw.clock, other_raw.clock, competitor_raw.clock];
        if clocks.iter().any(|clock| *clock > 999)
            || useful.endpoint.key.clock != useful_raw.clock
            || other.endpoint.key.clock != other_raw.clock
            || competitor.endpoint.key.clock != competitor_raw.clock
        {
            return Err("deadline mode requires matching raw and key clocks in 0..=999".into());
        }
        if useful_raw.clock <= other_raw.clock || useful_raw.clock <= competitor_raw.clock {
            return Err(
                "deadline mode requires the useful endpoint clock to exceed both competitors"
                    .into(),
            );
        }
    }
    if other.endpoint.wram == competitor.endpoint.wram {
        return Err("competitor must be an independent naturally replayed state".into());
    }

    let useful_continuation = continue_from(useful, &continuation, request.objective)?;
    let other_continuation = continue_from(other, &continuation, request.objective)?;
    let competitor_continuation = continue_from(competitor, &continuation, request.objective)?;
    let population = population_candidates(
        useful,
        other,
        competitor,
        request.population_start_action,
        request.mode,
    );
    if population.is_empty() {
        return Err(
            "population_start_action produced no preceding states in the shared room context"
                .into(),
        );
    }
    let trial_useful_first = archive_trial(
        &population,
        useful,
        other,
        competitor,
        request.seed,
        request.broken,
        request.mode,
        true,
        &continuation,
        request.objective,
    )?;
    let trial_other_first = archive_trial(
        &population,
        useful,
        other,
        competitor,
        request.seed,
        request.broken,
        request.mode,
        false,
        &continuation,
        request.objective,
    )?;

    fs::create_dir_all(output_dir)?;
    let pair_checks = json!({"useful_history_on_path": useful_loop, "other_history_on_path": other_loop,
            "competitor_history_on_path": competitor_loop,
            "history_feature_ablation_applied": request.mode == Mode::History && request.broken,
            "clock_preference_reversed": request.mode == Mode::Deadline && request.broken,
            "competitor_shares_other_normalized_slot": same_normalized_slot(other.endpoint.key, competitor.endpoint.key, request.mode),
            "competitor_is_distinct_actual_state": other.endpoint.wram != competitor.endpoint.wram,
            "same_raw_zone": same_zone(useful_raw, other_raw), "same_normalized_archive_place": same_place,
            "same_normalized_archive_progress": same_progress, "same_normalized_archive_identity": same_identity,
            "useful_clock_exceeds_other_and_competitor": useful_raw.clock > other_raw.clock && useful_raw.clock > competitor_raw.clock,
            "raw_clocks_match_production_keys": useful.endpoint.key.clock == useful_raw.clock && other.endpoint.key.clock == other_raw.clock && competitor.endpoint.key.clock == competitor_raw.clock,
            "clock_is_preference_and_may_differ": useful.endpoint.key.clock != other.endpoint.key.clock});
    let summary = json!({
        "schema": "smb-retention-calibration-v1", "mode": match request.mode { Mode::History => "history", Mode::Deadline => "deadline" },
        "scope": "local retention and supplied-suffix diagnostic",
        "claim_limit": "does not establish search-discovered continuation, full route reliability, or general level solvability",
        "continuation_role": "identical supplied witness suffix; not searcher input or a search oracle",
        "broken": request.broken,
        "key_policy": match (request.mode, request.broken) {
            (Mode::History, true) => "loop_on_path_cleared_only",
            (Mode::History, false) => "production_smb_key",
            (Mode::Deadline, true) => "clock_reversed_only",
            (Mode::Deadline, false) => "production_smb_key",
        },
        "control_field": match request.mode {
            Mode::History => "loop_on_path only",
            Mode::Deadline => "archive-key clock ranking reversed, information preserved",
        },
        "normalization_policy": match request.mode {
            Mode::History => "history_bit_cleared_for_endpoint_alignment_only",
            Mode::Deadline => "clock_zeroed_for_endpoint_alignment_only",
        },
        "rom_sha256": rom_sha256, "core_sha256": core_sha256,
        "useful_prefix_sha256": useful.input.sha256, "other_prefix_sha256": other.input.sha256,
        "competitor_prefix_sha256": competitor.input.sha256,
        "continuation_sha256": continuation.sha256,
        "useful_prefix_actions": useful.input.actions.len(), "other_prefix_actions": other.input.actions.len(),
        "competitor_prefix_actions": competitor.input.actions.len(), "total_prefix_actions": total_prefix_actions,
        "total_prefix_action_limit": TOTAL_PREFIX_ACTION_LIMIT,
        "continuation_actions": continuation.actions.len(), "useful_prefix_frames": useful.input.frames,
        "other_prefix_frames": other.input.frames, "competitor_prefix_frames": competitor.input.frames,
        "total_prefix_frames": total_prefix_frames, "total_prefix_frame_limit": TOTAL_PREFIX_FRAME_LIMIT,
        "continuation_frames": continuation.frames,
        "population_start_action": request.population_start_action,
        "objective": {"world": request.objective.world, "level": request.objective.level,
            "area_type": request.objective.area_type, "absolute_player_x_min": request.objective.absolute_player_x_min},
        "seed": request.seed, "prefix_endpoints": {"useful": state_json(&useful.endpoint),
            "other": state_json(&other.endpoint), "competitor": state_json(&competitor.endpoint)},
        "pair_checks": pair_checks,
        "continuation_results": {"useful": useful_continuation, "other": other_continuation,
            "competitor": competitor_continuation},
        "populated_archive_trials": [trial_useful_first, trial_other_first],
        "population_sampling": {"maximum_saved_states_per_prefix": MAX_POPULATION_SAMPLES_PER_PREFIX,
            "actual_matching_context_states": population.len(),
            "normalized_pair_cell_excluded": true, "warmup_candidate_cap": ARCHIVE_CAPACITY - 3},
    });
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_dir.join("summary.json"))?;
    output.write_all(&serde_json::to_vec_pretty(&summary)?)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: smb-retention-calibrate REQUEST.json OUTPUT_DIR".into());
    }
    run(Path::new(&args[0]), Path::new(&args[1]))
}

#[cfg(test)]
mod tests {
    use super::{
        AREA_TYPE_OFFSET, InputFile, LEVEL_OFFSET, Mode, Objective, PLAYER_X_OFFSET,
        PLAYER_X_PAGE_OFFSET, Request, WORLD_OFFSET, WRAM_SIZE, effective_key, normalized_location,
        raw_objective, same_normalized_slot,
    };
    use nes_workload::smb::archive::SmbArchiveKey;

    #[test]
    fn strict_request_and_input_reject_unknown_fields() {
        let request = r#"{
            "rom_path":"rom.nes","rom_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "core_path":"core.dylib","core_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "useful_prefix":"useful.json","other_prefix":"other.json",
            "competitor_prefix":"competitor.json","continuation":"suffix.json",
            "objective":{"world":7,"level":4,"area_type":2,"absolute_player_x_min":1000},
            "population_start_action":100,"seed":17,"broken":false,"unexpected":true
        }"#;
        assert!(serde_json::from_str::<Request>(request).is_err());
        let valid_request = request.replace(",\"unexpected\":true", "");
        let default_mode: Request = serde_json::from_str(&valid_request).unwrap();
        assert_eq!(default_mode.mode, Mode::History);
        assert!(
            serde_json::from_str::<Request>(
                &valid_request.replace("\"broken\":false", "\"broken\":false,\"mode\":\"unknown\"")
            )
            .is_err()
        );
        let deadline_request =
            valid_request.replace("\"broken\":false", "\"broken\":false,\"mode\":\"deadline\"");
        assert_eq!(
            serde_json::from_str::<Request>(&deadline_request)
                .unwrap()
                .mode,
            Mode::Deadline
        );
        assert!(
            serde_json::from_str::<InputFile>(
                r#"{"actions":[{"buttons":128,"hold_frames":8,"unexpected":1}]}"#
            )
            .is_err()
        );
    }

    #[test]
    fn history_mode_preserves_legacy_bit_only_ablation() {
        let key = SmbArchiveKey {
            world: 7,
            level: 4,
            progress: 321,
            player_y_bucket: 8,
            loop_on_path: true,
            room_x_bucket: 3,
            time_bucket: 9,
            clock: 456,
            room: [1, 2, 3],
        };
        let mut expected = key;
        expected.loop_on_path = false;
        assert_eq!(effective_key(key, true, Mode::History), expected);
        assert_eq!(effective_key(key, false, Mode::History), key);
    }

    #[test]
    fn deadline_mode_normalizes_alignment_and_reverses_only_clock_preference() {
        let key = SmbArchiveKey {
            world: 7,
            level: 4,
            progress: 321,
            player_y_bucket: 8,
            loop_on_path: true,
            room_x_bucket: 3,
            time_bucket: 9,
            clock: 456,
            room: [1, 2, 3],
        };
        let mut expected_broken = key;
        expected_broken.clock = 543;
        assert_eq!(effective_key(key, true, Mode::Deadline), expected_broken);
        assert_eq!(effective_key(key, false, Mode::Deadline), key);

        let mut expected_normalized = key;
        expected_normalized.clock = 0;
        assert_eq!(
            normalized_location(key, Mode::Deadline),
            expected_normalized
        );
        let later = SmbArchiveKey { clock: 999, ..key };
        assert!(same_normalized_slot(key, later, Mode::Deadline));
        assert!(same_normalized_slot(key, later, Mode::History));
        assert_eq!(normalized_location(key, Mode::History).clock, key.clock);
        assert_eq!(normalized_location(later, Mode::History).clock, later.clock);

        let different_history = SmbArchiveKey {
            loop_on_path: false,
            ..key
        };
        assert!(!same_normalized_slot(
            key,
            different_history,
            Mode::Deadline
        ));
        let different_bucket = SmbArchiveKey {
            time_bucket: 10,
            ..key
        };
        assert!(!same_normalized_slot(key, different_bucket, Mode::Deadline));
    }

    #[test]
    fn objective_reads_only_raw_wram_coordinates_and_requires_alive_state() {
        let objective = Objective {
            world: 7,
            level: 4,
            area_type: 2,
            absolute_player_x_min: 1000,
        };
        let mut wram = [0_u8; WRAM_SIZE];
        wram[WORLD_OFFSET] = objective.world;
        wram[LEVEL_OFFSET] = objective.level;
        wram[AREA_TYPE_OFFSET] = objective.area_type;
        wram[PLAYER_X_PAGE_OFFSET] = 3;
        wram[PLAYER_X_OFFSET] = 240;
        assert!(raw_objective(&wram, objective));
        wram[0x000e] = 0x0b;
        assert!(!raw_objective(&wram, objective));
    }
}
