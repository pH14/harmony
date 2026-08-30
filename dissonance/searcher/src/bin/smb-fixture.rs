// SPDX-License-Identifier: AGPL-3.0-or-later

//! Evaluator-private extraction and replay certification for snapshot-root fixtures.

use std::{
    env,
    error::Error,
    fs,
    io::{BufReader, Read},
    path::{Path, PathBuf},
};

use searcher::{
    search::archive::Input,
    smb::{
        archive::SmbArchiveReport,
        campaign::{
            SNAPSHOT_CHECKPOINT_FORMAT, SmbSnapshotCheckpoint, SmbSnapshotCheckpointEntry,
            SmbTerminalPredicate,
        },
        target::{ButtonChord, SmbInput, SmbSnapshot, SmbTarget, smb_mechanical_state_from_wram},
    },
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const MANIFEST_FORMAT: &str = "dissonance-fixture-private-v1";
const CHALLENGE_FORMAT: &str = "dissonance-fixture-challenge-v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PrivateManifest {
    format: String,
    logical_checkpoint_path: String,
    source_archive_path: String,
    source_archive_sha256: String,
    source_entry_id: u64,
    base_prefix_path: Option<String>,
    expected_world: u8,
    expected_level: u8,
    expected_progress: u16,
    action_count: usize,
    rom_sha256: String,
    prefix_sha256: String,
    checkpoint_sha256: String,
    trace_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct ChallengeDescriptor {
    format: String,
    id: String,
    checkpoint_file: String,
    checkpoint_sha256: String,
    rom_sha256: String,
    terminal_policy: String,
    workers: u32,
    action_limit: usize,
    screen_budget: u64,
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let mode = args
        .next()
        .ok_or("usage: smb-fixture <extract|verify> ...")?;
    if mode == "extract" {
        return extract(&mut args);
    }
    if mode == "verify" {
        return verify(&mut args);
    }
    Err("unknown smb-fixture mode".into())
}

fn extract(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let source_path = PathBuf::from(args.next().ok_or("missing source archive path")?);
    let source_entry_id = parse_u64(&args.next().ok_or("missing source entry id")?)?;
    let expected_world = parse_u8(&args.next().ok_or("missing expected world")?)?;
    let expected_level = parse_u8(&args.next().ok_or("missing expected level")?)?;
    let logical_checkpoint_path = args
        .next()
        .ok_or("missing logical checkpoint path")?
        .to_string_lossy()
        .into_owned();
    if logical_checkpoint_path.is_empty() {
        return Err("logical checkpoint path must not be empty".into());
    }
    let output = PathBuf::from(args.next().ok_or("missing output directory")?);
    let action_limit = usize::try_from(parse_u64(&args.next().ok_or("missing action limit")?)?)?;
    let screen_budget = parse_u64(&args.next().ok_or("missing screen budget")?)?;
    let mut base_prefix_path = None;
    while let Some(flag) = args.next() {
        if flag == "--base-prefix" {
            base_prefix_path = Some(PathBuf::from(
                args.next().ok_or("missing --base-prefix value")?,
            ));
        } else {
            return Err("unexpected extract argument".into());
        }
    }

    let rom = read_rom()?;
    let source_archive_sha256 = file_sha256(&source_path)?;
    let report: SmbArchiveReport =
        serde_json::from_reader(BufReader::new(fs::File::open(&source_path)?))?;
    let entry = report
        .entries
        .iter()
        .find(|entry| entry.id == source_entry_id)
        .ok_or("source archive has no entry with the requested id")?;
    let mut prefix = base_prefix_path
        .as_deref()
        .map(read_prefix)
        .transpose()?
        .unwrap_or_default();
    prefix.actions.extend_from_slice(&entry.input.actions);
    let (snapshot, trace_sha256) = replay_prefix(&rom, &prefix, expected_world, expected_level, 0)?;
    let checkpoint = one_entry_checkpoint(snapshot);
    let prefix_bytes = serde_json::to_vec_pretty(&prefix)?;
    let checkpoint_bytes = checkpoint.to_bytes()?;
    let prefix_sha256 = sha256(&prefix_bytes);
    let checkpoint_sha256 = sha256(&checkpoint_bytes);
    let rom_sha256 = sha256(&rom);

    fs::create_dir_all(&output)?;
    for name in [
        "prefix.json",
        "checkpoint.bin",
        "private-manifest.json",
        "challenge.json",
    ] {
        if output.join(name).exists() {
            return Err(format!(
                "refusing to overwrite existing {}",
                output.join(name).display()
            )
            .into());
        }
    }
    let manifest = PrivateManifest {
        format: MANIFEST_FORMAT.to_owned(),
        logical_checkpoint_path: logical_checkpoint_path.clone(),
        source_archive_path: source_path.to_string_lossy().into_owned(),
        source_archive_sha256,
        source_entry_id,
        base_prefix_path: base_prefix_path.map(|path| path.to_string_lossy().into_owned()),
        expected_world,
        expected_level,
        expected_progress: 0,
        action_count: prefix.actions.len(),
        rom_sha256: rom_sha256.clone(),
        prefix_sha256,
        checkpoint_sha256: checkpoint_sha256.clone(),
        trace_sha256,
    };
    let challenge = ChallengeDescriptor {
        format: CHALLENGE_FORMAT.to_owned(),
        id: logical_checkpoint_path,
        checkpoint_file: "checkpoint.bin".to_owned(),
        checkpoint_sha256,
        rom_sha256,
        terminal_policy: SmbTerminalPredicate::LevelTransition {
            world: expected_world,
            level: expected_level,
        }
        .identifier(),
        workers: 12,
        action_limit,
        screen_budget,
    };
    fs::write(output.join("prefix.json"), prefix_bytes)?;
    fs::write(output.join("checkpoint.bin"), checkpoint_bytes)?;
    fs::write(
        output.join("private-manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    fs::write(
        output.join("challenge.json"),
        serde_json::to_vec_pretty(&challenge)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&challenge)?);
    Ok(())
}

fn verify(args: &mut impl Iterator<Item = std::ffi::OsString>) -> Result<(), Box<dyn Error>> {
    let directory = PathBuf::from(args.next().ok_or("missing fixture directory")?);
    if args.next().is_some() {
        return Err("unexpected verify argument".into());
    }
    let manifest: PrivateManifest =
        serde_json::from_slice(&fs::read(directory.join("private-manifest.json"))?)?;
    let challenge: ChallengeDescriptor =
        serde_json::from_slice(&fs::read(directory.join("challenge.json"))?)?;
    if manifest.format != MANIFEST_FORMAT || challenge.format != CHALLENGE_FORMAT {
        return Err("fixture manifest format is not recognized".into());
    }
    if challenge.id != manifest.logical_checkpoint_path
        || challenge.checkpoint_file != "checkpoint.bin"
        || challenge.checkpoint_sha256 != manifest.checkpoint_sha256
        || challenge.rom_sha256 != manifest.rom_sha256
        || challenge.workers != 12
    {
        return Err("private and worker-visible fixture descriptors disagree".into());
    }
    let expected_terminal = SmbTerminalPredicate::LevelTransition {
        world: manifest.expected_world,
        level: manifest.expected_level,
    }
    .identifier();
    if challenge.terminal_policy != expected_terminal {
        return Err("challenge terminal predicate disagrees with the private manifest".into());
    }

    let source_path = Path::new(&manifest.source_archive_path);
    if file_sha256(source_path)? != manifest.source_archive_sha256 {
        return Err("source archive hash changed".into());
    }
    let rom = read_rom()?;
    if sha256(&rom) != manifest.rom_sha256 {
        return Err("fixture ROM hash changed".into());
    }
    let prefix_bytes = fs::read(directory.join("prefix.json"))?;
    if sha256(&prefix_bytes) != manifest.prefix_sha256 {
        return Err("fixture prefix hash changed".into());
    }
    let prefix: SmbInput = serde_json::from_slice(&prefix_bytes)?;
    if prefix.actions.len() != manifest.action_count {
        return Err("fixture prefix action count changed".into());
    }
    let (snapshot, trace_sha256) = replay_prefix(
        &rom,
        &prefix,
        manifest.expected_world,
        manifest.expected_level,
        manifest.expected_progress,
    )?;
    if trace_sha256 != manifest.trace_sha256 {
        return Err("fixture replay trace hash changed".into());
    }
    let recreated = one_entry_checkpoint(snapshot).to_bytes()?;
    let stored = fs::read(directory.join("checkpoint.bin"))?;
    if recreated != stored || sha256(&stored) != manifest.checkpoint_sha256 {
        return Err("fixture checkpoint is not the byte-identical replayed prefix state".into());
    }
    let decoded = SmbSnapshotCheckpoint::from_bytes(&stored, SNAPSHOT_CHECKPOINT_FORMAT)?;
    if decoded.entries.len() != 1 || decoded.entries[0].id != 0 {
        return Err("fixture checkpoint is not a one-entry id-zero root".into());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "certified": true,
            "id": challenge.id,
            "actions": manifest.action_count,
            "checkpoint_sha256": manifest.checkpoint_sha256,
            "trace_sha256": manifest.trace_sha256,
        }))?
    );
    Ok(())
}

