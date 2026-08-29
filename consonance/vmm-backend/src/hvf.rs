// SPDX-License-Identifier: AGPL-3.0-or-later
//! Apple Silicon Hypervisor.framework backend for the M1 virtual_time V-time
//! bring-up path.
//!
//! This backend is deliberately honest and narrow. The measured macOS 26.4.1
//! surface traps WFI, stage-2 MMIO, PMU, and the GICv3 CPU-interface sysregs,
//! but it does not trap `CNTVCT_EL0` or virtual-timer programming. The audited
//! guest therefore obtains time from the paravirtual V-time page and the
//! capability report keeps direct-counter/timer enforcement false.

use core::ffi::c_void;
use std::ptr::{self, NonNull};

use crate::arch::arm64::{
    Arm64, Arm64Caps, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState, GicIntId,
    canonicalize_core_regs, has_noncanonical_core_regs,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts, HypercallFrame};
use crate::types::{Gpa, MpState};

const HV_PAGE_SIZE: usize = 16 * 1024;
const HV_MEMORY_READ: u64 = 1;
const HV_MEMORY_WRITE: u64 = 2;
const HV_MEMORY_EXEC: u64 = 4;

const HV_EXIT_REASON_CANCELED: u32 = 0;
const HV_EXIT_REASON_EXCEPTION: u32 = 1;
const HV_EXIT_REASON_VTIMER_ACTIVATED: u32 = 2;

const HV_INTERRUPT_TYPE_IRQ: u32 = 0;
const HV_INTERRUPT_TYPE_FIQ: u32 = 1;

const HV_REG_PC: u32 = 31;
const HV_REG_FPCR: u32 = 32;
const HV_REG_FPSR: u32 = 33;
const HV_REG_CPSR: u32 = 34;

const HV_SYS_REG_SCTLR_EL1: u16 = 0xc080;
const HV_SYS_REG_DBGBVR0_EL1: u16 = 0x8004;
const HV_SYS_REG_DBGBCR0_EL1: u16 = 0x8005;
const HV_SYS_REG_DBGWVR0_EL1: u16 = 0x8006;
const HV_SYS_REG_DBGWCR0_EL1: u16 = 0x8007;
const HV_SYS_REG_MDSCR_EL1: u16 = 0x8012;
const HV_SYS_REG_CPACR_EL1: u16 = 0xc082;
const HV_SYS_REG_TTBR0_EL1: u16 = 0xc100;
const HV_SYS_REG_TTBR1_EL1: u16 = 0xc101;
const HV_SYS_REG_TCR_EL1: u16 = 0xc102;
const HV_SYS_REG_SPSR_EL1: u16 = 0xc200;
const HV_SYS_REG_ELR_EL1: u16 = 0xc201;
const HV_SYS_REG_SP_EL0: u16 = 0xc208;
const HV_SYS_REG_ESR_EL1: u16 = 0xc290;
const HV_SYS_REG_FAR_EL1: u16 = 0xc300;
const HV_SYS_REG_MAIR_EL1: u16 = 0xc510;
const HV_SYS_REG_VBAR_EL1: u16 = 0xc600;
const HV_SYS_REG_TPIDR_EL1: u16 = 0xc684;
const HV_SYS_REG_CNTKCTL_EL1: u16 = 0xc708;
const HV_SYS_REG_TPIDR_EL0: u16 = 0xde82;
const HV_SYS_REG_CNTV_CTL_EL0: u16 = 0xdf19;
const HV_SYS_REG_CNTV_CVAL_EL0: u16 = 0xdf1a;
const HV_SYS_REG_SP_EL1: u16 = 0xe208;

const ESR_EC_WFX: u64 = 0x01;
const ESR_EC_HVC64: u64 = 0x16;
const ESR_EC_SYSREG: u64 = 0x18;
const ESR_EC_DATA_ABORT_LOWER: u64 = 0x24;
const ESR_EC_DATA_ABORT_SAME: u64 = 0x25;

const PSTATE_MODE_MASK: u64 = 0x1f;
const PSTATE_MODE_EL0T: u64 = 0;
const PSTATE_MODE_EL1T: u64 = 4;
const PSTATE_MODE_EL1H: u64 = 5;
const PSTATE_DAIF: u64 = 0x3c0;
const PSTATE_SSBS: u64 = 1 << 12;
const PSTATE_PAN: u64 = 1 << 22;
const PSTATE_DIT: u64 = 1 << 24;
const PSTATE_NZCV: u64 = 0xf << 28;
const SCTLR_SPAN: u64 = 1 << 23;
const SCTLR_DSSBS: u64 = 1 << 44;
const ESR_IL: u64 = 1 << 25;

const ICC_IAR1_EL1_CANONICAL: u32 = 0x0030_3018;

// PSCI 0.2+ function IDs used by a uniprocessor Linux boot.
const PSCI_VERSION: u64 = 0x8400_0000;
const PSCI_CPU_SUSPEND32: u64 = 0x8400_0001;
const PSCI_CPU_SUSPEND64: u64 = 0xc400_0001;
const PSCI_CPU_OFF: u64 = 0x8400_0002;
const PSCI_CPU_ON32: u64 = 0x8400_0003;
const PSCI_CPU_ON64: u64 = 0xc400_0003;
const PSCI_AFFINITY_INFO32: u64 = 0x8400_0004;
const PSCI_AFFINITY_INFO64: u64 = 0xc400_0004;
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
const PSCI_FEATURES: u64 = 0x8400_000a;
/// The portable virtual-firmware contract advertised by the guest DTB.
///
/// Reporting a newer host-dependent version is observable: PSCI 1.1 adds
/// `SYSTEM_RESET2`, and Linux records that capability in guest RAM during
/// boot. Keep this pinned to 1.0 on both HVF and KVM.
const PSCI_VERSION_1_0: u64 = 0x0001_0000;
const SMCCC_VERSION: u64 = 0x8000_0000;
const SMCCC_VERSION_1_1: u64 = 0x0001_0001;
const SMCCC_ARCH_FEATURES: u64 = 0x8000_0001;
const SMCCC_ARCH_WORKAROUND_1: u64 = 0x8000_8000;
const SMCCC_ARCH_WORKAROUND_2: u64 = 0x8000_7fff;
const SMCCC_ARCH_WORKAROUND_3: u64 = 0x8000_3fff;
const SMCCC_TRNG_VERSION: u64 = 0x8400_0050;
const SMCCC_VENDOR_HYP_CALL_UID: u64 = 0x8600_ff01;
const PSCI_NOT_SUPPORTED: u64 = (-1i64) as u64;
const PSCI_ALREADY_ON: u64 = (-4i64) as u64;
const PSCI_NOT_PRESENT: u64 = (-7i64) as u64;

