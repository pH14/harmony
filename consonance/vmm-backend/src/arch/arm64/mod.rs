// SPDX-License-Identifier: AGPL-3.0-or-later
//! The arm64 vendor: the [`Arch`] implementation ([`Arm64`]) and its value-type
//! vocabulary — the exit variants ([`Arm64Exit`]), the vCPU record set
//! ([`Arm64VcpuState`] and its subrecords), the installed-policy tables
//! ([`Arm64Policy`]: [`IdRegModel`] + [`SysregTrapPolicy`]), the injectable
//! events ([`Arm64Injection`]), the GICv3 interrupt identity ([`GicIntId`]),
//! and the capability flags ([`Arm64Caps`]).
//!
//! This is the `docs/ARCH-BOUNDARY.md` §D pre-build skeleton (`hm-cbt`): built
//! against the *unfrozen* trait (designed-not-frozen — AA-3's trait-freeze memo
//! owns the freeze), trusted only once the Altra spike (`docs/ARM-ALTRA.md`)
//! returns GO. Every constant the spike measures is a named `TODO(AA-N)`,
//! never a default; the one number stated here as fact — `BR_RETIRED` raw
//! event `0x21` — is a documented hardware fact (Arm ARM PMU event
//! enumeration), not a measurement.

mod state;

pub use state::{Arm64CoreRegs, Arm64SysregFile, Arm64VcpuState};

use crate::arch::{Arch, ArchCaps, ArchExit};
use crate::exit::ExitReason;

/// `BR_RETIRED` (raw PMU event `0x21`, retired **taken** branches) — the arm64
/// work-counter event (`docs/ARM-PORT.md` §2: a *different* event than x86's
/// retired conditional branches `0x1c4`). The event *number* is a documented
/// hardware fact; every count offset, density, and `skid_margin` derived from
/// it is the spike's — `TODO(AA-1)`: measured constants pack.
pub const RAW_BR_RETIRED: u64 = 0x21;

/// The arm64 vendor (a zero-sized type; `docs/ARCH-BOUNDARY.md` §A/§D).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64;

impl Arch for Arm64 {
    type Exit = Arm64Exit;
    type Injection = Arm64Injection;
    type VcpuState = Arm64VcpuState;
    type Policy = Arm64Policy;
    type IntId = GicIntId;
    type Caps = Arm64Caps;
    type Completion = Arm64Completion;
}

/// The arm64-specific exit variants — the per-ISA half of the two-level
/// [`Exit`](crate::Exit). Cross-arch exits (MMIO — including the arm64
/// reserved-GPA hypercall doorbell — idle/WFI, shutdown, deadline) live in
/// [`CommonExit`](crate::CommonExit); do **not** duplicate them here.
///
/// **The whole enum is patched-ABI surface, not stock**: stock KVM/arm64
/// emulates supported sysregs and UNDEFs unsupported ones **in-kernel** — it
/// never surfaces a sysreg trap to userspace (there is no MSR-filter
/// analogue). So no variant here is reachable on the stock backend, exactly as
/// x86's `Cpuid`/`Rdtsc`/`Hypercall` exits are patched-only. The variants
/// exist for the AA-3 patched backend; the roster grows exactly as the AA-6
/// contract truth table dictates, each variant exhaustively matched by
/// `dispatch_arch` (no wildcard arm — default-deny stays structural).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Exit {
    /// A trapped ID/PMU/timer system-register access (the `HCR_EL2`/`MDCR_EL2`
    /// trap groups' userspace surface). A read (`write == None`) resolves via
    /// `complete_read` or `complete_fault` (deny-UNDEF); a write via
    /// `complete_ok` or `complete_fault` — mirroring the x86 MSR exit
    /// discipline. `TODO(patched-abi)`: the concrete exit ABI is the AA-3
    /// 0004-analogue patch's; this variant is the ruled *shape* only.
    Sysreg {
        /// The system-register encoding, packed `op0:op1:CRn:CRm:op2` exactly
        /// as ESR_EL2's ISS encodes a trapped `MRS`/`MSR` (bits `[21:1]` of
        /// the ISS, the architectural sysreg identity).
        sysreg: u32,
        /// `Some(v)` = a trapped write of `v` (`MSR`); `None` = a trapped read
        /// (`MRS`, awaits `complete_read`/`complete_fault`).
        write: Option<u64>,
    },
}

