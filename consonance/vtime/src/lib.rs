// SPDX-License-Identifier: AGPL-3.0-or-later
//! # vtime — virtual time engine & precise-injection planner
//!
//! In the deterministic hypervisor, the guest never sees real time. Every
//! guest-visible clock (TSC reads, timer interrupt deadlines) is derived from
//! **V-time**, a pure function of *work performed*: a hardware counter of
//! retired conditional branches, read at every VM exit. Same seed ⇒ same
//! instruction stream ⇒ same work counts ⇒ same observed clocks, bit for
//! bit. This crate is the pure-logic half of that mechanism: the work↔time
//! arithmetic ([`VClock`]), the timer deadline queue ([`TimerQueue`]), the
//! injection-planner state machine ([`InjectionPlanner`]) driven through a
//! backend trait ([`CpuBackend`]) — which reaches the next scheduled event *by
//! executing* — and its idle-jump dual ([`IdlePlanner`]), which reaches it *by
//! jumping* when the guest is idle (`HLT`). It has no OS dependencies; the real
//! perf_event/KVM backend is a separate component, and tests here run
//! against [`sim::SimCpu`], a simulator that models PMU skid adversarially.
//!
//! ## Work, not instructions
//!
//! `work` counts *counted events* (retired conditional branches), not
//! instructions: most instructions advance work by 0, an instruction
//! advances it by at most 1. An injection target `T` therefore means: stop
//! at the **first instruction boundary at which work reaches `T`** —
//! well-defined and deterministic because work is a pure function of the
//! deterministic instruction stream. V-time follows as
//! `vns(work) = vns_base + floor(work · ratio_num / ratio_den)` and the
//! virtual TSC as `tsc(work) = guest_base + floor(vns(work) · guest_hz / 10⁹)`,
//! all in integer/fixed-point math (`u128` intermediates, saturating to
//! `u64::MAX` — this crate denies `clippy::float_arithmetic`). `vns_base`
//! absorbs the two events where V-time moves without work: **idle-skip**
//! (the guest HLTed; the host warps V-time to the next deadline) and
//! **snapshot restore** (the hardware counter restarts at 0; the restored
//! clock carries the snapshot's effective V-time entirely in `vns_base`).
//!
//! ## PMU skid, and why injection is margin-then-single-step
//!
//! Timer interrupts must be injected at an *exact* work count: if a replay
//! injects one interrupt a single branch earlier or later than the recording
//! did, the executions diverge. But PMU overflow interrupts have **skid**:
//! when the counter crosses the armed value, the interrupt is delivered
//! asynchronously, and the CPU retires more instructions — and more counted
//! events — before execution actually stops. Skid is late-only (the
//! interrupt never fires before the armed count is reached) and bounded in
//! practice, but *unpredictable* within that bound, so stopping "wherever the
//! overflow lands" is nondeterministic. Arming exactly at the target would
//! therefore overshoot it irrecoverably.
//!
//! The fix, due to rr (Mozilla's record-and-replay debugger; see O'Callahan
//! et al., *"Engineering Record And Replay For Deployability"*, USENIX ATC
//! 2017, and <https://rr-project.org> — rr replays asynchronous events at
//! exact retired-conditional-branch counts using the same trick), is
//! two-phase:
//!
//! 1. **Arm early.** Program the overflow at `target − skid_margin`, with
//!    `skid_margin` STRICTLY greater than the worst-case skid. The overflow
//!    then stops the vCPU somewhere in `[target − skid_margin, target)` —
//!    near the target, but always STRICTLY BEFORE it (the overflow/SIGIO is
//!    not instruction-precise at the boundary, so it must leave room for the
//!    single-step). A stop at or past the target consumed the whole margin and
//!    is a [`VtimeError::SkidExceeded`] violation, never injected raw.
//! 2. **Single-step to exactness.** From there, execute one instruction at a
//!    time, checking the work counter at each instruction boundary, and stop
//!    at the first boundary where `work == target`. Now inject. Every landing
//!    is positioned by THIS exact step, never by the imprecise overflow.
//!
//! The counted-event distance covered by stepping is at most `skid_margin`,
//! but the *instruction* count is bounded only by the guest's event density:
//! a long branch-free stretch may need many steps per counted event. If the
//! hardware ever skids past the target despite the margin, that is a
//! determinism-destroying event and is reported loudly as
//! [`VtimeError::SkidExceeded`], never papered over. [`InjectionPlanner`]
//! implements exactly this state machine, plus the trivial fast paths
//! (target already current — e.g. right after an idle warp — and target in
//! the past, which is reported as [`PlanOutcome::TargetInPast`]).
//!
//! ## What the real backend maps to
//!
//! [`CpuBackend`] is the seam between this pure logic and the OS. The real
//! implementation (a later task) maps:
//!
//! - [`CpuBackend::work`] → reading the perf_event counter of retired
//!   conditional branches (`read(2)` on the perf fd, or `rdpmc`) at the
//!   current VM exit;
//! - [`CpuBackend::run_until_overflow`] → programming the counter's sample
//!   period so it overflows at the armed absolute count, re-entering the
//!   guest with `KVM_RUN`, and treating the overflow interrupt (delivered as
//!   SIGIO on the perf fd) as the kick that forces a KVM exit
//!   (`KVM_EXIT_INTR`), then reading the counter to report where execution
//!   stopped;
//! - [`CpuBackend::single_step`] → one `KVM_RUN` with
//!   `KVM_SET_GUEST_DEBUG(KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP)`,
//!   then reading the counter at the resulting debug exit.
//!
//! [`sim::SimCpu`] implements the same trait over an abstract instruction
//! stream with seeded skid drawn adversarially from `0..=max_skid`, so the
//! planner's exactness is property-tested without any of the above.
//!
//! ## Determinism rules embedded here
//!
//! Everything is integer arithmetic; saturation (to `u64::MAX`) is the
//! documented, deterministic overflow behavior everywhere. [`TimerQueue`]
//! fires equal deadlines in FIFO scheduling order (a documented total
//! order), and periodic timers re-arm at `fired deadline + period` — fixed
//! cadence with no drift accumulation. Nothing in this crate reads wall
//! clocks, uses unseeded randomness, or iterates a hash map.

mod clock;
mod error;
mod idle;
mod planner;
pub mod pvclock;
mod queue;
pub mod sim;

pub use clock::{VClock, VClockConfig};
pub use error::{BackendError, VtimeError};
pub use idle::{IdleAdvance, IdlePlanner};
pub use planner::{CpuBackend, InjectionPlanner, PlanOutcome, PlannerConfig};
pub use queue::{TimerQueue, TimerToken};
