// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{collections::BTreeMap, fs, path::Path};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    runner::{Outcome, Stop, answers, read_outcome, read_request, read_wall},
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
        Stop::Milestone | Stop::Budget => Some(Observation {
            time: work.min(budget_f),
            event: false,
        }),
        Stop::Memory | Stop::StartMismatch | Stop::RomMismatch | Stop::Error => None,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Failure {
    pub arm: String,
    pub seed: u64,
    pub stop: Stop,
    pub message: String,
}

pub const SIGNIFICANCE: f64 = 0.05;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Faster,
    Slower,
    None,
}

impl Direction {
    fn of(result: &stats::LogRank) -> Self {
        if result.p >= SIGNIFICANCE {
            Self::None
        } else if result.observed_minus_expected > 0.0 {
            Self::Faster
        } else if result.observed_minus_expected < 0.0 {
            Self::Slower
        } else {
            Self::None
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Faster => "faster",
            Self::Slower => "slower",
            Self::None => "none",
        }
    }
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
    pub logrank_o_minus_e: Option<f64>,
    pub direction: Option<Direction>,
    pub wall_median_seconds: Option<f64>,
    pub memory_stops: usize,
    pub failed: usize,
    pub incomplete: bool,
    #[serde(skip)]
    observations: Vec<Observation>,
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
    pub stale: BTreeMap<String, BTreeMap<String, usize>>,
}

pub fn collect(specs: &[Loaded], out: &Path, control: &str) -> Result<Collected, String> {
    let mut rows = Vec::new();
    let mut failures: BTreeMap<String, Vec<Failure>> = BTreeMap::new();
    let mut stale: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
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
            let mut memory_stops = 0;
            let mut failed = 0;
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
                let request = spec.request(&loaded.tape, seed, None)?;
                if !read_request(&dir).is_some_and(|stored| answers(&stored, &outcome, &request)) {
                    *stale
                        .entry(spec.name.clone())
                        .or_default()
                        .entry(arm.clone())
                        .or_default() += 1;
                    continue;
                }
                if outcome.stop == Stop::Memory {
                    memory_stops += 1;
                }
                match observation(&outcome, spec.work_budget) {
                    Some(obs) => {
                        observations.push(obs);
                        if let Some(wall) = read_wall(&dir) {
                            walls.push(wall);
                        }
                    }
                    None => {
                        failed += 1;
                        failures
                            .entry(spec.name.clone())
                            .or_default()
                            .push(Failure {
                                arm: arm.clone(),
                                seed,
                                stop: outcome.stop,
                                message: outcome.message,
                            });
                    }
                }
            }
            if observations.is_empty() && failed == 0 {
                continue;
            }
            let (median, interval) = if observations.is_empty() {
                (None, (f64::INFINITY, f64::INFINITY))
            } else {
                (
                    finite(stats::km_median(&observations)),
                    stats::bootstrap_interval(&observations, bootstrap_seed(&spec.name)),
                )
            };
            segment_rows.push(Row {
                segment: spec.name.clone(),
                game: spec.game.clone(),
                rom_sha256: spec.rom_sha256.clone(),
                arm,
                seeds: observations.len(),
                reached: observations.iter().filter(|o| o.event).count(),
                budget: spec.work_budget,
                median,
                low: finite(interval.0),
                high: finite(interval.1),
                logrank_p: None,
                logrank_o_minus_e: None,
                direction: None,
                wall_median_seconds: stats::median(&mut walls),
                memory_stops,
                failed,
                incomplete: failed > 0,
                observations,
            });
        }
        if let Some((base, base_failed)) = segment_rows
            .iter()
            .find(|r| r.arm == control)
            .map(|r| (r.observations.clone(), r.failed))
        {
            for row in segment_rows.iter_mut().filter(|r| r.arm != control) {
                row.incomplete |= base_failed > 0;
                if row.incomplete {
                    continue;
                }
                let result = stats::logrank(&row.observations, &base);
                row.logrank_p = Some(result.p);
                row.logrank_o_minus_e = Some(result.observed_minus_expected);
                row.direction = Some(Direction::of(&result));
            }
        }
        rows.extend(segment_rows);
    }
    Ok(Collected {
        rows,
        failures,
        stale,
    })
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Verdict {
    pub pass: bool,
    pub better: Vec<String>,
    pub worse: Vec<String>,
    pub games: Vec<String>,
    pub incomplete: Vec<String>,
}

