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
/// arm-overflow-early → single-step planner, then map the outcome to an [`Exit`].
///
/// **The complete boundary rule (P1 round-4).** At the exact deadline a
/// `pmu_work() == deadline` read does NOT by itself say whether the guest stopped
/// AT the deadline branch or ran one more *non-counted* instruction past it — an
/// IO/MMIO/HLT/read-style instruction retires no conditional branch, so it can
/// execute (and commit a guest-visible side effect) while the counter still reads
/// `deadline`. So the decision turns on **whether a guest exit was reported**, not on
/// the count alone:
///
/// - guest exit, `work < deadline` → **deliver** it (genuinely early; carries
///   guest-visible PIO/MMIO/read state, never dropped);
/// - **no** guest exit (the single-step stopped AT the deadline branch — nothing ran
///   past it) → [`Exit::Deadline`]: the **timer wins**. The post-deadline instruction
///   runs on the *next* entry, AFTER the timer ISR — its side effect is not yet
///   committed, so nothing is lost and the boundary is host-timing-independent;
/// - guest exit, `work == deadline` → **fail closed** (loud): a non-counted
///   instruction past the deadline branch already executed and its exit was
///   reported/consumed (side effect committed). Turning it into `Deadline` would drop
///   that guest-visible state and resume past it. The single-step-to-exact invariant
///   was violated;
/// - guest exit, `work > deadline` → **fail closed** (loud): counted instructions ran
///   past the branch (a worse overshoot) — same posture.
///
/// PRIMARY structural guarantee: with `skid_margin > max_skid` the free-run stops
/// STRICTLY before the deadline branch and the single-step lands exactly ON it
/// (stopping before the next instruction). So a non-counted post-deadline instruction
/// is **never free-run-executed** — the two fail-closed arms are unreachable in normal
/// operation; they are the loud backstop if that invariant is ever violated. (A
/// snapshot taken at a returned `Deadline` is therefore exact: nothing ran past the
/// branch, no pending completion is held.)
///
/// Also: `TargetInPast` (deadline already past on entry) → an overdue `Deadline` at
/// `now`; a backend syscall failure → its typed [`BackendError`];
/// [`VtimeError::SkidExceeded`] → a loud [`BackendError::Internal`].
///
/// Issues no syscall; all I/O is inside `cpu`'s trait methods.
pub(crate) fn drive_run_until<C: PreemptCpu>(
    planner: &InjectionPlanner,
    cpu: &mut C,
    deadline: u64,
) -> Result<Exit> {
    match planner.stop_at(cpu, deadline) {
        Ok(PlanOutcome::ReadyToInject { stopped_at, .. }) => match cpu.take_guest_exit() {
            // P1(a): classify the guest exit by its real work count vs the deadline —
            // the determinism-critical decision, a pure comparison kept HERE in the
            // covered + mutation-tested portable layer (the box `LiveCpu` is the thin
            // FFI that only *reports* the PMU read). The planner (vtime) handles the
            // work-vs-target stepping; "guest exit" is not a vtime concept, so its
            // disposition lives in this seam.
            Some((exit, work)) => match classify_guest_exit(work, deadline) {
                // work < deadline: a real exit genuinely BEFORE the deadline —
                // **deliver** it (a PIO/MMIO write or read-style exit short of the
                // timer count carries guest-visible state; never dropped). Pending-
                // completion is already armed on the backend, exactly like a plain
                // `run`, so the VMM services it and resumes.
                GuestExitDisposition::Early => Ok(exit),
                // work == deadline WITH a reported exit: a non-counted instruction past
                // the deadline branch already executed and committed its side effect
                // (the counter does not advance on it, so the count alone can't see
                // this — only the *presence* of the exit does). FAIL CLOSED (P1
                // round-4): never silently turn it into Deadline (that would DROP the
                // committed effect and resume past it). The single-step-to-exact
                // invariant was violated — the timer-wins `Deadline` is the *no-exit*
                // case below (the single-step stopped AT the branch).
                GuestExitDisposition::AtDeadline => Err(BackendError::Internal(
                    "run_until: a guest exit was reported at exactly the deadline — a non-counted \
                     instruction past the deadline branch already executed (side effect committed); \
                     the single-step-to-exact invariant was violated",
                )),
                // work > deadline: counted instructions ran BEYOND the exact branch — a
                // worse overshoot. The single-step never overshoots, so this is
                // impossible unless skid grossly exceeded the margin — fail closed
                // (loud), never absorb or deliver-late.
                GuestExitDisposition::PastDeadline => Err(BackendError::Internal(
                    "run_until: a guest exit landed past the deadline (overflow skid exceeded the \
                     margin) — the exact V-time injection point was missed",
                )),
            },
            // No guest exit: the single-step stopped AT the deadline branch — nothing
            // ran past it, so the TIMER WINS. The post-deadline instruction runs on the
            // next entry, after the timer ISR (host-timing-independent; nothing lost).
            None => Ok(Exit::Deadline {
                reached: Vtime(stopped_at),
            }),
        },
        // The deadline was already passed when we were called — the timer is
        // overdue. Deliver at once (reached ≥ deadline); never silently absorbed.
        Ok(PlanOutcome::TargetInPast { now, .. }) => Ok(Exit::Deadline {
            reached: Vtime(now),
        }),
        // Skid past the target despite the margin: a determinism hazard. Loud.
        Err(VtimeError::SkidExceeded { .. }) => Err(BackendError::Internal(
            "run_until: PMU skid exceeded the configured margin (determinism hazard)",
        )),
        // The only other error `stop_at` returns is `VtimeError::Backend` (a cpu
        // syscall failure) — recover its typed error. (The remaining `VtimeError`s
        // are VClock/sim-config faults that cannot arise here, since no clock is
        // built in this path; they fall through to the same fail-closed default.)
        // One arm, so it stays covered by the backend-failure test rather than
        // splitting off an unreachable catch-all.
        Err(_) => Err(cpu
            .take_error()
            .unwrap_or(BackendError::Internal("run_until: planner error"))),
    }
}

