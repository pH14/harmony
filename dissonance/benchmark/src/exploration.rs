// SPDX-License-Identifier: AGPL-3.0-or-later
//! The fault-free exploration measurement (tasks 84/86): distinct cells and
//! depth at a fixed branch budget, per configuration, medians + IQR, the STADS
//! discovery curve for the signal configuration, and the strict
//! signal-beats-random pass predicate — rendered into the committed
//! `SMB-EXPLORATION-REPORT.md` (task 86 gate 3).
//!
//! This module extends the task-69 crate (it does **not** fork it): the order
//! statistics are [`crate::stats`]'s exact rationals, the species accounting is
//! [`explorer::stads`], and the ≥[`crate::report::MIN_SEEDS`]-seeds and
//! conflicting-trial disciplines carry over unchanged. The log shape differs
//! from the task-69 [`crate::report::CampaignLog`] deliberately: an exploration
//! campaign has no bug and no finds — its per-branch record is *(touched
//! cells, depth reached, terminal `state_hash`)*, task 84's discovery-event
//! log.
//!
//! Determinism discipline (rule 4): cells, depths, and budgets are integers;
//! medians/IQRs are exact rationals compared by cross-multiplication; floats
//! appear only in the rendered markdown; iteration is `BTreeSet`/`BTreeMap`
//! only.

use std::collections::{BTreeMap, BTreeSet};

use explorer::stads::{Frac, SpeciesAccumulator, SpeciesStats};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::report::MIN_SEEDS;
use crate::stats::{frac_f64, median, quartiles};

/// The three exploration configurations task 86 measures. (Task 84's
/// "frontier-off" diagnostic column is task 86's "Selector v1" attribution
/// column — the same third-configuration slot, named for what it runs here.)
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum ExplorationConfig {
    /// The subject under test: archive branching with the tested selector.
    Signal,
    /// The primary baseline and pass/fail line (task 84's ruling, inherited):
    /// independent seeds, no archive branching — random-restart search.
    PureRandom,
    /// The attribution column: the task-84-era default (v1) selector.
    /// Separates "the archive helps at all" from "the tested selector's
    /// improvements transfer". Not part of the pass condition.
    SelectorV1,
}

impl ExplorationConfig {
    /// The report-table label.
    pub fn label(&self) -> &'static str {
        match self {
            ExplorationConfig::Signal => "signal",
            ExplorationConfig::PureRandom => "pure-random baseline",
            ExplorationConfig::SelectorV1 => "selector v1 (attribution)",
        }
    }
}

/// One branch's discovery record — task 84's per-branch discovery-event log
/// entry, extended with the depth metric and the determinism witness.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DiscoveryEvent {
    /// The branch index within the campaign (0-based, dense).
    pub branch: u64,
    /// The distinct cell keys this branch touched (already keyed by the
    /// campaign's `CellFn`; opaque integers here).
    pub touched: Vec<u64>,
    /// The depth this branch reached (for SMB: the furthest `(world, level)`
    /// ordinal, `REG_DEPTH`).
    pub depth: u64,
    /// The branch's terminal `state_hash` (hex) — the determinism witness the
    /// box gate compares 25/25; carried in the log so a report is auditable
    /// against the replay transcript.
    pub state_hash: String,
}

/// One campaign's discovery-event log: one `(config, seed)` run.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ExplorationLog {
    /// The workload this campaign ran (e.g. `"smb"`); reports refuse to mix
    /// workloads.
    pub workload: String,
    /// The configuration.
    pub config: ExplorationConfig,
    /// The campaign seed.
    pub seed: u64,
    /// Per-branch discovery events, in branch order.
    pub events: Vec<DiscoveryEvent>,
}

impl ExplorationLog {
    /// Distinct cells discovered within the first `budget` branches.
    pub fn distinct_cells_at(&self, budget: u64) -> u64 {
        let mut cells = BTreeSet::new();
        for e in self.events.iter().filter(|e| e.branch < budget) {
            cells.extend(e.touched.iter().copied());
        }
        cells.len() as u64
    }

    /// The maximum depth reached within the first `budget` branches.
    pub fn depth_at(&self, budget: u64) -> u64 {
        self.events
            .iter()
            .filter(|e| e.branch < budget)
            .map(|e| e.depth)
            .max()
            .unwrap_or(0)
    }
}

