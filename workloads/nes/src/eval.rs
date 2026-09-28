// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    search::{
        archive::{ArchiveKey, Input, MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{
            CampaignConfig, CampaignExecutionOptions, CampaignOrigin, ResultBuffering, Workload,
            replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
        },
        checkpoint::CheckpointPlan,
        draw::{draw_mixture_from_identifier, suffix_shape_from_identifier},
    },
    witness::replay_witness,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{self, BufWriter, LineWriter, Write},
    num::NonZeroU64,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> Instant {
    Instant::now()
}

fn objective_within_budget(first_objective_frames: Option<u64>, budget: Option<u64>) -> bool {
    first_objective_frames.is_some_and(|frames| budget.is_none_or(|limit| frames <= limit))
}

fn default_result_slots() -> usize {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub game: String,
    #[serde(default)]
    pub whole_game: bool,
    pub rom: PathBuf,
    pub core: PathBuf,
    pub rom_sha256: String,
    pub core_sha256: String,
    pub seed: u64,
    pub workers: u32,
    pub executions: u64,
    #[serde(default)]
    pub frames: Option<u64>,
    pub memory_mib: usize,
    pub window: usize,
    #[serde(default = "default_result_slots")]
    pub result_slots: usize,
    pub wall_seconds: u64,
    pub suffix: String,
    pub mixture: String,
    pub verification: String,
    #[serde(default)]
    pub level: Option<u8>,
    #[serde(default)]
    pub stage: Option<u8>,
    #[serde(default)]
    pub ai: Option<String>,
    #[serde(default)]
    pub metroid_terminal: Option<String>,
    #[serde(default)]
    pub root_input: Option<PathBuf>,
    #[serde(default)]
    pub checkpoint_every: Option<NonZeroU64>,
    #[serde(default)]
    pub checkpoint_on_progress: bool,
    #[serde(default)]
    pub resume: Option<PathBuf>,
}

struct StreamDigest {
    file: Option<BufWriter<fs::File>>,
    digest: Sha256,
    bytes: u64,
}
impl Write for StreamDigest {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.file {
            file.write_all(buf)?;
        }
        self.digest.update(buf);
        self.bytes += buf.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if let Some(file) = &mut self.file {
            file.flush()?;
        }
        Ok(())
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    let mut out = BufWriter::new(fs::File::create(&temporary)?);
    serde_json::to_writer(&mut out, value)?;
    out.write_all(b"\n")?;
    out.flush()?;
    fs::rename(temporary, path)?;
    Ok(())
}

fn phase(out: &Path, name: &str, started: Instant) -> Result<()> {
    write_json(
        &out.join("phase.json"),
        &json!({"phase": name, "elapsed_seconds": started.elapsed().as_secs_f64()}),
    )
}

