// SPDX-License-Identifier: AGPL-3.0-or-later

//! M6 absolute schedule-finding measurement over instrumented Go/Rust payloads.

use std::{
    collections::BTreeMap,
    env,
    error::Error,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

use hypercall_proto::{
    MAX_FRAME, SDK_COVERAGE_QUANTUM, SDK_COVERAGE_REQUEST_LEN, ServiceId, Status, decode,
    encode_response,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const RESULT_EVENT: u32 = 0x0600_0001;
const WRONG_SCHEDULE: [u32; 3] = [0, 0, 0];
const PLAN: &str = include_str!("../../../../harmony-linux/concurrency-suite/m6-plan.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    format: String,
    rust_lost_update: SeededPlan,
    go_publish_before_init: HeldOutPlan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SeededPlan {
    mode: String,
    budget: u64,
    seed: u64,
    max_choices: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HeldOutPlan {
    mode: String,
    budget: u64,
    seed: u64,
    max_choices: usize,
}

#[derive(Debug, Serialize)]
struct CoverageRecord {
    thread: u32,
    observed: u64,
    ready: u32,
    selected: u32,
}

#[derive(Debug)]
struct RunOutcome {
    bug: bool,
    value: u8,
    choices: Vec<u32>,
    coverage: Vec<CoverageRecord>,
    transcript_sha256: String,
}

#[derive(Debug, Serialize)]
struct BugReport {
    id: &'static str,
    language: &'static str,
    mode: &'static str,
    budget: u64,
    attempts: u64,
    seed: u64,
    wrong_schedule: Vec<u32>,
    wrong_schedule_reproduced: bool,
    reproducer_schedule: Vec<u32>,
    deterministic_replay: bool,
    transcript_sha256: String,
    coverage_exits: usize,
    coverage: Vec<CoverageRecord>,
    result_value: u8,
}

#[derive(Debug, Serialize)]
struct SuiteReport {
    format: &'static str,
    held_out_seed: u64,
    bugs: Vec<BugReport>,
}

fn read_u32(input: &[u8], offset: usize) -> Result<u32, Box<dyn Error>> {
    let bytes: [u8; 4] = input
        .get(offset..offset.checked_add(4).ok_or("offset overflow")?)
        .ok_or("short u32")?
        .try_into()?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &[u8], offset: usize) -> Result<u64, Box<dyn Error>> {
    let bytes: [u8; 8] = input
        .get(offset..offset.checked_add(8).ok_or("offset overflow")?)
        .ok_or("short u64")?
        .try_into()?;
    Ok(u64::from_le_bytes(bytes))
}

fn write_response(
    child: &mut Child,
    response: &[u8],
    transcript: &mut Sha256,
) -> Result<(), Box<dyn Error>> {
    let len = u32::try_from(response.len())?;
    let stdin = child.stdin.as_mut().ok_or("child stdin unavailable")?;
    stdin.write_all(&len.to_le_bytes())?;
    stdin.write_all(response)?;
    stdin.flush()?;
    transcript.update(len.to_le_bytes());
    transcript.update(response);
    Ok(())
}

fn finish_child(mut child: Child) -> Result<(), Box<dyn Error>> {
    drop(child.stdin.take());
    let status = child.wait()?;
    if status.success() {
        return Ok(());
    }
    let mut stderr = String::new();
    if let Some(mut stream) = child.stderr.take() {
        stream.read_to_string(&mut stderr)?;
    }
    Err(format!("payload exited {status}: {}", stderr.trim()).into())
}

fn run_candidate(program: &Path, schedule: &[u32]) -> Result<RunOutcome, Box<dyn Error>> {
    let mut child = Command::new(program)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or("child stdout unavailable")?;
    let mut thresholds = BTreeMap::<u32, u64>::new();
    let mut choice_cursor = 0_usize;
    let mut choices = Vec::new();
    let mut coverage = Vec::new();
    let mut transcript = Sha256::new();
    let (bug, value) = loop {
        let mut len = [0_u8; 4];
        stdout.read_exact(&mut len)?;
        let len_u32 = u32::from_le_bytes(len);
        let len = usize::try_from(len_u32)?;
        if len > MAX_FRAME {
            return Err("payload frame exceeds protocol maximum".into());
        }
        let mut request = vec![0_u8; len];
        stdout.read_exact(&mut request)?;
        transcript.update(len_u32.to_le_bytes());
        transcript.update(&request);
        let (header, payload) = decode(&request)?;
        if !header.is_request() {
            return Err("payload sent a response frame".into());
        }

        let mut response = [0_u8; MAX_FRAME];
        if header.service == ServiceId::Sdk as u16 && header.opcode == 2 {
            if payload.len() != SDK_COVERAGE_REQUEST_LEN {
                return Err("coverage request has wrong length".into());
            }
            let thread = read_u32(payload, 0)?;
            let observed = read_u64(payload, 4)?;
            let ready = read_u32(payload, 12)?;
            let expected = thresholds
                .get(&thread)
                .copied()
                .unwrap_or(SDK_COVERAGE_QUANTUM);
            if ready == 0 || observed != expected {
                return Err(format!(
                    "coverage threshold mismatch for thread {thread}: expected {expected}, got {observed}"
                )
                .into());
            }
            let next = observed
                .checked_add(SDK_COVERAGE_QUANTUM)
                .ok_or("coverage threshold overflow")?;
            let selected = if ready == 1 {
                0
            } else {
                let selected = *schedule
                    .get(choice_cursor)
                    .ok_or("candidate schedule exhausted at a real choice")?;
                choice_cursor += 1;
                choices.push(selected);
                selected
            };
            if selected >= ready {
                return Err(
                    format!("candidate selected {selected} with only {ready} ready").into(),
                );
            }
            thresholds.insert(thread, next);
            coverage.push(CoverageRecord {
                thread,
                observed,
                ready,
                selected,
            });
            let mut answer = [0_u8; 12];
            answer[0..8].copy_from_slice(&next.to_le_bytes());
            answer[8..12].copy_from_slice(&selected.to_le_bytes());
            let response_len = encode_response(
                ServiceId::Sdk,
                2,
                header.seq,
                Status::Ok,
                &answer,
                &mut response,
            )?;
            write_response(&mut child, &response[..response_len], &mut transcript)?;
            continue;
        }
        if header.service == ServiceId::Event as u16 && header.opcode == 1 {
            if payload.len() != 6 || read_u32(payload, 0)? != RESULT_EVENT {
                return Err("payload emitted an unknown result event".into());
            }
            let response_len = encode_response(
                ServiceId::Event,
                1,
                header.seq,
                Status::Ok,
                &[],
                &mut response,
            )?;
            write_response(&mut child, &response[..response_len], &mut transcript)?;
            break (payload[4] != 0, payload[5]);
        }
        return Err(format!(
            "payload sent unsupported service/opcode {}/{}",
            header.service, header.opcode
        )
        .into());
    };
    finish_child(child)?;
    Ok(RunOutcome {
        bug,
        value,
        choices,
        coverage,
        transcript_sha256: format!("{:x}", transcript.finalize()),
    })
}

fn schedule_for_word(word: u64, max_choices: usize) -> Vec<u32> {
    (0..max_choices)
        .map(|shift| ((word >> shift) & 1) as u32)
        .collect()
}

fn deterministic_replay(program: &Path, schedule: &[u32]) -> Result<RunOutcome, Box<dyn Error>> {
    let first = run_candidate(program, schedule)?;
    let second = run_candidate(program, schedule)?;
    if !first.bug || !second.bug {
        return Err("reproducer did not trigger the bug twice".into());
    }
    if first.transcript_sha256 != second.transcript_sha256
        || first.value != second.value
        || first.choices != second.choices
    {
        return Err("same schedule produced a different transcript".into());
    }
    Ok(first)
}

fn wrong_schedule_negative(program: &Path) -> Result<RunOutcome, Box<dyn Error>> {
    let outcome = run_candidate(program, &WRONG_SCHEDULE)?;
    if outcome.bug {
        return Err("wrong schedule reproduced the bug".into());
    }
    Ok(outcome)
}

fn seeded_rust(program: &Path, plan: &SeededPlan) -> Result<BugReport, Box<dyn Error>> {
    let wrong = wrong_schedule_negative(program)?;
    let schedule = schedule_for_word(plan.seed, plan.max_choices);
    let reproduced = deterministic_replay(program, &schedule)?;
    Ok(BugReport {
        id: "rust_lost_update",
        language: "rust",
        mode: "seeded_reproducer",
        budget: plan.budget,
        attempts: 1,
        seed: plan.seed,
        wrong_schedule: wrong.choices,
        wrong_schedule_reproduced: false,
        reproducer_schedule: reproduced.choices,
        deterministic_replay: true,
        transcript_sha256: reproduced.transcript_sha256,
        coverage_exits: reproduced.coverage.len(),
        coverage: reproduced.coverage,
        result_value: reproduced.value,
    })
}

fn held_out_go(program: &Path, plan: &HeldOutPlan) -> Result<BugReport, Box<dyn Error>> {
    let wrong = wrong_schedule_negative(program)?;
    let mut found = None;
    let mut attempts = 0;
    for attempt in 0..plan.budget {
        attempts += 1;
        let word = attempt ^ (plan.seed & (plan.budget - 1));
        let schedule = schedule_for_word(word, plan.max_choices);
        let outcome = run_candidate(program, &schedule)?;
        if outcome.bug {
            found = Some(outcome.choices);
            break;
        }
    }
    let schedule = found.ok_or("held-out bug was not found within its declared budget")?;
    let reproduced = deterministic_replay(program, &schedule)?;
    Ok(BugReport {
        id: "go_publish_before_init",
        language: "go",
        mode: "held_out_discovery",
        budget: plan.budget,
        attempts,
        seed: plan.seed,
        wrong_schedule: wrong.choices,
        wrong_schedule_reproduced: false,
        reproducer_schedule: reproduced.choices,
        deterministic_replay: true,
        transcript_sha256: reproduced.transcript_sha256,
        coverage_exits: reproduced.coverage.len(),
        coverage: reproduced.coverage,
        result_value: reproduced.value,
    })
}

fn arguments() -> Result<(PathBuf, PathBuf, PathBuf), Box<dyn Error>> {
    let mut args = env::args_os().skip(1);
    let rust = args.next().ok_or("missing Rust payload path")?.into();
    let go = args.next().ok_or("missing Go payload path")?.into();
    let report = args.next().ok_or("missing report path")?.into();
    if args.next().is_some() {
        return Err("usage: m6-concurrency RUST_PAYLOAD GO_PAYLOAD REPORT_JSON".into());
    }
    Ok((rust, go, report))
}

fn main() -> Result<(), Box<dyn Error>> {
    let (rust_program, go_program, report_path) = arguments()?;
    let plan: Plan = serde_json::from_str(PLAN)?;
    if plan.format != "consonance.m6-plan.v1"
        || plan.rust_lost_update.mode != "seeded_reproducer"
        || plan.go_publish_before_init.mode != "held_out_discovery"
        || plan.rust_lost_update.budget != 1
        || plan.rust_lost_update.max_choices == 0
        || plan.rust_lost_update.max_choices > 63
        || !plan.go_publish_before_init.budget.is_power_of_two()
        || plan.go_publish_before_init.budget == 0
        || plan.go_publish_before_init.max_choices == 0
        || plan.go_publish_before_init.max_choices > 63
    {
        return Err("invalid M6 predeclared plan".into());
    }
    let rust = seeded_rust(&rust_program, &plan.rust_lost_update)?;
    let go = held_out_go(&go_program, &plan.go_publish_before_init)?;
    for result in [&rust, &go] {
        println!(
            "M6_BUG_OK id={} mode={} attempts={}/{} schedule={:?} coverage_exits={} transcript={}",
            result.id,
            result.mode,
            result.attempts,
            result.budget,
            result.reproducer_schedule,
            result.coverage_exits,
            result.transcript_sha256
        );
    }
    let report = SuiteReport {
        format: "consonance.m6-concurrency.v1",
        held_out_seed: plan.go_publish_before_init.seed,
        bugs: vec![rust, go],
    };
    fs::write(report_path, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}
