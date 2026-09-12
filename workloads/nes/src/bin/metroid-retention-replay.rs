// SPDX-License-Identifier: AGPL-3.0-or-later
//! Exact saved-stream replay and independent paused inspection; never fresh search.
use nes_workload::{
    metroid::{
        boss_interval::classify,
        campaign::{
            MetroidCampaignRun, MetroidGame, MetroidNoTableHeader, SNAPSHOT_CHECKPOINT_FORMAT,
        },
        retention_capture::{CaptureIdentity, CaptureReader},
        target::{MetroidInput, MetroidSnapshot, MetroidTarget, MetroidTerminalPolicy},
    },
    search::campaign::{
        CampaignCheckpoint, CampaignStreamHeader, CampaignStreamRecord, InputPolicy, Reporting,
        SnapshotCheckpoint, SnapshotCheckpointEntry, replay_campaign_checkpointed,
    },
    target::{ExitKind, Target},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Header = CampaignStreamHeader<MetroidNoTableHeader>;
const INPUT_LIMIT: u64 = 32 * 1024 * 1024;
const SETUP: u64 = 929;
const QUALIFY_JOBS: u64 = 4;
#[path = "metroid-retention-replay/correspondence.rs"]
mod correspondence;
#[path = "metroid-retention-replay/extract.rs"]
mod extract;

// Host telemetry is outside replay state and cannot supply a draw or decision.
#[allow(clippy::disallowed_methods)]
fn telemetry_now() -> Instant {
    Instant::now()
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pinned {
    path: PathBuf,
    sha256: String,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reconstruction {
    input: Pinned,
    expected_actions: u64,
    expected_frames: u64,
}
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Qualify,
    Inspect,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    phase: Phase,
    stream: Pinned,
    origin: Pinned,
    final_checkpoint: Pinned,
    rom: Pinned,
    core: Pinned,
    original_core_sha256: String,
    root_snapshot_sha256: String,
    expected_root_context: Value,
    expected_jobs: u64,
    expected_frames: u64,
    expected_competitions: u64,
    physical_frame_ceiling: u64,
    wall_seconds: u64,
    /// Required only for inspection, binding the successful qualification report.
    qualification: Option<Pinned>,
    /// Explicitly registered action reconstruction; never import a foreign state.
    reconstruction: Option<Reconstruction>,
}
fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn valid_sha(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn read(path: &Path) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(INPUT_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len())? > INPUT_LIMIT {
        return Err("input exceeds 32 MiB".into());
    }
    Ok(bytes)
}
fn pinned(value: &Pinned) -> Result<Vec<u8>> {
    let bytes = read(&value.path)?;
    if !valid_sha(&value.sha256) || sha(&bytes) != value.sha256 {
        return Err("pinned input identity mismatch".into());
    }
    Ok(bytes)
}
fn write(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

struct Prepared {
    header: Header,
    stream: Vec<u8>,
    jobs: u64,
    frames: u64,
    qualify_frames: u64,
    original_backend: String,
    runtime_backend: String,
    complete_body_sha256: String,
}

#[derive(Default, Serialize)]
struct Cost {
    inspector_constructor_started: bool,
    inspector_setup_frames: Option<u64>,
    reconstruction_frames: u64,
    replay_started: bool,
    verified_replay_frames: Option<u64>,
}
impl Cost {
    fn receipt(&self, output: &Path, stage: &str) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(output.join("cost-progress.jsonl"))?;
        serde_json::to_writer(&mut file, &json!({"stage":stage,"cost":self}))?;
        file.write_all(b"\n")?;
        file.flush()?;
        Ok(())
    }
}

fn prepare(q: &Request, original: &[u8], game: &MetroidGame) -> Result<Prepared> {
    // QuickNES snapshots bind the exact core binary in their header. A stream
    // declaration cannot make a foreign-build snapshot importable. Check this
    // before creating either target; reconstruction from actions is a separate
    // protocol and must not rewrite the saved snapshot's identity.
    if q.reconstruction.is_none() && q.original_core_sha256 != q.core.sha256 {
        return Err("raw snapshot replay requires the original QuickNES core build".into());
    }
    if !valid_sha(&q.original_core_sha256)
        || !valid_sha(&q.root_snapshot_sha256)
        || !(QUALIFY_JOBS..=5_000).contains(&q.expected_jobs)
        || !(1..=1_000_000).contains(&q.expected_frames)
        || !(1..=5_000).contains(&q.expected_competitions)
        || !(1..=300).contains(&q.wall_seconds)
        || q.physical_frame_ceiling > 1_500_000
        || (q.phase == Phase::Inspect) != q.qualification.is_some()
    {
        return Err("invalid replay request bounds or qualification phase".into());
    }
    let original = std::str::from_utf8(original)?;
    let (header_line, body) = original
        .split_once('\n')
        .ok_or("missing stream header newline")?;
    if !body.ends_with('\n') {
        return Err("stream must end at a complete recorded line".into());
    }
    let header: Header = serde_json::from_str(header_line)?;
    if let Some(reconstruction) = &q.reconstruction
        && (!valid_sha(&reconstruction.input.sha256)
            || !(1..=4_096).contains(&reconstruction.expected_actions)
            || !(1..=4_096 * 120).contains(&reconstruction.expected_frames)
            || header.origin_checkpoint_path.as_deref()
                != Some(
                    format!("diagnostic-prefix-sha256:{}", reconstruction.input.sha256).as_str(),
                ))
    {
        return Err("reconstruction must bind the recorded original action prefix".into());
    }
    if header.format != game.stream_format()
        || header.origin_kind != "snapshot_root"
        || header.resume_actions != 0
        || header.origin_path.is_some()
        || header.origin_archive_sha256.is_some()
        || header
            .origin_checkpoint_path
            .as_ref()
            .is_none_or(String::is_empty)
        || header.resume_policy != "snapshot_root"
        || header.resume_input_sha256 != sha(b"{\"actions\":[]}")
        || header.executor_mode != "snapshot_resume_archive"
        || header.origin_checkpoint_sha256.as_deref() != Some(q.origin.sha256.as_str())
        || header.rom_sha256 != q.rom.sha256
        || header.action_limit != 4_096
        || header.workers != 4
        || header.memory_budget_mib != Some(512)
        || header.execution_budget != 5_000
        || header.frame_budget != Some(250_000)
        || header.suffix_policy != "one_to_six"
        || header.mixture_policy != "alphabet_only"
        || header.retention_policy != "admit_alive"
        || header.slot_retention.is_some()
        || header.parent_scheduler != "room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2"
        || header.milestone_stop.as_ref().is_none_or(|m| {
            m.name != "ridley_defeated" || m.observation_policy != "metroid-named-progress-v2"
        })
        || header.schedule_policy.as_deref() != Some("deterministic_window_1_per_worker_v3")
    {
        return Err("stream is outside the qualified short ordinary snapshot-root protocol".into());
    }
    let runtime = game.policies(&MetroidCampaignRun);
    let runtime_backend = runtime
        .get("emulator_backend")
        .ok_or("runtime backend missing")?
        .clone();
    let prefix = runtime_backend
        .strip_suffix(&q.core.sha256)
        .ok_or("runtime core identity missing")?;
    let original_backend = format!("{prefix}{}", q.original_core_sha256);
    let mut expected_original = runtime;
    expected_original.insert("emulator_backend".into(), original_backend.clone());
    if header.game_policies != expected_original {
        return Err(
            "original policy or backend differs beyond the registered core identity".into(),
        );
    }
    let (mut jobs, mut frames, mut prefix_end, mut offset, mut prefix_frames) =
        (0_u64, 0_u64, 0, 0, 0);
    for line in body.split_inclusive('\n') {
        let record: CampaignStreamRecord = serde_json::from_str(line)?;
        offset += line.len();
        // AP01 has no pre-execution skip records. Refuse a larger protocol here.
        let CampaignStreamRecord::Job(job) = record else {
            return Err("unexpected skip record".into());
        };
        jobs += 1;
        if job.sequence != jobs
            || job.worker >= 4
            || job.frames > 4_096 * 120
            || job.splice.is_some()
            || job.splice_weight != 0
            || job.mixture_weight != 128
            || job.draw_table_before.is_some()
            || job.draw_table_after.is_some()
            || !valid_sha(&job.result_sha256)
        {
            return Err("recorded job order, worker or frame bounds changed".into());
        }
        frames = frames
            .checked_add(job.frames)
            .ok_or("recorded frame count overflow")?;
        if jobs == QUALIFY_JOBS {
            prefix_end = offset;
            prefix_frames = frames;
        }
    }
    if (jobs, frames) != (q.expected_jobs, q.expected_frames) {
        return Err("complete stream counts differ from the frozen request".into());
    }
    let selected_body = if q.phase == Phase::Qualify {
        jobs = QUALIFY_JOBS;
        frames = prefix_frames;
        &body[..prefix_end]
    } else {
        body
    };
    // Each successful job verifies its physical frame count. The first divergent
    // job can execute at most its origin reconstruction plus six 120-frame actions.
    let worst_frames = frames
        .checked_add((4_096 + 6) * 120 + 2 * SETUP)
        .and_then(|frames| {
            frames.checked_add(q.reconstruction.as_ref().map_or(0, |r| r.expected_frames))
        })
        .ok_or("frame ceiling overflow")?;
    if worst_frames > q.physical_frame_ceiling {
        return Err("frame ceiling cannot cover the first mismatching job and setup".into());
    }
    // Keep the original header and every selected job byte intact.
    let mut stream = header_line.as_bytes().to_vec();
    stream.push(b'\n');
    stream.extend_from_slice(selected_body.as_bytes());
    Ok(Prepared {
        header,
        stream,
        jobs,
        frames,
        qualify_frames: prefix_frames,
        original_backend,
        runtime_backend,
        complete_body_sha256: sha(body.as_bytes()),
    })
}

fn decode_reconstruction(reconstruction: &Reconstruction, bytes: &[u8]) -> Result<MetroidInput> {
    if sha(bytes) != reconstruction.input.sha256 {
        return Err("reconstruction input hash differs".into());
    }
    let input: MetroidInput = serde_json::from_slice(bytes)?;
    if input.actions.len() as u64 != reconstruction.expected_actions
        || input
            .actions
            .iter()
            .any(|a| !(1..=120).contains(&a.hold_frames))
        || input
            .actions
            .iter()
            .map(|a| u64::from(a.hold_frames))
            .sum::<u64>()
            != reconstruction.expected_frames
    {
        return Err("reconstruction action or frame count differs".into());
    }
    Ok(input)
}

fn derive_runtime_stream(
    prepared: &Prepared,
    reconstruction: bool,
    origin_sha256: &str,
) -> Result<Vec<u8>> {
    if !reconstruction {
        return Ok(prepared.stream.clone());
    }
    let (header, body) = std::str::from_utf8(&prepared.stream)?
        .split_once('\n')
        .ok_or("missing header")?;
    let mut value: Value = serde_json::from_str(header)?;
    value["emulator_backend"] = json!(prepared.runtime_backend);
    value["origin_checkpoint_sha256"] = json!(origin_sha256);
    let mut stream = serde_json::to_vec(&value)?;
    stream.push(b'\n');
    stream.extend_from_slice(body.as_bytes());
    Ok(stream)
}

fn verify_qualification(
    report: &Value,
    binding: &Value,
    context: &Value,
    frames: u64,
) -> Result<()> {
    if report["format"] != "metroid-retention-replay-v2"
        || report["phase"] != "qualify"
        || report["binding"] != *binding
        || report["verified_jobs"] != QUALIFY_JOBS
        || report["verified_job_frames"] != frames
        || report["root"]["context"] != *context
        || report["status"] != "complete"
    {
        return Err(
            "successful qualification does not bind this executable and exact inputs".into(),
        );
    }
    Ok(())
}

fn paused(target: &mut MetroidTarget, snapshot: &MetroidSnapshot) -> Result<Value> {
    let before = target.frames_clocked();
    target.restore(snapshot)?;
    let context = target.diagnostic_boss_context()?;
    if target.frames_clocked() != before || target.snapshot().as_ref() != Some(snapshot) {
        return Err("paused inspection changed the snapshot or physical clock".into());
    }
    let slots: Vec<_> = (0..6)
        .filter_map(|slot| classify(&context, slot))
        .map(|s| {
            json!({
                "area":s.area,"offset":s.offset,"data_index":s.data_index,"attributes":s.attributes,
                "status":s.status,"hp":if s.hp == 255 { None } else { Some(s.hp) }
            })
        })
        .collect();
    Ok(
        json!({"snapshot_sha256":sha(&postcard::to_allocvec(snapshot)?),"state":snapshot.state(),
        "alive":!target.is_dead() && target.exit_kind() == ExitKind::Ok,
        "victory":target.is_victory(),"context":context,"classified_slots":slots}),
    )
}

fn evaluate(q: &Request, output: &Path, executable_sha256: &str, cost: &mut Cost) -> Result<Value> {
    let original = pinned(&q.stream)?;
    let origin_bytes = pinned(&q.origin)?;
    let final_bytes = pinned(&q.final_checkpoint)?;
    let expected_checkpoint = SnapshotCheckpoint::<MetroidSnapshot>::from_bytes(
        &final_bytes,
        SNAPSHOT_CHECKPOINT_FORMAT,
    )?;
    if expected_checkpoint.entries.is_empty() || expected_checkpoint.entries.len() > 1_000 {
        return Err("final checkpoint exceeds the bounded saved pilot".into());
    }
    let rom = pinned(&q.rom)?;
    pinned(&q.core)?;
    let game = MetroidGame::new(&rom, &q.core.path, &q.core.sha256)
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    let prepared = prepare(q, &original, &game)?;
    let snapshots = SnapshotCheckpoint::<MetroidSnapshot>::from_bytes(
        &origin_bytes,
        SNAPSHOT_CHECKPOINT_FORMAT,
    )?;
    let [entry] = snapshots.entries.as_slice() else {
        return Err("origin must contain exactly one snapshot".into());
    };
    if entry.id != 0 || sha(&postcard::to_allocvec(&entry.snapshot)?) != q.root_snapshot_sha256 {
        return Err("root snapshot differs from the frozen identity".into());
    }
    let reconstruction = q
        .reconstruction
        .as_ref()
        .map(|r| decode_reconstruction(r, &pinned(&r.input)?))
        .transpose()?;
    // Check the saved artifact's claimed identity without importing its state.
    correspondence::compare(
        &entry.snapshot,
        &entry.snapshot,
        &q.original_core_sha256,
        &q.original_core_sha256,
    )?;
    let reconstruction_binding = q.reconstruction.as_ref().map(|r| {
        json!({
        "input_sha256":r.input.sha256,"actions":r.expected_actions,"frames":r.expected_frames,
        "snapshot_comparison":correspondence::POLICY})
    });
    let binding = json!({"executable_sha256":executable_sha256,"original_stream_sha256":q.stream.sha256,
        "origin_sha256":q.origin.sha256,"root_snapshot_sha256":q.root_snapshot_sha256,
        "final_checkpoint_sha256":q.final_checkpoint.sha256,
        "rom_sha256":q.rom.sha256,"runtime_core_sha256":q.core.sha256,
        "original_core_sha256":q.original_core_sha256,"complete_body_sha256":prepared.complete_body_sha256,
        "reconstruction":reconstruction_binding});
    let qualification = q
        .qualification
        .as_ref()
        .map(|p| -> Result<Value> { Ok(serde_json::from_slice(&pinned(p)?)?) })
        .transpose()?;
    if let Some(report) = &qualification {
        verify_qualification(
            report,
            &binding,
            &q.expected_root_context,
            prepared.qualify_frames,
        )?;
    }
    cost.inspector_constructor_started = true;
    cost.receipt(output, "inspector_constructor_started")?;
    let mut inspector = MetroidTarget::from_rom_bytes_headless(&rom, &q.core.path, &q.core.sha256)?
        .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
    cost.inspector_setup_frames = Some(inspector.frames_clocked());
    cost.receipt(output, "inspector_constructor_completed")?;
    if inspector.frames_clocked() != SETUP {
        return Err("inspector setup changed".into());
    }
    let runtime_root = if let Some(input) = &reconstruction {
        for (index, action) in input.actions.iter().enumerate() {
            if inspector.is_dead() || inspector.is_victory() {
                return Err("reconstruction input continues after a terminal state".into());
            }
            let before = inspector.frames_clocked();
            inspector.apply(action);
            let frames = inspector.frames_clocked() - before;
            cost.reconstruction_frames += frames;
            if index % 128 == 0 {
                cost.receipt(output, "reconstruction_in_progress")?;
            }
            if inspector.exit_kind() != ExitKind::Ok || frames != u64::from(action.hold_frames) {
                return Err("reconstruction action failed or clocked unexpected frames".into());
            }
        }
        cost.receipt(output, "reconstruction_completed")?;
        inspector
            .snapshot()
            .ok_or("reconstructed root snapshot failed")?
    } else {
        entry.snapshot.clone()
    };
    write(&output.join("runtime-root-snapshot.json"), &runtime_root)?;
    if reconstruction.is_some() {
        correspondence::compare(
            &runtime_root,
            &entry.snapshot,
            &q.core.sha256,
            &q.original_core_sha256,
        )?;
    }
    let root = paused(&mut inspector, &runtime_root)?;
    if root["context"] != q.expected_root_context
        || root["alive"] != true
        || root["victory"] != false
    {
        return Err("restored root differs from the live qualified context".into());
    }
    let runtime_snapshots = SnapshotCheckpoint {
        format: snapshots.format.clone(),
        entries: vec![SnapshotCheckpointEntry {
            id: 0,
            snapshot: runtime_root,
        }],
    };
    let runtime_origin = runtime_snapshots.to_bytes()?;
    let runtime_origin_sha256 = sha(&runtime_origin);
    if reconstruction.is_none() && runtime_origin != origin_bytes {
        return Err("same-build origin changed during serialization".into());
    }
    if let Some(qualification) = &qualification
        && (qualification["runtime_origin_sha256"] != runtime_origin_sha256
            || qualification["root"]["snapshot_sha256"] != root["snapshot_sha256"])
    {
        return Err("runtime root or origin differs from the qualification".into());
    }
    fs::write(output.join("runtime-origin.bin"), &runtime_origin)?;
    let runtime_stream =
        derive_runtime_stream(&prepared, reconstruction.is_some(), &runtime_origin_sha256)?;
    write(
        &output.join("derivation.json"),
        &json!({"binding":binding,"phase":q.phase,
        "changed_header_fields":if reconstruction.is_some() { vec!["emulator_backend","origin_checkpoint_sha256"] } else { vec![] },
        "original_backend":prepared.original_backend,"runtime_backend":prepared.runtime_backend,
        "runtime_origin_sha256":runtime_origin_sha256,"selected_jobs":prepared.jobs,
        "selected_frames":prepared.frames,"derived_stream_sha256":sha(&runtime_stream),
        "scope":"only a normally regenerated runtime root may replace the origin; selected original job bytes are unchanged"}),
    )?;
    fs::write(output.join("derived-stream.jsonl"), &runtime_stream)?;
    let checkpoint = CampaignCheckpoint {
        path: prepared
            .header
            .origin_checkpoint_path
            .ok_or("origin path missing")?,
        file_sha256: runtime_origin_sha256.clone(),
        snapshots: runtime_snapshots,
    };
    let capture_path = output.join("competitions.bin");
    let identity = CaptureIdentity {
        stream_sha256: sha(&runtime_stream),
        origin_sha256: runtime_origin_sha256.clone(),
    };
    let game = if q.phase == Phase::Inspect {
        game.with_retention_capture(&capture_path, identity.clone())?
    } else {
        game
    };
    cost.replay_started = true;
    cost.receipt(output, "campaign_replay_started")?;
    let (report, replay_checkpoint) =
        replay_campaign_checkpointed(&game, &runtime_stream, None, Some(&checkpoint))?;
    cost.verified_replay_frames = Some(report.frames_emulated);
    cost.receipt(output, "campaign_replay_completed")?;
    if report.frames_emulated != prepared.frames || report.executions_completed != prepared.jobs {
        return Err("replay totals differ from the selected original records".into());
    }
    write(&output.join("replay-report.json"), &report)?;
    if q.phase == Phase::Inspect {
        let runtime_final = replay_checkpoint.to_bytes()?;
        if runtime_final.len() as u64 > INPUT_LIMIT {
            return Err("runtime final checkpoint exceeds the bounded inspection size".into());
        }
        fs::write(output.join("runtime-final-checkpoint.bin"), &runtime_final)?;
        if reconstruction.is_some() {
            correspondence::compare_checkpoints(
                &replay_checkpoint,
                &expected_checkpoint,
                &q.core.sha256,
                &q.original_core_sha256,
            )?;
        } else if replay_checkpoint.to_bytes()? != final_bytes {
            return Err("final raw checkpoint differs from the original saved campaign".into());
        }
    }
    game.finish_retention_observation()?;
    let mut inspected = 0_u64;
    let mut restores = 1_u64;
    if q.phase == Phase::Inspect {
        let mut capture = CaptureReader::open(&capture_path)?;
        if capture.identity() != &identity {
            return Err("capture identity changed".into());
        }
        let mut contexts = BufWriter::new(
            fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output.join("contexts.jsonl"))?,
        );
        let mut output_bytes = 0;
        while let Some(row) = capture.next_competition()? {
            let candidate = paused(&mut inspector, &row.candidate_snapshot)?;
            restores += 1;
            let incumbent = row
                .incumbent_snapshot
                .as_ref()
                .map(|s| paused(&mut inspector, s))
                .transpose()?;
            restores += u64::from(incumbent.is_some());
            let value = json!({"ordinal":row.ordinal,"execution":row.execution,"incumbent_id":row.incumbent_id,
                "replaces":row.replaces,"candidate_key":row.candidate_key,"incumbent_key":row.incumbent_key,
                "candidate":candidate,"incumbent":incumbent,
                "candidate_input_sha256":sha(&serde_json::to_vec(&row.candidate_input)?),
                "incumbent_input_sha256":sha(&serde_json::to_vec(&row.incumbent_input)?)});
            let bytes = serde_json::to_vec(&value)?;
            output_bytes += bytes.len() + 1;
            if output_bytes > 32 * 1024 * 1024 {
                return Err("context output ceiling reached".into());
            }
            contexts.write_all(&bytes)?;
            contexts.write_all(b"\n")?;
            inspected += 1;
        }
        contexts.flush()?;
        if inspected != q.expected_competitions {
            return Err("competition capture is not exhaustive for the frozen pilot".into());
        }
    }
    if inspector.frames_clocked()
        != cost.inspector_setup_frames.unwrap_or(0) + cost.reconstruction_frames
    {
        return Err("paused inspector clock changed after reconstruction".into());
    }
    Ok(
        json!({"format":"metroid-retention-replay-v2","status":"complete","phase":q.phase,"binding":binding,
        "root":root,"verified_jobs":prepared.jobs,"verified_job_frames":prepared.frames,
        "runtime_origin_sha256":runtime_origin_sha256,
        "inspected_competitions":inspected,"verified_paused_restores":restores,
        "verified_checkpoint_entries":if q.phase == Phase::Inspect { replay_checkpoint.entries.len() } else { 0 },
        "verified_original_final_checkpoint":q.phase == Phase::Inspect && reconstruction.is_none(),
        "verified_final_checkpoint_correspondence":q.phase == Phase::Inspect && reconstruction.is_some(),
        "snapshot_comparison_policy":if reconstruction.is_some() { correspondence::POLICY } else { "raw_exact" },
        "known_replay_frames":report.frames_emulated,"known_inspector_setup_frames":cost.inspector_setup_frames,
        "known_reconstruction_frames":cost.reconstruction_frames,"cost":cost,
        "replay_constructor_setup_frames":"not exposed by the generic replay report; prospective bound includes929",
        "physical_frame_ceiling":q.physical_frame_ceiling,"wall_seconds":q.wall_seconds,
        "scope":"fixed existing pilot; snapshot-local observations, no fresh-search or retention-utility claim"}),
    )
}

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if let [mode, request, output] = args.as_slice()
        && mode == "extract"
    {
        return extract::run(Path::new(request), Path::new(output));
    }
    let [request, output] = args.as_slice() else {
        return Err("usage: metroid-retention-replay REQUEST OUT".into());
    };
    let request_bytes = read(Path::new(request))?;
    let q: Request = serde_json::from_slice(&request_bytes)?;
    if !(1..=300).contains(&q.wall_seconds) {
        return Err("wall limit must be 1..300 seconds".into());
    }
    let limit = q.wall_seconds;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(limit));
        std::process::exit(124);
    });
    let output = Path::new(output);
    fs::create_dir(output)?;
    let started = telemetry_now();
    let executable_sha256 = sha(&fs::read(std::env::current_exe()?)?);
    let mut cost = Cost::default();
    let result = evaluate(&q, output, &executable_sha256, &mut cost);
    match result {
        Ok(mut report) => {
            report["request_sha256"] = sha(&request_bytes).into();
            report["elapsed_seconds"] = started.elapsed().as_secs_f64().into();
            write(&output.join("result.json"), &report)
        }
        Err(error) => {
            write(
                &output.join("failure.json"),
                &json!({"request_sha256":sha(&request_bytes),
                "executable_sha256":executable_sha256,"error":error.to_string(),
                "elapsed_seconds":started.elapsed().as_secs_f64(),"status":"incomplete",
                "cost":cost,
                "actual_frames":"Direct inspector/prefix counters are recorded. A failed constructor, generic replay's constructor and incomplete replay work remain unknown; on watchdog exit, use the last progress receipt and retain its subsequent-work gap."}),
            )?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Request, MetroidGame, Vec<u8>) {
        let pin = |value: char| Pinned {
            path: "unused".into(),
            sha256: value.to_string().repeat(64),
        };
        let q = Request {
            phase: Phase::Qualify,
            stream: pin('a'),
            origin: pin('b'),
            final_checkpoint: pin('9'),
            rom: pin('c'),
            core: pin('d'),
            original_core_sha256: "d".repeat(64),
            root_snapshot_sha256: "f".repeat(64),
            expected_root_context: json!({"fixture":true}),
            expected_jobs: 5,
            expected_frames: 50,
            expected_competitions: 1,
            physical_frame_ceiling: 550_000,
            wall_seconds: 2,
            qualification: None,
            reconstruction: None,
        };
        let game = MetroidGame::new(&[], Path::new("unused"), &q.core.sha256)
            .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
        let mut header = json!({"format":game.stream_format(),"campaign_seed":1,"workers":4,
            "schedule_policy":"deterministic_window_1_per_worker_v3",
            "host":"source-fixture","origin_kind":"snapshot_root","origin_path":null,
            "origin_archive_sha256":null,"origin_checkpoint_path":"fixture-origin",
            "origin_checkpoint_sha256":q.origin.sha256,"resume_input_sha256":sha(b"{\"actions\":[]}"),
            "resume_actions":0,"execution_budget":5000,"frame_budget":250000,
            "wall_budget_seconds":120,"action_limit":4096,"archive_entry_limit":4194304,
            "memory_budget_mib":512,"resume_policy":"snapshot_root","suffix_policy":"one_to_six",
            "mixture_policy":"alphabet_only","retention_policy":"admit_alive",
            "parent_scheduler":"room_cell_uniform_128_energy_progress_cheapest_v1:3,6,12,2",
            "executor_mode":"snapshot_resume_archive","worker_seed_derivation":"fixture",
            "rom_sha256":q.rom.sha256,"milestone_stop":{"name":"ridley_defeated",
                "observation_policy":"metroid-named-progress-v2"}});
        for (key, value) in game.policies(&MetroidCampaignRun) {
            header[&key] = value.into();
        }
        let mut bytes = serde_json::to_vec(&header).unwrap();
        bytes.push(b'\n');
        for sequence in 1..=5 {
            let job = json!({"event":"job","sequence":sequence,"worker":0,"parent_id":0,
                "mutation_seed":sequence,"frames":10,"result_sha256":"a".repeat(64),
                "decisions":[{"decision":"rejected"}],"mixture_weight":128,"splice_weight":0,
                "selector":{"path":"room_cell_uniform","classes_skipped":0,"counter_reset":false,
                    "concentration":{"window_size":1,"entered_window":1}}});
            // Whitespace deliberately differs from normal serialization.
            bytes.extend_from_slice(b"  ");
            bytes.extend(serde_json::to_vec(&job).unwrap());
            bytes.extend_from_slice(b" \n");
        }
        (q, game, bytes)
    }

    fn change_header(original: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
        let text = std::str::from_utf8(original).unwrap();
        let (head, body) = text.split_once('\n').unwrap();
        let mut value: Value = serde_json::from_str(head).unwrap();
        change(&mut value);
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        bytes.extend_from_slice(body.as_bytes());
        bytes
    }

    #[test]
    fn selection_preserves_exact_original_header_and_job_bytes() {
        let (mut q, game, original) = fixture();
        for phase in [Phase::Qualify, Phase::Inspect] {
            q.phase = phase;
            q.qualification = (phase == Phase::Inspect).then(|| Pinned {
                path: "unused".into(),
                sha256: "0".repeat(64),
            });
            let prepared = prepare(&q, &original, &game).unwrap();
            let (before, body) = std::str::from_utf8(&original)
                .unwrap()
                .split_once('\n')
                .unwrap();
            let (after, selected) = std::str::from_utf8(&prepared.stream)
                .unwrap()
                .split_once('\n')
                .unwrap();
            assert_eq!(before, after);
            let expected: String = body
                .split_inclusive('\n')
                .take(if phase == Phase::Qualify { 4 } else { 5 })
                .collect();
            assert_eq!(selected.as_bytes(), expected.as_bytes());
            assert_eq!(
                (prepared.jobs, prepared.frames),
                if phase == Phase::Qualify {
                    (4, 40)
                } else {
                    (5, 50)
                }
            );
            assert_eq!(prepared.complete_body_sha256, sha(body.as_bytes()));
        }
    }

    #[test]
    fn actual_rr01_cross_build_request_fails_before_reading_a_stream_or_creating_a_target() {
        let q: Request = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/rr01-qualify-request.json"
        ))
        .unwrap();
        let game = MetroidGame::new(&[], Path::new("unused"), &q.core.sha256);
        let Err(error) = prepare(&q, b"", &game) else {
            panic!("the frozen incompatible request must fail before stream parsing");
        };
        assert_eq!(
            error.to_string(),
            "raw snapshot replay requires the original QuickNES core build"
        );
    }

    #[test]
    fn incompatible_header_origin_and_source_policy_fail_before_a_target_exists() {
        let (q, game, original) = fixture();
        for (field, value) in [
            ("terminal_policy", json!("wrong-terminal")),
            ("origin_checkpoint_sha256", json!("wrong")),
            ("origin_checkpoint_path", Value::Null),
            ("parent_scheduler", json!("wrong-selector")),
            ("emulator_backend", json!("wrong-build")),
            ("action_limit", json!(8192)),
            ("resume_input_sha256", json!("wrong-input")),
            ("slot_retention", json!("resource_extremes_2_v0")),
        ] {
            assert!(
                prepare(&q, &change_header(&original, |v| v[field] = value), &game).is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn actual_searched_prefix_is_bounded_and_reconstruction_preserves_job_bytes() {
        let input = include_bytes!(
            "../../../../benchmarks/search/endpoint-encounter/first-encounter-input.json"
        );
        let reconstruction = Reconstruction {
            input: Pinned {
                path: "unused".into(),
                sha256: sha(input),
            },
            expected_actions: 3_314,
            expected_frames: 117_875,
        };
        assert_eq!(
            reconstruction.input.sha256,
            "5126eee64820c99697194dec63a20c44491ec7f913d49e77e4fefb97f4f319c5"
        );
        decode_reconstruction(&reconstruction, input).unwrap();
        let mut bad = reconstruction.clone();
        bad.expected_frames += 1;
        assert!(decode_reconstruction(&bad, input).is_err());
        bad = reconstruction.clone();
        bad.expected_actions -= 1;
        assert!(decode_reconstruction(&bad, input).is_err());
        assert!(decode_reconstruction(&reconstruction, &input[..input.len() - 1]).is_err());

        let (mut q, game, original) = fixture();
        q.original_core_sha256 = "e".repeat(64);
        q.reconstruction = Some(reconstruction.clone());
        assert!(prepare(&q, &original, &game).is_err());
        let original = change_header(&original, |h| {
            h["origin_checkpoint_path"] = json!(format!(
                "diagnostic-prefix-sha256:{}",
                reconstruction.input.sha256
            ));
            h["emulator_backend"] = json!(
                h["emulator_backend"]
                    .as_str()
                    .unwrap()
                    .replace(&q.core.sha256, &q.original_core_sha256)
            );
        });
        // The old ceiling did not reserve reconstruction work.
        assert!(prepare(&q, &original, &game).is_err());
        q.physical_frame_ceiling = 650_000;
        let prepared = prepare(&q, &original, &game).unwrap();
        let generated_origin_sha = "0".repeat(64);
        let derived = derive_runtime_stream(&prepared, true, &generated_origin_sha).unwrap();
        let (header, selected) = std::str::from_utf8(&derived)
            .unwrap()
            .split_once('\n')
            .unwrap();
        let after: Value = serde_json::from_str(header).unwrap();
        let (header, body) = std::str::from_utf8(&original)
            .unwrap()
            .split_once('\n')
            .unwrap();
        let mut before: Value = serde_json::from_str(header).unwrap();
        assert_ne!(before["emulator_backend"], after["emulator_backend"]);
        assert_ne!(
            before["origin_checkpoint_sha256"],
            after["origin_checkpoint_sha256"]
        );
        before["emulator_backend"] = json!(prepared.runtime_backend);
        before["origin_checkpoint_sha256"] = json!(generated_origin_sha);
        assert_eq!(before, after);
        assert_eq!(
            selected,
            body.split_inclusive('\n').take(4).collect::<String>()
        );
    }

    #[cfg(all(
        feature = "metroid-motion-context",
        feature = "metroid-boss-context-audit"
    ))]
    #[test]
    fn actual_saved_ap01_header_requires_its_exact_policy_before_reconstruction() {
        let mut q: Request = serde_json::from_str(include_str!(
            "../../../../benchmarks/search/continuation-reassessment/rr01-qualify-request.json"
        ))
        .unwrap();
        let prefix = include_bytes!(
            "../../../../benchmarks/search/endpoint-encounter/first-encounter-input.json"
        );
        q.reconstruction = Some(Reconstruction {
            input: Pinned {
                path: "unused".into(),
                sha256: sha(prefix),
            },
            expected_actions: 3_314,
            expected_frames: 117_875,
        });
        q.expected_jobs = 4;
        q.expected_frames = 865;
        q.physical_frame_ceiling = 650_000;
        let saved = include_bytes!(
            "../../../../benchmarks/search/continuation-reassessment/rr01-output/qualify/derived-stream.jsonl"
        );
        // Undo only RR01's documented header derivation; retain its actual job bytes.
        let original = change_header(saved, |h| {
            h["emulator_backend"] = json!(
                h["emulator_backend"]
                    .as_str()
                    .unwrap()
                    .replace(&q.core.sha256, &q.original_core_sha256)
            );
        });
        let game = MetroidGame::new(&[], Path::new("unused"), &q.core.sha256)
            .with_terminal_policy(MetroidTerminalPolicy::BcdUnderflow);
        if cfg!(feature = "metroid-retention-progress") {
            assert!(prepare(&q, &original, &game).is_err());
            return;
        }
        let prepared = prepare(&q, &original, &game).unwrap();
        assert_eq!((prepared.jobs, prepared.frames), (4, 865));
    }

    #[test]
    fn truncated_or_reordered_records_and_insufficient_bounds_are_rejected() {
        let (q, game, original) = fixture();
        assert!(prepare(&q, &original[..original.len() - 1], &game).is_err());
        let altered = std::str::from_utf8(&original)
            .unwrap()
            .replace("\"sequence\":2", "\"sequence\":3");
        assert_ne!(altered.as_bytes(), original);
        assert!(prepare(&q, altered.as_bytes(), &game).is_err());
        let mut short = q.clone();
        short.expected_jobs = 4;
        assert!(prepare(&short, &original, &game).is_err());
        short = q.clone();
        short.physical_frame_ceiling = 40 + 2 * SETUP;
        assert!(prepare(&short, &original, &game).is_err());
        short = q.clone();
        short.phase = Phase::Inspect;
        assert!(prepare(&short, &original, &game).is_err());
    }

    #[test]
    fn qualification_must_match_executable_inputs_context_and_completed_prefix() {
        let binding = json!({"executable_sha256":"build-A","original_stream_sha256":"stream-A"});
        let context = json!({"fixture":true});
        let report = json!({"format":"metroid-retention-replay-v2","phase":"qualify",
            "binding":binding,"root":{"context":context},"verified_jobs":4,"verified_job_frames":40,
            "status":"complete"});
        verify_qualification(&report, &binding, &context, 40).unwrap();
        for field in [
            "phase",
            "binding",
            "root",
            "verified_jobs",
            "verified_job_frames",
            "status",
        ] {
            let mut bad = report.clone();
            bad[field] = Value::Null;
            assert!(
                verify_qualification(&bad, &binding, &context, 40).is_err(),
                "{field}"
            );
        }
        let other = json!({"executable_sha256":"build-B","original_stream_sha256":"stream-A"});
        assert!(verify_qualification(&report, &other, &context, 40).is_err());
    }
}
