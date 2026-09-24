// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use nes_workload::{
    metroid::{
        archive::{
            MAX_METROID_ACTIONS, MetroidArchive, MetroidArchiveKey, MetroidMilestones, archive_key,
            chord_time, milestones,
        },
        target::{
            ButtonChord, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy,
        },
    },
    search::{
        archive::{ArchiveCandidate, ArchiveKey},
        rand::RomuDuoJrRand,
    },
    target::{ExitKind, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REQUEST_LIMIT: u64 = 64 * 1024;
const PREFIX_LIMIT: u64 = 1024 * 1024;
const ROM_LIMIT: u64 = 16 * 1024 * 1024;
const CORE_LIMIT: u64 = 128 * 1024 * 1024;
const PREFIX_ACTION_LIMIT: usize = MAX_METROID_ACTIONS;
const PREFIX_FRAME_LIMIT: u64 = 1_000_000;
const CONTINUATION_ACTION_LIMIT: usize = 128;
const CONTINUATION_FRAME_LIMIT: u64 = 15_360;
const ARCHIVE_CAPACITY: usize = 512;
const SELECTOR_SAMPLES: usize = 16;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    rom_path: PathBuf,
    rom_sha256: String,
    core_path: PathBuf,
    core_sha256: String,
    prefix_input: PathBuf,
    before_actions: usize,
    after_actions: usize,
    seed: u64,
    broken: bool,
    #[serde(default)]
    continuation: Option<PathBuf>,
    #[serde(default)]
    objective: Option<Objective>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixInput {
    actions: Vec<ChordInput>,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChordInput {
    buttons: u8,
    hold_frames: u8,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
struct Objective {
    area: u8,
    map_x: u8,
    map_y: u8,
}

struct Continuation {
    actions: Vec<ButtonChord>,
    frames: u64,
    sha256: String,
}

#[derive(Clone)]
struct Sample {
    count: usize,
    actions: Vec<ButtonChord>,
    snapshot: MetroidSnapshot,
    state: MetroidMechanicalState,
    key: MetroidArchiveKey,
    population_archive_id: Option<usize>,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, Box<dyn Error>> {
    let file = fs::File::open(path)?;
    let size = file.metadata()?.len();
    if size > limit {
        return Err(format!("input exceeds its {limit}-byte limit").into());
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size)?);
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > limit {
        return Err(format!("input exceeds its {limit}-byte limit").into());
    }
    Ok(bytes)
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_optional_continuation(
    continuation_present: bool,
    objective_present: bool,
) -> Result<(), Box<dyn Error>> {
    if continuation_present != objective_present {
        return Err(
            "continuation and objective must either both be present or both be absent".into(),
        );
    }
    Ok(())
}

fn parse_continuation(bytes: &[u8]) -> Result<Continuation, Box<dyn Error>> {
    if u64::try_from(bytes.len())? > PREFIX_LIMIT {
        return Err("continuation exceeds its 1 MiB input bound".into());
    }
    let input: PrefixInput = serde_json::from_slice(bytes)?;
    if input.actions.len() > CONTINUATION_ACTION_LIMIT {
        return Err(format!("continuation exceeds {CONTINUATION_ACTION_LIMIT} actions").into());
    }
    let mut frames = 0_u64;
    let mut actions = Vec::with_capacity(input.actions.len());
    for action in input.actions {
        if action.hold_frames == 0 || action.hold_frames > 120 {
            return Err("continuation hold_frames must be in 1..=120".into());
        }
        frames = frames
            .checked_add(u64::from(action.hold_frames))
            .ok_or("continuation frame count overflowed")?;
        if frames > CONTINUATION_FRAME_LIMIT {
            return Err(format!("continuation exceeds {CONTINUATION_FRAME_LIMIT} frames").into());
        }
        actions.push(ButtonChord::new(action.buttons, action.hold_frames));
    }
    Ok(Continuation {
        actions,
        frames,
        sha256: sha256(bytes),
    })
}

fn objective_reached(
    state: MetroidMechanicalState,
    objective: Objective,
    terminal_policy: MetroidTerminalPolicy,
) -> bool {
    !terminal_policy.is_dead(state)
        && state.area == objective.area
        && state.map_x == objective.map_x
        && state.map_y == objective.map_y
}

fn evaluate_continuation(
    target: &mut MetroidTarget,
    continuation: &Continuation,
    objective: Objective,
    terminal_policy: MetroidTerminalPolicy,
) -> Result<Value, Box<dyn Error>> {
    let mut actions_executed = 0_u64;
    let mut frames_executed = 0_u64;
    let mut requested_frames = 0_u64;
    let mut state = target.mechanical_state();
    let mut reached = objective_reached(state, objective, terminal_policy);
    let mut terminal = if terminal_policy.is_dead(state) {
        "dead"
    } else if target.is_victory() {
        "victory"
    } else {
        "running"
    };
    for action in &continuation.actions {
        if reached || terminal != "running" {
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
        state = target.mechanical_state();
        reached = objective_reached(state, objective, terminal_policy);
        if terminal_policy.is_dead(state) {
            terminal = "dead";
        } else if target.is_victory() {
            terminal = "victory";
        }
    }
    state = target.mechanical_state();
    Ok(json!({
        "status": if reached { "objective_reached" } else { terminal },
        "objective_reached_alive": reached,
        "alive": !terminal_policy.is_dead(state),
        "terminal": terminal,
        "actions_executed": actions_executed,
        "frames_executed": frames_executed,
        "requested_frames": requested_frames,
        "final_state": state_json(state),
    }))
}

fn evaluate_snapshot(
    target: &mut MetroidTarget,
    snapshot: &MetroidSnapshot,
    continuation: &Continuation,
    objective: Objective,
    terminal_policy: MetroidTerminalPolicy,
) -> Result<Value, Box<dyn Error>> {
    target.restore(snapshot)?;
    if target.mechanical_state() != snapshot.state() {
        return Err("selected snapshot restore changed its decoded state".into());
    }
    evaluate_continuation(target, continuation, objective, terminal_policy)
}

fn checked_digest(actual: &str, expected: &str, label: &str) -> Result<(), Box<dyn Error>> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} SHA-256 must be 64 hexadecimal characters").into());
    }
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!("{label} SHA-256 mismatch").into());
    }
    Ok(())
}