impl ArchExit for Arm64Exit {
    fn reason(&self) -> ExitReason {
        match self {
            Arm64Exit::Sysreg { .. } => ExitReason::Sysreg,
        }
    }

    fn stages_completion(&self) -> bool {
        // Both directions stage one: a read stages the destination-register
        // write, a write stages the fault-or-acknowledge resolution (the x86
        // Rdmsr/Wrmsr discipline).
        match self {
            Arm64Exit::Sysreg { .. } => true,
        }
    }
}

/// A GICv3 interrupt identity (INTID) — the arm64 [`Arch::IntId`]. `u32`-wide:
/// SGIs `0..16` (**deliverable** — not reserved as x86's vectors `< 16` are),
/// PPIs `16..32`, SPIs `32..=` the distributor-configured implementation limit
/// (`GICD_TYPER.ITLinesNumber`, architectural max **1019**); `1020..1024` are
/// special INTIDs ([`GicIntId::SPURIOUS`] = 1023); `1024..` (extended SPI /
/// LPI) are not modeled by the skeleton.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct GicIntId(pub u32);

impl GicIntId {
    /// The spurious INTID (`1023`): "no interrupt pending" at acknowledge.
    pub const SPURIOUS: GicIntId = GicIntId(1023);
    /// The architectural maximum ordinary SPI INTID (`1019`).
    pub const MAX_SPI: u32 = 1019;

    /// `true` for a software-generated interrupt (`0..16`).
    pub fn is_sgi(self) -> bool {
        self.0 < 16
    }

    /// `true` for a private peripheral interrupt (`16..32`).
    pub fn is_ppi(self) -> bool {
        (16..32).contains(&self.0)
    }

    /// `true` for a shared peripheral interrupt (`32..=1019`).
    pub fn is_spi(self) -> bool {
        (32..=Self::MAX_SPI).contains(&self.0)
    }
}

/// An event the VMM injects at a V-time-chosen boundary. arm64 has no NMI: the
/// only maskable-injection identity is a GIC INTID (IRQ; FIQ/Group-0 is not
/// modeled by the skeleton — `TODO(AA-6)`: the contract's group model).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Injection {
    /// A maskable interrupt identity for the next injectable entry.
    Interrupt {
        /// The GICv3 INTID.
        intid: GicIntId,
    },
}

/// The installed arm64 CPU-contract policy: the frozen synthetic `ID_AA64*`
/// model and the default-deny trapped-sysreg table, installed together
/// (before the first run) through [`Backend::set_policy`](crate::Backend).
///
/// **A policy *skeleton*** (spec non-goal 5): the shapes are ruled
/// (`docs/ARCH-BOUNDARY.md` §B, ARM row — "ID-reg freeze + trapped-sysreg
/// table, same data-driven table→model→enforce shape"), but the concrete row
/// set is `TODO(AA-6)` (the enforcement-mechanism truth table) and the trap
/// *enforcement* is `TODO(patched-abi)` (AA-3). It does not claim enforcement
/// completeness.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Arm64Policy {
    /// The frozen synthetic ID-register model (the det-N1 analogue of
    /// `det-cfl-v1`), installed config-time via KVM's writable-ID-register
    /// surface (`KVM_SET_ONE_REG` on the ID regs before the first `KVM_RUN`) —
    /// reachable on stock KVM.
    pub id_regs: IdRegModel,
    /// The default-deny trapped-sysreg table. Recording the shape only: the
    /// runtime trap-to-userspace enforcement is the AA-3 patched backend's.
    pub sysreg_traps: SysregTrapPolicy,
}

/// The frozen guest-visible `ID_AA64*` register values, keyed by the packed
/// `op0:op1:CRn:CRm:op2` sysreg encoding (the same packing as
/// [`Arm64Exit::Sysreg`]). Sorted map so iteration order (and any encoding
/// derived from it) is deterministic (rule #4).
///
/// Empty in the skeleton: the concrete frozen values are the ARM CPU
/// contract's — `TODO(AA-6)`.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct IdRegModel {
    /// `packed sysreg encoding → frozen value`.
    pub regs: std::collections::BTreeMap<u32, u64>,
}

