// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::{
    search::{
        archive::{ArchiveKey, Input, MAX_ARCHIVE_ENTRIES, RetentionPolicy},
        campaign::{
            CampaignConfig, CampaignExecutionOptions, CampaignOrigin, CampaignWatch,
            MinimizeConfig, SavedStates, Workload, replay_campaign_checkpointed,
            run_campaign_watched,
        },
        checkpoint::CheckpointPlan,
        draw::draw_mixture_from_identifier,
    },
    witness::replay_witness,
};
use nes_games::workload::{RECORDS_FILE, REPLAY_RECORDS_FILE};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json, value::RawValue};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
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
    pub wall_seconds: u64,
    #[serde(default)]
    pub mixture: String,
    pub verification: String,
    #[serde(default)]
    pub stage: Option<u8>,
    #[serde(default)]
    pub ai: Option<String>,
    #[serde(default)]
    pub metroid_terminal: Option<String>,
    #[serde(default)]
    pub smb_route: Option<String>,
    #[serde(default)]
    pub smb_terminal: Option<String>,
    #[serde(default)]
    pub smb_coverage_cap: Option<u8>,
    #[serde(default)]
    pub minimize: bool,
    #[serde(default)]
    pub root_input: Option<PathBuf>,
    #[serde(default)]
    pub checkpoint_every: Option<NonZeroU64>,
    #[serde(default)]
    pub checkpoint_on_progress: bool,
    #[serde(default)]
    pub checkpoint_on_stall: bool,
    #[serde(default)]
    pub resume: Option<PathBuf>,
    #[serde(default)]
    pub resume_states: Option<PathBuf>,
    #[serde(default)]
    pub live: Option<String>,
    #[serde(default)]
    pub live_site: Option<PathBuf>,
    #[serde(default)]
    pub strategy: Option<PathBuf>,
    #[serde(default)]
    pub records: bool,
    #[serde(default)]
    pub keep_stream: bool,
}

struct StreamDigest {
    file: Option<BufWriter<fs::File>>,
    digest: Sha256,
    bytes: u64,
    decisions: DecisionDigest,
}

pub const DECISION_MARKS_FILE: &str = "decision-marks.jsonl";
const DECISION_MARK_EVERY: u64 = 1024;
const WORKLOAD_FIELDS: [&str; 3] = [
    "result_sha256",
    "draw_checkpoint_before",
    "draw_checkpoint_after",
];

#[derive(Default)]
struct DecisionDigest {
    line: Vec<u8>,
    header: bool,
    canonical: Vec<u8>,
    digest: Sha256,
    records: u64,
    marks: Vec<(u64, String)>,
}

impl DecisionDigest {
    fn update(&mut self, buf: &[u8]) -> io::Result<()> {
        for chunk in buf.split_inclusive(|byte| *byte == b'\n') {
            self.line.extend_from_slice(chunk);
            if self.line.last() != Some(&b'\n') {
                continue;
            }
            if self.header {
                self.digest_line()?;
            } else {
                self.header = true;
            }
            self.line.clear();
        }
        Ok(())
    }

    fn digest_line(&mut self) -> io::Result<()> {
        self.canonical.clear();
        if canonical_record(&self.line, &mut self.canonical).is_err() {
            self.canonical.clear();
            let mut record: Value = serde_json::from_slice(&self.line).map_err(io::Error::other)?;
            if let Some(fields) = record.as_object_mut() {
                for field in WORKLOAD_FIELDS {
                    fields.remove(field);
                }
                if let Some(draw) = fields
                    .get_mut("duration_draw")
                    .and_then(Value::as_object_mut)
                {
                    draw.remove("context");
                }
            }
            serde_json::to_writer(&mut self.canonical, &record).map_err(io::Error::other)?;
        }
        self.digest.update(&self.canonical);
        self.records += 1;
        if self.records.is_multiple_of(DECISION_MARK_EVERY) {
            self.marks.push((
                self.records,
                format!("{:x}", self.digest.clone().finalize()),
            ));
        }
        Ok(())
    }
}

