// SPDX-License-Identifier: AGPL-3.0-or-later
//! The live, in-memory arm64 vCPU snapshot the backend produces (`save`) and
//! consumes (`restore`).
//!
//! The counterpart of `arch/x86/state.rs`: vmm-core marshals an
//! [`Arm64VcpuState`] into `vm-state`'s arm64 record set for the codec; per
//! rule #2 this crate does not depend on `vm-state`, so the field set is
//! mirrored plain data kept consistent by review.
//!
//! **A skeleton subset, deliberately.** The core registers (`x0..x30`, `SP`,
//! `PC`, `PSTATE`) and a small named EL1 system-register file are enough to
//! build, seal, and round-trip a trivial vCPU through the container — the M1
//! keystone. **Which sysregs a snapshot must carry is AA-6's measured
//! decision** (`docs/ARM-ALTRA.md` §AA-6); the full record set is
//! `TODO(AA-6)`, never guessed here. designed-not-frozen (AA-3).

use crate::types::MpState;

/// Full guest-visible arm64 vCPU state for snapshot/restore (skeleton subset;
/// full sysreg set `TODO(AA-6)`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64VcpuState {
    /// Core registers (`KVM_GET_ONE_REG` over the `KVM_REG_ARM_CORE` ids).
    pub core: Arm64CoreRegs,
    /// The skeleton EL1 system-register file (`KVM_GET_ONE_REG` over
    /// `KVM_REG_ARM64_SYSREG` ids).
    pub sysregs: Arm64SysregFile,
    /// Runnable vs halted (`KVM_GET_MP_STATE`; WFI-halted on arm64).
    pub mp_state: MpState,
}

/// The arm64 core register file (`struct kvm_regs.regs` — `user_pt_regs` —
/// plus the EL1 banked exception registers KVM carries alongside it).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Arm64CoreRegs {
    /// General-purpose registers `x0..x30`.
    pub x: [u64; 31],
    /// The stack pointer selected at the current EL (`SP_EL0` at EL0).
    pub sp: u64,
    /// The program counter.
    pub pc: u64,
    /// The processor state (`PSTATE`/`SPSR` layout: `DAIF`, the mode field,
    /// the condition flags).
    pub pstate: u64,
    /// `SP_EL1` — the banked EL1 stack pointer.
    pub sp_el1: u64,
    /// `ELR_EL1` — the EL1 exception link register.
    pub elr_el1: u64,
    /// `SPSR_EL1` — the EL1 saved processor state.
    pub spsr_el1: u64,
}

/// The skeleton EL1 system-register file: the named subset a trivial vCPU
/// round-trip needs (MMU/translation, vectors, thread pointers, the traps-
/// and-counter control the determinism contract cares about). **Not** the
/// snapshot contract: `TODO(AA-6)` owns which sysregs a snapshot must carry;
/// this file grows only from that measured record set.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[allow(missing_docs)] // the system-register names are self-documenting
pub struct Arm64SysregFile {
    pub sctlr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub cpacr_el1: u64,
    pub esr_el1: u64,
    pub far_el1: u64,
    pub tpidr_el0: u64,
    pub tpidr_el1: u64,
    /// `CNTKCTL_EL1` — the EL0 counter-access control the paravirt-clock
    /// closure story turns off (`docs/PARAVIRT-CLOCK.md` §4.2); carried so the
    /// closure posture survives a snapshot.
    pub cntkctl_el1: u64,
}
