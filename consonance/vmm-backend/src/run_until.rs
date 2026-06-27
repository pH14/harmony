// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **portable** orchestration for `Backend::run_until` (§2 inversion seam):
//! drive the pure [`vtime::InjectionPlanner`] over a guest-exit-aware
//! [`PreemptCpu`], and turn its [`PlanOutcome`] into an [`Exit`].
//!
//! The live `CpuBackend` underneath (real `perf_event` overflow + KVM
//! single-step) is box-only (`KvmBackend`, [`crate::kvm_sys`]); this file is the
//! seam *above* it and issues **no syscall**, so it compiles and is unit/property
//! tested on every platform against [`vtime::sim::SimCpu`]. Splitting the
//! orchestration out here is what lets the determinism-critical contract — that
//! `run_until` lands at **exactly** the deadline and is count-neutral with a plain
//! run — be proved on macOS, with only the raw PMU/ioctl wiring deferred to the box.
//!
//! ## Why the planner is wrapped, not driven directly
//!
//! [`vtime::CpuBackend`] (and [`vtime::sim::SimCpu`]) model a *pure* preemption
//! run: execution only ever stops because the armed overflow fired or a step
//! completed, and work always advances to the target. A real vCPU can also take a
//! **genuine guest exit** (IO/MMIO/HLT/MSR/…) *before* the deadline — and that
//! exit must be returned from `run_until` verbatim (short of the deadline), with
//! the backend's normal completion discipline, never swallowed. The planner has
//! no channel for "the guest exited", so [`PreemptCpu`] stashes the exit and the
//! adapter returns the deadline work count from the trait method, which makes the
//! planner stop cleanly ([`PlanOutcome::ReadyToInject`]); [`drive_run_until`] then
//! checks [`PreemptCpu::take_guest_exit`] and prefers the stashed exit.

use crate::error::{BackendError, Result};
use crate::exit::Exit;
use crate::types::Vtime;
use vtime::{CpuBackend, InjectionPlanner, PlanOutcome, VtimeError};

/// The arm-early margin (work units), in branches, consumed from task 07's
/// **measured** PMU skid (`docs/ROADMAP.md`: `skid_margin=128`, PR #20). The
/// overflow is armed at `deadline − SKID_MARGIN`, so that `armed_at + worst-case
/// skid` (the PMI/signal-delivery latency, all counted as skid) still lands at or
/// before the deadline; the remaining branches are covered by exact single-stepping.
/// It MUST exceed the box's worst-case skid or [`drive_run_until`] surfaces a loud
/// determinism error rather than silently injecting late.
pub(crate) const SKID_MARGIN: u64 = 128;

/// A [`vtime::CpuBackend`] that can also surface a **genuine guest exit** taken
/// before the deadline (and recover the typed backend error the opaque
/// [`VtimeError::Backend`] cannot carry across the pure planner).
///
/// The live impl is `KvmBackend`'s box-only adapter; the tests use a
/// [`vtime::sim::SimCpu`] wrapper.
pub(crate) trait PreemptCpu: CpuBackend {
    /// Take the genuine guest exit captured during the most recent
    /// `run_until_overflow`/`single_step`, **with the real work count at that
    /// exit**, if one occurred. When `Some`, the work value those calls returned to
    /// the planner is a sentinel (the deadline, to stop it) — the *real* stop is
    /// this exit at `work`. [`drive_run_until`] compares `work` to the deadline:
    /// only an exit at `work < deadline` is genuinely early; one at `work >=
    /// deadline` (the SIGIO-delay race — the overflow already reached the deadline
    /// before a natural exit surfaced) is the deadline, not an early exit (P1(a)).
    fn take_guest_exit(&mut self) -> Option<(Exit, u64)>;

    /// Take the typed [`BackendError`] behind the most recent
    /// [`VtimeError::Backend`] (a failed syscall), so `run_until` returns the real
    /// errno rather than a stringified placeholder. `None` if the last failure had
    /// no typed cause.
    fn take_error(&mut self) -> Option<BackendError>;
}

