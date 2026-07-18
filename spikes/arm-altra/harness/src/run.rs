// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `KVM_RUN` measurement loop: arm the work counter, run a single vCPU to its
//! window marks, sample `BR_RETIRED`, assemble a [`RunRecord`].
//!
//! This is the harness proper — deliverable 2's "minimal ioctl-level KVM harness
//! (single vCPU) plus run orchestration". The loop itself is **pure logic**: it
//! programs against two narrow seams ([`Vcpu`] and [`WorkCounter`]) rather than
//! against ioctls, so the whole of it — window-mark decode, counter bookkeeping,
//! overflow multiplicity, record assembly — is driven natively on the development
//! Mac by a scripted seam. [`crate::sys`] implements the same two traits with real
//! `KVM_RUN` and `perf_event_open` syscalls on Linux.
//!
//! **Untested on silicon.** The loop has never driven a real vCPU; the Altra is not
//! yet in hand. What is tested is everything the seam does not hide, which is the
//! part that decides what a record *says*.
//!
//! # Fail closed, always
//!
//! Every way this loop can fail to *measure* is an error, never a record with a
//! plausible zero in it (`docs/ARM-ALTRA.md` §Evidence integrity #1: a done-marker
//! is never a success condition). A guest that never opened its window, never
//! closed it, never printed its params mode, or never reached the exit sentinel
//! produces a [`RunError`] — there is no path here that invents a count. A record
//! this loop returns is one where a counter was really read at both marks.
//!
//! # What a "delivery" is, and why it is counted here
//!
//! §Evidence integrity #6 wants exactly-once shown **per record**. An armed
//! overflow is delivered exactly when the harness *observes* it leave `KVM_RUN`:
//! as the patched in-kernel [`ExitReason::Preempt`] exit (AA-3), or as the stock
//! host-side [`ExitReason::SignalKick`] (AA-1(c)). So the loop counts mechanism
//! exits while an overflow is armed, and writes that count into
//! [`OverflowRecord::deliveries`]. A lost PMI means no exit ever comes, the guest
//! runs to its sentinel, and the record says `deliveries: 0` — which the floor
//! checker rejects. A duplicate means two exits and `deliveries: 2`. Neither can
//! be smoothed over, because nothing here sums anything: the number in the record
//! is the number of exits this one sample saw.
//!
//! # Every record carries a state digest, and that is not decoration
//!
//! AA-3's replay-identity and AA-6's ≥1,000-rep bit-identity floors are *about* the
//! landed state, so [`Vcpu::state_digest`] is part of the seam and every record
//! carries its result. A rep floor that counted records without ever comparing
//! their digests would be vacuous on the axis it exists for — 1,000 reps with 1,000
//! divergent states would pass it. The digest is therefore a refusal point too: a
//! seam that cannot produce one fails the sample rather than writing an empty
//! string that every comparison would trivially satisfy.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use oracle_model::{Payload, Scale, UART_BASE};
use thiserror::Error;

use crate::console::{Console, Event};
use crate::evidence::{ExitReason, OverflowRecord, RunRecord, StepRecord, StepTransition};

/// The default per-`KVM_RUN` watchdog budget, in seconds — generous enough that a
/// healthy WFI/idle payload waiting on a real timer interrupt never trips it, tight
/// enough that a genuine wedge (a lost interrupt, a WFI with no wake, a livelocked
/// exclusive) is caught in bounded time. Lives here — the portable run module — so the
/// CLI can name it on any target; the Linux seam ([`crate::sys::machine`]) is what
/// arms `ITIMER_REAL` against it. A liveness backstop, not a measurement parameter.
pub const DEFAULT_WATCHDOG_SECS: u64 = 300;

/// The PL011 data register, the guest's one MMIO door for *output*. Every byte the
/// guest "prints" is a store here, and every store is a `KVM_EXIT_MMIO`.
pub const PL011_DR: u64 = UART_BASE;

/// The PL011 flag register (`UART_BASE + 0x18`). The guest **reads** this before
/// every byte (`putb` waits on `TXFF`) and before opening a window (`drain` waits on
/// `BUSY`). There is no in-kernel PL011, so those reads are MMIO exits the harness
/// must answer — see [`PL011_FR_READY`].
pub const PL011_FR: u64 = UART_BASE + 0x18;

/// The value the harness returns for an [`PL011_FR`] read: **zero** — `TXFF` clear
/// (the holding register can always take a byte) and `BUSY` clear (the transmitter
/// is always drained). This makes the guest's `while FR & TXFF {}` and
/// `while FR & BUSY {}` polls single-pass, exactly as QEMU's model does with the
/// FIFO disabled, so the guest behaves identically under TCG and KVM. These polls
/// live *outside* the counting window by construction (`payloads/runtime/src/uart.rs`),
/// so answering them does not touch any counted branch.
pub const PL011_FR_READY: u32 = 0;

/// The PL011 register block is one 4 KiB page at [`UART_BASE`].
const PL011_PAGE: u64 = 0x1000;

/// Whether `addr` falls in the PL011 register page — the harness's one userspace
/// MMIO device. The GIC is the in-kernel vGICv3 (KVM handles it), and the guest's
/// RAM (params/pvclock pages included) is a real memory slot, so neither exits here.
/// Anything outside this page is a genuine finding.
fn is_pl011(addr: u64) -> bool {
    (UART_BASE..UART_BASE + PL011_PAGE).contains(&addr)
}

/// How a `KVM_RUN` returned, as the seam reports it.
///
/// This is the raw shape — what the kernel said — deliberately *not* interpreted.
/// [`crate::sys`] maps `kvm_run.exit_reason` onto it with no smoothing, so the
/// mechanism a record attests is the one the kernel actually returned
/// (§Evidence integrity #4).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VcpuExit {
    /// `KVM_EXIT_MMIO`. `data` carries the bytes written (only writes to
    /// [`PL011_DR`] mean anything to this loop).
    Mmio {
        /// The guest-physical address touched.
        addr: u64,
        /// The bytes written, little-endian, as the kernel staged them.
        data: Vec<u8>,
        /// `false` for a read (the payloads never read the UART).
        is_write: bool,
    },
    /// `KVM_EXIT_MMIO` whose kernel-reported width cannot fit the ABI's
    /// eight-byte data field. Kept distinct so no decoder truncation can turn a
    /// malformed host value into a plausible access.
    MalformedMmio {
        /// Guest-physical address touched.
        addr: u64,
        /// Width reported by the kernel.
        width: u32,
    },
    /// `KVM_EXIT_PREEMPT` (42) — the patched in-kernel force-exit, AA-3's mechanism.
    Preempt,
    /// `KVM_RUN` returned `EINTR`: a host signal kicked the vCPU out. AA-1(c)'s
    /// pre-patch mechanism, and AA-3's forbidden fallback.
    SignalKick,
    /// `KVM_EXIT_DEBUG` — a single-step landed (AA-2).
    Debug,
    /// Anything else, carrying the raw `kvm_run.exit_reason`. Always a finding:
    /// this loop refuses to continue past one.
    Other(u32),
}

/// Why a sample could not be measured.
///
/// Every variant is a refusal to produce a record, never a record with a hole in
/// it. A [`RunRecord`] returned by [`run_sample`] is one whose counts were really
/// read; anything short of that lands here.
#[derive(Debug, Error)]
pub enum RunError {
    /// The seam itself failed (an ioctl or a counter read).
    #[error("{context}: {message}")]
    Seam {
        /// What was being attempted.
        context: &'static str,
        /// What the seam said.
        message: String,
    },
    /// The vCPU stayed inside a single `KVM_RUN` past the per-sample watchdog deadline
    /// — a wedged guest (a lost interrupt, a WFI with no wake, a livelocked exclusive).
    /// The watchdog converts the hang into this error so the caller can record the
    /// failed attempt and keep the evidence total, rather than blocking forever.
    #[error("the vCPU wedged inside KVM_RUN past the {secs}s watchdog deadline (no exit returned)")]
    Watchdog {
        /// The deadline that was exceeded, in seconds.
        secs: u64,
    },
    /// The guest reached its exit sentinel without ever opening the counting
    /// window. There is no count to record, and a zero would be a lie.
    #[error("the guest exited without opening its counting window (no MARK_BEGIN)")]
    NoWindowOpen,
    /// The window opened but never closed.
    #[error("the guest exited without closing its counting window (no MARK_END)")]
    NoWindowClose,
    /// The window closed before it opened, or twice.
    #[error("malformed window: {0}")]
    MalformedWindow(&'static str),
    /// The guest never printed the `PARAMS mode=` line, so the harness cannot
    /// attest which params page the payload actually saw — and an unattested
    /// params mode is exactly how a smoke-scale run masquerades as a 1e8 one.
    #[error("the guest never attested its params mode (no `PARAMS mode=` line)")]
    NoParamsMode,
    /// A payload whose count includes an in-band runtime term (`STXR`/seqlock retries)
    /// never printed its `retries=` line. The reported count is unaccountable — a
    /// defaulted 0 would let the record claim a term it never made.
    #[error(
        "payload {0:?} has a reported retry term but never printed a `retries=` line: the \
         count is unaccountable, and a defaulted 0 would be a fabricated report"
    )]
    MissingReportedTerm(Payload),
    /// The `scale=` the guest printed on its `PARAMS` line disagrees with the scale the
    /// harness is recording this sample under. The guest reports what it *actually saw*
    /// on the params page; a mismatch means the record would be labelled with an input the
    /// guest never ran (a stale/mis-written page), so the count and replay checks would
    /// grade evidence attributed to the wrong scale.
    #[error(
        "the guest ran scale {found:?} but this sample is recorded as {expected:?}: the params \
         page the guest saw disagrees with the sample spec — the record would be mislabelled"
    )]
    ReportedScaleMismatch {
        /// The scale the harness intended (from the sample spec).
        expected: &'static str,
        /// The `scale=` token the guest actually printed, if any.
        found: Option<String>,
    },
    /// The `seed=` the guest printed disagrees with the seed the harness is recording this
    /// sample under — the same mislabelling risk as [`RunError::ReportedScaleMismatch`],
    /// and it is exactly the seed-ignoring payloads (`straight-line`) whose counts and
    /// replay would still pass on the wrong seed, so the cross-check is what catches it.
    #[error(
        "the guest ran seed {found:?} but this sample is recorded as {expected:#x}: the params \
         page the guest saw disagrees with the sample spec — the record would be mislabelled"
    )]
    ReportedSeedMismatch {
        /// The seed the harness intended (from the sample spec).
        expected: u64,
        /// The `seed=` token the guest actually printed, if any.
        found: Option<String>,
    },
    /// The guest never reached `PAYLOAD EXIT`.
    #[error("the guest never reached its exit sentinel")]
    NoExitSentinel,
    /// A mechanism exit (`Preempt`/`SignalKick`) arrived with no overflow armed —
    /// a kick this loop did not ask for. Never silently absorbed.
    #[error("a mechanism exit ({0:?}) arrived with no overflow armed: an unexplained kick")]
    UnexpectedMechanismExit(ExitReason),
    /// A single-step landing arrived, but this loop never arms guest debug. AA-2
    /// owns stepping; a `KVM_EXIT_DEBUG` here is an unexplained exit, and counting
    /// it as an overflow delivery would conflate two different mechanisms.
    #[error("a KVM_EXIT_DEBUG (single-step) landing arrived, but this loop arms no guest debug")]
    UnexpectedDebugExit,
    /// A single step landed with `PC` outside the mapped guest RAM, so the stepped
    /// opcode cannot be read. AA-2 classifies a step from the instruction at
    /// `pc_before`; a `PC` that fell out of the slot is a finding (a wild step), never
    /// a plausible zero opcode.
    #[error(
        "a single step's pc_before {pc:#x} is outside mapped guest RAM: the stepped opcode \
         cannot be read, and a step off the mapping is a finding, not a decodable instruction"
    )]
    StepPcUnmapped {
        /// The `pc_before` that fell outside guest RAM.
        pc: u64,
    },
    /// `BR_RETIRED` went backwards across a single step. The work counter is
    /// monotonic while the guest runs; a decrease across one stepped instruction is a
    /// seam/hardware anomaly, refused rather than recorded as a huge wrapped delta.
    #[error("BR_RETIRED went backwards across a single step: before {before}, after {after}")]
    StepCounterWentBackwards {
        /// The counter before the step.
        before: u64,
        /// The counter after the step.
        after: u64,
    },
    /// The guest touched an MMIO address that is not the console.
    #[error("the guest touched {addr:#x}, which is not the PL011 data register")]
    UnexpectedMmio {
        /// The address touched.
        addr: u64,
    },
    /// The host supplied an MMIO width outside the KVM ABI's 1..=8-byte field.
    #[error("KVM reported malformed MMIO at {addr:#x} with width {width}")]
    MalformedMmio {
        /// Guest-physical address touched.
        addr: u64,
        /// Width reported by the kernel.
        width: u32,
    },
    /// `KVM_RUN` returned an exit reason this loop does not handle.
    #[error("unhandled KVM exit reason {0}")]
    UnexpectedExit(u32),
    /// The counter went backwards across the window.
    #[error("the work counter went backwards: work_end {end} < work_begin {begin}")]
    CounterWentBackwards {
        /// The counter at `MARK_BEGIN`.
        begin: u64,
        /// The counter at `MARK_END`.
        end: u64,
    },
    /// The seam produced an empty state digest. An empty digest compares equal to
    /// every other empty digest, so it would make the replay-identity and rep
    /// floors pass without measuring anything — the exact vacuity this refusal
    /// exists to prevent.
    #[error(
        "the seam produced an empty state digest: a digest that cannot diverge is not evidence"
    )]
    EmptyStateDigest,
    /// The vCPU kept exiting on unrelated host IRQs without the work counter ever
    /// reaching the target. Expected in small numbers (the host timer ticks); an
    /// unbounded stream of them with no progress means the guest is not advancing,
    /// and a measurement loop that spins forever is worse than one that fails.
    #[error(
        "{exits} advisory exits with no progress: the work counter is at {work}, target {target}. \
         The guest is not advancing"
    )]
    AdvisoryExitStorm {
        /// How many advisory exits were taken.
        exits: u64,
        /// Where the counter had reached.
        work: u64,
        /// Where it needed to reach.
        target: u64,
    },
    /// The AA-3 exact-landing arm-early period underflows: the drawn work `delta` is not
    /// strictly greater than the `skid_margin`, so the overflow cannot be armed a full margin
    /// **below** the target. Without room below the target the arm-early `Preempt` cannot be
    /// made to fire below it, and single-stepping up to an exact landing is impossible for this
    /// window — refused rather than armed at (or above) the target and silently degraded to the
    /// arm-at-target reliability proxy. The plan draws AA-3 deltas above the margin; a cell
    /// whose landable window is itself below the margin surfaces here.
    #[error(
        "exact landing: work delta {delta} is not greater than the skid margin {skid_margin}, so \
         the overflow cannot be armed a full margin below the target — this window is too small \
         to land exactly with this margin"
    )]
    ExactLandingWindowTooSmall {
        /// The drawn work delta (`target - work_begin`).
        delta: u64,
        /// The skid margin the overflow must be armed below the target by.
        skid_margin: u64,
    },
    /// The configured skid margin cannot be combined with the fixed canonical-landing headroom
    /// in `u64`. Refuse before subtracting so a hostile CLI value cannot panic in debug or wrap
    /// into a small, apparently valid margin in release.
    #[error(
        "exact landing: skid margin {skid_margin} plus the fixed landing headroom overflows u64"
    )]
    ExactLandingMarginOverflow {
        /// The configured skid margin.
        skid_margin: u64,
    },
    /// The AA-3 arm-early `Preempt` fired AT or ABOVE the target. The overflow was armed
    /// `skid_margin + LANDING_HEADROOM` below the target so it would fire STRICTLY below it,
    /// leaving room to single-step up to the canonical landing PC. Firing at or above the target
    /// means the skid exceeded margin+headroom: the landing would be BR-exact but at a
    /// PC-non-canonical point on the target's branchless plateau (which same-seed reps need not
    /// share), breaking replay identity — so it fails closed rather than recording it.
    #[error(
        "exact landing: the arm-early Preempt fired at work {landed}, at or above the target \
         {target} (skid margin {skid_margin} + headroom too small): the skid left no room to \
         single-step up to the canonical landing PC"
    )]
    ExactLandingKickAtOrAboveTarget {
        /// The exact landing target (`work_begin + delta`).
        target: u64,
        /// Where the `Preempt` actually fired.
        landed: u64,
        /// The skid margin the overflow was armed below the target by.
        skid_margin: u64,
    },
    /// A single step during the AA-3 landing moved the work counter PAST the target. A single
    /// step advances `BR_RETIRED` by 0 or 1 (AA-2), so it cannot skip the target — a step that
    /// did is a real seam/hardware anomaly, refused rather than recorded as an exact landing it
    /// is not.
    #[error(
        "exact landing: a single step advanced the work counter to {landed}, past the target \
         {target} — a single step advances BR_RETIRED by 0 or 1, so it cannot skip the target"
    )]
    ExactLandingOvershotTarget {
        /// The exact landing target.
        target: u64,
        /// Where the counter landed after the step (`> target`).
        landed: u64,
    },
    /// The AA-3 landing single-step loop took too many steps without the work counter reaching
    /// the target — a livelocked or stuck counter. A landing loop that spins forever is worse
    /// than one that fails, so it is bounded and refused.
    #[error(
        "exact landing: {steps} single steps without reaching the target (work {work}, target \
         {target}): the counter is not advancing — refused rather than stepped forever"
    )]
    ExactLandingStepStorm {
        /// How many single steps were taken.
        steps: u64,
        /// Where the counter had reached.
        work: u64,
        /// The exact landing target.
        target: u64,
    },
    /// The exact-landing walk consumed its caller-supplied `KVM_RUN` budget before reaching
    /// the target. Linux boot shares this primitive with AA-3, and its total exit ceiling must
    /// remain effective while the vCPU is being single-stepped (MMIO exits included).
    #[error(
        "exact landing: consumed the {limit}-exit landing budget before reaching work target \
         {target} — refused rather than running an unbounded single-step/MMIO loop"
    )]
    ExactLandingExitLimit {
        /// Maximum `KVM_RUN` calls permitted inside this landing walk.
        limit: u64,
        /// The exact work target the walk had not yet reached.
        target: u64,
    },
    /// A stock signal-kick arrived on the AA-3 exact-landing path. The exact landing rides the
    /// patched in-kernel `Preempt` force-exit ALONE; the signal kick is AA-3's forbidden
    /// fallback (`docs/ARM-ALTRA.md` §AA-3), so it is refused rather than landed on.
    #[error(
        "exact landing: a stock signal-kick arrived, but the exact landing rides the patched \
         Preempt force-exit alone — the signal kick is AA-3's forbidden fallback"
    )]
    ExactLandingSignalKick,
}

