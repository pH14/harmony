// SPDX-License-Identifier: AGPL-3.0-or-later
//! The ISA seam (`docs/ARCH-BOUNDARY.md` §A): the [`Arch`] trait — the
//! vocabulary of guest-observable CPU events and state, one implementation per
//! vendor — and the per-vendor modules that supply it.
//!
//! **Arch = exit kinds, the register/sysreg record set, the CPU-contract policy
//! tables, interrupt identities, and arch capability flags.** Everything above
//! the [`Backend`](crate::Backend) trait speaks only `(Gpa, Moment, bytes,
//! hashes)` plus the common exit vocabulary ([`CommonExit`](crate::CommonExit))
//! and is compiler-provably arch-blind: the engine cannot match a vendor's exit
//! enum, so an unhandled arch exit can never fall through an engine-written
//! wildcard arm (default-deny stays structural — the ruled two-level
//! [`Exit`](crate::Exit)).
//!
//! Two vendors: x86-64 ([`x86`]) and arm64 ([`arm64`], the `hm-cbt` pre-build
//! skeleton — additive exactly as this seam promised).

use core::fmt;

use crate::exit::ExitReason;

pub mod arm64;
pub mod x86;

/// One CPU architecture's vocabulary, as associated types. Vendors are
/// zero-sized types (e.g. [`x86::X86`]); the trait carries no methods beyond
/// what every consumer of an arch exit needs — nothing above `vmm-core` is
/// generic over this (`docs/ARCH-BOUNDARY.md` "generics stop at vmm-core").
pub trait Arch {
    /// The arch-specific exit variants — only the operations whose *identity*
    /// is per-ISA (x86: `Io`, `Rdmsr`/`Wrmsr`, `Cpuid`, `Rdtsc`/`Rdtscp`,
    /// `Rdrand`/`Rdseed`). Cross-arch concepts live in
    /// [`CommonExit`](crate::CommonExit).
    type Exit: ArchExit;
    /// The injectable events (x86: maskable interrupt vector / NMI; ARM later:
    /// the GIC INTID class).
    type Injection: Copy + fmt::Debug + PartialEq;
    /// The full guest-visible register record set for snapshot/restore.
    type VcpuState: Clone + fmt::Debug + PartialEq + Default;
    /// The installed CPU-contract policy (x86: the frozen CPUID model + the
    /// default-deny MSR filter; ARM later: an ID-register freeze + a
    /// trapped-sysreg table).
    type Policy: Clone + fmt::Debug + PartialEq;
    /// The interrupt identity the IRQ seam speaks (x86: the 8-bit vector; ARM
    /// later: a GIC INTID, which exceeds 8 bits).
    type IntId: Copy + fmt::Debug + PartialEq;
    /// The arch capability flags (x86: `deterministic_tsc`,
    /// `enforces_tsc_deadline_msr`). The *concepts* recur per-arch; the names
    /// don't — [`ArchCaps`] maps them to the engine's neutral questions.
    type Caps: ArchCaps;
    /// The arch-payload completions — completions whose payload shape is
    /// per-ISA (x86: the CPUID result quad). The neutral read/ok/fault/
    /// hypercall completions stay monomorphic methods on
    /// [`Backend`](crate::Backend).
    type Completion: fmt::Debug;
}

/// What every arch's exit enum must answer for the engine and the counters.
/// Implementations match their own enum **exhaustively** — a wildcard arm over
/// arch exits is a review-blocking defect (default-deny erosion).
pub trait ArchExit: Clone + fmt::Debug + PartialEq {
    /// The payload-free discriminant, for counting and reports.
    fn reason(&self) -> ExitReason;
    /// Whether servicing this exit stages a backend completion (a
    /// register-write and/or RIP-advance committed on the next entry). Drives
    /// the engine's restore-safety bookkeeping
    /// (`Vmm::completion_staged`).
    fn stages_completion(&self) -> bool;
}

/// The engine's neutral questions over a vendor's arch-named capability flags.
pub trait ArchCaps: Copy + fmt::Debug + PartialEq {
    /// Deterministic guest clock: reads of the guest's clock resolve to V-time
    /// (x86: `deterministic_tsc`), so the exact-count `run_until` seam is
    /// trustworthy for preemption/arrival deadlines.
    fn deterministic_clock(&self) -> bool;
}