/// The SMB report configuration (task 86): the manifest parameters the
/// campaign ran with, recorded verbatim in the report so results are
/// comparable across runs of the same dump. Input shaping (alphabet, window,
/// bucket) is legitimately tunable; the game is not.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct GameManifest {
    /// The workload name (`"smb"`).
    pub workload: String,
    /// The user-supplied ROM's sha256; `None` means no ROM was provisioned —
    /// the report renders as a loud SKIP, never a green gate.
    pub rom_sha256: Option<String>,
    /// The weighted chord alphabet spec string (the play-agent's `--alphabet`).
    pub chord_alphabet: String,
    /// The input window `W` in frames.
    pub window: u32,
    /// The x-bucket width in pixels.
    pub x_bucket_px: u32,
    /// The fixed branch budget every configuration runs at (identical across
    /// configurations — task 84's ruling).
    pub branch_budget: u64,
}

impl GameManifest {
    /// The SMB manifest with the play-agent's default input shaping.
    pub fn smb(rom_sha256: Option<String>, branch_budget: u64) -> Self {
        GameManifest {
            workload: "smb".to_string(),
            rom_sha256,
            chord_alphabet:
                "RIGHT:56,RIGHT+B:56,RIGHT+A:48,RIGHT+A+B:48,A:16,LEFT:12,DOWN:12,NEUTRAL:8"
                    .to_string(),
            window: 12,
            x_bucket_px: 128,
            branch_budget,
        }
    }
}

/// The per-configuration summary: seeds, medians + quartiles for both
/// measures, and the raw per-seed samples (kept for the report's audit trail).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ConfigSummary {
    /// The configuration.
    pub config: ExplorationConfig,
    /// Independent seeds measured.
    pub seeds: u64,
    /// Per-seed distinct-cell counts at the budget (sorted).
    pub cells: Vec<u64>,
    /// Per-seed max depths at the budget (sorted).
    pub depths: Vec<u64>,
    /// Median distinct cells.
    pub cells_median: Frac,
    /// Distinct-cell quartiles `(q1, q2, q3)`.
    pub cells_quartiles: (Frac, Frac, Frac),
    /// Median depth.
    pub depth_median: Frac,
    /// Depth quartiles `(q1, q2, q3)`.
    pub depth_quartiles: (Frac, Frac, Frac),
}

/// One strict comparison: greater median AND non-overlapping IQRs (the
/// signal's q1 strictly above the baseline's q3) — exact rational
/// cross-multiplication, never a float.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StrictBeats {
    /// `median(signal) > median(baseline)`.
    pub median_greater: bool,
    /// `q1(signal) > q3(baseline)` — the non-overlap condition.
    pub iqr_disjoint: bool,
}

impl StrictBeats {
    /// The combined strict-win condition.
    pub fn beats(&self) -> bool {
        self.median_greater && self.iqr_disjoint
    }
}

/// The pooled STADS instrumentation for the signal configuration.
#[derive(Clone, Debug)]
pub struct ExplorationStads {
    /// The pooled frequency-count snapshot.
    pub stats: SpeciesStats,
    /// The pooled species-accumulation curve (S_obs per branch sample).
    pub curve: Vec<u64>,
    /// The first pooled sample at which discovery probability fell below the
    /// stopping ε, if ever — the exhaustion signal at the budget.
    pub stop_at_sample: Option<u64>,
}

/// The verdict (task 86 gate 4).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    /// Signal strictly beats pure-random on BOTH distinct cells and depth,
    /// against a demonstrably live control.
    Pass,
    /// The full measurement ran and the signal did not strictly win — a
    /// publishable generalization finding, not a blocked task (the FAIL
    /// routing is the spec's: one documented cell-key retune, then escalate).
    Fail,
    /// The comparison could not be scored (a configuration missing — e.g. the
    /// M0 bring-up runs only baselines — or a dead control). The reason is
    /// rendered loudly; an incomplete report is never a green gate.
    Incomplete {
        /// Why the verdict could not be scored.
        reason: String,
    },
}

