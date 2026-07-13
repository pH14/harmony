// SPDX-License-Identifier: AGPL-3.0-or-later
//! The correlation report: the four measures the spec mandates, computed from the
//! campaigns' discovery-event logs, and rendered to `CORRELATION-REPORT.md` with
//! an explicit **GO / NO-GO** ruling (mirrors task 63).
//!
//! The measures (spec §"The correlation harness"):
//! 1. **Novelty↔progress** — Spearman ρ between cells discovered at a fixed
//!    budget and time-to-bug, per bug: does a run that discovers more cells find
//!    bugs sooner? (The *right* direction is **negative**: more cells ⇒ less
//!    time.)
//! 2. **Trajectory** — does a finding run's ancestor chain pass through
//!    novel-cell admissions at an above-chance rate?
//! 3. **STADS** — species-accumulation curves (species = cells, samples =
//!    branches), Good–Turing discovery probability, Chao1 richness (via
//!    [`explorer::stads`]): was discovery still live when each bug fired, and how
//!    much is estimated left? Prototypes the stopping rule.
//! 4. **Baseline comparison** — median time-to-bug + IQR, signal vs baseline, per
//!    bug.
//!
//! Every decision (effect-size threshold, "not worse than baseline", the stopping
//! rule) is an exact integer/rational comparison; floats appear only in the
//! rendered prose.

use crate::manifest::{Benchmark, BugId};
use crate::stats::{RankCorr, frac_f64, iqr, median, spearman};
use explorer::stads::{Frac, SpeciesAccumulator, SpeciesStats};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// A report cannot be computed from inconsistent inputs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReportError {
    /// Two logs share a `(bug, config, seed)` trial but diverge in content — the
    /// **same seed produced different outcomes**, which is a determinism
    /// violation. This is surfaced loudly, never silently deduped: a benchmark
    /// whose trials are non-reproducible cannot yield a trustworthy GO/NO-GO.
    #[error(
        "conflicting trials for bug {bug:?} / {config:?} / seed {seed}: the same seed produced \
         divergent logs (a determinism violation — the campaign is non-reproducible)"
    )]
    ConflictingTrial {
        /// The bug attempted.
        bug: BugId,
        /// The configuration.
        config: Configuration,
        /// The seed whose two logs diverged.
        seed: u64,
    },
}

/// The Klees trial-discipline floor: a `Ruling::Go` requires at least this many
/// **independent** seeds per configuration, and this many distinct finding seeds
/// on any bug that counts toward the correlation. A GO/NO-GO gate must never rule
/// GO on undersampled/duplicated data — that is the exact failure this gate
/// exists to prevent — so the floor is a **hard precondition** of GO, not just a
/// rendered warning.
pub const MIN_SEEDS: u64 = 20;

/// Which of the two identical-budget configurations a campaign ran under.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum Configuration {
    /// The Phase-D signal stack (65 RunTraces → 67 sensors + CellFn v1 → 64
    /// Archive with the default v1 Selector → 68 materialization).
    Signal,
    /// Task 60's blind seed search.
    Baseline,
}

/// One branch's contribution to a campaign's discovery-event log: the branch
/// index and the **opaque** cell ids its run touched. Cumulative distinct cells
/// (the species-accumulation curve) is derived; the estimators fold the id
/// stream. Cell ids are opaque `u64`s — the report never interprets them
/// (search-loop-blind, invariant 5).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BranchEvent {
    /// The branch index within the campaign (0-based, monotone).
    pub branch: u64,
    /// The opaque cell ids this branch's run touched.
    pub touched: Vec<u64>,
}

/// The record the campaign emits when a bug fires: which bug, at what branch
/// (time-to-bug), and the finding run's ancestor-chain trajectory through
/// novel-cell admissions (measure 2).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FindRecord {
    /// The bug found.
    pub bug: BugId,
    /// The branch index at which it fired — the time-to-bug in branches.
    pub branch: u64,
    /// Length of the finding exemplar's ancestor chain (root → find).
    pub path_len: u64,
    /// How many links of that chain were novel-cell admissions.
    pub novel_on_path: u64,
}

/// One campaign's discovery-event log (one `(config, seed)` run). The
/// campaign driver (`consonance/vmm-core` / the conductor campaign bin) emits
/// these; the report analyses them offline.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CampaignLog {
    /// The bug this campaign **attempted** — recorded whether or not it was found,
    /// so the report can count attempts per `(bug, config)` and enforce the seed
    /// floor **per bug/config**, not globally (a no-find attempt for bug 2 must be
    /// distinguishable from a bug-1 log — round-3 gate integrity).
    pub bug: BugId,
    /// The configuration this campaign ran under.
    pub config: Configuration,
    /// The campaign seed (Klees-style: ≥20 distinct seeds per configuration).
    pub seed: u64,
    /// Per-branch discovery events, in branch order.
    pub events: Vec<BranchEvent>,
    /// Per-bug find records (a bug may be absent if this seed never found it).
    pub finds: Vec<FindRecord>,
    /// The signal config's **effective** explore period (every Nth step explores;
    /// the rest exploit). Recorded so the artifact is **self-describing** — a
    /// same-seed result must never depend on an ambient env var invisibly (PR#90
    /// round-2). `#[serde(default)]` = 4 for pre-record logs (the committed campaign
    /// ran the default 4). The PR#90 ablation records 1 here.
    #[serde(default = "default_explore_period")]
    pub explore_period: u64,
    /// The **effective** bug-2 (`OrderingInterrupt`) mint fault-offset search width.
    /// Same self-describing rule as `explore_period`; `#[serde(default)]` = 64 for
    /// pre-record logs. Irrelevant to bugs 1/3 (no `OrderingInterrupt` mint), kept
    /// so every campaign artifact fully pins its search knobs.
    #[serde(default = "default_order_range")]
    pub order_range: u64,
}

