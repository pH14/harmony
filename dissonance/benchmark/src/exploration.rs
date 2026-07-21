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
    ///
    /// The maze gate (task 134) runs this as its **subject**: the simple
    /// archive-guided selector under test, scored against the two permanent
    /// controls.
    SelectorV1,
    /// Task 84's ruled diagnostic control, permanent from the maze gate
    /// (task 134) on: the full snapshot/materialization machinery runs, but
    /// the selector never exploits an admitted Entry (always-explore). It
    /// separates "the archive machinery is behavior-neutral" from "novelty
    /// steering helps"; never part of a pass condition.
    FrontierOff,
}

impl ExplorationConfig {
    /// The report-table label.
    pub fn label(&self) -> &'static str {
        match self {
            ExplorationConfig::Signal => "signal",
            ExplorationConfig::PureRandom => "pure-random baseline",
            ExplorationConfig::SelectorV1 => "selector v1 (attribution)",
            ExplorationConfig::FrontierOff => "frontier-off (diagnostic)",
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
    /// The workload this campaign ran (e.g. `"smb"`); a report refuses logs
    /// whose workload is not the manifest's.
    pub workload: String,
    /// The sha256 of the ROM the campaign ran, when the driver knows it
    /// (`--rom-sha256`). A report refuses a log whose recorded dump differs
    /// from the manifest's — results are only comparable across runs of the
    /// same dump. `None` = unstamped (accepted; the manifest's hash stands).
    #[serde(default)]
    pub rom_sha256: Option<String>,
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
    /// The per-rollout v-time deadline (ns past the sealed base) every
    /// configuration ran with — part of the budget (round-9 P1): logs
    /// measured under different rollout durations are not comparable, so a
    /// `--deadline-delta` change across appends is manifest drift, exactly
    /// like a branch-budget or ROM change. `None` = the portable toy's
    /// natural-terminal rollouts (no v-time deadline).
    pub deadline_delta: Option<u64>,
}

impl GameManifest {
    /// The SMB manifest with the play-agent's default input shaping.
    pub fn smb(
        rom_sha256: Option<String>,
        branch_budget: u64,
        deadline_delta: Option<u64>,
    ) -> Self {
        GameManifest {
            workload: "smb".to_string(),
            rom_sha256,
            chord_alphabet:
                "RIGHT:56,RIGHT+B:56,RIGHT+A:48,RIGHT+A+B:48,A:16,LEFT:12,DOWN:12,NEUTRAL:8"
                    .to_string(),
            window: 12,
            x_bucket_px: 128,
            branch_budget,
            deadline_delta,
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
    /// A log's workload is not the manifest's — scoring one workload's logs
    /// under another workload's manifest would compare incomparable spaces.
    #[error("{config:?} seed {seed}: log is from workload {log:?}, the manifest is {manifest:?}")]
    WorkloadMismatch {
        /// The manifest's workload.
        manifest: String,
        /// The offending log's workload.
        log: String,
        /// The configuration.
        config: ExplorationConfig,
        /// The seed.
        seed: u64,
    },
    /// A log records a different ROM dump than the manifest — results are
    /// only comparable across runs of the same dump.
    #[error("{config:?} seed {seed}: log ran ROM {log:?}, the manifest records {manifest:?}")]
    RomMismatch {
        /// The manifest's ROM sha256.
        manifest: Option<String>,
        /// The offending log's recorded ROM sha256.
        log: Option<String>,
        /// The configuration.
        config: ExplorationConfig,
        /// The seed.
        seed: u64,
    },
    /// A log's branch sequence is not the dense `0..branch_budget` the
    /// manifest requires (truncated, padded, or gapped). Scoring a short log
    /// as-is would silently compare unequal budgets — loud error instead.
    #[error(
        "{config:?} seed {seed}: expected the dense branch sequence 0..{want}, got {got} events \
         (first mismatch at event {at})"
    )]
    BadBranchSequence {
        /// The configuration.
        config: ExplorationConfig,
        /// The seed.
        seed: u64,
        /// The manifest's branch budget.
        want: u64,
        /// The events actually present.
        got: u64,
        /// The first event index whose branch (or absence) mismatches.
        at: u64,
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
        let budget = manifest.branch_budget;
        for log in logs {
            // Every log must be from the MANIFEST's workload and ROM dump
            // (round-2 P2) — validating against logs[0] alone would let a
            // uniform-but-wrong log set score under another workload's
            // manifest.
            if log.workload != manifest.workload {
                return Err(ExplorationError::WorkloadMismatch {
                    manifest: manifest.workload.clone(),
                    log: log.workload.clone(),
                    config: log.config,
                    seed: log.seed,
                });
            }
            if log.rom_sha256.is_some() && log.rom_sha256 != manifest.rom_sha256 {
                return Err(ExplorationError::RomMismatch {
                    manifest: manifest.rom_sha256.clone(),
                    log: log.rom_sha256.clone(),
                    config: log.config,
                    seed: log.seed,
                });
            }
            dense_check(log, budget)?;
        }

        let by_key = dedupe_trials(logs)?;
        let mut configs = Vec::new();
        for config in [
            ExplorationConfig::Signal,
            ExplorationConfig::PureRandom,
            ExplorationConfig::SelectorV1,
            ExplorationConfig::FrontierOff,
        ] {
            configs.extend(summarize_config(&by_key, config, budget)?);
        }

        let find = |c: ExplorationConfig| configs.iter().find(|s| s.config == c);
        let signal = find(ExplorationConfig::Signal);
        let baseline = find(ExplorationConfig::PureRandom);

        let cells_beats = signal.zip(baseline).map(|(s, b)| strict_beats(s, b, true));
        let depth_beats = signal.zip(baseline).map(|(s, b)| strict_beats(s, b, false));
        // Round-9 P1: "live" means MOVEMENT — a guest frozen on its initial
        // gameplay cell still touches exactly one cell every branch, so the
        // bar is strictly more than one distinct cell at the median, not
        // merely nonzero.
        let baseline_live = baseline.map(|b| b.cells_median > Frac::whole(1));

        // Pooled STADS for the signal configuration: one branch = one sample,
        // logs folded in (config, seed) order, the running Good–Turing
        // stopping rule checked per sample (the task-69 shape).
        let stads =
            signal.map(|_| pooled_stads(&by_key, ExplorationConfig::Signal, budget, stop_eps));

        // The ROM gate comes BEFORE any win-condition evaluation (task 86's
        // "gates SKIP loudly" rule): a ROM-less run has no comparable game
        // workload, so no arrangement of logs — winning or otherwise — may
        // ever render Pass from it. The vacuous-green class.
        let verdict = if manifest.rom_sha256.is_none() {
            Verdict::Incomplete {
                reason: "no ROM was provisioned (HARMONY_SMB_ROM unset) — the run is a SKIP; \
                         a skipped gate is never a green gate"
                    .to_string(),
            }
        } else {
            Self::score(signal, baseline, baseline_live, cells_beats, depth_beats)
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

    /// Score the pass condition over the summarized configurations. Called
    /// only after the ROM gate above has passed — never entered on a SKIP.
    fn score(
        signal: Option<&ConfigSummary>,
        baseline: Option<&ConfigSummary>,
        baseline_live: Option<bool>,
        cells_beats: Option<StrictBeats>,
        depth_beats: Option<StrictBeats>,
    ) -> Verdict {
        match (signal, baseline, baseline_live) {
            (Some(_), Some(_), Some(false)) => Verdict::Incomplete {
                reason: "the pure-random control never moved beyond a single cell at the median \
                         — a dead or frozen control cannot ground a win (check the workload \
                         wiring before scoring)"
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
        }
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
        match m.deadline_delta {
            Some(d) => {
                let _ = writeln!(
                    md,
                    "- Rollout deadline (v-time ns past the sealed base): {d}"
                );
            }
            None => {
                let _ = writeln!(
                    md,
                    "- Rollout deadline: none (portable toy — natural terminals)"
                );
            }
        }
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

/// Every log must carry the dense `0..budget` branch sequence its manifest
/// requires: a truncated / padded / gapped log scored as-is would silently
/// compare unequal budgets — loud error instead. Shared by the SMB and maze
/// gate reports.
pub(crate) fn dense_check(log: &ExplorationLog, budget: u64) -> Result<(), ExplorationError> {
    let dense_mismatch = log
        .events
        .iter()
        .enumerate()
        .find(|(i, e)| e.branch != *i as u64)
        .map(|(i, _)| i as u64);
    if log.events.len() as u64 != budget || dense_mismatch.is_some() {
        return Err(ExplorationError::BadBranchSequence {
            config: log.config,
            seed: log.seed,
            want: budget,
            got: log.events.len() as u64,
            at: dense_mismatch.unwrap_or_else(|| (log.events.len() as u64).min(budget)),
        });
    }
    Ok(())
}

/// Deduplicate identical `(config, seed)` reruns; reject divergent ones (the
/// task-69 conflicting-trial discipline — never render a report over
/// non-reproducible data).
pub(crate) fn dedupe_trials(
    logs: &[ExplorationLog],
) -> Result<BTreeMap<(ExplorationConfig, u64), &ExplorationLog>, ExplorationError> {
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
    Ok(by_key)
}

/// Summarize one configuration's deduped logs at `budget`: `Ok(None)` when the
/// configuration is absent, the seed floor enforced when present.
pub(crate) fn summarize_config(
    by_key: &BTreeMap<(ExplorationConfig, u64), &ExplorationLog>,
    config: ExplorationConfig,
    budget: u64,
) -> Result<Option<ConfigSummary>, ExplorationError> {
    let runs: Vec<&&ExplorationLog> = by_key
        .iter()
        .filter(|((c, _), _)| *c == config)
        .map(|(_, l)| l)
        .collect();
    if runs.is_empty() {
        return Ok(None);
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
    // ≥ MIN_SEEDS ≥ 2 samples, so median/quartiles are Some (the expect is
    // statically justified by the floor above).
    let cells_median = median(&cells).expect("seed floor guarantees samples");
    let cells_quartiles = quartiles(&cells).expect("seed floor guarantees samples");
    let depth_median = median(&depths).expect("seed floor guarantees samples");
    let depth_quartiles = quartiles(&depths).expect("seed floor guarantees samples");
    Ok(Some(ConfigSummary {
        config,
        seeds,
        cells,
        depths,
        cells_median,
        cells_quartiles,
        depth_median,
        depth_quartiles,
    }))
}

/// One strict subject-vs-baseline comparison on cells (`true`) or depth
/// (`false`): greater median AND non-overlapping IQRs, exact rationals.
pub(crate) fn strict_beats(s: &ConfigSummary, b: &ConfigSummary, cells: bool) -> StrictBeats {
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
        iqr_disjoint: sq.0 > bq.2, // q1(subject) > q3(baseline)
    }
}

/// Pool one configuration's discovery events into the STADS instrumentation:
/// one branch = one sample, logs folded in `(config, seed)` order, the running
/// Good–Turing stopping rule checked per sample.
pub(crate) fn pooled_stads(
    by_key: &BTreeMap<(ExplorationConfig, u64), &ExplorationLog>,
    config: ExplorationConfig,
    budget: u64,
    stop_eps: (u64, u64),
) -> ExplorationStads {
    let mut acc = SpeciesAccumulator::new();
    let mut stop_at_sample = None;
    let mut sample = 0u64;
    for ((c, _), log) in by_key {
        if *c != config {
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
            rom_sha256: Some("f00d".to_string()),
            config,
            seed,
            events,
        }
    }

    fn manifest() -> GameManifest {
        GameManifest::smb(Some("f00d".to_string()), 8, None)
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
            rom_sha256: None, // unstamped logs are accepted
            config: ExplorationConfig::PureRandom,
            seed: s,
            events: (0..8).map(|b| ev(b, &[], 0)).collect(),
        }));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert!(matches!(report.verdict, Verdict::Incomplete { .. }));
    }

    /// Round-9 P1: a control frozen on its single initial gameplay cell
    /// (every branch touches exactly the same one cell, depth 0) is NOT a
    /// live control — "live" means movement beyond the initial cell, so the
    /// verdict is Incomplete, never a Pass grounded on a frozen guest.
    #[test]
    fn one_cell_frozen_control_is_incomplete_not_pass() {
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        logs.extend((0..MIN_SEEDS).map(|s| ExplorationLog {
            workload: "smb".to_string(),
            rom_sha256: None,
            config: ExplorationConfig::PureRandom,
            seed: s,
            // The frozen shape: the same single cell, every branch.
            events: (0..8).map(|b| ev(b, &[42], 0)).collect(),
        }));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.baseline_live, Some(false));
        assert!(matches!(report.verdict, Verdict::Incomplete { .. }));
        // Two distinct cells at the median is movement — live again.
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        logs.extend((0..MIN_SEEDS).map(|s| ExplorationLog {
            workload: "smb".to_string(),
            rom_sha256: None,
            config: ExplorationConfig::PureRandom,
            seed: s,
            events: (0..8).map(|b| ev(b, &[42, 43], 0)).collect(),
        }));
        let report = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.baseline_live, Some(true));
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

    /// Round-2 P2: logs are validated against the MANIFEST's workload — one
    /// stray log fails, and so does a uniform-but-wrong log set (which the old
    /// logs[0] cross-check could never catch).
    #[test]
    fn foreign_workload_logs_are_rejected_against_the_manifest() {
        // One stray log.
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs[3].workload = "metroid".to_string();
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::WorkloadMismatch { seed: 3, .. })
        ));