/// Why a report could not be computed.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum ExplorationError {
    /// No logs at all.
    #[error("no exploration logs")]
    NoLogs,
    /// Two logs for the same `(config, seed)` disagree — a determinism
    /// violation; never render a report over non-reproducible data.
    #[error("conflicting trials for {config:?} seed {seed} — same seed, different events")]
    ConflictingTrial {
        /// The configuration.
        config: ExplorationConfig,
        /// The seed.
        seed: u64,
    },
    /// A configuration ran fewer than the seed floor.
    #[error("{config:?} has {got} seeds, the floor is {need}")]
    TooFewSeeds {
        /// The configuration.
        config: ExplorationConfig,
        /// Seeds present.
        got: u64,
        /// The floor ([`MIN_SEEDS`]).
        need: u64,
    },
    /// Logs from more than one workload were mixed.
    #[error("mixed workloads in one report: {a:?} vs {b:?}")]
    MixedWorkloads {
        /// The first workload seen.
        a: String,
        /// The offending workload.
        b: String,
    },
}

/// The exploration report: the offline analysis of the campaign's
/// discovery-event logs, rendered to `SMB-EXPLORATION-REPORT.md`.
#[derive(Clone, Debug)]
pub struct ExplorationReport {
    /// The manifest the campaign ran with.
    pub manifest: GameManifest,
    /// Per-configuration summaries (in [`ExplorationConfig`] order).
    pub configs: Vec<ConfigSummary>,
    /// The strict signal-vs-pure-random comparison on distinct cells, when
    /// both configurations are present.
    pub cells_beats: Option<StrictBeats>,
    /// The strict comparison on depth.
    pub depth_beats: Option<StrictBeats>,
    /// Whether the pure-random control demonstrably still explores (non-zero
    /// median distinct cells).
    pub baseline_live: Option<bool>,
    /// The pooled STADS instrumentation for the signal configuration.
    pub stads: Option<ExplorationStads>,
    /// The verdict.
    pub verdict: Verdict,
}