/// Pre-record default (the committed campaign's value) — see [`CampaignLog::explore_period`].
fn default_explore_period() -> u64 {
    4
}
/// Pre-record default (the committed campaign's value) — see [`CampaignLog::order_range`].
fn default_order_range() -> u64 {
    64
}

impl CampaignLog {
    /// Cumulative distinct cells discovered by branch `budget` (inclusive). If the
    /// run is shorter, its final distinct count.
    fn distinct_cells_at(&self, budget: u64) -> u64 {
        let mut seen = BTreeSet::new();
        for e in &self.events {
            if e.branch > budget {
                break;
            }
            for &c in &e.touched {
                seen.insert(c);
            }
        }
        seen.len() as u64
    }

    /// The branch a given bug fired at, if this seed found it.
    fn find_branch(&self, bug: BugId) -> Option<u64> {
        self.finds.iter().find(|f| f.bug == bug).map(|f| f.branch)
    }

    /// Fraction of branches that admitted at least one **novel** cell (a cell not
    /// seen in any earlier branch) — the campaign-wide base rate a trajectory is
    /// compared against. Returned as `(num, den)` = (novel-admitting branches,
    /// total branches).
    fn novel_admission_rate(&self) -> (u64, u64) {
        let mut seen = BTreeSet::new();
        let mut novel_branches = 0u64;
        for e in &self.events {
            let mut any_new = false;
            for &c in &e.touched {
                if seen.insert(c) {
                    any_new = true;
                }
            }
            if any_new {
                novel_branches += 1;
            }
        }
        (novel_branches, self.events.len() as u64)
    }
}

/// Measure 2 for one bug: the finding runs' aggregate trajectory through
/// novel-cell admissions vs the campaign-wide base rate.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TrajectoryMeasure {
    /// Total novel-admission links across all finding runs' ancestor chains.
    pub novel_on_path: u64,
    /// Total ancestor-chain length across all finding runs.
    pub path_total: u64,
    /// Base-rate numerator (novel-admitting branches, pooled).
    pub base_num: u64,
    /// Base-rate denominator (total branches, pooled).
    pub base_den: u64,
    /// Whether the on-path novel rate exceeds the base rate (exact
    /// cross-multiplication): `novel_on_path/path_total > base_num/base_den`.
    pub above_chance: bool,
}

/// The full per-bug measurement.
#[derive(Clone, Debug)]
pub struct BugMeasure {
    /// The bug.
    pub bug: BugId,
    /// Human name (report heading).
    pub name: String,
    /// Independent seeds ATTEMPTED for this bug under the signal config (the
    /// per-bug/config trial count the seed floor is enforced against).
    pub signal_attempts: u64,
    /// Independent seeds attempted for this bug under the baseline config.
    pub baseline_attempts: u64,
    /// Seeds (of the signal config) that found this bug — the paired-sample count
    /// for the novelty↔progress correlation.
    pub signal_finders: u64,
    /// Independent seeds (of the baseline config) that found this bug — a bug the
    /// baseline never found has no grounded comparison.
    pub baseline_finders: u64,
    /// Measure 1: novelty↔progress Spearman (signal config), if ≥2 finders.
    pub novelty_progress: Option<RankCorr>,
    /// Measure 2: trajectory (signal config).
    pub trajectory: TrajectoryMeasure,
    /// Measure 4: signal median time-to-bug + IQR (over finding seeds).
    pub signal_median: Option<Frac>,
    /// Signal IQR.
    pub signal_iqr: Option<Frac>,
    /// Baseline median time-to-bug + IQR.
    pub baseline_median: Option<Frac>,
    /// Baseline IQR.
    pub baseline_iqr: Option<Frac>,
    /// Whether the signal median is **not worse** than baseline (≤), exactly. A
    /// bug the signal found but baseline did not counts as not-worse; a bug
    /// neither found is vacuously not-worse.
    pub signal_not_worse: bool,
    /// Whether measure 1 shows the right direction with a meaningful effect size
    /// (ρ ≤ −effect_floor): novelty correlates with progress for this bug.
    pub correlates: bool,
}

/// Measure 3 for one configuration: the pooled STADS instrumentation.
#[derive(Clone, Debug)]
pub struct StadsMeasure {
    /// The configuration.
    pub config: Configuration,
    /// The pooled frequency-count snapshot.
    pub stats: SpeciesStats,
    /// Chao1 richness estimate.
    pub chao1: Frac,
    /// Good–Turing discovery probability at the end of the pooled fold.
    pub discovery: Frac,
    /// The pooled species-accumulation curve (S_obs per sample).
    pub curve: Vec<u64>,
    /// The first sample index at which pooled discovery probability fell below the
    /// stopping-rule ε, if ever — the prototype stopping point.
    pub stop_at_sample: Option<u64>,
}

/// The GO / NO-GO verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Ruling {
    /// Cell novelty correlates with bug progress on ≥2 of 3 bugs, right
    /// direction and meaningful effect size, and the signal median is not worse
    /// than baseline on any bug → Phase F / task 70 dispatches.
    Go,
    /// Correlation absent or inverted, or signal worse than baseline on some bug
    /// → iterate the CellFn (task 67) and re-run. **The search is not the fix.**
    NoGo,
}

