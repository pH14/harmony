// SPDX-License-Identifier: AGPL-3.0-or-later
//! # `benchmark` — the seeded-bug benchmark + signal→bug correlation harness
//!
//! This crate is **GO/NO-GO #2**, the gate on Phase F (`docs/EXPLORATION.md`'s
//! second hard problem): *a feedback signal that does not correlate with bugs
//! makes a better search optimise the wrong thing faster.* It extends task 60's
//! single planted bug into a **benchmark** of ≥3 bugs of distinct classes and
//! measures, with ground truth, whether the Phase-D signal stack's cell novelty
//! correlates with progress toward them.
//!
//! It is **pure logic** — everything here runs on macOS + Linux and is fully
//! unit/property tested. It has three parts:
//!
//! * [`manifest`] — the benchmark fixture: the bugs, their distinct classes,
//!   per-bug serial markers, and **tunable** trigger thresholds. Designed so the
//!   later bugs (iv) partition-duration, (v) depth-2 concurrency, (vi)
//!   convergence/liveness slot in without restructuring (tasks 72/75).
//! * [`trigger`] — the **toy trigger predicates**: a pure model of each bug's
//!   fire condition over an opaque `(seed, fault schedule)` scenario. This is the
//!   portable stand-in for the guest payloads (mirrors
//!   `dissonance/campaign-runner`'s `ToyPlantedMachine` for bug 1); the gate is that
//!   the right schedule fires 100% and a nominal scenario never fires.
//! * [`stats`] and [`report`] — the correlation instrument: hand-rolled rank
//!   (Spearman) correlation over integers, median/IQR, the STADS species curves
//!   (via [`explorer::stads`]), and the four measures the spec mandates rendered
//!   into `CORRELATION-REPORT.md` with an explicit **GO / NO-GO** ruling.
//!
//! Floats are confined to report **rendering** ([`stats::RankCorr::rho_f64`] and
//! the markdown); every *decision* (effect-size threshold, median comparison,
//! stopping rule) is an exact integer/rational cross-multiplication (conventions
//! rule 4).

pub mod exploration;
pub mod manifest;
pub mod maze;
pub mod report;
pub mod stats;
pub mod trigger;

pub use exploration::{
    DiscoveryEvent, ExplorationConfig, ExplorationError, ExplorationLog, ExplorationReport,
    GameManifest, Verdict,
};
pub use manifest::{Benchmark, BugClass, BugId, BugSpec, CrashKind, TriggerParams};
pub use maze::{MazeGateManifest, MazeGateReport};
pub use report::{
    BranchEvent, CampaignLog, Configuration, CorrelationReport, ReportError, Ruling,
    TrajectoryMeasure,
};
pub use stats::{RankCorr, iqr, median, spearman};
pub use trigger::{FaultKind, Perturbation, Scenario};