impl ExplorationReport {
    /// Compute the report. `stop_eps = (num, den)` is the STADS stopping-rule
    /// ε. Fails loudly on conflicting same-seed trials, a violated seed floor,
    /// or mixed workloads.
    pub fn compute(
        manifest: &GameManifest,
        logs: &[ExplorationLog],
        stop_eps: (u64, u64),
    ) -> Result<ExplorationReport, ExplorationError> {
        if logs.is_empty() {
            return Err(ExplorationError::NoLogs);
        }
        for log in logs {
            if log.workload != logs[0].workload {
                return Err(ExplorationError::MixedWorkloads {
                    a: logs[0].workload.clone(),
                    b: log.workload.clone(),
                });
            }
        }

        // Deduplicate identical (config, seed) reruns; reject divergent ones
        // (the task-69 conflicting-trial discipline).
        let mut by_key: BTreeMap<(ExplorationConfig, u64), &ExplorationLog> = BTreeMap::new();
        for log in logs {
            match by_key.entry((log.config, log.seed)) {
                std::collections::btree_map::Entry::Vacant(v) => {
                    v.insert(log);
                }
                std::collections::btree_map::Entry::Occupied(o) => {
                    if o.get().events != log.events {
                        return Err(ExplorationError::ConflictingTrial {
                            config: log.config,
                            seed: log.seed,
                        });
                    }
                }
            }
        }

        let budget = manifest.branch_budget;
        let mut configs = Vec::new();
        for config in [
            ExplorationConfig::Signal,
            ExplorationConfig::PureRandom,
            ExplorationConfig::SelectorV1,
        ] {
            let runs: Vec<&&ExplorationLog> = by_key
                .iter()
                .filter(|((c, _), _)| *c == config)
                .map(|(_, l)| l)
                .collect();
            if runs.is_empty() {
                continue;
            }
            let seeds = runs.len() as u64;
            if seeds < MIN_SEEDS {
                return Err(ExplorationError::TooFewSeeds {
                    config,
                    got: seeds,
                    need: MIN_SEEDS,
                });
            }
            let mut cells: Vec<u64> = runs.iter().map(|l| l.distinct_cells_at(budget)).collect();
            let mut depths: Vec<u64> = runs.iter().map(|l| l.depth_at(budget)).collect();
            cells.sort_unstable();
            depths.sort_unstable();
            // ≥ MIN_SEEDS ≥ 2 samples, so median/quartiles are Some (the
            // expect is statically justified by the floor above).
            let cells_median = median(&cells).expect("seed floor guarantees samples");
            let cells_quartiles = quartiles(&cells).expect("seed floor guarantees samples");
            let depth_median = median(&depths).expect("seed floor guarantees samples");
            let depth_quartiles = quartiles(&depths).expect("seed floor guarantees samples");
            configs.push(ConfigSummary {
                config,
                seeds,
                cells,
                depths,
                cells_median,
                cells_quartiles,
                depth_median,
                depth_quartiles,
            });
        }

        let find = |c: ExplorationConfig| configs.iter().find(|s| s.config == c);
        let signal = find(ExplorationConfig::Signal);
        let baseline = find(ExplorationConfig::PureRandom);

        let strict = |s: &ConfigSummary, b: &ConfigSummary, cells: bool| -> StrictBeats {
            let (sm, sq, bm, bq) = if cells {
                (
                    s.cells_median,
                    s.cells_quartiles,
                    b.cells_median,
                    b.cells_quartiles,
                )
            } else {
                (
                    s.depth_median,
                    s.depth_quartiles,
                    b.depth_median,
                    b.depth_quartiles,
                )
            };
            StrictBeats {
                median_greater: sm > bm,
                iqr_disjoint: sq.0 > bq.2, // q1(signal) > q3(baseline)
            }
        };
        let cells_beats = signal.zip(baseline).map(|(s, b)| strict(s, b, true));
        let depth_beats = signal.zip(baseline).map(|(s, b)| strict(s, b, false));
        let baseline_live = baseline.map(|b| b.cells_median > Frac::whole(0));

        // Pooled STADS for the signal configuration: one branch = one sample,
        // logs folded in (config, seed) order, the running Good–Turing
        // stopping rule checked per sample (the task-69 shape).
        let stads = signal.map(|_| {
            let mut acc = SpeciesAccumulator::new();
            let mut stop_at_sample = None;
            let mut sample = 0u64;
            for ((c, _), log) in &by_key {
                if *c != ExplorationConfig::Signal {
                    continue;
                }
                for e in log.events.iter().filter(|e| e.branch < budget) {
                    acc.observe_branch(e.touched.iter().map(|cell| cell.to_be_bytes().to_vec()));
                    sample += 1;
                    if stop_at_sample.is_none()
                        && acc.stats().individuals > 0
                        && acc.stats().discovery_below(stop_eps.0, stop_eps.1)
                    {
                        stop_at_sample = Some(sample);
                    }
                }
            }
            ExplorationStads {
                stats: acc.stats(),
                curve: acc.curve().to_vec(),
                stop_at_sample,
            }
        });

        let verdict = match (signal, baseline, baseline_live) {
            (Some(_), Some(_), Some(false)) => Verdict::Incomplete {
                reason: "the pure-random control discovered no cells — a dead control cannot \
                         ground a win (check the workload wiring before scoring)"
                    .to_string(),
            },
            (Some(_), Some(_), Some(true)) => {
                let cb = cells_beats.expect("signal and baseline present");
                let db = depth_beats.expect("signal and baseline present");
                if cb.beats() && db.beats() {
                    Verdict::Pass
                } else {
                    Verdict::Fail
                }
            }
            _ => {
                let mut missing = Vec::new();
                if signal.is_none() {
                    missing.push("signal");
                }
                if baseline.is_none() {
                    missing.push("pure-random baseline");
                }
                Verdict::Incomplete {
                    reason: format!(
                        "missing configuration(s): {} — the verdict needs both sides",
                        missing.join(", ")
                    ),
                }
            }
        };

        Ok(ExplorationReport {
            manifest: manifest.clone(),
            configs,
            cells_beats,
            depth_beats,
            baseline_live,
            stads,
            verdict,
        })
    }

    /// Render the committed markdown report (floats here only — rendering).
    pub fn render_markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut md = String::new();
        let m = &self.manifest;
        let _ = writeln!(
            md,
            "# {} exploration report (task 86)",
            m.workload.to_uppercase()
        );
        md.push('\n');
        match &m.rom_sha256 {
            Some(sha) => {
                let _ = writeln!(md, "- ROM sha256: `{sha}`");
            }
            None => {
                let _ = writeln!(
                    md,
                    "- **ROM ABSENT — SKIP.** No user-supplied ROM was provisioned \
                     (`HARMONY_SMB_ROM` unset); a skipped gate is not a green gate."
                );
            }
        }
        let _ = writeln!(
            md,
            "- Faults **off** the whole time: `FaultPolicy::none()`, buggify off — the `quiet` arm."
        );
        let _ = writeln!(
            md,
            "- Branch budget (identical for every configuration): {}",
            m.branch_budget
        );
        let _ = writeln!(
            md,
            "- Input shaping: window {} frames, x-bucket {} px, alphabet `{}`",
            m.window, m.x_bucket_px, m.chord_alphabet
        );
        md.push('\n');