/// The assembled correlation report.
#[derive(Clone, Debug)]
pub struct CorrelationReport {
    /// The budget (branches) at which cells are measured for measure 1.
    pub budget: u64,
    /// The effect-size floor `num/den` (|ρ| must reach it, negative), e.g. 3/10.
    pub effect_floor: (i128, i128),
    /// The stopping-rule ε `num/den` for measure 3, e.g. 1/1000.
    pub stop_eps: (u64, u64),
    /// Seeds per configuration (Klees discipline; ≥20 required).
    pub signal_seeds: u64,
    /// Baseline seeds.
    pub baseline_seeds: u64,
    /// Per-bug measures.
    pub bugs: Vec<BugMeasure>,
    /// STADS per configuration.
    pub stads: Vec<StadsMeasure>,
    /// The ruling.
    pub ruling: Ruling,
}

/// Fold a configuration's logs (in seed order) into one pooled species
/// accumulator, one branch = one sample.
/// A stable content digest of a log — the final tie-breaker for the STADS fold
/// sort, so two logs with the same `(seed, config)` still order canonically by
/// their contents (never by input position). An order-independent FNV-1a fold of
/// the canonical JSON encoding (a `BTreeMap`-free struct, so its serialization is
/// itself canonical).
fn content_digest(log: &CampaignLog) -> u64 {
    let bytes = serde_json::to_vec(log).unwrap_or_default();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn pooled_accumulator(logs: &[&CampaignLog]) -> SpeciesAccumulator {
    let mut acc = SpeciesAccumulator::new();
    for log in logs {
        for e in &log.events {
            acc.observe_branch(e.touched.iter().map(|c| c.to_be_bytes().to_vec()));
        }
    }
    acc
}

/// The first sample at which the running Good–Turing discovery probability falls
/// below ε, folding logs in order (prototype stopping rule). `None` if it never
/// does.
fn stop_sample(logs: &[&CampaignLog], eps_num: u64, eps_den: u64) -> Option<u64> {
    let mut acc = SpeciesAccumulator::new();
    let mut sample = 0u64;
    for log in logs {
        for e in &log.events {
            acc.observe_branch(e.touched.iter().map(|c| c.to_be_bytes().to_vec()));
            sample += 1;
            // Only once discovery has genuinely started (some individuals folded).
            if acc.stats().individuals > 0 && acc.stats().discovery_below(eps_num, eps_den) {
                return Some(sample);
            }
        }
    }
    None
}

impl CorrelationReport {
    /// Compute the report from the two configurations' logs. `budget` is the fixed
    /// branch count for measure 1; `effect_floor` (num, den) is the negative
    /// effect-size threshold; `stop_eps` is the stopping-rule ε.
    pub fn compute(
        benchmark: &Benchmark,
        logs: &[CampaignLog],
        budget: u64,
        effect_floor: (i128, i128),
        stop_eps: (u64, u64),
    ) -> Result<Self, ReportError> {
        // Fold trials keyed by (bug, config, seed). An **exact** duplicate (the
        // same trial logged twice) collapses silently; a **conflicting** duplicate
        // — the same seed with a DIFFERENT outcome — is a determinism violation and
        // is REJECTED loudly (round-6 P2), never silently deduped. A `BTreeMap`
        // yields the survivors in canonical key order, so everything downstream is
        // input-order-independent.
        let mut by_trial: BTreeMap<(BugId, Configuration, u64), &CampaignLog> = BTreeMap::new();
        for l in logs {
            match by_trial.get(&(l.bug, l.config, l.seed)) {
                Some(existing) if *existing != l => {
                    return Err(ReportError::ConflictingTrial {
                        bug: l.bug,
                        config: l.config,
                        seed: l.seed,
                    });
                }
                Some(_) => {} // exact duplicate — collapse
                None => {
                    by_trial.insert((l.bug, l.config, l.seed), l);
                }
            }
        }
        let deduped: Vec<&CampaignLog> = by_trial.into_values().collect();

        let signal: Vec<&CampaignLog> = deduped
            .iter()
            .copied()
            .filter(|l| l.config == Configuration::Signal)
            .collect();
        let baseline: Vec<&CampaignLog> = deduped
            .iter()
            .copied()
            .filter(|l| l.config == Configuration::Baseline)
            .collect();

        // Distinct seeds ATTEMPTED for a given bug under a given config — the
        // per-bug/config trial count the seed floor is enforced against.
        let attempts = |these: &[&CampaignLog], id: BugId| -> u64 {
            these
                .iter()
                .filter(|l| l.bug == id)
                .map(|l| l.seed)
                .collect::<BTreeSet<_>>()
                .len() as u64
        };

        let mut bugs = Vec::new();
        for spec in &benchmark.bugs {
            let id = spec.id;
            let signal_attempts = attempts(&signal, id);
            let baseline_attempts = attempts(&baseline, id);

            // Measure 1: novelty↔progress over signal seeds that found this bug.
            let mut cells = Vec::new();
            let mut ttb = Vec::new();
            let mut finder_seeds = BTreeSet::new();
            for log in &signal {
                if log.bug == id
                    && let Some(b) = log.find_branch(id)
                {
                    cells.push(log.distinct_cells_at(budget));
                    ttb.push(b);
                    finder_seeds.insert(log.seed);
                }
            }
            // Count **independent** finders (distinct seeds), so duplicated logs
            // cannot inflate the sample toward the GO floor.
            let signal_finders = finder_seeds.len() as u64;
            let novelty_progress = spearman(&cells, &ttb);

            // Measure 4: medians + IQR.
            let signal_median = median(&ttb);
            let signal_iqr = iqr(&ttb);
            let mut base_finder_seeds = BTreeSet::new();
            let base_ttb: Vec<u64> = baseline
                .iter()
                .filter(|l| l.bug == id)
                .filter_map(|l| {
                    l.find_branch(id).inspect(|_| {
                        base_finder_seeds.insert(l.seed);
                    })
                })
                .collect();
            let baseline_finders = base_finder_seeds.len() as u64;
            let baseline_median = median(&base_ttb);
            let baseline_iqr = iqr(&base_ttb);

            // Not worse: signal median ≤ baseline median (exact Frac compare). If
            // baseline never found it, signal is not worse. If signal never found
            // it but baseline did, signal IS worse.
            let signal_not_worse = match (signal_median, baseline_median) {
                (Some(s), Some(b)) => s <= b,
                (None, Some(_)) => false,
                (_, None) => true,
            };

            // Measure 2: trajectory (signal config), pooled over finding runs.
            let (mut novel_on_path, mut path_total) = (0u64, 0u64);
            let (mut base_num, mut base_den) = (0u64, 0u64);
            for log in signal.iter().filter(|l| l.bug == id) {
                let (bn, bd) = log.novel_admission_rate();
                base_num += bn;
                base_den += bd;
                for f in &log.finds {
                    if f.bug == id {
                        novel_on_path += f.novel_on_path;
                        path_total += f.path_len;
                    }
                }
            }
            // above_chance: novel_on_path/path_total > base_num/base_den.
            let above_chance = path_total > 0
                && base_den > 0
                && (novel_on_path as u128) * (base_den as u128)
                    > (base_num as u128) * (path_total as u128);
            let trajectory = TrajectoryMeasure {
                novel_on_path,
                path_total,
                base_num,
                base_den,
                above_chance,
            };

            // correlates: right direction (negative) AND |ρ| ≥ floor ⟺ ρ ≤ −floor,
            // AND the seed floor is met PER bug/config — ≥MIN_SEEDS independent
            // seeds ATTEMPTED under both configs (so a bug that never got the
            // trials cannot count toward a GO via other bugs — round-3), and
            // ≥MIN_SEEDS independent finders (so the correlation itself is not
            // undersampled — round-1).
            let (fnum, fden) = effect_floor;
            let correlates = signal_attempts >= MIN_SEEDS
                && baseline_attempts >= MIN_SEEDS
                && signal_finders >= MIN_SEEDS
                && novelty_progress
                    .map(|c| c.is_defined() && c.at_most(-fnum, fden))
                    .unwrap_or(false);

            bugs.push(BugMeasure {
                bug: id,
                name: spec.name.clone(),
                signal_attempts,
                baseline_attempts,
                signal_finders,
                baseline_finders,
                novelty_progress,
                trajectory,
                signal_median,
                signal_iqr,
                baseline_median,
                baseline_iqr,
                signal_not_worse,
                correlates,
            });
        }

        // Measure 3: STADS per configuration.
        let stads = [Configuration::Signal, Configuration::Baseline]
            .into_iter()
            .map(|cfg| {
                // Sort by seed before folding: the species-accumulation curve and
                // the stopping-rule sample both depend on the order branches are
                // folded, so a canonical (seed) order makes the rendered report
                // byte-identical regardless of the caller's log-concatenation
                // order (a determinism leak otherwise — round-2 P2).
                let mut these: Vec<&CampaignLog> = deduped
                    .iter()
                    .copied()
                    .filter(|l| l.config == cfg)
                    .collect();
                // Sort by a TOTAL key: seed, then config, then a content digest —
                // so even two logs sharing a seed (e.g. different bugs, same seed)
                // fold in a canonical order and the report never depends on input
                // ordering (round-4 P2). `sort_by_cached_key` computes each digest
                // once.
                these.sort_by_cached_key(|l| (l.seed, l.config, content_digest(l)));
                let acc = pooled_accumulator(&these);
                StadsMeasure {
                    config: cfg,
                    stats: acc.stats(),
                    chao1: acc.chao1(),
                    discovery: acc.discovery_probability(),
                    curve: acc.curve().to_vec(),
                    stop_at_sample: stop_sample(&these, stop_eps.0, stop_eps.1),
                }
            })
            .collect();

        // Independent-seed counts (distinct seeds, not log counts — duplicated
        // logs cannot inflate the sample toward the floor).
        let signal_seeds = signal.iter().map(|l| l.seed).collect::<BTreeSet<_>>().len() as u64;
        let baseline_seeds = baseline
            .iter()
            .map(|l| l.seed)
            .collect::<BTreeSet<_>>()
            .len() as u64;

        // The ruling. **Hard per-bug precondition (round-5):** EVERY bug in the
        // benchmark must have real data under BOTH configurations — ≥MIN_SEEDS
        // independent seeds attempted AND ≥1 certified find, per config. This is
        // checked before the ≥2-of-3 rule so a GO can never fire while the third
        // bug was never attempted or never found: an aggregate seed count can be
        // met via other bugs, and `not_worse_all` is vacuously true for a bug with
        // no data, so neither alone is sufficient.
        let every_bug_covered = bugs.iter().all(|b| {
            b.signal_attempts >= MIN_SEEDS
                && b.baseline_attempts >= MIN_SEEDS
                && b.signal_finders >= 1
                && b.baseline_finders >= 1
        });
        // ⚠️ SUPERSEDED by the M2 amendment (integrator ruling `fa9d323`, Paul
        // 2026-07-07). This binary "novelty correlates on ≥2 of 3 bugs AND signal
        // not worse on any" verdict is the PRE-AMENDMENT rule. With bug 2 deferred
        // (structurally uncalibratable) and bug 1 degenerate by design, ≥2-of-3 can
        // never be met, so this auto-verdict is now ADVISORY only. The AUTHORITATIVE
        // gate-4 ruling is DIRECTIONAL and lives in the hand-written
        // `CORRELATION-REPORT.md`: bug 3 (the sole real discriminator) clearly
        // positive AND no bug inverted → provisional GO; else NO-GO → SCORING
        // E-fails. This computed `ruling` field is retained only so the renderer can
        // print the measures with a banner pointing at the directional rule; do NOT
        // treat it as the gate decision.
        let correlating = bugs.iter().filter(|b| b.correlates).count();
        let not_worse_all = bugs.iter().all(|b| b.signal_not_worse);
        let ruling = if every_bug_covered && correlating >= 2 && not_worse_all {
            Ruling::Go
        } else {
            Ruling::NoGo
        };

        Ok(CorrelationReport {
            budget,
            effect_floor,
            stop_eps,
            signal_seeds,
            baseline_seeds,
            bugs,
            stads,
            ruling,
        })
    }

    /// Render `CORRELATION-REPORT.md`.
    pub fn render_markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# CORRELATION-REPORT — GO/NO-GO #2 (Phase F gate)\n");
        let _ = writeln!(
            s,
            "Signal seeds: **{}** · Baseline seeds: **{}** · Measure-1 budget: **{} branches** · \
             Effect-size floor: **ρ ≤ −{}/{}** · Stopping ε: **{}/{}**\n",
            self.signal_seeds,
            self.baseline_seeds,
            self.budget,
            self.effect_floor.0,
            self.effect_floor.1,
            self.stop_eps.0,
            self.stop_eps.1,
        );
        if self.signal_seeds < 20 || self.baseline_seeds < 20 {
            let _ = writeln!(
                s,
                "> ⚠️ **Trial discipline:** Klees et al. require ≥20 seeds per configuration; \
                 this report has {} signal / {} baseline. Ruling is provisional until met.\n",
                self.signal_seeds, self.baseline_seeds
            );
        }

        let _ = writeln!(s, "## Per-bug measures\n");
        let _ = writeln!(
            s,
            "| Bug | Attempts (sig/base) | Signal finders | 1: novelty↔progress ρ | correlates? | 2: trajectory (on-path vs base) | 4: signal median (IQR) | baseline median (IQR) | not worse? |"
        );
        let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|");
        for b in &self.bugs {
            let rho = match b.novelty_progress {
                Some(c) if c.is_defined() => format!("{:+.3} (n={})", c.rho_f64(), c.n),
                _ => "—".to_string(),
            };
            let traj = format!(
                "{}/{} vs {}/{} → {}",
                b.trajectory.novel_on_path,
                b.trajectory.path_total,
                b.trajectory.base_num,
                b.trajectory.base_den,
                if b.trajectory.above_chance {
                    "above"
                } else {
                    "at/below"
                }
            );
            let _ = writeln!(
                s,
                "| {} ({}) | {}/{} | {} | {} | {} | {} | {} | {} | {} |",
                b.bug.0,
                b.name,
                b.signal_attempts,
                b.baseline_attempts,
                b.signal_finders,
                rho,
                if b.correlates { "✅" } else { "❌" },
                traj,
                fmt_med(b.signal_median, b.signal_iqr),
                fmt_med(b.baseline_median, b.baseline_iqr),
                if b.signal_not_worse { "✅" } else { "❌" },
            );
        }

        let _ = writeln!(s, "\n## Measure 3: STADS species instrumentation\n");
        for m in &self.stads {
            let _ = writeln!(
                s,
                "### {:?}\n\n- Samples (branches): {}\n- Observed species S_obs: {}\n- \
                 Singletons f1: {} · doubletons f2: {}\n- Good–Turing discovery probability: \
                 {} ({:.5})\n- Chao1 richness: {} ({:.2}), estimated remaining ≈ {:.2}\n- \
                 Stopping rule (discovery < {}/{}) reached at sample: {}\n",
                m.config,
                m.stats.samples,
                m.stats.s_obs,
                m.stats.f1,
                m.stats.f2,
                fmt_frac(m.discovery),
                frac_f64(m.discovery),
                fmt_frac(m.chao1),
                frac_f64(m.chao1),
                (frac_f64(m.chao1) - m.stats.s_obs as f64).max(0.0),
                self.stop_eps.0,
                self.stop_eps.1,
                m.stop_at_sample
                    .map(|x| x.to_string())
                    .unwrap_or_else(|| "never (still discovering)".to_string()),
            );
            let _ = writeln!(s, "Species-accumulation curve (S_obs per branch):\n");
            let _ = writeln!(s, "```\n{}\n```\n", sparkline(&m.curve));
        }

        let _ = writeln!(s, "## The ruling (advisory — SUPERSEDED)\n");
        let _ = writeln!(
            s,
            "> ⚠️ The verdict below is the **pre-amendment** binary \"≥2 of 3 bugs\" auto-rule. It \
             is **superseded** by the M2 amendment (integrator ruling `fa9d323`): with bug 2 \
             deferred and bug 1 degenerate, ≥2-of-3 can never be met. The **authoritative** gate-4 \
             ruling is **directional** — bug 3 (the sole real discriminator) clearly positive AND \
             no bug inverted → provisional GO, else NO-GO — and is stated in the hand-written \
             `CORRELATION-REPORT.md`. Read this section as the measures' summary, not the gate.\n"
        );
        let correlating = self.bugs.iter().filter(|b| b.correlates).count();
        match self.ruling {
            Ruling::Go => {
                let _ = writeln!(
                    s,
                    "**GO.** Cell novelty correlates with bug progress (right direction, \
                     meaningful effect size) on **{}/{}** bugs, and the signal configuration's \
                     median time-to-bug is not worse than baseline on any bug. Phase F / task 70 \
                     dispatches.",
                    correlating,
                    self.bugs.len()
                );
            }
            Ruling::NoGo => {
                let _ = writeln!(
                    s,
                    "**NO-GO.** Correlation is absent or inverted, or the signal is worse than \
                     baseline on some bug ({}/{} bugs correlate). **The fix is the cell function \
                     (iterate task 67), never the search** — re-run this harness after.",
                    correlating,
                    self.bugs.len()
                );
            }
        }
        s
    }
}