/// The default-deny trapped-sysreg table: the set of sysreg encodings whose
/// guest access must surface (patched ABI) rather than be emulated in-kernel.
/// **Default-deny is the posture, not this set**: an encoding absent here is
/// denied *by the contract*, and the skeleton's empty set simply records that
/// no row has been ruled yet — `TODO(AA-6)`: the enforcement-mechanism truth
/// table supplies the rows; `TODO(patched-abi)`: AA-3 supplies the exits.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SysregTrapPolicy {
    /// The packed sysreg encodings to trap.
    pub trapped: std::collections::BTreeSet<u32>,
}

/// The arm64 arch capability flags (the per-vendor half of
/// [`Capabilities`](crate::Capabilities)). The *concepts* mirror x86's
/// (`deterministic_tsc` / `enforces_tsc_deadline_msr`); the names are arm64's
/// own registers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Arm64Caps {
    /// Guest reads of the virtual counter resolve to V-time — on arm64 via the
    /// paravirt work-derived clock page (`docs/PARAVIRT-CLOCK.md` §4.2: no
    /// `CNTVCT` trap exists on reachable silicon, so closure is contract-level,
    /// never interception). `TODO(AA-5)`: validated on silicon; **honestly
    /// `false` for the stock backend.**
    pub deterministic_cntvct: bool,
    /// Can loudly enforce the contract's disposition on the guest's EL1
    /// virtual-timer compare sysregs (`CNTV_CVAL_EL0`/`CNTV_TVAL_EL0`) — the
    /// arm64 analogue of x86's `IA32_TSC_DEADLINE` enforcement.
    /// `TODO(patched-abi)`: stock KVM services the virtual timer in-kernel.
    pub enforces_cntv_cval: bool,
}

impl ArchCaps for Arm64Caps {
    fn deterministic_clock(&self) -> bool {
        self.deterministic_cntvct
    }
}

/// The arm64 arch-payload completions ([`Arch::Completion`]). **Uninhabited in
/// the skeleton**: the one arch exit ([`Arm64Exit::Sysreg`]) resolves through
/// the neutral `complete_read`/`complete_ok`/`complete_fault` trio, and no
/// arm64 completion carries an arch-shaped payload (x86's is the CPUID quad).
/// Grows only as the AA-6 contract dictates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Arm64Completion {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sysreg_exit_reason_and_completion_staging() {
        let read = Arm64Exit::Sysreg {
            sysreg: 0x0001_2345,
            write: None,
        };
        let write = Arm64Exit::Sysreg {
            sysreg: 0x0001_2345,
            write: Some(7),
        };
        assert_eq!(read.reason(), ExitReason::Sysreg);
        assert_eq!(write.reason(), ExitReason::Sysreg);
        assert!(read.stages_completion());
        assert!(write.stages_completion());
    }

    #[test]
    fn gic_intid_classes_are_the_gicv3_identity_space() {
        // SGIs deliver on arm64 (never x86's `< 16` reserved rule).
        assert!(GicIntId(0).is_sgi());
        assert!(GicIntId(15).is_sgi());
        assert!(!GicIntId(16).is_sgi());
        assert!(GicIntId(16).is_ppi());
        assert!(GicIntId(31).is_ppi());
        assert!(!GicIntId(32).is_ppi());
        assert!(GicIntId(32).is_spi());
        assert!(GicIntId(1019).is_spi());
        // 1020..1024 are special INTIDs, not SPIs.
        assert!(!GicIntId(1020).is_spi());
        assert_eq!(GicIntId::SPURIOUS, GicIntId(1023));
    }

    #[test]
    fn arm64_caps_answer_the_neutral_clock_question() {
        let stock = Arm64Caps {
            deterministic_cntvct: false,
            enforces_cntv_cval: false,
        };
        assert!(!stock.deterministic_clock());
        let det = Arm64Caps {
            deterministic_cntvct: true,
            enforces_cntv_cval: false,
        };
        assert!(det.deterministic_clock());
    }
}