pub fn evaluate<G: Workload>(
    game: G,
    run: G::Run,
    request: &Request,
    out: &Path,
    started: Instant,
) -> Result<()>
where
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
{
    let full = request.verification == "campaign";
    if request.verification != "witness" && !full {
        return Err("verification must be campaign or witness".into());
    }
    if full && request.executions > 5000 {
        return Err("full campaign verification is bounded to 5000 executions; use witness verification for performance runs".into());
    }
    if full && request.resume.is_some() {
        return Err("a run resumed from a search checkpoint has no stream to replay; use witness verification".into());
    }
    let origin = match &request.resume {
        Some(path) => CampaignOrigin::SearchCheckpoint { path: path.clone() },
        None => CampaignOrigin::Genesis,
    };
    let checkpoints =
        (request.checkpoint_every.is_some() || request.checkpoint_on_progress).then(|| {
            CheckpointPlan {
                directory: out.join("checkpoints"),
                every: request.checkpoint_every,
                on_marks: request.checkpoint_on_progress,
                on_top_progress: request.checkpoint_on_progress,
            }
        });
    let config = CampaignConfig {
        campaign_seed: request.seed,
        workers: request.workers,
        execution_budget: request.executions,
        host: "nes-eval".into(),
        wall_budget: Some(Duration::from_secs(request.wall_seconds)),
        stop_rollout_on_objective: true,
        stop_campaign_on_objective: true,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: request.window,
        memory_budget_mib: Some(request.memory_mib),
        materialize_final_artifacts: full,
        run: run.clone(),
        suffix: suffix_shape_from_identifier(&request.suffix)?,
        mixture: draw_mixture_from_identifier(&request.mixture)?,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: Some(out.join("victory-input.json")),
    };
    if request.workers == 0
        || request.executions == 0
        || request.memory_mib == 0
        || request.window == 0
        || request.wall_seconds == 0
    {
        return Err("search limits must be positive".into());
    }
    let result_buffering = match request.result_slots {
        1 => ResultBuffering::OnePerWorker,
        2 => ResultBuffering::TwoPerWorker,
        _ => return Err("result_slots must be 1 or 2".into()),
    };
    write_json(
        &out.join("identity.json"),
        &json!({"format":"nes-eval-identity-v2", "game":request.game, "whole_game":request.whole_game, "level":request.level, "stage":request.stage, "ai":request.ai, "rom_sha256":request.rom_sha256, "core_sha256":request.core_sha256, "backend":"native", "source_tree_sha256":option_env!("HARMONY_SEARCH_SOURCE_SHA256"), "policies":game.policies(&run), "seed":request.seed, "workers":request.workers, "executions":request.executions, "frames":request.frames, "memory_mib":request.memory_mib, "window":request.window, "result_slots":request.result_slots, "wall_seconds":request.wall_seconds, "suffix":request.suffix, "mixture":request.mixture, "verification":request.verification, "checkpoint_every":request.checkpoint_every, "checkpoint_on_progress":request.checkpoint_on_progress, "resume":request.resume}),
    )?;
    let mut stream = StreamDigest {
        file: if full {
            Some(BufWriter::new(fs::File::create(out.join("stream.jsonl"))?))
        } else {
            None
        },
        digest: Sha256::new(),
        bytes: 0,
    };
    let mut progress = LineWriter::new(fs::File::create(out.join("progress.jsonl"))?);
    let preparation_seconds = started.elapsed().as_secs_f64();
    phase(out, "search", started)?;
    let search_started = telemetry_now();
    let (report, checkpoint) = run_campaign_checkpointed_with_options(
        &game,
        &config,
        &origin,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            work_budget: request.frames,
            result_buffering,
            checkpoints,
            placement: None,
        },
    )?;
    stream.flush()?;
    progress.flush()?;
    let search_seconds = search_started.elapsed().as_secs_f64();
    phase(out, "export", started)?;
    let export_started = telemetry_now();
    let mut value = serde_json::to_value(&report)?;
    let witness: Input<G::Action> = if let Some(input) = &report.objective_witness {
        input.clone()
    } else {
        serde_json::from_value(value["archive"]["champion_input"].clone())?
    };
    write_json(&out.join("witness-input.json"), &witness)?;
    if let Some(deepest) = game
        .source_entries(&report.archive)
        .iter()
        .max_by_key(|entry| entry.key.progress())
    {
        write_json(&out.join("deepest-input.json"), &deepest.input)?;
    }
    if full {
        write_json(&out.join("checkpoint.json"), &checkpoint)?;
    }
    if let Some(archive) = value.get_mut("archive").and_then(Value::as_object_mut) {
        archive.remove("entries");
    }
    write_json(&out.join("campaign.json"), &value)?;
    let export_seconds = export_started.elapsed().as_secs_f64();
    phase(out, "verification", started)?;
    let verify_started = telemetry_now();
    let first = replay_witness(&game, &run, &witness)?;
    let second = replay_witness(&game, &run, &witness)?;
    if first != second {
        return Err("witness replay endpoint is nondeterministic".into());
    }
    if report.objectives_reached > 0 && first["victory"] != true {
        return Err("reported victory was not reproduced by its witness".into());
    }
    if full {
        let bytes = fs::read(out.join("stream.jsonl"))?;
        let (replayed, replay_checkpoint) =
            replay_campaign_checkpointed(&game, &bytes, None, None)?;
        if serde_json::to_value(&replayed)? != serde_json::to_value(&report)?
            || checkpoint != replay_checkpoint
        {
            return Err("campaign replay report/checkpoint differs".into());
        }
    }
    let mut milestone_witnesses = serde_json::Map::new();
    let milestone_directory = out.join("milestone-inputs");
    if milestone_directory.is_dir() {
        let mut paths: Vec<_> = fs::read_dir(&milestone_directory)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<_>>()?;
        paths.sort();
        for path in paths {
            let bytes = fs::read(&path)?;
            let input: Input<G::Action> = serde_json::from_slice(&bytes)?;
            let replay = replay_witness(&game, &run, &input)?;
            if replay != replay_witness(&game, &run, &input)? {
                return Err("milestone witness replay is nondeterministic".into());
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("invalid milestone name")?;
            let milestone = name
                .split_once('-')
                .map_or(name, |(milestone, _)| milestone);
            if replay["diagnostics"]["named_progress"]["first_seen"][milestone].is_null() {
                return Err(format!("milestone witness did not reproduce {name}").into());
            }
            milestone_witnesses.insert(
                name.into(),
                json!({
                    "input_sha256": format!("{:x}", Sha256::digest(&bytes)), "replay": replay
                }),
            );
        }
    }
    let verification_seconds = verify_started.elapsed().as_secs_f64();
    let solved = objective_within_budget(report.work_to_first_objective, request.frames);
    write_json(
        &out.join("result.json"),
        &json!({"format":"nes-eval-result-v1", "status":"complete", "solved":solved,
            "victory_observed":report.objectives_reached>0,
            "frame_budget_overshoot":request.frames.map(|limit| report.execution_work.saturating_sub(limit)),
            "executions": report.executions_completed, "frames_emulated":report.execution_work,
            "frames_to_first_victory":report.work_to_first_objective, "executions_to_first_victory":report.executions_to_first_objective,
            "preparation_seconds":preparation_seconds, "search_seconds":search_seconds, "executions_per_second":report.executions_completed as f64 / search_seconds,
            "progress":value["archive"]["progress_watermark"], "milestones":value["archive"]["milestones"], "frames_per_second": report.execution_work as f64 / search_seconds,
            "export_seconds":export_seconds, "verification_seconds":verification_seconds,
            "witness_replays":2, "campaign_replay":full,
            "verification":request.verification, "witness":first, "milestone_witnesses":milestone_witnesses,
            "stream_sha256":format!("{:x}",stream.digest.finalize()), "stream_bytes_generated":stream.bytes, "stream_retained":full,
            "stop_reason":if solved {"victory"} else if report.executions_completed >= request.executions {"execution_limit"} else if request.frames.is_some_and(|limit| report.execution_work >= limit) {"frame_limit"} else {"wall_limit"}
        }),
    )?;
    phase(out, "done", started)
}

