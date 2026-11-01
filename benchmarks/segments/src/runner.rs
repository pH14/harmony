// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
    thread,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::spec::Loaded;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arm {
    pub command: Vec<String>,
}

pub fn load_arms(path: &Path) -> Result<BTreeMap<String, Arm>, String> {
    let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let arms: BTreeMap<String, Arm> =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    for (name, arm) in &arms {
        if arm.command.is_empty() {
            return Err(format!("arm {name} has an empty command"));
        }
    }
    Ok(arms)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stop {
    Milestone,
    Budget,
    Memory,
    StartMismatch,
    RomMismatch,
    Error,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Outcome {
    pub stop: Stop,
    pub work: u64,
    #[serde(default)]
    pub job_stream_sha256: String,
    #[serde(default)]
    pub message: String,
}

pub fn read_outcome(dir: &Path) -> Option<Outcome> {
    serde_json::from_slice(&fs::read(dir.join("result.json")).ok()?).ok()
}

pub fn read_wall(dir: &Path) -> Option<f64> {
    let value: Value = serde_json::from_slice(&fs::read(dir.join("run.json")).ok()?).ok()?;
    value["wall_seconds"].as_f64()
}

pub struct Task {
    pub segment: String,
    pub arm: String,
    pub seed: u64,
    pub command: Vec<String>,
    pub request: Value,
    pub dir: PathBuf,
}

impl Task {
    pub fn new(
        loaded: &Loaded,
        arm: &str,
        command: &[String],
        seed: u64,
        workers: Option<u32>,
        dir: PathBuf,
    ) -> Result<Self, String> {
        Ok(Self {
            segment: loaded.spec.name.clone(),
            arm: arm.to_owned(),
            seed,
            command: command.to_vec(),
            request: loaded.spec.request(&loaded.tape, seed, workers)?,
            dir,
        })
    }

    fn label(&self) -> String {
        let run = self.dir.file_name().map_or_else(
            || format!("s{}", self.seed),
            |n| n.to_string_lossy().into_owned(),
        );
        format!("{} {} {run}", self.segment, self.arm)
    }

    pub fn done(&self) -> bool {
        let same_request = fs::read(self.dir.join("request.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|stored| stored == self.request);
        same_request && read_outcome(&self.dir).is_some_and(|o| o.stop != Stop::Error)
    }

    fn execute(&self) -> Result<Outcome, String> {
        fs::create_dir_all(&self.dir).map_err(|e| format!("{}: {e}", self.dir.display()))?;
        let result = self.dir.join("result.json");
        let _ = fs::remove_file(&result);
        let request = self.dir.join("request.json");
        let body = serde_json::to_vec_pretty(&self.request).map_err(|e| e.to_string())?;
        fs::write(&request, body).map_err(|e| e.to_string())?;
        let log = fs::File::create(self.dir.join("log")).map_err(|e| e.to_string())?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let started = wall_clock();
        let status = Command::new(&self.command[0])
            .args(&self.command[1..])
            .arg(&request)
            .arg(&result)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(err)
            .status();
        let wall = started.elapsed().as_secs_f64();
        let exit = match &status {
            Ok(status) => json!(status.code()),
            Err(error) => json!(error.to_string()),
        };
        fs::write(
            self.dir.join("run.json"),
            serde_json::to_vec_pretty(&json!({"wall_seconds": wall, "exit": exit}))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if let Some(outcome) = read_outcome(&self.dir) {
            return Ok(outcome);
        }
        let outcome = Outcome {
            stop: Stop::Error,
            work: 0,
            job_stream_sha256: String::new(),
            message: format!("adapter exited ({exit}) without a valid result.json"),
        };
        fs::write(
            &result,
            serde_json::to_vec_pretty(&outcome).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        Ok(outcome)
    }
}

#[allow(clippy::disallowed_methods)]
fn wall_clock() -> Instant {
    Instant::now()
}

fn say(line: &str) {
    let _ = writeln!(std::io::stdout().lock(), "{line}");
}

pub fn run_all(tasks: Vec<Task>, jobs: usize) -> Result<(usize, usize), String> {
    let (done, todo): (Vec<Task>, Vec<Task>) = tasks.into_iter().partition(Task::done);
    let skipped = done.len();
    let total = todo.len();
    say(&format!(
        "{} runs: {skipped} already have a result, {total} to run",
        skipped + total
    ));
    let queue = Mutex::new(todo.into_iter().rev().collect::<Vec<_>>());
    let errors = Mutex::new(Vec::new());
    thread::scope(|scope| {
        for _ in 0..jobs.max(1) {
            scope.spawn(|| {
                loop {
                    let next = queue
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pop();
                    let Some(task) = next else { break };
                    match task.execute() {
                        Ok(outcome) => say(&format!(
                            "{}: {:?} work {}{}",
                            task.label(),
                            outcome.stop,
                            outcome.work,
                            if outcome.message.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", outcome.message)
                            }
                        )),
                        Err(error) => errors
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(format!("{}: {error}", task.label())),
                    }
                }
            });
        }
    });
    let errors = errors
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if errors.is_empty() {
        Ok((skipped, total))
    } else {
        Err(errors.join("\n"))
    }
}

pub fn parse_seeds(text: &str) -> Result<Vec<u64>, String> {
    let mut seeds = Vec::new();
    for part in text.split(',') {
        let bad = || format!("bad seed list '{text}'");
        if let Some((low, high)) = part.split_once('-') {
            let low: u64 = low.trim().parse().map_err(|_| bad())?;
            let high: u64 = high.trim().parse().map_err(|_| bad())?;
            if high < low {
                return Err(bad());
            }
            seeds.extend(low..=high);
        } else {
            seeds.push(part.trim().parse().map_err(|_| bad())?);
        }
    }
    Ok(seeds)
}

pub fn run_dir(out: &Path, segment: &str, arm: &str, seed: u64) -> PathBuf {
    out.join(segment).join(arm).join(format!("s{seed}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_parse() {
        assert_eq!(parse_seeds("1-3,7").unwrap(), vec![1, 2, 3, 7]);
        assert!(parse_seeds("3-1").is_err());
        assert!(parse_seeds("x").is_err());
    }

    #[test]
    fn outcomes_parse() {
        let outcome: Outcome = serde_json::from_value(json!({
            "stop": "start_mismatch", "work": 0, "job_stream_sha256": "", "message": "state differs"
        }))
        .unwrap();
        assert_eq!(outcome.stop, Stop::StartMismatch);
        assert!(serde_json::from_value::<Outcome>(json!({"stop": "victory", "work": 1})).is_err());
    }
}