/// The disposition of a genuine guest exit relative to the requested deadline — the
/// P1(a) decision, isolated as a pure comparison so it is covered, mutation-tested,
/// and property-tested (the box-only FFI never makes this call).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum GuestExitDisposition {
    /// `work_at_exit < deadline`: a true early exit — delivered to the VMM.
    Early,
    /// `work_at_exit == deadline` **with a reported guest exit**: a non-counted
    /// instruction past the deadline branch already executed (the count can't see it,
    /// only the exit's presence does) → **fail closed** (P1 round-4). The timer-wins
    /// `Exit::Deadline` is the *no-exit* case (single-step stopped AT the branch), not
    /// this one. Should-never-happen with `skid_margin > max_skid`.
    AtDeadline,
    /// `work_at_exit > deadline`: the free-run executed past the exact injection
    /// point → a loud determinism error (fail closed).
    PastDeadline,
}

/// Classify a guest exit at `work_at_exit` against `deadline`. Pure arithmetic.
pub(crate) fn classify_guest_exit(work_at_exit: u64, deadline: u64) -> GuestExitDisposition {
    match work_at_exit.cmp(&deadline) {
        std::cmp::Ordering::Less => GuestExitDisposition::Early,
        std::cmp::Ordering::Equal => GuestExitDisposition::AtDeadline,
        std::cmp::Ordering::Greater => GuestExitDisposition::PastDeadline,
    }
}

/// The free-run phase's decision after **any non-guest-exit** `KVM_RUN` stop — a
/// signal kick (EINTR / `KVM_EXIT_INTR`) **or** an `KVM_EXIT_IRQ_WINDOW_OPEN`
/// control exit: has the overflow reached the armed count?
///
/// `Some(work)` ⇒ stop the free-run here (the overflow fired; hand off to the
/// single-step phase). `None` ⇒ re-enter the guest (a spurious pre-overflow signal,
/// or an IRQ-window re-entry). Applying this UNIFORMLY to every such stop — not just
/// the signal path — is the round-2 P1(b) fix: an IRQ-window re-entry that ignores a
/// crossed overflow would overshoot the exact preemption point. Pure (the box code
/// supplies the real `pmu_work()`), so the comparison is covered + tested here.
pub(crate) fn free_run_decision(work: u64, armed_at: u64) -> Option<u64> {
    (work >= armed_at).then_some(work)
}