fn experiment_key(mut key: MetroidArchiveKey, broken: bool) -> MetroidArchiveKey {
    if broken {
        key.missiles = 0;
    }
    key
}

fn state_json(state: MetroidMechanicalState) -> Value {
    json!({
        "area": state.area,
        "map_x": state.map_x,
        "map_y": state.map_y,
        "x": state.x,
        "y": state.y,
        "health": state.health,
        "missiles": state.missiles,
        "missile_capacity": state.missile_capacity,
        "energy_tanks": state.energy_tanks,
        "equipment": state.equipment,
        "mode": state.mode,
        "pose": state.pose,
        "door": state.door,
    })
}

fn key_location_json(key: MetroidArchiveKey) -> Value {
    json!({
        "place": [key.area, key.map_x, key.map_y, key.boss_damage, key.columns],
        "identity": [key.x, key.y, key.posture, key.door],
        "progress": key.items,
    })
}

fn insert_pair_candidate(
    archive: &mut MetroidArchive,
    sample: &Sample,
    broken: bool,
) -> Result<Option<usize>, Box<dyn Error>> {
    let candidate = ArchiveCandidate {
        suffix: sample.actions.clone(),
        key: experiment_key(sample.key, broken),
        milestones: MetroidMilestones::default(),
    };
    archive.insert(
        None,
        u64::try_from(sample.count)?,
        candidate,
        sample.snapshot.clone(),
    )
}

