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

/// The arm-early margin (work units, in retired conditional branches). The overflow
/// is armed at `deadline − SKID_MARGIN` so that `armed_at + skid` (the PMI/signal-
/// delivery latency, all counted as skid) lands **STRICTLY BEFORE** the deadline,
/// leaving the remaining branches to exact single-stepping — the precision invariant
/// (P1 round-6): every `Exit::Deadline` is positioned by the precise single-step, and
/// an overflow that stops at/past the deadline is a loud `SkidExceeded`, never injected
/// raw (the overflow/SIGIO is not instruction-precise at the boundary).
///
/// It MUST be **strictly greater** than the box's worst-case skid. Task 07
/// (`docs/ROADMAP.md`, PR #20) recommended `skid_margin = 128` (measured max × a safety
/// factor; the acceptance bound is `skid ≤ 128`). We arm at **256** — double that bound
/// — so even a skid at the full task-07 bound (128) leaves ≥ 128 branches of headroom
/// for the single-step (`stopped ≤ deadline − 128 < deadline`); the result is
/// unchanged (the single-step always lands at exactly the deadline), only the arm point
/// moves earlier. A skid that still reaches the deadline exceeds 2× the measured
/// margin → a genuine determinism violation, surfaced loudly.
pub(crate) const SKID_MARGIN: u64 = 256;

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
/// **The precision invariant (P1 round-6).** EVERY returned `Exit::Deadline` is
/// positioned by the precise single-step, NEVER by the instruction-imprecise overflow.
/// The planner enforces it: an overflow that stops at `stopped >= target` consumed the
/// whole margin → `SkidExceeded` (a loud error here), so a Phase-1 (overflow) landing
/// always finishes with ≥ 1 single-step to the exact boundary. Audit of the three
/// `Deadline`-producing paths: (1) overflow + single-step (precise by the invariant);
/// (2) `target == now` / `0 < target − now ≤ margin` — no overflow, the guest is at a
/// clean exit boundary or is single-stepped the whole way (precise); (3) `TargetInPast`
/// — the deadline was already past at entry, so `reached = now` is the clean entry
/// boundary, not an overflow stop (precise). None returns `Deadline` from a raw
/// overflow stop.
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

/// **The complete `run_until` contract (P1 round-8) — deadline vs current work.** A
/// pure decision so it is covered, mutation-tested, and the box `run_until` reads it
/// once at entry. The three cases (and what each does) are the explicit contract,
/// documented as a table in IMPLEMENTATION.md:
///
/// | `deadline` vs `current` | meaning | action |
/// |---|---|---|
/// | `>`  | the timer is ahead | drive the planner: arm overflow, single-step to EXACTLY the deadline (precision invariant) |
/// | `==` | already at the deadline | return `Exit::Deadline` with **ZERO guest steps toward the deadline** — never step a guest instruction past it |
/// | `<`  | the deadline is in the past | **invalid** — the LAPIC timer deadline is always in the future; fail closed (cannot run backward) |
///
/// The `==` case does the first-entry baseline reset (the caller's `ensure_first_run`)
/// but executes **no** guest instruction toward the deadline. A completion staged by
/// the prior step is left in the run page and committed by the NEXT entry's `KVM_RUN`
/// (it is not lost on re-entry); the caller contract is to commit it on a normal entry
/// (a `>` deadline, or `run`) BEFORE a save, never to save across an owed completion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RunUntilStart {
    /// `deadline > current`: drive the planner to exactly the deadline.
    Drive,
    /// `deadline == current`: return `Deadline` with zero guest steps.
    AlreadyAtDeadline,
    /// `deadline < current`: invalid — fail closed.
    DeadlineInPast,
}

/// Classify a `run_until` deadline against the current work count. Pure arithmetic.
pub(crate) fn classify_run_until(deadline: u64, current: u64) -> RunUntilStart {
    match deadline.cmp(&current) {
        std::cmp::Ordering::Greater => RunUntilStart::Drive,
        std::cmp::Ordering::Equal => RunUntilStart::AlreadyAtDeadline,
        std::cmp::Ordering::Less => RunUntilStart::DeadlineInPast,
    }
}