/// A single vCPU, as this loop needs it.
///
/// The real implementation is `KVM_RUN` on a vCPU fd ([`crate::sys`]); the test
/// implementation is a scripted list of exits. Both are exactly this wide.
pub trait Vcpu {
    /// Enter the guest and return at the next exit.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the ioctl itself failed.
    fn run(&mut self) -> Result<VcpuExit, RunError>;

    /// Hand the value of an MMIO **read** back to the guest, so the next [`Vcpu::run`]
    /// resumes with it (the KVM MMIO-read protocol: userspace fills the shared
    /// `kvm_run.mmio.data` and re-enters). Called only after a read exit — the guest
    /// polls the PL011 flag register before it can print, and with no in-kernel PL011
    /// the harness is what answers.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the value could not be staged.
    fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError>;

    /// A digest of the guest's architectural state — the registers and memory that
    /// AA-3's replay-identity and AA-6's bit-identity floors compare.
    ///
    /// Sampled **at the landing** (for armed runs) and again at the exit sentinel.
    /// The landing one is the one AA-3's claim is about: two runs of the same seed
    /// must be in the same state at the same Moment, and a digest taken after the
    /// guest resumed can converge — two different landed states can reach the same
    /// final state — so it cannot establish landing identity.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the state could not be read. A sample whose state could
    /// not be digested is a sample that cannot testify to determinism, and is
    /// refused rather than recorded with a blank.
    fn state_digest(&mut self) -> Result<String, RunError>;
}

/// The work counter (raw `BR_RETIRED`, armed guest-only and pinned).
pub trait WorkCounter {
    /// The counter's current value.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the counter could not be read.
    fn read(&mut self) -> Result<u64, RunError>;

    /// Arm a one-shot overflow `delta` events from now, and start counting.
    ///
    /// Called **at `MARK_BEGIN`**, never before: the counting window opens at the
    /// mark, and a deadline armed at open time would count the guest's whole boot
    /// against it — a small delta (the plan draws from 1) would then overflow during
    /// boot, and the kick would arrive before anything was armed.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the event could not be armed.
    fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError>;

    /// Re-arm the one-shot after an **advisory** exit — one that left `KVM_RUN` while
    /// armed but before the counter reached the target.
    ///
    /// The patch's arm64 arch-difference makes this necessary: the PMU overflow is an
    /// ordinary maskable IRQ there, so the armed vCPU exits on *any* host IRQ and the
    /// kernel clears its one-shot flag on the way out. Without a re-arm the real
    /// overflow, when it comes, would find nothing armed and pass straight through.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the one-shot could not be re-armed.
    fn rearm(&mut self) -> Result<(), RunError>;

    /// Put the counter back into free-running counting mode after the deadline has
    /// been delivered.
    ///
    /// The one-shot **disables the event** when it overflows. Without this the
    /// counter would freeze at the landing, `work_end` would read the landing value
    /// rather than the window's true end, and every armed record's count would
    /// disagree with the oracle — which grades the *whole window*.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the counter could not be resumed.
    fn resume_counting(&mut self) -> Result<(), RunError>;
}

/// How many advisory exits one sample may take before the harness gives up.
///
/// Advisory exits are expected (the host timer ticks), but an unbounded stream of
/// them with no progress means the guest is not advancing — and a measurement loop
/// that spins forever is worse than one that fails. The bound is generous: a 1e8-branch
/// window at a 1 kHz tick produces on the order of a thousand.
const MAX_ADVISORY_EXITS: u64 = 100_000;

/// How many single steps the AA-3 exact-landing loop may take before giving up.
///
/// The landing walks `BR_RETIRED` up from the arm-early `Preempt` (below the target) to the
/// target — a distance of at most `skid_margin` retired branches — one guest instruction at a
/// time. A tight counting window retires branches every few instructions, so a real landing is
/// tens to hundreds of steps; this bound is a generous livelock/stuck-counter backstop (AA-3's
/// guest is LSE-only, so the `llsc` single-step livelock does not apply, but the loop is bounded
/// defensively so a counter that never advances to the target fails rather than spins forever).
const MAX_LANDING_STEPS: u64 = 1_000_000;

/// Hard ceiling on all `KVM_RUN` calls made by one AA-3 landing walk. A stepped MMIO
/// instruction can produce an MMIO exit followed by its debug exit, so this is deliberately
/// larger than [`MAX_LANDING_STEPS`] while still making the loop total.
const MAX_LANDING_RUN_EXITS: u64 = 2_000_000;

/// Extra `BR_RETIRED` headroom, ADDED to the measured `skid_margin`, that the exact-landing
/// overflow is armed below the target.
///
/// `BR_RETIRED` does not uniquely pin `PC`: it ticks only on retired branches, so across a
/// branchless run (clock-page's seqlock body is ~10 non-branch instructions between its loop
/// branches) many consecutive `PC`s share one `BR_RETIRED` value — a *plateau*. The exact
/// landing's canonical point is the FIRST instruction at which `work == target`, i.e. the one
/// immediately after the target-th retiring branch; the single-step-up loop reaches it from ANY
/// start strictly below the target (the deterministic instruction stream converges there). But
/// an arm-early `Preempt` that fires AT `work == target` lands at an ARBITRARY `PC` inside the
/// plateau — BR-exact yet PC-non-canonical — so two same-seed reps that land one via a step-up
/// and one via that boundary digest DIFFERENT `PC`s (everything else, RAM included, is
/// bit-identical). The measured `skid_margin` alone leaves the `Preempt` able to fire exactly at
/// the target (skid == margin); this headroom arms `skid_margin + LANDING_HEADROOM` below it so
/// the `Preempt` fires STRICTLY below the target with room to single-step up to the canonical
/// `PC`. Landing at/above the target then never occurs on the measured skid and is kept as a
/// fail-closed anomaly (the skid exceeded margin+headroom), not silently accepted. The cost is a
/// few extra single steps per landing; the payoff is a canonical, path-independent landing `PC`.
pub const LANDING_HEADROOM: u64 = 16;

pub(crate) fn exact_arm_delta(delta: u64, skid_margin: u64) -> Result<u64, RunError> {
    let early_by = skid_margin
        .checked_add(LANDING_HEADROOM)
        .ok_or(RunError::ExactLandingMarginOverflow { skid_margin })?;
    match delta.checked_sub(early_by) {
        Some(arm_delta) if arm_delta >= 1 => Ok(arm_delta),
        _ => Err(RunError::ExactLandingWindowTooSmall { delta, skid_margin }),
    }
}

/// Result of servicing one patched [`VcpuExit::Preempt`] for an exact work deadline.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ExactPreemptOutcome {
    /// An unrelated host IRQ forced the armed vCPU out before the early arm point. The caller
    /// should continue; [`WorkCounter::rearm`] has already restored the patch's one-shot.
    Advisory {
        /// Work observed at the advisory exit.
        work: u64,
    },
    /// The overflow arrived below the target and the vCPU was single-stepped to its canonical
    /// first instruction boundary at exactly `target`.
    Landed {
        /// Number of retired instructions single-stepped (debug exits).
        single_steps: u64,
        /// Total `KVM_RUN` calls inside the walk, including serviced MMIO exits.
        run_exits: u64,
    },
}

/// Service an armed patched-Preempt exit and, when it is the real overflow, land exactly.
///
/// This is the common AA-3/AA-5 primitive: arm-early overflow, advisory-host-IRQ handling, and
/// single-step convergence to the first instruction boundary whose work count is `target`.
/// `service_mmio` is deliberately supplied by the caller because the bare payload and Linux
/// expose different PL011 surfaces. It runs only while the vCPU is stopped.
pub(crate) fn service_exact_preempt<V, F, E>(
    vcpu: &mut V,
    counter: &mut impl WorkCounter,
    target: u64,
    arm_point: u64,
    skid_margin: u64,
    max_run_exits: u64,
    mut service_mmio: F,
) -> Result<ExactPreemptOutcome, E>
where
    V: StepVcpu,
    F: FnMut(&mut V, u64, &[u8], bool) -> Result<(), E>,
    E: From<RunError>,
{
    let mut work = counter.read()?;
    if work < arm_point {
        counter.rearm()?;
        return Ok(ExactPreemptOutcome::Advisory { work });
    }
    if work >= target {
        return Err(RunError::ExactLandingKickAtOrAboveTarget {
            target,
            landed: work,
            skid_margin,
        }
        .into());
    }

    counter.resume_counting()?;
    vcpu.arm_single_step()?;
    let mut single_steps = 0;
    let mut run_exits = 0;
    while work != target {
        if run_exits == max_run_exits {
            return Err(RunError::ExactLandingExitLimit {
                limit: max_run_exits,
                target,
            }
            .into());
        }
        run_exits += 1;
        match vcpu.run()? {
            VcpuExit::Debug => {}
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                service_mmio(vcpu, addr, &data, is_write)?;
                continue;
            }
            other => {
                return Err(RunError::Seam {
                    context: "exact-landing single step expected KVM_EXIT_DEBUG or serviced MMIO",
                    message: format!("got a non-debug non-MMIO exit: {other:?}"),
                }
                .into());
            }
        }

        single_steps += 1;
        if single_steps > MAX_LANDING_STEPS {
            return Err(RunError::ExactLandingStepStorm {
                steps: single_steps,
                work,
                target,
            }
            .into());
        }
        let after = counter.read()?;
        if after < work {
            return Err(RunError::StepCounterWentBackwards {
                before: work,
                after,
            }
            .into());
        }
        if after > target {
            return Err(RunError::ExactLandingOvershotTarget {
                target,
                landed: after,
            }
            .into());
        }
        work = after;
    }
    vcpu.disarm_single_step()?;
    Ok(ExactPreemptOutcome::Landed {
        single_steps,
        run_exits,
    })
}

/// What one sample is asked to be: which payload, at what scale and seed, and — if
/// this is an overflow run — how far past the window's opening to arm the deadline.
#[derive(Clone, Debug)]
pub struct SampleSpec {
    /// Dense index into the run-set's attempted samples.
    pub sample_id: u64,
    /// The payload.
    pub payload: Payload,
    /// The scale.
    pub scale: Scale,
    /// The seed the params page carries.
    pub seed: u64,
    /// The trip count the params page carries.
    pub trips: u64,
    /// The experimental condition (`pinned-solo`, `co-tenant-load`, …).
    pub condition: String,
    /// Work events past `MARK_BEGIN` to arm the overflow at. `None` in counting
    /// mode (AA-1(b)): no deadline is armed and no mechanism exit is expected.
    pub target_delta: Option<u64>,
    /// The migration-probe handle, when AA-1's churner is running. `run_sample` reads it at
    /// `arm_overflow` and at the landing to attest a move that fell STRICTLY inside this
    /// sample's armed interval — not one that merely happened during the sample. `None`
    /// outside the migration probe.
    pub migration_probe: Option<ArmedMigrationProbe>,
}

/// Reads the migration churner's live move counter so [`run_sample`] can attest an affinity
/// move that falls strictly inside a sample's ARMED interval (`arm_overflow` → landing).
///
/// A whole-sample before/after count is not that: VM/vGIC creation, image load, perf setup,
/// and the guest's own boot all precede the arm, and the churner moves every 200µs, so a
/// sample-wide count is satisfied by boot-time moves even when the short armed window saw
/// none — vacuously satisfying AA-1's required armed-migration probe. Snapshotting at the arm
/// and reading again at the landing bounds the observation to the interval that matters.
#[derive(Clone, Debug)]
pub struct ArmedMigrationProbe {
    moves: Arc<AtomicU64>,
    observed: Arc<AtomicBool>,
}

impl ArmedMigrationProbe {
    /// Wrap the churner's live move counter (see [`crate::sys::MigrationChurner::moves_handle`]).
    #[must_use]
    pub fn new(moves: Arc<AtomicU64>) -> Self {
        Self {
            moves,
            observed: Arc::new(AtomicBool::new(false)),
        }
    }

    fn moves(&self) -> u64 {
        self.moves.load(Ordering::Relaxed)
    }

    fn mark_observed(&self) {
        self.observed.store(true, Ordering::Relaxed);
    }

    /// Whether any sample saw an affinity move within its armed interval.
    #[must_use]
    pub fn observed(&self) -> bool {
        self.observed.load(Ordering::Relaxed)
    }
}

/// The overflow bookkeeping a run accumulates (§Evidence integrity #6), shared by both
/// landing policies. [`run_sample`] fills it via the advisory (arm-at-target) path;
/// [`run_sample_exact`] via the exact (arm-early + single-step) path. Either way the
/// **record assembly** below is identical, so both build this and hand it to
/// [`assemble_measured_record`].
struct OverflowBookkeeping {
    /// The armed deadline (`work_begin + delta`), or `None` for an unarmed counting run.
    target: Option<u64>,
    /// How many mechanism exits were counted as deliveries (multiplicity grades this).
    deliveries: u64,
    /// How many mechanism exits were advisory (below the arm point / target) and re-armed.
    advisory_exits: u64,
    /// Where the delivery landed (`work` at the landing), or `None` for a lost PMI.
    landed: Option<u64>,
    /// The state digest AT the landing Moment (what replay identity compares).
    landed_digest: Option<String>,
    /// The mechanism reason of the landing (`Preempt`/`SignalKick`), or `None` if the run
    /// reached its console sentinel without a delivery — then the record's exit is `Mmio`.
    mechanism_exit: Option<ExitReason>,
}

/// Everything the shared record-assembly reads that the run loop accumulated, beyond the
/// overflow bookkeeping — bundled so the assembler takes three arguments, not a dozen.
struct SampleAssembly {
    status: Option<u8>,
    work_begin: Option<u64>,
    work_end: Option<u64>,
    params_mode: Option<String>,
    reported_scale: Option<String>,
    reported_seed: Option<String>,
    clockpage_mode: Option<String>,
    reported: Option<u64>,
    overflow: OverflowBookkeeping,
}

