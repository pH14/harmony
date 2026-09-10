// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded use of the existing archive engine from one supplied Metroid state.
//! Supplied-state capability is never fresh-search performance evidence.
use nes_workload::{
    metroid::{
        boss_probe::BossContext,
        campaign::{MetroidCampaignRun, MetroidGame},
        target::{
            MetroidInput, MetroidMechanicalState, MetroidSnapshot, MetroidTarget,
            MetroidTerminalPolicy,
        },
    },
    search::{
        archive::{MAX_ARCHIVE_ENTRIES, RetentionPolicy, selector_policy_from_identifier},
        campaign::{
            CampaignCheckpoint, CampaignConfig, CampaignExecutionOptions, CampaignOrigin,
            Evaluation, Reporting, ResultBuffering, SnapshotCheckpoint, SnapshotCheckpointEntry,
            replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
        },
        draw::{draw_mixture_from_identifier, suffix_shape_from_identifier},
    },
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{BufWriter, LineWriter, Write},
    path::{Path, PathBuf},
    time::Duration,
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    core: PathBuf,
    rom: PathBuf,
    core_sha256: String,
    rom_sha256: String,
    input: PathBuf,
    input_sha256: String,
    expected_endpoint: MetroidMechanicalState,
    expected_context: Value,
    expected_emulator_sha256: String,
    expected_snapshot_sha256: Option<String>,
    seed: u64,
    workers: u32,
    executions: u64,
    frames: u64,
    actions: usize,
    memory_mib: usize,
    result_slots: usize,
    wall_seconds: u64,
    direct_frame_limit: u64,
    milestone: String,
    #[serde(default)]
    local_terminal_retry: bool,
}

fn valid_limits(q: &Request, execute: bool) -> bool {
    (1..=4).contains(&q.workers)
        && (1..=5000).contains(&q.executions)
        && (1..=1_000_000).contains(&q.frames)
        && (1..=8192).contains(&q.actions)
        && (16..=8192).contains(&q.memory_mib)
        && (1..=2).contains(&q.result_slots)
        && (1..=300).contains(&q.wall_seconds)
        && (1..=8_000_000).contains(&q.direct_frame_limit)
        && matches!(q.milestone.as_str(), "kraid_defeated" | "ridley_defeated")
        && (!execute || q.expected_snapshot_sha256.is_some())
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}
fn emulator_hash(snapshot: &MetroidSnapshot) -> Result<String> {
    let value = serde_json::to_value(snapshot)?;
    let bytes: Vec<u8> = serde_json::from_value(value["emulator_state"].clone())?;
    Ok(hash(&bytes))
}
fn completed_goal(execution: u64, frames: u64, limit: u64) -> bool {
    execution > 0 && frames > 0 && frames <= limit
}
fn concat(prefix: &MetroidInput, local: &MetroidInput) -> MetroidInput {
    let mut actions = prefix.actions.clone();
    actions.extend_from_slice(&local.actions);
    MetroidInput { actions }
}
fn defeated(context: &BossContext, milestone: &str) -> bool {
    match milestone {
        "kraid_defeated" => context.memory.kraid_status & 1 != 0,
        "ridley_defeated" => context.memory.ridley_status & 2 != 0,
        _ => false,
    }
}

