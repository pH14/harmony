// SPDX-License-Identifier: AGPL-3.0-or-later
//! Per-operation cost of the SMB target: raw frames, snapshot and restore,
//! action application with observations, the admission probe, and the
//! archive key. Numbers are wall-clock and machine-specific; the binary
//! exists to find which operation bounds campaign throughput.

use std::{env, error::Error, fs, time::Instant};

use searcher::{
    smb::target::{ButtonChord, SmbTarget},
    target::Target,
};
use sha2::{Digest, Sha256};

#[allow(clippy::disallowed_methods)] // wall time is the measurement here.
fn main() -> Result<(), Box<dyn Error>> {
    let rom =
        fs::read(env::var_os("HARMONY_SMB_ROM").ok_or("HARMONY_SMB_ROM must name the SMB ROM")?)?;
    let mut target = SmbTarget::from_smb_rom_bytes_headless(&rom)?;
    let rounds: u32 = env::args()
        .nth(1)
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(200);

    let base = target.snapshot().ok_or("snapshot")?;
    println!("snapshot bytes: {}", serde_json::to_vec(&base)?.len());

    let started = Instant::now();
    let mut frames = 0_u64;
    for _ in 0..rounds {
        target.restore(&base)?;
        target.survives_probe(0x81, 120);
        frames += 120;
    }
    let probe_secs = started.elapsed().as_secs_f64();
    println!(
        "probe 120 frames + restore: {:.2} ms each, {:.0} frames/s incl restore",
        probe_secs * 1000.0 / f64::from(rounds),
        frames as f64 / probe_secs
    );

    let started = Instant::now();
    for _ in 0..rounds {
        target.restore(&base)?;
    }
    let restore_ms = started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds);
    println!("restore: {restore_ms:.3} ms");

    let started = Instant::now();
    for _ in 0..rounds {
        let _ = target.snapshot().ok_or("snapshot")?;
    }
    println!(
        "snapshot: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    );

    let started = Instant::now();
    let mut applied_frames = 0_u64;
    let mut observations = 0_usize;
    for _ in 0..rounds {
        target.restore(&base)?;
        target.apply(&ButtonChord::new(0x81, 100));
        applied_frames += 100;
        observations += target.last_action_observations().len();
    }
    let apply_secs = started.elapsed().as_secs_f64();
    println!(
        "apply 100-frame chord + restore: {:.2} ms each, {:.0} frames/s, {:.1} observations/action",
        apply_secs * 1000.0 / f64::from(rounds),
        applied_frames as f64 / apply_secs,
        observations as f64 / f64::from(rounds)
    );

    let started = Instant::now();
    for _ in 0..rounds {
        let _ = Sha256::digest(target.wram());
    }
    println!(
        "wram sha256: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    );

    let snapshot = target.snapshot().ok_or("snapshot")?;
    let started = Instant::now();
    for _ in 0..rounds {
        let _ = snapshot.clone();
    }
    println!(
        "snapshot clone: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    );
    let started = Instant::now();
    for _ in 0..rounds {
        let _ = serde_json::to_vec(&snapshot)?;
    }
    println!(
        "snapshot to_json: {:.3} ms",
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(rounds)
    );
    Ok(())
}
