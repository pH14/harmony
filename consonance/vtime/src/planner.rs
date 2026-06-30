// SPDX-License-Identifier: AGPL-3.0-or-later
//! The precise-injection planner: [`CpuBackend`], [`InjectionPlanner`].

use crate::error::{BackendError, VtimeError};

/// What the planner asks of the CPU/counter backend.
///
/// The real implementation (perf_event + KVM single-step) comes later; tests
/// use [`crate::sim::SimCpu`]. In the real backend, `work()` reads the
/// retired-conditional-branch counter at the current VM exit,
/// `run_until_overflow` programs a perf_event overflow interrupt and re-enters
/// the guest, and `single_step` runs one instruction under
/// `KVM_GUESTDBG_SINGLESTEP` (see the crate docs).
pub trait CpuBackend {
    /// Current work count (e.g. the counter read at the current VM exit).
    fn work(&self) -> u64;
    /// Arm an overflow interrupt at the given absolute work count, then run.
    /// Returns the work count at which execution actually stopped:
    /// guaranteed `>=` the armed count, overshoot bounded by the backend's
    /// skid.
    ///
    /// # Errors
    ///
    /// Backend-specific failure (in the real backend: a failed syscall).
    fn run_until_overflow(&mut self, armed_at: u64) -> Result<u64, BackendError>;
    /// Execute exactly one INSTRUCTION (not one work unit). Returns the new
    /// work count, which advances by 0 or 1 — most instructions are not
    /// counted events.
    ///
    /// # Errors
    ///
    /// Backend-specific failure (in the real backend: a failed syscall).
    fn single_step(&mut self) -> Result<u64, BackendError>;
}

/// Configuration for an [`InjectionPlanner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannerConfig {
    /// Arm the counter this many work units BEFORE the target, so that
    /// `armed_at + worst-case skid` still lands before the target. Must be
    /// greater than the backend's maximum skid, or [`stop_at`]
    /// (`InjectionPlanner::stop_at`) will report
    /// [`VtimeError::SkidExceeded`].
    ///
    /// [`stop_at`]: InjectionPlanner::stop_at
    pub skid_margin: u64,
}

/// Result of a successful [`InjectionPlanner::stop_at`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOutcome {
    /// Stopped exactly at `target`; the caller may now inject the interrupt.
    ReadyToInject {
        /// The work count that was asked for (and reached exactly).
        target: u64,
        /// Where execution stopped; always equal to `target` in this variant.
        stopped_at: u64,
        /// Number of INSTRUCTIONS stepped; it is bounded only by the guest's
        /// event density, NOT by `skid_margin` (a long branch-free stretch
        /// may need many steps per counted event). The `skid_margin` bound
        /// applies to the *counted-event distance* covered by stepping.
        single_steps_used: u64,
    },
    /// Target already passed when planning started (caller bug or missed
    /// deadline): reported, never silently absorbed.
    TargetInPast {
        /// The work count that was asked for.
        target: u64,
        /// The backend's work count at the time of the call (`> target`).
        now: u64,
    },
}

/// Drives a [`CpuBackend`] so that it stops at an exact work count, using the
/// arm-early-then-single-step strategy described in the crate docs.
#[derive(Debug, Clone)]
pub struct InjectionPlanner {
    cfg: PlannerConfig,
}

impl InjectionPlanner {
    /// Creates a planner with the given config.
    pub fn new(cfg: PlannerConfig) -> Self {
        InjectionPlanner { cfg }
    }

