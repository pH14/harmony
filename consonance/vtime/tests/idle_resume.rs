// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 (task 52) — idle-resume property tests: an idle jump lands the clock
//! at exactly `D`, advances V-time monotonically, and **never** fabricates a
//! retired branch (the load-bearing invariant: a jump moves only the idle
//! accumulator, never the execution component). Driven against an **independent**
//! reference model of `elapsed = execution + idle` — not a mirror of the
//! `VClock`/`IdlePlanner` impl (it sums `work·ratio + idle` from scratch, with no
//! `vns_base` accumulation), so a bug in the impl's accounting diverges from it.

use proptest::prelude::*;
use vtime::{IdlePlanner, VClock, VClockConfig};

/// An independent model of the guest clock as `execution + idle`: it tracks the
/// cumulative retired branches (`work`) and the cumulative idle V-time
/// (`idle_vns`) separately and computes the guest-perceived V-time directly as
/// `work·ratio + idle_vns` (saturating), with **no** reference to the impl's
/// `vns_base` mechanics. `ratio_den` is fixed at 1 (the production constraint —
/// `VtimeWiring` rejects a fractional ratio), so `execution = work·ratio` is
/// exact.
struct ExecPlusIdle {
    ratio_num: u64,
    work: u64,
    idle_vns: u64,
}

impl ExecPlusIdle {
    fn new(ratio_num: u64, initial_idle_vns: u64) -> Self {
        Self {
            ratio_num,
            work: 0,
            idle_vns: initial_idle_vns,
        }
    }

    /// Guest-perceived V-time = execution + idle, computed independently in
    /// `u128` and saturated to `u64` (the crate-wide overflow rule).
    fn vtime(&self) -> u64 {
        let execution = u128::from(self.work) * u128::from(self.ratio_num);
        u64::try_from(execution + u128::from(self.idle_vns)).unwrap_or(u64::MAX)
    }

    /// Execute `branches` retired conditional branches (pure execution; idle
    /// untouched).
    fn execute(&mut self, branches: u64) {
        self.work = self.work.saturating_add(branches);
    }

    /// Idle-jump to `deadline_vns`: add `max(0, deadline − now)` to the idle
    /// accumulator. Returns the advance applied (for cross-checking).
    fn idle_to(&mut self, deadline_vns: u64) -> u64 {
        let now = self.vtime();
        let advance = deadline_vns.saturating_sub(now);
        self.idle_vns = self.idle_vns.saturating_add(advance);
        advance
    }
}

/// One driver step: either execute some branches, or idle to a deadline.
#[derive(Debug, Clone)]
enum Op {
    Execute(u64),
    Idle(u64),
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Bias toward modest, realistic branch counts and deadlines, with a
        // tail into the huge regime to exercise saturation.
        3 => (prop_oneof![6 => 0u64..=1_000_000, 1 => 0u64..=u64::MAX / 8])
            .prop_map(Op::Execute),
        3 => (prop_oneof![6 => 0u64..=2_000_000, 1 => (u64::MAX - 4_000)..=u64::MAX])
            .prop_map(Op::Idle),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Drive a VClock + IdlePlanner and the independent `ExecPlusIdle` model
    /// through the same op sequence. After every op the impl's `vns(work)` must
    /// equal the model's `vtime()`; an idle jump must land the clock exactly at
    /// the deadline (when in the future) and leave the execution component (and
    /// thus the work count) untouched; V-time is monotonic throughout.
    #[test]
    fn idle_and_execute_track_independent_model(
        ratio_num in 1u64..=4_000,
        initial_idle_vns in 0u64..=1 << 40,
        ops in proptest::collection::vec(op_strategy(), 1..50),
    ) {
        let cfg = VClockConfig {
            ratio_num,
            ratio_den: 1,
            guest_hz: 2_000_000_000,
            guest_base: 0,
            vns_base: initial_idle_vns, // the idle accumulator's starting value
        };
        let mut clk = VClock::new(cfg).expect("ratio_den==1 config is valid");
        let mut model = ExecPlusIdle::new(ratio_num, initial_idle_vns);
        let planner = IdlePlanner::new();

        // Our own copy of the work counter: ONLY `Execute` mutates it. The impl
        // never sees it during an idle jump, so any divergence on an idle op
        // proves a fabricated branch.
        let mut work = 0u64;
        let mut prev_vtime = clk.vns(work);
        prop_assert_eq!(prev_vtime, model.vtime(), "initial clock != model");

        for op in ops {
            match op {
                Op::Execute(branches) => {
                    work = work.saturating_add(branches);
                    model.execute(branches);
                }
                Op::Idle(deadline) => {
                    let work_before = work;
                    let now = clk.vns(work);
                    let advance = planner.plan(now, deadline);
                    // Planner ⟷ model agreement on the advance amount.
                    let model_advance = model.idle_to(deadline);
                    prop_assert_eq!(advance.advance_vns, model_advance,
                        "planner advance != model advance");
                    clk.advance_idle(advance.advance_vns);

                    // The jump fabricates NO retired branch.
                    prop_assert_eq!(work, work_before, "idle jump changed the work count");
                    // Landed exactly at D for a future deadline; never backward.
                    if deadline >= now {
                        prop_assert_eq!(clk.vns(work), deadline,
                            "idle jump did not land exactly at D");
                    }
                    prop_assert_eq!(clk.vns(work), advance.landed_vns,
                        "clock landed somewhere other than the planner's landed_vns");
                    // The timer would fire: the clock at the frozen work reaches D.
                    prop_assert!(clk.vns(work) >= deadline || clk.vns(work) == u64::MAX,
                        "clock below the deadline after the jump");
                }
            }

            let v = clk.vns(work);
            prop_assert_eq!(v, model.vtime(), "impl diverged from execution+idle model");
            prop_assert!(v >= prev_vtime, "V-time moved backward: {} -> {}", prev_vtime, v);
            prev_vtime = v;
        }
    }

    /// Determinism: the SAME op sequence on two fresh clocks yields a
    /// bit-identical final clock state and landing trace — the planner inputs
    /// are pure, so its outputs are reproducible (the planner-level analogue of
    /// the box "deterministic-twice" gate).
    #[test]
    fn same_ops_are_deterministic_twice(
        ratio_num in 1u64..=4_000,
        ops in proptest::collection::vec(op_strategy(), 1..50),
    ) {
        fn run(ratio_num: u64, ops: &[Op]) -> (Vec<u64>, u64) {
            let cfg = VClockConfig {
                ratio_num, ratio_den: 1, guest_hz: 2_000_000_000, guest_base: 0, vns_base: 0,
            };
            let mut clk = VClock::new(cfg).unwrap();
            let planner = IdlePlanner::new();
            let mut work = 0u64;
            let mut landings = Vec::new();
            for op in ops {
                match op {
                    Op::Execute(b) => work = work.saturating_add(*b),
                    Op::Idle(d) => {
                        let a = planner.plan(clk.vns(work), *d);
                        clk.advance_idle(a.advance_vns);
                        landings.push(clk.vns(work)); // the landed V-time
                    }
                }
            }
            (landings, clk.vns(work))
        }
        let a = run(ratio_num, &ops);
        let b = run(ratio_num, &ops);
        prop_assert_eq!(a.0, b.0, "landing traces differ across identical runs");
        prop_assert_eq!(a.1, b.1, "final V-time differs across identical runs");
    }
}