/// The first-entry **PMU-reset discipline** for the backend's shared-thread
/// retired-branch counter (P1(b)), factored out of the box-only `KvmBackend` so the
/// determinism invariant it encodes is covered + mutation-tested + stateful-property-
/// tested, not box-only review.
///
/// The box `perf_event` counter is shared across the (CPU-pinned) vCPU thread and
/// `exclude_host`, so it accumulates **every** VM's guest branches on that thread.
/// Each VM establishes its own baseline by resetting the counter at its **first
/// guest entry** (mirroring vmm-core's V-time `WorkSource::start_run`). A snapshot
/// **restore** must re-arm that reset for the *next* entry: a coexisting VM may run
/// on the shared thread between the restore and this VM's next entry, and resetting
/// at restore time would let those foreign branches accumulate into this VM's
/// counter (diverging it from vmm-core's V-time counter — the branching/multiverse
/// path). Deferring the reset to the next entry excludes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FirstEntryReset {
    /// Whether the next guest entry must reset the counter to re-baseline.
    pending: bool,
}

impl FirstEntryReset {
    /// A fresh VM: the very first entry resets (establishing the per-VM baseline).
    pub(crate) fn new() -> Self {
        Self { pending: true }
    }

    /// Re-arm the reset for the next entry (call on restore — P1(b)).
    pub(crate) fn rearm(&mut self) {
        self.pending = true;
    }

    /// Called at each guest entry: returns whether the counter must be reset **now**,
    /// and disarms (so the reset fires exactly once per arming).
    pub(crate) fn take_reset(&mut self) -> bool {
        std::mem::replace(&mut self.pending, false)
    }
}

impl Default for FirstEntryReset {
    fn default() -> Self {
        Self::new()
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
        deadline: u64,
        stashed: Option<(Exit, u64)>,
    }
    impl ExitAtCpu {
        fn new(work_at_exit: u64, deadline: u64) -> Self {
            Self {
                work_at_exit,
                deadline,
                stashed: None,
            }
        }
        /// Stash the guest exit (once) at its real work; return the deadline sentinel
        /// the live `LiveCpu` adapter returns on a guest exit — EXACTLY the deadline,
        /// so the planner always reaches ReadyToInject (never its own SkidExceeded)
        /// and `drive_run_until` makes the real early/at/past decision from the work.
        fn sentinel(&mut self) -> u64 {
            self.stashed.get_or_insert((GUEST_EXIT, self.work_at_exit));
            self.deadline
        }
    }
    impl CpuBackend for ExitAtCpu {
        fn work(&self) -> u64 {
            0
        }
        fn run_until_overflow(
            &mut self,
            _armed_at: u64,
        ) -> std::result::Result<u64, vtime::BackendError> {
            Ok(self.sentinel())
        }
        fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
            Ok(self.sentinel())
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

