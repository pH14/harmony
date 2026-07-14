// SPDX-License-Identifier: AGPL-3.0-or-later
//! The observable-exit surface: the two-level [`Exit`] and the per-reason trap
//! counters.
//!
//! `Exit` **is** the CPU/MSR contract's trapped surface. Default-deny is
//! structural, at two levels (`docs/ARCH-BOUNDARY.md` §A): an operation not
//! represented here either never exits (the backend never enabled its exit
//! control / it is serviced in-kernel) or is a contract violation that fails
//! closed as a [`crate::BackendError`]; and the arch-specific variants live in
//! each vendor's own exit enum ([`Arch::Exit`]), exhaustively matched by that
//! vendor's own dispatch — an unhandled arch exit can never fall through an
//! engine-written wildcard arm. (A single superset enum and an opaque exit are
//! the ruling's *rejected* alternatives.)

use crate::arch::{Arch, ArchExit};
use crate::types::{Gpa, Moment};

/// Every way the guest can become observable to the VMM: a cross-arch
/// [`Common`](Exit::Common) exit, or the vendor's own [`Arch`](Exit::Arch)
/// exit.
///
/// Read-style variants (`Mmio { write: None }` and the arch read-style exits),
/// the arch MSR/CPUID exits, and `Hypercall` stay **pending** until the
/// matching completion is called; resuming `run` with one un-serviced is
/// [`BackendError::PendingCompletion`](crate::BackendError::PendingCompletion).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Exit<A: Arch> {
    /// A cross-arch exit — one concept on every vendor.
    Common(CommonExit),
    /// The vendor's own exit — dispatched only by that vendor's dispatch,
    /// which matches the enum exhaustively.
    Arch(A::Exit),
}

impl<A: Arch> Exit<A> {
    /// The payload-free discriminant of this exit, for counting and reports.
    pub fn reason(&self) -> ExitReason {
        match self {
            Exit::Common(c) => c.reason(),
            Exit::Arch(e) => e.reason(),
        }
    }

    /// Whether servicing this exit stages a backend completion (a
    /// register-write and/or RIP-advance committed on the next entry): every
    /// read-style / MSR / CPUID / determinism exit calls a `complete_*`.
    /// Write-style stores, `Idle`, `Shutdown`, `Deadline`, and the unmodeled
    /// `Hypercall` resume with nothing pending. Drives the engine's
    /// restore-safety bookkeeping (`Vmm::completion_staged`).
    pub fn stages_completion(&self) -> bool {
        match self {
            Exit::Common(c) => c.stages_completion(),
            Exit::Arch(e) => e.stages_completion(),
        }
    }
}

/// The cross-arch exits — operations whose identity is the same on every
/// vendor. Everything else is per-vendor vocabulary ([`Arch::Exit`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommonExit {
    /// MMIO (on x86 the userspace xAPIC page at `0xFEE0_0000` falls through
    /// here, R1). `write = Some(v)` is a store (no completion); `None` is a
    /// load, resolved by `complete_read`.
    Mmio {
        /// Guest-physical address of the access.
        gpa: Gpa,
        /// Access width in bytes.
        size: u8,
        /// `Some(v)` = store value; `None` = load (awaits `complete_read`).
        write: Option<u64>,
    },
    /// Hypercall transport (INTEGRATION.md §1) → `complete_hypercall(ret)`.
    /// **Not surfaced by stock `KvmBackend`** (stock KVM services VMCALL
    /// in-kernel); it exists for `PatchedKvmBackend`/`DirectVmxBackend`.
    Hypercall(HypercallFrame),
    /// The guest went idle waiting for an event (x86 `HLT` / ARM `WFI` — one
    /// concept above the trait; `KVM_EXIT_HLT` on x86 KVM). Idle-skip
    /// (INTEGRATION.md §3) or terminal; vmm-core decides. No completion.
    Idle,
    /// `KVM_EXIT_SHUTDOWN` (an unrecoverable guest fault / guest shutdown).
    /// Terminal. No completion.
    Shutdown,
    /// `run_until` reached the V-time deadline with no guest exit first. No
    /// completion.
    Deadline {
        /// The V-time actually reached (≥ the requested deadline by the skid
        /// margin task 07 bounds).
        reached: Moment,
    },
}