/// Identity rows visible to guest `MRS` instructions but absent from the
/// Hypervisor.framework get/set enum on this validated host. The M5 probe
/// measured these exact native values. A policy may acknowledge only those
/// values; any attempted drift fails closed instead of being silently skipped.
const fn hvf_implicit_identity_value(encoding: u32) -> Option<u64> {
    match encoding {
        0xc022 | 0xc027 | 0xc02a | 0xc032 | 0xc033 | 0xc03b | 0xc03c => Some(0),
        0xd801 => Some(0x0000_0000_8444_c004),
        _ => None,
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HvExitException {
    syndrome: u64,
    virtual_address: u64,
    physical_address: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct HvVcpuExit {
    reason: u32,
    _padding: u32,
    exception: HvExitException,
}

// Stable Rust rejects a platform SIMD value in a foreign signature. This
// AAPCS64 thunk accepts a byte pointer in X2, loads Q0, and tail-calls the
// framework's by-value setter.
core::arch::global_asm!(
    ".globl _harmony_backend_hv_vcpu_set_simd_fp_reg",
    "_harmony_backend_hv_vcpu_set_simd_fp_reg:",
    "ldr q0, [x2]",
    "b _hv_vcpu_set_simd_fp_reg",
);

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    fn hv_vm_create(config: *mut c_void) -> i32;
    fn hv_vm_destroy() -> i32;
    fn hv_vm_map(addr: *mut c_void, ipa: u64, size: usize, flags: u64) -> i32;
    fn hv_vm_unmap(ipa: u64, size: usize) -> i32;

    fn hv_vcpu_create(vcpu: *mut u64, exit: *mut *const HvVcpuExit, config: *mut c_void) -> i32;
    fn hv_vcpu_destroy(vcpu: u64) -> i32;
    fn hv_vcpu_run(vcpu: u64) -> i32;
    fn hv_vcpus_exit(vcpus: *const u64, vcpu_count: u32) -> i32;
    fn hv_vcpu_get_reg(vcpu: u64, reg: u32, value: *mut u64) -> i32;
    fn hv_vcpu_set_reg(vcpu: u64, reg: u32, value: u64) -> i32;
    fn hv_vcpu_get_simd_fp_reg(vcpu: u64, reg: u32, value: *mut [u8; 16]) -> i32;
    fn harmony_backend_hv_vcpu_set_simd_fp_reg(vcpu: u64, reg: u32, value: *const u8) -> i32;
    fn hv_vcpu_get_sys_reg(vcpu: u64, reg: u16, value: *mut u64) -> i32;
    fn hv_vcpu_set_sys_reg(vcpu: u64, reg: u16, value: u64) -> i32;
    fn hv_vcpu_get_pending_interrupt(vcpu: u64, kind: u32, pending: *mut bool) -> i32;
    fn hv_vcpu_set_pending_interrupt(vcpu: u64, kind: u32, pending: bool) -> i32;
    fn hv_vcpu_get_trap_debug_exceptions(vcpu: u64, value: *mut bool) -> i32;
    fn hv_vcpu_set_trap_debug_exceptions(vcpu: u64, value: bool) -> i32;
    fn hv_vcpu_get_trap_debug_reg_accesses(vcpu: u64, value: *mut bool) -> i32;
    fn hv_vcpu_set_trap_debug_reg_accesses(vcpu: u64, value: bool) -> i32;
    fn hv_vcpu_get_vtimer_mask(vcpu: u64, value: *mut bool) -> i32;
    fn hv_vcpu_set_vtimer_mask(vcpu: u64, value: bool) -> i32;
    fn hv_vcpu_get_vtimer_offset(vcpu: u64, value: *mut u64) -> i32;
    fn hv_vcpu_set_vtimer_offset(vcpu: u64, value: u64) -> i32;
}

fn hv(operation: &'static str, status: i32) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(BackendError::Io(std::io::Error::other(format!(
            "{operation}: Hypervisor.framework error {:#010x}",
            status as u32
        ))))
    }
}