fn replay_prefix(
    rom: &[u8],
    prefix: &Input<ButtonChord>,
    expected_world: u8,
    expected_level: u8,
    expected_progress: u16,
) -> Result<(SmbSnapshot, String), Box<dyn Error>> {
    let mut target = SmbTarget::from_smb_rom_bytes_headless(rom)?;
    target.reset();
    let mut trace = Sha256::new();
    update_trace(&mut trace, &target.wram()[..])?;
    for action in &prefix.actions {
        if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
            return Err("fixture prefix reached a terminal state before its endpoint".into());
        }
        update_trace(&mut trace, action)?;
        target.apply(action);
        update_trace(&mut trace, target.last_action_observations())?;
    }
    if target.is_dead() || target.is_victory() || target.exit_kind() != ExitKind::Ok {
        return Err("fixture prefix endpoint is not an alive resumable state".into());
    }
    let state = smb_mechanical_state_from_wram(&target.wram());
    if (state.world, state.level, state.progress)
        != (expected_world, expected_level, expected_progress)
    {
        return Err(format!(
            "fixture endpoint is ({},{},{}) instead of ({expected_world},{expected_level},{expected_progress})",
            state.world, state.level, state.progress
        )
        .into());
    }
    let snapshot = target
        .snapshot()
        .ok_or("failed to snapshot fixture endpoint")?;
    Ok((snapshot, format!("{:x}", trace.finalize())))
}

