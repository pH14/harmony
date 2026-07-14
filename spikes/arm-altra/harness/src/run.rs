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

/// The PL011 data register, the guest's one MMIO door. Every byte the guest
/// "prints" is a store here, and every store is a `KVM_EXIT_MMIO`.
pub const PL011_DR: u64 = UART_BASE;

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

    /// A digest of the guest's architectural state — the registers and memory that
    /// AA-3's replay-identity and AA-6's bit-identity floors compare.
    ///
    /// Sampled once, after the guest has reached its exit sentinel, so two runs of
    /// the same seed are compared at the same point in their lives.
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

    /// Arm a one-shot overflow `delta` events from now.
    ///
    /// # Errors
    /// [`RunError::Seam`] if the event could not be re-armed.
    fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError>;
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
    let mut clockpage_mode: Option<String> = None;
    let mut reported_taken: u64 = 0;
    let mut status: Option<u8> = None;

    // Overflow bookkeeping, per record (§Evidence integrity #6).
    let mut target: Option<u64> = None;
    let mut deliveries: u64 = 0;
    let mut landed: Option<u64> = None;
    let mut mechanism_exit: Option<ExitReason> = None;

    'run: while status.is_none() {
        match vcpu.run()? {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                if addr != PL011_DR || !is_write {
                    // The payloads touch exactly one MMIO address, and only to
                    // write. Anything else is a finding, not something to skip past.
                    return Err(RunError::UnexpectedMmio { addr });
                }
                // A PL011 data-register store carries its byte in the low lane,
                // whatever the access width. A zero-length store is not a byte the
                // guest printed — it is a malformed exit, and it is refused rather
                // than skipped, because skipping it would silently drop console
                // content the record's attestations are read out of.
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
                            &mut clockpage_mode,
                            &mut reported_taken,
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

            // A mechanism exit: the armed overflow left KVM_RUN. Record the landing
            // and re-enter — the guest still has to finish and print its sentinel.
            exit @ (VcpuExit::Preempt | VcpuExit::SignalKick) => {
                let reason = if exit == VcpuExit::Preempt {
                    ExitReason::Preempt
                } else {
                    ExitReason::SignalKick
                };
                if target.is_none() {
                    // Nothing was armed, so nothing should have kicked. An
                    // unexplained kick is never absorbed into a clean record.
                    return Err(RunError::UnexpectedMechanismExit(reason));
                }
                deliveries += 1;
                // The FIRST landing is the one the contract is about; a second is a
                // duplicate delivery, and it is `deliveries` — not this field — that
                // makes it visible.
                if landed.is_none() {
                    landed = Some(counter.read()?);
                    mechanism_exit = Some(reason);
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
            target,
            landed,
            skid: i64::try_from(i128::from(landed) - i128::from(target)).unwrap_or(i64::MIN),
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
    clockpage_mode: &mut Option<String>,
    reported_taken: &mut u64,
) {
    if let Some(rest) = line.strip_prefix("PARAMS ") {
        if let Some(mode) = field(rest, "mode") {
            *params_mode = Some(mode.to_string());
        }
    } else if let Some(rest) = line.strip_prefix("CLOCKPAGE ") {
        if let Some(mode) = field(rest, "mode") {
            *clockpage_mode = Some(mode.to_string());
        }
        if let Some(n) = field(rest, "retries").and_then(|v| v.parse::<u64>().ok()) {
            *reported_taken = n;
        }
    } else if let Some(rest) = line.strip_prefix("LLSC ")
        && let Some(n) = field(rest, "retries").and_then(|v| v.parse::<u64>().ok())
    {
        *reported_taken = n;
    }
}

/// The value of `key=` in a space-separated `k=v` line.
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
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
    /// digest the seam would report at the sentinel.
    struct ScriptedVcpu {
        exits: std::collections::VecDeque<VcpuExit>,
        digest: String,
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
            }
        }
    }

    impl Vcpu for ScriptedVcpu {
        fn run(&mut self) -> Result<VcpuExit, RunError> {
            self.exits.pop_front().ok_or(RunError::NoExitSentinel)
        }

        fn state_digest(&mut self) -> Result<String, RunError> {
            Ok(self.digest.clone())
        }
    }

    /// A scripted counter: hands out a programmed sequence of readings.
    struct ScriptedCounter {
        readings: std::collections::VecDeque<u64>,
        armed: Vec<u64>,
    }

    impl ScriptedCounter {
        fn new(readings: &[u64]) -> ScriptedCounter {
            ScriptedCounter {
                readings: readings.iter().copied().collect(),
                armed: Vec::new(),
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
        assert_eq!(
            o.target, 1_500,
            "target is measured from the window's opening"
        );
        assert_eq!(o.landed, 1_500);
        assert_eq!(o.skid, 0);
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
        let mut counter = ScriptedCounter::new(&[1_000, 1_500, 2_001]);
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
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
    fn the_clockpage_mode_is_taken_from_the_guest() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
    fn a_double_window_open_is_refused() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"PARAMS mode=managed scale=smoke seed=0x1\n");
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
