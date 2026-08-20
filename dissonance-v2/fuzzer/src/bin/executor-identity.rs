// SPDX-License-Identifier: AGPL-3.0-or-later

//! Phase 1 old-versus-new scheduling-path identity and work gate.

use std::{env, error::Error, fs, path::PathBuf, time::Instant};

use fuzzer::{
    phase1::{MazeExecutionWork, MazeExecutorMode, run_guided_with_executor},
    phase4a::{
        AdventureExecutionWork, AdventureExecutorMode, SearchArm, TriageArm,
        run_adventure_campaign_with_executor,
    },
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

    let report = IdentityGateReport {
        seed: SEED,
        execution_budget: EXECUTION_BUDGET,
        maze,
        adventure,
        accepted: false,
    };
    let mut report = report;
    report.accepted = report.maze.identity && report.adventure.identity;
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