    /// Drive `backend` so it stops exactly at `work == target`:
    ///
    /// - if `target == now`: [`PlanOutcome::ReadyToInject`] immediately, zero
    ///   steps (the normal case right after an idle warp made a deadline
    ///   current);
    /// - if `target - now > skid_margin`: arm at `target - skid_margin`,
    ///   run to the overflow (which, with `skid_margin` STRICTLY above the
    ///   worst-case skid, stops STRICTLY BEFORE `target`), then single-step
    ///   (instruction by instruction) the rest of the way to `target` — so the
    ///   landing is always positioned by the exact single-step, never by the
    ///   instruction-imprecise overflow;
    /// - if `0 < target - now <= skid_margin`: single-step the whole way
    ///   (again: loop until WORK reaches `target`, not a fixed number of
    ///   steps);
    /// - if `target < now`: [`PlanOutcome::TargetInPast`].
    ///
    /// Termination relies on the backend's contract: work is monotonic,
    /// `single_step` advances it by 0 or 1, and counted events keep
    /// occurring. A guest that never retires another counted event would
    /// step forever — exactly as on real hardware, where such a deadline
    /// work count is simply never reached.
    ///
    /// # Errors
    ///
    /// - [`VtimeError::SkidExceeded`] if the overflow stops AT or PAST the
    ///   target (`stopped >= target`) — the skid consumed the whole margin, so
    ///   no room is left for the precise single-step and the overflow's
    ///   instruction-imprecise stop would otherwise be injected raw. This is a
    ///   determinism-destroying event and is loud, never papered over.
    ///   (Defensively, a backend whose `single_step` violates the 0-or-1
    ///   contract and jumps past the target is reported the same way.)
    /// - [`VtimeError::Backend`] if a backend call fails.
    pub fn stop_at(
        &self,
        backend: &mut dyn CpuBackend,
        target: u64,
    ) -> Result<PlanOutcome, VtimeError> {
        let now = backend.work();
        if target < now {
            return Ok(PlanOutcome::TargetInPast { target, now });
        }

        // Phase 1: if the target is farther away than the skid margin, let
        // the counter overflow carry us most of the way. With `skid_margin`
        // STRICTLY greater than the worst-case skid, `armed_at + skid` lands in
        // [target - skid_margin, target) — i.e. STRICTLY BEFORE the target — so
        // Phase 2's single-step always runs and walks precisely to the exact
        // instruction boundary at `target`.
        //
        // The overflow (perf/SIGIO) is NOT instruction-precise at the boundary:
        // non-counted instructions after the target's counted event can already
        // have retired while the counter still reads `== target`. So an overflow
        // that stops at `stopped >= target` is a SKID VIOLATION (the margin failed
        // to leave room for the precise single-step) — reported loudly via
        // [`VtimeError::SkidExceeded`], NEVER accepted as a (raw, imprecise)
        // landing. This is the precision invariant: every `ReadyToInject` reached
        // through Phase 1 has been positioned by the exact single-step phase.
        let mut current = now;
        let mut armed_at = now; // diagnostic value when nothing is armed
        if target - now > self.cfg.skid_margin {
            let arm = target - self.cfg.skid_margin;
            let stopped = backend.run_until_overflow(arm)?;
            if stopped >= target {
                eprintln!("DIAG-SKID49 phase=1-OVERFLOW armed_at={} target={} stopped_at={} over={} skid_from_armed={}", arm, target, stopped, stopped.saturating_sub(target), stopped.saturating_sub(arm));
                return Err(VtimeError::SkidExceeded {
                    armed_at: arm,
                    target,
                    stopped_at: stopped,
                });
            }
            armed_at = arm;
            current = stopped;
        }

        // Phase 2: single-step to the exact target, checking the work count
        // at every instruction boundary. The counted-event distance covered
        // here is at most skid_margin; the instruction count is unbounded by
        // it (sparse streams step many instructions per counted event).
        let mut single_steps_used: u64 = 0;
        while current < target {
            current = backend.single_step()?;
            single_steps_used += 1;
        }
        if current > target {
            eprintln!("DIAG-SKID49 phase=2-SINGLESTEP armed_at={} target={} stopped_at={} over={}", armed_at, target, current, current.saturating_sub(target));
            return Err(VtimeError::SkidExceeded {
                armed_at,
                target,
                stopped_at: current,
            });
        }
        Ok(PlanOutcome::ReadyToInject {
            target,
            stopped_at: current,
            single_steps_used,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal hand-rolled backend for trait-level planner tests; the full
    /// adversarial simulator lives in `crate::sim`.
    struct ScriptedCpu {
        work: u64,
        /// Work delta applied by each successive `single_step` call.
        step_deltas: Vec<u64>,
        steps_taken: usize,
        /// Forced stop position for `run_until_overflow`.
        overflow_stops_at: Option<u64>,
        fail: bool,
    }

    impl CpuBackend for ScriptedCpu {
        fn work(&self) -> u64 {
            self.work
        }
        fn run_until_overflow(&mut self, armed_at: u64) -> Result<u64, BackendError> {
            if self.fail {
                return Err(BackendError::new("scripted failure"));
            }
            self.work = self.overflow_stops_at.unwrap_or(armed_at);
            Ok(self.work)
        }
        fn single_step(&mut self) -> Result<u64, BackendError> {
            if self.fail {
                return Err(BackendError::new("scripted failure"));
            }
            let delta = self.step_deltas.get(self.steps_taken).copied().unwrap_or(1);
            self.steps_taken += 1;
            self.work += delta;
            Ok(self.work)
        }
    }

    fn cpu(work: u64) -> ScriptedCpu {
        ScriptedCpu {
            work,
            step_deltas: vec![],
            steps_taken: 0,
            overflow_stops_at: None,
            fail: false,
        }
    }

    #[test]
    fn target_equals_now_is_immediate() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 8 });
        let mut backend = cpu(42);
        let outcome = planner.stop_at(&mut backend, 42).unwrap();
        assert_eq!(
            outcome,
            PlanOutcome::ReadyToInject {
                target: 42,
                stopped_at: 42,
                single_steps_used: 0
            }
        );
        assert_eq!(backend.steps_taken, 0);
    }