    /// P1 round-4 — the complete 3-case rule for a *reported guest exit*: `work <
    /// deadline` delivers it (early); `work == deadline` **fails closed** (a
    /// non-counted instruction already ran past the branch and committed a side
    /// effect — the count can't see it, only the exit's presence does); `work >
    /// deadline` fails closed (a worse overshoot). The timer-wins `Deadline` is the
    /// *no-exit* case, asserted in `lands_exactly_at_deadline_with_no_guest_exit`.
    #[test]
    fn reported_guest_exit_at_or_past_the_deadline_fails_closed() {
        let d = 1_000_000;
        // strictly before → the guest exit is delivered (genuinely early).
        let mut early = ExitAtCpu::new(d - 1, d);
        assert_eq!(
            drive_run_until(&planner(), &mut early, d).unwrap(),
            GUEST_EXIT,
            "a guest exit before the deadline is delivered"
        );
        // EXACTLY at the deadline WITH a reported exit → FAIL CLOSED. An IO/MMIO/HLT
        // exit *reported* at the deadline count means a non-counted instruction past
        // the deadline branch already executed + committed its side effect; turning it
        // into Deadline would drop that state and resume past it (P1 round-4).
        let mut at = ExitAtCpu::new(d, d);
        match drive_run_until(&planner(), &mut at, d) {
            Err(BackendError::Internal(msg)) => assert!(
                msg.contains("single-step-to-exact invariant was violated"),
                "the error names the violated invariant: {msg}"
            ),
            other => panic!("a reported exit AT the deadline must fail closed, got {other:?}"),
        }
        // past the deadline → loud determinism error (the exact instant was missed).
        let mut past = ExitAtCpu::new(d + 5, d);
        match drive_run_until(&planner(), &mut past, d) {
            Err(BackendError::Internal(msg)) => assert!(
                msg.contains("past the deadline"),
                "the error names the past-deadline overshoot: {msg}"
            ),
            other => panic!("a guest exit past the deadline must be a loud error, got {other:?}"),
        }
    }

