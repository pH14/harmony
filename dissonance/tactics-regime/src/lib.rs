// SPDX-License-Identifier: AGPL-3.0-or-later
#![forbid(unsafe_code)]
//! # tactics-regime — bursty regime fault tactics + `EnvCodec` sequence mutators
//!
//! The first real content for the two proposal axes `docs/EXPLORATION.md` splits
//! the Proposal seam into, behind the task-64 spine:
//!
//! - The **entropy axis (inner):** [`RegimeTactic`], a spine [`Tactic`] whose
//!   fault entropy is *bursty and autocorrelated* rather than IID. A two-state
//!   [`RegimeProcess`] Markov chain (`Calm`/`Storm`) modulates a per-class fault
//!   table; storms are geometric dwell periods of elevated, clustered fault
//!   probability. FoundationDB's operational lesson — real faults cluster, and a
//!   *second* fault landing while the system is still recovering from the first
//!   is what finds recovery-path bugs an IID coin at a progress-permitting rate
//!   never reaches — transplanted as `buggify`+swizzle.
//! - The **mutation axis (outer):** [`SeqMutators`], AFLNet-style region
//!   operators over a `Recorded` env's `Moment → Action` schedule
//!   (`insert`/`delete`/`retarget`/`shift`), each a pure deterministic
//!   `(env, salt) -> EnvSpec` matching [`environment::EnvCodec::mutate`]'s shape.
//!
//! Both are one arm of the eventual task-70/72 portfolio, never a replacement
//! for the `quiet`/IID baselines (Coyote: no single strategy dominates).
//! Enforcement of what these tactics/schedules *emit* is tasks 59/61's business;
//! this crate is pure logic and never touches `consonance/vmm-core`.
//!
//! ## Invariants (each is gated)
//!
//! - **(a) Open-loop rollout.** [`RegimeTactic::decide`] is a function of
//!   `(own regime state, point, rng)` and nothing else — no `Sensor`/`Archive`/
//!   `RunTrace` type appears anywhere in this crate's dependency graph, so a
//!   decision *structurally cannot* read mid-run feedback. Identical
//!   `(state, point, rng)` ⇒ identical answer; all adaptation happens between
//!   runs, in the search loop.
//! - **(b) Determinism discipline.** Seeded [`Prng`](explorer::Prng) only, and
//!   **no floats anywhere** — regime/transition probabilities, the stationary
//!   rate, and every statistical gate are integer/fixed-point rationals compared
//!   by cross-multiplication.
//! - **(c) search loop untouched.** These tactics/mutators grow the Proposal
//!   seam only; no explorer-engine change is needed or made (the `DISSONANCE.md`
//!   D-invariant).
//!
//! ## Mutator safety (inherited from [`environment::EnvCodec::mutate`] + task 93)
//!
//! - [`Action::Guest`](environment::Action::Guest) overrides are preserved
//!   verbatim — never removed, relocated, or overwritten (a guest answer is
//!   context-bound and needs the live decision the offline codec lacks).
//! - No [`StandingFault`](environment::StandingFault) is ever introduced.
//! - `Moment` arithmetic rejects overflow, never wraps; a translation or
//!   collision hazard **fails closed** (the env is returned unchanged).
//! - Inserted host faults are confined to the enforced v1 vocabulary
//!   ([`CorruptMemory`](environment::HostFault::CorruptMemory),
//!   [`InjectInterrupt`](environment::HostFault::InjectInterrupt)); the
//!   task-59-deferred `SkewTime`/`SetClockRate` are never emitted.

mod mutators;
mod regime;
mod tactic;

pub use mutators::SeqMutators;
pub use regime::{Regime, RegimeError, RegimeParams, RegimeProcess, StateTable};
pub use tactic::{RegimeTactic, class_of, class_tag};