    #[test]
    fn target_in_past_is_reported() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 8 });
        let mut backend = cpu(100);
        let outcome = planner.stop_at(&mut backend, 99).unwrap();
        assert_eq!(
            outcome,
            PlanOutcome::TargetInPast {
                target: 99,
                now: 100
            }
        );
    }

    #[test]
    fn short_distance_steps_only() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 8 });
        // 0-advance steps interleaved: loop must continue until WORK reaches
        // the target, not for a fixed number of steps.
        let mut backend = ScriptedCpu {
            work: 10,
            step_deltas: vec![1, 0, 0, 1, 0, 1],
            steps_taken: 0,
            overflow_stops_at: None,
            fail: false,
        };
        let outcome = planner.stop_at(&mut backend, 13).unwrap();
        assert_eq!(
            outcome,
            PlanOutcome::ReadyToInject {
                target: 13,
                stopped_at: 13,
                single_steps_used: 6
            }
        );
    }

    #[test]
    fn long_distance_arms_then_steps() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 4 });
        let mut backend = cpu(0);
        backend.overflow_stops_at = Some(98); // armed at 96, skid 2
        let outcome = planner.stop_at(&mut backend, 100).unwrap();
        assert_eq!(
            outcome,
            PlanOutcome::ReadyToInject {
                target: 100,
                stopped_at: 100,
                single_steps_used: 2
            }
        );
    }

    #[test]
    fn overshoot_after_arm_is_skid_exceeded() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 4 });
        let mut backend = cpu(0);
        backend.overflow_stops_at = Some(103); // skid 7 > margin 4
        let err = planner.stop_at(&mut backend, 100).unwrap_err();
        assert_eq!(
            err,
            VtimeError::SkidExceeded {
                armed_at: 96,
                target: 100,
                stopped_at: 103
            }
        );
    }

    /// The precision invariant: an overflow landing EXACTLY on the target (skid ==
    /// margin) is a SkidExceeded violation, NOT a raw landing — the overflow is
    /// instruction-imprecise at the boundary, so the exact single-step must always
    /// finish the walk. `stopped == target` leaves it no room → loud error.
    #[test]
    fn overflow_exactly_on_target_is_skid_exceeded() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 4 });
        let mut backend = cpu(0);
        backend.overflow_stops_at = Some(100); // skid == margin 4 → stops AT the target
        let err = planner.stop_at(&mut backend, 100).unwrap_err();
        assert_eq!(
            err,
            VtimeError::SkidExceeded {
                armed_at: 96,
                target: 100,
                stopped_at: 100
            }
        );
    }

    /// The complementary case: an overflow strictly before the target (skid < margin)
    /// IS single-stepped precisely to the exact boundary (`single_steps_used > 0`).
    #[test]
    fn overflow_one_before_target_single_steps_to_exact() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 4 });
        let mut backend = cpu(0);
        backend.overflow_stops_at = Some(99); // skid 3 < margin 4 → one short
        let outcome = planner.stop_at(&mut backend, 100).unwrap();
        assert_eq!(
            outcome,
            PlanOutcome::ReadyToInject {
                target: 100,
                stopped_at: 100,
                single_steps_used: 1
            }
        );
    }

    #[test]
    fn contract_violating_step_is_loud() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 8 });
        let mut backend = cpu(10);
        backend.step_deltas = vec![5]; // violates the 0-or-1 contract
        let err = planner.stop_at(&mut backend, 13).unwrap_err();
        assert_eq!(
            err,
            VtimeError::SkidExceeded {
                armed_at: 10,
                target: 13,
                stopped_at: 15
            }
        );
    }

    #[test]
    fn backend_errors_propagate() {
        let planner = InjectionPlanner::new(PlannerConfig { skid_margin: 4 });
        let mut backend = cpu(0);
        backend.fail = true;
        let err = planner.stop_at(&mut backend, 100).unwrap_err();
        assert_eq!(
            err,
            VtimeError::Backend(BackendError::new("scripted failure"))
        );
    }
}