impl CommonExit {
    /// The payload-free discriminant of this exit, for counting and reports.
    pub fn reason(&self) -> ExitReason {
        match self {
            CommonExit::Mmio { .. } => ExitReason::Mmio,
            CommonExit::Hypercall(_) => ExitReason::Hypercall,
            CommonExit::Idle => ExitReason::Idle,
            CommonExit::Shutdown => ExitReason::Shutdown,
            CommonExit::Deadline { .. } => ExitReason::Deadline,
        }
    }

    /// See [`Exit::stages_completion`]. Of the common exits only an MMIO
    /// **load** stages one (`Hypercall` is unmodeled above the trait and
    /// resumes with nothing pending).
    pub fn stages_completion(&self) -> bool {
        match self {
            CommonExit::Mmio { write: None, .. } => true,
            CommonExit::Mmio { write: Some(_), .. }
            | CommonExit::Hypercall(_)
            | CommonExit::Idle
            | CommonExit::Shutdown
            | CommonExit::Deadline { .. } => false,
        }
    }
}

impl<A: Arch> From<CommonExit> for Exit<A> {
    fn from(c: CommonExit) -> Self {
        Exit::Common(c)
    }
}

/// The hypercall argument frame (INTEGRATION.md §1): four guest argument slots
/// in transport-ABI order — `args[0]` = the transport magic `0x3150_4348`,
/// `args[1]` = request-page GPA, `args[2]` = response-page GPA, `args[3]` is
/// reserved/forward-compat. Which guest registers carry the slots is the
/// backend's per-arch knowledge (x86: `RAX`, `RBX`, `RCX`, `RDX`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct HypercallFrame {
    /// The four argument slots, in transport-ABI order.
    pub args: [u64; 4],
}

/// What this backend can honestly provide. The unison report reads this to
/// **refuse to claim determinism** for a payload that needs a capability the
/// backend lacks. Stock `KvmBackend` reports every determinism field `false`;
/// `PatchedKvmBackend`/`DirectVmxBackend` raise them. `C` is the vendor's
/// arch-named flag set ([`Arch::Caps`]); the engine reads it only through the
/// neutral [`ArchCaps`](crate::arch::ArchCaps) questions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities<C> {
    /// Human-readable backend name for the report (e.g. `"kvm-stock"`).
    pub name: &'static str,
    /// Surfaces the guest's hardware-RNG reads as exits resolvable to a seeded
    /// stream (NOT the host RNG). x86: `RDRAND`/`RDSEED`.
    pub deterministic_rng: bool,
    /// The vendor's arch-named capability flags.
    pub arch: C,
}

/// The payload-free discriminant of [`Exit`], for [`ExitCounts::entries`] and
/// the unison report. Ordered to match `ExitCounts`' field order.
///
/// The roster names the *whole* trapped surface — the common exits plus (today)
/// the x86 vendor's. Counters are observability, not dispatch: default-deny
/// lives in the two-level [`Exit`], and this roster gains vendor variants
/// additively when a new vendor lands (the ARM window owns that evolution).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum ExitReason {
    /// Port I/O.
    Io,
    /// MMIO.
    Mmio,
    /// MSR read.
    Rdmsr,
    /// MSR write.
    Wrmsr,
    /// Hypercall transport.
    Hypercall,
    /// CPUID.
    Cpuid,
    /// `RDTSC`.
    Rdtsc,
    /// `RDTSCP`.
    Rdtscp,
    /// `RDRAND`.
    Rdrand,
    /// `RDSEED`.
    Rdseed,
    /// Idle halt.
    Idle,
    /// Shutdown / unrecoverable guest fault.
    Shutdown,
    /// `run_until` deadline reached.
    Deadline,
}

