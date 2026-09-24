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
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PrefixInput {
    actions: Vec<ChordInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChordInput {
    buttons: u8,
    hold_frames: u8,
}

#[derive(Clone)]
struct Sample {
    count: usize,
    actions: Vec<ButtonChord>,
    snapshot: MetroidSnapshot,
    state: MetroidMechanicalState,
    key: MetroidArchiveKey,
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

fn assert_snapshot_replay(
    target: &mut MetroidTarget,
    before: &Sample,
    after: &Sample,
    next_action: ButtonChord,
) -> Result<(), Box<dyn Error>> {
    target.restore(&before.snapshot)?;
    if target.mechanical_state() != before.state {
        return Err("before-checkpoint restore changed the decoded state".into());
    }
    target.apply(&next_action);
    if target.exit_kind() != ExitKind::Ok || target.mechanical_state() != after.state {
        return Err(
            "one-action replay from the before checkpoint did not reproduce after state".into(),
        );
    }
    target.restore(&after.snapshot)?;
    if target.mechanical_state() != after.state {
        return Err("after-checkpoint restore changed the decoded state".into());
    }
    Ok(())
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
) -> Result<Vec<Value>, Box<dyn Error>> {
    let (reports, snapshots) = archive.take_entry_reports_and_snapshots();
    let states: BTreeMap<u64, MetroidMechanicalState> = snapshots
        .into_iter()
        .map(|(id, snapshot)| (id, snapshot.state()))
        .collect();
    selected
        .iter()
        .map(|index| {
            let report = reports
                .get(*index)
                .ok_or("selected archive index has no insertion report")?;
            let id = report.id;
            let state = states
                .get(&id)
                .copied()
                .ok_or("selected retained entry has no actual emulator snapshot")?;
            Ok(json!({
                "entry_id": id,
                "state": state_json(state),
                "key_location": key_location_json(report.key),
            }))
        })
        .collect()
}

fn pair_trial(
    before: &Sample,
    after: &Sample,
    seed: u64,
    broken: bool,
    before_first: bool,
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
    let selected = archive_snapshot_states(&mut archive, &[selected_index])?
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

fn validate_checkpoints(before: usize, after: usize) -> Result<(), Box<dyn Error>> {
    if before == 0 || after != before.saturating_add(1) || after >= PREFIX_ACTION_LIMIT {
        return Err(
            "action checkpoints must be consecutive, nonzero, and below the selection action cap"
                .into(),
        );
    }
    Ok(())
}

fn run(request_path: &Path, output_dir: &Path) -> Result<(), Box<dyn Error>> {
    let request: Request = serde_json::from_slice(&read_bounded(request_path, REQUEST_LIMIT)?)?;
    validate_checkpoints(request.before_actions, request.after_actions)?;

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
        actions[request.before_actions],
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
    let before_first = pair_trial(&before, &after, request.seed, request.broken, true)?;
    let after_first = pair_trial(&before, &after, request.seed, request.broken, false)?;

    let populated_active = parentless_archive.active_count();
    let populated_snapshots = parentless_archive.resident_snapshot_count();
    let mut random = RomuDuoJrRand::with_seed(request.seed);
    let mut selected_ids = Vec::with_capacity(SELECTOR_SAMPLES);
    for _ in 0..SELECTOR_SAMPLES {
        let (id, draw) = parentless_archive.select_parent(&mut random, MAX_METROID_ACTIONS)?;
        parentless_archive.record_selection(id, &draw);
        selected_ids.push(id);
    }
    let population_states = archive_snapshot_states(&mut parentless_archive, &selected_ids)?;
    let selected_missiles: Vec<u64> = population_states
        .iter()
        .filter_map(|entry| entry["state"]["missiles"].as_u64())
        .collect();

    fs::create_dir_all(output_dir)?;
    let summary = json!({
        "schema": "metroid-retention-calibration-v1",
        "scope": "actual snapshot retention and seeded archive parent selection only",
        "claim_limit": "does not establish door opening, route completion, or full resource calibration",
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
            "selector_seed": request.seed,
            "selection_samples": population_states,
            "selected_actual_missiles_min": selected_missiles.iter().copied().min(),
            "selected_actual_missiles_max": selected_missiles.iter().copied().max(),
        },
        "snapshot_replay": "before restored, next real action reproduced after state, after restored",
    });
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
    use super::{MetroidArchiveKey, PrefixInput, Request, experiment_key, validate_checkpoints};

    #[test]
    fn checkpoints_must_leave_room_for_parent_selection() {
        assert!(validate_checkpoints(8190, 8191).is_ok());
        assert!(validate_checkpoints(8191, 8192).is_err());
        assert!(validate_checkpoints(0, 1).is_err());
        assert!(validate_checkpoints(17, 19).is_err());
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
}