/// Service a PL011 **register read** (the guest polling the flag register before it prints).
///
/// The one MMIO read both the counting loop and the exact-landing single-step loop must
/// answer identically: the PL011 is the harness's only userspace MMIO device, so a guest
/// boots by configuring it and polling `PL011_FR` before every byte, and with no in-kernel
/// PL011 every poll is an exit that must be answered "ready" or `KVM_RUN` resumes with stale
/// data. Anything outside the PL011 page is a genuine finding (the GIC is the in-kernel vGIC,
/// RAM is a real slot), refused. Returns `Ok(true)` if it serviced a PL011 read (caller
/// `continue`s), `Ok(false)` if it was a PL011 WRITE (caller handles the byte), `Err` if the
/// access was not the PL011 at all.
fn service_pl011_read(
    vcpu: &mut impl Vcpu,
    addr: u64,
    data: &[u8],
    is_write: bool,
) -> Result<bool, RunError> {
    if !is_pl011(addr) {
        return Err(RunError::UnexpectedMmio { addr });
    }
    if !is_write {
        let width = data.len().clamp(1, 8);
        vcpu.complete_mmio_read(&PL011_FR_READY.to_le_bytes()[..width.min(4)])?;
        return Ok(true);
    }
    Ok(false)
}

/// Assemble the [`RunRecord`] from a completed run's accumulated state — the shared tail of
/// both landing policies.
///
/// This is the "attestation / digest / record" path the two run functions must keep in
/// lockstep: the fail-closed refusals (no sentinel / no window / counter backwards / no
/// params), the guest-attested scale+seed cross-checks (the only thing that catches a stale
/// seed on a seed-ignoring payload), the reported-term accounting, the sentinel state digest,
/// and the overflow sub-record. Single-sourced so a change here cannot land in one policy and
/// not the other. The **only** thing the two policies decide is the OverflowBookkeeping they
/// hand in (arm-at-target advisory skid vs arm-early exact `skid == 0`) — the record shape is
/// one.
fn assemble_measured_record(
    vcpu: &mut impl Vcpu,
    spec: &SampleSpec,
    a: SampleAssembly,
) -> Result<RunRecord, RunError> {
    let status = a.status.ok_or(RunError::NoExitSentinel)?;
    let begin = a.work_begin.ok_or(RunError::NoWindowOpen)?;
    let end = a.work_end.ok_or(RunError::NoWindowClose)?;
    if end < begin {
        return Err(RunError::CounterWentBackwards { begin, end });
    }
    let params_mode = a.params_mode.ok_or(RunError::NoParamsMode)?;

    // Cross-check the scale and seed the GUEST attested against the sample spec. The guest
    // prints what it read off the params page; if that disagrees with the (payload, scale,
    // seed) this record is labelled with, the page was stale or mis-written and the record
    // would attribute its counts to an input the guest never ran. It is precisely the
    // seed-ignoring payloads whose counts still pass on a wrong seed, so this is the only
    // thing that catches a stale seed on them.
    match a.reported_scale.as_deref() {
        Some(s) if s == spec.scale.name() => {}
        found => {
            return Err(RunError::ReportedScaleMismatch {
                expected: spec.scale.name(),
                found: found.map(str::to_string),
            });
        }
    }
    match a.reported_seed.as_deref().map(parse_hex_u64) {
        Some(Some(seed)) if seed == spec.seed => {}
        _ => {
            return Err(RunError::ReportedSeedMismatch {
                expected: spec.seed,
                found: a.reported_seed,
            });
        }
    }

    // A payload whose count includes an in-band runtime term (`STXR`/seqlock retries) MUST
    // have printed it. Defaulting to 0 would let the record match the oracle while claiming a
    // reported term it never made. A payload with no reported term that printed none reports 0.
    let reported_taken = if spec.payload.has_reported_term() {
        a.reported
            .ok_or(RunError::MissingReportedTerm(spec.payload))?
    } else {
        a.reported.unwrap_or(0)
    };

    // The landed state, digested at the sentinel — read from the seam, never synthesised.
    let state_digest = vcpu.state_digest()?;
    if state_digest.is_empty() {
        return Err(RunError::EmptyStateDigest);
    }

    let ov = a.overflow;
    let overflow = ov.target.map(|target| {
        // A lost PMI means no exit ever came: `landed` is None and `deliveries` is 0. The
        // record says so — it does not quietly substitute the window's end. When a landing WAS
        // made the skid is `landed - target` (0 by construction for the exact policy).
        let landed = ov.landed.unwrap_or(0);
        OverflowRecord {
            armed: true,
            deliveries: ov.deliveries,
            advisory_exits: ov.advisory_exits,
            target,
            landed,
            skid: i64::try_from(i128::from(landed) - i128::from(target)).unwrap_or(i64::MIN),
            landed_digest: ov.landed_digest.unwrap_or_default(),
        }
    });

    Ok(RunRecord {
        sample_id: spec.sample_id,
        payload: spec.payload,
        scale: spec.scale,
        seed: spec.seed,
        trips: spec.trips,
        condition: spec.condition.clone(),
        work_begin: begin,
        work_end: end,
        measured_taken: end - begin,
        reported_taken,
        // With no delivery the run ended at the console sentinel: the record says `Mmio`
        // rather than borrowing a mechanism it never exercised.
        exit_reason: ov.mechanism_exit.unwrap_or(ExitReason::Mmio),
        overflow,
        // These counting/landing loops measure windows, not steps — never step evidence.
        step: None,
        state_digest,
        params_mode,
        clockpage_mode: a.clockpage_mode,
        payload_status: i32::from(status),
    })
}

/// Run one sample to completion and assemble its [`RunRecord`].
///
/// The loop: enter the guest; on each MMIO store to [`PL011_DR`] feed the byte to
/// the [`Console`] decoder; sample the work counter at the two window marks; on a
/// mechanism exit record the landing and re-enter; stop at the exit sentinel.
///
/// # Errors
///
/// [`RunError`] whenever the sample could not be *measured* — see the type. The
/// loop never returns a record it did not read a counter for.
pub fn run_sample(
    vcpu: &mut impl Vcpu,
    counter: &mut impl WorkCounter,
    spec: &SampleSpec,
) -> Result<RunRecord, RunError> {
    let mut console = Console::new();
    let mut work_begin: Option<u64> = None;
    let mut work_end: Option<u64> = None;
    let mut params_mode: Option<String> = None;
    // The scale and seed the guest attests it saw on the params page, cross-checked against
    // the sample spec at assembly so a stale/mis-written page cannot mislabel the record.
    let mut reported_scale: Option<String> = None;
    let mut reported_seed: Option<String> = None;
    let mut clockpage_mode: Option<String> = None;
    // `None` until the guest prints its `retries=` term. A payload with a reported term
    // that never supplied one is refused below — a defaulted 0 would let the record claim
    // an in-band count that was never observed.
    let mut reported: Option<u64> = None;
    let mut status: Option<u8> = None;

    // Overflow bookkeeping, per record (§Evidence integrity #6).
    let mut target: Option<u64> = None;
    let mut deliveries: u64 = 0;
    let mut advisory_exits: u64 = 0;
    let mut landed: Option<u64> = None;
    let mut landed_digest: Option<String> = None;
    let mut mechanism_exit: Option<ExitReason> = None;
    // The churner's move count AT the arm — the lower bound of the armed interval. A move
    // between here and the landing migrated a LIVE armed context, the rr #3607 mode AA-1
    // probes; a move before the arm (boot, setup) does not, and must not count.
    let mut moves_at_arm: Option<u64> = None;

    'run: while status.is_none() {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                // The PL011 is the harness's one userspace MMIO device (guests boot by
                // configuring it and polling its flag register before every byte). Service a
                // register read here; a non-PL011 access is a genuine finding, refused.
                if service_pl011_read(vcpu, addr, &data, is_write)? {
                    continue 'run;
                }

                if addr != PL011_DR {
                    // A config-register write (CR/IBRD/FBRD/LCR_H). The harness models
                    // no baud/line state; the write is accepted and ignored. It is not
                    // a finding — the guest must configure the UART before it prints.
                    continue 'run;
                }

                // A data-register store carries its byte in the low lane, whatever the
                // access width. A zero-length store is not a byte the guest printed —
                // it is a malformed exit, refused rather than skipped, because skipping
                // it would silently drop console content the attestations are read from.
                let Some(&byte) = data.first() else {
                    return Err(RunError::UnexpectedMmio { addr });
                };
                match console.push(byte) {
                    Some(Event::MarkBegin) => {
                        if work_begin.is_some() {
                            return Err(RunError::MalformedWindow("MARK_BEGIN twice"));
                        }
                        let begin = counter.read()?;
                        work_begin = Some(begin);
                        // Arm the deadline *at the mark*, so the target is measured
                        // from the window's opening, not from an arbitrary earlier
                        // moment: `target = work_begin + delta`.
                        if let Some(delta) = spec.target_delta {
                            counter.arm_overflow(delta)?;
                            target = Some(begin.saturating_add(delta));
                            // Open the armed interval for the migration probe here, at the arm.
                            moves_at_arm = spec
                                .migration_probe
                                .as_ref()
                                .map(ArmedMigrationProbe::moves);
                        }
                    }
                    Some(Event::MarkEnd) => {
                        if work_begin.is_none() {
                            return Err(RunError::MalformedWindow("MARK_END before MARK_BEGIN"));
                        }
                        if work_end.is_some() {
                            return Err(RunError::MalformedWindow("MARK_END twice"));
                        }
                        work_end = Some(counter.read()?);
                    }
                    Some(Event::Line(line)) => {
                        absorb_line(
                            &line,
                            &mut params_mode,
                            &mut reported_scale,
                            &mut reported_seed,
                            &mut clockpage_mode,
                            &mut reported,
                        );
                    }
                    Some(Event::Exit(code)) => {
                        status = Some(code);
                        break 'run;
                    }
                    None => {}
                }
            }

            // A single-step landing this loop never asked for. AA-2 owns stepping;
            // absorbing a debug exit here would let a stepped run masquerade as an
            // overflow delivery, which is two mechanisms wearing one name.
            VcpuExit::Debug => return Err(RunError::UnexpectedDebugExit),

            // A mechanism exit. It is NOT automatically a delivery: on arm64 the PMU
            // overflow is an ordinary maskable IRQ, so the patch's armed vCPU exits on
            // ANY host IRQ (the timer tick included) and the patch's own commit
            // message says to treat every KVM_EXIT_PREEMPT as ADVISORY — re-read the
            // work counter, and re-arm if the target has not been reached.
            //
            // Counting these exits as deliveries would record an early timer tick as
            // an exactly-once PMI, landed at a count that is not the target, while the
            // real overflow was never surfaced. So the counter decides, not the exit.
            exit @ (VcpuExit::Preempt | VcpuExit::SignalKick) => {
                let reason = if exit == VcpuExit::Preempt {
                    ExitReason::Preempt
                } else {
                    ExitReason::SignalKick
                };
                let Some(target) = target else {
                    // Nothing was armed, so nothing should have kicked. An
                    // unexplained kick is never absorbed into a clean record.
                    return Err(RunError::UnexpectedMechanismExit(reason));
                };

                let work = counter.read()?;
                // The advisory path is PREEMPT-ONLY. The patch's armed vCPU exits on ANY host
                // IRQ, so a `KVM_EXIT_PREEMPT` below the target is an unrelated IRQ (a timer
                // tick), not the overflow — re-read, re-arm, continue. A stock `SignalKick`
                // reaching here is SOURCE-VERIFIED (the run loop discarded foreign signals and
                // only surfaces a kick sourced by the armed perf fd), so it is raised ONLY by
                // the perf overflow: a kick below the target is the overflow landing EARLY —
                // the negative-skid case AA-1 exists to measure, not advisory. And `rearm` is a
                // no-op for the stock mechanism, so treating it as advisory would hang the
                // sample waiting for a second signal that never comes.
                if work < target && exit == VcpuExit::Preempt {
                    advisory_exits += 1;
                    if advisory_exits > MAX_ADVISORY_EXITS {
                        return Err(RunError::AdvisoryExitStorm {
                            exits: advisory_exits,
                            work,
                            target,
                        });
                    }
                    counter.rearm()?;
                    continue;
                }

                deliveries += 1;
                // The FIRST landing is the one the contract is about; a second is a
                // duplicate delivery, and it is `deliveries` — not these fields — that
                // makes it visible.
                if landed.is_none() {
                    landed = Some(work);
                    mechanism_exit = Some(reason);
                    // Close the armed interval AT THE LANDING: if the churner moved the thread
                    // between the arm and this landing, a live armed context migrated across
                    // cores. The NO-DELIVERY case (a lost PMI) is closed symmetrically after the
                    // run loop — see below — since the probe exists to observe exactly that.
                    if let (Some(at_arm), Some(probe)) =
                        (moves_at_arm, spec.migration_probe.as_ref())
                        && probe.moves() > at_arm
                    {
                        probe.mark_observed();
                    }
                    // Digest the state HERE — at the landing, before the guest is
                    // resumed. This is the state AA-3's replay identity is about; the
                    // one at the exit sentinel can converge from different landings.
                    landed_digest = Some(vcpu.state_digest()?);
                    // The one-shot disabled the counter when it overflowed. Resume it,
                    // or `work_end` freezes at the landing and the record's count is of
                    // a fraction of the window while the oracle grades the whole of it.
                    counter.resume_counting()?;
                }
            }

            VcpuExit::MalformedMmio { addr, width } => {
                return Err(RunError::MalformedMmio { addr, width });
            }
            VcpuExit::Other(reason) => return Err(RunError::UnexpectedExit(reason)),
        }
    }

    // A LOST PMI closes the armed interval too. `mark_observed` at the landing fires only on a
    // delivery; but the migration probe exists to observe exactly the NO-DELIVERY case — an
    // armed overflow that a cross-core migration caused KVM to MISS (rr #3607). If a deadline
    // was armed but nothing landed, the armed interval ran from the arm to here, so a churner
    // move within it still migrated a live armed context and must be recorded on this path.
    if landed.is_none()
        && let (Some(at_arm), Some(probe)) = (moves_at_arm, spec.migration_probe.as_ref())
        && probe.moves() > at_arm
    {
        probe.mark_observed();
    }

    // The advisory landing policy fills the overflow bookkeeping; the record assembly (the
    // fail-closed refusals, the guest-attested scale/seed cross-checks, the digest, the
    // overflow sub-record) is shared with the exact policy.
    assemble_measured_record(
        vcpu,
        spec,
        SampleAssembly {
            status,
            work_begin,
            work_end,
            params_mode,
            reported_scale,
            reported_seed,
            clockpage_mode,
            reported,
            overflow: OverflowBookkeeping {
                target,
                deliveries,
                advisory_exits,
                landed,
                landed_digest,
                mechanism_exit,
            },
        },
    )
}