        let _ = writeln!(md, "## Configurations\n");
        let _ = writeln!(
            md,
            "| configuration | seeds | distinct cells (median) | cells IQR [q1, q3] | depth (median) | depth IQR [q1, q3] |"
        );
        let _ = writeln!(md, "|---|---|---|---|---|---|");
        for s in &self.configs {
            let _ = writeln!(
                md,
                "| {} | {} | {:.1} | [{:.1}, {:.1}] | {:.1} | [{:.1}, {:.1}] |",
                s.config.label(),
                s.seeds,
                frac_f64(s.cells_median),
                frac_f64(s.cells_quartiles.0),
                frac_f64(s.cells_quartiles.2),
                frac_f64(s.depth_median),
                frac_f64(s.depth_quartiles.0),
                frac_f64(s.depth_quartiles.2),
            );
        }
        md.push('\n');

        if let (Some(cb), Some(db)) = (self.cells_beats, self.depth_beats) {
            let _ = writeln!(md, "## Signal vs pure-random (the pass condition)\n");
            let _ = writeln!(
                md,
                "- distinct cells: median greater = **{}**, IQRs disjoint (q1 > q3) = **{}**",
                cb.median_greater, cb.iqr_disjoint
            );
            let _ = writeln!(
                md,
                "- depth: median greater = **{}**, IQRs disjoint (q1 > q3) = **{}**",
                db.median_greater, db.iqr_disjoint
            );
            let _ = writeln!(
                md,
                "- control demonstrably live: **{}**",
                self.baseline_live.unwrap_or(false)
            );
            md.push('\n');
        }

        if let Some(st) = &self.stads {
            let _ = writeln!(md, "## STADS (signal configuration, pooled)\n");
            let _ = writeln!(
                md,
                "- observed species: {}; Chao1 richness: {:.1}; end-of-fold discovery probability: {:.4}",
                st.curve.last().copied().unwrap_or(0),
                frac_f64(st.stats.chao1()),
                frac_f64(st.stats.discovery_probability()),
            );
            match st.stop_at_sample {
                Some(s) => {
                    let _ = writeln!(
                        md,
                        "- exhaustion: discovery fell below ε at pooled sample {s}"
                    );
                }
                None => {
                    let _ = writeln!(
                        md,
                        "- exhaustion: never below ε within the budget — discovery still live"
                    );
                }
            }
            // The accumulation curve, decimated to ≤32 points for the report.
            let step = (st.curve.len() / 32).max(1);
            let pts: Vec<String> = st
                .curve
                .iter()
                .enumerate()
                .filter(|(i, _)| i.is_multiple_of(step))
                .map(|(i, s)| format!("({i}, {s})"))
                .collect();
            let _ = writeln!(md, "- curve (sample, S_obs): {}", pts.join(" "));
            md.push('\n');
        }

