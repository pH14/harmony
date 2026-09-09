// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reconstruct recorded competitors and read motion bytes without new suffixes.

use nes_workload::{
    metroid::{
        retention_audit::ReplacementPair,
        target::{MetroidInput, MetroidMechanicalState, MetroidTarget, MetroidTerminalPolicy},
    },
    target::{ExitKind, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{error::Error, fs, io::Write, path::Path};

#[derive(Deserialize)]
struct Audit {
    samples: Vec<Vec<ReplacementPair>>,
}

fn replay(
    core: &Path,
    rom: &[u8],
    core_hash: &str,
    input: &MetroidInput,
    expected: MetroidMechanicalState,
    used: &mut u64,
) -> Result<Value, Box<dyn Error>> {
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, core, core_hash)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    let setup = target.frames_clocked();
    *used += setup;
    for action in &input.actions {
        if *used + u64::from(action.bounded_hold_frames()) > 2_000_000 {
            return Err("motion diagnostic exceeded its total physical frame bound".into());
        }
        if target.is_dead() || target.is_victory() {
            return Err("competitor input continues after corrected terminal boundary".into());
        }
        let before = target.frames_clocked();
        target.apply(action);
        *used += target.frames_clocked() - before;
        if target.exit_kind() != ExitKind::Ok {
            return Err("motion diagnostic emulator failure".into());
        }
    }
    if target.mechanical_state() != expected {
        return Err(
            "reconstructed competitor differs from its recorded mechanical endpoint".into(),
        );
    }
    let before = serde_json::to_vec(&target.snapshot().ok_or("snapshot failed")?)?;
    let motion = target.diagnostic_kinematics()?;
    let after = serde_json::to_vec(&target.snapshot().ok_or("snapshot failed")?)?;
    if before != after {
        return Err("reading motion bytes changed snapshot state".into());
    }
    Ok(
        json!({"endpoint":target.mechanical_state(), "motion":motion,
              "snapshot_sha256":format!("{:x}",Sha256::digest(before)),
              "physical_frames":target.frames_clocked(), "setup_frames":setup}),
    )
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 4 {
        return Err("usage: metroid-kinematics-probe CORE ROM AUDIT.json OUT".into());
    }
    let core = Path::new(&args[0]);
    let rom = fs::read(&args[1])?;
    let bytes = fs::read(&args[2])?;
    let audit: Audit = serde_json::from_slice(&bytes)?;
    let pairs: Vec<_> = audit
        .samples
        .iter()
        .enumerate()
        .flat_map(|(s, pairs)| pairs.iter().enumerate().map(move |(i, pair)| (s, i, pair)))
        .collect();
    if pairs.is_empty() || pairs.len() > 16 {
        return Err("motion diagnostic requires one to sixteen recorded pairs".into());
    }
    for (_, _, pair) in &pairs {
        for input in [&pair.candidate_input, &pair.incumbent_input] {
            let frames: u64 = input
                .actions
                .iter()
                .map(|a| u64::from(a.bounded_hold_frames()))
                .sum();
            if input.actions.is_empty() || input.actions.len() > 8192 || frames > 250_000 {
                return Err("competitor input exceeds action or frame bound".into());
            }
        }
    }
    let out = Path::new(&args[3]);
    fs::create_dir(out)?;
    let mut log = fs::File::create(out.join("pairs.jsonl"))?;
    let core_hash = format!("{:x}", Sha256::digest(fs::read(core)?));
    let mut used = 0;
    for (stratum, index, pair) in &pairs {
        let mut record = json!({"stratum":stratum,"pair":index,"execution":pair.execution,
                                "candidate_replaces":pair.replaces,"verified_replays_per_endpoint":2});
        for (name, input, expected) in [
            ("candidate", &pair.candidate_input, pair.candidate),
            ("incumbent", &pair.incumbent_input, pair.incumbent),
        ] {
            let first = replay(core, &rom, &core_hash, input, expected, &mut used)?;
            let second = replay(core, &rom, &core_hash, input, expected, &mut used)?;
            if first != second {
                return Err("independent competitor motion replays differ".into());
            }
            record[name] = first;
        }
        record["cumulative_physical_frames"] = json!(used);
        writeln!(log, "{record}")?;
        log.flush()?;
    }
    let report = json!({"format":"metroid-competitor-motion-probe-v1",
        "scope":"existing recorded competitors, no new search or suffix trials",
        "pairs":pairs.len(),"verified_replays_per_endpoint":2,"physical_frames":used,
        "audit_sha256":format!("{:x}",Sha256::digest(bytes)),"core_sha256":core_hash,
        "rom_sha256":format!("{:x}",Sha256::digest(rom)),
        "terminal_policy":"death_or_bcd_underflow_or_ending_v3",
        "read_only_snapshot_checks":true});
    fs::write(
        out.join("summary.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}