fn archive_snapshot_states(
    archive: &mut MetroidArchive,
    selected: &[usize],
    target: &mut MetroidTarget,
    continuation: Option<&Continuation>,
    objective: Option<Objective>,
    terminal_policy: MetroidTerminalPolicy,
) -> Result<Vec<Value>, Box<dyn Error>> {
    let (reports, snapshots) = archive.take_entry_reports_and_snapshots();
    let states: BTreeMap<u64, MetroidSnapshot> = snapshots.into_iter().collect();
    selected
        .iter()
        .map(|index| {
            let report = reports
                .get(*index)
                .ok_or("selected archive index has no insertion report")?;
            let id = report.id;
            let Some(snapshot) = states.get(&id) else {
                return Ok(json!({
                    "archive_index": index,
                    "entry_id": id,
                    "active": true,
                    "snapshot_available": false,
                    "state": Value::Null,
                    "key_location": key_location_json(report.key),
                    "shared_continuation": Value::Null,
                }));
            };
            let state = snapshot.state();
            target.restore(snapshot)?;
            if target.mechanical_state() != state {
                return Err("selected actual snapshot state could not be restored".into());
            }
            let mut entry = json!({
                "archive_index": index,
                "entry_id": id,
                "active": true,
                "snapshot_available": true,
                "state": state_json(state),
                "key_location": key_location_json(report.key),
            });
            if let (Some(continuation), Some(objective)) = (continuation, objective) {
                entry["shared_continuation"] =
                    evaluate_snapshot(target, snapshot, continuation, objective, terminal_policy)?;
            }
            Ok(entry)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn pair_trial(
    before: &Sample,
    after: &Sample,
    seed: u64,
    broken: bool,
    before_first: bool,
    target: &mut MetroidTarget,
    continuation: Option<&Continuation>,
    objective: Option<Objective>,
    terminal_policy: MetroidTerminalPolicy,
) -> Result<Value, Box<dyn Error>> {
    let mut archive = MetroidArchive::new(chord_time);
    archive.max_entries = ARCHIVE_CAPACITY;
    let (first, second) = if before_first {
        (before, after)
    } else {
        (after, before)
    };
    let first_id = insert_pair_candidate(&mut archive, first, broken)?;
    let second_id = insert_pair_candidate(&mut archive, second, broken)?;
    let (before_id, after_id) = if before_first {
        (first_id, second_id)
    } else {
        (second_id, first_id)
    };
    let before_retained =
        before_id.is_some_and(|id| archive.active.get(id).copied().unwrap_or(false));
    let after_retained =
        after_id.is_some_and(|id| archive.active.get(id).copied().unwrap_or(false));
    let active_entries = archive.active_count();
    let mut random = RomuDuoJrRand::with_seed(seed);
    let (selected_index, draw) = archive.select_parent(&mut random, MAX_METROID_ACTIONS)?;
    archive.record_selection(selected_index, &draw);
    let selected = archive_snapshot_states(
        &mut archive,
        &[selected_index],
        target,
        continuation,
        objective,
        terminal_policy,
    )?
    .into_iter()
    .next()
    .ok_or("pair selector returned no retained snapshot")?;
    Ok(json!({
        "insertion_order": if before_first { "before_then_after" } else { "after_then_before" },
        "before_insert_returned_entry": before_id,
        "after_insert_returned_entry": after_id,
        "before_retained": before_retained,
        "after_retained": after_retained,
        "active_entries": active_entries,
        "selected_retained_snapshot": selected,
    }))
}

fn populated_endpoint_retention(archive: &MetroidArchive, sample: &Sample) -> Value {
    match sample.population_archive_id {
        Some(id) => match archive.active.get(id) {
            Some(active) => json!({
                "archive_id": id,
                "insert_returned_id": true,
                "active": active,
                "status": if *active { "active" } else { "inactive_after_insertion" },
                "state_inferred_from_archive": false,
            }),
            None => json!({
                "archive_id": id,
                "insert_returned_id": true,
                "active": false,
                "status": "id_not_present_in_active_table",
                "state_inferred_from_archive": false,
            }),
        },
        None => json!({
            "archive_id": Value::Null,
            "insert_returned_id": false,
            "active": false,
            "status": "not_inserted",
            "explanation": "archive.insert returned None; no retained endpoint state is inferred",
            "state_inferred_from_archive": false,
        }),
    }
}

fn validate_checkpoints(before: usize, after: usize) -> Result<(), Box<dyn Error>> {
    if before == 0 || after <= before || after >= PREFIX_ACTION_LIMIT {
        return Err(
            "action checkpoints must be ordered, nonzero, and below the selection action cap"
                .into(),
        );
    }
    Ok(())
}

fn assert_snapshot_replay(
    target: &mut MetroidTarget,
    before: &Sample,
    after: &Sample,
    actions: &[ButtonChord],
) -> Result<(), Box<dyn Error>> {
    target.restore(&before.snapshot)?;
    if target.mechanical_state() != before.state {
        return Err("before-checkpoint restore changed the decoded state".into());
    }
    for action in actions {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("checkpoint interval replay returned a target error".into());
        }
    }
    if target.mechanical_state() != after.state {
        return Err("checkpoint interval replay did not reproduce the after state".into());
    }
    target.restore(&after.snapshot)?;
    if target.mechanical_state() != after.state {
        return Err("after-checkpoint restore changed the decoded state".into());
    }
    Ok(())
}

fn run(request_path: &Path, output_dir: &Path) -> Result<(), Box<dyn Error>> {
    let request: Request = serde_json::from_slice(&read_bounded(request_path, REQUEST_LIMIT)?)?;
    validate_checkpoints(request.before_actions, request.after_actions)?;
    validate_optional_continuation(request.continuation.is_some(), request.objective.is_some())?;

    let rom = read_bounded(&request.rom_path, ROM_LIMIT)?;
    let core = read_bounded(&request.core_path, CORE_LIMIT)?;
    let rom_sha256 = sha256(&rom);
    let core_sha256 = sha256(&core);
    checked_digest(&rom_sha256, &request.rom_sha256, "ROM")?;
    checked_digest(&core_sha256, &request.core_sha256, "core")?;

    let prefix_bytes = read_bounded(&request.prefix_input, PREFIX_LIMIT)?;
    let input: PrefixInput = serde_json::from_slice(&prefix_bytes)?;
    if input.actions.len() != request.after_actions || input.actions.len() > PREFIX_ACTION_LIMIT {
        return Err(
            "prefix must end exactly at the requested checkpoint and stay within the fixed cap"
                .into(),
        );
    }
    let mut frame_count = 0_u64;
    let mut actions = Vec::with_capacity(input.actions.len());
    for chord in input.actions {
        if chord.hold_frames == 0 || chord.hold_frames > 120 {
            return Err("prefix hold_frames must be in 1..=120".into());
        }
        frame_count = frame_count.saturating_add(u64::from(chord.hold_frames));
        actions.push(ButtonChord::new(chord.buttons, chord.hold_frames));
    }
    if frame_count > PREFIX_FRAME_LIMIT {
        return Err("prefix exceeds the fixed frame cap".into());
    }
    let prefix_sha256 = sha256(&prefix_bytes);
    let continuation = if let Some(path) = &request.continuation {
        Some(parse_continuation(&read_bounded(path, PREFIX_LIMIT)?)?)
    } else {
        None
    };

    let terminal_policy = MetroidTerminalPolicy::BcdUnderflow;
    let mut target =
        MetroidTarget::from_rom_bytes_headless(&rom, &request.core_path, &core_sha256)?
            .with_terminal_policy(terminal_policy);
    let root_state = target.mechanical_state();
    let root_snapshot = target
        .snapshot()
        .ok_or("could not snapshot emulator root")?;
    let (genesis_items, genesis_tanks) = target.genesis_holdings();
    let mut parentless_archive = MetroidArchive::new(chord_time);
    parentless_archive.max_entries = ARCHIVE_CAPACITY;
    let mut parent_id = parentless_archive
        .insert(
            None,
            0,
            ArchiveCandidate {
                suffix: Vec::new(),
                key: experiment_key(archive_key(root_state), request.broken),
                milestones: milestones(root_state, genesis_items, genesis_tanks),
            },
            root_snapshot.clone(),
        )?
        .ok_or("archive did not retain initial gameplay state")?;
    let mut pending_suffix = Vec::new();
    let mut before = None;
    let mut after = None;
    let mut inserted_states = 1_u64;

    for (index, action) in actions.iter().enumerate().take(request.after_actions) {
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok || target.is_dead() || target.is_victory() {
            return Err(format!("prefix became terminal at action {}", index + 1).into());
        }
        let count = index + 1;
        let state = target.mechanical_state();
        let snapshot = target.snapshot().ok_or("could not snapshot prefix state")?;
        pending_suffix.push(*action);
        let key = archive_key(state);
        let archived = parentless_archive.insert(
            Some(parent_id),
            u64::try_from(count)?,
            ArchiveCandidate {
                suffix: pending_suffix.clone(),
                key: experiment_key(key, request.broken),
                milestones: milestones(state, genesis_items, genesis_tanks),
            },
            snapshot.clone(),
        )?;
        if let Some(id) = archived {
            parent_id = id;
            pending_suffix.clear();
            inserted_states = inserted_states.saturating_add(1);
        }
        if count == request.before_actions || count == request.after_actions {
            let sample = Sample {
                count,
                actions: actions[..count].to_vec(),
                snapshot,
                state,
                key,
                population_archive_id: archived,
            };
            if count == request.before_actions {
                before = Some(sample);
            } else {
                after = Some(sample);
            }
        }
    }
    let before = before.ok_or("before checkpoint was not observed")?;
    let after = after.ok_or("after checkpoint was not observed")?;
    assert_snapshot_replay(
        &mut target,
        &before,
        &after,
        &actions[request.before_actions..request.after_actions],
    )?;

    let same_place = before.key.place() == after.key.place();
    let same_identity = before.key.identity() == after.key.identity();
    let same_progress = before.key.progress() == after.key.progress();
    let mut before_key_without_missiles = before.key;
    before_key_without_missiles.missiles = 0;
    let mut after_key_without_missiles = after.key;
    after_key_without_missiles.missiles = 0;
    let same_key_except_missiles = before_key_without_missiles == after_key_without_missiles;
    let same_health = before.state.health == after.state.health;
    let missiles_increased = after.state.missiles > before.state.missiles;
    if !(same_place
        && same_identity
        && same_progress
        && same_key_except_missiles
        && same_health
        && missiles_increased)
    {
        return Err("observed pair lacks same-cell/identity/progress, missile-only key change, unchanged health, and increased actual missiles".into());
    }
    let direct_continuation = match (&continuation, request.objective) {
        (Some(continuation), Some(objective)) => Some(json!({
            "before": evaluate_snapshot(
                &mut target,
                &before.snapshot,
                continuation,
                objective,
                terminal_policy,
            )?,
            "after": evaluate_snapshot(
                &mut target,
                &after.snapshot,
                continuation,
                objective,
                terminal_policy,
            )?,
        })),
        (None, None) => None,
        _ => return Err("continuation and objective must be paired".into()),
    };
    let before_first = pair_trial(
        &before,
        &after,
        request.seed,
        request.broken,
        true,
        &mut target,
        continuation.as_ref(),
        request.objective,
        terminal_policy,
    )?;
    let after_first = pair_trial(
        &before,
        &after,
        request.seed,
        request.broken,
        false,
        &mut target,
        continuation.as_ref(),
        request.objective,
        terminal_policy,
    )?;

    let populated_active = parentless_archive.active_count();
    let populated_snapshots = parentless_archive.resident_snapshot_count();
    let endpoint_retention = json!({
        "before": populated_endpoint_retention(&parentless_archive, &before),
        "after": populated_endpoint_retention(&parentless_archive, &after),
    });
    let mut random = RomuDuoJrRand::with_seed(request.seed);
    let mut selected_ids = Vec::with_capacity(SELECTOR_SAMPLES);
    for _ in 0..SELECTOR_SAMPLES {
        let (id, draw) = parentless_archive.select_parent(&mut random, MAX_METROID_ACTIONS)?;
        parentless_archive.record_selection(id, &draw);
        selected_ids.push(id);
    }
    let compared_slot_ids: Vec<usize> = parentless_archive
        .active
        .iter()
        .enumerate()
        .filter_map(|(id, active)| {
            if !active {
                return None;
            }
            let key = parentless_archive.entry_key(id)?;
            (key.place() == before.key.place()
                && key.progress() == before.key.progress()
                && key.identity() == before.key.identity())
            .then_some(id)
        })
        .collect();
    let selection_sample_count = selected_ids.len();
    let mut snapshot_indices = selected_ids;
    snapshot_indices.extend(compared_slot_ids.iter().copied());
    let sampled_archive_states = archive_snapshot_states(
        &mut parentless_archive,
        &snapshot_indices,
        &mut target,
        continuation.as_ref(),
        request.objective,
        terminal_policy,
    )?;
    let selection_samples = sampled_archive_states[..selection_sample_count].to_vec();
    let compared_slot_snapshots = sampled_archive_states[selection_sample_count..].to_vec();
    let selected_missiles: Vec<u64> = selection_samples
        .iter()
        .filter_map(|entry| entry["state"]["missiles"].as_u64())
        .collect();
    let objective_successes = |states: &[Value]| {
        states
            .iter()
            .filter(|entry| entry["shared_continuation"]["objective_reached_alive"] == true)
            .count()
    };
    let selection_successes = continuation
        .as_ref()
        .map(|_| objective_successes(&selection_samples));
    let compared_slot_successes = continuation
        .as_ref()
        .map(|_| objective_successes(&compared_slot_snapshots));

    fs::create_dir_all(output_dir)?;
    let mut summary = json!({
        "schema": "metroid-retention-calibration-v1",
        "scope": if continuation.is_some() {
            "actual snapshot retention, seeded parent selection, and supplied-suffix diagnostic"
        } else {
            "actual snapshot retention and seeded archive parent selection only"
        },
        "claim_limit": if continuation.is_some() {
            "supplied suffix is diagnostic evidence, not search input; does not establish autonomous search or full route completion"
        } else {
            "does not establish door opening, route completion, or full resource calibration"
        },
        "broken": request.broken,
        "key_policy": if request.broken { "missiles_zeroed_only" } else { "production_metroid_key" },
        "terminal_policy": terminal_policy.identifier(),
        "rom_sha256": rom_sha256,
        "core_sha256": core_sha256,
        "prefix_sha256": prefix_sha256,
        "prefix_actions": actions.len(),
        "prefix_frames": frame_count,
        "before_actions": before.count,
        "after_actions": after.count,
        "before": state_json(before.state),
        "after": state_json(after.state),
        "pair_checks": {
            "same_archive_place": same_place,
            "same_archive_identity": same_identity,
            "same_archive_progress": same_progress,
            "same_key_except_missiles": same_key_except_missiles,
            "health_unchanged": same_health,
            "actual_missiles_strictly_increased": missiles_increased,
            "before_key_location": key_location_json(before.key),
            "after_key_location": key_location_json(after.key),
        },
        "pair_insertion_order_trials": [before_first, after_first],
        "populated_archive": {
            "capacity": ARCHIVE_CAPACITY,
            "active_entries": populated_active,
            "inserted_states": inserted_states,
            "resident_snapshots_before_selection": populated_snapshots,
            "endpoint_retention": endpoint_retention,
            "selector_seed": request.seed,
            "selection_sample_count": selection_samples.len(),
            "selection_shared_suffix_successes": selection_successes,
            "selection_samples": selection_samples,
            "compared_slot_active_indices": compared_slot_ids.len(),
            "compared_slot_snapshot_count": compared_slot_snapshots.len(),
            "compared_slot_shared_suffix_successes": compared_slot_successes,
            "compared_slot_snapshots": compared_slot_snapshots,
            "selected_actual_missiles_min": selected_missiles.iter().copied().min(),
            "selected_actual_missiles_max": selected_missiles.iter().copied().max(),
        },
        "snapshot_replay": {
            "before_restored": true,
            "interval_actions_reproduced_after": request.after_actions - request.before_actions,
            "after_restored": true,
        },
    });
    if let (Some(continuation), Some(objective), Some(endpoints)) =
        (&continuation, request.objective, direct_continuation)
    {
        summary["continuation_diagnostic"] = json!({
            "role": "fixed supplied suffix, used only for local diagnostic evaluation",
            "sha256": continuation.sha256,
            "actions": continuation.actions.len(),
            "frames": continuation.frames,
            "objective": {"area": objective.area, "map_x": objective.map_x, "map_y": objective.map_y},
            "pair_endpoint_results": endpoints,
        });
    }
    let mut report = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output_dir.join("summary.json"))?;
    report.write_all(&serde_json::to_vec_pretty(&summary)?)?;
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: metroid-retention-calibrate REQUEST.json OUTPUT_DIR".into());
    }
    run(Path::new(&args[0]), Path::new(&args[1]))
}