fn one_entry_checkpoint(snapshot: SmbSnapshot) -> SmbSnapshotCheckpoint {
    SmbSnapshotCheckpoint {
        format: SNAPSHOT_CHECKPOINT_FORMAT.to_owned(),
        entries: vec![SmbSnapshotCheckpointEntry { id: 0, snapshot }],
    }
}

fn update_trace<T: Serialize + ?Sized>(
    trace: &mut Sha256,
    value: &T,
) -> Result<(), Box<dyn Error>> {
    let bytes = postcard::to_allocvec(value)?;
    trace.update(u64::try_from(bytes.len())?.to_le_bytes());
    trace.update(bytes);
    Ok(())
}

fn read_prefix(path: &Path) -> Result<SmbInput, Box<dyn Error>> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn read_rom() -> Result<Vec<u8>, Box<dyn Error>> {
    let path = env::var_os("HARMONY_SMB_ROM")
        .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?;
    Ok(fs::read(path)?)
}

fn file_sha256(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn parse_u64(value: &std::ffi::OsStr) -> Result<u64, Box<dyn Error>> {
    Ok(value.to_string_lossy().replace('_', "").parse()?)
}

fn parse_u8(value: &std::ffi::OsStr) -> Result<u8, Box<dyn Error>> {
    Ok(value.to_string_lossy().parse()?)
}
