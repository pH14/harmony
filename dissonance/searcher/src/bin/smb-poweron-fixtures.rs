// SPDX-License-Identifier: AGPL-3.0-or-later

//! Evaluator-only fixture capture from a controller prefix replayed at NES power-on.

use std::{env, error::Error, fs, path::PathBuf};

use machine::{Machine, StopConditions, nes, quicknes::QuickNesMachine};
use searcher::smb::{
    campaign::{
        SNAPSHOT_CHECKPOINT_FORMAT, SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry,
        SmbTerminalPredicate,
    },
    target::{
        ROOM_IDENTITY_BYTES, SmbSnapshot, smb_mechanical_state_from_wram, smb_milestones_from_wram,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct ChallengeDescriptor {
    format: &'static str,
    id: String,
    checkpoint_file: &'static str,
    checkpoint_sha256: String,
    rom_sha256: String,
    emulator_backend: String,
    terminal_policy: String,
    workers: u32,
    action_limit: usize,
    screen_budget: u64,
}

#[derive(Serialize)]
struct PrivateManifest {
    format: &'static str,
    logical_checkpoint_path: String,
    prefix_origin: &'static str,
    prefix_path: String,
    prefix_sha256: String,
    source_frame: u64,
    expected_world: u8,
    expected_level: u8,
    expected_progress: u16,
    workers: u32,
    action_limit: usize,
    screen_budget: u64,
    rom_sha256: String,
    emulator_backend: String,
    checkpoint_sha256: String,
    trace_sha256: String,
}

struct TargetFixture {
    id: String,
    world: u8,
    level: u8,
    output: PathBuf,
    captured: bool,
}

#[derive(Deserialize)]
struct PowerOnPrefix {
    actions: Vec<nes::ButtonChord>,
    #[serde(default)]
    pokes: Vec<WramPoke>,
    poke_after_frame: Option<u64>,
    #[serde(default)]
    capture_after_frame: u64,
}

#[derive(Deserialize)]
struct WramPoke {
    address: usize,
    value: u8,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let prefix_path = PathBuf::from(
        args.next()
            .ok_or("usage: smb-poweron-fixtures <prefix.json> <id:world:level:output>...")?,
    );
    let mut targets = Vec::new();
    for value in args {
        let value = value.to_string_lossy();
        let mut fields = value.splitn(4, ':');
        let id = fields.next().ok_or("missing fixture id")?.to_owned();
        let world = fields.next().ok_or("missing world")?.parse()?;
        let level = fields.next().ok_or("missing level")?.parse()?;
        let output = PathBuf::from(fields.next().ok_or("missing output")?);
        targets.push(TargetFixture {
            id,
            world,
            level,
            output,
            captured: false,
        });
    }
    if targets.is_empty() {
        return Err("at least one target fixture is required".into());
    }

    let rom_path = PathBuf::from(env::var_os("HARMONY_SMB_ROM").ok_or("missing SMB ROM")?);
    let core_path =
        PathBuf::from(env::var_os("HARMONY_QUICKNES_CORE").ok_or("missing QuickNES core")?);
    let rom = fs::read(&rom_path)?;
    let core_sha256 = file_sha256(&core_path)?;
    let rom_sha256 = sha256(&rom);
    let emulator_backend = format!(
        "quicknes:{}:{}",
        machine::quicknes::QUICKNES_REVISION,
        core_sha256
    );
    let prefix_bytes = fs::read(&prefix_path)?;
    let prefix: PowerOnPrefix = serde_json::from_slice(&prefix_bytes)?;
    if prefix
        .pokes
        .iter()
        .any(|poke| poke.address >= nes::WRAM_SIZE)
    {
        return Err("power-on prefix contains an out-of-range WRAM poke".into());
    }
    if prefix.pokes.is_empty() != prefix.poke_after_frame.is_none() {
        return Err("power-on prefix must specify pokes and poke_after_frame together".into());
    }
    let prefix_sha256 = sha256(&prefix_bytes);
    let mut machine = QuickNesMachine::from_rom_bytes(&rom, &core_path, &core_sha256)?;
    let mut current = machine.snapshot()?;
    let mut frame = 0_u64;
    let mut trace = Sha256::new();
    let mut previous_route = None;
    let mut previous_engine = None;
    let mut deepest_route: Option<(u8, u8, u16)> = None;
    let mut first_death = None;

    for action in &prefix.actions {
        for _ in 0..action.bounded_hold_frames() {
            let one = nes::ButtonChord::new(action.buttons, 1);
            machine.branch(current, &nes::reproducer(&[one]))?;
            machine.run(StopConditions::default(), None)?;
            machine.drop_snapshot(current)?;
            current = machine.snapshot()?;
            frame = frame.saturating_add(1);
            if prefix.poke_after_frame == Some(frame) {
                for poke in &prefix.pokes {
                    machine.poke_wram(poke.address, poke.value);
                }
                machine.drop_snapshot(current)?;
                current = machine.snapshot()?;
            }
            let wram = machine.read_wram()?;
            let state = smb_mechanical_state_from_wram(&wram);
            let route = (state.world, state.level);
            if previous_route != Some(route) {
                eprintln!(
                    "route frame={frame} world={} level={} progress={} engine={} dead={} flag={}",
                    state.world,
                    state.level,
                    state.progress,
                    state.player_engine_state,
                    state.dead,
                    state.flag_active
                );
                previous_route = Some(route);
            }
            if previous_engine != Some(state.player_engine_state) {
                if frame <= 1_000 || state.player_engine_state <= 8 {
                    eprintln!(
                        "engine frame={frame} value={} world={} level={} progress={} dead={}",
                        state.player_engine_state,
                        state.world,
                        state.level,
                        state.progress,
                        state.dead
                    );
                }
                previous_engine = Some(state.player_engine_state);
            }
            if state.world < 8 && state.level < 4 {
                deepest_route = Some(deepest_route.unwrap_or_default().max((
                    state.world,
                    state.level,
                    state.progress,
                )));
            }
            if state.world < 8 && state.level < 4 && state.dead && first_death.is_none() {
                first_death = Some((frame, state.world, state.level, state.progress));
                eprintln!(
                    "first-death frame={frame} world={} level={} progress={}",
                    state.world, state.level, state.progress
                );
            }
            let encoded = postcard::to_allocvec(&(frame, state))?;
            trace.update(u64::try_from(encoded.len())?.to_le_bytes());
            trace.update(encoded);
            for target in targets.iter_mut().filter(|target| !target.captured) {
                if frame < prefix.capture_after_frame
                    || (state.world, state.level, state.progress) != (target.world, target.level, 0)
                    || state.dead
                    || !matches!(state.player_engine_state, 7 | 8)
                {
                    continue;
                }
                let raw = machine.take_snapshot(current)?;
                current = machine.import_snapshot(&raw);
                let snapshot = snapshot_from_raw(raw, &wram, frame)?;
                write_fixture(
                    target,
                    &snapshot,
                    &prefix_path,
                    &prefix_sha256,
                    frame,
                    &rom_sha256,
                    &emulator_backend,
                    &format!("{:x}", trace.clone().finalize()),
                )?;
                target.captured = true;
            }
            if targets.iter().all(|target| target.captured) {
                return Ok(());
            }
        }
    }
    if prefix
        .poke_after_frame
        .is_some_and(|poke_frame| poke_frame > frame)
    {
        return Err("power-on prefix ended before its WRAM poke frame".into());
    }
    let missing = targets
        .iter()
        .filter(|target| !target.captured)
        .map(|target| target.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    eprintln!(
        "summary frames={frame} deepest={deepest_route:?} first_death={first_death:?} trace_sha256={:x}",
        trace.finalize()
    );
    Err(format!("power-on prefix ended before fixtures were captured: {missing}").into())
}

fn snapshot_from_raw(
    emulator_state: Vec<u8>,
    wram: &[u8; nes::WRAM_SIZE],
    frame: u64,
) -> Result<SmbSnapshot, Box<dyn Error>> {
    let state = smb_mechanical_state_from_wram(wram);
    Ok(serde_json::from_value(serde_json::json!({
        "emulator_state": emulator_state,
        "observation": {
            "frame_count": frame,
            "decoded": state,
            "milestones": smb_milestones_from_wram(wram),
            "changed_indices": [],
            "dead": false,
            "log_line": format!("frame={frame} changed=[]"),
        },
        "room_area": [wram[ROOM_IDENTITY_BYTES[0]], wram[ROOM_IDENTITY_BYTES[1]]],
        "dead": false,
        "failed": false,
    }))?)
}

#[allow(clippy::too_many_arguments)]
fn write_fixture(
    target: &TargetFixture,
    snapshot: &SmbSnapshot,
    prefix_path: &std::path::Path,
    prefix_sha256: &str,
    frame: u64,
    rom_sha256: &str,
    emulator_backend: &str,
    trace_sha256: &str,
) -> Result<(), Box<dyn Error>> {
    fs::create_dir_all(&target.output)?;
    for name in ["checkpoint.bin", "private-manifest.json", "challenge.json"] {
        if target.output.join(name).exists() {
            return Err(format!("refusing to overwrite {}", target.output.display()).into());
        }
    }
    let checkpoint = SmbSnapshotCheckpoint {
        format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
        entries: vec![SmbSnapshotCheckpointEntry {
            id: 0,
            snapshot: snapshot.clone(),
        }],
    };
    let checkpoint_bytes = checkpoint.to_bytes()?;
    let checkpoint_sha256 = sha256(&checkpoint_bytes);
    let challenge = ChallengeDescriptor {
        format: "dissonance-fixture-challenge-v2",
        id: target.id.clone(),
        checkpoint_file: "checkpoint.bin",
        checkpoint_sha256: checkpoint_sha256.clone(),
        rom_sha256: rom_sha256.to_owned(),
        emulator_backend: emulator_backend.to_owned(),
        terminal_policy: SmbTerminalPredicate::LevelTransition {
            world: target.world,
            level: target.level,
        }
        .identifier(),
        workers: 12,
        action_limit: 512,
        screen_budget: 30_000,
    };
    let manifest = PrivateManifest {
        format: "dissonance-fixture-private-power-on-v1",
        logical_checkpoint_path: target.id.clone(),
        prefix_origin: "quicknes-power-on",
        prefix_path: prefix_path.to_string_lossy().into_owned(),
        prefix_sha256: prefix_sha256.to_owned(),
        source_frame: frame,
        expected_world: target.world,
        expected_level: target.level,
        expected_progress: 0,
        workers: 12,
        action_limit: 512,
        screen_budget: 30_000,
        rom_sha256: rom_sha256.to_owned(),
        emulator_backend: emulator_backend.to_owned(),
        checkpoint_sha256,
        trace_sha256: trace_sha256.to_owned(),
    };
    fs::write(target.output.join("checkpoint.bin"), checkpoint_bytes)?;
    fs::write(
        target.output.join("private-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(
        target.output.join("challenge.json"),
        serde_json::to_vec_pretty(&challenge)?,
    )?;
    println!("captured {} at frame {frame}", target.id);
    Ok(())
}

fn file_sha256(path: &std::path::Path) -> Result<String, Box<dyn Error>> {
    Ok(sha256(&fs::read(path)?))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