        let _ = writeln!(md, "## Verdict\n");
        match &self.verdict {
            Verdict::Pass => {
                let _ = writeln!(
                    md,
                    "**PASS** — signal strictly beats pure-random on both distinct cells and \
                     depth (greater medians, non-overlapping IQRs) against a live control."
                );
            }
            Verdict::Fail => {
                let _ = writeln!(
                    md,
                    "**FAIL** — the signal configuration did not strictly beat pure-random on \
                     both measures. Routing (task 86 gate 4): one documented host-side cell-key \
                     retune; a persisting FAIL is a generalization finding about the selector — \
                     escalate this report to the integrator. Never a workload nerf."
                );
            }
            Verdict::Incomplete { reason } => {
                let _ = writeln!(md, "**INCOMPLETE** — {reason}.");
            }
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(branch: u64, touched: &[u64], depth: u64) -> DiscoveryEvent {
        DiscoveryEvent {
            branch,
            touched: touched.to_vec(),
            depth,
            state_hash: format!("{branch:064x}"),
        }
    }

    /// A log whose per-branch cells are `base + branch` (all distinct) and
    /// whose depth ramps to `max_depth`.
    fn ramp_log(
        config: ExplorationConfig,
        seed: u64,
        cells_per: u64,
        max_depth: u64,
    ) -> ExplorationLog {
        let events = (0..8u64)
            .map(|b| {
                let touched: Vec<u64> = (0..cells_per)
                    .map(|i| seed * 10_000 + b * 100 + i)
                    .collect();
                ev(b, &touched, (b * max_depth) / 8)
            })
            .collect();
        ExplorationLog {
            workload: "smb".to_string(),
            config,
            seed,
            events,
        }
    }

    fn manifest() -> GameManifest {
        GameManifest::smb(Some("f00d".to_string()), 8)
    }

    fn twenty(config: ExplorationConfig, cells_per: u64, max_depth: u64) -> Vec<ExplorationLog> {
        (0..MIN_SEEDS)
            .map(|s| ramp_log(config, s, cells_per, max_depth))
            .collect()
    }

    #[test]
    fn pass_when_signal_strictly_beats_a_live_baseline() {
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 3));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(report.cells_beats.unwrap().beats());
        assert!(report.depth_beats.unwrap().beats());
        assert_eq!(report.baseline_live, Some(true));
        let md = report.render_markdown();
        assert!(md.contains("PASS"));
        assert!(md.contains("f00d"));
        assert!(md.contains("quiet"));
    }

    #[test]
    fn fail_when_medians_tie_or_iqrs_overlap() {
        let mut logs = twenty(ExplorationConfig::Signal, 4, 6);
        logs.extend(twenty(ExplorationConfig::PureRandom, 4, 6));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.verdict, Verdict::Fail);
    }

    #[test]
    fn incomplete_without_both_sides_and_never_green() {
        let logs = twenty(ExplorationConfig::PureRandom, 4, 6);
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert!(matches!(report.verdict, Verdict::Incomplete { .. }));
        let md = report.render_markdown();
        assert!(md.contains("INCOMPLETE"));
    }

    #[test]
    fn dead_control_is_incomplete_not_pass() {
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        // A baseline that discovered nothing at all.
        logs.extend((0..MIN_SEEDS).map(|s| ExplorationLog {
            workload: "smb".to_string(),
            config: ExplorationConfig::PureRandom,
            seed: s,
            events: (0..8).map(|b| ev(b, &[], 0)).collect(),
        }));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert!(matches!(report.verdict, Verdict::Incomplete { .. }));
    }

    #[test]
    fn seed_floor_is_enforced_per_configuration() {
        let logs: Vec<ExplorationLog> = (0..MIN_SEEDS - 1)
            .map(|s| ramp_log(ExplorationConfig::PureRandom, s, 2, 3))
            .collect();
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::TooFewSeeds { got, .. }) if got == MIN_SEEDS - 1
        ));
    }

    #[test]
    fn conflicting_same_seed_trials_are_rejected() {
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        let mut divergent = logs[0].clone();
        divergent.events[0].depth += 1;
        logs.push(divergent);
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::ConflictingTrial { seed: 0, .. })
        ));
    }

    #[test]
    fn identical_duplicate_trials_collapse() {
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs.push(logs[0].clone());
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.configs[0].seeds, MIN_SEEDS);
    }

    #[test]
    fn mixed_workloads_are_rejected() {
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs[3].workload = "metroid".to_string();
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::MixedWorkloads { .. })
        ));
    }

    #[test]
    fn rom_less_manifest_renders_a_loud_skip() {
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 3));
        let m = GameManifest::smb(None, 8);
        let md = ExplorationReport::compute(&m, &logs, (1, 1000))
            .unwrap()
            .render_markdown();
        assert!(md.contains("ROM ABSENT — SKIP"));
    }

    #[test]
    fn budget_truncates_both_measures() {
        let log = ramp_log(ExplorationConfig::Signal, 1, 2, 8);
        assert_eq!(log.distinct_cells_at(8), 16);
        assert_eq!(log.distinct_cells_at(4), 8);
        assert!(log.depth_at(4) < log.depth_at(8));
        assert_eq!(log.depth_at(0), 0);
    }
}