pub fn verdicts(rows: &[Row], control: &str) -> BTreeMap<String, Verdict> {
    let mut out: BTreeMap<String, Verdict> = BTreeMap::new();
    for row in rows.iter().filter(|r| r.arm != control) {
        if row.incomplete {
            out.entry(row.arm.clone())
                .or_default()
                .incomplete
                .push(row.segment.clone());
            continue;
        }
        let Some(direction) = row.direction else {
            continue;
        };
        let verdict = out.entry(row.arm.clone()).or_default();
        match direction {
            Direction::Faster => {
                verdict.better.push(row.segment.clone());
                if !verdict.games.contains(&row.rom_sha256) {
                    verdict.games.push(row.rom_sha256.clone());
                }
            }
            Direction::Slower => verdict.worse.push(row.segment.clone()),
            Direction::None => {}
        }
    }
    for verdict in out.values_mut() {
        verdict.games.sort();
        verdict.pass =
            verdict.games.len() >= 2 && verdict.worse.is_empty() && verdict.incomplete.is_empty();
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
        "| segment | game | arm | reached | KM median work | 95% interval | log-rank p | O-E | direction | median wall s |".to_owned(),
        "|---|---|---|---|---|---|---|---|---|---|".to_owned(),
    ];
    for r in &collected.rows {
        out.push(format!(
            "| {} | {} | {} | {}/{} | {} | {} .. {} | {} | {} | {} | {} |",
            r.segment,
            r.game,
            r.arm,
            r.reached,
            r.seeds,
            frames(r.median, r.budget),
            frames(r.low, r.budget),
            frames(r.high, r.budget),
            r.logrank_p.map_or("-".to_owned(), |p| format!("{p:.3}")),
            r.logrank_o_minus_e
                .map_or("-".to_owned(), |d| format!("{d:+.1}")),
            if r.incomplete {
                "incomplete"
            } else {
                r.direction.map_or("-", Direction::label)
            },
            r.wall_median_seconds
                .map_or("-".to_owned(), |w| format!("{w:.1}")),
        ));
    }
    out.push(String::new());
    out.push(format!(
        "Work in Mi frames (2^20). Control arm: {control}. Direction is the log-rank result at p < {SIGNIFICANCE}; the KM median and its interval are information only."
    ));
    for (arm, v) in verdicts {
        if !v.incomplete.is_empty() {
            out.push(format!(
                "{arm}: INCOMPLETE. Run the failed seeds again on {} before reading a verdict.",
                v.incomplete.join(", ")
            ));
            continue;
        }
        out.push(format!(
            "{arm}: {}. Faster than {control} on {} (games: {}); slower on {}.",
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
    let mut memory: BTreeMap<&str, usize> = BTreeMap::new();
    for r in &collected.rows {
        *memory.entry(r.segment.as_str()).or_default() += r.memory_stops;
    }
    for (segment, count) in memory.into_iter().filter(|(_, count)| *count > 0) {
        out.push(format!(
            "Memory stops on {segment}: {count}. Each one invalidates its seed: raise memory_limit in the spec and run every arm on the segment again."
        ));
    }
    for (segment, arms) in &collected.stale {
        for (arm, count) in arms {
            out.push(format!(
                "Stale results, left out: {segment} {arm}: {count} (stored request differs from the spec)"
            ));
        }
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
    pub median: Option<f64>,
    pub suggested_budget: Option<u64>,
    pub refit: bool,
    pub raise_budget: bool,
    pub memory_stops: usize,
    pub raise_memory_limit: bool,
}

pub const BUDGET_FACTOR: f64 = 3.0;

fn suggested_budget(median: f64) -> u64 {
    let unit = f64::from(1u32 << 20);
    (BUDGET_FACTOR * median / unit).ceil().max(1.0) as u64 * (1 << 20)
}

pub fn calibrate(rows: &[Row], control: &str) -> Vec<Calibration> {
    rows.iter()
        .filter(|r| r.arm == control)
        .map(|r| Calibration {
            segment: r.segment.clone(),
            seeds: r.seeds,
            reached: r.reached,
            budget: r.budget,
            median: r.median,
            suggested_budget: r.median.map(suggested_budget),
            refit: r.seeds > 0 && r.reached == 0,
            raise_budget: r.seeds > 0 && r.median.is_none(),
            memory_stops: r.memory_stops,
            raise_memory_limit: r.memory_stops > 0,
        })
        .collect()
}

pub fn render_calibration(list: &[Calibration]) -> String {
    let mut out = vec![
        "| segment | control reached | spec budget | KM median | suggested budget | flags |"
            .to_owned(),
        "|---|---|---|---|---|---|".to_owned(),
    ];
    for c in list {
        out.push(format!(
            "| {} | {}/{} | {} | {} | {} | {} |",
            c.segment,
            c.reached,
            c.seeds,
            c.budget,
            c.median.map_or("-".to_owned(), |m| format!("{m:.0}")),
            c.suggested_budget.map_or("-".to_owned(), |b| b.to_string()),
            flags(c),
        ));
    }
    out.push(String::new());
    out.push(format!(
        "Suggested budget is {BUDGET_FACTOR}x the control's KM median, rounded up to a multiple of 2^20."
    ));
    out.join("\n")
}

fn flags(c: &Calibration) -> String {
    let mut list = Vec::new();
    if c.refit {
        list.push("refit");
    }
    if c.raise_budget {
        list.push("raise budget");
    }
    if c.raise_memory_limit {
        list.push("raise memory_limit");
    }
    if list.is_empty() {
        "-".to_owned()
    } else {
        list.join(", ")
    }
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
        for stop in [
            Stop::Memory,
            Stop::StartMismatch,
            Stop::RomMismatch,
            Stop::Error,
        ] {
            assert_eq!(observation(&outcome(stop, 10), 100), None);
        }
    }

    fn row(segment: &str, rom: &str, arm: &str, direction: Option<Direction>) -> Row {
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
            logrank_o_minus_e: None,
            direction,
            wall_median_seconds: None,
            memory_stops: 0,
            failed: 0,
            incomplete: false,
            observations: Vec::new(),
        }
    }

    #[test]
    fn direction_needs_significance_and_takes_the_sign_of_observed_minus_expected() {
        let result = |p, observed_minus_expected| stats::LogRank {
            chi2: 0.0,
            p,
            observed_minus_expected,
        };
        assert_eq!(Direction::of(&result(0.01, 3.0)), Direction::Faster);
        assert_eq!(Direction::of(&result(0.01, -3.0)), Direction::Slower);
        assert_eq!(Direction::of(&result(0.05, 3.0)), Direction::None);
        assert_eq!(Direction::of(&result(0.2, -3.0)), Direction::None);
    }

    #[test]
    fn verdict_rule() {
        use Direction::{Faster, None as Same, Slower};
        let rows = vec![
            row("a", "g1", "c", None),
            row("b", "g2", "c", None),
            row("d", "g1", "c", None),
            row("a", "g1", "x", Some(Faster)),
            row("b", "g2", "x", Some(Faster)),
            row("d", "g1", "x", Some(Same)),
            row("a", "g1", "y", Some(Faster)),
            row("d", "g1", "y", Some(Faster)),
            row("a", "g1", "z", Some(Faster)),
            row("b", "g2", "z", Some(Faster)),
            row("d", "g1", "z", Some(Slower)),
            row("a", "g1", "w", Some(Faster)),
            row("b", "g2", "w", Some(Faster)),
            Row {
                incomplete: true,
                ..row("d", "g1", "w", None)
            },
        ];
        let v = verdicts(&rows, "c");
        assert!(v["x"].pass);
        assert_eq!(v["x"].better, vec!["a", "b"]);
        assert!(!v["y"].pass, "two segments of one game");
        assert!(!v["z"].pass);
        assert_eq!(v["z"].worse, vec!["d"]);
        assert!(!v["w"].pass, "a segment with failed seeds");
        assert_eq!(v["w"].incomplete, vec!["d"]);
    }

    #[test]
    fn calibration_suggests_three_times_the_median_in_whole_mebiframes() {
        let mut r = row("a", "g", "c", None);
        r.median = Some(1.5 * f64::from(1u32 << 20));
        let list = calibrate(std::slice::from_ref(&r), "c");
        assert_eq!(list[0].suggested_budget, Some(5 << 20));
        assert!(!list[0].refit);
        assert!(!list[0].raise_budget);
        r.median = None;
        let list = calibrate(std::slice::from_ref(&r), "c");
        assert_eq!(list[0].suggested_budget, None);
        assert!(list[0].raise_budget);
        assert!(!list[0].refit);
        r.reached = 0;
        let list = calibrate(std::slice::from_ref(&r), "c");
        assert!(list[0].refit);
        assert!(list[0].raise_budget);
        assert!(!list[0].raise_memory_limit);
        r.memory_stops = 1;
        assert!(calibrate(&[r], "c")[0].raise_memory_limit);
    }
}
