// SPDX-License-Identifier: AGPL-3.0-or-later
//! The observable-exit surface: `Exit` and the per-reason trap counters.
//!
//! `Exit` **is** the CPU/MSR contract's trapped surface. Default-deny is
//! structural: an operation not represented here either never exits (the backend
//! never enabled its exit control / it is serviced in-kernel) or is a contract
//! violation that fails closed as a [`crate::BackendError`]. Nothing else is
//! reachable through the trait.

use crate::types::{Gpa, Moment};

/// Every way the guest can become observable to the VMM.
///
/// Read-style variants (`Io { write: None }`, `Mmio { write: None }`, `Rdmsr`,
/// and the instruction-reads `Rdtsc`/`Rdtscp`/`Rdrand`/`Rdseed`), `Wrmsr`,
/// `Hypercall`, and `Cpuid` stay **pending** until the matching completion is
/// called; resuming `run` with one un-serviced is
/// [`BackendError::PendingCompletion`](crate::BackendError::PendingCompletion).
/// Only `Io` OUT, `Mmio` store, `Idle`, `Shutdown`, and `Deadline` need no
/// completion.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Exit {
    /// Port I/O. `write = Some(v)` is `OUT(v)` (no completion); `write = None` is
    /// `IN`, resolved by `complete_read`.
    Io {
        /// I/O port.
        port: u16,
        /// Access width in bytes (1/2/4).
        size: u8,
        /// `Some(v)` = OUT value; `None` = IN (awaits `complete_read`).
        write: Option<u32>,
    },
    /// MMIO (the userspace xAPIC page at `0xFEE0_0000` falls through here, R1).
    /// `write = Some(v)` is a store (no completion); `None` is a load, resolved
    /// by `complete_read`.
    Mmio {
        /// Guest-physical address of the access.
        gpa: Gpa,
        /// Access width in bytes.
        size: u8,
        /// `Some(v)` = store value; `None` = load (awaits `complete_read`).
        write: Option<u64>,
    },
    /// A filtered MSR read → `complete_read(value)` (allow/fixed/emulate) or
    /// `complete_fault()` (the contract's `deny-gp`).
    Rdmsr {
        /// The MSR index the guest read.
        index: u32,
    },
    /// A filtered MSR write → `complete_ok()` (allow/drop) or `complete_fault()`
    /// (`deny-gp`). Stays pending until one is called: resuming without a
    /// completion is taken by KVM as a silent *allow* (`msr.error == 0`).
    Wrmsr {
        /// The MSR index the guest wrote.
        index: u32,
        /// The value the guest wrote.
        value: u64,
    },
    /// VMCALL transport (INTEGRATION.md §1) → `complete_hypercall(ret)`. **Not
    /// surfaced by stock `KvmBackend`** (stock KVM services VMCALL in-kernel); it
    /// exists for `PatchedKvmBackend`/`DirectVmxBackend`.
    Hypercall(HypercallFrame),
    /// CPUID → `complete_cpuid(eax, ebx, ecx, edx)`. **Stock `KvmBackend`
    /// services CPUID in-kernel from the `set_cpuid` table and does not surface
    /// this**; a backend that does is completed with the dyn-overlaid quad.
    Cpuid {
        /// CPUID leaf (`EAX`).
        leaf: u32,
        /// CPUID subleaf (`ECX`).
        subleaf: u32,
    },
    /// `RDTSC`. Backend-dependent (contract §1). **Not surfaced by stock
    /// `KvmBackend`** — a declared determinism hole, never a runtime trap.
    Rdtsc,
    /// `RDTSCP`. Backend-dependent; not surfaced by stock `KvmBackend`.
    Rdtscp,
    /// `RDRAND`. Backend-dependent; not surfaced by stock `KvmBackend`.
    Rdrand {
        /// Destination width in bytes (2/4/8).
        width: u8,
    },
    /// `RDSEED`. Backend-dependent; not surfaced by stock `KvmBackend`.
    Rdseed {
        /// Destination width in bytes (2/4/8).
        width: u8,
    },
    /// The guest went idle waiting for an event (`KVM_EXIT_HLT`). Idle-skip
    /// (INTEGRATION.md §3) or terminal; vmm-core decides. No completion.
    Idle,
    /// `KVM_EXIT_SHUTDOWN` (triple fault / guest shutdown). Terminal. No
    /// completion.
    Shutdown,
    /// `run_until` reached the V-time deadline with no guest exit first. (Phase
    /// 2; stock `KvmBackend` never produces it in this task's scope.) No
    /// completion.
    Deadline {
        /// The V-time actually reached (≥ the requested deadline by the skid
        /// margin task 07 bounds).
        reached: Moment,
    },
}

impl Exit {
    /// The payload-free discriminant of this exit, for counting and reports.
    pub fn reason(&self) -> ExitReason {
        match self {
            Exit::Io { .. } => ExitReason::Io,
            Exit::Mmio { .. } => ExitReason::Mmio,
            Exit::Rdmsr { .. } => ExitReason::Rdmsr,
            Exit::Wrmsr { .. } => ExitReason::Wrmsr,
            Exit::Hypercall(_) => ExitReason::Hypercall,
            Exit::Cpuid { .. } => ExitReason::Cpuid,
            Exit::Rdtsc => ExitReason::Rdtsc,
            Exit::Rdtscp => ExitReason::Rdtscp,
            Exit::Rdrand { .. } => ExitReason::Rdrand,
            Exit::Rdseed { .. } => ExitReason::Rdseed,
            Exit::Idle => ExitReason::Idle,
            Exit::Shutdown => ExitReason::Shutdown,
            Exit::Deadline { .. } => ExitReason::Deadline,
        }
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
/// `PatchedKvmBackend`/`DirectVmxBackend` raise them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities {
    /// Human-readable backend name for the report (e.g. `"kvm-stock"`).
    pub name: &'static str,
    /// Surfaces RDTSC/RDTSCP as exits resolvable to a V-time value (NOT host
    /// TSC).
    pub deterministic_tsc: bool,
    /// Surfaces RDRAND/RDSEED as exits resolvable to a seeded stream (NOT host
    /// RNG).
    pub deterministic_rng: bool,
    /// Can loudly enforce a `deny-gp` on `IA32_TSC_DEADLINE` (`0x6E0`) writes.
    /// Moot under R1 (the guest never writes it) but declared honestly: stock
    /// KVM swallows it in the WRMSR fastpath.
    pub enforces_tsc_deadline_msr: bool,
}

/// The discriminant of [`Exit`] (payload-free), for [`ExitCounts::entries`] and
/// the unison report. Ordered to match `ExitCounts`' field order.
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
    /// VMCALL transport.
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