/// Run one AA-3 sample to an **exact** landing (`work == target`) and assemble its
/// [`RunRecord`].
///
/// The AA-3 exact-landing contract (`docs/ARM-ALTRA.md` §AA-3). [`run_sample`] arms the
/// overflow AT the target and records where the mechanism exit fired (`landed = target +
/// skid`); box measurement showed arming AT the target has a ~1.2% boundary-miss — the
/// overflow occasionally does not fire and the guest runs to its sentinel. This variant closes
/// the loop instead, in two moves:
///
/// 1. **Arm early.** The overflow is armed `skid_margin` events BELOW the target (period
///    `delta - skid_margin`), so the `Preempt` fires reliably *below* the target
///    (`target - skid_margin + preempt_skid`, with `preempt_skid <= skid_margin`) — no
///    boundary miss.
/// 2. **Single-step up to the target.** After the `Preempt` lands below the target, guest
///    single-step is armed and the guest is stepped one instruction at a time until the work
///    counter reads EXACTLY the target. Each step advances `BR_RETIRED` by 0 or 1 (AA-2's
///    validated single-step semantics), so the counter cannot skip the target — it lands
///    exactly. The record carries `landed == target`, `skid == 0`, `deliveries == 1`,
///    `exit_reason == Preempt` — the mechanism attestation the AA-3 checker requires still
///    holds.
///
/// It **fails closed, never fudges**: a `Preempt` at or above the target
/// ([`RunError::ExactLandingKickAtOrAboveTarget`] — the skid exceeded `skid_margin +
/// LANDING_HEADROOM`, so it could not reach the canonical landing PC), a step that
/// moves the counter past the target ([`RunError::ExactLandingOvershotTarget`] — impossible
/// under +0/+1, so a real bug), an arm-early period that underflows the margin
/// ([`RunError::ExactLandingWindowTooSmall`]), a margin whose headroom addition overflows
/// ([`RunError::ExactLandingMarginOverflow`]), or a stock signal-kick
/// ([`RunError::ExactLandingSignalKick`] — AA-3's forbidden fallback) each surface as an error
/// rather than a landing that is not exact.
///
/// Only the PATCHED `Preempt` path lands exactly; the stock `SignalKick` path (AA-1(c), which
/// MEASURES skid by arming at the target) stays with [`run_sample`], unchanged. The caller
/// gates this on the AA-3 configuration (patched mechanism + a measured skid margin).
///
/// # Errors
/// [`RunError`] whenever the sample could not be *measured* or landed exactly — see the type.
pub fn run_sample_exact(
    vcpu: &mut impl StepVcpu,
    counter: &mut impl WorkCounter,
    spec: &SampleSpec,
    skid_margin: u64,
) -> Result<RunRecord, RunError> {
    let mut console = Console::new();
    let mut work_begin: Option<u64> = None;
    let mut work_end: Option<u64> = None;
    let mut params_mode: Option<String> = None;
    let mut reported_scale: Option<String> = None;
    let mut reported_seed: Option<String> = None;
    let mut clockpage_mode: Option<String> = None;
    let mut reported: Option<u64> = None;
    let mut status: Option<u8> = None;

    // Overflow bookkeeping, per record (§Evidence integrity #6).
    let mut target: Option<u64> = None;
    // Where the overflow is armed — a full `skid_margin` BELOW the target — so the `Preempt`
    // fires below the target and single-stepping can walk the counter up to it.
    let mut arm_point: Option<u64> = None;
    let mut deliveries: u64 = 0;
    let mut advisory_exits: u64 = 0;
    let mut landed: Option<u64> = None;
    let mut landed_digest: Option<String> = None;

    'run: while status.is_none() {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                // The console handling mirrors `run_sample`: same PL011 read servicing (shared),
                // same config-write skip.
                if service_pl011_read(vcpu, addr, &data, is_write)? {
                    continue 'run;
                }
                if addr != PL011_DR {
                    continue 'run;
                }
                let Some(&byte) = data.first() else {
                    return Err(RunError::UnexpectedMmio { addr });
                };
                match console.push(byte) {
                    Some(Event::MarkBegin) => {
                        if work_begin.is_some() {
                            return Err(RunError::MalformedWindow("MARK_BEGIN twice"));
                        }
                        let begin = counter.read()?;
                        work_begin = Some(begin);
                        // `target = work_begin + delta` is the exact landing point; the overflow
                        // is armed a full `skid_margin` BELOW it (period `delta - skid_margin`),
                        // so it fires below the target reliably (no boundary miss) and the loop
                        // single-steps up to the exact target.
                        if let Some(delta) = spec.target_delta {
                            let t = begin.saturating_add(delta);
                            target = Some(t);
                            // The arm-early period: a full `skid_margin + LANDING_HEADROOM` below
                            // the target, so the `Preempt` fires STRICTLY below it (skid <= margin
                            // < margin + headroom) with room to single-step up to the canonical
                            // landing PC (see `LANDING_HEADROOM`). An underflow (`delta` no larger
                            // than that combined margin) means there is no room below the target —
                            // refuse rather than arm at/above the target and land non-canonically.
                            let arm_delta = exact_arm_delta(delta, skid_margin)?;
                            arm_point = Some(begin.saturating_add(arm_delta));
                            counter.arm_overflow(arm_delta)?;
                        }
                    }
                    Some(Event::MarkEnd) => {
                        if work_begin.is_none() {
                            return Err(RunError::MalformedWindow("MARK_END before MARK_BEGIN"));
                        }
                        if work_end.is_some() {
                            return Err(RunError::MalformedWindow("MARK_END twice"));
                        }
                        work_end = Some(counter.read()?);
                    }
                    Some(Event::Line(line)) => {
                        absorb_line(
                            &line,
                            &mut params_mode,
                            &mut reported_scale,
                            &mut reported_seed,
                            &mut clockpage_mode,
                            &mut reported,
                        );
                    }
                    Some(Event::Exit(code)) => {
                        status = Some(code);
                        break 'run;
                    }
                    None => {}
                }
            }

            // This loop arms guest single-step ONLY during the exact-landing walk (below), and
            // disarms it before resuming. A Debug exit reaching the main loop is therefore an
            // unrequested single-step landing — refused exactly as `run_sample` refuses one.
            VcpuExit::Debug => return Err(RunError::UnexpectedDebugExit),

            // The stock signal-kick is AA-3's forbidden fallback: the exact landing rides the
            // patched in-kernel `Preempt` force-exit alone (§Evidence integrity #4).
            VcpuExit::SignalKick => return Err(RunError::ExactLandingSignalKick),

            VcpuExit::Preempt => {
                let Some(target) = target else {
                    // Nothing was armed, so nothing should have kicked.
                    return Err(RunError::UnexpectedMechanismExit(ExitReason::Preempt));
                };
                let Some(arm_point) = arm_point else {
                    return Err(RunError::Seam {
                        context: "exact-landing target has no arm point",
                        message: "target was installed without its arm-early point".into(),
                    });
                };

                if landed.is_some() {
                    // A second `Preempt` after the exact landing. The perf overflow was parked
                    // and the patch's one-shot was not re-armed, so none is expected; count it
                    // as a duplicate delivery (multiplicity flags it) rather than re-landing.
                    deliveries += 1;
                    continue 'run;
                }

                match service_exact_preempt(
                    vcpu,
                    counter,
                    target,
                    arm_point,
                    skid_margin,
                    MAX_LANDING_RUN_EXITS,
                    |vcpu, addr, data, is_write| {
                        service_pl011_read(vcpu, addr, data, is_write).map(|_| ())
                    },
                )? {
                    ExactPreemptOutcome::Advisory { work } => {
                        advisory_exits += 1;
                        if advisory_exits > MAX_ADVISORY_EXITS {
                            return Err(RunError::AdvisoryExitStorm {
                                exits: advisory_exits,
                                work,
                                target,
                            });
                        }
                    }
                    ExactPreemptOutcome::Landed { .. } => {
                        // Digest the landed state at the exact Moment, before free-running guest
                        // execution resumes. The helper already disarmed guest debug.
                        deliveries += 1;
                        landed = Some(target);
                        landed_digest = Some(vcpu.state_digest()?);
                    }
                }
            }

            VcpuExit::MalformedMmio { addr, width } => {
                return Err(RunError::MalformedMmio { addr, width });
            }
            VcpuExit::Other(reason) => return Err(RunError::UnexpectedExit(reason)),
        }
    }

    // The exact landing policy fills the overflow bookkeeping — `landed == target` (so
    // `skid == 0`) by construction of the single-step loop above, and the mechanism is the
    // patched `Preempt` whenever a landing was made. The record assembly is shared with the
    // advisory policy (`run_sample`), so the two cannot drift apart.
    assemble_measured_record(
        vcpu,
        spec,
        SampleAssembly {
            status,
            work_begin,
            work_end,
            params_mode,
            reported_scale,
            reported_seed,
            clockpage_mode,
            reported,
            overflow: OverflowBookkeeping {
                target,
                deliveries,
                advisory_exits,
                landed,
                landed_digest,
                // A landing rode the patched `Preempt`; a lost PMI ran to the sentinel and the
                // shared assembler records `Mmio`.
                mechanism_exit: landed.map(|_| ExitReason::Preempt),
            },
        },
    )
}

/// Absorb one guest protocol line into the record's attested fields.
///
/// The guest's own words, parsed — never the harness's assumption. `PARAMS mode=`
/// is what makes a smoke-scale run unable to masquerade as a 1e8 one, and the
/// `retries=` terms are the in-band reported counts the oracle needs (the `STXR`
/// and seqlock retry loops, whose trip count is data, not structure).
fn absorb_line(
    line: &str,
    params_mode: &mut Option<String>,
    reported_scale: &mut Option<String>,
    reported_seed: &mut Option<String>,
    clockpage_mode: &mut Option<String>,
    reported: &mut Option<u64>,
) {
    if let Some(rest) = line.strip_prefix("PARAMS ") {
        // The guest prints `PARAMS mode=<m> scale=<s> seed=<hex>` — the params page it
        // actually saw. All three are attested: `mode` guards published-vs-self-seeded,
        // and `scale`/`seed` are cross-checked against the sample spec so a stale or
        // mis-written page cannot mislabel the record (checked at record assembly).
        if let Some(mode) = field(rest, "mode") {
            *params_mode = Some(mode.to_string());
        }
        if let Some(scale) = field(rest, "scale") {
            *reported_scale = Some(scale.to_string());
        }
        if let Some(seed) = field(rest, "seed") {
            *reported_seed = Some(seed.to_string());
        }
    } else if let Some(rest) = line.strip_prefix("CLOCKPAGE ") {
        if let Some(mode) = field(rest, "mode") {
            *clockpage_mode = Some(mode.to_string());
        }
        if let Some(n) = field(rest, "retries").and_then(|v| v.parse::<u64>().ok()) {
            *reported = Some(n);
        }
    } else if let Some(rest) = line.strip_prefix("LLSC ")
        && let Some(n) = field(rest, "retries").and_then(|v| v.parse::<u64>().ok())
    {
        *reported = Some(n);
    }
}

/// The value of `key=` in a space-separated `k=v` line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

/// Parse a `seed=` token, which the guest prints as `{:#x}` (a `0x`-prefixed hex u64).
/// Tolerates a missing prefix; `None` on anything that is not a hex integer.
fn parse_hex_u64(token: &str) -> Option<u64> {
    let digits = token
        .strip_prefix("0x")
        .or_else(|| token.strip_prefix("0X"))
        .unwrap_or(token);
    u64::from_str_radix(digits, 16).ok()
}

// ---------------------------------------------------------------------------------------------
// AA-2: the single-step run path.
//
// `run_sample` above measures a COUNTING window and refuses an unrequested `KVM_EXIT_DEBUG`.
// AA-2 instead arms `KVM_GUESTDBG_SINGLESTEP` and steps the guest one instruction at a time,
// emitting one `RunRecord` (carrying a `StepRecord`) per step. Like the counting loop it is
// pure logic over the [`Vcpu`]/[`WorkCounter`] seams — extended by [`StepVcpu`] — so it is
// driven natively against a scripted vCPU. The measured SEMANTICS (one instruction per step,
// the per-class `BR_RETIRED` weight) are the box's to confirm; this loop records exactly what
// it measured, and the floor checker (`check_debug_evidence`) grades it.
// ---------------------------------------------------------------------------------------------

/// The `ERET` encoding (exception return). Fixed, no operands.
const ERET_OPCODE: u32 = 0xD69F_03E0;
/// The `WFI` hint encoding.
const WFI_OPCODE: u32 = 0xD503_207F;
/// The AArch64 exception vector table is 16 slots of `0x80` bytes = `0x800`, in four `0x200`
/// groups (EL-target/SP variants). Within each group: `0x000` synchronous, `0x080` IRQ,
/// `0x100` FIQ, `0x180` SError.
const VECTOR_TABLE_SIZE: u64 = 0x800;

/// Whether `word` is an `SVC #imm` (any immediate). The payloads issue `svc #0`.
fn is_svc(word: u32) -> bool {
    word & 0xFFE0_001F == 0xD400_0001
}

/// Classify one single step's transition, from the stepped opcode at `pc_before` and where
/// `pc_after` landed (with `vbar` = `VBAR_EL1` for vector-page detection).
///
/// This is a **hypothesis** the box measurement confirms, not a verdict imposed on it. The
/// class is read from the instruction and the observed control flow; it is never forced onto
/// the measured `BR_RETIRED` delta or `pc_after`. Where the opcode and the observed `pc_after`
/// disagree with the class's expected shape — a "sequential" step that skipped an instruction,
/// a branch that did not go where it points — that disagreement **is** the AA-2 finding: the
/// record carries the class the opcode implies together with the PC and counter actually
/// measured, and the floor checker's per-class rule surfaces the mismatch. Reuses
/// [`crate::scan`]'s decoders; no new instruction decode lives here.
#[must_use]
pub fn classify_transition(word: u32, pc_before: u64, pc_after: u64, vbar: u64) -> StepTransition {
    use crate::scan::{branch_target, decode_branch, is_exclusive};
    use oracle_model::BranchKind;

    // An LL/SC exclusive (`LDXR`/`STXR` family) is a load/store, not a branch — the AA-4
    // hazard AA-2 must step. `BR_RETIRED` must not move; the retry `CBNZ` steps as its own
    // `TakenBranch`.
    if is_exclusive(word) {
        return StepTransition::LlscExclusive;
    }
    // A synchronous exception raised by the instruction itself: `SVC`.
    if is_svc(word) {
        return StepTransition::ExceptionEntry;
    }
    // `ERET` is exception return. `decode_branch` also classes it (`BranchKind::Eret`), so it
    // is matched here, ahead of the branch arm, to keep it out of `TakenBranch`.
    if word == ERET_OPCODE {
        return StepTransition::ExceptionReturn;
    }
    // `WFI`: waited, resumed by an interrupt.
    if word == WFI_OPCODE {
        return StepTransition::Wfi;
    }
    // A branch INSTRUCTION: taken iff the PC went where the branch points.
    if let Some(kind) = decode_branch(word) {
        // Defensive: `ERET` is handled above, but never let it read as a taken branch.
        if kind == BranchKind::Eret {
            return StepTransition::ExceptionReturn;
        }
        let taken = match branch_target(word, pc_before) {
            // An immediate branch (`B`/`BL`/`B.cond`/`CBZ`/`TBZ`/…): taken iff the PC landed
            // on the resolved target; not taken (a conditional that fell through to `pc + 4`) is a
            // NotTakenBranch — the branch INSTRUCTION still retired (AA1-F1), so it is not a
            // `Sequential` non-branch step.
            Some(target) => pc_after == target,
            // A register/indirect branch (`BR`/`BLR`/`RET`): its target is a runtime register,
            // so any transfer off the `pc + 4` fall-through is "taken".
            None => pc_after != pc_before.wrapping_add(4),
        };
        return if taken {
            StepTransition::TakenBranch
        } else {
            // A branch instruction that fell through to `pc + 4`: the branch retired but did not
            // transfer. On N1 `BR_RETIRED` counts it (delta 1); the floor checker grades it as a
            // NotTakenBranch (delta 1, lands at `pc + 4`), distinct from a non-branch `Sequential`.
            StepTransition::NotTakenBranch
        };
    }
    // A non-branch instruction whose step nonetheless landed in the EL1 vector page took an
    // exception the instruction does not name: a data/instruction abort (a faulting load/store
    // — how the abort payload enters) or an asynchronous injected interrupt. Which one is told
    // by the vector slot: IRQ/FIQ (async) → injection, synchronous/SError → exception entry.
    if pc_after >= vbar && pc_after < vbar.wrapping_add(VECTOR_TABLE_SIZE) {
        let slot = (pc_after - vbar) % 0x200;
        if slot == 0x080 || slot == 0x100 {
            return StepTransition::Injection;
        }
        return StepTransition::ExceptionEntry;
    }
    // Everything else fell through to the next instruction.
    StepTransition::Sequential
}

/// The vCPU as AA-2's single-step run path needs it: the counting seam ([`Vcpu`]) plus the
/// four primitives stepping adds.
///
/// The real implementation is `KVM_SET_GUEST_DEBUG` + one-reg reads on a vCPU fd
/// ([`crate::sys::Machine`]); the test implementation is a scripted vCPU, so [`step_run`] is
/// driven natively exactly as [`run_sample`] is against [`Vcpu`].
pub trait StepVcpu: Vcpu {
    /// Arm `KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP` once, so every subsequent
    /// [`Vcpu::run`] returns [`VcpuExit::Debug`] after a single guest instruction.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the debug ioctl failed.
    fn arm_single_step(&mut self) -> Result<(), RunError>;

    /// Disarm guest single-step (`KVM_SET_GUEST_DEBUG` with an all-zero control word), so the
    /// vCPU resumes ordinary `KVM_RUN` execution. AA-3's exact-landing loop
    /// ([`run_sample_exact`]) arms single-step to walk `BR_RETIRED` up to the target, then MUST
    /// disarm before resuming the guest to `MARK_END` and the sentinel — otherwise every later
    /// `KVM_RUN` would trap after one instruction and the counting loop would refuse the
    /// unrequested `KVM_EXIT_DEBUG`.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the debug ioctl failed.
    fn disarm_single_step(&mut self) -> Result<(), RunError>;