fn exit_ec(syndrome: u64) -> u64 {
    syndrome >> 26
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pending {
    None,
    MmioLoad {
        reg: u32,
        size: u8,
        sign_extend: bool,
        sf: bool,
    },
    SysregRead {
        reg: u32,
    },
    SysregWrite,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct UndefinedException {
    pc: u64,
    pstate: u64,
    elr_el1: u64,
    spsr_el1: u64,
    esr_el1: u64,
}

/// Model the AArch64 `TakeException` state transition for an undefined
/// instruction delivered to EL1. HVF reports a sysreg trap after this backend
/// has advanced PC, so ELR must point back at the faulting instruction.
fn undefined_exception(
    next_pc: u64,
    old_pstate: u64,
    sctlr_el1: u64,
    vbar_el1: u64,
) -> Result<UndefinedException> {
    let fault_pc = next_pc.checked_sub(4).ok_or(BackendError::InvalidState)?;
    let vector_offset = match old_pstate & PSTATE_MODE_MASK {
        PSTATE_MODE_EL0T => 0x400,
        PSTATE_MODE_EL1T => 0,
        PSTATE_MODE_EL1H => 0x200,
        _ => return Err(BackendError::InvalidState),
    };
    let pc = vbar_el1
        .checked_add(vector_offset)
        .ok_or(BackendError::InvalidState)?;

    let mut pstate = old_pstate & (PSTATE_NZCV | PSTATE_DIT | PSTATE_PAN);
    if sctlr_el1 & SCTLR_SPAN == 0 {
        pstate |= PSTATE_PAN;
    }
    if sctlr_el1 & SCTLR_DSSBS != 0 {
        pstate |= PSTATE_SSBS;
    }
    pstate |= PSTATE_DAIF | PSTATE_MODE_EL1H;

    Ok(UndefinedException {
        pc,
        pstate,
        elr_el1: fault_pc,
        spsr_el1: old_pstate,
        // EC=UNKNOWN is zero. AArch64 instructions are 32 bits, so IL=1.
        esr_el1: ESR_IL,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DataAbort {
    gpa: Gpa,
    size: u8,
    write: bool,
    reg: u32,
    sign_extend: bool,
    sf: bool,
}

fn decode_data_abort(exit: HvVcpuExit) -> Result<DataAbort> {
    let iss = exit.exception.syndrome & 0x01ff_ffff;
    if iss & (1 << 24) == 0 {
        return Err(BackendError::Internal(
            "HVF data abort without a valid instruction syndrome",
        ));
    }
    let sas = ((iss >> 22) & 0x3) as u8;
    let size = 1u8 << sas;
    let reg = ((iss >> 16) & 0x1f) as u32;
    let sign_extend = iss & (1 << 21) != 0;
    let sf = iss & (1 << 15) != 0;
    let write = iss & (1 << 6) != 0;
    Ok(DataAbort {
        gpa: Gpa(exit.exception.physical_address),
        size,
        write,
        reg,
        sign_extend,
        sf,
    })
}

fn canonical_sysreg(iss: u64) -> u32 {
    // Keep the architectural op fields, clear Rt[9:5] and direction[0].
    ((iss & 0x003f_ffff) & !(0x1f << 5) & !1) as u32
}

fn sysreg_rt(iss: u64) -> u32 {
    ((iss >> 5) & 0x1f) as u32
}

fn accepted_irq_for_sysreg(
    pending_irq: Option<GicIntId>,
    sysreg: u32,
    read: bool,
) -> Option<GicIntId> {
    (read && sysreg == ICC_IAR1_EL1_CANONICAL)
        .then_some(pending_irq)
        .flatten()
}

/// Cross-thread handle for the liveness monitor's non-guest-visible abort.
/// Requesting an exit only makes `hv_vcpu_run` return canceled; it never
/// injects state into the guest.
#[derive(Clone, Copy, Debug)]
pub struct HvfExitHandle {
    vcpu: u64,
}

impl HvfExitHandle {
    /// Ask Hypervisor.framework to return from the current vCPU entry.
    /// Calling this while the vCPU is not running is harmless.
    pub fn request_exit(self) -> Result<()> {
        let vcpus = [self.vcpu];
        // SAFETY: the array is live for the call. HVF treats vCPU identifiers
        // as values and returns an error for an identifier no longer present.
        hv("hv_vcpus_exit", unsafe {
            hv_vcpus_exit(vcpus.as_ptr(), vcpus.len() as u32)
        })
    }
}

/// Hypervisor.framework backend for the measured Apple Silicon bring-up host.
pub struct HvfBackend {
    vcpu: u64,
    exit: NonNull<HvVcpuExit>,
    configured: bool,
    pending: Pending,
    pending_irq: Option<GicIntId>,
    accepted_irq: Option<GicIntId>,
    counts: ExitCounts,
    regions: Vec<(u64, usize)>,
}

impl HvfBackend {
    /// Create the process-global HVF VM and its single vCPU.
    pub fn new() -> Result<Self> {
        // SAFETY: null selects the documented default VM configuration.
        hv("hv_vm_create", unsafe { hv_vm_create(ptr::null_mut()) })?;
        let mut vcpu = 0;
        let mut exit = ptr::null();
        // SAFETY: output pointers are live and null selects the default config.
        if let Err(error) = hv("hv_vcpu_create", unsafe {
            hv_vcpu_create(&mut vcpu, &mut exit, ptr::null_mut())
        }) {
            // SAFETY: this constructor created the VM and no vCPU exists.
            let _ = unsafe { hv_vm_destroy() };
            return Err(error);
        }
        let Some(exit) = NonNull::new(exit.cast_mut()) else {
            // SAFETY: creation returned a live vCPU and this is its owner.
            let _ = unsafe { hv_vcpu_destroy(vcpu) };
            // SAFETY: the only vCPU is gone.
            let _ = unsafe { hv_vm_destroy() };
            return Err(BackendError::Internal(
                "hv_vcpu_create returned a null exit page",
            ));
        };
        // The framework timer is tied to mach_absolute_time. Keep its automatic
        // activation exit disabled; the userspace GIC timer is V-time-derived.
        // SAFETY: `vcpu` is live and owned by this thread.
        if let Err(error) = hv("hv_vcpu_set_vtimer_mask", unsafe {
            hv_vcpu_set_vtimer_mask(vcpu, true)
        }) {
            // SAFETY: constructor cleanup on the owning thread.
            let _ = unsafe { hv_vcpu_destroy(vcpu) };
            // SAFETY: the only vCPU is gone.
            let _ = unsafe { hv_vm_destroy() };
            return Err(error);
        }
        // KVM has no portable counterpart to HVF's host-counter offset. Fix
        // the private substrate state at zero before first entry.
        // SAFETY: `vcpu` is live and owned by this thread.
        if let Err(error) = hv("hv_vcpu_set_vtimer_offset", unsafe {
            hv_vcpu_set_vtimer_offset(vcpu, 0)
        }) {
            // SAFETY: constructor cleanup on the owning thread.
            let _ = unsafe { hv_vcpu_destroy(vcpu) };
            // SAFETY: the only vCPU is gone.
            let _ = unsafe { hv_vm_destroy() };
            return Err(error);
        }
        Ok(Self {
            vcpu,
            exit,
            configured: false,
            pending: Pending::None,
            pending_irq: None,
            accepted_irq: None,
            counts: ExitCounts::default(),
            regions: Vec::new(),
        })
    }

    /// Obtain the token used by a host-only liveness monitor to abort a stuck
    /// guest entry without perturbing guest state.
    pub fn exit_handle(&self) -> HvfExitHandle {
        HvfExitHandle { vcpu: self.vcpu }
    }

    fn reg(&self, reg: u32) -> Result<u64> {
        let mut value = 0;
        // SAFETY: output points to a live u64 and this is the vCPU owner.
        hv("hv_vcpu_get_reg", unsafe {
            hv_vcpu_get_reg(self.vcpu, reg, &mut value)
        })?;
        Ok(value)
    }

    fn set_reg(&self, reg: u32, value: u64) -> Result<()> {
        // SAFETY: the vCPU is live and this is its owning thread.
        hv("hv_vcpu_set_reg", unsafe {
            hv_vcpu_set_reg(self.vcpu, reg, value)
        })
    }

    fn sysreg(&self, reg: u16) -> Result<u64> {
        let mut value = 0;
        // SAFETY: output points to a live u64 and this is the vCPU owner.
        hv("hv_vcpu_get_sys_reg", unsafe {
            hv_vcpu_get_sys_reg(self.vcpu, reg, &mut value)
        })?;
        Ok(value)
    }

    fn set_sysreg(&self, reg: u16, value: u64) -> Result<()> {
        // SAFETY: the vCPU is live and this is its owning thread.
        hv("hv_vcpu_set_sys_reg", unsafe {
            hv_vcpu_set_sys_reg(self.vcpu, reg, value)
        })
    }

    fn advance_pc(&self) -> Result<()> {
        let next = self
            .reg(HV_REG_PC)?
            .checked_add(4)
            .ok_or(BackendError::InvalidState)?;
        self.set_reg(HV_REG_PC, next)
    }

    fn ensure_runnable(&self) -> Result<()> {
        if !self.configured {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    fn apply_pending_irq(&mut self) -> Result<()> {
        let inject = self.pending_irq.is_some();
        // SAFETY: this is the owning thread. Keep the level asserted across
        // unrelated exits and while PSTATE.I is masked. HVF holds the level
        // until the guest unmasks IRQs, matching an in-kernel vGIC; acceptance
        // is observable only when the guest reads ICC_IAR1_EL1.
        hv("hv_vcpu_set_pending_interrupt", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_IRQ, inject)
        })
    }

    fn handle_psci(&self, function: u64) -> Result<Option<Exit<Arm64>>> {
        let result = match function {
            PSCI_VERSION => PSCI_VERSION_1_0,
            SMCCC_VERSION => SMCCC_VERSION_1_1,
            SMCCC_ARCH_FEATURES => match self.reg(1)? {
                SMCCC_ARCH_WORKAROUND_1 => 1,              // unaffected
                SMCCC_ARCH_WORKAROUND_2 => (-2i64) as u64, // not required
                SMCCC_ARCH_WORKAROUND_3 => 0,              // available
                _ => PSCI_NOT_SUPPORTED,
            },
            SMCCC_TRNG_VERSION => PSCI_NOT_SUPPORTED,
            SMCCC_VENDOR_HYP_CALL_UID => PSCI_NOT_SUPPORTED,
            PSCI_MIGRATE_INFO_TYPE => 2,
            PSCI_FEATURES => {
                let queried = self.reg(1)?;
                if matches!(
                    queried,
                    SMCCC_VERSION
                        | PSCI_VERSION
                        | PSCI_SYSTEM_OFF
                        | PSCI_SYSTEM_RESET
                        | PSCI_FEATURES
                ) {
                    0
                } else {
                    PSCI_NOT_SUPPORTED
                }
            }
            PSCI_CPU_ON32 | PSCI_CPU_ON64 => PSCI_ALREADY_ON,
            PSCI_AFFINITY_INFO32 | PSCI_AFFINITY_INFO64 => PSCI_NOT_PRESENT,
            PSCI_CPU_SUSPEND32 | PSCI_CPU_SUSPEND64 | PSCI_CPU_OFF => PSCI_NOT_SUPPORTED,
            PSCI_SYSTEM_OFF | PSCI_SYSTEM_RESET => return Ok(Some(CommonExit::Shutdown.into())),
            _ => return Ok(None),
        };
        self.set_reg(0, result)?;
        Ok(None)
    }

    fn enter_guest(&mut self) -> Result<Exit<Arm64>> {
        loop {
            self.apply_pending_irq()?;
            // SAFETY: the vCPU is live and this call runs on its owning thread.
            hv("hv_vcpu_run", unsafe { hv_vcpu_run(self.vcpu) })?;
            // SAFETY: HVF owns this exit page for the vCPU lifetime and run has
            // completed its write before returning.
            let raw = unsafe { *self.exit.as_ptr() };
            match raw.reason {
                HV_EXIT_REASON_CANCELED => {
                    return Err(BackendError::Internal("HVF vCPU run canceled"));
                }
                HV_EXIT_REASON_VTIMER_ACTIVATED => {
                    return Err(BackendError::Internal(
                        "host-time HVF virtual timer activated despite permanent mask",
                    ));
                }
                HV_EXIT_REASON_EXCEPTION => {}
                _ => return Err(BackendError::Internal("unhandled HVF exit reason")),
            }
            let exit = match exit_ec(raw.exception.syndrome) {
                ESR_EC_WFX => {
                    self.advance_pc()?;
                    CommonExit::Idle.into()
                }
                ESR_EC_HVC64 => {
                    let function = self.reg(0)?;
                    if let Some(exit) = self.handle_psci(function)? {
                        exit
                    } else if matches!(
                        function,
                        SMCCC_VERSION
                            | SMCCC_ARCH_FEATURES
                            | SMCCC_TRNG_VERSION
                            | SMCCC_VENDOR_HYP_CALL_UID
                            | PSCI_VERSION
                            | PSCI_CPU_SUSPEND32
                            | PSCI_CPU_SUSPEND64
                            | PSCI_CPU_OFF
                            | PSCI_CPU_ON32
                            | PSCI_CPU_ON64
                            | PSCI_AFFINITY_INFO32
                            | PSCI_AFFINITY_INFO64
                            | PSCI_MIGRATE_INFO_TYPE
                            | PSCI_FEATURES
                    ) {
                        continue;
                    } else {
                        CommonExit::Hypercall(HypercallFrame {
                            args: [function, self.reg(1)?, self.reg(2)?, self.reg(3)?],
                        })
                        .into()
                    }
                }
                ESR_EC_SYSREG => {
                    let iss = raw.exception.syndrome & 0x01ff_ffff;
                    let reg = sysreg_rt(iss);
                    let read = iss & 1 != 0;
                    let sysreg = canonical_sysreg(iss);
                    if let Some(accepted) = accepted_irq_for_sysreg(self.pending_irq, sysreg, read)
                    {
                        self.accepted_irq = Some(accepted);
                    }
                    // Rt=31 is XZR for MSR/MRS, not an index into HVF's
                    // register enum (where numeric 31 names PC). Linux uses
                    // `msr ICC_BPR1_EL1, xzr` during GIC CPU-interface init.
                    let write = if read {
                        None
                    } else if reg == 31 {
                        Some(0)
                    } else {
                        Some(self.reg(reg)?)
                    };
                    self.advance_pc()?;
                    self.pending = if read {
                        Pending::SysregRead { reg }
                    } else {
                        Pending::SysregWrite
                    };
                    Exit::Arch(Arm64Exit::Sysreg { sysreg, write })
                }
                ESR_EC_DATA_ABORT_LOWER | ESR_EC_DATA_ABORT_SAME => {
                    let abort = decode_data_abort(raw)?;
                    self.advance_pc()?;
                    if abort.write {
                        let value = if abort.reg == 31 {
                            0
                        } else {
                            self.reg(abort.reg)?
                        };
                        let mask = if abort.size == 8 {
                            u64::MAX
                        } else {
                            (1u64 << (u32::from(abort.size) * 8)) - 1
                        };
                        CommonExit::Mmio {
                            gpa: abort.gpa,
                            size: abort.size,
                            write: Some(value & mask),
                        }
                        .into()
                    } else {
                        self.pending = Pending::MmioLoad {
                            reg: abort.reg,
                            size: abort.size,
                            sign_extend: abort.sign_extend,
                            sf: abort.sf,
                        };
                        CommonExit::Mmio {
                            gpa: abort.gpa,
                            size: abort.size,
                            write: None,
                        }
                        .into()
                    }
                }
                _ => return Err(BackendError::Internal("unhandled HVF exception class")),
            };
            self.counts.bump(exit.reason());
            return Ok(exit);
        }
    }
}

impl Drop for HvfBackend {
    fn drop(&mut self) {
        for &(gpa, len) in self.regions.iter().rev() {
            // SAFETY: every entry records a successful map owned by this VM.
            let _ = unsafe { hv_vm_unmap(gpa, len) };
        }
        // SAFETY: this backend owns the vCPU and drops on its owning thread.
        let _ = unsafe { hv_vcpu_destroy(self.vcpu) };
        // SAFETY: the sole vCPU has been destroyed.
        let _ = unsafe { hv_vm_destroy() };
    }
}

impl Backend for HvfBackend {
    type A = Arm64;

    fn set_policy(&mut self, policy: &Arm64Policy) -> Result<()> {
        for (&encoding, &value) in &policy.id_regs.regs {
            if let Some(measured) = hvf_implicit_identity_value(encoding) {
                if value != measured {
                    return Err(BackendError::InvalidState);
                }
                continue;
            }
            let reg = u16::try_from(encoding).map_err(|_| BackendError::InvalidState)?;
            self.set_sysreg(reg, value)?;
        }
        let _ = &policy.sysreg_traps;
        self.configured = true;
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        if host.is_empty() {
            return Err(BackendError::Memory("zero-length memory region"));
        }
        if !gpa.0.is_multiple_of(HV_PAGE_SIZE as u64) {
            return Err(BackendError::Memory("gpa is not 16 KiB-aligned for HVF"));
        }
        if !host.len().is_multiple_of(HV_PAGE_SIZE) {
            return Err(BackendError::Memory(
                "region length is not 16 KiB-aligned for HVF",
            ));
        }
        if !(host.as_ptr() as usize).is_multiple_of(HV_PAGE_SIZE) {
            return Err(BackendError::Memory(
                "host address is not 16 KiB-aligned for HVF",
            ));
        }
        let end = gpa
            .0
            .checked_add(host.len() as u64)
            .ok_or(BackendError::Memory("region wraps address space"))?;
        for &(mapped_gpa, mapped_len) in &self.regions {
            let mapped_end = mapped_gpa + mapped_len as u64;
            if gpa.0 < mapped_end && mapped_gpa < end {
                return Err(BackendError::Memory("region overlaps an existing map"));
            }
        }
        // SAFETY: the caller guarantees pinned, unaliased backing; this method
        // additionally verified the framework's 16 KiB alignment and length.
        hv("hv_vm_map", unsafe {
            hv_vm_map(
                host.as_mut_ptr().cast(),
                gpa.0,
                host.len(),
                HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
            )
        })?;
        self.regions.push((gpa.0, host.len()));
        Ok(())
    }

    fn run(&mut self) -> Result<Exit<Arm64>> {
        self.ensure_runnable()?;
        self.enter_guest()
    }

    fn inject(&mut self, event: Arm64Injection) -> Result<()> {
        match event {
            Arm64Injection::Interrupt { intid } => self.set_pending_irq(Some(intid)),
        }
    }

    fn set_pending_irq(&mut self, id: Option<GicIntId>) -> Result<()> {
        self.pending_irq = id;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        self.accepted_irq.take()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::MmioLoad {
                reg,
                size,
                sign_extend,
                sf,
            } => {
                let bits = u32::from(size) * 8;
                let masked = if bits == 64 {
                    value
                } else {
                    value & ((1u64 << bits) - 1)
                };
                let completed = if sign_extend && bits < 64 {
                    let shift = 64 - bits;
                    ((masked << shift) as i64 >> shift) as u64
                } else if sf {
                    masked
                } else {
                    masked as u32 as u64
                };
                if reg != 31 {
                    self.set_reg(reg, completed)?;
                }
                self.pending = Pending::None;
                Ok(())
            }
            Pending::SysregRead { reg } => {
                if reg != 31 {
                    self.set_reg(reg, value)?;
                }
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::NoPendingRead),
        }
    }

    fn complete_fault(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregRead { .. } | Pending::SysregWrite => {
                let exception = undefined_exception(
                    self.reg(HV_REG_PC)?,
                    self.reg(HV_REG_CPSR)?,
                    self.sysreg(HV_SYS_REG_SCTLR_EL1)?,
                    self.sysreg(HV_SYS_REG_VBAR_EL1)?,
                )?;
                self.set_sysreg(HV_SYS_REG_ELR_EL1, exception.elr_el1)?;
                self.set_sysreg(HV_SYS_REG_SPSR_EL1, exception.spsr_el1)?;
                self.set_sysreg(HV_SYS_REG_ESR_EL1, exception.esr_el1)?;
                self.set_reg(HV_REG_CPSR, exception.pstate)?;
                self.set_reg(HV_REG_PC, exception.pc)?;
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        if self.pending == Pending::SysregWrite {
            self.pending = Pending::None;
            Ok(())
        } else {
            Err(BackendError::BadCompletion)
        }
    }

    fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
        Err(BackendError::NoPendingRead)
    }

    fn complete_arch(&mut self, completion: crate::arch::arm64::Arm64Completion) -> Result<()> {
        match completion {}
    }

    fn save(&self) -> Result<Arm64VcpuState> {
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        // Exclusive-monitor canonicalization (VM-EXIT-COUNT-VTIME M1). HVF has
        // no public monitor get/set/clear API. The cooperative image is scanned
        // at build time by `consonance/harmony-linux/scripts/aa4-exclusive-scan.py`, whose
        // planted LDXR/STXR negative must fail before the real kernel, vDSO, and
        // init are accepted. Therefore no instruction in the admitted image can
        // create a reservation: the monitor starts empty at vCPU creation and
        // is canonically empty at every sealable boundary. It is deliberately
        // absent from `Arm64VcpuState` rather than represented by a fabricated
        // bit that the backend could not enforce.
        let mut state = Arm64VcpuState::default();
        for (reg, slot) in state.core.x.iter_mut().enumerate() {
            *slot = self.reg(reg as u32)?;
        }
        state.core.pc = self.reg(HV_REG_PC)?;
        state.core.pstate = self.reg(HV_REG_CPSR)?;
        state.core.sp = self.sysreg(HV_SYS_REG_SP_EL0)?;
        state.core.sp_el1 = self.sysreg(HV_SYS_REG_SP_EL1)?;
        state.core.elr_el1 = self.sysreg(HV_SYS_REG_ELR_EL1)?;
        state.core.spsr_el1 = self.sysreg(HV_SYS_REG_SPSR_EL1)?;
        canonicalize_core_regs(&mut state.core);
        state.simd_fp.fpcr = self.reg(HV_REG_FPCR)?;
        state.simd_fp.fpsr = self.reg(HV_REG_FPSR)?;
        for (reg, q) in state.simd_fp.q.iter_mut().enumerate() {
            // SAFETY: `q` points to 16 writable bytes and the register index is
            // in the measured Q0..Q31 public range.
            hv("hv_vcpu_get_simd_fp_reg", unsafe {
                hv_vcpu_get_simd_fp_reg(self.vcpu, reg as u32, q)
            })?;
        }
        state.sysregs.sctlr_el1 = self.sysreg(HV_SYS_REG_SCTLR_EL1)?;
        state.sysregs.ttbr0_el1 = self.sysreg(HV_SYS_REG_TTBR0_EL1)?;
        state.sysregs.ttbr1_el1 = self.sysreg(HV_SYS_REG_TTBR1_EL1)?;
        state.sysregs.tcr_el1 = self.sysreg(HV_SYS_REG_TCR_EL1)?;
        state.sysregs.mair_el1 = self.sysreg(HV_SYS_REG_MAIR_EL1)?;
        state.sysregs.vbar_el1 = self.sysreg(HV_SYS_REG_VBAR_EL1)?;
        state.sysregs.cpacr_el1 = self.sysreg(HV_SYS_REG_CPACR_EL1)?;
        state.sysregs.esr_el1 = self.sysreg(HV_SYS_REG_ESR_EL1)?;
        state.sysregs.far_el1 = self.sysreg(HV_SYS_REG_FAR_EL1)?;
        state.sysregs.tpidr_el0 = self.sysreg(HV_SYS_REG_TPIDR_EL0)?;
        state.sysregs.tpidr_el1 = self.sysreg(HV_SYS_REG_TPIDR_EL1)?;
        state.sysregs.cntkctl_el1 = self.sysreg(HV_SYS_REG_CNTKCTL_EL1)?;
        for index in 0..16u16 {
            state.debug.breakpoint_value[index as usize] =
                self.sysreg(HV_SYS_REG_DBGBVR0_EL1 + index * 8)?;
            state.debug.breakpoint_control[index as usize] =
                self.sysreg(HV_SYS_REG_DBGBCR0_EL1 + index * 8)?;
            state.debug.watchpoint_value[index as usize] =
                self.sysreg(HV_SYS_REG_DBGWVR0_EL1 + index * 8)?;
            state.debug.watchpoint_control[index as usize] =
                self.sysreg(HV_SYS_REG_DBGWCR0_EL1 + index * 8)?;
        }
        state.debug.mdscr_el1 = self.sysreg(HV_SYS_REG_MDSCR_EL1)?;
        // SAFETY: outputs are live bools and this is the owning thread.
        hv("hv_vcpu_get_trap_debug_exceptions", unsafe {
            hv_vcpu_get_trap_debug_exceptions(self.vcpu, &mut state.debug.trap_debug_exceptions)
        })?;
        // SAFETY: outputs are live bools and this is the owning thread.
        hv("hv_vcpu_get_trap_debug_reg_accesses", unsafe {
            hv_vcpu_get_trap_debug_reg_accesses(self.vcpu, &mut state.debug.trap_debug_reg_accesses)
        })?;
        // ISTATUS is a read-only, host-counter-derived observation and differs
        // across substrates even while the timer is disabled. Only the two
        // writable control bits belong in a portable snapshot.
        state.vtimer.cntv_ctl_el0 = self.sysreg(HV_SYS_REG_CNTV_CTL_EL0)? & 0b11;
        state.vtimer.cntv_cval_el0 = self.sysreg(HV_SYS_REG_CNTV_CVAL_EL0)?;
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_vtimer_mask", unsafe {
            hv_vcpu_get_vtimer_mask(self.vcpu, &mut state.vtimer.masked)
        })?;
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_vtimer_offset", unsafe {
            hv_vcpu_get_vtimer_offset(self.vcpu, &mut state.vtimer.offset)
        })?;
        if !state.vtimer.masked || state.vtimer.offset != 0 {
            return Err(BackendError::InvalidState);
        }
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_pending_interrupt(IRQ)", unsafe {
            hv_vcpu_get_pending_interrupt(
                self.vcpu,
                HV_INTERRUPT_TYPE_IRQ,
                &mut state.interrupts.irq,
            )
        })?;
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_pending_interrupt(FIQ)", unsafe {
            hv_vcpu_get_pending_interrupt(
                self.vcpu,
                HV_INTERRUPT_TYPE_FIQ,
                &mut state.interrupts.fiq,
            )
        })?;
        state.mp_state = MpState::Runnable;
        Ok(state)
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
        if has_noncanonical_core_regs(&state.core)
            || state.mp_state != MpState::Runnable
            || self.pending != Pending::None
            || !state.vtimer.masked
            || state.vtimer.offset != 0
            || state.vtimer.cntv_ctl_el0 & !0b11 != 0
        {
            return Err(BackendError::InvalidState);
        }
        // The exclusive monitor remains the canonical empty value described in
        // `save`: restore is admitted only for the LL/SC-free cooperative image,
        // and that image cannot have changed the reset-empty monitor before or
        // after this boundary.
        for (reg, value) in state.core.x.iter().copied().enumerate() {
            self.set_reg(reg as u32, value)?;
        }
        self.set_reg(HV_REG_PC, state.core.pc)?;
        self.set_reg(HV_REG_CPSR, state.core.pstate)?;
        self.set_sysreg(HV_SYS_REG_SP_EL0, state.core.sp)?;
        self.set_sysreg(HV_SYS_REG_SP_EL1, state.core.sp_el1)?;
        self.set_sysreg(HV_SYS_REG_ELR_EL1, state.core.elr_el1)?;
        self.set_sysreg(HV_SYS_REG_SPSR_EL1, state.core.spsr_el1)?;
        self.set_reg(HV_REG_FPCR, state.simd_fp.fpcr)?;
        self.set_reg(HV_REG_FPSR, state.simd_fp.fpsr)?;
        for (reg, q) in state.simd_fp.q.iter().enumerate() {
            // SAFETY: `q` has 16 readable bytes and the thunk passes it in Q0
            // according to AAPCS64.
            hv("hv_vcpu_set_simd_fp_reg", unsafe {
                harmony_backend_hv_vcpu_set_simd_fp_reg(self.vcpu, reg as u32, q.as_ptr())
            })?;
        }
        self.set_sysreg(HV_SYS_REG_SCTLR_EL1, state.sysregs.sctlr_el1)?;
        self.set_sysreg(HV_SYS_REG_TTBR0_EL1, state.sysregs.ttbr0_el1)?;
        self.set_sysreg(HV_SYS_REG_TTBR1_EL1, state.sysregs.ttbr1_el1)?;
        self.set_sysreg(HV_SYS_REG_TCR_EL1, state.sysregs.tcr_el1)?;
        self.set_sysreg(HV_SYS_REG_MAIR_EL1, state.sysregs.mair_el1)?;
        self.set_sysreg(HV_SYS_REG_VBAR_EL1, state.sysregs.vbar_el1)?;
        self.set_sysreg(HV_SYS_REG_CPACR_EL1, state.sysregs.cpacr_el1)?;
        self.set_sysreg(HV_SYS_REG_ESR_EL1, state.sysregs.esr_el1)?;
        self.set_sysreg(HV_SYS_REG_FAR_EL1, state.sysregs.far_el1)?;
        self.set_sysreg(HV_SYS_REG_TPIDR_EL0, state.sysregs.tpidr_el0)?;
        self.set_sysreg(HV_SYS_REG_TPIDR_EL1, state.sysregs.tpidr_el1)?;
        self.set_sysreg(HV_SYS_REG_CNTKCTL_EL1, state.sysregs.cntkctl_el1)?;
        for index in 0..16u16 {
            self.set_sysreg(
                HV_SYS_REG_DBGBVR0_EL1 + index * 8,
                state.debug.breakpoint_value[index as usize],
            )?;
            self.set_sysreg(
                HV_SYS_REG_DBGBCR0_EL1 + index * 8,
                state.debug.breakpoint_control[index as usize],
            )?;
            self.set_sysreg(
                HV_SYS_REG_DBGWVR0_EL1 + index * 8,
                state.debug.watchpoint_value[index as usize],
            )?;
            self.set_sysreg(
                HV_SYS_REG_DBGWCR0_EL1 + index * 8,
                state.debug.watchpoint_control[index as usize],
            )?;
        }
        self.set_sysreg(HV_SYS_REG_MDSCR_EL1, state.debug.mdscr_el1)?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_trap_debug_exceptions", unsafe {
            hv_vcpu_set_trap_debug_exceptions(self.vcpu, state.debug.trap_debug_exceptions)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_trap_debug_reg_accesses", unsafe {
            hv_vcpu_set_trap_debug_reg_accesses(self.vcpu, state.debug.trap_debug_reg_accesses)
        })?;
        self.set_sysreg(HV_SYS_REG_CNTV_CVAL_EL0, state.vtimer.cntv_cval_el0)?;
        // Arm only after restoring CVAL; CTL-first can transiently assert the
        // host virtual-timer output against the old compare value.
        self.set_sysreg(HV_SYS_REG_CNTV_CTL_EL0, state.vtimer.cntv_ctl_el0)?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_vtimer_mask", unsafe {
            hv_vcpu_set_vtimer_mask(self.vcpu, true)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_vtimer_offset", unsafe {
            hv_vcpu_set_vtimer_offset(self.vcpu, 0)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_pending_interrupt(IRQ)", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_IRQ, state.interrupts.irq)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_pending_interrupt(FIQ)", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_FIQ, state.interrupts.fiq)
        })?;
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn capabilities(&self) -> Capabilities<Arm64Caps> {
        Capabilities {
            name: "hvf-arm64-virtual_time",
            deterministic_rng: false,
            arch: Arm64Caps {
                in_kernel_gic: false,
                deterministic_cntvct: false,
                enforces_cntv_cval: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw_exit(syndrome: u64, ipa: u64) -> HvVcpuExit {
        HvVcpuExit {
            reason: HV_EXIT_REASON_EXCEPTION,
            _padding: 0,
            exception: HvExitException {
                syndrome,
                virtual_address: ipa,
                physical_address: ipa,
            },
        }
    }

    #[test]
    fn implicit_identity_allowlist_is_exact_and_value_sensitive() {
        for encoding in [0xc022, 0xc027, 0xc02a, 0xc032, 0xc033, 0xc03b, 0xc03c] {
            assert_eq!(hvf_implicit_identity_value(encoding), Some(0));
        }
        assert_eq!(
            hvf_implicit_identity_value(0xd801),
            Some(0x0000_0000_8444_c004)
        );
        assert_eq!(hvf_implicit_identity_value(0xc020), None);
        assert_ne!(hvf_implicit_identity_value(0xc032), Some(2));
    }

    #[test]
    fn psci_version_matches_the_portable_firmware_contract() {
        assert_eq!(PSCI_VERSION_1_0, 0x0001_0000);
        assert_ne!(PSCI_VERSION_1_0, 0x0001_0001);
    }

    #[test]
    fn probe_data_abort_decodes_exact_mmio_shape() {
        let decoded = decode_data_abort(raw_exit(0x93c4_8007, 0x20000)).unwrap();
        assert_eq!(
            decoded,
            DataAbort {
                gpa: Gpa(0x20000),
                size: 8,
                write: false,
                reg: 4,
                sign_extend: false,
                sf: true,
            }
        );
    }

    #[test]
    fn measured_gic_sysreg_syndromes_canonicalize_independent_of_rt_and_direction() {
        let iar = 0x6230_3019u64 & 0x01ff_ffff;
        let eoir = 0x6232_3038u64 & 0x01ff_ffff;
        let pmr_write = 0x6230_104cu64 & 0x01ff_ffff;
        let pmr_read = 0x6230_106du64 & 0x01ff_ffff;
        assert_eq!(canonical_sysreg(iar), 0x0030_3018);
        assert_eq!(canonical_sysreg(eoir), 0x0032_3018);
        assert_eq!(canonical_sysreg(pmr_write), 0x0030_100c);
        assert_eq!(canonical_sysreg(pmr_read), canonical_sysreg(pmr_write));
        assert_eq!(sysreg_rt(eoir), 1);
        let bpr1_xzr = 0x0036_3018u64 | (31 << 5);
        assert_eq!(sysreg_rt(bpr1_xzr), 31);
        assert_eq!(canonical_sysreg(bpr1_xzr), 0x0036_3018);
    }

    #[test]
    fn pending_irq_is_accepted_only_at_the_guest_iar_read() {
        let pending = Some(GicIntId(20));
        assert_eq!(
            accepted_irq_for_sysreg(pending, ICC_IAR1_EL1_CANONICAL, true),
            pending
        );
        assert_eq!(
            accepted_irq_for_sysreg(pending, ICC_IAR1_EL1_CANONICAL, false),
            None,
            "a write-shaped trap cannot accept an IRQ"
        );
        assert_eq!(
            accepted_irq_for_sysreg(pending, 0x0030_100c, true),
            None,
            "an unrelated trapped read cannot accept an IRQ"
        );
        assert_eq!(
            accepted_irq_for_sysreg(None, ICC_IAR1_EL1_CANONICAL, true),
            None,
            "an IAR read cannot fabricate an IRQ"
        );
    }

    #[test]
    fn untrusted_data_abort_without_isv_fails_closed() {
        assert!(decode_data_abort(raw_exit(0x9000_0007, 0)).is_err());
    }

    #[test]
    fn undef_from_el0_enters_the_lower_aarch64_sync_vector() {
        let old = PSTATE_NZCV | PSTATE_DIT | PSTATE_SSBS | PSTATE_MODE_EL0T;
        let exception = undefined_exception(0x1004, old, SCTLR_DSSBS, 0x8000).unwrap();
        assert_eq!(
            exception,
            UndefinedException {
                pc: 0x8400,
                pstate: PSTATE_NZCV
                    | PSTATE_DIT
                    | PSTATE_PAN
                    | PSTATE_SSBS
                    | PSTATE_DAIF
                    | PSTATE_MODE_EL1H,
                elr_el1: 0x1000,
                spsr_el1: old,
                esr_el1: ESR_IL,
            }
        );
    }

    #[test]
    fn undef_vector_and_pstate_are_mode_and_sctlr_sensitive() {
        let el1t = undefined_exception(0x2004, PSTATE_MODE_EL1T, SCTLR_SPAN, 0x8000).unwrap();
        assert_eq!(el1t.pc, 0x8000);
        assert_eq!(el1t.pstate & PSTATE_PAN, 0);

        let el1h = undefined_exception(0x2004, PSTATE_MODE_EL1H, 0, 0x8000).unwrap();
        assert_eq!(el1h.pc, 0x8200);
        assert_ne!(el1h.pstate & PSTATE_PAN, 0);
        assert_eq!(el1h.pstate & PSTATE_SSBS, 0);

        assert!(undefined_exception(3, PSTATE_MODE_EL0T, 0, 0).is_err());
        assert!(undefined_exception(4, 0x10, 0, 0).is_err());
        assert!(undefined_exception(4, PSTATE_MODE_EL0T, 0, u64::MAX).is_err());
    }
}