        // Uniformly wrong: every log from another workload entirely.
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        for log in &mut logs {
            log.workload = "metroid".to_string();
        }
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::WorkloadMismatch { .. })
        ));
    }

    /// Round-2 P2 (ROM half): a log stamped with a different dump than the
    /// manifest records is rejected — including a stamped log under a ROM-less
    /// manifest. Unstamped logs are accepted (the manifest's hash stands).
    #[test]
    fn rom_dump_mismatches_are_rejected() {
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs[7].rom_sha256 = Some("beef".to_string());
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::RomMismatch { seed: 7, .. })
        ));

        // A stamped log under a ROM-less manifest is inconsistent metadata.
        let logs = twenty(ExplorationConfig::PureRandom, 2, 3); // stamped "f00d"
        assert!(matches!(
            ExplorationReport::compute(&GameManifest::smb(None, 8, None), &logs, (1, 1000)),
            Err(ExplorationError::RomMismatch { .. })
        ));
    }

    /// The vacuous-green regression (round-1 P1): a ROM-less manifest must
    /// force the SKIP verdict BEFORE any win-condition evaluation — even over
    /// logs that would otherwise be a clean strict win, the verdict is never
    /// Pass (and never Fail either: nothing comparable ran).
    #[test]
    fn rom_less_manifest_can_never_pass_even_on_winning_logs() {
        // The exact log set that renders Pass under a ROM-carrying manifest…
        let mut logs = twenty(ExplorationConfig::Signal, 8, 12);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 3));
        let with_rom = ExplorationReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(
            with_rom.verdict,
            Verdict::Pass,
            "the win is real with a ROM"
        );

        // …must be a SKIP under a ROM-less one. (Unstamped logs, so the SKIP
        // verdict path is exercised — stamped logs under a ROM-less manifest
        // are the RomMismatch error, tested separately.)
        let m = GameManifest::smb(None, 8, None);
        for log in &mut logs {
            log.rom_sha256 = None;
        }
        let report = ExplorationReport::compute(&m, &logs, (1, 1000)).unwrap();
        match &report.verdict {
            Verdict::Incomplete { reason } => assert!(reason.contains("SKIP")),
            other => panic!("ROM-less verdict must be the SKIP Incomplete, got {other:?}"),
        }
        let md = report.render_markdown();
        assert!(md.contains("ROM ABSENT — SKIP"));
        assert!(md.contains("INCOMPLETE"));
        assert!(!md.contains("**PASS**"));
    }

    /// The unequal-budget regression (round-1 P2): a log shorter than the
    /// manifest's branch budget — or one with a gapped/non-dense branch
    /// sequence — is a loud error, never scored as-is.
    #[test]
    fn truncated_or_gapped_logs_are_rejected() {
        // Truncated: one seed's log lost its last branch.
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs[5].events.pop();
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::BadBranchSequence {
                seed: 5,
                want: 8,
                got: 7,
                ..
            })
        ));

        // Gapped: right length, but a branch index is missing from the middle.
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        logs[3].events[4].branch = 6;
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::BadBranchSequence { seed: 3, at: 4, .. })
        ));

        // Padded: one branch too many.
        let mut logs = twenty(ExplorationConfig::PureRandom, 2, 3);
        let extra = logs[0].events.last().unwrap().clone();
        logs[0].events.push(DiscoveryEvent { branch: 8, ..extra });
        assert!(matches!(
            ExplorationReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::BadBranchSequence {
                seed: 0,
                want: 8,
                got: 9,
                ..
            })
        ));
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