    /// The current `PC` (a one-reg read of the core `pc`).
    ///
    /// # Errors
    /// [`RunError::Seam`] if the register could not be read.
    fn pc(&mut self) -> Result<u64, RunError>;

    /// The 32-bit instruction word at guest-physical `addr` (4 bytes of guest RAM), or `None`
    /// when `addr` is outside the mapped slot — a step whose `pc_before` fell out of guest RAM
    /// is a finding, not a decodable instruction.
    ///
    /// # Errors
    /// [`RunError::Seam`] if guest RAM could not be read.
    fn opcode_at(&mut self, addr: u64) -> Result<Option<u32>, RunError>;

    /// `VBAR_EL1` — the exception vector base, for classifying whether a step landed in the
    /// vector page (an exception/injection boundary) versus fell through.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the register could not be read.
    fn vbar(&mut self) -> Result<u64, RunError>;

    /// A **registers-only** digest — the vCPU register (and vGIC) state
    /// [`Vcpu::state_digest`] reads, hashed WITHOUT the guest-RAM slice. This is the cheap
    /// per-step replay key: `state_digest` faults in and hashes the whole guest-RAM slot
    /// every call, and single-stepping digests once per instruction, so a full-RAM hash per
    /// step is infeasible. [`step_run`] stamps this on every step but the LAST, which pays
    /// the full-RAM cost so memory divergence across the stepped window is still caught
    /// end-to-end (the amendment). The digest is domain-separated from the full one
    /// ([`crate::sys::digest_regs_only`]) so the two can never collide.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the register/vGIC state could not be read.
    fn regs_digest(&mut self) -> Result<String, RunError>;
}

/// One measured single step, buffered until the run's window count and final state are known
/// (both are stamped onto every step's [`RunRecord`] at assembly).
struct StepMeasurement {
    pc_before: u64,
    pc_after: u64,
    br_retired_delta: u64,
    transition: StepTransition,
    step_digest: String,
}

/// Run one payload under single-step, emitting one [`RunRecord`] (each carrying its
/// [`StepRecord`], `exit_reason == Debug`) per stepped instruction, until the console sentinel
/// **or** `max_steps` steps, whichever comes first.
///
/// The sibling of [`run_sample`]: same seams, same console/params discipline, same fail-closed
/// refusals — but it arms `KVM_GUESTDBG_SINGLESTEP` and records a step per `KVM_EXIT_DEBUG`
/// instead of arming an overflow.
///
/// # `max_steps`, and why a full stepped run needs it
///
/// `max_steps == 0` is **unbounded**: run to the console sentinel, exactly as before. A
/// nonzero `max_steps` **bounds** the run to that many steps — normal, not an error. Two
/// reasons a real stepped run needs the bound:
///
/// - **The digest cost.** [`Vcpu::state_digest`] faults in and hashes the whole 4 MiB guest-RAM
///   slot; a 1e6 window is millions of steps, so full-hashing per step is infeasible. Every
///   step but the last therefore carries the cheap [`StepVcpu::regs_digest`] (registers only),
///   and only the FINAL recorded step's `step_digest` is the full-payload hash — so memory
///   divergence anywhere in the stepped window is still caught end-to-end by replay-identity
///   (which compares `step_digest`), never resting on registers-only evidence for the window.
/// - **The `llsc-atomics` livelock.** Each step clears the exclusive monitor, so `STXR` never
///   succeeds and the retry `CBNZ` loops forever — the run never reaches `MARK_END` or the
///   sentinel. `max_steps` bounds it to a finite stepped prefix (the AA-4 hazard, characterized
///   rather than hung on).
///
/// A bounded run may stop before `MARK_END` (the livelock never closes its window), so its
/// window fields are taken from whatever closed and kept **self-consistent**
/// (`measured_taken == work_end - work_begin`, e.g. `0/0/0`); they are NOT graded against the
/// oracle (the floor checker exempts a `step` record from count-exactness). `PARAMS`/scale/seed
/// are still required — a record without them is unlabelable — because the realistic bound (the
/// livelock) prints them before `MARK_BEGIN`.
///
/// The returned records carry `sample_id = 0..n` (their index within THIS run); the caller
/// reassigns dense ids across every planned run before assembling the run-set.
///
/// # Errors
///
/// [`RunError`] whenever the run could not be *measured* — no params attestation, a mislabelled
/// scale/seed, an empty digest, a step whose `pc_before` left guest RAM
/// ([`RunError::StepPcUnmapped`]) or whose `BR_RETIRED` went backwards
/// ([`RunError::StepCounterWentBackwards`]). A run that reaches its SENTINEL must also have a
/// complete window and (for a reported-term payload) its retry line, exactly as [`run_sample`];
/// a run cut short at `max_steps` is exempt from those, since it never got there.
pub fn step_run(
    vcpu: &mut impl StepVcpu,
    counter: &mut impl WorkCounter,
    spec: &SampleSpec,
    max_steps: u64,
) -> Result<Vec<RunRecord>, RunError> {
    let mut console = Console::new();
    let mut work_begin: Option<u64> = None;
    let mut work_end: Option<u64> = None;
    let mut params_mode: Option<String> = None;
    let mut reported_scale: Option<String> = None;
    let mut reported_seed: Option<String> = None;
    let mut clockpage_mode: Option<String> = None;
    let mut reported: Option<u64> = None;
    let mut status: Option<u8> = None;
    let mut steps: Vec<StepMeasurement> = Vec::new();
    // Set when the run stopped because it reached `max_steps` before the sentinel — a bounded
    // run, which may legitimately have no closed window and no exit code (the llsc livelock).
    let mut hit_max = false;

    vcpu.arm_single_step()?;

    'run: while status.is_none() {
        // Stop once `max_steps` steps are recorded (0 = unbounded). Checked at the top, before
        // the next exit: console/MMIO exits do not count as steps, so this bounds the STEP
        // count, not the exit count. Hitting the bound is the normal end of a bounded run.
        if max_steps != 0 && steps.len() as u64 >= max_steps {
            hit_max = true;
            break 'run;
        }
        // The step anchors, read BEFORE the guest runs: the PC of the instruction about to
        // execute and the `BR_RETIRED` before it. Only USED on a `Debug` exit — but they must
        // be captured before `run`, since after it the instruction has already retired.
        let pc_before = vcpu.pc()?;
        let work_before = counter.read()?;
        match vcpu.run()? {
            VcpuExit::Debug => {
                let pc_after = vcpu.pc()?;
                let work_after = counter.read()?;
                let br_retired_delta = work_after.checked_sub(work_before).ok_or(
                    RunError::StepCounterWentBackwards {
                        before: work_before,
                        after: work_after,
                    },
                )?;
                let word = vcpu
                    .opcode_at(pc_before)?
                    .ok_or(RunError::StepPcUnmapped { pc: pc_before })?;
                let vbar = vcpu.vbar()?;
                let transition = classify_transition(word, pc_before, pc_after, vbar);
                // The step-moment digest, sampled at the single step before the guest resumes.
                // Registers-only ([`StepVcpu::regs_digest`]) — the CHEAP key: full-hashing the
                // 4 MiB guest-RAM slot every step is infeasible. The run's FINAL step is later
                // upgraded to the full-payload hash (below), so memory divergence across the
                // stepped window is caught end-to-end and registers-only is never the sole
                // evidence for the window.
                let step_digest = vcpu.regs_digest()?;
                steps.push(StepMeasurement {
                    pc_before,
                    pc_after,
                    br_retired_delta,
                    transition,
                    step_digest,
                });
            }
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                // The console handling mirrors `run_sample` exactly: the PL011 is the one
                // userspace MMIO device, and a guest boots by configuring it and polling its
                // flag register before every byte.
                if !is_pl011(addr) {
                    return Err(RunError::UnexpectedMmio { addr });
                }
                if !is_write {
                    let width = data.len().clamp(1, 8);
                    vcpu.complete_mmio_read(&PL011_FR_READY.to_le_bytes()[..width.min(4)])?;
                    continue 'run;
                }
                if addr != PL011_DR {
                    // A config-register write (CR/IBRD/FBRD/LCR_H): accepted and ignored.
                    continue 'run;
                }
                let Some(&byte) = data.first() else {
                    return Err(RunError::UnexpectedMmio { addr });
                };
                match console.push(byte) {
                    Some(Event::MarkBegin) => {
                        if work_begin.is_some() {
                            return Err(RunError::MalformedWindow("MARK_BEGIN twice"));
                        }
                        // The window count is measured under single-step too — `BR_RETIRED`
                        // counts retired branches regardless of the debug trap — so it should
                        // equal the oracle, and the checker grades it exactly as a counting run.
                        work_begin = Some(counter.read()?);
                    }
                    Some(Event::MarkEnd) => {
                        if work_begin.is_none() {
                            return Err(RunError::MalformedWindow("MARK_END before MARK_BEGIN"));
                        }
                        if work_end.is_some() {
                            return Err(RunError::MalformedWindow("MARK_END twice"));
                        }
                        work_end = Some(counter.read()?);
                    }
                    Some(Event::Line(line)) => {
                        absorb_line(
                            &line,
                            &mut params_mode,
                            &mut reported_scale,
                            &mut reported_seed,
                            &mut clockpage_mode,
                            &mut reported,
                        );
                    }
                    Some(Event::Exit(code)) => {
                        status = Some(code);
                        break 'run;
                    }
                    None => {}
                }
            }
            // A stepped run arms no overflow, so a mechanism exit is unexplained — never
            // absorbed, exactly as the counting loop refuses an unrequested debug exit.
            exit @ (VcpuExit::Preempt | VcpuExit::SignalKick) => {
                let reason = if exit == VcpuExit::Preempt {
                    ExitReason::Preempt
                } else {
                    ExitReason::SignalKick
                };
                return Err(RunError::UnexpectedMechanismExit(reason));
            }
            VcpuExit::MalformedMmio { addr, width } => {
                return Err(RunError::MalformedMmio { addr, width });
            }
            VcpuExit::Other(reason) => return Err(RunError::UnexpectedExit(reason)),
        }
    }

    // PARAMS / scale / seed are required WHETHER OR NOT the run completed: a record without
    // them is unlabelable evidence (a smoke run masquerading as 1e8, a wrong seed). The
    // realistic bound — the llsc livelock — prints all three before `MARK_BEGIN`, so this
    // holds for a bounded run too.
    let params_mode = params_mode.ok_or(RunError::NoParamsMode)?;
    match reported_scale.as_deref() {
        Some(s) if s == spec.scale.name() => {}
        found => {
            return Err(RunError::ReportedScaleMismatch {
                expected: spec.scale.name(),
                found: found.map(str::to_string),
            });
        }
    }
    match reported_seed.as_deref().map(parse_hex_u64) {
        Some(Some(seed)) if seed == spec.seed => {}
        _ => {
            return Err(RunError::ReportedSeedMismatch {
                expected: spec.seed,
                found: reported_seed,
            });
        }
    }

    // The window. A run that reached its SENTINEL must have a complete window and (for a
    // reported-term payload) its retry line, exactly as `run_sample` — a full run with no
    // window or no attested term is a failure to measure. A run cut short at `max_steps` may
    // legitimately have neither (the llsc livelock never closes its window, let alone prints
    // `retries=`), so its window is taken from whatever closed, kept self-consistent and NOT
    // graded against the oracle (a `step` record is exempt from count-exactness). `work_end`
    // absent ⇒ `end == begin` ⇒ `measured_taken == 0` (the `0/0/0` shape the checker's
    // well-formed identity accepts); the counter is monotonic, so `end >= begin` still holds
    // and a true backwards counter is still refused.
    let (begin, end) = if hit_max {
        let begin = work_begin.unwrap_or(0);
        let end = work_end.unwrap_or(begin);
        (begin, end)
    } else {
        (
            work_begin.ok_or(RunError::NoWindowOpen)?,
            work_end.ok_or(RunError::NoWindowClose)?,
        )
    };
    if end < begin {
        return Err(RunError::CounterWentBackwards { begin, end });
    }

    // A completed run that reached its sentinel must attest a reported-term payload's retry
    // count (`run_sample`'s refusal). A bounded run that never got there reports 0 — harmless,
    // since a step record's window count is not oracle-graded, so no fabricated term can slip
    // through on it.
    let reported_taken = if !hit_max && spec.payload.has_reported_term() {
        reported.ok_or(RunError::MissingReportedTerm(spec.payload))?
    } else {
        reported.unwrap_or(0)
    };

    // A bounded run may never reach `PAYLOAD EXIT`, so it has no exit code — record 0 (cut
    // short deliberately, not a self-check failure). A completed run carries the real status.
    let status = if hit_max {
        status.unwrap_or(0)
    } else {
        status.ok_or(RunError::NoExitSentinel)?
    };

    // The full-payload digest, computed ONCE here — registers + ALL guest RAM + vGIC. It is
    // both the record-level `state_digest` (every record carries the complete-state digest,
    // schema-required non-empty) AND — the amendment — the FINAL recorded step's `step_digest`:
    // every intermediate step is registers-only (cheap), and only this last step pays the
    // full-RAM cost, so memory divergence anywhere in the stepped window is caught end-to-end
    // by replay-identity (which compares `step_digest`). For a `max_steps`-bounded run the
    // guest is paused right after the final step, so this IS that step's Moment exactly; for a
    // sentinel-terminated run it is the run's end state (the guest stepped on to the sentinel),
    // the same Moment as `state_digest` — either way it bounds the stepped window's RAM.
    let state_digest = vcpu.state_digest()?;
    if state_digest.is_empty() {
        return Err(RunError::EmptyStateDigest);
    }
    if let Some(last) = steps.last_mut() {
        last.step_digest = state_digest.clone();
    }

    // One record per step, each stamped with the run's shared window count and final state.
    let records = steps
        .into_iter()
        .enumerate()
        .map(|(i, s)| RunRecord {
            sample_id: i as u64,
            payload: spec.payload,
            scale: spec.scale,
            seed: spec.seed,
            trips: spec.trips,
            condition: spec.condition.clone(),
            work_begin: begin,
            work_end: end,
            measured_taken: end - begin,
            reported_taken,
            // A single step lands on `KVM_EXIT_DEBUG`; the record says so, and the floor
            // checker binds the (byte-flippable) label to the measured step beside it.
            exit_reason: ExitReason::Debug,
            // A stepped record is never an armed landing — they are mutually exclusive.
            overflow: None,
            step: Some(StepRecord {
                // Stable identity of the PLAN entry that emitted every step in this run. The
                // caller later renumbers record-level sample ids, but this remains unchanged so
                // the checker can require distinct coverage of every planned run.
                planned_sample_id: spec.sample_id,
                // The step's position WITHIN THIS run (0-based) — the replay-identity key. It is
                // NOT the record's `sample_id` (which the caller renumbers densely across every
                // planned run); it is stamped here and stays put, so step N of one rep is compared
                // to step N of another.
                step_index: i as u64,
                pc_before: s.pc_before,
                pc_after: s.pc_after,
                // A single step retires exactly one instruction by construction of
                // single-stepping. The box confirms it against the oracle; the checker
                // rejects any record that claims otherwise.
                insn_retired: 1,
                br_retired_delta: s.br_retired_delta,
                transition: s.transition,
                step_digest: s.step_digest,
            }),
            state_digest: state_digest.clone(),
            params_mode: params_mode.clone(),
            clockpage_mode: clockpage_mode.clone(),
            payload_status: i32::from(status),
        })
        .collect();
    Ok(records)
}

#[cfg(test)]
mod tests {
    //! The loop, driven end to end against a scripted seam.
    //!
    //! This is the part of deliverable 2 that can be *tested* pre-silicon: the
    //! ioctls are stubbed out, but everything that decides what a record says —
    //! mark decode, counter sampling, delivery counting, skid, the fail-closed
    //! refusals — runs here natively.

    use super::*;
    use oracle_model::{MARK_BEGIN, MARK_END};

    /// A scripted vCPU: a queue of exits, handed out one per `run()`, plus the state
    /// digest the seam would report at the sentinel and the last value the loop
    /// handed back for an MMIO read.
    struct ScriptedVcpu {
        exits: std::collections::VecDeque<VcpuExit>,
        digest: String,
        last_read_reply: Option<Vec<u8>>,
    }

