// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 1 old-versus-new scheduling-path identity and work gate.

use std::{env, error::Error, fs, path::PathBuf, time::Instant};

use fuzzer::{
    phase1::{MazeExecutionWork, MazeExecutorMode, run_guided_with_executor},
    phase4a::{
        AdventureExecutionWork, AdventureExecutorMode, SearchArm, TriageArm,
        run_adventure_campaign_with_executor,
    },
    phase4b::{SmbExecutionWork, SmbExecutorMode, run_smb_ratchet_with_executor},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const SEED: u64 = 0x5eed_ee01;
const EXECUTION_BUDGET: u64 = 5_000;

#[derive(Debug, Deserialize, Serialize)]
struct TargetGate<W> {
    identity: bool,
    semantic_sha256: String,
    old_work: W,
    new_work: W,
    old_wall_nanos: u128,
    new_wall_nanos: u128,
    wall_ratio: String,
}

#[derive(Debug, Serialize)]
struct IdentityGateReport {
    seed: u64,
    execution_budget: u64,
    maze: TargetGate<MazeExecutionWork>,
    adventure: TargetGate<AdventureExecutionWork>,
    smb: TargetGate<SmbExecutionWork>,
    smb_frame_reduction_at_least_tenfold: bool,
    accepted: bool,
}

#[allow(clippy::disallowed_methods)] // Wall time is benchmark evidence, never campaign state.
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let output = PathBuf::from(
        args.next()
            .ok_or("usage: executor-identity <output-directory>")?,
    );
    if args.next().is_some() {
        return Err("unexpected executor-identity argument".into());
    }
    fs::create_dir_all(&output)?;

    let maze_gate_path = output.join("maze-gate.json");
    let maze = if maze_gate_path.exists() {
        serde_json::from_slice(&fs::read(&maze_gate_path)?)?
    } else {
        let old_started = Instant::now();
        let mut maze_old =
            run_guided_with_executor(SEED, EXECUTION_BUDGET, MazeExecutorMode::Legacy)?;
        let maze_old_wall = old_started.elapsed().as_nanos();
        let new_started = Instant::now();
        let mut maze_new =
            run_guided_with_executor(SEED, EXECUTION_BUDGET, MazeExecutorMode::SnapshotResume)?;
        let maze_new_wall = new_started.elapsed().as_nanos();
        let maze_old_work = maze_old.executor_work;
        let maze_new_work = maze_new.executor_work;
        normalize_maze(&mut maze_old);
        normalize_maze(&mut maze_new);
        let gate = TargetGate {
            identity: maze_old == maze_new,
            semantic_sha256: semantic_sha(&maze_new)?,
            old_work: maze_old_work,
            new_work: maze_new_work,
            old_wall_nanos: maze_old_wall,
            new_wall_nanos: maze_new_wall,
            wall_ratio: ratio(maze_old_wall, maze_new_wall),
        };
        fs::write(&maze_gate_path, serde_json::to_vec_pretty(&gate)?)?;
        gate
    };

    let old_adventure_dir = output.join("adventure-old");
    let new_adventure_dir = output.join("adventure-new");
    let adventure_gate_path = output.join("adventure-gate.json");
    let adventure = if adventure_gate_path.exists() {
        serde_json::from_slice(&fs::read(&adventure_gate_path)?)?
    } else {
        let old_started = Instant::now();
        let mut adventure_old = run_adventure_campaign_with_executor(
            &old_adventure_dir,
            SEED,
            EXECUTION_BUDGET,
            TriageArm::Null,
            SearchArm::Base,
            AdventureExecutorMode::Legacy,
        )?;
        let adventure_old_wall = old_started.elapsed().as_nanos();
        let new_started = Instant::now();
        let mut adventure_new = run_adventure_campaign_with_executor(
            &new_adventure_dir,
            SEED,
            EXECUTION_BUDGET,
            TriageArm::Null,
            SearchArm::Base,
            AdventureExecutorMode::SnapshotResume,
        )?;
        let adventure_new_wall = new_started.elapsed().as_nanos();
        let adventure_old_work = adventure_old.executor_work;
        let adventure_new_work = adventure_new.executor_work;
        normalize_adventure(&mut adventure_old);
        normalize_adventure(&mut adventure_new);
        let gate = TargetGate {
            identity: adventure_old == adventure_new,
            semantic_sha256: semantic_sha(&adventure_new)?,
            old_work: adventure_old_work,
            new_work: adventure_new_work,
            old_wall_nanos: adventure_old_wall,
            new_wall_nanos: adventure_new_wall,
            wall_ratio: ratio(adventure_old_wall, adventure_new_wall),
        };
        fs::write(&adventure_gate_path, serde_json::to_vec_pretty(&gate)?)?;
        gate
    };

    let rom_path = PathBuf::from(
        env::var_os("HARMONY_SMB_ROM")
            .ok_or("HARMONY_SMB_ROM must name the external Super Mario Bros ROM")?,
    );
    let rom = fs::read(rom_path)?;
    let old_report_path = output.join("smb-old.json");
    let (mut smb_old, smb_old_wall) = if old_report_path.exists() {
        let report = serde_json::from_slice(&fs::read(&old_report_path)?)?;
        let wall: u128 = fs::read_to_string(output.join("smb-old-wall-nanos.txt"))?.parse()?;
        (report, wall)
    } else {
        let started = Instant::now();
        let report =
            run_smb_ratchet_with_executor(&rom, SEED, EXECUTION_BUDGET, SmbExecutorMode::Legacy)?;
        let wall = started.elapsed().as_nanos();
        fs::write(&old_report_path, serde_json::to_vec_pretty(&report)?)?;
        fs::write(output.join("smb-old-wall-nanos.txt"), wall.to_string())?;
        (report, wall)
    };
    let new_report_path = output.join("smb-new.json");
    let (mut smb_new, smb_new_wall) = if new_report_path.exists() {
        let report = serde_json::from_slice(&fs::read(&new_report_path)?)?;
        let wall: u128 = fs::read_to_string(output.join("smb-new-wall-nanos.txt"))?.parse()?;
        (report, wall)
    } else {
        let started = Instant::now();
        let report = run_smb_ratchet_with_executor(
            &rom,
            SEED,
            EXECUTION_BUDGET,
            SmbExecutorMode::SnapshotResume,
        )?;
        let wall = started.elapsed().as_nanos();
        fs::write(&new_report_path, serde_json::to_vec_pretty(&report)?)?;
        fs::write(output.join("smb-new-wall-nanos.txt"), wall.to_string())?;
        (report, wall)
    };
    let smb_old_work = smb_old.executor_work;
    let smb_new_work = smb_new.executor_work;
    normalize_smb(&mut smb_old);
    normalize_smb(&mut smb_new);
    let smb = TargetGate {
        identity: smb_old == smb_new,
        semantic_sha256: semantic_sha(&smb_new)?,
        old_work: smb_old_work,
        new_work: smb_new_work,
        old_wall_nanos: smb_old_wall,
        new_wall_nanos: smb_new_wall,
        wall_ratio: ratio(smb_old_wall, smb_new_wall),
    };
    let smb_frame_reduction_at_least_tenfold =
        smb.new_work.emulated_frames.saturating_mul(10) <= smb.old_work.emulated_frames;

    let report = IdentityGateReport {
        seed: SEED,
        execution_budget: EXECUTION_BUDGET,
        maze,
        adventure,
        smb,
        smb_frame_reduction_at_least_tenfold,
        accepted: false,
    };
    let mut report = report;
    report.accepted = report.maze.identity
        && report.adventure.identity
        && report.smb.identity
        && report.smb_frame_reduction_at_least_tenfold;
    fs::write(
        output.join("executor-identity-report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.accepted {
        return Err("Phase 1 executor identity gate failed".into());
    }
    Ok(())
}

fn normalize_maze(report: &mut fuzzer::phase1::CampaignReport) {
    report.executor_mode = MazeExecutorMode::SnapshotResume;
    report.executor_work = MazeExecutionWork::default();
}

fn normalize_adventure(report: &mut fuzzer::phase4a::AdventureRunReport) {
    report.executor_mode = AdventureExecutorMode::SnapshotResume;
    report.executor_work = AdventureExecutionWork::default();
}

fn normalize_smb(report: &mut fuzzer::phase4b::SmbCampaignReport) {
    report.executor_mode = SmbExecutorMode::SnapshotResume;
    report.executor_work = SmbExecutionWork::default();
}

fn semantic_sha<T: Serialize>(value: &T) -> Result<String, Box<dyn Error>> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

fn ratio(numerator: u128, denominator: u128) -> String {
    if denominator == 0 {
        return "infinite".to_owned();
    }
    let hundredths = numerator.saturating_mul(100) / denominator;
    format!("{}.{:02}x", hundredths / 100, hundredths % 100)
}