#[derive(Default, Serialize)]
struct Cost {
    direct_physical_frames: u64,
    direct_setup_frames: u64,
    admitted_search_frames: u64,
    campaign_replay_admitted_frames: u64,
}
impl Cost {
    fn room(&self, frames: u64, limit: u64) -> Result<()> {
        if self
            .direct_physical_frames
            .checked_add(frames)
            .is_none_or(|sum| sum > limit)
        {
            return Err("direct replay/setup frame ceiling reached".into());
        }
        Ok(())
    }
}
struct Replay {
    snapshot: MetroidSnapshot,
    context: BossContext,
    dead: bool,
}
fn replay(
    q: &Request,
    rom: &[u8],
    input: &MetroidInput,
    root: Option<&MetroidSnapshot>,
    cost: &mut Cost,
) -> Result<Replay> {
    cost.room(929, q.direct_frame_limit)?;
    let mut target = MetroidTarget::from_rom_bytes_headless(rom, &q.core, &q.core_sha256)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    let setup = target.frames_clocked();
    cost.direct_physical_frames += setup;
    cost.direct_setup_frames += setup;
    if setup != 929 {
        return Err("ordinary genesis setup changed".into());
    }
    if let Some(root) = root {
        target.restore(root)?;
        if target.snapshot().as_ref() != Some(root) {
            return Err("restored root snapshot differs".into());
        }
    }
    for action in &input.actions {
        if target.is_dead() || target.is_victory() {
            return Err("replay input continues after terminal".into());
        }
        cost.room(
            u64::from(action.bounded_hold_frames()),
            q.direct_frame_limit,
        )?;
        let before = target.frames_clocked();
        target.apply(action);
        cost.direct_physical_frames += target.frames_clocked() - before;
        if target.exit_kind() != ExitKind::Ok {
            return Err("replay emulator failure".into());
        }
    }
    Ok(Replay {
        snapshot: target.snapshot().ok_or("replay snapshot failed")?,
        context: target.diagnostic_boss_context()?,
        dead: target.is_dead(),
    })
}
fn same_replay(a: &Replay, b: &Replay) -> bool {
    a.snapshot == b.snapshot && a.context == b.context && a.dead == b.dead
}