    impl ScriptedVcpu {
        /// A guest that "prints" `bytes`, one MMIO exit per byte, with `injected`
        /// exits spliced in after the byte index they are keyed to.
        fn printing(bytes: &[u8], injected: &[(usize, VcpuExit)]) -> ScriptedVcpu {
            let mut exits = std::collections::VecDeque::new();
            for (i, &b) in bytes.iter().enumerate() {
                exits.push_back(VcpuExit::Mmio {
                    addr: PL011_DR,
                    data: vec![b],
                    is_write: true,
                });
                for (at, exit) in injected {
                    if *at == i {
                        exits.push_back(exit.clone());
                    }
                }
            }
            ScriptedVcpu {
                exits,
                digest: "sha256:1234".into(),
                last_read_reply: None,
            }
        }

        /// A vCPU that hands out exactly the given exits, in order.
        fn from_exits(exits: Vec<VcpuExit>) -> ScriptedVcpu {
            ScriptedVcpu {
                exits: exits.into(),
                digest: "sha256:1234".into(),
                last_read_reply: None,
            }
        }
    }

    impl Vcpu for ScriptedVcpu {
        fn run(&mut self) -> Result<VcpuExit, RunError> {
            self.exits.pop_front().ok_or(RunError::NoExitSentinel)
        }

        fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError> {
            self.last_read_reply = Some(data.to_vec());
            Ok(())
        }

        fn state_digest(&mut self) -> Result<String, RunError> {
            Ok(self.digest.clone())
        }
    }

    /// A scripted counter: hands out a programmed sequence of readings, and records
    /// what the loop asked of it — which is how the re-arm and resume contracts are
    /// tested without a kernel.
    struct ScriptedCounter {
        readings: std::collections::VecDeque<u64>,
        armed: Vec<u64>,
        rearms: u64,
        resumes: u64,
    }

    impl ScriptedCounter {
        fn new(readings: &[u64]) -> ScriptedCounter {
            ScriptedCounter {
                readings: readings.iter().copied().collect(),
                armed: Vec::new(),
                rearms: 0,
                resumes: 0,
            }
        }
    }

    impl WorkCounter for ScriptedCounter {
        fn read(&mut self) -> Result<u64, RunError> {
            self.readings.pop_front().ok_or(RunError::Seam {
                context: "scripted counter",
                message: "ran out of readings".into(),
            })
        }
        fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError> {
            self.armed.push(delta);
            Ok(())
        }
        fn rearm(&mut self) -> Result<(), RunError> {
            self.rearms += 1;
            Ok(())
        }
        fn resume_counting(&mut self) -> Result<(), RunError> {
            self.resumes += 1;
            Ok(())
        }
    }