fn canonical_record(line: &[u8], out: &mut Vec<u8>) -> serde_json::Result<()> {
    let mut fields: BTreeMap<&str, &RawValue> = serde_json::from_slice(line)?;
    for field in WORKLOAD_FIELDS {
        fields.remove(field);
    }
    canonical_object(fields, out, |key| key == "duration_draw")
}

fn canonical_object(
    fields: BTreeMap<&str, &RawValue>,
    out: &mut Vec<u8>,
    drops_context: impl Fn(&str) -> bool,
) -> serde_json::Result<()> {
    out.push(b'{');
    for (index, (key, value)) in fields.into_iter().enumerate() {
        if index > 0 {
            out.push(b',');
        }
        serde_json::to_writer(&mut *out, key)?;
        out.push(b':');
        canonical_value(value, out, drops_context(key))?;
    }
    out.push(b'}');
    Ok(())
}

fn canonical_value(
    value: &RawValue,
    out: &mut Vec<u8>,
    drops_context: bool,
) -> serde_json::Result<()> {
    let text = value.get();
    match text.as_bytes().first() {
        Some(b'{') => {
            let mut fields: BTreeMap<&str, &RawValue> = serde_json::from_str(text)?;
            if drops_context {
                fields.remove("context");
            }
            canonical_object(fields, out, |_| false)?;
        }
        Some(b'[') => {
            let items: Vec<&RawValue> = serde_json::from_str(text)?;
            out.push(b'[');
            for (index, item) in items.into_iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                canonical_value(item, out, false)?;
            }
            out.push(b']');
        }
        _ => out.extend_from_slice(text.as_bytes()),
    }
    Ok(())
}

impl Write for StreamDigest {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(file) = &mut self.file {
            file.write_all(buf)?;
        }
        self.digest.update(buf);
        self.decisions.update(buf)?;
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
    execution_sha256: &str,
    out: &Path,
    started: Instant,
) -> Result<()>
where
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Snapshot: crate::witness::EndpointIdentity,
{
    if request.live.is_some() {
        return Err("this game has no live view".into());
    }
    evaluate_watched(&game, run, request, execution_sha256, out, started, &())
}

