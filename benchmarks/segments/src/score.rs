// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, fs, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    runner::{Outcome, Stop, read_outcome, read_wall},
    spec::Loaded,
    stats::{self, Observation},
};

pub fn observation(outcome: &Outcome, budget: u64) -> Option<Observation> {
    let budget_f = budget as f64;
    let work = outcome.work as f64;
    match outcome.stop {
        Stop::Milestone if outcome.work <= budget => Some(Observation {
            time: work,
            event: true,
        }),
        Stop::Milestone | Stop::Budget | Stop::Memory => Some(Observation {
            time: work.min(budget_f),
            event: false,
        }),
        Stop::StartMismatch | Stop::RomMismatch | Stop::Error => None,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub arm: String,
    pub seed: u64,
    pub stop: Stop,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub segment: String,
    pub game: String,
    pub rom_sha256: String,
    pub arm: String,
    pub seeds: usize,
    pub reached: usize,
    pub budget: u64,
    pub median: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub logrank_p: Option<f64>,
    pub wall_median_seconds: Option<f64>,
    #[serde(skip)]
    observations: Vec<Observation>,
    #[serde(skip)]
    interval: (f64, f64),
}

fn finite(value: f64) -> Option<f64> {
    value.is_finite().then_some(value)
}

fn bootstrap_seed(segment: &str) -> u64 {
    let digest = Sha256::digest(format!("segment-bench/{segment}"));
    u64::from_le_bytes(digest[..8].try_into().unwrap_or([0; 8]))
}

pub struct Collected {
    pub rows: Vec<Row>,
    pub failures: BTreeMap<String, Vec<Failure>>,
}

pub fn collect(specs: &[Loaded], out: &Path, control: &str) -> Result<Collected, String> {
    let mut rows = Vec::new();
    let mut failures: BTreeMap<String, Vec<Failure>> = BTreeMap::new();
    for loaded in specs {
        let spec = &loaded.spec;
        let segment_dir = out.join(&spec.name);
        let Ok(arms) = fs::read_dir(&segment_dir) else {
            continue;
        };
        let mut arm_names: Vec<String> = arms
            .filter_map(Result::ok)
            .filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        arm_names.sort_by_key(|name| (name != control, name.clone()));
        let mut segment_rows = Vec::new();
        for arm in arm_names {
            let mut observations = Vec::new();
            let mut walls = Vec::new();
            let Ok(seeds) = fs::read_dir(segment_dir.join(&arm)) else {
                continue;
            };
            let mut dirs: Vec<(u64, std::path::PathBuf)> = seeds
                .filter_map(Result::ok)
                .filter_map(|e| {
                    let name = e.file_name().into_string().ok()?;
                    Some((name.strip_prefix('s')?.parse().ok()?, e.path()))
                })
                .collect();
            dirs.sort();
            for (seed, dir) in dirs {
                let Some(outcome) = read_outcome(&dir) else {
                    continue;
                };
                match observation(&outcome, spec.work_budget) {
                    Some(obs) => {
                        observations.push(obs);
                        if let Some(wall) = read_wall(&dir) {
                            walls.push(wall);
                        }
                    }
                    None => failures
                        .entry(spec.name.clone())
                        .or_default()
                        .push(Failure {
                            arm: arm.clone(),
                            seed,
                            stop: outcome.stop,
                            message: outcome.message,
                        }),
                }
            }
            if observations.is_empty() {
                continue;
            }
            let interval = stats::bootstrap_interval(&observations, bootstrap_seed(&spec.name));
            segment_rows.push(Row {
                segment: spec.name.clone(),
                game: spec.game.clone(),
                rom_sha256: spec.rom_sha256.clone(),
                arm,
                seeds: observations.len(),
                reached: observations.iter().filter(|o| o.event).count(),
                budget: spec.work_budget,
                median: finite(stats::km_median(&observations)),
                low: finite(interval.0),
                high: finite(interval.1),
                logrank_p: None,
                wall_median_seconds: stats::median(&mut walls),
                observations,
                interval,
            });
        }
        if let Some(base) = segment_rows
            .iter()
            .find(|r| r.arm == control)
            .map(|r| r.observations.clone())
        {
            for row in segment_rows.iter_mut().filter(|r| r.arm != control) {
                row.logrank_p = Some(stats::logrank(&row.observations, &base).1);
            }
        }
        rows.extend(segment_rows);
    }
    Ok(Collected { rows, failures })
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Verdict {
    pub pass: bool,
    pub better: Vec<String>,
    pub worse: Vec<String>,
    pub games: Vec<String>,
}

pub fn verdicts(rows: &[Row], control: &str) -> BTreeMap<String, Verdict> {
    let base: BTreeMap<&str, &Row> = rows
        .iter()
        .filter(|r| r.arm == control)
        .map(|r| (r.segment.as_str(), r))
        .collect();
    let mut out: BTreeMap<String, Verdict> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.arm != control) {
        let Some(ctrl) = base.get(row.segment.as_str()) else {
            continue;
        };
        let verdict = out.entry(row.arm.clone()).or_default();
        if row.interval.1 < ctrl.interval.0 {
            verdict.better.push(row.segment.clone());
            if !verdict.games.contains(&row.rom_sha256) {
                verdict.games.push(row.rom_sha256.clone());
            }
        } else if row.interval.0 > ctrl.interval.1 {
            verdict.worse.push(row.segment.clone());
        }
    }
    for verdict in out.values_mut() {
        verdict.games.sort();
        verdict.pass = verdict.games.len() >= 2 && verdict.worse.is_empty();
    }
    out
}

fn frames(value: Option<f64>, budget: u64) -> String {
    match value {
        Some(v) => format!("{:.2}M", v / f64::from(1u32 << 20)),
        None => format!(">{:.2}M", budget as f64 / f64::from(1u32 << 20)),
    }
}

pub fn render(
    collected: &Collected,
    verdicts: &BTreeMap<String, Verdict>,
    control: &str,
) -> String {
    let mut out = vec![
        "| segment | game | arm | reached | KM median work | 95% interval | log-rank p | median wall s |".to_owned(),
        "|---|---|---|---|---|---|---|---|".to_owned(),
    ];
    for r in &collected.rows {
        out.push(format!(
            "| {} | {} | {} | {}/{} | {} | {} .. {} | {} | {} |",
            r.segment,
            r.game,
            r.arm,
            r.reached,
            r.seeds,
            frames(r.median, r.budget),
            frames(r.low, r.budget),
            frames(r.high, r.budget),
            r.logrank_p.map_or("-".to_owned(), |p| format!("{p:.3}")),
            r.wall_median_seconds
                .map_or("-".to_owned(), |w| format!("{w:.1}")),
        ));
    }
    out.push(String::new());
    out.push(format!("Work in Mi frames (2^20). Control arm: {control}."));
    for (arm, v) in verdicts {
        out.push(format!(
            "{arm}: {}. Better than {control} on {} ({} games); worse on {}.",
            if v.pass { "PASS" } else { "FAIL" },
            if v.better.is_empty() {
                "no segment".to_owned()
            } else {
                v.better.join(", ")
            },
            v.games.len(),
            if v.worse.is_empty() {
                "no segment".to_owned()
            } else {
                v.worse.join(", ")
            },
        ));
    }
    for (segment, list) in &collected.failures {
        for f in list {
            out.push(format!(
                "Failed run, left out: {segment} {} s{}: {:?} {}",
                f.arm, f.seed, f.stop, f.message
            ));
        }
    }
    out.join("\n")
}

#[derive(Clone, Debug, Serialize)]
pub struct Calibration {
    pub segment: String,
    pub seeds: usize,
    pub reached: usize,
    pub budget: u64,
    pub suggested_budget: Option<u64>,
    pub refit: bool,
}

pub fn calibrate(rows: &[Row], control: &str) -> Vec<Calibration> {
    rows.iter()
        .filter(|r| r.arm == control)
        .map(|r| Calibration {
            segment: r.segment.clone(),
            seeds: r.seeds,
            reached: r.reached,
            budget: r.budget,
            suggested_budget: r
                .median
                .map(|m| (m / f64::from(1u32 << 20)).ceil().max(1.0) as u64 * (1 << 20)),
            refit: r.reached == 0 || r.reached == r.seeds,
        })
        .collect()
}

pub fn render_calibration(list: &[Calibration]) -> String {
    let mut out = vec![
        "| segment | control reached | spec budget | suggested budget | refit |".to_owned(),
        "|---|---|---|---|---|".to_owned(),
    ];
    for c in list {
        out.push(format!(
            "| {} | {}/{} | {} | {} | {} |",
            c.segment,
            c.reached,
            c.seeds,
            c.budget,
            c.suggested_budget.map_or("-".to_owned(), |b| b.to_string()),
            if c.refit { "yes" } else { "no" },
        ));
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(stop: Stop, work: u64) -> Outcome {
        Outcome {
            stop,
            work,
            job_stream_sha256: String::new(),
            message: String::new(),
        }
    }

    #[test]
    fn results_above_the_budget_are_censored_at_the_budget() {
        assert_eq!(
            observation(&outcome(Stop::Milestone, 90), 100),
            Some(Observation {
                time: 90.0,
                event: true
            })
        );
        assert_eq!(
            observation(&outcome(Stop::Milestone, 100), 100),
            Some(Observation {
                time: 100.0,
                event: true
            })
        );
        assert_eq!(
            observation(&outcome(Stop::Milestone, 150), 100),
            Some(Observation {
                time: 100.0,
                event: false
            })
        );
        assert_eq!(
            observation(&outcome(Stop::Budget, 400), 100),
            Some(Observation {
                time: 100.0,
                event: false
            })
        );
        assert_eq!(
            observation(&outcome(Stop::Memory, 40), 100),
            Some(Observation {
                time: 40.0,
                event: false
            })
        );
        for stop in [Stop::StartMismatch, Stop::RomMismatch, Stop::Error] {
            assert_eq!(observation(&outcome(stop, 10), 100), None);
        }
    }

    fn row(segment: &str, rom: &str, arm: &str, interval: (f64, f64)) -> Row {
        Row {
            segment: segment.into(),
            game: rom.into(),
            rom_sha256: rom.into(),
            arm: arm.into(),
            seeds: 16,
            reached: 8,
            budget: 100,
            median: None,
            low: None,
            high: None,
            logrank_p: None,
            wall_median_seconds: None,
            observations: Vec::new(),
            interval,
        }
    }

    #[test]
    fn verdict_rule() {
        let rows = vec![
            row("a", "g1", "c", (40.0, 60.0)),
            row("b", "g2", "c", (40.0, 60.0)),
            row("d", "g1", "c", (40.0, 60.0)),
            row("a", "g1", "x", (10.0, 30.0)),
            row("b", "g2", "x", (10.0, 39.0)),
            row("d", "g1", "x", (30.0, 50.0)),
            row("a", "g1", "y", (10.0, 30.0)),
            row("d", "g1", "y", (10.0, 30.0)),
            row("a", "g1", "z", (10.0, 30.0)),
            row("b", "g2", "z", (10.0, 30.0)),
            row("d", "g1", "z", (61.0, f64::INFINITY)),
        ];
        let v = verdicts(&rows, "c");
        assert!(v["x"].pass);
        assert_eq!(v["x"].better, vec!["a", "b"]);
        assert!(!v["y"].pass, "two segments of one game");
        assert!(!v["z"].pass);
        assert_eq!(v["z"].worse, vec!["d"]);
    }

    #[test]
    fn calibration_rounds_up_to_whole_mebiframes() {
        let mut r = row("a", "g", "c", (0.0, 0.0));
        r.median = Some(1.5 * f64::from(1u32 << 20));
        let list = calibrate(&[r.clone()], "c");
        assert_eq!(list[0].suggested_budget, Some(2 << 20));
        assert!(!list[0].refit);
        r.reached = 16;
        assert!(calibrate(&[r], "c")[0].refit);
    }
}
