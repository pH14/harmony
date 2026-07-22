// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **maze exploration gate** report (task 134, `hm-cs5`): the offline
//! analysis of the maze campaign's discovery-event logs — the first
//! cooperative Differential exploration gate's committed artifact
//! (`MAZE-EXPLORATION-REPORT.md`).
//!
//! The subject under test is the **simple archive-guided selector**
//! ([`ExplorationConfig::SelectorV1`] — the explore/exploit v1 shape over the
//! two-barrier controller's retained Entries), scored against the two ruled
//! permanent controls at the identical branch budget:
//!
//! - [`ExplorationConfig::PureRandom`] — the **pass/fail line** (task 84's
//!   ruling, inherited): always-explore with the frontier held empty (no
//!   candidate materialization at all) — random-restart search.
//! - [`ExplorationConfig::FrontierOff`] — the **diagnostic column**: the full
//!   snapshot/materialization machinery runs, the selector never exploits.
//!   Separates "the archive machinery is behavior-neutral" from "novelty
//!   steering helps"; never part of the pass condition.
//!
//! Unlike the SMB report there is no ROM: the maze's vacuity gates are the
//! workload's own — the manifest records the **exact reachable-cell frontier**
//! (`maze::reachable_cells`), and a pass additionally requires the baseline to
//! plateau *below* it (a saturated control would make the win vacuous). The
//! goal witness needs no extra log field: a seed whose depth reaches
//! `levels` reached the goal tile (its Y register at the goal), so held
//! progress evidence is derived per configuration from the depth samples.
//!
//! Determinism discipline: identical to [`crate::exploration`] — exact
//! rationals for every decision, floats only in the rendered markdown.

use std::fmt::Write as _;

use explorer::stads::Frac;
use serde::{Deserialize, Serialize};

use crate::exploration::{
    ConfigSummary, ExplorationConfig, ExplorationError, ExplorationLog, ExplorationStads,
    StrictBeats, Verdict, dedupe_trials, dense_check, pooled_stads, strict_beats, summarize_config,
};
use crate::stats::frac_f64;

/// The maze gate's manifest: the workload shape and the campaign budget knobs,
/// recorded verbatim so results are comparable across runs of the same maze
/// and the non-vacuity claim is checked against the documented frontier.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct MazeGateManifest {
    /// The workload name (`"maze"`).
    pub workload: String,
    /// Corridor length per level (`maze::MazeSpec::width`).
    pub width: u32,
    /// Corridor levels (`maze::MazeSpec::levels`) — the goal's depth witness:
    /// a seed whose max depth equals `levels` reached the goal.
    pub levels: u32,
    /// Doors per junction (`maze::MazeSpec::doors`).
    pub doors: u32,
    /// The maze seed (fixes the correct doors; not a campaign seed).
    pub maze_seed: u64,
    /// The **exact** reachable observable-tile count
    /// (`maze::reachable_cells`) — the documented frontier the baseline must
    /// demonstrably stay below.
    pub reachable_cells: u64,
    /// Walk steps per rollout (the portable toy's natural terminal; a live
    /// guest walks until its V-time deadline).
    pub steps_per_rollout: u32,
    /// The per-rollout V-time deadline (ns past the branch point) every
    /// configuration ran with — part of the budget: logs measured under
    /// different rollout durations are not comparable, so a change across
    /// appends is manifest drift. `None` = the portable toy's natural
    /// terminals.
    #[serde(default)]
    pub deadline_delta: Option<u64>,
    /// The fixed branch budget every configuration runs at (identical across
    /// configurations — task 84's ruling).
    pub branch_budget: u64,
    /// SelectorV1's explore period (every Nth step explores fresh).
    pub explore_period: u64,
    /// The two-barrier per-step materialization cap (zeroed for PureRandom —
    /// the frontier-held-empty control).
    pub candidate_cap: usize,
    /// The campaign-total materialization-replay budget (zeroed for
    /// PureRandom).
    pub replay_budget: u64,
}