/// Per-exit-reason trap counts since the last reset (R-Backend observability).
/// Plain `u64` counters surfaced in the unison report. **Recorded every
/// run** and the empirical input that gates the deferred RDTSC optimization.
/// Deterministic: equal run ⇒ equal counts, fixed accessor order.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct ExitCounts {
    /// Port-I/O exits.
    pub io: u64,
    /// MMIO exits.
    pub mmio: u64,
    /// MSR-read exits.
    pub rdmsr: u64,
    /// MSR-write exits.
    pub wrmsr: u64,
    /// Hypercall exits.
    pub hypercall: u64,
    /// CPUID exits.
    pub cpuid: u64,
    /// `RDTSC` exits.
    pub rdtsc: u64,
    /// `RDTSCP` exits.
    pub rdtscp: u64,
    /// `RDRAND` exits.
    pub rdrand: u64,
    /// `RDSEED` exits.
    pub rdseed: u64,
    /// Idle-halt exits.
    pub idle: u64,
    /// Shutdown exits.
    pub shutdown: u64,
    /// `run_until` deadline exits.
    pub deadline: u64,
}

impl ExitCounts {
    /// Total trapped exits — the sum of every per-reason counter. **Saturating**:
    /// the counters are public and individually saturate at `u64::MAX`, so a plain
    /// `sum()` could overflow (panic in debug / wrap in release). The fold
    /// saturates instead, matching the per-counter discipline.
    pub fn total(&self) -> u64 {
        self.entries()
            .iter()
            .fold(0u64, |acc, (_, n)| acc.saturating_add(*n))
    }

    /// `(reason, count)` pairs in a fixed, deterministic order (the field order
    /// above), for the report. Exactly one entry per [`ExitReason`].
    pub fn entries(&self) -> [(ExitReason, u64); 13] {
        [
            (ExitReason::Io, self.io),
            (ExitReason::Mmio, self.mmio),
            (ExitReason::Rdmsr, self.rdmsr),
            (ExitReason::Wrmsr, self.wrmsr),
            (ExitReason::Hypercall, self.hypercall),
            (ExitReason::Cpuid, self.cpuid),
            (ExitReason::Rdtsc, self.rdtsc),
            (ExitReason::Rdtscp, self.rdtscp),
            (ExitReason::Rdrand, self.rdrand),
            (ExitReason::Rdseed, self.rdseed),
            (ExitReason::Idle, self.idle),
            (ExitReason::Shutdown, self.shutdown),
            (ExitReason::Deadline, self.deadline),
        ]
    }

    /// Increment the counter for `reason` (saturating, so a pathological run can
    /// never wrap a counter into a smaller value and corrupt the report). Used by
    /// the backends (`mock` / Linux `KvmBackend`); dead in a bare default build.
    #[cfg_attr(
        not(any(feature = "mock", test, target_os = "linux")),
        allow(dead_code)
    )]
    pub(crate) fn bump(&mut self, reason: ExitReason) {
        let slot = match reason {
            ExitReason::Io => &mut self.io,
            ExitReason::Mmio => &mut self.mmio,
            ExitReason::Rdmsr => &mut self.rdmsr,
            ExitReason::Wrmsr => &mut self.wrmsr,
            ExitReason::Hypercall => &mut self.hypercall,
            ExitReason::Cpuid => &mut self.cpuid,
            ExitReason::Rdtsc => &mut self.rdtsc,
            ExitReason::Rdtscp => &mut self.rdtscp,
            ExitReason::Rdrand => &mut self.rdrand,
            ExitReason::Rdseed => &mut self.rdseed,
            ExitReason::Idle => &mut self.idle,
            ExitReason::Shutdown => &mut self.shutdown,
            ExitReason::Deadline => &mut self.deadline,
        };
        *slot = slot.saturating_add(1);
    }
}
