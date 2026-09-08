// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bounded search from a development-discovered endpoint; never fresh validation.
use nes_workload::{
    metroid::{
        campaign::{MetroidCampaignRun, MetroidGame},
        progress::NamedProgress,
        target::{MetroidInput, MetroidSnapshot, MetroidTarget},
    },
    search::{
        archive::{
            MAX_ARCHIVE_ENTRIES, RetentionPolicy, SlotRetentionPolicy,
            selector_policy_from_identifier,
        },
        campaign::{
            CampaignCheckpoint, CampaignConfig, CampaignExecutionOptions, CampaignOrigin,
            Reporting, ResultBuffering, SnapshotCheckpoint, SnapshotCheckpointEntry,
            replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
        },
        draw::{DrawMixture, suffix_shape_from_identifier},
    },
    target::{ExitKind, Target},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Deserialize)]
struct Request {
    core: PathBuf,
    rom: PathBuf,
    input: PathBuf,
    input_sha256: String,
    seed: u64,
    executions: u64,
    frames: u64,
    wall_seconds: u64,
    suffix: String,
    full_replay: bool,
    terminal: Option<String>,
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn write(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    let temp = path.with_extension("tmp");
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(temp, path)?;
    Ok(())
}
fn replay(
    rom: &[u8],
    core: &Path,
    core_hash: &str,
    input: &MetroidInput,
    policy: nes_workload::metroid::target::MetroidTerminalPolicy,
) -> Result<(MetroidSnapshot, Value)> {
    let mut target =
        MetroidTarget::from_rom_bytes_headless(rom, core, core_hash)?.with_terminal_policy(policy);
    let before = target.frames_clocked();
    let mut named = NamedProgress::default();
    for (index, action) in input.actions.iter().enumerate() {
        if target.is_dead() || target.is_victory() {
            return Err("source/witness continues after terminal event".into());
        }
        target.apply(action);
        if target.exit_kind() != ExitKind::Ok {
            return Err("source/witness emulator failure".into());
        }
        for observation in target.last_action_observations() {
            named.observe(
                observation,
                index as u64 + 1,
                target.frames_clocked() - before,
            );
        }
    }
    let snapshot = target.snapshot().ok_or("source/witness snapshot failed")?;
    let value = json!({"endpoint":target.mechanical_state(),"named_progress":named,
        "snapshot_sha256":hash(&postcard::to_allocvec(&snapshot)?),"actions":input.actions.len(),
        "total_physical_frames":target.frames_clocked(),"suffix_physical_frames":target.frames_clocked()-before});
    Ok((snapshot, value))
}
// Host timing is diagnostic only and never enters replay or search decisions.
#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> Instant {
    Instant::now()
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: metroid-local-search REQUEST OUTPUT".into());
    }
    let started = telemetry_now();
    let request_bytes = fs::read(&args[0])?;
    let request: Request = serde_json::from_slice(&request_bytes)?;
    if request.executions == 0
        || request.executions > 150000
        || request.frames == 0
        || request.frames > 20000000
        || request.wall_seconds == 0
        || request.wall_seconds > 1080
        || (request.full_replay && request.executions > 5000)
    {
        return Err("diagnostic request exceeds its bounded limits".into());
    }
    if fs::metadata(&request.input)?.len() > 1048576 {
        return Err("source input is too large".into());
    }
    let source_bytes = fs::read(&request.input)?;
    if hash(&source_bytes) != request.input_sha256 {
        return Err("source input checksum mismatch".into());
    }
    let source: MetroidInput = serde_json::from_slice(&source_bytes)?;
    let remaining = 4096_usize
        .checked_sub(source.actions.len())
        .filter(|n| *n > 0)
        .ok_or("source leaves no action horizon")?;
    let out = PathBuf::from(&args[1]);
    fs::create_dir(&out)?;
    let rom = fs::read(&request.rom)?;
    let core_hash = hash(&fs::read(&request.core)?);
    let policy = nes_workload::metroid::target::MetroidTerminalPolicy::parse(
        request.terminal.as_deref().unwrap_or("death_or_ending_v2"),
    )?;
    let (snapshot, source_proof) = replay(&rom, &request.core, &core_hash, &source, policy)?;
    let (_, second_source) = replay(&rom, &request.core, &core_hash, &source, policy)?;
    if source_proof != second_source {
        return Err("source replay differs across independent targets".into());
    }
    let source_cost = 2 * source_proof["total_physical_frames"]
        .as_u64()
        .ok_or("source cost missing")?;
    let game = MetroidGame::new(&rom, &request.core, &core_hash)
        .with_terminal_policy(policy)
        .with_milestone_input_dir(out.join("milestone-inputs"));
    let snapshots = SnapshotCheckpoint {
        format: game.checkpoint_format().into(),
        entries: vec![SnapshotCheckpointEntry { id: 0, snapshot }],
    };
    let checkpoint_bytes = snapshots.to_bytes()?;
    fs::write(out.join("origin-checkpoint.bin"), &checkpoint_bytes)?;
    let origin_checkpoint = CampaignCheckpoint {
        path: "development-origin-checkpoint".into(),
        file_sha256: hash(&checkpoint_bytes),
        snapshots,
    };
    write(
        &out.join("identity.json"),
        &json!({"format":"metroid-local-search-v1","scope":"development-discovered snapshot root; not fresh search",
        "request_sha256":hash(&request_bytes),"source_input_sha256":request.input_sha256,"source_proof":source_proof,
        "source_verified_replays":2,"terminal_policy":policy.identifier(),"source_physical_frames":source_cost,"core_sha256":core_hash,"rom_sha256":hash(&rom),
        "total_action_limit":4096,"local_action_limit":remaining,"workers":4,"memory_mib":8192,
        "source_checkpoint_bytes":checkpoint_bytes.len(),"source_tree_sha256":option_env!("HARMONY_SEARCH_SOURCE_SHA256")}),
    )?;
    let origin = CampaignOrigin::SnapshotRoot {
        checkpoint: origin_checkpoint.clone(),
    };
    let config = CampaignConfig {
        campaign_seed: request.seed,
        workers: 4,
        execution_budget: request.executions,
        action_limit: remaining,
        host: "metroid-local-diagnostic".into(),
        wall_budget: Some(Duration::from_secs(request.wall_seconds)),
        continue_after_victory: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: 2,
        memory_budget_mib: Some(8192),
        materialize_final_artifacts: request.full_replay,
        run: MetroidCampaignRun,
        suffix: suffix_shape_from_identifier(&request.suffix)?,
        mixture: DrawMixture::AlphabetOnly,
        retention: RetentionPolicy::AdmitAlive,
        selector: selector_policy_from_identifier(
            "room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2",
            3,
        )?,
        victory_input_path: Some(out.join("local-victory-input.json")),
    };
    let mut bytes = Vec::new();
    let mut sink = std::io::sink();
    let stream: &mut dyn Write = if request.full_replay {
        &mut bytes
    } else {
        &mut sink
    };
    let mut progress = BufWriter::new(fs::File::create(out.join("progress.jsonl"))?);
    let search_started = telemetry_now();
    let (report, checkpoint) = run_campaign_checkpointed_with_options(
        &game,
        &config,
        &origin,
        stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            frame_budget: Some(request.frames),
            result_buffering: ResultBuffering::TwoPerWorker,
            slot_retention: SlotRetentionPolicy::Representative,
        },
    )?;
    progress.flush()?;
    let search_seconds = search_started.elapsed().as_secs_f64();
    if request.full_replay {
        fs::write(out.join("stream.jsonl"), &bytes)?;
        let replayed = replay_campaign_checkpointed(&game, &bytes, None, Some(&origin_checkpoint))?;
        if replayed != (report.clone(), checkpoint) {
            return Err("local full campaign/checkpoint replay differs".into());
        }
    }
    let mut report_value = serde_json::to_value(&report)?;
    let local: MetroidInput = if let Some(input) = &report.victory_input {
        input.clone()
    } else {
        serde_json::from_value(report_value["archive"]["champion_input"].clone())?
    };
    let mut full = source.clone();
    full.actions.extend(local.actions);
    write(&out.join("full-witness-input.json"), &full)?;
    let (_, witness) = replay(&rom, &request.core, &core_hash, &full, policy)?;
    if witness != replay(&rom, &request.core, &core_hash, &full, policy)?.1 {
        return Err("composed witness replay differs".into());
    }
    let mut verification_frames = 2 * witness["total_physical_frames"]
        .as_u64()
        .ok_or("witness cost missing")?;
    let mut milestones = serde_json::Map::new();
    if out.join("milestone-inputs").exists() {
        let mut paths: Vec<_> = fs::read_dir(out.join("milestone-inputs"))?
            .map(|p| p.map(|e| e.path()))
            .collect::<std::io::Result<_>>()?;
        paths.sort();
        for path in paths {
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let local: MetroidInput = serde_json::from_slice(&fs::read(&path)?)?;
            let mut full = source.clone();
            full.actions.extend(local.actions);
            let (_, proof) = replay(&rom, &request.core, &core_hash, &full, policy)?;
            if proof != replay(&rom, &request.core, &core_hash, &full, policy)?.1 {
                return Err("composed milestone replay differs".into());
            }
            verification_frames += 2 * proof["total_physical_frames"]
                .as_u64()
                .ok_or("milestone cost missing")?;
            write(&path, &full)?;
            milestones.insert(
                path.file_stem()
                    .ok_or("milestone name missing")?
                    .to_string_lossy()
                    .into(),
                proof,
            );
        }
    }
    if let Some(archive) = report_value
        .get_mut("archive")
        .and_then(Value::as_object_mut)
    {
        archive.remove("entries");
    }
    write(&out.join("campaign.json"), &report_value)?;
    let result = json!({"format":"metroid-local-search-result-v1","scope":"diagnostic origin; does not qualify fresh search",
        "source_physical_frames":source_cost,"composed_verification_physical_frames":verification_frames,"search_seconds":search_seconds,
        "elapsed_seconds":started.elapsed().as_secs_f64(),"campaign":report_value,"witness":witness,"milestone_witnesses":milestones,
        "verified_replays":2,"full_campaign_replay":request.full_replay,"cost_limitations":"source and composed verification are counted separately; ordinary target boot and optional full campaign replay are not included in those two fields"});
    write(&out.join("result.json"), &result)?;
    println!(
        "{}",
        json!({"status":"complete","search_seconds":search_seconds,"witness":witness})
    );
    Ok(())
}
