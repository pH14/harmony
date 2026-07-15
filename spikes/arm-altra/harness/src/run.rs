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

use oracle_model::{Payload, Scale, UART_BASE};
use thiserror::Error;

use crate::console::{Console, Event};
use crate::evidence::{ExitReason, OverflowRecord, RunRecord};

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
    /// The guest touched an MMIO address that is not the console.
    #[error("the guest touched {addr:#x}, which is not the PL011 data register")]
    UnexpectedMmio {
        /// The address touched.
        addr: u64,
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

    'run: while status.is_none() {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                // The PL011 is the harness's one userspace MMIO device. A guest boots
                // by writing the UART's config registers (CR/IBRD/FBRD/LCR_H) and polls
                // its flag register before it can print a byte; with no in-kernel
                // PL011, every one of those is an exit the harness must model, or the
                // very first `runtime_init` write is rejected and no payload reaches
                // MARK_BEGIN. Anything OUTSIDE the PL011 page, though, is a genuine
                // finding (the GIC is the in-kernel vGIC; RAM is a real memory slot).
                if !is_pl011(addr) {
                    return Err(RunError::UnexpectedMmio { addr });
                }

                if !is_write {
                    // A register read — the guest is polling. Answer the flag register
                    // as "ready" (see PL011_FR_READY) and re-enter; any other PL011
                    // read (the payloads make none) also reads as zero. The value MUST
                    // be handed back, or KVM_RUN resumes with stale data.
                    let width = data.len().clamp(1, 8);
                    vcpu.complete_mmio_read(&PL011_FR_READY.to_le_bytes()[..width.min(4)])?;
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

            VcpuExit::Other(reason) => return Err(RunError::UnexpectedExit(reason)),
        }
    }

    let status = status.ok_or(RunError::NoExitSentinel)?;
    let begin = work_begin.ok_or(RunError::NoWindowOpen)?;
    let end = work_end.ok_or(RunError::NoWindowClose)?;
    if end < begin {
        return Err(RunError::CounterWentBackwards { begin, end });
    }
    let params_mode = params_mode.ok_or(RunError::NoParamsMode)?;

    // Cross-check the scale and seed the GUEST attested against the sample spec. The guest
    // prints what it read off the params page; if that disagrees with the (payload, scale,
    // seed) this record is being labelled with, the page was stale or mis-written and the
    // record would attribute its counts to an input the guest never ran. It is precisely
    // the seed-ignoring payloads whose counts still pass on a wrong seed, so this is the
    // only thing that catches a stale seed on them.
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

    // A payload whose count includes an in-band runtime term (`STXR`/seqlock retries)
    // MUST have printed it. If it did not, the count is unaccountable — defaulting to 0
    // would let the record match the oracle while claiming a reported term it never made,
    // and pass the floor checker on a fabricated zero. A payload with no reported term
    // that printed no retries legitimately reports 0.
    let reported_taken = if spec.payload.has_reported_term() {
        reported.ok_or(RunError::MissingReportedTerm(spec.payload))?
    } else {
        reported.unwrap_or(0)
    };

    // The landed state, digested at the sentinel — the thing AA-3's replay identity
    // and AA-6's rep floor actually compare. Read from the seam, never synthesised.
    let state_digest = vcpu.state_digest()?;
    if state_digest.is_empty() {
        return Err(RunError::EmptyStateDigest);
    }

    let overflow = target.map(|target| {
        // A lost PMI means no exit ever came: `landed` is None and `deliveries` is
        // 0. The record says so — it does not quietly substitute the window's end.
        let landed = landed.unwrap_or(0);
        OverflowRecord {
            armed: true,
            deliveries,
            advisory_exits,
            target,
            landed,
            skid: i64::try_from(i128::from(landed) - i128::from(target)).unwrap_or(i64::MIN),
            landed_digest: landed_digest.unwrap_or_default(),
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
        // With no overflow armed, the run ended at the console sentinel: the last
        // exit really was an MMIO one, and the record says so rather than borrowing
        // a mechanism it never exercised.
        exit_reason: mechanism_exit.unwrap_or(ExitReason::Mmio),
        overflow,
        // AA-2's single-step run path is arrival-day; this loop measures counting
        // windows, not steps, so it never produces step evidence.
        step: None,
        state_digest,
        params_mode,
        clockpage_mode,
        payload_status: i32::from(status),
    })
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
        }
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
}