pub fn run_cli(
    dispatch: impl FnOnce(Request, Vec<u8>, PathBuf, Instant) -> Result<()>,
) -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: EVALUATOR REQUEST.json OUTPUT_DIRECTORY".into());
    }
    let started = telemetry_now();
    let request: Request = serde_json::from_slice(&fs::read(&args[0])?)?;
    let out = PathBuf::from(&args[1]);
    if out.exists() && fs::read_dir(&out)?.next().is_some() {
        return Err("output directory must be empty".into());
    }
    fs::create_dir_all(&out)?;
    phase(&out, "preparation", started)?;
    let rom = fs::read(&request.rom)?;
    let core = fs::read(&request.core)?;
    if format!("{:x}", Sha256::digest(&rom)) != request.rom_sha256
        || format!("{:x}", Sha256::digest(&core)) != request.core_sha256
    {
        return Err("ROM/core checksum mismatch".into());
    }
    dispatch(request, rom, out, started)
}

#[cfg(test)]
mod tests {
    use super::objective_within_budget;

    #[test]
    fn a_victory_in_the_drained_window_does_not_pass_the_frame_check() {
        assert!(objective_within_budget(Some(128), Some(128)));
        assert!(!objective_within_budget(Some(129), Some(128)));
        assert!(objective_within_budget(Some(129), None));
        assert!(!objective_within_budget(None, Some(128)));
    }
}