#[cfg(test)]
mod tests {
    use super::{
        CONTINUATION_ACTION_LIMIT, CONTINUATION_FRAME_LIMIT, MetroidArchiveKey,
        MetroidMechanicalState, MetroidTerminalPolicy, Objective, PrefixInput, Request,
        experiment_key, objective_reached, parse_continuation, validate_checkpoints,
        validate_optional_continuation,
    };

    #[test]
    fn checkpoint_interval_may_span_multiple_actions() {
        assert!(validate_checkpoints(8190, 8191).is_ok());
        assert!(validate_checkpoints(8191, 8192).is_err());
        assert!(validate_checkpoints(0, 1).is_err());
        assert!(validate_checkpoints(17, 19).is_ok());
        assert!(validate_checkpoints(19, 19).is_err());
    }

    #[test]
    fn request_and_prefix_schemas_reject_unknown_fields() {
        let request = r#"{
            "rom_path":"rom.nes","rom_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "core_path":"core.dylib","core_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
            "prefix_input":"prefix.json","before_actions":1274,"after_actions":1275,"seed":17,"broken":false,
            "extra":true
        }"#;
        assert!(serde_json::from_str::<Request>(request).is_err());
        assert!(
            serde_json::from_str::<Request>(&request.replace(",\n            \"extra\":true", ""))
                .is_ok()
        );
        let valid_request = request.replace(",\n            \"extra\":true", "");
        let with_continuation = valid_request.replace(
            "\"broken\":false",
            "\"broken\":false,\"continuation\":\"suffix.json\",\"objective\":{\"area\":2,\"map_x\":3,\"map_y\":4}",
        );
        assert!(serde_json::from_str::<Request>(&with_continuation).is_ok());
        assert!(validate_optional_continuation(false, false).is_ok());
        assert!(validate_optional_continuation(true, true).is_ok());
        assert!(validate_optional_continuation(true, false).is_err());
        assert!(validate_optional_continuation(false, true).is_err());
        let prefix = r#"{"actions":[{"buttons":0,"hold_frames":1,"extra":true}]}"#;
        assert!(serde_json::from_str::<PrefixInput>(prefix).is_err());
        assert!(
            serde_json::from_str::<PrefixInput>(r#"{"actions":[{"buttons":0,"hold_frames":1}]}"#)
                .is_ok()
        );
    }