    /// A well-formed guest transcript: params line, window, protocol line, sentinel.
    fn transcript() -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"PAYLOAD straight-line START\n");
        b.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        b.extend_from_slice(b"WINDOW trips=1000\n");
        b.push(MARK_BEGIN);
        b.push(MARK_END);
        b.extend_from_slice(b"OK counter-exact\n");
        b.extend_from_slice(b"PAYLOAD EXIT 0\n");
        b
    }

    fn spec(target_delta: Option<u64>) -> SampleSpec {
        SampleSpec {
            sample_id: 7,
            payload: Payload::StraightLine,
            scale: Scale::Smoke,
            seed: 0x5eed,
            trips: 1_000,
            condition: "pinned-solo".into(),
            target_delta,
            migration_probe: None,
        }
    }

    #[test]
    fn exact_arm_delta_checks_margin_addition_before_subtraction() {
        assert_eq!(exact_arm_delta(100, 53).expect("landable"), 31);
        assert!(matches!(
            exact_arm_delta(u64::MAX, u64::MAX),
            Err(RunError::ExactLandingMarginOverflow {
                skid_margin: u64::MAX
            })
        ));
        assert!(matches!(
            exact_arm_delta(69, 53),
            Err(RunError::ExactLandingWindowTooSmall {
                delta: 69,
                skid_margin: 53
            })
        ));
    }

    #[test]
    fn a_counting_run_samples_both_marks_and_assembles_the_record() {
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        // Two readings: one at MARK_BEGIN, one at MARK_END.
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(None)).expect("measured");

        assert_eq!(record.sample_id, 7);
        assert_eq!(record.work_begin, 1_000);
        assert_eq!(record.work_end, 2_001);
        assert_eq!(record.measured_taken, 1_001);
        assert_eq!(record.params_mode, "managed");
        assert_eq!(record.payload_status, 0);
        // Nothing was armed, so the run really did end at the console sentinel.
        assert_eq!(record.exit_reason, ExitReason::Mmio);
        assert!(record.overflow.is_none());
        assert!(counter.armed.is_empty());
        // Every record carries the digest the floors compare. An empty one would
        // make the rep floor vacuous, so there is no path to one.
        assert_eq!(record.state_digest, "sha256:1234");
    }

    #[test]
    fn an_armed_run_arms_at_the_mark_and_records_an_exactly_once_delivery() {
        // The Preempt exit lands right after MARK_BEGIN (byte index of the mark in
        // the transcript), which is what an in-kernel force-exit looks like.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::Preempt)]);
        // begin=1000, landing read=1500, end=2001.
        let mut counter = ScriptedCounter::new(&[1_000, 1_500, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");

        // Armed at the mark, for exactly the requested delta.
        assert_eq!(counter.armed, vec![500]);
        assert_eq!(record.exit_reason, ExitReason::Preempt);
        let o = record.overflow.expect("armed");
        assert!(o.armed);
        assert_eq!(o.deliveries, 1, "exactly-once, shown per record");
        assert_eq!(o.advisory_exits, 0, "the deadline was the first exit");
        assert_eq!(
            o.target, 1_500,
            "target is measured from the window's opening"
        );
        assert_eq!(o.landed, 1_500);
        assert_eq!(o.skid, 0);
        // The digest is taken AT the landing, before the guest resumes — that is the
        // state AA-3's replay identity is about.
        assert_eq!(o.landed_digest, "sha256:1234");
        // The one-shot disables the counter on overflow; the loop resumed it so the
        // window's true end is measured, not the landing.
        assert_eq!(
            counter.resumes, 1,
            "the counter was resumed after the landing"
        );
        assert_eq!(counter.rearms, 0, "no advisory exit, so no re-arm");
    }

    #[test]
    fn an_advisory_exit_before_the_target_is_not_a_delivery_and_rearms() {
        // The patch's arm64 arch-difference: the armed vCPU exits on ANY host IRQ, so
        // an early timer tick produces a Preempt exit BEFORE the counter reached the
        // target. Counting it as a delivery would record a landing that is not the
        // target and let the real overflow pass unseen. The counter, not the exit,
        // decides.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(
            &bytes,
            &[
                (mark_at, VcpuExit::Preempt), // advisory: counter still short of target
                (mark_at, VcpuExit::Preempt), // the real deadline
            ],
        );
        // begin=1000, target=1500; first exit reads 1200 (< target => advisory), the
        // real overflow reads 1500, end reads 2001.
        let mut counter = ScriptedCounter::new(&[1_000, 1_200, 1_500, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        let o = record.overflow.expect("armed");
        assert_eq!(o.deliveries, 1, "exactly one real delivery");
        assert_eq!(
            o.advisory_exits, 1,
            "the early tick is recorded, not hidden"
        );
        assert_eq!(o.landed, 1_500, "landed at the target, not the early tick");
        assert_eq!(counter.rearms, 1, "the cleared one-shot was re-armed");
        assert_eq!(counter.resumes, 1);
    }

    #[test]
    fn an_early_source_verified_signal_kick_is_a_negative_skid_landing_not_advisory() {
        // The STOCK path: the run loop only surfaces a SignalKick sourced by the armed perf
        // fd, so a kick below the target is the overflow landing EARLY — the negative-skid
        // case AA-1 exists to measure, not an unrelated IRQ. Treating it as advisory would
        // re-arm (a no-op for stock) and hang the sample. It must record a delivery.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::SignalKick)]);
        // begin=1000, target=1500; the kick reads 1400 (< target ⇒ an EARLY landing), end 2001.
        let mut counter = ScriptedCounter::new(&[1_000, 1_400, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        let o = record.overflow.expect("armed");
        assert_eq!(
            o.deliveries, 1,
            "the early kick is a delivery, not advisory"
        );
        assert_eq!(o.advisory_exits, 0);
        assert_eq!(o.landed, 1_400, "landed early, below the target");
        assert_eq!(
            o.skid, -100,
            "negative skid = landed (1400) - target (1500)"
        );
        assert_eq!(record.exit_reason, ExitReason::SignalKick);
        assert_eq!(
            counter.rearms, 0,
            "no re-arm on the stock landing (rearm is a no-op)"
        );
    }

    // Ignored under Miri: it drives MAX_ADVISORY_EXITS+1 (100_001) scripted exits
    // through the loop, which the interpreter runs ~100x slower — minutes for a bound
    // check whose logic (a `>` comparison) has no unsafe and nothing for Miri to
    // verify. Convention: keep the interpreted suite quick.
    #[cfg_attr(
        miri,
        ignore = "100k scripted iterations; a plain bound check, no unsafe"
    )]
    #[test]
    fn an_advisory_storm_with_no_progress_is_refused_not_spun_on() {
        // If the guest never advances, an unbounded stream of advisory exits would
        // spin forever. A measurement loop that hangs is worse than one that fails.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let injected: Vec<(usize, VcpuExit)> = (0..MAX_ADVISORY_EXITS + 1)
            .map(|_| (mark_at, VcpuExit::Preempt))
            .collect();
        let mut vcpu = ScriptedVcpu::printing(&bytes, &injected);
        // begin, then an endless supply of below-target reads.
        let mut readings = vec![1_000u64];
        readings.extend(std::iter::repeat_n(
            1_100u64,
            (MAX_ADVISORY_EXITS + 2) as usize,
        ));
        let mut counter = ScriptedCounter::new(&readings);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(Some(500))),
            Err(RunError::AdvisoryExitStorm { .. })
        ));
    }

    #[test]
    fn a_wedged_vcpu_surfaces_as_an_error_not_a_hang_or_a_record() {
        // The watchdog turns a KVM_RUN that never returns into RunError::Watchdog. The
        // run loop must propagate it (so the caller records a failed attempt), never
        // absorb it into a record — a wedge has no measured window.
        struct Wedged;
        impl Vcpu for Wedged {
            fn run(&mut self) -> Result<VcpuExit, RunError> {
                Err(RunError::Watchdog { secs: 300 })
            }
            fn complete_mmio_read(&mut self, _data: &[u8]) -> Result<(), RunError> {
                Ok(())
            }
            fn state_digest(&mut self) -> Result<String, RunError> {
                Ok("sha256:00".into())
            }
        }
        let mut counter = ScriptedCounter::new(&[]);
        assert!(matches!(
            run_sample(&mut Wedged, &mut counter, &spec(Some(500))),
            Err(RunError::Watchdog { secs: 300 })
        ));
    }

    #[test]
    fn a_lost_pmi_is_recorded_as_zero_deliveries_not_smoothed_away() {
        // The overflow is armed but no exit ever comes — rr #3607's failure mode.
        // The record must SAY so; the floor checker is what rejects it.
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        let o = record.overflow.expect("armed");
        assert_eq!(o.deliveries, 0, "a lost PMI is visible, not absorbed");
        assert_eq!(o.landed, 0);
    }

    #[test]
    fn a_lost_pmi_records_the_migration_on_the_no_delivery_path() {
        // A counter that "migrates" the thread (bumps the churn count) on every read, so a move
        // falls strictly inside the armed interval — after the arm, before the run ends.
        struct MigratingCounter {
            readings: std::collections::VecDeque<u64>,
            moves: Arc<AtomicU64>,
        }
        impl WorkCounter for MigratingCounter {
            fn read(&mut self) -> Result<u64, RunError> {
                self.moves.fetch_add(1, Ordering::Relaxed);
                self.readings.pop_front().ok_or(RunError::Seam {
                    context: "migrating counter",
                    message: "ran out of readings".into(),
                })
            }
            fn arm_overflow(&mut self, _: u64) -> Result<(), RunError> {
                Ok(())
            }
            fn rearm(&mut self) -> Result<(), RunError> {
                Ok(())
            }
            fn resume_counting(&mut self) -> Result<(), RunError> {
                Ok(())
            }
        }

        // An armed sample whose transcript ends at the sentinel with NO mechanism exit → a LOST
        // PMI (deliveries == 0, no landing). A churn move inside its armed interval must still
        // be recorded — the migration probe exists to observe exactly this rr #3607 case.
        let moves = Arc::new(AtomicU64::new(0));
        let probe = ArmedMigrationProbe::new(Arc::clone(&moves));
        let mut s = spec(Some(500));
        s.migration_probe = Some(probe.clone());
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        let mut counter = MigratingCounter {
            readings: [1_000u64, 2_001].into_iter().collect(),
            moves,
        };
        let record = run_sample(&mut vcpu, &mut counter, &s).expect("a lost PMI is a valid record");
        assert_eq!(
            record.overflow.as_ref().unwrap().deliveries,
            0,
            "no delivery"
        );
        assert!(
            probe.observed(),
            "a move inside a LOST PMI's armed interval must be recorded on the no-delivery path"
        );

        // Control: no migration (a plain counter that never moves) → not observed.
        let moves = Arc::new(AtomicU64::new(0));
        let probe = ArmedMigrationProbe::new(Arc::clone(&moves));
        let mut s = spec(Some(500));
        s.migration_probe = Some(probe.clone());
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        run_sample(&mut vcpu, &mut counter, &s).expect("measured");
        assert!(!probe.observed(), "no move → nothing to record");
    }

    #[test]
    fn a_duplicate_delivery_is_counted_not_deduplicated() {
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(
            &bytes,
            &[
                (mark_at, VcpuExit::Preempt),
                (mark_at, VcpuExit::Preempt), // the same PMI delivered twice
            ],
        );
        // begin(1000), preempt1(1500)=delivery, preempt2(1500)=delivery, end(2001).
        // The loop reads the counter on EVERY mechanism exit now, to classify it.
        let mut counter = ScriptedCounter::new(&[1_000, 1_500, 1_500, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        assert_eq!(record.overflow.expect("armed").deliveries, 2);
    }

    #[test]
    fn an_overshoot_is_recorded_with_a_positive_skid() {
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::Preempt)]);
        // Landed at 1_600 against a target of 1_500: late, which the late-only-stop
        // contract forbids — and which the record must therefore state plainly.
        let mut counter = ScriptedCounter::new(&[1_000, 1_600, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        let o = record.overflow.expect("armed");
        assert_eq!(o.landed, 1_600);
        assert_eq!(o.skid, 100);
    }

    #[test]
    fn the_signal_kick_is_recorded_as_itself_never_as_the_patched_exit() {
        // §Evidence integrity #4: the stock fallback must be structurally unable to
        // masquerade as the patched mechanism. The loop records what the seam said.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::SignalKick)]);
        let mut counter = ScriptedCounter::new(&[1_000, 1_500, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(Some(500))).expect("measured");
        assert_eq!(record.exit_reason, ExitReason::SignalKick);
    }

    #[test]
    fn the_reported_retry_term_is_taken_from_the_guest_not_assumed() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"LLSC retries=17 final=1000\n");
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(None)).expect("measured");
        assert_eq!(record.reported_taken, 17);
    }

    #[test]
    fn a_reported_term_payload_that_never_printed_retries_is_refused() {
        // llsc-atomics' count includes an in-band `STXR` retry term. If the guest omits
        // the `retries=` line while the true count is 0, a defaulted 0 would let the
        // record match the oracle and pass — claiming a report it never made. The sample
        // is refused instead.
        let mut llsc = spec(None);
        llsc.payload = Payload::LlscAtomics;

        // No retries= line at all → refused.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &llsc),
            Err(RunError::MissingReportedTerm(Payload::LlscAtomics))
        ));

        // With the line — even `retries=0` — it is accepted: 0 REPORTED is not 0 DEFAULTED.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"LLSC retries=0 final=1000\n");
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        let record = run_sample(&mut vcpu, &mut counter, &llsc).expect("measured");
        assert_eq!(record.reported_taken, 0);
    }

    #[test]
    fn a_stale_scale_or_seed_the_guest_reports_is_refused() {
        // The guest prints the (scale, seed) it ACTUALLY read off the params page. A stale
        // or mis-written page — a wrong seed on a seed-ignoring payload whose counts still
        // match the oracle — is caught by cross-checking that report against the sample
        // spec, which is the only thing that catches it (the counts do not).
        let mk = |line: &str| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(line.as_bytes());
            bytes.push(MARK_BEGIN);
            bytes.push(MARK_END);
            bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
            bytes
        };
        // spec() is Scale::Smoke, seed 0x5eed.
        let run = |line: &str| {
            let bytes = mk(line);
            let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
            let mut counter = ScriptedCounter::new(&[10, 20]);
            run_sample(&mut vcpu, &mut counter, &spec(None))
        };

        // A seed the guest ran that is not the sample's seed.
        assert!(matches!(
            run("PARAMS mode=managed scale=smoke seed=0xdead\n"),
            Err(RunError::ReportedSeedMismatch {
                expected: 0x5eed,
                ..
            })
        ));
        // A scale the guest ran that is not the sample's scale (checked before seed).
        assert!(matches!(
            run("PARAMS mode=managed scale=1e6 seed=0x5eed\n"),
            Err(RunError::ReportedScaleMismatch {
                expected: "smoke",
                ..
            })
        ));
        // A PARAMS line that omits the seed entirely is refused, not silently accepted.
        assert!(matches!(
            run("PARAMS mode=managed scale=smoke\n"),
            Err(RunError::ReportedSeedMismatch { found: None, .. })
        ));
        // The matching report is accepted.
        assert!(run("PARAMS mode=managed scale=smoke seed=0x5eed\n").is_ok());
    }

    #[test]
    fn the_clockpage_mode_is_taken_from_the_guest() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.extend_from_slice(b"CLOCKPAGE mode=managed abi=1 flags=0x1\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"CLOCKPAGE retries=3\n");
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(None)).expect("measured");
        assert_eq!(record.clockpage_mode.as_deref(), Some("managed"));
        assert_eq!(record.reported_taken, 3);
    }

    #[test]
    fn a_nonzero_payload_status_survives_into_the_record() {
        // A payload that ran to completion but failed its own self-checks is a
        // failed sample, however good its counts look.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"PAYLOAD EXIT 3\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(None)).expect("measured");
        assert_eq!(record.payload_status, 3);
    }

    // --- the fail-closed refusals: every way to NOT measure is an error ---

    #[test]
    fn a_guest_that_never_opened_its_window_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::NoWindowOpen)
        ));
    }

    #[test]
    fn a_guest_that_never_closed_its_window_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::NoWindowClose)
        ));
    }

    #[test]
    fn a_guest_that_never_attested_its_params_mode_is_refused() {
        // The whole point of the attestation: without it, a smoke-scale run could
        // pass for a 1e8 one. No line, no record.
        let mut bytes = Vec::new();
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::NoParamsMode)
        ));
    }

    #[test]
    fn a_guest_that_never_reached_its_sentinel_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_END);
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::NoExitSentinel)
        ));
    }

    #[test]
    fn an_unexplained_kick_with_nothing_armed_is_refused() {
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::SignalKick)]);
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        // No target armed, yet a kick arrived: never absorbed into a clean record.
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::UnexpectedMechanismExit(ExitReason::SignalKick))
        ));
    }

    #[test]
    fn an_unhandled_exit_reason_is_refused_not_skipped() {
        let bytes = transcript();
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(0, VcpuExit::Other(8))]);
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::UnexpectedExit(8))
        ));
    }

    #[test]
    fn a_backwards_counter_is_refused() {
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        let mut counter = ScriptedCounter::new(&[2_000, 1_000]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::CounterWentBackwards {
                begin: 2_000,
                end: 1_000
            })
        ));
    }

    #[test]
    fn an_empty_state_digest_is_refused() {
        // A digest that cannot diverge would satisfy every replay-identity and
        // rep-floor comparison without measuring anything. The loop refuses to write
        // one rather than let the floors go vacuous downstream.
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        vcpu.digest = String::new();
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::EmptyStateDigest)
        ));
    }

    #[test]
    fn a_single_step_landing_is_refused_because_this_loop_arms_no_debug() {
        // A KVM_EXIT_DEBUG counted as an overflow delivery would be two mechanisms
        // wearing one name. AA-2 owns stepping; this loop refuses what it did not
        // arm.
        let bytes = transcript();
        let mark_at = bytes
            .iter()
            .position(|&b| b == MARK_BEGIN)
            .expect("mark present");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[(mark_at, VcpuExit::Debug)]);
        let mut counter = ScriptedCounter::new(&[1_000, 1_500, 2_001]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(Some(500))),
            Err(RunError::UnexpectedDebugExit)
        ));
    }

    #[test]
    fn an_mmio_access_that_is_not_the_console_is_refused() {
        // The payloads touch exactly one MMIO address. A store anywhere else is a
        // finding — and a read of the console is not a thing the payloads do either.
        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        vcpu.exits.push_front(VcpuExit::Mmio {
            addr: 0xDEAD_0000,
            data: vec![0],
            is_write: true,
        });
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::UnexpectedMmio { addr: 0xDEAD_0000 })
        ));

        let mut vcpu = ScriptedVcpu::printing(&transcript(), &[]);
        vcpu.exits.push_front(VcpuExit::Mmio {
            addr: PL011_DR,
            data: Vec::new(),
            is_write: true,
        });
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        assert!(
            matches!(
                run_sample(&mut vcpu, &mut counter, &spec(None)),
                Err(RunError::UnexpectedMmio { .. })
            ),
            "a zero-length console store is malformed, not a byte to skip"
        );
    }

    #[test]
    fn the_uart_config_writes_and_flag_reads_a_real_guest_makes_are_serviced() {
        // The bug this pins: a real guest boots by writing the PL011 config registers
        // (CR/IBRD/FBRD/LCR_H) and polling the flag register before every byte. With
        // no in-kernel PL011 those are all MMIO exits; the round-2 loop accepted only
        // DR writes and rejected the very first `runtime_init` write, so no payload
        // reached MARK_BEGIN. Here the config writes and an FR read are spliced in
        // ahead of the console stream, exactly as a booting guest emits them.
        let mut exits = vec![
            // CR=0, IBRD, FBRD, LCR_H, CR=UARTEN|TXE|RXE — config writes, no DR.
            VcpuExit::Mmio {
                addr: UART_BASE + 0x30,
                data: vec![0, 0, 0, 0],
                is_write: true,
            },
            VcpuExit::Mmio {
                addr: UART_BASE + 0x24,
                data: vec![1, 0, 0, 0],
                is_write: true,
            },
            VcpuExit::Mmio {
                addr: UART_BASE + 0x2c,
                data: vec![0x60, 0, 0, 0],
                is_write: true,
            },
            // A flag-register READ — the `putb` poll. The loop must answer it.
            VcpuExit::Mmio {
                addr: PL011_FR,
                data: vec![0, 0, 0, 0],
                is_write: false,
            },
        ];
        // Then the ordinary console byte stream.
        for &b in &transcript() {
            exits.push(VcpuExit::Mmio {
                addr: PL011_DR,
                data: vec![b],
                is_write: true,
            });
        }
        let mut vcpu = ScriptedVcpu::from_exits(exits);
        let mut counter = ScriptedCounter::new(&[1_000, 2_001]);
        let record = run_sample(&mut vcpu, &mut counter, &spec(None)).expect("guest booted");
        assert_eq!(
            record.measured_taken, 1_001,
            "the window was still measured"
        );
        // The FR read was answered "ready" (zero), so the guest's poll is single-pass.
        assert_eq!(vcpu.last_read_reply.as_deref(), Some(&[0u8, 0, 0, 0][..]));
    }

    #[test]
    fn a_double_window_open_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x5eed\n");
        bytes.push(MARK_BEGIN);
        bytes.push(MARK_BEGIN);
        bytes.extend_from_slice(b"PAYLOAD EXIT 0\n");
        let mut vcpu = ScriptedVcpu::printing(&bytes, &[]);
        let mut counter = ScriptedCounter::new(&[10, 20]);
        assert!(matches!(
            run_sample(&mut vcpu, &mut counter, &spec(None)),
            Err(RunError::MalformedWindow(_))
        ));
    }

    // ----------------------------------------------------------------------------------------
    // AA-2: the single-step run path, driven against a scripted step-vCPU (the run.rs pattern).
    // ----------------------------------------------------------------------------------------

    use std::cell::Cell;
    use std::rc::Rc;

    /// The scripted base address of the stepped instruction stream, and the guest's `VBAR_EL1`.
    const STEP_PC: u64 = 0x4000_8000;
    const STEP_VBAR: u64 = 0x4020_0000;

    /// One scripted exit for [`step_run`]: a console byte, a flag-register poll, a single step,
    /// a mechanism kick, or a non-console MMIO access.
    #[derive(Clone)]
    enum Scripted {
        Console(u8),
        Poll,
        /// A single guest step: it advances the PC to `pc_after`, retires `opcode`, and moves
        /// `BR_RETIRED` by `delta` (signed, so a backwards counter can be scripted).
        Step {
            pc_after: u64,
            opcode: u32,
            delta: i64,
        },
        /// Like [`Scripted::Step`] but the opcode word is UNREADABLE (a wild `pc_before`).
        StepUnmapped {
            pc_after: u64,
        },
        Preempt,
        BadMmio(u64),
    }

    /// A scripted step-vCPU: hands out exits, tracks the PC and the last stepped opcode, and
    /// shares a `BR_RETIRED` cell with [`ScriptedStepCounter`] (a real step advances the counter
    /// while inside `run`, exactly as the guest does inside `KVM_RUN`).
    struct ScriptedStepVcpu {
        exits: std::collections::VecDeque<Scripted>,
        pc: u64,
        last_opcode: Option<u32>,
        vbar: u64,
        counter: Rc<Cell<u64>>,
        /// The registers-only digest returned per step (the cheap key).
        regs_digest: String,
        /// The full-payload digest returned at the run's end (and stamped on the final step).
        digest: String,
        armed: bool,
        last_read_reply: Option<Vec<u8>>,
    }

    impl Vcpu for ScriptedStepVcpu {
        fn run(&mut self) -> Result<VcpuExit, RunError> {
            match self.exits.pop_front().ok_or(RunError::NoExitSentinel)? {
                Scripted::Console(b) => Ok(VcpuExit::Mmio {
                    addr: PL011_DR,
                    data: vec![b],
                    is_write: true,
                }),
                Scripted::Poll => Ok(VcpuExit::Mmio {
                    addr: PL011_FR,
                    data: vec![0, 0, 0, 0],
                    is_write: false,
                }),
                Scripted::Step {
                    pc_after,
                    opcode,
                    delta,
                } => {
                    self.pc = pc_after;
                    self.last_opcode = Some(opcode);
                    let now = self.counter.get() as i64 + delta;
                    self.counter.set(now as u64);
                    Ok(VcpuExit::Debug)
                }
                Scripted::StepUnmapped { pc_after } => {
                    self.pc = pc_after;
                    self.last_opcode = None;
                    Ok(VcpuExit::Debug)
                }
                Scripted::Preempt => Ok(VcpuExit::Preempt),
                Scripted::BadMmio(addr) => Ok(VcpuExit::Mmio {
                    addr,
                    data: vec![0],
                    is_write: true,
                }),
            }
        }

        fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError> {
            self.last_read_reply = Some(data.to_vec());
            Ok(())
        }

        fn state_digest(&mut self) -> Result<String, RunError> {
            Ok(self.digest.clone())
        }
    }

    impl StepVcpu for ScriptedStepVcpu {
        fn arm_single_step(&mut self) -> Result<(), RunError> {
            self.armed = true;
            Ok(())
        }
        fn disarm_single_step(&mut self) -> Result<(), RunError> {
            self.armed = false;
            Ok(())
        }
        fn pc(&mut self) -> Result<u64, RunError> {
            Ok(self.pc)
        }
        fn opcode_at(&mut self, _addr: u64) -> Result<Option<u32>, RunError> {
            // The scripted vCPU returns the opcode the last step retired, or `None` for the
            // unmapped-PC case — the loop passes `pc_before`, but the mapping is scripted.
            Ok(self.last_opcode)
        }
        fn vbar(&mut self) -> Result<u64, RunError> {
            Ok(self.vbar)
        }
        fn regs_digest(&mut self) -> Result<String, RunError> {
            // A DISTINCT value from `state_digest`, so a test can tell an intermediate
            // (registers-only) step's digest from the full-payload final one.
            Ok(self.regs_digest.clone())
        }
    }

    /// The counter half of the shared cell (see [`ScriptedStepVcpu`]).
    struct ScriptedStepCounter {
        value: Rc<Cell<u64>>,
    }
    impl WorkCounter for ScriptedStepCounter {
        fn read(&mut self) -> Result<u64, RunError> {
            Ok(self.value.get())
        }
        fn arm_overflow(&mut self, _delta: u64) -> Result<(), RunError> {
            Ok(())
        }
        fn rearm(&mut self) -> Result<(), RunError> {
            Ok(())
        }
        fn resume_counting(&mut self) -> Result<(), RunError> {
            Ok(())
        }
    }

    /// A booting console prologue: the guest's `PARAMS` attestation, then `MARK_BEGIN`.
    fn boot_prologue() -> Vec<Scripted> {
        let mut v: Vec<Scripted> = b"PARAMS mode=managed scale=smoke seed=0x5eed\n"
            .iter()
            .map(|&b| Scripted::Console(b))
            .collect();
        v.push(Scripted::Console(MARK_BEGIN));
        v
    }

    /// A closing console epilogue: `MARK_END`, then the exit sentinel.
    fn exit_epilogue() -> Vec<Scripted> {
        let mut v = vec![Scripted::Console(MARK_END)];
        v.extend(b"PAYLOAD EXIT 0\n".iter().map(|&b| Scripted::Console(b)));
        v
    }

    /// Drive [`step_run`] over an exact `script` at `counter_start` under `max_steps` (0 =
    /// unbounded). The scripted seam returns `sha256:regs` per step (the registers-only key) and
    /// `sha256:final` at the run's end (the full-payload digest, which the final step inherits).
    fn drive_script(
        counter_start: u64,
        script: Vec<Scripted>,
        max_steps: u64,
    ) -> Result<Vec<RunRecord>, RunError> {
        let cell = Rc::new(Cell::new(counter_start));
        let mut vcpu = ScriptedStepVcpu {
            exits: script.into(),
            pc: STEP_PC,
            last_opcode: None,
            vbar: STEP_VBAR,
            counter: Rc::clone(&cell),
            regs_digest: "sha256:regs".into(),
            digest: "sha256:final".into(),
            armed: false,
            last_read_reply: None,
        };
        let mut counter = ScriptedStepCounter { value: cell };
        step_run(&mut vcpu, &mut counter, &spec(None), max_steps)
    }

    /// Drive [`step_run`] unbounded over a scripted body spliced between the boot prologue and
    /// the exit epilogue (so the run reaches its sentinel), starting the counter at
    /// `counter_start`.
    fn drive(counter_start: u64, body: Vec<Scripted>) -> Result<Vec<RunRecord>, RunError> {
        let mut script = boot_prologue();
        script.extend(body);
        script.extend(exit_epilogue());
        drive_script(counter_start, script, 0)
    }

    /// One step of the given opcode landing at `pc_after` with a `BR_RETIRED` delta, driven to
    /// its single [`StepRecord`].
    fn one_step(pc_after: u64, opcode: u32, delta: i64) -> StepRecord {
        let records = drive(
            0,
            vec![Scripted::Step {
                pc_after,
                opcode,
                delta,
            }],
        )
        .expect("a single stepped record");
        assert_eq!(records.len(), 1, "one step ⇒ one record");
        let r = &records[0];
        assert_eq!(r.exit_reason, ExitReason::Debug, "a step lands on Debug");
        assert!(r.overflow.is_none(), "a stepped record is never armed");
        let step = r.step.clone().expect("carries a step measurement");
        assert_eq!(step.pc_before, STEP_PC);
        step
    }

    /// The `pc_before` of a record's step measurement.
    fn step_pc_before(r: &RunRecord) -> u64 {
        r.step.as_ref().expect("stepped").pc_before
    }

    #[test]
    fn each_transition_class_is_classified_and_measured() {
        // Sequential: a NOP that fell through to PC+4, no branch retired.
        let s = one_step(STEP_PC + 4, 0xD503_201F, 0);
        assert_eq!(s.transition, StepTransition::Sequential);
        assert_eq!(s.pc_after, STEP_PC + 4);
        assert_eq!(s.br_retired_delta, 0);
        assert_eq!(s.insn_retired, 1);

        // Taken branch (immediate `b .+8`): landed on the resolved target, one branch retired.
        let s = one_step(STEP_PC + 8, 0x1400_0002, 1);
        assert_eq!(s.transition, StepTransition::TakenBranch);
        assert_eq!(s.br_retired_delta, 1);

        // Taken branch (register `ret`): its target is a register, so any move off PC+4 is taken.
        let s = one_step(0x4000_9000, 0xD65F_03C0, 1);
        assert_eq!(s.transition, StepTransition::TakenBranch);

        // A NOT-taken conditional (`b.ne .+8` that fell through to PC+4) is a NotTakenBranch,
        // NOT Sequential: the branch instruction retired (AA1-F1), so BR_RETIRED moved by 1 even
        // though the PC landed at PC+4.
        let s = one_step(STEP_PC + 4, 0x5400_0041, 1);
        assert_eq!(s.transition, StepTransition::NotTakenBranch);
        assert_eq!(s.pc_after, STEP_PC + 4);
        assert_eq!(s.br_retired_delta, 1);

        // SVC — synchronous exception entry, classified from the opcode not the PC.
        let s = one_step(STEP_VBAR + 0x400, 0xD400_0001, 0);
        assert_eq!(s.transition, StepTransition::ExceptionEntry);

        // ERET — exception return (a branch encoding, but ExceptionReturn, never TakenBranch).
        let s = one_step(0x4000_A000, 0xD69F_03E0, 0);
        assert_eq!(s.transition, StepTransition::ExceptionReturn);

        // WFI — waited and resumed.
        let s = one_step(STEP_PC + 4, 0xD503_207F, 0);
        assert_eq!(s.transition, StepTransition::Wfi);

        // LL/SC exclusive (`ldxr`) — a load, not a branch: BR_RETIRED must not move.
        let s = one_step(STEP_PC + 4, 0xC85F_7C41, 0);
        assert_eq!(s.transition, StepTransition::LlscExclusive);
        assert_eq!(s.br_retired_delta, 0);

        // Injection — a non-branch instruction whose step landed in the IRQ vector slot.
        let s = one_step(STEP_VBAR + 0x080, 0xD503_201F, 0);
        assert_eq!(s.transition, StepTransition::Injection);
    }

    #[test]
    fn a_skipped_instruction_is_recorded_faithfully_not_forced_to_pc_plus_4() {
        // A sequential (non-branch) step that advanced by 8 skipped an instruction. The loop
        // records the class the OPCODE implies (Sequential) and the PC it actually MEASURED
        // (PC+8) — it does not force pc_after to PC+4. That faithful record is exactly what
        // `check_debug_evidence` then rejects (a sequential step must land at PC+4).
        let s = one_step(STEP_PC + 8, 0xD503_201F, 0);
        assert_eq!(s.transition, StepTransition::Sequential);
        assert_eq!(
            s.pc_after,
            STEP_PC + 8,
            "the measured skip is recorded, not smoothed to PC+4"
        );
    }

    #[test]
    fn a_taken_branch_that_did_not_move_the_counter_is_recorded_faithfully() {
        // A `b .+8` that took the branch but whose BR_RETIRED did not move: the class is
        // TakenBranch (the opcode branched and landed on target) and the delta is the measured
        // 0. The loop records both truthfully; the checker's "a taken branch must increment
        // BR_RETIRED by exactly 1" is what catches the disagreement.
        let s = one_step(STEP_PC + 8, 0x1400_0002, 0);
        assert_eq!(s.transition, StepTransition::TakenBranch);
        assert_eq!(
            s.br_retired_delta, 0,
            "the measured delta is recorded, not forced to 1"
        );
    }

    #[test]
    fn one_record_per_step_with_dense_ids_and_a_shared_window_and_final_state() {
        // Three sequential steps: three records, ids 0..3, each carrying the run's shared
        // window count and final state digest, its own step-moment digest and measurement.
        let body = vec![
            Scripted::Step {
                pc_after: STEP_PC + 4,
                opcode: 0xD503_201F,
                delta: 1,
            },
            Scripted::Step {
                pc_after: STEP_PC + 8,
                opcode: 0xD503_201F,
                delta: 0,
            },
            Scripted::Step {
                pc_after: STEP_PC + 12,
                opcode: 0xD503_201F,
                delta: 0,
            },
        ];
        let records = drive(0, body).expect("three stepped records");
        assert_eq!(records.len(), 3);
        for (i, r) in records.iter().enumerate() {
            assert_eq!(r.sample_id, i as u64, "dense ids within the run");
            assert_eq!(r.exit_reason, ExitReason::Debug);
            assert_eq!(r.state_digest, "sha256:final", "shared final-state digest");
            // The window count is measured under single-step (one branch retired across the
            // three steps), and stamped onto every record so the oracle check grades it.
            assert_eq!(r.work_begin, 0);
            assert_eq!(r.work_end, 1);
            assert_eq!(r.measured_taken, 1);
        }
        // The PC chains: each step's pc_before is the previous step's pc_after.
        assert_eq!(step_pc_before(&records[0]), STEP_PC);
        assert_eq!(step_pc_before(&records[1]), STEP_PC + 4);
        assert_eq!(step_pc_before(&records[2]), STEP_PC + 8);
    }

    #[test]
    fn a_console_poll_while_stepping_is_answered() {
        // A flag-register read spliced into the stream is answered "ready", exactly as the
        // counting loop does — a stepped guest still polls the UART before it prints.
        let records = drive(
            0,
            vec![
                Scripted::Poll,
                Scripted::Step {
                    pc_after: STEP_PC + 4,
                    opcode: 0xD503_201F,
                    delta: 0,
                },
            ],
        )
        .expect("measured");
        assert_eq!(records.len(), 1);
    }

    #[test]
    fn a_mechanism_kick_while_stepping_is_refused() {
        // A stepped run arms no overflow, so a Preempt/SignalKick is an unexplained kick —
        // refused, exactly as the counting loop refuses an unrequested debug exit.
        assert!(matches!(
            drive(0, vec![Scripted::Preempt]),
            Err(RunError::UnexpectedMechanismExit(ExitReason::Preempt))
        ));
    }

    #[test]
    fn a_non_console_mmio_while_stepping_is_refused() {
        assert!(matches!(
            drive(0, vec![Scripted::BadMmio(0xDEAD_0000)]),
            Err(RunError::UnexpectedMmio { addr: 0xDEAD_0000 })
        ));
    }

    #[test]
    fn a_step_off_the_mapping_is_refused_not_read_as_a_zero_opcode() {
        // A step whose pc_before left guest RAM cannot have its opcode read: a finding, refused
        // rather than decoded from a plausible zero.
        assert!(matches!(
            drive(
                0,
                vec![Scripted::StepUnmapped {
                    pc_after: STEP_PC + 4
                }]
            ),
            Err(RunError::StepPcUnmapped { pc }) if pc == STEP_PC
        ));
    }

    #[test]
    fn a_step_whose_counter_went_backwards_is_refused() {
        // BR_RETIRED is monotonic while the guest runs; a decrease across one step is a
        // seam/hardware anomaly, refused rather than recorded as a huge wrapped delta.
        assert!(matches!(
            drive(
                10,
                vec![Scripted::Step {
                    pc_after: STEP_PC + 4,
                    opcode: 0xD503_201F,
                    delta: -5,
                }]
            ),
            Err(RunError::StepCounterWentBackwards {
                before: 10,
                after: 5
            })
        ));
    }

    #[test]
    fn a_stepped_run_still_refuses_a_guest_that_never_opened_its_window() {
        // The same fail-closed refusals as the counting loop: an UNBOUNDED run that reached its
        // sentinel with no MARK_BEGIN has no window and no record. (A bounded run is different —
        // it may stop before the window opens; that is the livelock case below.)
        let mut script: Vec<Scripted> = b"PARAMS mode=managed scale=smoke seed=0x5eed\n"
            .iter()
            .map(|&b| Scripted::Console(b))
            .collect();
        script.extend(b"PAYLOAD EXIT 0\n".iter().map(|&b| Scripted::Console(b)));
        assert!(matches!(
            drive_script(0, script, 0),
            Err(RunError::NoWindowOpen)
        ));
    }

    // --- AA-2 bounded stepping (--max-steps): the digest split, the livelock bound ---

    /// `n` sequential NOP steps (each PC+4, no branch retired).
    fn nop_steps(n: usize) -> Vec<Scripted> {
        (0..n)
            .map(|i| Scripted::Step {
                pc_after: STEP_PC + 4 * (i as u64 + 1),
                opcode: 0xD503_201F,
                delta: 0,
            })
            .collect()
    }

    #[test]
    fn max_steps_stops_at_exactly_n_steps_even_though_the_sentinel_is_reachable() {
        // The body has ten steps and the sentinel is present, but --max-steps 4 stops the run
        // at exactly four recorded steps — a bounded run ending before its sentinel is normal.
        let records = drive(0, nop_steps(10)).expect("unbounded reaches the sentinel");
        assert_eq!(records.len(), 10, "unbounded records every step");

        let mut script = boot_prologue();
        script.extend(nop_steps(10));
        script.extend(exit_epilogue());
        let bounded = drive_script(0, script, 4).expect("a bounded run is not an error");
        assert_eq!(
            bounded.len(),
            4,
            "--max-steps 4 stops at exactly four steps"
        );
        for (i, r) in bounded.iter().enumerate() {
            assert_eq!(r.sample_id, i as u64);
            assert_eq!(r.exit_reason, ExitReason::Debug);
        }
    }

    #[test]
    fn the_final_step_carries_the_full_payload_digest_and_intermediates_are_registers_only() {
        // The amendment: every step but the last carries the CHEAP registers-only digest
        // (`sha256:regs`); the final recorded step's `step_digest` is the full-payload hash
        // (`sha256:final`), so memory divergence across the stepped window is caught end-to-end.
        let mut script = boot_prologue();
        script.extend(nop_steps(6));
        script.extend(exit_epilogue());
        let records = drive_script(0, script, 3).expect("bounded run");
        assert_eq!(records.len(), 3);
        let digest = |r: &RunRecord| r.step.as_ref().expect("stepped").step_digest.clone();
        assert_eq!(
            digest(&records[0]),
            "sha256:regs",
            "intermediate is registers-only"
        );
        assert_eq!(
            digest(&records[1]),
            "sha256:regs",
            "intermediate is registers-only"
        );
        assert_eq!(
            digest(&records[2]),
            "sha256:final",
            "the FINAL step pays the full-payload cost"
        );
        assert_ne!(
            digest(&records[1]),
            digest(&records[2]),
            "the final digest must differ from the intermediate registers-only one"
        );
        // The full-payload digest is also the record-level complete-state digest.
        for r in &records {
            assert_eq!(r.state_digest, "sha256:final");
        }
    }

    #[test]
    fn a_run_that_never_sentinels_stops_cleanly_at_n_the_livelock_case() {
        // The llsc-atomics livelock: each step clears the exclusive monitor, so the run never
        // reaches MARK_END or the sentinel. With --max-steps it stops cleanly at N rather than
        // running the seam dry (which would be NoExitSentinel). The window opened (MARK_BEGIN)
        // but never closed, so the window fields are self-consistent 0/0-style, NOT the oracle.
        let mut script = boot_prologue(); // PARAMS + MARK_BEGIN, then...
        script.extend(nop_steps(1_000)); // ...an endless stepped prefix, no MARK_END, no sentinel.
        let records = drive_script(5, script, 8).expect("a bounded livelock is not an error");
        assert_eq!(
            records.len(),
            8,
            "stopped cleanly at --max-steps, not run dry"
        );
        for r in &records {
            assert_eq!(r.exit_reason, ExitReason::Debug);
            // Window opened at MARK_BEGIN (counter 5) but never closed: end defaults to begin,
            // so measured_taken is 0 and the endpoints are self-consistent.
            assert_eq!(r.work_begin, 5);
            assert_eq!(r.work_end, 5);
            assert_eq!(r.measured_taken, 0);
            assert_eq!(
                r.measured_taken,
                r.work_end - r.work_begin,
                "step-record window fields are self-consistent"
            );
            // A cut-short run has no exit code; it is not a self-check failure.
            assert_eq!(r.payload_status, 0);
        }
    }

    #[test]
    fn a_bounded_run_stopped_before_the_window_opened_is_self_consistent_zeroes() {
        // --max-steps can even stop before MARK_BEGIN (a very small budget). The window never
        // opened, so work_begin/work_end/measured_taken are all 0 — self-consistent, ungraded.
        let mut script: Vec<Scripted> = b"PARAMS mode=managed scale=smoke seed=0x5eed\n"
            .iter()
            .map(|&b| Scripted::Console(b))
            .collect();
        // Steps BEFORE any MARK_BEGIN, then never a window/sentinel.
        script.extend(nop_steps(50));
        let records = drive_script(9, script, 2).expect("bounded before the window is fine");
        assert_eq!(records.len(), 2);
        for r in &records {
            assert_eq!(r.work_begin, 0);
            assert_eq!(r.work_end, 0);
            assert_eq!(r.measured_taken, 0);
        }
    }

    #[test]
    fn a_bounded_run_still_requires_its_params_attestation() {
        // Even cut short, a record must be labelable: a run that stepped before printing PARAMS
        // is refused (a smoke run must not masquerade as 1e8). The realistic bound prints PARAMS
        // before MARK_BEGIN, so this only bites a pathologically tiny budget.
        let script = nop_steps(50); // steps, but NO PARAMS line ever.
        assert!(matches!(
            drive_script(0, script, 3),
            Err(RunError::NoParamsMode)
        ));
    }

    #[test]
    fn a_bounded_llsc_run_need_not_attest_a_retry_term_it_never_reached() {
        // llsc-atomics HAS a reported retry term, printed AFTER the window closes — which a
        // livelocked bounded run never reaches. Requiring it would fail exactly the run
        // --max-steps exists to bound, so a cut-short reported-term payload reports 0 (its
        // window count is not oracle-graded, so no fabricated term can slip through).
        let mut llsc = spec(None);
        llsc.payload = Payload::LlscAtomics;
        let cell = Rc::new(Cell::new(0u64));
        let mut script = boot_prologue();
        script.extend(nop_steps(1_000)); // livelock: no MARK_END, no `LLSC retries=` line.
        let mut vcpu = ScriptedStepVcpu {
            exits: script.into(),
            pc: STEP_PC,
            last_opcode: None,
            vbar: STEP_VBAR,
            counter: Rc::clone(&cell),
            regs_digest: "sha256:regs".into(),
            digest: "sha256:final".into(),
            armed: false,
            last_read_reply: None,
        };
        let mut counter = ScriptedStepCounter { value: cell };
        let records = step_run(&mut vcpu, &mut counter, &llsc, 8).expect("a bounded llsc run");
        assert_eq!(records.len(), 8);
        assert!(records.iter().all(|r| r.reported_taken == 0));
    }

    #[test]
    fn classify_transition_is_a_pure_reuse_of_the_scanner() {
        // Direct unit coverage of the classifier, independent of the loop.
        assert_eq!(
            classify_transition(0xD503_201F, STEP_PC, STEP_PC + 4, STEP_VBAR),
            StepTransition::Sequential
        );
        assert_eq!(
            classify_transition(0x1400_0002, STEP_PC, STEP_PC + 8, STEP_VBAR),
            StepTransition::TakenBranch
        );
        assert_eq!(
            classify_transition(0x5400_0041, STEP_PC, STEP_PC + 4, STEP_VBAR),
            StepTransition::NotTakenBranch,
            "a conditional branch that fell through to PC+4 is a not-taken branch, not sequential \
             — the branch instruction retired"
        );
        assert_eq!(
            classify_transition(0xC85F_7C41, STEP_PC, STEP_PC + 4, STEP_VBAR),
            StepTransition::LlscExclusive
        );
        assert_eq!(
            classify_transition(0xD400_0001, STEP_PC, STEP_VBAR + 0x400, STEP_VBAR),
            StepTransition::ExceptionEntry
        );
        assert_eq!(
            classify_transition(0xD69F_03E0, STEP_PC, 0x4000_A000, STEP_VBAR),
            StepTransition::ExceptionReturn
        );
        assert_eq!(
            classify_transition(0xD503_207F, STEP_PC, STEP_PC + 4, STEP_VBAR),
            StepTransition::Wfi
        );
        // A non-branch step into the IRQ vector slot is an injection; into the sync slot, an
        // abort (exception entry).
        assert_eq!(
            classify_transition(0xD503_201F, STEP_PC, STEP_VBAR + 0x080, STEP_VBAR),
            StepTransition::Injection
        );
        assert_eq!(
            classify_transition(0xD503_201F, STEP_PC, STEP_VBAR, STEP_VBAR),
            StepTransition::ExceptionEntry
        );
    }
}