/// The maze exploration report (the gate artifact).
#[derive(Clone, Debug)]
pub struct MazeGateReport {
    /// The manifest the campaign ran with.
    pub manifest: MazeGateManifest,
    /// Per-configuration summaries, in `[SelectorV1, PureRandom, FrontierOff]`
    /// order (present configurations only).
    pub configs: Vec<ConfigSummary>,
    /// Per-configuration goal counts: how many of the configuration's seeds
    /// reached the goal (max depth = `levels`) — the held progress evidence.
    pub goal_seeds: Vec<(ExplorationConfig, u64)>,
    /// The strict subject-vs-pure-random comparison on distinct cells.
    pub cells_beats: Option<StrictBeats>,
    /// The strict comparison on depth.
    pub depth_beats: Option<StrictBeats>,
    /// Whether the pure-random control demonstrably still explores (median
    /// distinct cells strictly above one — movement beyond the start).
    pub baseline_live: Option<bool>,
    /// Whether the pure-random control plateaus **below** the documented
    /// reachable frontier (median distinct cells strictly below
    /// `reachable_cells`) — the non-vacuity gate.
    pub baseline_below_frontier: Option<bool>,
    /// The pooled STADS instrumentation for the subject configuration.
    pub stads: Option<ExplorationStads>,
    /// The verdict.
    pub verdict: Verdict,
}

impl MazeGateReport {
    /// Compute the report over the campaign logs. `stop_eps = (num, den)` is
    /// the STADS stopping-rule ε. Fails loudly on a foreign workload,
    /// non-dense logs, conflicting same-seed trials, or a violated seed floor.
    pub fn compute(
        manifest: &MazeGateManifest,
        logs: &[ExplorationLog],
        stop_eps: (u64, u64),
    ) -> Result<MazeGateReport, ExplorationError> {
        if logs.is_empty() {
            return Err(ExplorationError::NoLogs);
        }
        let budget = manifest.branch_budget;
        for log in logs {
            if log.workload != manifest.workload {
                return Err(ExplorationError::WorkloadMismatch {
                    manifest: manifest.workload.clone(),
                    log: log.workload.clone(),
                    config: log.config,
                    seed: log.seed,
                });
            }
            dense_check(log, budget)?;
        }
        let by_key = dedupe_trials(logs)?;

        let mut configs = Vec::new();
        let mut goal_seeds = Vec::new();
        for config in [
            ExplorationConfig::SelectorV1,
            ExplorationConfig::PureRandom,
            ExplorationConfig::FrontierOff,
        ] {
            let Some(summary) = summarize_config(&by_key, config, budget)? else {
                continue;
            };
            // The goal witness: a seed whose max depth reached `levels` stood
            // on the goal tile (its Y register there is exactly `levels`).
            let goal = summary
                .depths
                .iter()
                .filter(|d| **d >= u64::from(manifest.levels))
                .count() as u64;
            goal_seeds.push((config, goal));
            configs.push(summary);
        }

        let find = |c: ExplorationConfig| configs.iter().find(|s| s.config == c);
        let subject = find(ExplorationConfig::SelectorV1);
        let baseline = find(ExplorationConfig::PureRandom);
        let diagnostic = find(ExplorationConfig::FrontierOff);

        let cells_beats = subject.zip(baseline).map(|(s, b)| strict_beats(s, b, true));
        let depth_beats = subject
            .zip(baseline)
            .map(|(s, b)| strict_beats(s, b, false));
        // "Live" means movement beyond the single start cell (the SMB round-9
        // rule, inherited).
        let baseline_live = baseline.map(|b| b.cells_median > Frac::whole(1));
        // The maze's own non-vacuity gate: the control must plateau below the
        // documented reachable frontier, else the win is vacuous.
        let baseline_below_frontier =
            baseline.map(|b| Frac::whole(u128::from(manifest.reachable_cells)) > b.cells_median);

        let stads =
            subject.map(|_| pooled_stads(&by_key, ExplorationConfig::SelectorV1, budget, stop_eps));

        let verdict = match (subject, baseline, diagnostic) {
            (Some(_), Some(_), Some(_)) => {
                if baseline_live == Some(false) {
                    Verdict::Incomplete {
                        reason: "the pure-random control never moved beyond a single cell at the \
                                 median — a dead or frozen control cannot ground a win (check the \
                                 workload wiring before scoring)"
                            .to_string(),
                    }
                } else if baseline_below_frontier == Some(false) {
                    Verdict::Incomplete {
                        reason: format!(
                            "the pure-random control saturated the documented reachable frontier \
                             ({} cells) at the median — the maze is too easy at this budget, so \
                             any win would be vacuous; deepen the manifest",
                            manifest.reachable_cells
                        ),
                    }
                } else {
                    // Statically infallible: subject and baseline are present.
                    let cb = cells_beats.expect("subject and baseline present");
                    let db = depth_beats.expect("subject and baseline present");
                    if cb.beats() && db.beats() {
                        Verdict::Pass
                    } else {
                        Verdict::Fail
                    }
                }
            }
            _ => {
                let mut missing = Vec::new();
                if subject.is_none() {
                    missing.push("archive-guided (selector v1)");
                }
                if baseline.is_none() {
                    missing.push("pure-random baseline");
                }
                if diagnostic.is_none() {
                    missing.push("frontier-off diagnostic");
                }
                Verdict::Incomplete {
                    reason: format!(
                        "missing configuration(s): {} — the gate needs the subject and BOTH \
                         permanent controls",
                        missing.join(", ")
                    ),
                }
            }
        };

        Ok(MazeGateReport {
            manifest: manifest.clone(),
            configs,
            goal_seeds,
            cells_beats,
            depth_beats,
            baseline_live,
            baseline_below_frontier,
            stads,
            verdict,
        })
    }