fn fmt_frac(f: Frac) -> String {
    if f.den() == 1 {
        format!("{}", f.num())
    } else {
        format!("{}/{}", f.num(), f.den())
    }
}

fn fmt_med(m: Option<Frac>, i: Option<Frac>) -> String {
    match m {
        Some(m) => format!("{:.1} ({:.1})", frac_f64(m), i.map(frac_f64).unwrap_or(0.0)),
        None => "not found".to_string(),
    }
}

/// A tiny text sparkline of a monotone curve, for the report's species-curve
/// block (report rendering — the only place a curve is lossily summarised).
fn sparkline(curve: &[u64]) -> String {
    if curve.is_empty() {
        return "(empty)".to_string();
    }
    let bars = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = *curve.iter().max().unwrap_or(&1);
    let max = max.max(1);
    // Downsample to ≤ 60 columns.
    let step = curve.len().div_ceil(60).max(1);
    let mut out = String::new();
    for chunk in curve.chunks(step) {
        let v = *chunk.last().unwrap();
        let idx = ((v as u128 * (bars.len() as u128 - 1)) / max as u128) as usize;
        out.push(bars[idx]);
    }
    let _ = write!(out, "  S_obs 0→{max}");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Benchmark;

    /// Build a signal log where more cells ⇒ fewer branches to the bug (the
    /// GO-shaped correlation), for `n` seeds.
    fn signal_log(seed: u64, cells: u64, ttb: u64, bug: BugId) -> CampaignLog {
        // `cells` distinct ids touched across `ttb` branches; the find at `ttb`.
        let mut events = Vec::new();
        for branch in 0..=ttb {
            // Touch a fresh cell each branch up to `cells`, so distinct = min.
            let touched = if branch < cells {
                vec![seed * 10_000 + branch]
            } else {
                vec![]
            };
            events.push(BranchEvent { branch, touched });
        }
        CampaignLog {
            bug,
            config: Configuration::Signal,
            seed,
            events,
            finds: vec![FindRecord {
                bug,
                branch: ttb,
                path_len: 4,
                novel_on_path: 4,
            }],
            explore_period: 4,
            order_range: 64,
        }
    }

    fn baseline_log(seed: u64, ttb: u64, bug: BugId) -> CampaignLog {
        CampaignLog {
            bug,
            config: Configuration::Baseline,
            seed,
            events: (0..=ttb)
                .map(|branch| BranchEvent {
                    branch,
                    touched: vec![seed * 99 + branch],
                })
                .collect(),
            finds: vec![FindRecord {
                bug,
                branch: ttb,
                path_len: 4,
                novel_on_path: 0,
            }],
            explore_period: 4,
            order_range: 64,
        }
    }

    #[test]
    fn go_shaped_data_rules_go() {
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        // For each bug, 20 signal seeds: cells anti-correlated with ttb (more
        // cells → fewer branches), and baseline slower than signal.
        for spec in &bench.bugs {
            for k in 0..20u64 {
                // cells 20..1, ttb 100..(100+19*5): monotone opposite.
                let cells = 20 - k; // 20,19,...,1
                let ttb = 50 + k * 5; // 50,55,... increasing
                logs.push(signal_log(spec.id.0 as u64 * 1000 + k, cells, ttb, spec.id));
                logs.push(baseline_log(
                    spec.id.0 as u64 * 2000 + k,
                    200 + k * 5,
                    spec.id,
                ));
            }
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        assert_eq!(rep.signal_seeds, 60); // 3 bugs × 20 (all signal logs)
        // Each bug: perfect negative rank corr ⇒ correlates; signal median 55-ish
        // ≪ baseline 250-ish ⇒ not worse.
        for b in &rep.bugs {
            assert!(b.correlates, "{} should correlate", b.name);
            assert!(b.signal_not_worse, "{} signal not worse", b.name);
            assert_eq!(b.novelty_progress.unwrap().direction(), -1);
        }
        assert_eq!(rep.ruling, Ruling::Go);
        // The markdown renders and contains the verdict.
        let md = rep.render_markdown();
        assert!(md.contains("**GO.**"));
        assert!(md.contains("Chao1"));
    }

    #[test]
    fn undersampled_data_cannot_rule_go() {
        // Perfect GO-shaped correlation but only a FEW seeds — below the ≥20
        // independent-seed floor. A GO here would be exactly the failure the gate
        // exists to prevent, so it must rule NO-GO.
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..3u64 {
                let cells = 3 - k;
                let ttb = 50 + k * 5;
                logs.push(signal_log(spec.id.0 as u64 * 1000 + k, cells, ttb, spec.id));
                logs.push(baseline_log(
                    spec.id.0 as u64 * 2000 + k,
                    200 + k * 5,
                    spec.id,
                ));
            }
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        assert_eq!(rep.signal_seeds, 9); // 3 bugs × 3 seeds — well under 20
        // No bug counts as correlating (each has <20 finders), and the per-config
        // seed floor is unmet — either alone forces NO-GO.
        assert!(rep.bugs.iter().all(|b| !b.correlates));
        assert_eq!(rep.ruling, Ruling::NoGo);
        // The rendered report still surfaces the ⚠️ trial-discipline note.
        assert!(rep.render_markdown().contains("Trial discipline"));
    }

    /// Even at ≥20 seeds per config, a bug found by fewer than 20 independent
    /// finders cannot count toward the GO (its correlation is undersampled).
    #[test]
    fn a_bug_with_too_few_finders_does_not_count() {
        let bench = Benchmark::wave5();
        let bug = bench.bugs[0].id;
        let mut logs = Vec::new();
        // 25 signal + 25 baseline seeds per config, but bug 1 is found by only 5
        // of them (a perfect anti-correlation among those 5).
        for k in 0..25u64 {
            let finds_bug = k < 5;
            let cells = if finds_bug { 25 - k } else { 0 };
            let ttb = 50 + k * 5;
            let mut slog = signal_log(1000 + k, cells, ttb, bug);
            if !finds_bug {
                slog.finds.clear();
            }
            logs.push(slog);
            logs.push(baseline_log(2000 + k, 200 + k, bug));
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        assert_eq!(rep.signal_seeds, 25);
        let m = rep.bugs.iter().find(|b| b.bug == bug).unwrap();
        assert_eq!(m.signal_finders, 5);
        assert!(!m.correlates, "5 finders is below the seed floor");
    }

    #[test]
    fn per_bug_config_attempt_floor_enforced() {
        // Bug 1 has 20 signal finders (passes the finder floor) but only 5
        // BASELINE attempts — the comparison is ungrounded, so it must NOT count
        // toward a GO even though the global seed floor is met via other bugs.
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        // Bugs 2 & 3: full, GO-shaped (so the global floor is comfortably met).
        for id in [BugId(2), BugId(3)] {
            for k in 0..20u64 {
                logs.push(signal_log(id.0 as u64 * 1000 + k, 20 - k, 50 + k * 5, id));
                logs.push(baseline_log(id.0 as u64 * 2000 + k, 200 + k * 5, id));
            }
        }
        // Bug 1: 20 signal finders, but only 5 baseline attempts.
        for k in 0..20u64 {
            logs.push(signal_log(1000 + k, 20 - k, 50 + k * 5, BugId(1)));
        }
        for k in 0..5u64 {
            logs.push(baseline_log(2000 + k, 200 + k, BugId(1)));
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        let b1 = rep.bugs.iter().find(|b| b.bug == BugId(1)).unwrap();
        assert_eq!(b1.baseline_attempts, 5);
        assert!(
            !b1.correlates,
            "bug 1 with <20 baseline attempts cannot count toward GO"
        );
        // Bugs 2 & 3 still correlate → the ruling can still be GO on those two,
        // but bug 1's incomplete coverage is correctly excluded.
        assert!(rep.bugs.iter().filter(|b| b.correlates).count() >= 2);
    }

    #[test]
    fn two_of_three_bugs_with_data_cannot_rule_go() {
        // Bugs 1 & 2 have full, GO-shaped data (they correlate) but bug 3 has NO
        // data at all — never attempted/found. A GO must NOT fire: the third bug's
        // absence makes `not_worse_all` vacuously true and the aggregate seed floor
        // is met via bugs 1 & 2, yet coverage is incomplete (round-5 P1).
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for id in [BugId(1), BugId(2)] {
            for k in 0..20u64 {
                logs.push(signal_log(id.0 as u64 * 1000 + k, 20 - k, 50 + k * 5, id));
                logs.push(baseline_log(id.0 as u64 * 2000 + k, 200 + k * 5, id));
            }
        }
        // Bug 3: no logs.
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        assert!(rep.bugs.iter().filter(|b| b.correlates).count() >= 2);
        let b3 = rep.bugs.iter().find(|b| b.bug == BugId(3)).unwrap();
        assert_eq!(b3.signal_attempts, 0);
        assert_eq!(b3.baseline_finders, 0);
        assert_eq!(
            rep.ruling,
            Ruling::NoGo,
            "GO must require every bug to have data under both configs"
        );
    }

    /// A find under signal but never under baseline also blocks GO (no grounded
    /// comparison for that bug).
    #[test]
    fn a_bug_never_found_by_baseline_blocks_go() {
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                logs.push(signal_log(
                    spec.id.0 as u64 * 1000 + k,
                    20 - k,
                    50 + k * 5,
                    spec.id,
                ));
                // Baseline attempts exist for every bug, but bug 3's baseline
                // never FINDS it (clear the finds) — so baseline_finders == 0.
                let mut b = baseline_log(spec.id.0 as u64 * 2000 + k, 200 + k * 5, spec.id);
                if spec.id == BugId(3) {
                    b.finds.clear();
                }
                logs.push(b);
            }
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        let b3 = rep.bugs.iter().find(|b| b.bug == BugId(3)).unwrap();
        assert_eq!(b3.baseline_attempts, 20);
        assert_eq!(b3.baseline_finders, 0);
        assert_eq!(rep.ruling, Ruling::NoGo);
    }

    #[test]
    fn duplicate_trials_are_deduped() {
        // Duplicating a (bug, config, seed) log must not change the report.
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                logs.push(signal_log(
                    spec.id.0 as u64 * 1000 + k,
                    20 - k,
                    50 + k * 5,
                    spec.id,
                ));
                logs.push(baseline_log(
                    spec.id.0 as u64 * 2000 + k,
                    200 + k * 5,
                    spec.id,
                ));
            }
        }
        let clean = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        // Append exact duplicates of every log.
        let mut with_dups = logs.clone();
        with_dups.extend(logs.iter().cloned());
        let dup = CorrelationReport::compute(&bench, &with_dups, 30, (3, 10), (1, 1000)).unwrap();
        assert_eq!(
            clean.signal_seeds, dup.signal_seeds,
            "dedup keeps seed counts"
        );
        assert_eq!(
            clean.render_markdown(),
            dup.render_markdown(),
            "duplicate trials must not change the report"
        );
    }

    #[test]
    fn conflicting_trials_are_rejected() {
        // Two logs share the SAME (bug, config, seed) but DIFFER in content — the
        // same seed produced divergent outcomes, a determinism violation. This must
        // be REJECTED loudly, not silently deduped (round-6 P2). Order-independent:
        // whichever conflicting log comes first, the error is the same.
        let bench = Benchmark::wave5();
        let bug = BugId(1);
        let a = signal_log(7, 5, 50, bug);
        let b = signal_log(7, 5, 60, bug); // same (bug, config, seed), different ttb
        let err =
            CorrelationReport::compute(&bench, &[a.clone(), b.clone()], 30, (3, 10), (1, 1000))
                .unwrap_err();
        assert_eq!(
            err,
            ReportError::ConflictingTrial {
                bug,
                config: Configuration::Signal,
                seed: 7,
            }
        );
        // Reversed input rejects identically.
        let err2 = CorrelationReport::compute(&bench, &[b, a], 30, (3, 10), (1, 1000)).unwrap_err();
        assert_eq!(err, err2);
    }

    #[test]
    fn report_order_independent_with_colliding_seeds() {
        // Bugs share the SAME seed values across configs — so `(seed, config)`
        // alone does not uniquely order the logs; the content tie-breaker must
        // still make the report input-order-independent (round-4 P2).
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                // Identical seed `k` for every bug (collisions within a config).
                logs.push(signal_log(k, 20 - k, 50 + k * 5, spec.id));
                logs.push(baseline_log(k, 200 + k * 5, spec.id));
            }
        }
        let a = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000))
            .unwrap()
            .render_markdown();
        let mut shuffled = logs.clone();
        shuffled.reverse();
        let b = CorrelationReport::compute(&bench, &shuffled, 30, (3, 10), (1, 1000))
            .unwrap()
            .render_markdown();
        assert_eq!(a, b, "colliding-seed report must not depend on input order");
    }

    #[test]
    fn report_is_order_independent() {
        // The rendered report (incl. the STADS species curves + stopping sample)
        // must be byte-identical regardless of the caller's log order.
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                logs.push(signal_log(
                    spec.id.0 as u64 * 1000 + k,
                    20 - k,
                    50 + k * 5,
                    spec.id,
                ));
                logs.push(baseline_log(
                    spec.id.0 as u64 * 2000 + k,
                    200 + k * 5,
                    spec.id,
                ));
            }
        }
        let a = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000))
            .unwrap()
            .render_markdown();
        // A different concatenation order (reversed) must render identically.
        let mut shuffled = logs.clone();
        shuffled.reverse();
        let b = CorrelationReport::compute(&bench, &shuffled, 30, (3, 10), (1, 1000))
            .unwrap()
            .render_markdown();
        assert_eq!(a, b, "report must not depend on input log order");
    }

    #[test]
    fn inverted_correlation_rules_no_go() {
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                // POSITIVE correlation: more cells → MORE branches (wrong way).
                let cells = k + 1;
                let ttb = 50 + k * 5;
                logs.push(signal_log(spec.id.0 as u64 * 1000 + k, cells, ttb, spec.id));
                logs.push(baseline_log(
                    spec.id.0 as u64 * 2000 + k,
                    200 + k * 5,
                    spec.id,
                ));
            }
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        for b in &rep.bugs {
            assert!(!b.correlates, "{} inverted must not correlate", b.name);
        }
        assert_eq!(rep.ruling, Ruling::NoGo);
        assert!(rep.render_markdown().contains("**NO-GO.**"));
    }

    #[test]
    fn signal_worse_than_baseline_rules_no_go() {
        let bench = Benchmark::wave5();
        let mut logs = Vec::new();
        for spec in &bench.bugs {
            for k in 0..20u64 {
                let cells = 20 - k;
                // Signal SLOWER than baseline (worse) but still anti-correlated.
                let ttb = 500 + k * 5;
                logs.push(signal_log(spec.id.0 as u64 * 1000 + k, cells, ttb, spec.id));
                logs.push(baseline_log(spec.id.0 as u64 * 2000 + k, 100 + k, spec.id));
            }
        }
        let rep = CorrelationReport::compute(&bench, &logs, 30, (3, 10), (1, 1000)).unwrap();
        // Correlation is right, but signal median ≫ baseline ⇒ worse ⇒ NO-GO.
        assert!(rep.bugs.iter().all(|b| b.correlates));
        assert!(rep.bugs.iter().all(|b| !b.signal_not_worse));
        assert_eq!(rep.ruling, Ruling::NoGo);
    }

    #[test]
    fn trajectory_above_chance_detected() {
        let bench = Benchmark::wave5();
        let bug = bench.bugs[0].id;
        // One signal seed: base rate low (few novel-admitting branches), but the
        // find's path is all-novel ⇒ above chance.
        let mut events: Vec<BranchEvent> = (0..100)
            .map(|branch| BranchEvent {
                branch,
                // Only the first 10 branches admit novel cells (base rate 10/100).
                touched: if branch < 10 { vec![branch] } else { vec![0] },
            })
            .collect();
        events.push(BranchEvent {
            branch: 100,
            touched: vec![0],
        });
        let log = CampaignLog {
            bug,
            config: Configuration::Signal,
            seed: 1,
            events,
            finds: vec![FindRecord {
                bug,
                branch: 100,
                path_len: 4,
                novel_on_path: 4,
            }],
            explore_period: 4,
            order_range: 64,
        };
        let rep = CorrelationReport::compute(&bench, &[log], 50, (3, 10), (1, 1000)).unwrap();
        let m = rep.bugs.iter().find(|b| b.bug == bug).unwrap();
        // 4/4 on-path novel = 1.0 > base 10/101 ⇒ above chance.
        assert!(m.trajectory.above_chance);
    }
}