/// Fail-closed poison for a guest exit that was **decoded** (consumed by KVM — the
/// instruction retired, the exit is guest-visible) but **not yet delivered** to the VMM
/// (P2 round-9, generalizing the round-5 read-style fix to no-completion exits). The box
/// backend `arm`s it BEFORE the fallible post-exit PMU read and marks it `delivered` on
/// success; if that read fails, it stays armed, so the next entry `is_poisoned()` and
/// fails closed — a retry never re-enters PAST a consumed exit the VMM never observed
/// (e.g. a PIO `OUT`/MMIO-write whose device side effect was never dispatched, a
/// `HLT`/shutdown never reported). Read-style exits ALSO set a pending completion (the
/// `PendingCompletion` guard), but a no-completion exit has no pending — this poison is
/// its fail-closed. Factored here so the state machine is covered + mutation-tested.
#[derive(Default)]
pub(crate) struct ExitPoison {
    armed: bool,
}
impl ExitPoison {
    /// Record an exit as in-flight, BEFORE the fallible post-exit work read.
    pub(crate) fn arm(&mut self) {
        self.armed = true;
    }
    /// The exit was delivered (the read succeeded): clear the poison.
    pub(crate) fn delivered(&mut self) {
        self.armed = false;
    }
    /// Whether an exit is armed-but-not-delivered (its read failed) → the next entry
    /// must fail closed (never skip the consumed exit).
    pub(crate) fn is_poisoned(&self) -> bool {
        self.armed
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
///
/// # The first-entry-reset invariant (task 47, stated globally — round-11)
///
/// **The pending reset is consumed (the counter zeroed and the flag disarmed) by an
/// ACTUAL guest entry — a real `KVM_RUN` — and by nothing else.** No
/// zero-step / `AlreadyAtDeadline` / `DeadlineInPast` / `restore` / any
/// `Deadline`-without-entry path may consume or disarm it; it stays **pending** until
/// a real entry occurs. The reason is the contamination above: if a no-entry path
/// disarmed it, a coexisting VM running on the shared thread before this VM's *next*
/// real entry would be folded into this VM's baseline (`B ≢ A`). This recurred across
/// rounds because the rule lived only at call sites; it is now enforced structurally:
/// the sole consumer ([`KvmBackend::ensure_first_run`]) is called **only** on a path
/// that immediately performs a `KVM_RUN` (`run` → `enter_guest`; `run_until`'s `Drive`
/// branch → `drive_run_until`), never before the `run_until` classify that may pick a
/// no-entry branch. No-entry paths read the *deferred* baseline (work `0` while
/// [`is_pending`](Self::is_pending) holds) without touching the flag.
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

    /// Non-consuming peek: is a first-entry reset still pending? Used by `run_until` to
    /// read the *deferred* baseline (work `0`) on a no-entry path WITHOUT disarming the
    /// reset — preserving the invariant that only a real `KVM_RUN` consumes it.
    pub(crate) fn is_pending(&self) -> bool {
        self.pending
    }

    /// Called at a REAL guest entry (a `KVM_RUN`): returns whether the counter must be
    /// reset **now**, and disarms (so the reset fires exactly once per arming). Per the
    /// invariant above, call this ONLY immediately before an actual entry.
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
        /// Stash a guest exit (with its real work count) iff work crossed the
        /// threshold, and return the planner sentinel: in the FREE-RUN phase, STRICTLY
        /// below the deadline (round-6: an overflow stop `>= target` is `SkidExceeded`,
        /// so the free-run never reports the deadline directly — the single-step phase
        /// then reaches it); in the SINGLE-STEP phase, AT the deadline (to end the
        /// planner's step loop at ReadyToInject). Else the real work count.
        fn maybe_exit(&mut self, work: u64, free_run: bool) -> u64 {
            if let Some(at) = self.guest_exit_at
                && work >= at
                && self.pending_exit.is_none()
            {
                self.pending_exit = Some((GUEST_EXIT, work));
                return if free_run {
                    self.deadline.saturating_sub(1)
                } else {
                    self.deadline
                };
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
            Ok(self.maybe_exit(stopped, true))
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
            Ok(self.maybe_exit(w, false))
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
        /// Stash the guest exit (once) at its real work count. Mirrors the live
        /// `LiveCpu` adapter, which stashes the genuine exit and drives the planner to
        /// ReadyToInject via the sentinel returns below; `drive_run_until` then makes
        /// the real early/at/past decision from the stashed WORK.
        fn stash(&mut self) {
            self.stashed.get_or_insert((GUEST_EXIT, self.work_at_exit));
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
            // Free-run sentinel: report STRICTLY BELOW the deadline (round-6: an
            // overflow stop `>= target` is SkidExceeded). The single-step phase then
            // reaches the deadline so the planner stops at ReadyToInject.
            self.stash();
            Ok(self.deadline.saturating_sub(1))
        }
        fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
            // Single-step sentinel: reach the deadline so the planner's step loop ends
            // at ReadyToInject (== target, never > target).
            self.stash();
            Ok(self.deadline)
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

    /// A [`PreemptCpu`] whose overflow phase stops at a FIXED absolute work count
    /// (a deterministic skid), and single-steps by 1 — to test the round-6 precision
    /// invariant directly: an overflow landing exactly ON the deadline must be a loud
    /// `SkidExceeded`, never a raw `Deadline`; one strictly before is single-stepped
    /// to the exact boundary.
    struct OverflowStopAtCpu {
        work: u64,
        overflow_stop: u64,
    }
    impl CpuBackend for OverflowStopAtCpu {
        fn work(&self) -> u64 {
            self.work
        }
        fn run_until_overflow(
            &mut self,
            _armed_at: u64,
        ) -> std::result::Result<u64, vtime::BackendError> {
            // The PMU never fires early: stop no earlier than the current work.
            self.work = self.overflow_stop.max(self.work);
            Ok(self.work)
        }
        fn single_step(&mut self) -> std::result::Result<u64, vtime::BackendError> {
            self.work += 1;
            Ok(self.work)
        }
    }
    impl PreemptCpu for OverflowStopAtCpu {
        fn take_guest_exit(&mut self) -> Option<(Exit, u64)> {
            None
        }
        fn take_error(&mut self) -> Option<BackendError> {
            None
        }
    }

    /// P1 round-6 precision invariant: an overflow that lands EXACTLY on the deadline
    /// (skid == margin) must NOT yield a raw `Exit::Deadline` — the overflow is
    /// instruction-imprecise at the boundary, so it is a loud `SkidExceeded`; an
    /// overflow strictly before is single-stepped to the exact deadline.
    #[test]
    fn overflow_landing_exactly_on_deadline_is_skid_exceeded_not_raw_deadline() {
        let deadline = 10_000; // > SKID_MARGIN, so the planner arms the overflow
        // Overflow stops EXACTLY on the deadline → SkidExceeded (never raw Deadline).
        let mut at_target = OverflowStopAtCpu {
            work: 0,
            overflow_stop: deadline,
        };
        match drive_run_until(&planner(), &mut at_target, deadline) {
            Err(BackendError::Internal(msg)) => assert!(
                msg.contains("skid"),
                "an overflow landing exactly on the deadline is a skid violation: {msg}"
            ),
            other => panic!(
                "an overflow landing exactly on the deadline must be SkidExceeded, never a raw \
                 Deadline; got {other:?}"
            ),
        }
        // Overflow strictly before → single-stepped to the exact boundary → Deadline.
        let mut before = OverflowStopAtCpu {
            work: 0,
            overflow_stop: deadline - 1,
        };
        assert_eq!(
            drive_run_until(&planner(), &mut before, deadline).unwrap(),
            Exit::Deadline {
                reached: Vtime(deadline)
            },
            "an overflow strictly before the deadline is single-stepped to the exact boundary"
        );
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

    /// P1 round-8 — the complete `run_until` contract for ALL deadline-vs-current
    /// cases: `>` drives the planner; `==` is already at the deadline (zero guest
    /// steps); `<` is invalid (can't run backward — fail closed).
    #[test]
    fn classify_run_until_covers_every_deadline_vs_current_case() {
        // deadline > current → drive the planner to exactly the deadline.
        assert_eq!(classify_run_until(101, 100), RunUntilStart::Drive);
        assert_eq!(
            classify_run_until(1, 0),
            RunUntilStart::Drive,
            "fresh VM, future deadline"
        );
        // deadline == current → already there, zero guest steps.
        assert_eq!(
            classify_run_until(0, 0),
            RunUntilStart::AlreadyAtDeadline,
            "fresh run_until(Vtime(0)): at the deadline, NO guest step"
        );
        assert_eq!(
            classify_run_until(100, 100),
            RunUntilStart::AlreadyAtDeadline
        );
        // deadline < current → invalid (the timer deadline is always in the future).
        assert_eq!(
            classify_run_until(99, 100),
            RunUntilStart::DeadlineInPast,
            "a past deadline can't be run to — fail closed"
        );
        assert_eq!(classify_run_until(0, 1), RunUntilStart::DeadlineInPast);
    }

    /// P2 round-9: the exit poison fails closed for a decoded-but-undelivered guest exit
    /// (the box arms it before the fallible post-exit read; a failed read leaves it
    /// armed) and clears once an exit is delivered (the read succeeded).
    #[test]
    fn exit_poison_fails_closed_until_an_exit_is_delivered() {
        let mut p = ExitPoison::default();
        assert!(!p.is_poisoned(), "fresh: not poisoned");
        // A delivered exit (post-exit read succeeded): arm, then delivered → clean.
        p.arm();
        assert!(
            p.is_poisoned(),
            "armed: exit decoded, post-exit read still pending"
        );
        p.delivered();
        assert!(
            !p.is_poisoned(),
            "delivered: read succeeded → poison cleared"
        );
        // A FAILED read: armed, NO delivered → stays poisoned (the next entry fails closed
        // so a no-completion exit the VMM never observed is not skipped).
        p.arm();
        assert!(
            p.is_poisoned(),
            "armed but not delivered (post-exit read failed) → fail closed on retry"
        );
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
        // A representative spread of densities + skids (skid < margin = 256), incl.
        // values near the margin — all must land at EXACTLY the deadline (the overflow
        // stops strictly before, the single-step finishes precisely).
        for &(seed, num, den, skid) in &[
            (1u64, 1u64, 1u64, 0u64),
            (2, 1, 3, 7),
            (3, 1, 1000, 64),
            (4, 1, 10, 200),
            (5, 1, 5, 255),
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
        // max_skid (400) deliberately exceeds SKID_MARGIN (256): the overflow can stop
        // at or past the target, which MUST surface loudly (SkidExceeded), never be
        // tolerated as a raw landing — the round-6 precision invariant.
        let mut saw_skid_error = false;
        for seed in 0..64u64 {
            let mut cpu = SimPreempt::new(
                SimCpuConfig {
                    seed,
                    density_num: 1,
                    density_den: 1,
                    max_skid: 400,
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
        /// density, and skid STRICTLY within the margin, the arm-overflow-then-single-
        /// step `run_until` lands at **exactly** the deadline. Because `SimCpu` retires
        /// the same instruction stream whether free-running (`run_until_overflow`)
        /// or single-stepping, landing at the exact target — regardless of where
        /// the (adversarially-drawn) skid fell — *is* the count-neutrality proof:
        /// the preemption instant is a pure function of the seed, not of the skid.
        /// `max_skid < SKID_MARGIN` (the round-6 invariant: the overflow must stop
        /// STRICTLY before the deadline so the single-step finishes; skid == margin is
        /// the loud `SkidExceeded` case, covered separately). Deadlines/densities are
        /// bounded so the suite stays well under the ~3-min budget. Both the
        /// long-distance (overflow + step) and short-distance (step-only) regimes
        /// are covered since `deadline` straddles `SKID_MARGIN`.
        #[test]
        fn run_until_is_count_neutral_and_exact(
            seed in 1u64..=u64::MAX,
            density_num in 1u64..=8,
            extra_den in 0u64..=24,
            max_skid in 0u64..SKID_MARGIN,
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
    /// (restore) — never spontaneously. P1 round-11: `is_pending` peeks the armed state
    /// WITHOUT consuming it (the seam that lets `run_until`'s no-entry branches read the
    /// deferred baseline while leaving the reset armed for the next real entry).
    #[test]
    fn first_entry_reset_fires_once_then_only_after_rearm() {
        let mut r = FirstEntryReset::new();
        // `is_pending` is a non-consuming peek: it reports armed and leaves it armed.
        assert!(r.is_pending(), "a fresh VM is armed");
        assert!(r.is_pending(), "peeking does not consume — still armed");
        assert!(
            r.take_reset(),
            "the very first entry resets (per-VM baseline)"
        );
        assert!(!r.is_pending(), "consumed by the entry — no longer pending");
        assert!(!r.take_reset(), "no reset on subsequent entries");
        assert!(!r.take_reset());
        r.rearm();
        assert!(r.is_pending(), "restore re-arms — pending again");
        assert!(
            r.take_reset(),
            "restore re-arms: the next entry resets again"
        );
        assert!(!r.take_reset(), "and only that next entry");
        // `Default` == `new` (a fresh VM resets on its first entry).
        assert!(FirstEntryReset::default().take_reset());
    }
}

/// Stateful (model-based) property test for the first-entry PMU-reset discipline
/// (P1(b)): random run/restore sequences over N VMs sharing one pinned thread, with
/// the real [`FirstEntryReset`] as the system-under-test (SUT) and an **INDEPENDENT**
/// reference that tracks each VM's OWN retired branches directly — NOT derived from the
/// shared counter.
///
/// **Round-7 P2.** A reference that computed the expected work as the *shared* total
/// minus the reset point would MIRROR the implementation: for any interleaving it would
/// include exactly the foreign branches the SUT includes, so the test would pass even
/// WITH the contamination its name claims to disprove. So the reference here keeps a
/// per-VM `own` tally (incremented only when THAT vm runs) and the expected work is
/// `own − own_baseline` — computed with no reference to the shared counter. The
/// invariant: a VM's `run_until` counter (the backend's `B`, modelled as the shared
/// `total − reset_at`) sees only ITS OWN branches, equal to that independent tally. A
/// regression in the discipline (e.g. a `rearm` that no longer re-arms) makes the SUT
/// include a coexisting VM's branches → it diverges from the own-branch reference → the
/// test fails on CI, not only on the box.
///
/// Transitions model the REAL execution: a VM (re)enters only when the discipline
/// re-baselines it on entry — it is the VM currently running (continuing its own run),
/// is fresh (first entry auto-resets), or was just restored (restore re-arms). The VMM
/// never time-slices a VM back in after another ran WITHOUT a snapshot restore
/// (branching IS restore), so that contaminating interleaving is not generated.
/// Miri-excluded: pure arithmetic, no `unsafe` to scrutinize.
#[cfg(all(test, not(miri)))]
mod reset_discipline_stateful {
    use crate::run_until::FirstEntryReset;
    use proptest::prelude::*;
    use proptest::test_runner::Config;
    use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};

    /// VMs sharing one CPU-pinned thread (the box `perf_event` `exclude_host`
    /// counter accumulates every VM's guest branches on that thread).
    const N_VMS: usize = 3;

    /// One VM in the INDEPENDENT reference: its OWN retired-branch tally (`own`), the
    /// tally at its last baseline reset (`own_baseline`), and the discipline flags. The
    /// expected `run_until` work is `own − own_baseline` — computed WITHOUT the shared
    /// counter, so it can never inherit a coexisting VM's branches.
    #[derive(Clone, Debug)]
    struct RefVm {
        own: u64,
        own_baseline: u64,
        entered: bool,
        restore_pending: bool,
    }
    #[derive(Clone, Debug)]
    struct RefState {
        /// The VM currently running (for realistic transition generation).
        active: Option<usize>,
        vms: Vec<RefVm>,
        /// (vm, expected OWN-branch work) of the most recent `Enter`, for the SUT assert.
        last_enter: Option<(usize, u64)>,
    }

    #[derive(Clone, Debug)]
    enum Op {
        /// A VM enters the guest and retires `branches` of ITS OWN guest branches.
        Enter { vm: usize, branches: u64 },
        /// A snapshot restore re-arms the VM's next-entry reset.
        Restore { vm: usize },
    }

    /// A VM may Enter iff the discipline re-baselines it on this entry: it is active
    /// (continuing its own run), fresh (first entry resets), or restore_pending
    /// (restore re-armed). This is exactly the real VMM's execution — a VM is never
    /// time-sliced back in after another ran without a restore.
    fn may_enter(s: &RefState, vm: usize) -> bool {
        s.active == Some(vm) || !s.vms[vm].entered || s.vms[vm].restore_pending
    }

    struct RefMachine;
    impl ReferenceStateMachine for RefMachine {
        type State = RefState;
        type Transition = Op;
        fn init_state() -> BoxedStrategy<RefState> {
            Just(RefState {
                active: None,
                vms: vec![
                    RefVm {
                        own: 0,
                        own_baseline: 0,
                        entered: false,
                        restore_pending: false,
                    };
                    N_VMS
                ],
                last_enter: None,
            })
            .boxed()
        }
        fn transitions(s: &RefState) -> BoxedStrategy<Op> {
            let enterable: Vec<usize> = (0..N_VMS).filter(|&vm| may_enter(s, vm)).collect();
            let enter = (proptest::sample::select(enterable), 1u64..10_000)
                .prop_map(|(vm, branches)| Op::Enter { vm, branches });
            let restore = (0..N_VMS).prop_map(|vm| Op::Restore { vm });
            prop_oneof![3 => enter, 1 => restore].boxed()
        }
        fn apply(mut s: RefState, op: &Op) -> RefState {
            match *op {
                Op::Enter { vm, branches } => {
                    let v = &mut s.vms[vm];
                    // Re-baseline the OWN tally at the first entry and the first entry
                    // after a restore (the real discipline's reset points).
                    if !v.entered || v.restore_pending {
                        v.own_baseline = v.own;
                        v.entered = true;
                        v.restore_pending = false;
                    }
                    v.own = v.own.saturating_add(branches);
                    let work = v.own - v.own_baseline;
                    s.active = Some(vm);
                    s.last_enter = Some((vm, work));
                }
                Op::Restore { vm } => {
                    s.vms[vm].restore_pending = true;
                    s.last_enter = None;
                }
            }
            s
        }
        fn preconditions(s: &RefState, op: &Op) -> bool {
            // Enforced during generation AND shrinking: an `Enter` is valid only for a
            // VM the discipline re-baselines on entry (see `may_enter`).
            match *op {
                Op::Enter { vm, .. } => may_enter(s, vm),
                Op::Restore { .. } => true,
            }
        }
    }

    /// The SUT faithfully models the backend: a SHARED `total` counter, the real
    /// `FirstEntryReset` per VM, and the counter's reset point (`work = total − reset_at`).
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
                    // The backend counter `B` (shared `total − reset_at`) must equal the
                    // INDEPENDENT own-branch tally. A regression in the reset discipline
                    // (e.g. a `rearm` that no longer re-arms) leaves `reset_at` stale, so
                    // `B` inherits a coexisting VM's branches and diverges from the tally.
                    let (rv, expected) = ref_state.last_enter.expect("ref tracked the enter");
                    assert_eq!(rv, vm);
                    assert_eq!(
                        work, expected,
                        "vm {vm}: backend counter B (shared total − reset_at = {work}) diverged \
                         from the INDEPENDENT own-branch tally {expected} — foreign-branch \
                         contamination / reset-point desync"
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

    /// Direct unit test of the transition validity (`preconditions`/`may_enter`): a VM
    /// may re-enter only when the discipline re-baselines it (active / fresh /
    /// just-restored), so the property test never models the contaminating
    /// "re-enter after a foreign VM without a restore" interleaving.
    #[test]
    fn preconditions_reject_a_non_rebaselined_reenter() {
        let entered = |restore_pending| RefVm {
            own: 1,
            own_baseline: 0,
            entered: true,
            restore_pending,
        };
        // vm0 already ran; vm1 is the active (running) VM; vm2 is fresh.
        let s = RefState {
            active: Some(1),
            vms: vec![
                entered(false),
                entered(false),
                RefVm {
                    own: 0,
                    own_baseline: 0,
                    entered: false,
                    restore_pending: false,
                },
            ],
            last_enter: None,
        };
        // vm0 re-entering after vm1 ran, WITHOUT a restore, would inherit vm1's branches
        // → rejected (not a real scenario, and not re-baselined).
        assert!(!RefMachine::preconditions(
            &s,
            &Op::Enter { vm: 0, branches: 1 }
        ));
        // The active VM continues, a fresh VM first-enters, a restore is always valid.
        assert!(RefMachine::preconditions(
            &s,
            &Op::Enter { vm: 1, branches: 1 }
        ));
        assert!(RefMachine::preconditions(
            &s,
            &Op::Enter { vm: 2, branches: 1 }
        ));
        assert!(RefMachine::preconditions(&s, &Op::Restore { vm: 0 }));
        // And after a restore, vm0 may re-enter (re-baselined).
        let mut restored = s;
        restored.vms[0].restore_pending = true;
        assert!(RefMachine::preconditions(
            &restored,
            &Op::Enter { vm: 0, branches: 1 }
        ));
    }
}