    /// Render the committed markdown report (floats here only — rendering).
    pub fn render_markdown(&self) -> String {
        let mut md = String::new();
        let m = &self.manifest;
        let _ = writeln!(md, "# Maze exploration gate report (task 134)");
        md.push('\n');
        let _ = writeln!(
            md,
            "- Maze: {}×{} corridor levels, {} doors/junction, maze seed {:#x}",
            m.width, m.levels, m.doors, m.maze_seed
        );
        let _ = writeln!(
            md,
            "- Documented reachable frontier: **{} observable tiles** (exact)",
            m.reachable_cells
        );
        let _ = writeln!(
            md,
            "- Faults **off** the whole time: zero fault vocabulary — the `quiet` arm."
        );
        let _ = writeln!(
            md,
            "- Branch budget (identical for every configuration): {}; {} walk steps/rollout",
            m.branch_budget, m.steps_per_rollout
        );
        match m.deadline_delta {
            Some(d) => {
                let _ = writeln!(
                    md,
                    "- Rollout deadline (V-time ns past the branch point): {d}"
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
            "- Subject: archive-guided selector v1 (explore period {}); candidate cap {}, \
             replay budget {} (zeroed for the pure-random frontier-held-empty control)",
            m.explore_period, m.candidate_cap, m.replay_budget
        );
        md.push('\n');

        let _ = writeln!(md, "## Configurations\n");
        let _ = writeln!(
            md,
            "| configuration | seeds | distinct cells (median) | cells IQR [q1, q3] | \
             depth (median) | depth IQR [q1, q3] | goal reached (seeds) |"
        );
        let _ = writeln!(md, "|---|---|---|---|---|---|---|");
        for s in &self.configs {
            let goal = self
                .goal_seeds
                .iter()
                .find(|(c, _)| *c == s.config)
                .map(|(_, g)| *g)
                .unwrap_or(0);
            let _ = writeln!(
                md,
                "| {} | {} | {:.1} | [{:.1}, {:.1}] | {:.1} | [{:.1}, {:.1}] | {}/{} |",
                s.config.label(),
                s.seeds,
                frac_f64(s.cells_median),
                frac_f64(s.cells_quartiles.0),
                frac_f64(s.cells_quartiles.2),
                frac_f64(s.depth_median),
                frac_f64(s.depth_quartiles.0),
                frac_f64(s.depth_quartiles.2),
                goal,
                s.seeds,
            );
        }
        md.push('\n');

        if let (Some(cb), Some(db)) = (self.cells_beats, self.depth_beats) {
            let _ = writeln!(
                md,
                "## Archive-guided vs pure-random (the pass condition)\n"
            );
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
                "- control demonstrably live: **{}**; control below the reachable frontier: **{}**",
                self.baseline_live.unwrap_or(false),
                self.baseline_below_frontier.unwrap_or(false),
            );
            md.push('\n');
        }

        if let Some(st) = &self.stads {
            let _ = writeln!(md, "## STADS (archive-guided configuration, pooled)\n");
            let _ = writeln!(
                md,
                "- observed species: {}; Chao1 richness: {:.1}; end-of-fold discovery \
                 probability: {:.4}",
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
            md.push('\n');
        }

        let _ = writeln!(md, "## Verdict\n");
        match &self.verdict {
            Verdict::Pass => {
                let _ = writeln!(
                    md,
                    "**PASS** — the archive-guided simple selector strictly beats pure-random on \
                     both distinct cells and depth (greater medians, non-overlapping IQRs) \
                     against a live control that plateaus below the documented reachable \
                     frontier."
                );
            }
            Verdict::Fail => {
                let _ = writeln!(
                    md,
                    "**FAIL** — the archive-guided configuration did not strictly beat \
                     pure-random on both measures. Routing (task 84 gate 4, inherited): the \
                     suspect is the cell function or the workload manifest — one documented \
                     host-side retune, then escalate to the integrator. Never search cleverness, \
                     never a workload nerf."
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
    use crate::exploration::DiscoveryEvent;
    use crate::report::MIN_SEEDS;

    fn manifest() -> MazeGateManifest {
        MazeGateManifest {
            workload: "maze".to_string(),
            width: 4,
            levels: 6,
            doors: 4,
            maze_seed: 0x6d61_7a65,
            reachable_cells: 43,
            steps_per_rollout: 48,
            deadline_delta: None,
            branch_budget: 8,
            explore_period: 3,
            candidate_cap: 2,
            replay_budget: 64,
        }
    }

    fn ev(branch: u64, touched: &[u64], depth: u64) -> DiscoveryEvent {
        DiscoveryEvent {
            branch,
            touched: touched.to_vec(),
            depth,
            state_hash: format!("{branch:064x}"),
        }
    }

    /// A log whose per-branch cells ramp (all distinct per seed) to
    /// `cells_per × 8` and whose depth ramps to `max_depth`.
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
                ev(
                    b,
                    &touched,
                    (b * max_depth) / 8 + if b == 7 { max_depth % 8 } else { 0 },
                )
            })
            .collect();
        ExplorationLog {
            workload: "maze".to_string(),
            rom_sha256: None,
            config,
            seed,
            events,
        }
    }

    fn twenty(config: ExplorationConfig, cells_per: u64, max_depth: u64) -> Vec<ExplorationLog> {
        (0..MIN_SEEDS)
            .map(|s| ramp_log(config, s, cells_per, max_depth))
            .collect()
    }

    fn all_three(subject_cells: u64, subject_depth: u64) -> Vec<ExplorationLog> {
        let mut logs = twenty(ExplorationConfig::SelectorV1, subject_cells, subject_depth);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 2));
        logs.extend(twenty(ExplorationConfig::FrontierOff, 2, 2));
        logs
    }

    #[test]
    fn pass_when_subject_strictly_beats_a_live_below_frontier_baseline() {
        let report = MazeGateReport::compute(&manifest(), &all_three(4, 6), (1, 1000)).unwrap();
        assert_eq!(report.verdict, Verdict::Pass);
        assert!(report.cells_beats.unwrap().beats());
        assert!(report.depth_beats.unwrap().beats());
        assert_eq!(report.baseline_live, Some(true));
        assert_eq!(report.baseline_below_frontier, Some(true));
        // The goal witness: subject seeds reached depth = levels (6).
        let goal = report
            .goal_seeds
            .iter()
            .find(|(c, _)| *c == ExplorationConfig::SelectorV1)
            .unwrap()
            .1;
        assert_eq!(goal, MIN_SEEDS);
        let md = report.render_markdown();
        assert!(md.contains("PASS"));
        assert!(md.contains("frontier-off (diagnostic)"));
        assert!(md.contains("quiet"));
    }

    #[test]
    fn fail_when_medians_tie() {
        let mut logs = twenty(ExplorationConfig::SelectorV1, 2, 2);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 2));
        logs.extend(twenty(ExplorationConfig::FrontierOff, 2, 2));
        let report = MazeGateReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.verdict, Verdict::Fail);
    }

    /// The gate needs the subject and BOTH permanent controls — a missing
    /// frontier-off diagnostic is Incomplete, never scored around.
    #[test]
    fn missing_frontier_off_control_is_incomplete() {
        let mut logs = twenty(ExplorationConfig::SelectorV1, 4, 6);
        logs.extend(twenty(ExplorationConfig::PureRandom, 2, 2));
        let report = MazeGateReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        match &report.verdict {
            Verdict::Incomplete { reason } => assert!(reason.contains("frontier-off")),
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// A control that saturates the documented reachable frontier makes any
    /// win vacuous — Incomplete, never Pass.
    #[test]
    fn saturated_baseline_is_incomplete_not_pass() {
        let mut m = manifest();
        m.reachable_cells = 10; // the ramp logs discover 16 cells/seed
        let report = MazeGateReport::compute(&m, &all_three(8, 6), (1, 1000)).unwrap();
        assert_eq!(report.baseline_below_frontier, Some(false));
        match &report.verdict {
            Verdict::Incomplete { reason } => assert!(reason.contains("saturated")),
            other => panic!("expected Incomplete, got {other:?}"),
        }
    }

    /// A frozen control (one cell every branch) is dead — Incomplete.
    #[test]
    fn frozen_baseline_is_incomplete() {
        let mut logs = twenty(ExplorationConfig::SelectorV1, 4, 6);
        logs.extend((0..MIN_SEEDS).map(|s| ExplorationLog {
            workload: "maze".to_string(),
            rom_sha256: None,
            config: ExplorationConfig::PureRandom,
            seed: s,
            events: (0..8).map(|b| ev(b, &[42], 0)).collect(),
        }));
        logs.extend(twenty(ExplorationConfig::FrontierOff, 2, 2));
        let report = MazeGateReport::compute(&manifest(), &logs, (1, 1000)).unwrap();
        assert_eq!(report.baseline_live, Some(false));
        assert!(matches!(report.verdict, Verdict::Incomplete { .. }));
    }

    /// Foreign-workload logs are rejected against the manifest.
    #[test]
    fn foreign_workload_logs_are_rejected() {
        let mut logs = all_three(4, 6);
        logs[3].workload = "smb".to_string();
        assert!(matches!(
            MazeGateReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::WorkloadMismatch { seed: 3, .. })
        ));
    }

    /// The task-69 disciplines carry over: dense budgets, conflicting trials,
    /// and the seed floor.
    #[test]
    fn shared_disciplines_carry_over() {
        // Truncated log.
        let mut logs = all_three(4, 6);
        logs[5].events.pop();
        assert!(matches!(
            MazeGateReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::BadBranchSequence { seed: 5, .. })
        ));
        // Conflicting same-seed trial.
        let mut logs = all_three(4, 6);
        let mut divergent = logs[0].clone();
        divergent.events[0].depth += 1;
        logs.push(divergent);
        assert!(matches!(
            MazeGateReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::ConflictingTrial { seed: 0, .. })
        ));
        // Seed floor.
        let logs: Vec<ExplorationLog> = (0..MIN_SEEDS - 1)
            .map(|s| ramp_log(ExplorationConfig::PureRandom, s, 2, 2))
            .collect();
        assert!(matches!(
            MazeGateReport::compute(&manifest(), &logs, (1, 1000)),
            Err(ExplorationError::TooFewSeeds { .. })
        ));
    }
}