    /// Round-2 P1(b): `free_run_decision` stops the free-run iff the overflow reached
    /// the armed count — the uniform check the box applies to EINTR / KVM_EXIT_INTR /
    /// IRQ_WINDOW_OPEN alike (so an IRQ-window re-entry can't overshoot a crossed
    /// overflow).
    #[test]
    fn free_run_decision_stops_only_at_or_past_armed() {
        assert_eq!(free_run_decision(99, 100), None, "below armed → re-enter");
        assert_eq!(free_run_decision(100, 100), Some(100), "at armed → stop");
        assert_eq!(free_run_decision(150, 100), Some(150), "past armed → stop");
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

    /// P1 round-4 (case 2): a non-counted IO right AFTER the deadline branch — modeled
    /// as a guest exit at `deadline + 1`, the very next instruction — is NEVER reached.
    /// The free-run stops strictly before the branch (margin > max_skid) and the
    /// single-step stops exactly ON it, so `run_until` returns `Deadline` with the IO
    /// un-executed (it re-occurs on the next entry, after the timer ISR). Bit-identical
    /// across the full skid (overflow-signal-timing) range. If the planner ever
    /// overshot to `deadline + 1`, that exit would surface (→ a loud `PastDeadline`
    /// error) and `.expect` would panic — so this directly proves "not executed".
    #[test]
    fn post_deadline_io_is_not_executed_across_signal_timing() {
        for skid in [0u64, 1, 50, 127] {
            let mut cpu = SimPreempt::new(
                SimCpuConfig {
                    seed: 9,
                    density_num: 1,
                    density_den: 4,
                    max_skid: skid,
                    initial_work: 0,
                },
                10_000,
            )
            .with_guest_exit(10_001); // the IO is the instruction AFTER the deadline branch
            let exit = drive_run_until(&planner(), &mut cpu, 10_000)
                .expect("must land at the deadline, never reaching the post-deadline IO");
            assert_eq!(
                exit,
                Exit::Deadline {
                    reached: Vtime(10_000)
                },
                "timer wins at the branch; the post-deadline IO is not reached (skid {skid})"
            );
            assert!(
                cpu.take_guest_exit().is_none(),
                "the post-deadline IO was NOT executed/reported (skid {skid})"
            );
            assert_eq!(
                cpu.work(),
                10_000,
                "landed at EXACTLY the deadline branch (skid {skid})"
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

        /// P1 round-4 property: for ALL (work_at_exit, deadline), the pure classifier
        /// matches the comparison, and `drive_run_until` enforces the complete rule for
        /// a *reported* guest exit — deliver strictly before, FAIL CLOSED at exactly the
        /// deadline (a non-counted instruction already ran past the branch) and past it.
        /// (The timer-wins `Deadline` is the no-exit land, covered separately.)
        #[test]
        fn drive_run_until_classifies_any_guest_exit(
            deadline in 1u64..=1_000_000,
            work_at_exit in 0u64..=2_000_000,
        ) {
            let disp = classify_guest_exit(work_at_exit, deadline);
            prop_assert_eq!(disp == GuestExitDisposition::Early, work_at_exit < deadline);
            prop_assert_eq!(disp == GuestExitDisposition::AtDeadline, work_at_exit == deadline);
            prop_assert_eq!(disp == GuestExitDisposition::PastDeadline, work_at_exit > deadline);

            let mut cpu = ExitAtCpu::new(work_at_exit, deadline);
            let got = drive_run_until(&planner(), &mut cpu, deadline);
            match disp {
                // Strictly before the deadline: the real guest exit is delivered.
                GuestExitDisposition::Early => {
                    prop_assert!(matches!(got, Ok(ref e) if *e == GUEST_EXIT));
                }
                // At OR past the deadline WITH a reported exit: fail closed (a
                // post-deadline instruction already executed — never silently absorbed).
                GuestExitDisposition::AtDeadline | GuestExitDisposition::PastDeadline => {
                    prop_assert!(matches!(got, Err(BackendError::Internal(_))));
                }
            }
        }
    }

    /// P1(b): the reset fires at the very first entry, then only after a `rearm`
    /// (restore) — never spontaneously.
    #[test]
    fn first_entry_reset_fires_once_then_only_after_rearm() {
        let mut r = FirstEntryReset::new();
        assert!(
            r.take_reset(),
            "the very first entry resets (per-VM baseline)"
        );
        assert!(!r.take_reset(), "no reset on subsequent entries");
        assert!(!r.take_reset());
        r.rearm();
        assert!(
            r.take_reset(),
            "restore re-arms: the next entry resets again"
        );
        assert!(!r.take_reset(), "and only that next entry");
        // `Default` == `new` (a fresh VM resets on its first entry).
        assert!(FirstEntryReset::default().take_reset());
    }
}

/// Stateful (model-based) property test for the P1(b) first-entry PMU-reset
/// discipline: random restore/run sequences over N VMs sharing one pinned thread,
/// with the real [`FirstEntryReset`] as the system-under-test and an INDEPENDENT
/// reference that recomputes each VM's own-branches-since-reset. It pins the
/// determinism invariant — a VM's `run_until` counter sees only ITS OWN branches,
/// never a coexisting VM's — that the box-only FFI (`kvm_sys`) cannot have covered
/// by llvm-cov / cargo-mutants. A regression in the discipline (e.g. a `rearm` that
/// stops re-arming) surfaces as a SUT/reference divergence on CI, not only on the
/// box. Miri-excluded: pure arithmetic, no `unsafe` to scrutinize.
#[cfg(all(test, not(miri)))]
mod reset_discipline_stateful {
    use crate::run_until::FirstEntryReset;
    use proptest::prelude::*;
    use proptest::test_runner::Config;
    use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

    /// VMs sharing one CPU-pinned thread (the box `perf_event` `exclude_host`
    /// counter accumulates every VM's guest branches on that thread).
    const N_VMS: usize = 3;

    /// One VM in the independent reference: an INDEPENDENT shared-thread counter
    /// (vmm-core's V-time counter `A`), reset at its baseline points by a discipline
    /// encoded DIRECTLY here (not via `FirstEntryReset`). Effective work is
    /// `total − reset_at` — the shared counter legitimately includes foreign branches
    /// retired between this VM's runs (so does the backend counter `B`); what must
    /// match is the **reset point**, so `B`'s reset discipline equals `A`'s.
    #[derive(Clone, Debug)]
    struct RefVm {
        reset_at: u64,
        entered: bool,
        restore_pending: bool,
    }
    #[derive(Clone, Debug)]
    struct RefState {
        /// The shared thread's total retired guest branches (`A`'s raw count).
        total: u64,
        vms: Vec<RefVm>,
        /// (vm, expected `A` work) of the most recent `Enter`, for the SUT assert.
        last_enter: Option<(usize, u64)>,
    }

    #[derive(Clone, Debug)]
    enum Op {
        /// A VM enters the guest and retires `branches` guest branches on the thread.
        Enter { vm: usize, branches: u64 },
        /// A snapshot restore re-arms the VM's next-entry reset.
        Restore { vm: usize },
    }

    struct RefMachine;
    impl ReferenceStateMachine for RefMachine {
        type State = RefState;
        type Transition = Op;
        fn init_state() -> BoxedStrategy<RefState> {
            Just(RefState {
                total: 0,
                vms: vec![
                    RefVm {
                        reset_at: 0,
                        entered: false,
                        restore_pending: false,
                    };
                    N_VMS
                ],
                last_enter: None,
            })
            .boxed()
        }
        fn transitions(_: &RefState) -> BoxedStrategy<Op> {
            prop_oneof![
                3 => (0..N_VMS, 1u64..10_000).prop_map(|(vm, branches)| Op::Enter { vm, branches }),
                1 => (0..N_VMS).prop_map(|vm| Op::Restore { vm }),
            ]
            .boxed()
        }
        fn apply(mut s: RefState, op: &Op) -> RefState {
            match *op {
                Op::Enter { vm, branches } => {
                    // The correct discipline: re-baseline at the first entry and at
                    // the first entry after a restore (`A` re-arms its first-entry
                    // reset on `restore_vm_state`).
                    let total = s.total;
                    let v = &mut s.vms[vm];
                    if !v.entered || v.restore_pending {
                        v.reset_at = total;
                        v.entered = true;
                        v.restore_pending = false;
                    }
                    s.total = total.saturating_add(branches);
                    s.last_enter = Some((vm, s.total - s.vms[vm].reset_at));
                }
                Op::Restore { vm } => {
                    s.vms[vm].restore_pending = true;
                    s.last_enter = None;
                }
            }
            s
        }
    }

    /// One VM's view of the shared counter in the SUT: the real `FirstEntryReset`
    /// plus the counter's reset point (`work = shared_total - reset_at`).
    struct SutVm {
        arm: FirstEntryReset,
        reset_at: u64,
    }
    struct Sut {
        /// All VMs' guest branches retired on the shared thread (the perf counter).
        total: u64,
        vms: Vec<SutVm>,
    }

    struct Machine;
    impl StateMachineTest for Machine {
        type SystemUnderTest = Sut;
        type Reference = RefMachine;
        fn init_test(_: &RefState) -> Sut {
            Sut {
                total: 0,
                vms: (0..N_VMS)
                    .map(|_| SutVm {
                        arm: FirstEntryReset::new(),
                        reset_at: 0,
                    })
                    .collect(),
            }
        }
        fn apply(mut sut: Sut, ref_state: &RefState, op: Op) -> Sut {
            match op {
                Op::Enter { vm, branches } => {
                    // First-entry / post-restore reset re-baselines the shared counter.
                    if sut.vms[vm].arm.take_reset() {
                        sut.vms[vm].reset_at = sut.total;
                    }
                    sut.total = sut.total.saturating_add(branches);
                    let work = sut.total - sut.vms[vm].reset_at;
                    // The backend counter `B`'s effective work must equal vmm-core's
                    // V-time counter `A`'s — i.e. their reset points agree across the
                    // save/restore/run interleaving (P1(b)). A regression in the reset
                    // discipline (e.g. a `rearm` that no longer re-arms) diverges them.
                    let (rv, expected) = ref_state.last_enter.expect("ref tracked the enter");
                    assert_eq!(rv, vm);
                    assert_eq!(
                        work, expected,
                        "vm {vm}: backend run_until counter B diverged from V-time counter A \
                         (B work {work} != A work {expected}) — reset-point desync"
                    );
                }
                Op::Restore { vm } => sut.vms[vm].arm.rearm(),
            }
            sut
        }
        fn check_invariants(_: &Sut, _: &RefState) {}
    }

    prop_state_machine! {
        #![proptest_config(Config { cases: 256, ..Config::default() })]
        #[test]
        fn first_entry_reset_excludes_foreign_branches(sequential 1..50 => Machine);
    }
}