fn evaluate(q: &Request, output: &Path, execute: bool, cost: &mut Cost) -> Result<Value> {
    let rom = fs::read(&q.rom)?;
    if hash(&rom) != q.rom_sha256 || hash(&fs::read(&q.core)?) != q.core_sha256 {
        return Err("ROM/core identity mismatch".into());
    }
    if fs::metadata(&q.input)?.len() > 1_048_576 {
        return Err("prefix file too large".into());
    }
    let bytes = fs::read(&q.input)?;
    if hash(&bytes) != q.input_sha256 {
        return Err("searched prefix identity mismatch".into());
    }
    let prefix: MetroidInput = serde_json::from_slice(&bytes)?;
    if prefix.actions.is_empty()
        || prefix.actions.len() > 8192
        || prefix
            .actions
            .iter()
            .map(|a| u64::from(a.bounded_hold_frames()))
            .sum::<u64>()
            > 250_000
    {
        return Err("prefix exceeds the fixed diagnostic bounds".into());
    }
    let first = replay(q, &rom, &prefix, None, cost)?;
    let root = &first.snapshot;
    if first.dead
        || !root.state().in_play()
        || root.state().ending
        || root.state() != q.expected_endpoint
        || serde_json::to_value(&first.context)? != q.expected_context
        || emulator_hash(root)? != q.expected_emulator_sha256
        || defeated(&first.context, &q.milestone)
    {
        return Err("root differs from the qualified, live, undefeated input".into());
    }
    let root_sha = hash(&postcard::to_allocvec(root)?);
    if q.expected_snapshot_sha256
        .as_ref()
        .is_some_and(|sha| *sha != root_sha)
    {
        return Err("frozen root snapshot identity changed".into());
    }
    if !same_replay(&first, &replay(q, &rom, &prefix, None, cost)?) {
        return Err("prefix replays disagree".into());
    }
    let root_report = json!({"format":"metroid-archive-challenge-root-v1",
        "scope":"supplied searched prefix; diagnostic only", "input_sha256":q.input_sha256,
        "snapshot_sha256":root_sha,"emulator_sha256":q.expected_emulator_sha256,
        "endpoint":root.state(),"context":first.context,"verified_prefix_replays":2,
        "cost":cost});
    write(&output.join("root.json"), &root_report)?;
    write(&output.join("root-snapshot.json"), root)?;
    if !execute {
        return Ok(root_report);
    }

    let game = MetroidGame::new(&rom, &q.core, &q.core_sha256)
        .with_local_terminal_retry(q.local_terminal_retry)
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow)
        .with_milestone_input_dir(output.join("milestone-inputs"));
    let snapshots = SnapshotCheckpoint {
        format: game.checkpoint_format().to_owned(),
        entries: vec![SnapshotCheckpointEntry {
            id: 0,
            snapshot: root.clone(),
        }],
    };
    let checkpoint_bytes = snapshots.to_bytes()?;
    fs::write(output.join("origin.bin"), &checkpoint_bytes)?;
    let checkpoint = CampaignCheckpoint {
        path: format!("diagnostic-prefix-sha256:{}", q.input_sha256),
        file_sha256: hash(&checkpoint_bytes),
        snapshots,
    };
    let origin = CampaignOrigin::SnapshotRoot {
        checkpoint: checkpoint.clone(),
    };
    let config = CampaignConfig {
        campaign_seed: q.seed,
        workers: q.workers,
        execution_budget: q.executions,
        action_limit: q.actions,
        host: "metroid-archive-challenge".into(),
        wall_budget: Some(Duration::from_secs(q.wall_seconds)),
        continue_after_victory: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: 1,
        memory_budget_mib: Some(q.memory_mib),
        materialize_final_artifacts: true,
        run: MetroidCampaignRun,
        suffix: suffix_shape_from_identifier("one_to_six")?,
        mixture: draw_mixture_from_identifier("alphabet_only")?,
        retention: RetentionPolicy::AdmitAlive,
        selector: selector_policy_from_identifier(
            "room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2",
            3,
        )?,
        victory_input_path: Some(output.join("victory-input.json")),
    };
    let (milestone, _) = game
        .named_milestone(&q.milestone)
        .ok_or("unknown milestone")?;
    let mut stream = BufWriter::new(fs::File::create(output.join("stream.jsonl"))?);
    let mut progress = LineWriter::new(fs::File::create(output.join("progress.jsonl"))?);
    let (report, final_checkpoint) = run_campaign_checkpointed_with_options(
        &game,
        &config,
        &origin,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            frame_budget: Some(q.frames),
            stop_after_milestone: Some(milestone),
            result_buffering: if q.result_slots == 1 {
                ResultBuffering::OnePerWorker
            } else {
                ResultBuffering::TwoPerWorker
            },
            slot_retention: Default::default(),
        },
    )?;
    stream.flush()?;
    progress.flush()?;
    cost.admitted_search_frames = report.frames_emulated;
    let reached = report
        .first_milestone
        .is_some_and(|event| completed_goal(event.execution, event.frames_emulated, q.frames));
    let complete = reached
        || report.executions_completed >= q.executions
        || report.frames_emulated >= q.frames;
    let mut value = serde_json::to_value(&report)?;
    value["archive"]
        .as_object_mut()
        .ok_or("missing archive report")?
        .remove("entries");
    write(&output.join("campaign.json"), &value)?;
    fs::write(output.join("checkpoint.bin"), final_checkpoint.to_bytes()?)?;
    let stream_bytes = fs::read(output.join("stream.jsonl"))?;
    let (verified, verified_checkpoint) =
        replay_campaign_checkpointed(&game, &stream_bytes, None, Some(&checkpoint))?;
    cost.campaign_replay_admitted_frames = verified.frames_emulated;
    if serde_json::to_value(&verified)? != serde_json::to_value(&report)?
        || verified_checkpoint != final_checkpoint
    {
        return Err("complete campaign replay differs".into());
    }
    let local: MetroidInput = if reached {
        serde_json::from_slice(&fs::read(
            output
                .join("milestone-inputs")
                .join(format!("{}.json", q.milestone)),
        )?)?
    } else if q.local_terminal_retry {
        report
            .first_local_retry
            .as_ref()
            .ok_or("no surviving retry exercised")?
            .input
            .clone()
    } else {
        serde_json::from_value(value["archive"]["champion_input"].clone())?
    };
    let full = concat(&prefix, &local);
    let local_first = replay(q, &rom, &local, Some(root), cost)?;
    if q.local_terminal_retry && !reached {
        let witness = report
            .first_local_retry
            .as_ref()
            .ok_or("retry witness missing")?;
        if local_first.dead
            || hash(&postcard::to_allocvec(&local_first.snapshot)?) != witness.snapshot_sha256
        {
            return Err("linear input differs from the original surviving retry snapshot".into());
        }
    }
    for from_root in [true, false, false] {
        let next = if from_root {
            replay(q, &rom, &local, Some(root), cost)?
        } else {
            replay(q, &rom, &full, None, cost)?
        };
        if !same_replay(&local_first, &next) {
            return Err("root-local and complete-prefix witness replays disagree".into());
        }
    }
    if reached && (local_first.dead || !defeated(&local_first.context, &q.milestone)) {
        return Err("reported challenge milestone lacks a surviving full-prefix witness".into());
    }
    write(&output.join("witness-local.json"), &local)?;
    write(&output.join("witness-full.json"), &full)?;
    Ok(
        json!({"format":"metroid-archive-challenge-result-v1","scope":"supplied-state capability; never fresh validation",
        "complete":complete,"milestone":q.milestone,"milestone_reached_within_budget":reached,
        "first_milestone":report.first_milestone,"executions":report.executions_completed,
        "frames":report.frames_emulated,"frame_budget_overshoot":report.frames_emulated.saturating_sub(q.frames),
        "stop_reason":if reached {"milestone"} else if report.executions_completed>=q.executions {"execution_limit"} else if report.frames_emulated>=q.frames {"frame_limit"} else {"wall_limit"},
        "origin":report.origin,"stream_sha256":hash(&stream_bytes),"root":root_report,
        "full_campaign_replay":true,"root_local_witness_replays":2,"complete_prefix_witness_replays":2,
        "witness":{"endpoint":local_first.snapshot.state(),"context":local_first.context,
            "dead":local_first.dead,"emulator_sha256":emulator_hash(&local_first.snapshot)?,
            "local_input_sha256":hash(&fs::read(output.join("witness-local.json"))?),
            "full_input_sha256":hash(&fs::read(output.join("witness-full.json"))?)},"cost":cost,
        "unknown":"Engine target setup, unadmitted work and reconstruction remain additional unknown costs. Campaign replay admitted frames are an inferred component, not total physical replay work."}),
    )
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [mode, request, output] = args.as_slice() else {
        return Err("usage: metroid-archive-challenge prepare|run REQUEST OUT".into());
    };
    if !matches!(mode.as_str(), "prepare" | "run") {
        return Err("unknown challenge mode".into());
    }
    let execute = mode == "run";
    let request_bytes = fs::read(request)?;
    let q: Request = serde_json::from_slice(&request_bytes)?;
    if !valid_limits(&q, execute) {
        return Err("challenge limits or frozen root are invalid".into());
    }
    let output = Path::new(output);
    fs::create_dir(output)?;
    let mut cost = Cost::default();
    let result = evaluate(&q, output, execute, &mut cost);
    write(
        &output.join("usage.json"),
        &json!({"cost":cost,"request_sha256":hash(&request_bytes),
        "completed_execution":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string),
        "unknown":"A killed process can omit current work; use retained progress and the registered ceilings, never assume zero."}),
    )?;
    write(&output.join("result.json"), &result?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use nes_workload::metroid::target::ButtonChord;
    #[test]
    fn inherited_and_post_budget_events_do_not_pass() {
        assert!(!completed_goal(0, 0, 100));
        assert!(!completed_goal(1, 101, 100));
        assert!(completed_goal(1, 100, 100));
    }
    #[test]
    fn full_witness_keeps_the_prefix_exactly_once() {
        let prefix = MetroidInput {
            actions: vec![ButtonChord::new(1, 3), ButtonChord::new(2, 4)],
        };
        let local = MetroidInput {
            actions: vec![ButtonChord::new(3, 5)],
        };
        assert_eq!(
            concat(&prefix, &local).actions,
            vec![
                ButtonChord::new(1, 3),
                ButtonChord::new(2, 4),
                ButtonChord::new(3, 5)
            ]
        );
        assert_eq!(concat(&prefix, &MetroidInput::default()), prefix);
    }
    #[test]
    fn direct_budget_refuses_overflow_and_overshoot() {
        let cost = Cost {
            direct_physical_frames: 100,
            ..Cost::default()
        };
        assert!(cost.room(20, 120).is_ok());
        assert!(cost.room(21, 120).is_err());
        assert!(cost.room(u64::MAX, u64::MAX).is_err());
    }
}