/// Drive `cpu` to **exactly** `deadline` retired-branch work units via the
/// arm-overflow-early → single-step planner, then map the outcome to an [`Exit`]:
///
/// - a genuine guest exit **strictly before** the deadline → **that** exit (short
///   of `deadline`);
/// - a guest exit at `work == deadline` (the SIGIO-delay race: the overflow reached
///   the deadline before the natural exit surfaced) → [`Exit::Deadline`] at the
///   deadline, **not** an early exit — the timer instant was reached, so the timer
///   takes precedence (P1(a));
/// - a guest exit at `work > deadline` → a loud [`BackendError::Internal`]: the
///   free-run executed *past* the exact V-time injection point, a determinism
///   violation (the overflow skid exceeded the margin), reported, never absorbed;
/// - no guest exit → [`Exit::Deadline`] at exactly `deadline` (or at `now`, if the
///   deadline was already in the past when `run_until` was entered — the timer is
///   overdue, deliver it immediately; never *past* a future deadline);
/// - a backend syscall failure → its typed [`BackendError`];
/// - [`VtimeError::SkidExceeded`] (the overflow overshot the margin with no guest
///   exit) → a loud [`BackendError::Internal`], same posture.
///
/// Issues no syscall; all I/O is inside `cpu`'s trait methods.
pub(crate) fn drive_run_until<C: PreemptCpu>(
    planner: &InjectionPlanner,
    cpu: &mut C,
    deadline: u64,
) -> Result<Exit> {
    match planner.stop_at(cpu, deadline) {
        Ok(PlanOutcome::ReadyToInject { stopped_at, .. }) => match cpu.take_guest_exit() {
            // A natural guest exit BEFORE the deadline: return it (short of
            // `deadline`), pending-completion already armed on the backend like `run`.
            Some((exit, work)) if work < deadline => Ok(exit),
            // P1(a): a natural guest exit at-or-past the deadline is NOT early — the
            // overflow reached the deadline first (its SIGIO just hadn't landed). At
            // exactly the deadline the timer instant is reached → preempt (the exit is
            // absorbed; `run_until` clears the pending). Past it, the free-run ran
            // beyond the exact injection point → a loud determinism error.
            Some((_exit, work)) if work == deadline => Ok(Exit::Deadline {
                reached: Vtime(deadline),
            }),
            Some((_exit, work)) => {
                debug_assert!(work > deadline);
                Err(BackendError::Internal(
                    "run_until: a guest exit landed past the deadline (overflow skid exceeded the \
                     margin) — the exact V-time injection point was missed",
                ))
            }
            // No guest exit: the planner landed at exactly `deadline` branches.
            None => Ok(Exit::Deadline {
                reached: Vtime(stopped_at),
            }),
        },
        // The deadline was already passed when we were called — the timer is
        // overdue. Deliver at once (reached ≥ deadline); never silently absorbed.
        Ok(PlanOutcome::TargetInPast { now, .. }) => Ok(Exit::Deadline {
            reached: Vtime(now),
        }),
        // A genuine guest exit can also be stashed alongside a planner *error* path
        // only via the backend; here the planner returned an error, so prefer the
        // typed backend error when the cause was a syscall.
        Err(VtimeError::Backend(_)) => Err(cpu
            .take_error()
            .unwrap_or(BackendError::Internal("run_until: cpu backend failure"))),
        // Skid past the target despite the margin: a determinism hazard. Loud.
        Err(VtimeError::SkidExceeded { .. }) => Err(BackendError::Internal(
            "run_until: PMU skid exceeded the configured margin (determinism hazard)",
        )),
        // The remaining `VtimeError`s are VClock/sim-config faults that cannot arise
        // here (no clock is built in this path); fail closed if one ever does.
        Err(_) => Err(BackendError::Internal("run_until: planner error")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use vtime::PlannerConfig;
    use vtime::sim::{SimCpu, SimCpuConfig};

    /// The sentinel guest exit the test wrapper injects (any exit shape works; an
    /// `Io` IN is representative of a read-style exit that must survive `run_until`).
    const GUEST_EXIT: Exit = Exit::Io {
        port: 0x3F8,
        size: 1,
        write: None,
    };

    /// A [`PreemptCpu`] over [`SimCpu`]: optionally injects a guest exit the first
    /// time work crosses `guest_exit_at`, modelling a natural VM-exit mid-preemption.
    struct SimPreempt {
        inner: SimCpu,
        guest_exit_at: Option<u64>,
        deadline: u64,
        /// The stashed (exit, real-work-at-exit) — see [`PreemptCpu::take_guest_exit`].
        pending_exit: Option<(Exit, u64)>,
        fail: bool,
    }

    impl SimPreempt {
        fn new(cfg: SimCpuConfig, deadline: u64) -> Self {
            Self {
                inner: SimCpu::new(cfg).expect("valid sim config"),
                guest_exit_at: None,
                deadline,
                pending_exit: None,
                fail: false,
            }
        }
        /// Inject a guest exit the first time work reaches `at`.
        fn with_guest_exit(mut self, at: u64) -> Self {
            self.guest_exit_at = Some(at);
            self
        }
        /// Make every backend call fail (drives the error path).
        fn failing(mut self) -> Self {
            self.fail = true;
            self
        }
        /// Stash a guest exit **with its real work count** + return the deadline
        /// sentinel iff work crossed the threshold; else return the real work count.
        fn maybe_exit(&mut self, work: u64) -> u64 {
            if let Some(at) = self.guest_exit_at
                && work >= at
                && self.pending_exit.is_none()
            {
                self.pending_exit = Some((GUEST_EXIT, work));
                return self.deadline;
            }
            work
        }
    }

    impl CpuBackend for SimPreempt {
        fn work(&self) -> u64 {
            self.inner.work()
        }
        fn run_until_overflow(
            &mut self,
            armed_at: u64,
        ) -> std::result::Result<u64, vtime::BackendError> {
            if self.fail {
                return Err(vtime::BackendError::new("scripted failure"));
            }
            let stopped = self.inner.run_until_overflow(armed_at)?;
            Ok(self.maybe_exit(stopped))
        }
        fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
            if self.fail {
                return Err(vtime::BackendError::new("scripted failure"));
            }
            // Once a guest exit is stashed, stop advancing (the planner is told we
            // already reached the deadline) — mirrors the live adapter never
            // re-entering after a pending-completion exit.
            if self.pending_exit.is_some() {
                return Ok(self.deadline);
            }
            let w = self.inner.single_step()?;
            Ok(self.maybe_exit(w))
        }
    }

    impl PreemptCpu for SimPreempt {
        fn take_guest_exit(&mut self) -> Option<(Exit, u64)> {
            self.pending_exit.take()
        }
        fn take_error(&mut self) -> Option<BackendError> {
            // The sim's failures are opaque; model "no typed cause" so the caller
            // falls back to the Internal placeholder.
            None
        }
    }

    /// A minimal [`PreemptCpu`] that stashes a guest exit at a **fixed** work count
    /// on the first `run_until_overflow`, to test [`drive_run_until`]'s P1(a) decision
    /// (early vs at-deadline vs past-deadline) directly, independent of the planner's
    /// stepping. Models the SIGIO-delay race: a natural exit surfaced at `work_at_exit`.
    struct ExitAtCpu {
        work_at_exit: u64,
        stashed: Option<(Exit, u64)>,
    }
    impl CpuBackend for ExitAtCpu {
        fn work(&self) -> u64 {
            0
        }
        fn run_until_overflow(
            &mut self,
            _armed_at: u64,
        ) -> std::result::Result<u64, vtime::BackendError> {
            self.stashed = Some((GUEST_EXIT, self.work_at_exit));
            Ok(self.deadline_sentinel())
        }
        fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
            Ok(self.deadline_sentinel())
        }
    }
    impl ExitAtCpu {
        // The sentinel the live `LiveCpu` adapter returns on a guest exit: EXACTLY the
        // deadline, so the planner always reaches ReadyToInject (never its own
        // SkidExceeded) and `drive_run_until` makes the real early/at/past decision
        // from the stashed work.
        fn deadline_sentinel(&self) -> u64 {
            EXIT_AT_DEADLINE
        }
    }
    impl PreemptCpu for ExitAtCpu {
        fn take_guest_exit(&mut self) -> Option<(Exit, u64)> {
            self.stashed.take()
        }
        fn take_error(&mut self) -> Option<BackendError> {
            None
        }
    }
    /// The deadline used by the P1(a) decision test.
    const EXIT_AT_DEADLINE: u64 = 1_000_000;

    /// P1(a): a guest exit at `work >= deadline` (overflow reached the deadline before
    /// the natural exit surfaced — the SIGIO-delay race) is the **deadline**, not an
    /// early exit; past the deadline it is a loud determinism error; only `< deadline`
    /// is genuinely early.
    #[test]
    fn guest_exit_at_or_past_deadline_is_not_treated_as_early() {
        let d = EXIT_AT_DEADLINE;
        // strictly before → the guest exit is returned (genuinely early).
        let mut early = ExitAtCpu {
            work_at_exit: d - 1,
            stashed: None,
        };
        assert_eq!(
            drive_run_until(&planner(), &mut early, d).unwrap(),
            GUEST_EXIT,
            "a guest exit before the deadline is returned as the early exit"
        );
        // exactly at the deadline → Deadline (the timer instant was reached).
        let mut at = ExitAtCpu {
            work_at_exit: d,
            stashed: None,
        };
        assert_eq!(
            drive_run_until(&planner(), &mut at, d).unwrap(),
            Exit::Deadline { reached: Vtime(d) },
            "a guest exit AT the deadline is the Deadline, not an early exit (P1(a))"
        );
        // past the deadline → loud determinism error (the exact instant was missed).
        let mut past = ExitAtCpu {
            work_at_exit: d + 5,
            stashed: None,
        };
        match drive_run_until(&planner(), &mut past, d) {
            Err(BackendError::Internal(msg)) => assert!(
                msg.contains("past the deadline"),
                "the error names the past-deadline overshoot: {msg}"
            ),
            other => panic!("a guest exit past the deadline must be a loud error, got {other:?}"),
        }
    }

    fn planner() -> InjectionPlanner {
        InjectionPlanner::new(PlannerConfig {
            skid_margin: SKID_MARGIN,
        })
    }

    /// Proptest config: far fewer cases under Miri (10–100× slower interpreted), and
    /// **no failure-persistence** there (its regression-file path resolution uses
    /// `getcwd`, which Miri's fs isolation rejects). Mirrors the crate's other
    /// proptest helpers (`tests/run_loop.rs`).
    fn cases(native: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 8 } else { native });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    #[test]
    fn lands_exactly_at_deadline_with_no_guest_exit() {
        // A representative spread of densities + skids (skid ≤ margin).
        for &(seed, num, den, skid) in &[
            (1u64, 1u64, 1u64, 0u64),
            (2, 1, 3, 7),
            (3, 1, 1000, 64),
            (4, 1, 10, 127),
        ] {
            let mut cpu = SimPreempt::new(
                SimCpuConfig {
                    seed,
                    density_num: num,
                    density_den: den,
                    max_skid: skid,
                    initial_work: 0,
                },
                10_000,
            );
            let exit = drive_run_until(&planner(), &mut cpu, 10_000).expect("run_until");
            assert_eq!(
                exit,
                Exit::Deadline {
                    reached: Vtime(10_000)
                },
                "must land at EXACTLY the deadline (count-neutral), seed {seed}"
            );
            assert_eq!(
                cpu.work(),
                10_000,
                "the live work counter is at the deadline"
            );
        }
    }

    #[test]
    fn returns_the_guest_exit_when_one_occurs_before_the_deadline() {
        let mut cpu = SimPreempt::new(
            SimCpuConfig {
                seed: 9,
                density_num: 1,
                density_den: 4,
                max_skid: 16,
                initial_work: 0,
            },
            100_000,
        )
        .with_guest_exit(40_000);
        let exit = drive_run_until(&planner(), &mut cpu, 100_000).expect("run_until");
        assert_eq!(exit, GUEST_EXIT, "the natural guest exit must be returned");
        assert!(
            cpu.work() < 100_000,
            "the guest exit is SHORT of the deadline, never past it (got {})",
            cpu.work()
        );
    }

    #[test]
    fn target_in_past_delivers_immediately() {
        let mut cpu = SimCpuConfig {
            seed: 5,
            density_num: 1,
            density_den: 1,
            max_skid: 0,
            initial_work: 0,
        };
        cpu.initial_work = 500; // already past the deadline 100
        let mut p = SimPreempt::new(cpu, 100);
        let exit = drive_run_until(&planner(), &mut p, 100).expect("run_until");
        assert_eq!(
            exit,
            Exit::Deadline {
                reached: Vtime(500)
            },
            "an overdue deadline delivers at once (reached = now ≥ deadline)"
        );
    }

    #[test]
    fn skid_past_margin_is_a_loud_determinism_error() {
        // max_skid (200) deliberately exceeds SKID_MARGIN (128): the overflow can
        // overshoot the target, which MUST surface loudly, not be tolerated.
        let mut saw_skid_error = false;
        for seed in 0..64u64 {
            let mut cpu = SimPreempt::new(
                SimCpuConfig {
                    seed,
                    density_num: 1,
                    density_den: 1,
                    max_skid: 200,
                    initial_work: 0,
                },
                10_000,
            );
            if let Err(BackendError::Internal(msg)) = drive_run_until(&planner(), &mut cpu, 10_000)
            {
                assert!(
                    msg.contains("skid"),
                    "the error names the skid hazard: {msg}"
                );
                saw_skid_error = true;
                break;
            }
        }
        assert!(
            saw_skid_error,
            "an over-margin skid must eventually surface as a loud error"
        );
    }

    #[test]
    fn backend_failure_surfaces_as_an_error() {
        let mut cpu = SimPreempt::new(
            SimCpuConfig {
                seed: 1,
                density_num: 1,
                density_den: 1,
                max_skid: 0,
                initial_work: 0,
            },
            10_000,
        )
        .failing();
        let err = drive_run_until(&planner(), &mut cpu, 10_000).expect_err("must error");
        assert!(matches!(err, BackendError::Internal(_)));
    }

    proptest! {
        #![proptest_config(cases(256))]

        /// THE count-neutrality + exactness property (gate 1): for any seed, event
        /// density, and skid within the margin, the arm-overflow-then-single-step
        /// `run_until` lands at **exactly** the deadline. Because `SimCpu` retires
        /// the same instruction stream whether free-running (`run_until_overflow`)
        /// or single-stepping, landing at the exact target — regardless of where
        /// the (adversarially-drawn) skid fell — *is* the count-neutrality proof:
        /// the preemption instant is a pure function of the seed, not of the skid.
        /// Deadlines/densities are bounded so the suite stays well under the ~3-min
        /// budget (the live PMU's count-neutrality is the box gate). Both the
        /// long-distance (overflow + step) and short-distance (step-only) regimes
        /// are covered since `deadline` straddles `SKID_MARGIN`.
        #[test]
        fn run_until_is_count_neutral_and_exact(
            seed in 1u64..=u64::MAX,
            density_num in 1u64..=8,
            extra_den in 0u64..=24,
            max_skid in 0u64..=SKID_MARGIN,
            deadline in 1u64..=4_000,
        ) {
            let density_den = density_num + extra_den; // ensures num <= den
            let cfg = SimCpuConfig { seed, density_num, density_den, max_skid, initial_work: 0 };
            let mut cpu = SimPreempt::new(cfg, deadline);
            let exit = drive_run_until(&planner(), &mut cpu, deadline)
                .expect("run_until on an in-margin skid");
            prop_assert_eq!(exit, Exit::Deadline { reached: Vtime(deadline) });
            prop_assert_eq!(cpu.work(), deadline);
        }
    }
}