pub fn evaluate_watched<G: Workload, W: CampaignWatch<G>>(
    game: &G,
    run: G::Run,
    request: &Request,
    execution_sha256: &str,
    out: &Path,
    started: Instant,
    watch: &W,
) -> Result<()>
where
    G::ArchiveReport: Serialize + serde::de::DeserializeOwned,
    G::Snapshot: crate::witness::EndpointIdentity,
{
    let full = request.verification == "campaign";
    if request.verification != "witness" && !full {
        return Err("verification must be campaign or witness".into());
    }
    if full && request.executions > 10000 {
        return Err("full campaign verification is bounded to 10000 executions; use witness verification for performance runs".into());
    }
    if full && (request.resume.is_some() || request.resume_states.is_some()) {
        return Err("a run resumed from a search checkpoint has no stream to replay; use witness verification".into());
    }
    let origin = match (&request.resume, &request.resume_states) {
        (Some(_), Some(_)) => {
            return Err("resume and resume_states name two origins; choose one".into());
        }
        (Some(path), None) => CampaignOrigin::SearchCheckpoint {
            path: path.clone(),
            expected_sha256: None,
        },
        (None, Some(path)) => CampaignOrigin::SavedStates {
            states: SavedStates::open(game, path)?,
        },
        (None, None) => CampaignOrigin::Genesis,
    };
    let checkpoints = (request.checkpoint_every.is_some()
        || request.checkpoint_on_progress
        || request.checkpoint_on_stall)
        .then(|| CheckpointPlan {
            directory: out.join("checkpoints"),
            every: request.checkpoint_every,
            on_marks: request.checkpoint_on_progress,
            on_top_progress: request.checkpoint_on_progress,
            on_stall: request.checkpoint_on_stall,
            checkpoint_owner: None,
            max_image_bytes: 64 << 20,
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
        window: request
            .window
            .saturating_mul(usize::try_from(request.workers)?),
        memory_budget_mib: Some(request.memory_mib),
        materialize_final_artifacts: full,
        run: run.clone(),
        mixture: draw_mixture_from_identifier(&request.mixture)?,
        retention: RetentionPolicy::Unprobed,
        objective_witness_path: Some(out.join("victory-input.json")),
        minimize: request.minimize.then(MinimizeConfig::default),
    };
    if request.workers == 0
        || request.executions == 0
        || request.memory_mib == 0
        || request.window == 0
        || request.wall_seconds == 0
    {
        return Err("search limits must be positive".into());
    }
    let strategy_sha256 = request
        .strategy
        .as_ref()
        .map(|path| fs::read(path).map(|text| format!("{:x}", Sha256::digest(text))))
        .transpose()?;
    write_json(
        &out.join("identity.json"),
        &json!({"format":"nes-eval-identity-v5", "strategy":request.strategy, "strategy_sha256":strategy_sha256, "records":request.records, "keep_stream":request.keep_stream, "game":request.game, "whole_game":request.whole_game, "stage":request.stage, "ai":request.ai, "rom_sha256":request.rom_sha256, "core_sha256":request.core_sha256, "prepared_execution_sha256":execution_sha256, "backend":"native", "source_tree_sha256":option_env!("HARMONY_SEARCH_SOURCE_SHA256"), "policies":game.policies(&run), "seed":request.seed, "workers":request.workers, "executions":request.executions, "frames":request.frames, "memory_mib":request.memory_mib, "window":request.window, "wall_seconds":request.wall_seconds, "mixture":request.mixture, "verification":request.verification, "checkpoint_every":request.checkpoint_every, "checkpoint_on_progress":request.checkpoint_on_progress, "checkpoint_on_stall":request.checkpoint_on_stall, "resume":request.resume, "resume_states":request.resume_states, "root_input":request.root_input, "minimize":request.minimize}),
    )?;
    let mut stream = StreamDigest {
        file: if full || request.keep_stream {
            Some(BufWriter::new(fs::File::create(out.join("stream.jsonl"))?))
        } else {
            None
        },
        digest: Sha256::new(),
        bytes: 0,
        decisions: DecisionDigest::default(),
    };
    let mut progress = LineWriter::new(fs::File::create(out.join("progress.jsonl"))?);
    let preparation_seconds = started.elapsed().as_secs_f64();
    phase(out, "search", started)?;
    let search_started = telemetry_now();
    let (report, checkpoint) = run_campaign_watched(
        game,
        &config,
        &origin,
        &mut stream,
        Some(&mut progress),
        CampaignExecutionOptions {
            work_budget: request.frames,
            checkpoints,
            placement: None,
            interrupt: None,
        },
        watch,
    )?;
    stream.flush()?;
    progress.flush()?;
    let search_seconds = search_started.elapsed().as_secs_f64();
    let records = game.direct_records(None)?;
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
        checkpoint.write_to(&mut fs::File::create(out.join("checkpoint.bin"))?)?;
    }
    if let Some(archive) = value.get_mut("archive").and_then(Value::as_object_mut) {
        archive.remove("entries");
    }
    write_json(&out.join("campaign.json"), &value)?;
    let export_seconds = export_started.elapsed().as_secs_f64();
    phase(out, "verification", started)?;
    let verify_started = telemetry_now();
    let first = replay_witness(game, &run, &witness)?;
    let second = replay_witness(game, &run, &witness)?;
    if first != second {
        return Err("witness replay endpoint is nondeterministic".into());
    }
    if report.objectives_reached > 0 && first["victory"] != true {
        return Err("reported victory was not reproduced by its witness".into());
    }
    let mut replay_records_identical = None;
    if full {
        let bytes = fs::read(out.join("stream.jsonl"))?;
        if records.is_some() {
            game.direct_records(Some(&out.join(REPLAY_RECORDS_FILE)))?;
        }
        let (replayed, replay_checkpoint) = replay_campaign_checkpointed(game, &bytes, None, None)?;
        if records.is_some() {
            game.direct_records(None)?;
            replay_records_identical =
                Some(fs::read(out.join(RECORDS_FILE))? == fs::read(out.join(REPLAY_RECORDS_FILE))?);
        }
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
    let mut marks = BufWriter::new(fs::File::create(out.join(DECISION_MARKS_FILE))?);
    for (records, digest) in &stream.decisions.marks {
        writeln!(marks, "{}", json!({"records": records, "sha256": digest}))?;
    }
    marks.flush()?;
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
            "strategy_sha256":strategy_sha256,
            "records":records.map(|size| json!({"jobs":size.jobs, "bytes":size.bytes, "bytes_per_job":size.bytes as f64 / size.jobs.max(1) as f64, "replay_identical":replay_records_identical})),
            "decision_stream_sha256":format!("{:x}",stream.decisions.digest.clone().finalize()), "decision_stream_records":stream.decisions.records,
            "stream_sha256":format!("{:x}",stream.digest.finalize()), "stream_bytes_generated":stream.bytes, "stream_retained":full || request.keep_stream,
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
    use super::{DecisionDigest, WORKLOAD_FIELDS, objective_within_budget};
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    fn decisions(lines: &[&str]) -> (String, u64) {
        let mut digest = DecisionDigest::default();
        digest.update(b"{\"header\":1}\n").unwrap();
        for line in lines {
            let (first, second) = line.split_at(line.len() / 2);
            digest.update(first.as_bytes()).unwrap();
            digest.update(second.as_bytes()).unwrap();
            digest.update(b"\n").unwrap();
        }
        (format!("{:x}", digest.digest.finalize()), digest.records)
    }

    #[test]
    fn the_decision_digest_ignores_only_the_workload_fields() {
        let base = decisions(&[
            r#"{"job":3,"result_sha256":"aa","draw_checkpoint_before":{"k":[1]},"duration_draw":{"steps":4,"context":{"x":1}}}"#,
        ]);
        let other_workload = decisions(&[
            r#"{"result_sha256":"bb","job":3,"draw_checkpoint_after":null,"duration_draw":{"context":{"y":2},"steps":4}}"#,
        ]);
        let other_choice = decisions(&[
            r#"{"job":3,"result_sha256":"aa","duration_draw":{"steps":5,"context":{"x":1}}}"#,
        ]);
        assert_eq!(base, other_workload);
        assert_ne!(base.0, other_choice.0);
        assert_eq!(base.1, 1);
    }

    #[test]
    fn the_decision_digest_matches_a_parsed_value_digest() {
        let lines = [
            r#"{"selector":{"path":"tiers","tier_rank":0,"best_preference":0},"job":1}"#,
            r#"{"picks":[{"z":1,"a":[{"y":2,"b":null}]},3.5,"s\"q"],"duration_draw":{"steps":4,"context":{"x":1},"b":{"d":1,"c":2}}}"#,
            r#"{"na\u006de":1,"result_sha256":"aa","inner":{"k\u0065y":{"b":1,"a":2}}}"#,
            r#"[3,{"b":1,"a":2}]"#,
            r#"{ "spaced" : { "b" : 1 , "a" : [ 1 , 2 ] } }"#,
        ];
        let mut reference = Sha256::new();
        for line in lines {
            let mut record: Value = serde_json::from_str(line).unwrap();
            if let Some(fields) = record.as_object_mut() {
                for field in WORKLOAD_FIELDS {
                    fields.remove(field);
                }
                if let Some(draw) = fields
                    .get_mut("duration_draw")
                    .and_then(Value::as_object_mut)
                {
                    draw.remove("context");
                }
            }
            reference.update(serde_json::to_vec(&record).unwrap());
        }
        assert_eq!(
            decisions(&lines),
            (format!("{:x}", reference.finalize()), lines.len() as u64)
        );
    }

    #[test]
    fn a_victory_in_the_drained_window_does_not_pass_the_frame_check() {
        assert!(objective_within_budget(Some(128), Some(128)));
        assert!(!objective_within_budget(Some(129), Some(128)));
        assert!(objective_within_budget(Some(129), None));
        assert!(!objective_within_budget(None, Some(128)));
    }
}