    #[test]
    fn broken_control_changes_only_missiles_in_the_key() {
        let key = MetroidArchiveKey {
            items: 3,
            tanks: 4,
            health: 250,
            missiles: 9,
            ..MetroidArchiveKey::default()
        };
        let mut expected = key;
        expected.missiles = 0;
        assert_eq!(experiment_key(key, true), expected);
        assert_eq!(experiment_key(key, false), key);
    }

    #[test]
    fn continuation_enforces_action_frame_and_file_bounds() {
        let actions: Vec<_> = (0..CONTINUATION_ACTION_LIMIT)
            .map(|_| serde_json::json!({"buttons": 1, "hold_frames": 120}))
            .collect();
        let bytes = serde_json::to_vec(&serde_json::json!({"actions": actions})).unwrap();
        let parsed = parse_continuation(&bytes).unwrap();
        assert_eq!(parsed.actions.len(), CONTINUATION_ACTION_LIMIT);
        assert_eq!(parsed.frames, CONTINUATION_FRAME_LIMIT);

        let too_many: Vec<_> = (0..=CONTINUATION_ACTION_LIMIT)
            .map(|_| serde_json::json!({"buttons": 1, "hold_frames": 1}))
            .collect();
        let bytes = serde_json::to_vec(&serde_json::json!({"actions": too_many})).unwrap();
        assert!(parse_continuation(&bytes).is_err());
        assert!(parse_continuation(&vec![b' '; 1024 * 1024 + 1]).is_err());
        assert!(parse_continuation(br#"{"actions":[{"buttons":1,"hold_frames":0}]}"#).is_err());
        assert!(
            serde_json::from_str::<Objective>(r#"{"area":1,"map_x":2,"map_y":3,"extra":0}"#)
                .is_err()
        );
    }

    #[test]
    fn objective_requires_alive_state_at_exact_raw_map_coordinates() {
        let objective = Objective {
            area: 3,
            map_x: 8,
            map_y: 2,
        };
        let state = MetroidMechanicalState {
            area: 3,
            map_x: 8,
            map_y: 2,
            health: 99,
            ..MetroidMechanicalState::default()
        };
        assert!(objective_reached(
            state,
            objective,
            MetroidTerminalPolicy::BcdUnderflow
        ));
        assert!(!objective_reached(
            MetroidMechanicalState { health: 0, ..state },
            objective,
            MetroidTerminalPolicy::BcdUnderflow
        ));
        assert!(!objective_reached(
            MetroidMechanicalState { map_x: 7, ..state },
            objective,
            MetroidTerminalPolicy::BcdUnderflow
        ));
        let underflow_state = MetroidMechanicalState {
            health: 8_000,
            ..state
        };
        assert!(!objective_reached(
            underflow_state,
            objective,
            MetroidTerminalPolicy::BcdUnderflow
        ));
        assert!(objective_reached(
            underflow_state,
            objective,
            MetroidTerminalPolicy::Legacy
        ));
    }
}
