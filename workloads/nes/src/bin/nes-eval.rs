// SPDX-License-Identifier: AGPL-3.0-or-later
//! Compact, headless evaluation of NES packages through the shared campaign engine.

use nes_workload::{
    metroid::campaign::{MetroidCampaignRun, MetroidGame},
    mm2::{
        campaign::{Mm2CampaignRun, Mm2Game},
        target::{Mm2Input, Mm2Stage},
    },
    nova::{
        campaign::{NovaCampaignRun, NovaGame},
        target::NovaLevel,
    },
    search::{
        archive::{
            ArchiveKey, Input, MAX_ARCHIVE_ENTRIES, RetentionPolicy,
            selector_policy_from_identifier,
        },
        campaign::{
            CampaignConfig, CampaignExecutionOptions, CampaignOrigin, Game, ResultBuffering,
            TargetExecution, replay_campaign_checkpointed, run_campaign_checkpointed_with_options,
        },
        draw::{draw_mixture_from_identifier, suffix_shape_from_identifier},
    },
    smb::campaign::{SmbCampaignRun, SmbGame, SmbTerminalPredicate},
    stb::{
        campaign::{StbCampaignRun, StbGame},
        target::StbAi,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{self, BufWriter, LineWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

// Host telemetry never enters the campaign stream or adapter state.
#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> Instant {
    Instant::now()
}

fn event_within_budget(first_victory_frames: Option<u64>, budget: Option<u64>) -> bool {
    first_victory_frames.is_some_and(|frames| budget.is_none_or(|limit| frames <= limit))
}

fn default_result_slots() -> usize {
    1
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    game: String,
    #[serde(default)]
    retention_audit: bool,
    #[serde(default)]
    slot_retention: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stop_after_milestone: Option<String>,
    #[serde(default)]
    metroid_terminal: Option<String>,
    #[serde(default)]
    mm2_chain: bool,
    #[serde(default)]
    prefix_input: Option<PathBuf>,
    #[serde(default)]
    prefix_sha256: Option<String>,
    #[serde(default)]
    whole_game: bool,
    rom: PathBuf,
    core: PathBuf,
    rom_sha256: String,
    core_sha256: String,
    seed: u64,
    workers: u32,
    executions: u64,
    #[serde(default)]
    frames: Option<u64>,
    actions: usize,
    memory_mib: usize,
    window: usize,
    #[serde(default = "default_result_slots")]
    result_slots: usize,
    wall_seconds: u64,
    selector: String,
    suffix: String,
    mixture: String,
    verification: String,
    #[serde(default)]
    level: Option<u8>,
    #[serde(default)]
    stage: Option<u8>,
    #[serde(default)]
    ai: Option<String>,
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

fn replay_witness<G: Game>(game: &G, run: &G::Run, input: &Input<G::Action>) -> Result<Value> {
    let mut target = game.new_target()?;
    let mut aggregate = G::Milestones::default();
    let mut evidence = G::Evidence::default();
    game.merge_snapshot_root_evidence(&mut evidence, &target)?;
    let mut victory = false;
    let mut dead = false;
    let mut failed = false;
    let frames_before = game.frames_clocked(&target);
    for (i, action) in input.actions.iter().enumerate() {
        if dead || victory || failed {
            return Err("witness contains actions after a terminal event".into());
        }
        let snapshot = game.snapshot(&mut target)?;
        let result = game.execute_job(
            run,
            &mut target,
            &snapshot,
            &[],
            i,
            aggregate,
            &[*action],
            input.actions.len(),
            RetentionPolicy::AdmitAlive,
        )?;
        let [last] = result.actions.as_slice() else {
            return Err("witness did not execute its next action".into());
        };
        G::merge_witness_diagnostics(&mut evidence, &last.observations, i as u64 + 1);
        aggregate = last.milestones;
        victory = last.victory;
        dead = last.dead;
        failed = last.failed;
    }
    if failed {
        return Err("witness emulator failed".into());
    }
    let endpoint = game.snapshot(&mut target)?;
    Ok(
        json!({"victory": victory, "dead": dead, "milestones": aggregate,
            "physical_suffix_frames": game.frames_clocked(&target) - frames_before,
            "diagnostics": G::diagnostics(&evidence),
            "diagnostics_scope": "one replayed trajectory; execution fields count replayed actions from one", "snapshot_sha256": format!("{:x}", Sha256::digest(postcard::to_allocvec(&endpoint)?))}),
    )
}

fn evaluate<G: Game>(
    game: &G,
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
    let config = CampaignConfig {
        campaign_seed: request.seed,
        workers: request.workers,
        execution_budget: request.executions,
        action_limit: request.actions,
        host: "nes-eval".into(),
        wall_budget: Some(Duration::from_secs(request.wall_seconds)),
        continue_after_victory: false,
        archive_entry_limit: MAX_ARCHIVE_ENTRIES,
        reservations_per_worker: request.window,
        memory_budget_mib: Some(request.memory_mib),
        materialize_final_artifacts: full,
        run: run.clone(),
        suffix: suffix_shape_from_identifier(&request.suffix)?,
        mixture: draw_mixture_from_identifier(&request.mixture)?,
        retention: RetentionPolicy::AdmitAlive,
        selector: selector_policy_from_identifier(
            &request.selector,
            G::Key::groups().saturating_sub(2),
        )?,
        victory_input_path: Some(out.join("victory-input.json")),
    };
    if request.workers == 0
        || request.executions == 0
        || request.actions == 0
        || request.memory_mib == 0
        || request.window == 0
        || request.wall_seconds == 0
    {
        return Err("search limits must be positive".into());
    }
    if request.actions > game.max_action_limit() {
        return Err("actions exceed the adapter limit".into());
    }
    let result_buffering = match request.result_slots {
        1 => ResultBuffering::OnePerWorker,
        2 => ResultBuffering::TwoPerWorker,
        _ => return Err("result_slots must be 1 or 2".into()),
    };
    let stop_after_milestone = request
        .stop_after_milestone
        .as_deref()
        .map(|name| {
            game.named_milestone(name)
                .ok_or("workload does not support this milestone stop")
        })
        .transpose()?;
    let mut identity = json!({"format":"nes-eval-identity-v1", "game":request.game, "whole_game":request.whole_game, "level":request.level, "stage":request.stage, "ai":request.ai, "rom_sha256":request.rom_sha256, "core_sha256":request.core_sha256, "backend":"native", "source_tree_sha256":option_env!("HARMONY_SEARCH_SOURCE_SHA256"), "policies":game.policies(&run), "seed":request.seed, "workers":request.workers, "executions":request.executions, "frames":request.frames, "actions":request.actions, "memory_mib":request.memory_mib, "window":request.window, "result_slots":request.result_slots, "wall_seconds":request.wall_seconds, "selector":request.selector, "suffix":request.suffix, "mixture":request.mixture, "verification":request.verification, "retention_audit":request.retention_audit,"slot_retention":request.slot_retention,"mm2_chain":request.mm2_chain,"prefix_sha256":request.prefix_sha256});
    if let Some((name, policy)) = stop_after_milestone {
        identity["milestone_stop"] = json!({"name":name,"observation_policy":policy});
    }
    write_json(&out.join("identity.json"), &identity)?;
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
        game,
        &config,
        &CampaignOrigin::Genesis,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            frame_budget: request.frames,
            stop_after_milestone: stop_after_milestone.map(|(name, _)| name),
            result_buffering,
            slot_retention: nes_workload::search::archive::SlotRetentionPolicy::from_identifier(
                request.slot_retention.as_deref(),
            )?,
        },
    )?;
    stream.flush()?;
    progress.flush()?;
    let search_seconds = search_started.elapsed().as_secs_f64();
    phase(out, "export", started)?;
    let export_started = telemetry_now();
    let mut value = serde_json::to_value(&report)?;
    let witness: Input<G::Action> = if let Some(input) = &report.victory_input {
        input.clone()
    } else {
        serde_json::from_value(value["archive"]["champion_input"].clone())?
    };
    write_json(&out.join("witness-input.json"), &witness)?;
    if full {
        write_json(&out.join("checkpoint.json"), &checkpoint)?;
    }
    // Large archives are verification-only intermediates. Keep bounded campaign evidence.
    if let Some(archive) = value.get_mut("archive").and_then(Value::as_object_mut) {
        archive.remove("entries");
    }
    write_json(&out.join("campaign.json"), &value)?;
    let export_seconds = export_started.elapsed().as_secs_f64();
    game.finish_retention_observation()?;
    phase(out, "verification", started)?;
    let verify_started = telemetry_now();
    let first = replay_witness(game, &run, &witness)?;
    let second = replay_witness(game, &run, &witness)?;
    if first != second {
        return Err("witness replay endpoint is nondeterministic".into());
    }
    if report.victories > 0 && first["victory"] != true {
        return Err("reported victory was not reproduced by its witness".into());
    }
    if full {
        let bytes = fs::read(out.join("stream.jsonl"))?;
        let (replayed, replay_checkpoint) = replay_campaign_checkpointed(game, &bytes, None, None)?;
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
            let replay = replay_witness(game, &run, &input)?;
            if replay != replay_witness(game, &run, &input)? {
                return Err("milestone witness replay is nondeterministic".into());
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .ok_or("invalid milestone name")?;
            if replay["diagnostics"]["named_progress"]["first_seen"][name].is_null() {
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
    let solved = event_within_budget(report.frames_to_first_victory, request.frames);
    let milestone_reached = event_within_budget(
        report.first_milestone.map(|event| event.frames_emulated),
        request.frames,
    );
    let mut result = json!({"format":"nes-eval-result-v1", "status":"complete", "solved":solved,
        "victory_observed":report.victories>0,
        "frame_budget_overshoot":request.frames.map(|limit| report.frames_emulated.saturating_sub(limit)),
        "executions": report.executions_completed, "frames_emulated":report.frames_emulated,
        "frames_to_first_victory":report.frames_to_first_victory, "executions_to_first_victory":report.executions_to_first_victory,
        "preparation_seconds":preparation_seconds, "search_seconds":search_seconds, "executions_per_second":report.executions_completed as f64 / search_seconds,
        "progress":value["archive"]["progress_watermark"], "milestones":value["archive"]["milestones"], "frames_per_second": report.frames_emulated as f64 / search_seconds,
        "export_seconds":export_seconds, "verification_seconds":verification_seconds,
        "verification":request.verification, "witness":first, "milestone_witnesses":milestone_witnesses,
        "stream_sha256":format!("{:x}",stream.digest.finalize()), "stream_bytes_generated":stream.bytes, "stream_retained":full,
        "stop_reason":if solved {"victory"} else if milestone_reached {"milestone"} else if report.executions_completed >= request.executions {"execution_limit"} else if request.frames.is_some_and(|limit| report.frames_emulated >= limit) {"frame_limit"} else {"wall_limit"}
    });
    if let Some(condition) = &report.milestone_stop {
        result["milestone_stop"] = serde_json::to_value(condition)?;
        result["first_milestone"] = serde_json::to_value(report.first_milestone)?;
        result["milestone_within_budget"] = json!(milestone_reached);
    }
    write_json(&out.join("result.json"), &result)?;
    phase(out, "done", started)
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: nes-eval REQUEST.json OUTPUT_DIRECTORY".into());
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
    if (request.mm2_chain || request.prefix_input.is_some() || request.prefix_sha256.is_some())
        && request.game != "mm2"
    {
        return Err("chain origins are supported only for MM2".into());
    }
    if request.prefix_input.is_some() != request.prefix_sha256.is_some()
        || (request.prefix_input.is_some() && !request.mm2_chain)
    {
        return Err("a chain prefix requires explicit chain scope and SHA-256".into());
    }
    if request.retention_audit && request.game != "metroid" {
        return Err("replacement-pair observation currently requires Metroid".into());
    }
    if request.metroid_terminal.is_some() && request.game != "metroid" {
        return Err("Metroid terminal policy requires the Metroid adapter".into());
    }
    match request.game.as_str() {
        "smb" | "metroid"
            if request.level.is_some()
                || request.stage.is_some()
                || request.ai.is_some()
                || request.whole_game =>
        {
            return Err("this game takes no level, stage, ai or whole_game option".into());
        }
        "nova"
            if request.stage.is_some()
                || request.ai.is_some()
                || (request.whole_game && request.level.unwrap_or(1) != 1) =>
        {
            return Err(
                "Nova whole-game evaluation must start at level 1; stage and ai are unsupported"
                    .into(),
            );
        }
        "mm2" if request.level.is_some() || request.ai.is_some() || request.whole_game => {
            return Err("MM2 evaluation currently supports independent stages only".into());
        }
        "stb" if request.level.is_some() || request.stage.is_some() || request.whole_game => {
            return Err("STB evaluation takes only an ai option".into());
        }
        _ => {}
    }
    let p = &request.core;
    let h = &request.core_sha256;
    match request.game.as_str() {
        "smb" => evaluate(
            &SmbGame::new(&rom, p, h),
            SmbCampaignRun {
                chord: Default::default(),
                vocabulary: Default::default(),
                terminal: Some(SmbTerminalPredicate::GameVictory),
            },
            &request,
            &out,
            started,
        ),
        "nova" => {
            let game = NovaGame::new_at_level(
                &rom,
                p,
                h,
                NovaLevel::from_number(request.level.unwrap_or(1))?,
            );
            evaluate(
                &if request.whole_game {
                    game.with_whole_game()
                } else {
                    game
                },
                NovaCampaignRun,
                &request,
                &out,
                started,
            )
        }
        "mm2" => {
            let stage = Mm2Stage::from_number(request.stage.unwrap_or(0))?;
            let prefix = request
                .prefix_input
                .as_ref()
                .map(|path| -> Result<Mm2Input> {
                    let bytes = fs::read(path)?;
                    if Some(format!("{:x}", Sha256::digest(&bytes))) != request.prefix_sha256 {
                        return Err("chain prefix checksum mismatch".into());
                    }
                    Ok(serde_json::from_slice(&bytes)?)
                })
                .transpose()?;
            let game = match prefix {
                Some(input) => Mm2Game::new_at_stage_after(&rom, p, h, input.actions, stage),
                None => Mm2Game::new_at_stage(&rom, p, h, stage),
            };
            let outcome = evaluate(&game, Mm2CampaignRun, &request, &out, started);
            let mut chain_setup = json!({"format":"mm2-chain-stage-cost-v1", "new_target_setup_frames":game.setup_frame_count(), "stage":stage.name(), "prefix_sha256":request.prefix_sha256, "scope":"freshness established by enclosing chain manifest, not by this stage tool"});
            if request.mm2_chain {
                write_json(&out.join("chain-cost.json"), &chain_setup)?;
            }
            if outcome.is_ok() && request.mm2_chain && out.join("victory-input.json").is_file() {
                let victory: Mm2Input =
                    serde_json::from_slice(&fs::read(out.join("victory-input.json"))?)?;
                let mut target = game.new_target()?;
                let mut next = target.genesis_prefix().to_vec();
                let before = target.frames_clocked();
                let physical = target.physical_input(&victory)?;
                chain_setup["physical_export_replay_frames"] =
                    json!(target.frames_clocked() - before);
                chain_setup["searched_victory_endpoint"] =
                    serde_json::to_value(target.mechanical_state())?;
                next.extend(physical.actions);
                let full = Mm2Input {
                    actions: next.clone(),
                };
                write_json(&out.join("full-victory-input.json"), &full)?;
                let mut transition_frames = 0;
                if !stage.is_wily() {
                    let walk = game.walk_to_stage_select(&next)?;
                    transition_frames = next
                        .iter()
                        .chain(&walk)
                        .map(|a| u64::from(a.bounded_hold_frames()))
                        .sum::<u64>();
                    next.extend(walk);
                }
                write_json(&out.join("next-prefix.json"), &Mm2Input { actions: next })?;
                chain_setup["new_target_setup_frames"] = json!(game.setup_frame_count());
                chain_setup["award_transition_physical_frames"] = json!(transition_frames);
            }
            if request.mm2_chain {
                write_json(&out.join("chain-cost.json"), &chain_setup)?;
            }
            outcome
        }
        "metroid" => evaluate(
            &{
                let game = MetroidGame::new(&rom, p, h)
                    .with_milestone_input_dir(out.join("milestone-inputs"))
                    .with_terminal_policy(
                        nes_workload::metroid::target::MetroidTerminalPolicy::parse(
                            request
                                .metroid_terminal
                                .as_deref()
                                .unwrap_or("death_or_ending_v2"),
                        )?,
                    );
                if request.retention_audit {
                    game.with_retention_audit(out.join("retention-audit.json"))
                } else {
                    game
                }
            },
            MetroidCampaignRun,
            &request,
            &out,
            started,
        ),
        "stb" => {
            let ai = match request.ai.as_deref().unwrap_or("hard") {
                "easy" => StbAi::Easy,
                "fair" => StbAi::Fair,
                "hard" => StbAi::Hard,
                _ => return Err("unknown STB difficulty".into()),
            };
            evaluate(
                &StbGame::with_ai(&rom, p, h, ai),
                StbCampaignRun,
                &request,
                &out,
                started,
            )
        }
        _ => Err("unsupported game".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::event_within_budget;

    #[test]
    fn a_victory_in_the_drained_window_does_not_pass_the_frame_gate() {
        assert!(event_within_budget(Some(128), Some(128)));
        assert!(!event_within_budget(Some(129), Some(128)));
        assert!(event_within_budget(Some(129), None));
        assert!(!event_within_budget(None, Some(128)));
    }
}
