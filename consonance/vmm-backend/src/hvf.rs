// SPDX-License-Identifier: AGPL-3.0-or-later

use core::ffi::c_void;
use std::ptr::{self, NonNull};

use sha2::{Digest, Sha256};

use crate::arch::arm64::{
    Arm64, Arm64AsidBits, Arm64Caps, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState,
    GicIntId, canonicalize_core_regs, has_noncanonical_core_regs, has_unsupported_execution_mode,
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
const HV_SYS_REG_ACTLR_EL1: u16 = 0xc081;
const HV_FIXED_ZERO_SYSREGS: [u16; 6] =
    [HV_SYS_REG_ACTLR_EL1, 0xc687, 0xde87, 0xde85, 0xc094, 0xc096];
const HV_UNSUPPORTED: i32 = 0xfae9_400f_u32 as i32;
const HV_PAUTH_KEY_SYSREGS: [u16; 10] = [
    0xc108, 0xc109, 0xc10a, 0xc10b, 0xc110, 0xc111, 0xc112, 0xc113, 0xc118, 0xc119,
];
const HV_SYS_REG_CONTEXTIDR_EL1: u16 = 0xc681;
const HV_SYS_REG_AMAIR_EL1: u16 = 0xc518;
const HV_SYS_REG_PAR_EL1: u16 = 0xc3a0;
const HV_SYS_REG_CSSELR_EL1: u16 = 0xd000;
const HV_SYS_REG_AFSR0_EL1: u16 = 0xc288;
const HV_SYS_REG_AFSR1_EL1: u16 = 0xc289;
const HV_SYS_REG_DBGBVR0_EL1: u16 = 0x8004;
const HV_SYS_REG_DBGBCR0_EL1: u16 = 0x8005;
const HV_SYS_REG_DBGWVR0_EL1: u16 = 0x8006;
const HV_SYS_REG_DBGWCR0_EL1: u16 = 0x8007;
const HV_SYS_REG_MDCCINT_EL1: u16 = 0x8010;
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
const HV_SYS_REG_TPIDRRO_EL0: u16 = 0xde83;
const HV_SYS_REG_CNTV_CTL_EL0: u16 = 0xdf19;
const HV_SYS_REG_CNTV_CVAL_EL0: u16 = 0xdf1a;
const HV_SYS_REG_SP_EL1: u16 = 0xe208;

const TLB_FLUSH_STUB: [u32; 5] = [
    0xd508_831f,
    0xd508_711f,
    0xd503_3b9f,
    0xd503_3fdf,
    0xd400_0002,
];

const SCTLR_M: u64 = 1;

const ESR_EC_WFX: u64 = 0x01;
const ESR_EC_HVC64: u64 = 0x16;
const ESR_EC_SYSREG: u64 = 0x18;
const ESR_EC_INSTRUCTION_ABORT_LOWER: u64 = 0x20;
const ESR_EC_DATA_ABORT_LOWER: u64 = 0x24;
const GUEST_PAGE: u64 = 4096;
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
const MDCCINT_WRITABLE_BITS: u64 = (1 << 29) | (1 << 30);
const SCTLR_SPAN: u64 = 1 << 23;
const SCTLR_DSSBS: u64 = 1 << 44;
const ESR_IL: u64 = 1 << 25;

const ICC_IAR1_EL1_CANONICAL: u32 = 0x0030_3018;

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

const fn hvf_implicit_identity_value(encoding: u32) -> Option<u64> {
    match encoding {
        0xc022 | 0xc027 | 0xc02a | 0xc032 | 0xc033 | 0xc03b | 0xc03c => Some(0),
        0xd801 => Some(0x0000_0000_8444_c004),
        _ => None,
    }
}

const ID_FEATURE_REGISTERS: core::ops::RangeInclusive<u32> = 0xc020..=0xc03f;

const SIGNED_ID_FIELDS: [(u32, u32); 5] = [
    (0xc020, 16),
    (0xc020, 20),
    (0xc028, 36),
    (0xc038, 24),
    (0xc038, 28),
];

const UNCHECKED_ID_FIELDS: [(u32, u32); 8] = [
    (0xc020, 24),
    (0xc038, 32),
    (0xc038, 36),
    (0xc038, 40),
    (0xc039, 4),
    (0xc039, 8),
    (0xc03a, 40),
    (0xc03a, 56),
];

fn id_field_level(encoding: u32, shift: u32, value: u64) -> i8 {
    let field = ((value >> shift) & 0xf) as i8;
    if field >= 8 && SIGNED_ID_FIELDS.contains(&(encoding, shift)) {
        field - 16
    } else {
        field
    }
}

fn id_register_within_host(encoding: u32, requested: u64, host: u64) -> bool {
    !ID_FEATURE_REGISTERS.contains(&encoding)
        || (0..64).step_by(4).all(|shift| {
            UNCHECKED_ID_FIELDS.contains(&(encoding, shift))
                || id_field_level(encoding, shift, requested)
                    <= id_field_level(encoding, shift, host)
        })
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

core::arch::global_asm!(
    ".globl _harmony_backend_hv_vcpu_set_simd_fp_reg",
    "_harmony_backend_hv_vcpu_set_simd_fp_reg:",
    "ldr q0, [x2]",
    "b _hv_vcpu_set_simd_fp_reg",
);

#[repr(C)]
#[derive(Default)]
struct HvSmeState {
    streaming: bool,
    za: bool,
}

type GetSmeMaxVl = unsafe extern "C" fn(*mut usize) -> i32;
type GetSmeState = unsafe extern "C" fn(u64, *mut HvSmeState) -> i32;
type SetSmeState = unsafe extern "C" fn(u64, *const HvSmeState) -> i32;

struct SmeApi {
    get: GetSmeState,
    set: SetSmeState,
    max_vl: GetSmeMaxVl,
}

impl SmeApi {
    fn load() -> Result<Option<Self>> {
        // SAFETY: RTLD_DEFAULT is the documented macOS handle; names are static C strings.
        let (get, set, max_vl) = unsafe {
            (
                dlsym((-2isize) as *mut c_void, c"hv_vcpu_get_sme_state".as_ptr()),
                dlsym((-2isize) as *mut c_void, c"hv_vcpu_set_sme_state".as_ptr()),
                dlsym(
                    (-2isize) as *mut c_void,
                    c"hv_sme_config_get_max_svl_bytes".as_ptr(),
                ),
            )
        };
        if !validate_sme_symbol_pair(!get.is_null(), !set.is_null(), !max_vl.is_null())? {
            let mut value = -1i32;
            let mut length = core::mem::size_of_val(&value);
            // SAFETY: the name is static; value and length are writable; null requests a read.
            let status = unsafe {
                sysctlbyname(
                    c"hw.optional.arm.FEAT_SME".as_ptr(),
                    (&mut value as *mut i32).cast(),
                    &mut length,
                    ptr::null_mut(),
                    0,
                )
            };
            validate_missing_sme_api(status, value, length)?;
            return Ok(None);
        }
        // SAFETY: nonnull symbols have the exact documented Hypervisor.framework ABI.
        Ok(Some(unsafe {
            Self {
                get: core::mem::transmute::<*mut c_void, GetSmeState>(get),
                set: core::mem::transmute::<*mut c_void, SetSmeState>(set),
                max_vl: core::mem::transmute::<*mut c_void, GetSmeMaxVl>(max_vl),
            }
        }))
    }
}

fn validate_sme_symbol_pair(get: bool, set: bool, max_vl: bool) -> Result<bool> {
    if get != set || get != max_vl {
        return Err(BackendError::Capability {
            cap: "complete HVF SME state inspection and reset API",
        });
    }
    Ok(get)
}

fn validate_sme_native_config(value: u64, api_available: bool) -> Result<()> {
    if !api_available && value & (0xf << 24) != 0 {
        return Err(BackendError::Capability {
            cap: "HVF SME API for native SME configuration",
        });
    }
    Ok(())
}

fn validate_missing_sme_api(status: i32, value: i32, length: usize) -> Result<()> {
    if status != 0 || value != 0 || length != core::mem::size_of::<i32>() {
        return Err(BackendError::Capability {
            cap: "HVF SME API or independently verified native SME absence",
        });
    }
    Ok(())
}

struct HvConfig(NonNull<c_void>);

impl HvConfig {
    fn new() -> Result<Self> {
        // SAFETY: the returned configuration is retained and owned by this wrapper.
        NonNull::new(unsafe { hv_vcpu_config_create() })
            .map(Self)
            .ok_or(BackendError::Internal(
                "hv_vcpu_config_create returned null",
            ))
    }

    fn identity(&self, sme_api_available: bool) -> Result<[u8; 32]> {
        let mut hash = Sha256::new();
        hash.update(b"harmony-arm64-hvf-configuration-v1\0");
        let feature_count = if sme_api_available { 14u32 } else { 12 };
        hash.update(feature_count.to_le_bytes());
        for feature in 0..feature_count {
            let mut value = 0;
            // SAFETY: the configuration is live and value is writable.
            hv("hv_vcpu_config_get_feature_reg", unsafe {
                hv_vcpu_config_get_feature_reg(self.0.as_ptr(), feature, &mut value)
            })?;
            if feature == 7 && value & (0xf << 32) != 0 {
                return Err(BackendError::Capability {
                    cap: "HVF snapshot support without non-streaming SVE",
                });
            }
            if feature == 8 {
                validate_sme_native_config(value, sme_api_available)?;
            }
            hash.update(feature.to_le_bytes());
            hash.update(value.to_le_bytes());
        }
        for cache_type in 0..2u32 {
            let mut values = [0u64; 8];
            // SAFETY: the configuration is live and values has eight writable entries.
            hv("hv_vcpu_config_get_ccsidr_el1_sys_reg_values", unsafe {
                hv_vcpu_config_get_ccsidr_el1_sys_reg_values(
                    self.0.as_ptr(),
                    cache_type,
                    values.as_mut_ptr(),
                )
            })?;
            hash.update(cache_type.to_le_bytes());
            for value in values {
                hash.update(value.to_le_bytes());
            }
        }
        Ok(hash.finalize().into())
    }
}

impl Drop for HvConfig {
    fn drop(&mut self) {
        // SAFETY: this wrapper owns the retained configuration and releases it once.
        unsafe { os_release(self.0.as_ptr()) };
    }
}

unsafe extern "C" {
    fn sysctlbyname(
        name: *const core::ffi::c_char,
        old: *mut c_void,
        length: *mut usize,
        new: *mut c_void,
        new_length: usize,
    ) -> i32;
    fn dlsym(handle: *mut c_void, name: *const core::ffi::c_char) -> *mut c_void;
    fn os_release(object: *mut c_void);
    fn sys_icache_invalidate(start: *mut c_void, len: usize);
}

#[link(name = "Hypervisor", kind = "framework")]
unsafe extern "C" {
    fn hv_vcpu_config_create() -> *mut c_void;
    fn hv_vcpu_config_get_feature_reg(config: *mut c_void, feature: u32, value: *mut u64) -> i32;
    fn hv_vcpu_config_get_ccsidr_el1_sys_reg_values(
        config: *mut c_void,
        cache_type: u32,
        values: *mut u64,
    ) -> i32;
    fn hv_vm_create(config: *mut c_void) -> i32;
    fn hv_vm_destroy() -> i32;
    fn hv_vm_map(addr: *mut c_void, ipa: u64, size: usize, flags: u64) -> i32;
    fn hv_vm_unmap(ipa: u64, size: usize) -> i32;
    fn hv_vm_protect(ipa: u64, size: usize, flags: u64) -> i32;

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

fn measured_entry(
    telemetry: &mut crate::PhysicalTelemetry,
    purpose: crate::RunPurpose,
    enter: impl FnOnce() -> Result<HvVcpuExit>,
) -> Result<HvVcpuExit> {
    telemetry.measure(crate::RunPlatform::HvfArm64, purpose, || {
        let result = enter();
        let outcome = result.as_ref().map_or(crate::RunReturn::Error, |raw| {
            crate::RunReturn::Returned(raw.reason)
        });
        (result, outcome)
    })
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

fn tracked_write_fault(exit: &HvExitException, regions: &[(u64, usize)]) -> Option<u64> {
    let ec = exit_ec(exit.syndrome);
    let ipa = exit.physical_address;
    ((ec == ESR_EC_DATA_ABORT_LOWER || ec == ESR_EC_INSTRUCTION_ABORT_LOWER)
        && regions
            .iter()
            .any(|&(gpa, len)| ipa >= gpa && ipa - gpa < len as u64))
    .then_some(ipa & !(HV_PAGE_SIZE as u64 - 1))
}

fn contiguous_runs<'a>(pages: impl IntoIterator<Item = &'a u64>) -> Vec<(u64, usize)> {
    let mut runs: Vec<(u64, usize)> = Vec::new();
    for &page in pages {
        match runs.last_mut() {
            Some((start, len)) if *start + *len as u64 == page => *len += HV_PAGE_SIZE,
            _ => runs.push((page, HV_PAGE_SIZE)),
        }
    }
    runs
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

fn mapped_host_spans(
    ranges: &[(usize, usize)],
    host_addr: usize,
    len: usize,
) -> impl Iterator<Item = (usize, usize)> + '_ {
    let end = host_addr.saturating_add(len);
    ranges.iter().filter_map(move |&(start, size)| {
        let lo = host_addr.max(start);
        let hi = end.min(start.saturating_add(size));
        (lo < hi).then(|| (lo, hi - lo))
    })
}

fn flush_stub_gpa(regions: &[(u64, usize)]) -> u64 {
    regions
        .iter()
        .map(|&(gpa, len)| gpa + len as u64)
        .max()
        .unwrap_or(0)
        .next_multiple_of(HV_PAGE_SIZE as u64)
}

fn canonical_sysreg(iss: u64) -> u32 {
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

fn host_counter() -> u64 {
    let value: u64;
    // SAFETY: CNTPCT_EL0 is a read-only counter register that macOS leaves
    // readable from EL0, and the read has no side effects.
    unsafe {
        core::arch::asm!("isb", "mrs {0}, cntpct_el0", out(reg) value, options(nomem, nostack));
    }
    value
}

fn validate_restore_vcpu_state(state: &Arm64VcpuState) -> Result<()> {
    if has_noncanonical_core_regs(&state.core)
        || has_unsupported_execution_mode(&state.core)
        || state.mp_state != MpState::Runnable
        || !state.vtimer.masked
        || state.vtimer.cntv_ctl_el0 & !0b10 != 0
        || u32::try_from(state.simd_fp.fpcr).is_err()
        || u32::try_from(state.simd_fp.fpsr).is_err()
        || state.debug.mdccint_el1 & !MDCCINT_WRITABLE_BITS != 0
    {
        return Err(BackendError::InvalidState);
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
pub struct HvfExitHandle {
    vcpu: u64,
}

impl HvfExitHandle {
    pub fn request_exit(self) -> Result<()> {
        let vcpus = [self.vcpu];
        // SAFETY: the array is live for the call. HVF treats vCPU identifiers
        // as values and returns an error for an identifier no longer present.
        hv("hv_vcpus_exit", unsafe {
            hv_vcpus_exit(vcpus.as_ptr(), vcpus.len() as u32)
        })
    }
}

pub struct HvfBackend {
    vcpu: u64,
    exit: NonNull<HvVcpuExit>,
    configured: bool,
    pending: Pending,
    pending_irq: Option<GicIntId>,
    accepted_irq: Option<GicIntId>,
    counts: ExitCounts,
    physical: crate::PhysicalTelemetry,
    regions: Vec<(u64, usize)>,
    writable: std::collections::BTreeSet<u64>,
    host_ranges: Vec<(usize, usize)>,
    cancel_run: std::sync::Arc<std::sync::atomic::AtomicBool>,
    flush_stub: Option<FlushStub>,
    host_id_regs: std::collections::BTreeMap<u16, u64>,
    register_identity: [u8; 32],
    sme_api: Option<SmeApi>,
    state_poisoned: bool,
    #[cfg(test)]
    fail_flush_after_staging: bool,
    #[cfg(test)]
    fail_policy_after_first_write: bool,
    #[cfg(test)]
    fail_fault_after_pstate: bool,
    #[cfg(test)]
    fail_run_after_guest: bool,
}

struct FlushStub {
    gpa: u64,
    page: *mut u8,
    layout: std::alloc::Layout,
}

impl HvfBackend {
    pub fn new() -> Result<Self> {
        let sme_api = SmeApi::load()?;
        let config = HvConfig::new()?;
        let register_identity = config.identity(sme_api.is_some())?;
        // SAFETY: null selects the documented default VM configuration.
        hv("hv_vm_create", unsafe { hv_vm_create(ptr::null_mut()) })?;
        let mut vcpu = 0;
        let mut exit = ptr::null();
        // SAFETY: output pointers and the retained configuration are live.
        if let Err(error) = hv("hv_vcpu_create", unsafe {
            hv_vcpu_create(&mut vcpu, &mut exit, config.0.as_ptr())
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
        let mut backend = Self {
            vcpu,
            exit,
            configured: false,
            pending: Pending::None,
            pending_irq: None,
            accepted_irq: None,
            counts: ExitCounts::default(),
            physical: crate::PhysicalTelemetry::default(),
            regions: Vec::new(),
            writable: std::collections::BTreeSet::new(),
            host_ranges: Vec::new(),
            cancel_run: std::sync::Arc::default(),
            flush_stub: None,
            host_id_regs: std::collections::BTreeMap::new(),
            register_identity,
            sme_api,
            state_poisoned: false,
            #[cfg(test)]
            fail_flush_after_staging: false,
            #[cfg(test)]
            fail_policy_after_first_write: false,
            #[cfg(test)]
            fail_fault_after_pstate: false,
            #[cfg(test)]
            fail_run_after_guest: false,
        };
        backend.reset_sme_state()?;
        let mut hash = Sha256::new();
        hash.update(b"harmony-arm64-hvf-register-identity-v1\0");
        hash.update(backend.register_identity);
        let mut max_vl = 0usize;
        if backend.sme_state()?.is_some() {
            // SAFETY: max_vl is writable and the symbol has the documented API signature.
            hv("hv_sme_config_get_max_svl_bytes", unsafe {
                (backend
                    .sme_api
                    .as_ref()
                    .ok_or(BackendError::InvalidState)?
                    .max_vl)(&mut max_vl)
            })?;
            if !(16..=256).contains(&max_vl) || !max_vl.is_power_of_two() {
                return Err(BackendError::InvalidState);
            }
        }
        hash.update((max_vl as u64).to_le_bytes());
        backend.register_identity = hash.finalize().into();
        for reg in HV_FIXED_ZERO_SYSREGS {
            backend.sysreg(reg)?;
            backend.set_sysreg(reg, 0)?;
            if backend.sysreg(reg)? != 0 {
                return Err(BackendError::Arm64RegisterInventory {
                    register: u64::from(reg),
                    reason: "HVF fixed register did not reset to zero",
                });
            }
        }
        let missing_pauth_keys = || BackendError::Capability {
            cap: "HVF pointer-authentication key read/write for deterministic snapshots",
        };
        for reg in HV_PAUTH_KEY_SYSREGS {
            backend.sysreg(reg).map_err(|_| missing_pauth_keys())?;
            backend
                .set_sysreg(reg, 0)
                .map_err(|_| missing_pauth_keys())?;
            if backend.sysreg(reg).map_err(|_| missing_pauth_keys())? != 0 {
                return Err(missing_pauth_keys());
            }
        }
        backend.set_sysreg(HV_SYS_REG_CNTV_CTL_EL0, 0)?;
        Ok(backend)
    }

    fn sme_state(&self) -> Result<Option<HvSmeState>> {
        let Some(api) = &self.sme_api else {
            return Ok(None);
        };
        let mut state = HvSmeState::default();
        // SAFETY: the vCPU is owned by this thread and state is writable.
        let status = unsafe { (api.get)(self.vcpu, &mut state) };
        if status == HV_UNSUPPORTED {
            return Ok(None);
        }
        hv("hv_vcpu_get_sme_state", status)?;
        Ok(Some(state))
    }

    fn reset_sme_state(&self) -> Result<()> {
        if self.sme_state()?.is_some() {
            // SAFETY: the vCPU is owned by this thread and state is readable.
            hv("hv_vcpu_set_sme_state", unsafe {
                (self.sme_api.as_ref().ok_or(BackendError::InvalidState)?.set)(
                    self.vcpu,
                    &HvSmeState::default(),
                )
            })?;
            if self
                .sme_state()?
                .is_none_or(|state| state.streaming || state.za)
            {
                return Err(BackendError::InvalidState);
            }
        }
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<()> {
        if self.state_poisoned {
            return Err(BackendError::Internal("backend state mutation failed"));
        }
        Ok(())
    }

    fn ensure_flush_stub(&mut self) -> Result<u64> {
        if let Some(stub) = &self.flush_stub {
            return Ok(stub.gpa);
        }
        let gpa = flush_stub_gpa(&self.regions);
        let layout = std::alloc::Layout::from_size_align(HV_PAGE_SIZE, HV_PAGE_SIZE)
            .map_err(|_| BackendError::Internal("HVF flush stub layout"))?;
        // SAFETY: the layout has a non-zero size, so this is a valid allocation
        // request; the pointer is checked for null before any use.
        let page = unsafe { std::alloc::alloc_zeroed(layout) };
        if page.is_null() {
            return Err(BackendError::Memory("HVF flush stub allocation failed"));
        }
        for (slot, word) in TLB_FLUSH_STUB.iter().enumerate() {
            // SAFETY: the allocation is HV_PAGE_SIZE bytes and the stub is five
            // words, so every write lands inside it.
            unsafe { page.add(slot * 4).cast::<u32>().write(*word) };
        }
        // SAFETY: the allocation is live for as long as this backend, nothing
        // else refers to it, and its layout gives it the alignment and length
        // the framework requires.
        let mapped = hv("hv_vm_map", unsafe {
            hv_vm_map(
                page.cast(),
                gpa,
                HV_PAGE_SIZE,
                HV_MEMORY_READ | HV_MEMORY_EXEC,
            )
        });
        if let Err(error) = mapped {
            // SAFETY: the allocation came from this layout and nothing mapped it.
            unsafe { std::alloc::dealloc(page, layout) };
            return Err(error);
        }
        // SAFETY: `page` is a live HV_PAGE_SIZE allocation owned by this
        // backend, and the call only performs cache maintenance on it.
        unsafe { sys_icache_invalidate(page.cast(), HV_PAGE_SIZE) };
        self.flush_stub = Some(FlushStub { gpa, page, layout });
        Ok(gpa)
    }

    fn run_entry(&mut self, purpose: crate::RunPurpose) -> Result<HvVcpuExit> {
        let vcpu = self.vcpu;
        let exit = self.exit;
        measured_entry(&mut self.physical, purpose, || {
            // SAFETY: the owning thread exclusively owns this live vCPU for the duration of the call.
            hv("hv_vcpu_run", unsafe { hv_vcpu_run(vcpu) })?;
            // SAFETY: HVF owns the live exit page; the successful run has completed writing it and the owner reads it before another entry.
            Ok(unsafe { *exit.as_ptr() })
        })
    }

    fn flush_guest_translations(&mut self) -> Result<()> {
        let gpa = self.ensure_flush_stub()?;
        let sctlr = self.sysreg(HV_SYS_REG_SCTLR_EL1)?;
        self.set_sysreg(HV_SYS_REG_SCTLR_EL1, sctlr & !SCTLR_M)?;
        self.set_reg(HV_REG_CPSR, PSTATE_DAIF | PSTATE_MODE_EL1H)?;
        self.set_reg(HV_REG_PC, gpa)?;
        // SAFETY: this is the owning thread and the vCPU is live.
        hv("hv_vcpu_set_pending_interrupt", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_IRQ, false)
        })?;
        #[cfg(test)]
        if self.fail_flush_after_staging {
            return Err(BackendError::Internal("injected HVF flush failure"));
        }
        for _ in 0..2 {
            let raw = self.run_entry(crate::RunPurpose::Maintenance)?;
            if raw.reason == HV_EXIT_REASON_CANCELED {
                continue;
            }
            if raw.reason == HV_EXIT_REASON_EXCEPTION
                && exit_ec(raw.exception.syndrome) == ESR_EC_HVC64
                && self.reg(HV_REG_PC)? == gpa + (TLB_FLUSH_STUB.len() * 4) as u64
            {
                return Ok(());
            }
            break;
        }
        Err(BackendError::Internal(
            "HVF translation flush stub did not reach its hypercall",
        ))
    }

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
        self.ensure_healthy()?;
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
                SMCCC_ARCH_WORKAROUND_1 => 1,
                SMCCC_ARCH_WORKAROUND_2 => PSCI_NOT_SUPPORTED,
                SMCCC_ARCH_WORKAROUND_3 => 0,
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
            let raw = self.run_entry(crate::RunPurpose::Guest)?;
            if raw.reason == HV_EXIT_REASON_EXCEPTION
                && let Some(page) = tracked_write_fault(&raw.exception, &self.regions)
            {
                if !self.writable.insert(page) {
                    return Err(BackendError::Internal(
                        "HVF reported a write fault on a page it already made writable",
                    ));
                }
                // SAFETY: the page lies inside a region this backend mapped with
                // `hv_vm_map`, so the call only changes the guest's permissions on
                // memory the backend already exposes.
                hv("hv_vm_protect", unsafe {
                    hv_vm_protect(
                        page,
                        HV_PAGE_SIZE,
                        HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
                    )
                })?;
                continue;
            }
            #[cfg(test)]
            if self.fail_run_after_guest {
                return Err(BackendError::Internal("injected post-run HVF failure"));
            }
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
        if let Some(stub) = self.flush_stub.take() {
            // SAFETY: this backend mapped the stub page at that address and is
            // the sole owner of the allocation behind it.
            unsafe {
                let _ = hv_vm_unmap(stub.gpa, HV_PAGE_SIZE);
                std::alloc::dealloc(stub.page, stub.layout);
            }
        }
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
        self.ensure_healthy()?;
        let mut writes = Vec::new();
        for (&encoding, &value) in &policy.id_regs.regs {
            if !matches!(
                encoding,
                0xc000 | 0xc005 | 0xc006 | 0xc008..=0xc03f | 0xc801 | 0xc807 | 0xd801 | 0xd807
            ) {
                return Err(BackendError::Arm64RegisterInventory {
                    register: u64::from(encoding),
                    reason: "HVF policy does not name a supported identity field",
                });
            }
            if let Some(measured) = hvf_implicit_identity_value(encoding) {
                if value != measured {
                    return Err(BackendError::InvalidState);
                }
                continue;
            }
            let reg = u16::try_from(encoding).map_err(|_| BackendError::InvalidState)?;
            let host = match self.host_id_regs.get(&reg) {
                Some(&host) => host,
                None => {
                    let host = self.sysreg(reg)?;
                    self.host_id_regs.insert(reg, host);
                    host
                }
            };
            if !id_register_within_host(encoding, value, host) {
                return Err(BackendError::IdRegisterAboveHost {
                    encoding,
                    requested: value,
                    host,
                });
            }
            writes.push((reg, value));
        }
        self.state_poisoned = true;
        self.reset_sme_state()?;
        for reg in HV_FIXED_ZERO_SYSREGS {
            self.set_sysreg(reg, 0)?;
        }
        for reg in HV_PAUTH_KEY_SYSREGS {
            self.set_sysreg(reg, 0)?;
        }
        for (reg, value) in writes {
            self.set_sysreg(reg, value)?;
            #[cfg(test)]
            if self.fail_policy_after_first_write {
                return Err(BackendError::Internal("injected HVF policy failure"));
            }
        }
        self.state_poisoned = false;
        let _ = &policy.sysreg_traps;
        self.configured = true;
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        self.ensure_healthy()?;
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
        let stub = self.flush_stub.as_ref().map(|stub| stub.gpa);
        for (mapped_gpa, mapped_len) in self
            .regions
            .iter()
            .copied()
            .chain(stub.map(|gpa| (gpa, HV_PAGE_SIZE)))
        {
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
                HV_MEMORY_READ | HV_MEMORY_EXEC,
            )
        })?;
        self.regions.push((gpa.0, host.len()));
        self.host_ranges
            .push((host.as_mut_ptr() as usize, host.len()));
        Ok(())
    }

    fn drain_dirty_pages(&mut self) -> Result<Vec<u64>> {
        self.ensure_healthy()?;
        for (start, len) in contiguous_runs(&self.writable) {
            // SAFETY: every recorded page lies inside a region this backend
            // mapped with `hv_vm_map`, and the call only removes the guest's
            // write permission there.
            hv("hv_vm_protect", unsafe {
                hv_vm_protect(start, len, HV_MEMORY_READ | HV_MEMORY_EXEC)
            })?;
        }
        let per_page = HV_PAGE_SIZE as u64 / GUEST_PAGE;
        let gfns = std::mem::take(&mut self.writable)
            .into_iter()
            .flat_map(|page| (page / GUEST_PAGE..).take(per_page as usize))
            .collect();
        Ok(gfns)
    }

    fn run(&mut self) -> Result<Exit<Arm64>> {
        if self.cancel_run.load(std::sync::atomic::Ordering::Acquire) {
            return Err(BackendError::Internal("HVF run canceled by host"));
        }
        self.ensure_runnable()?;
        match self.enter_guest() {
            Ok(exit) => Ok(exit),
            Err(error) => {
                self.state_poisoned = true;
                Err(error)
            }
        }
    }

    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        Some(std::sync::Arc::clone(&self.cancel_run))
    }

    fn invalidate_instruction_cache(&mut self, host_addr: usize, len: usize) {
        for (start, size) in mapped_host_spans(&self.host_ranges, host_addr, len) {
            // SAFETY: the span lies inside a host range this backend passed to
            // `hv_vm_map`, whose caller keeps it pinned while mapped, and the
            // call only performs cache maintenance on those bytes.
            unsafe { sys_icache_invalidate(start as *mut c_void, size) };
        }
    }

    fn inject(&mut self, event: Arm64Injection) -> Result<()> {
        match event {
            Arm64Injection::Interrupt { intid } => self.set_pending_irq(Some(intid)),
        }
    }

    fn set_pending_irq(&mut self, id: Option<GicIntId>) -> Result<()> {
        self.ensure_healthy()?;
        self.pending_irq = id;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        self.accepted_irq.take()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        self.ensure_healthy()?;
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
        self.ensure_healthy()?;
        match self.pending {
            Pending::SysregRead { .. } | Pending::SysregWrite => {
                let exception = undefined_exception(
                    self.reg(HV_REG_PC)?,
                    self.reg(HV_REG_CPSR)?,
                    self.sysreg(HV_SYS_REG_SCTLR_EL1)?,
                    self.sysreg(HV_SYS_REG_VBAR_EL1)?,
                )?;
                self.state_poisoned = true;
                self.set_sysreg(HV_SYS_REG_ELR_EL1, exception.elr_el1)?;
                self.set_sysreg(HV_SYS_REG_SPSR_EL1, exception.spsr_el1)?;
                self.set_sysreg(HV_SYS_REG_ESR_EL1, exception.esr_el1)?;
                self.set_reg(HV_REG_CPSR, exception.pstate)?;
                #[cfg(test)]
                if self.fail_fault_after_pstate {
                    return Err(BackendError::Internal(
                        "injected HVF fault completion failure",
                    ));
                }
                self.set_reg(HV_REG_PC, exception.pc)?;
                self.pending = Pending::None;
                self.state_poisoned = false;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        self.ensure_healthy()?;
        if self.pending == Pending::SysregWrite {
            self.pending = Pending::None;
            Ok(())
        } else {
            Err(BackendError::BadCompletion)
        }
    }

    fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
        self.ensure_healthy()?;
        Err(BackendError::NoPendingRead)
    }

    fn complete_arch(&mut self, completion: crate::arch::arm64::Arm64Completion) -> Result<()> {
        match completion {}
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        self.ensure_healthy()
    }

    fn prepare_snapshot(&mut self) -> Result<()> {
        self.ensure_healthy()
    }

    fn read_irq_mask(&mut self) -> Result<Option<bool>> {
        self.ensure_healthy()?;
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        Ok(Some(self.reg(HV_REG_CPSR)? & (1 << 7) != 0))
    }

    fn save(&mut self) -> Result<Arm64VcpuState> {
        self.ensure_healthy()?;
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        if self
            .sme_state()?
            .is_some_and(|state| state.streaming || state.za)
        {
            return Err(BackendError::Unsupported {
                what: "save HVF with active SME state",
            });
        }
        for reg in HV_FIXED_ZERO_SYSREGS {
            if self.sysreg(reg)? != 0 {
                if reg == HV_SYS_REG_ACTLR_EL1 {
                    return Err(BackendError::Unsupported {
                        what: "save HVF with noncanonical ACTLR_EL1",
                    });
                }
                return Err(BackendError::Arm64RegisterInventory {
                    register: u64::from(reg),
                    reason: "HVF register must remain fixed at zero",
                });
            }
        }
        for reg in HV_PAUTH_KEY_SYSREGS {
            if self.sysreg(reg)? != 0 {
                return Err(BackendError::Unsupported {
                    what: "save HVF with noncanonical pointer-authentication key",
                });
            }
        }
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
        if has_unsupported_execution_mode(&state.core) {
            return Err(BackendError::Unsupported {
                what: "HVF supports only EL0/EL1 AArch64 execution state",
            });
        }
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
        state.sysregs.tpidrro_el0 = self.sysreg(HV_SYS_REG_TPIDRRO_EL0)?;
        state.sysregs.cntkctl_el1 = self.sysreg(HV_SYS_REG_CNTKCTL_EL1)?;
        state.sysregs.contextidr_el1 = self.sysreg(HV_SYS_REG_CONTEXTIDR_EL1)?;
        state.sysregs.amair_el1 = self.sysreg(HV_SYS_REG_AMAIR_EL1)?;
        state.sysregs.par_el1 = self.sysreg(HV_SYS_REG_PAR_EL1)?;
        state.sysregs.csselr_el1 = self.sysreg(HV_SYS_REG_CSSELR_EL1)?;
        state.sysregs.afsr0_el1 = self.sysreg(HV_SYS_REG_AFSR0_EL1)?;
        state.sysregs.afsr1_el1 = self.sysreg(HV_SYS_REG_AFSR1_EL1)?;
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
        state.debug.mdccint_el1 = self.sysreg(HV_SYS_REG_MDCCINT_EL1)?;
        state.debug.mdscr_el1 = self.sysreg(HV_SYS_REG_MDSCR_EL1)?;
        // SAFETY: outputs are live bools and this is the owning thread.
        hv("hv_vcpu_get_trap_debug_exceptions", unsafe {
            hv_vcpu_get_trap_debug_exceptions(self.vcpu, &mut state.debug.trap_debug_exceptions)
        })?;
        // SAFETY: outputs are live bools and this is the owning thread.
        hv("hv_vcpu_get_trap_debug_reg_accesses", unsafe {
            hv_vcpu_get_trap_debug_reg_accesses(self.vcpu, &mut state.debug.trap_debug_reg_accesses)
        })?;
        state.vtimer.cntv_ctl_el0 = self.sysreg(HV_SYS_REG_CNTV_CTL_EL0)? & 0b11;
        if state.vtimer.cntv_ctl_el0 & 1 != 0 {
            return Err(BackendError::InvalidState);
        }
        state.vtimer.cntv_cval_el0 = self.sysreg(HV_SYS_REG_CNTV_CVAL_EL0)?;
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_vtimer_mask", unsafe {
            hv_vcpu_get_vtimer_mask(self.vcpu, &mut state.vtimer.masked)
        })?;
        let mut vtimer_offset = 0u64;
        // SAFETY: outputs are live and this is the owning thread.
        hv("hv_vcpu_get_vtimer_offset", unsafe {
            hv_vcpu_get_vtimer_offset(self.vcpu, &mut vtimer_offset)
        })?;
        state.vtimer.counter = host_counter().wrapping_sub(vtimer_offset);
        if !state.vtimer.masked {
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

    fn validate_restore_state(&self, state: &Arm64VcpuState) -> Result<()> {
        self.ensure_healthy()?;
        validate_restore_vcpu_state(state)?;
        if state.core.pstate >> 32 != 0 {
            return Err(BackendError::InvalidState);
        }
        Ok(())
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
        self.ensure_healthy()?;
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        self.validate_restore_state(state)?;
        self.state_poisoned = true;
        self.reset_sme_state()?;
        self.flush_guest_translations()?;
        for reg in HV_FIXED_ZERO_SYSREGS {
            self.set_sysreg(reg, 0)?;
        }
        for reg in HV_PAUTH_KEY_SYSREGS {
            self.set_sysreg(reg, 0)?;
        }
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
        self.set_sysreg(HV_SYS_REG_TPIDRRO_EL0, state.sysregs.tpidrro_el0)?;
        self.set_sysreg(HV_SYS_REG_CNTKCTL_EL1, state.sysregs.cntkctl_el1)?;
        self.set_sysreg(HV_SYS_REG_CONTEXTIDR_EL1, state.sysregs.contextidr_el1)?;
        self.set_sysreg(HV_SYS_REG_AMAIR_EL1, state.sysregs.amair_el1)?;
        self.set_sysreg(HV_SYS_REG_PAR_EL1, state.sysregs.par_el1)?;
        self.set_sysreg(HV_SYS_REG_CSSELR_EL1, state.sysregs.csselr_el1)?;
        self.set_sysreg(HV_SYS_REG_AFSR0_EL1, state.sysregs.afsr0_el1)?;
        self.set_sysreg(HV_SYS_REG_AFSR1_EL1, state.sysregs.afsr1_el1)?;
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
        self.set_sysreg(HV_SYS_REG_MDCCINT_EL1, state.debug.mdccint_el1)?;
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
        self.set_sysreg(HV_SYS_REG_CNTV_CTL_EL0, state.vtimer.cntv_ctl_el0)?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_vtimer_mask", unsafe {
            hv_vcpu_set_vtimer_mask(self.vcpu, true)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_vtimer_offset", unsafe {
            hv_vcpu_set_vtimer_offset(self.vcpu, host_counter().wrapping_sub(state.vtimer.counter))
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_pending_interrupt(IRQ)", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_IRQ, state.interrupts.irq)
        })?;
        // SAFETY: the vCPU is live and this is the owning thread.
        hv("hv_vcpu_set_pending_interrupt(FIQ)", unsafe {
            hv_vcpu_set_pending_interrupt(self.vcpu, HV_INTERRUPT_TYPE_FIQ, state.interrupts.fiq)
        })?;
        self.pending_irq = None;
        self.accepted_irq = None;
        self.state_poisoned = false;
        Ok(())
    }

    fn physical_telemetry(&self) -> crate::PhysicalTelemetry {
        self.physical
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
            arch: Arm64Caps {
                in_kernel_gic: false,
                asid_bits: Arm64AsidBits::Eight,
                smccc_workaround_levels: Arm64Caps::FIXED_SMCCC_WORKAROUND_LEVELS,
                psci_profile: crate::arch::arm64::Arm64PsciProfile::HvfSingleVcpu,
                gic_ctlr_fixed_bits: 0,
                gicd_iidr: 0,
                gicd_typer2: 0,
                register_identity: self.register_identity,
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
    fn missing_sme_api_requires_independent_exact_absence() {
        for bits in 0..8 {
            assert_eq!(
                validate_sme_symbol_pair(bits & 1 != 0, bits & 2 != 0, bits & 4 != 0).is_ok(),
                matches!(bits, 0 | 7),
            );
        }
        for (status, value, length, accepted) in [
            (0, 0, 4, true),
            (0, 1, 4, false),
            (0, -1, 4, false),
            (-1, 0, 4, false),
            (0, 0, 0, false),
            (0, 0, 8, false),
        ] {
            assert_eq!(
                validate_missing_sme_api(status, value, length).is_ok(),
                accepted
            );
        }
        assert!(validate_sme_native_config(0, false).is_ok());
        for level in 1..16 {
            assert!(validate_sme_native_config(level << 24, false).is_err());
            assert!(validate_sme_native_config(level << 24, true).is_ok());
        }
    }

    #[test]
    #[ignore = "live HVF inventory boundary; registered in HOST_TESTS"]
    fn fixed_register_inventory_refuses_capture_and_resets_restore() {
        for reg in HV_FIXED_ZERO_SYSREGS {
            let mut backend = HvfBackend::new().unwrap();
            backend.set_policy(&Arm64Policy::default()).unwrap();
            let baseline = backend.save().unwrap();
            let value = if reg == HV_SYS_REG_ACTLR_EL1 { 2 } else { 1 };
            backend.set_sysreg(reg, value).unwrap();
            let observed = backend.sysreg(reg).unwrap();
            eprintln!("HVF fixed register {reg:#06x}: detour={observed:#x}");
            assert_ne!(observed, 0, "detour did not exercise {reg:#06x}");
            assert!(backend.save().is_err(), "capture accepted {reg:#06x}");
            assert_eq!(backend.sysreg(reg).unwrap(), observed);
            backend.restore(&baseline).unwrap();
            assert_eq!(backend.sysreg(reg).unwrap(), 0);
            drop(backend);
            let mut fresh = HvfBackend::new().unwrap();
            fresh.set_policy(&Arm64Policy::default()).unwrap();
            fresh.restore(&baseline).unwrap();
            assert_eq!(fresh.sysreg(reg).unwrap(), 0);
        }
    }

    #[test]
    #[ignore = "live HVF SME boundary; registered in HOST_TESTS"]
    fn sme_capture_requires_inactive_state_and_restore_clears_detour() {
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let Some(_) = backend.sme_state().unwrap() else {
            eprintln!("SME_ACTIVE_TEST=UNSUPPORTED; explicit HV_UNSUPPORTED verified");
            return;
        };
        let mut baseline = backend.save().unwrap();
        baseline.simd_fp.q[3] = [0x73; 16];
        baseline.simd_fp.fpsr = 0;
        for (streaming, za) in [(true, false), (false, true), (true, true)] {
            let state = HvSmeState { streaming, za };
            // SAFETY: this test owns the live vCPU and state is readable.
            hv("hv_vcpu_set_sme_state", unsafe {
                (backend.sme_api.as_ref().unwrap().set)(backend.vcpu, &state)
            })
            .unwrap();
            let observed = backend.sme_state().unwrap().unwrap();
            assert_eq!((observed.streaming, observed.za), (streaming, za));
            assert!(matches!(
                backend.save(),
                Err(BackendError::Unsupported {
                    what: "save HVF with active SME state",
                })
            ));
            let observed = backend.sme_state().unwrap().unwrap();
            assert_eq!((observed.streaming, observed.za), (streaming, za));
            backend.restore(&baseline).unwrap();
            let observed = backend.sme_state().unwrap().unwrap();
            assert!(!observed.streaming && !observed.za);
            let restored = backend.save().unwrap();
            assert_eq!(restored.simd_fp, baseline.simd_fp);
        }
        eprintln!("SME_ACTIVE_TEST=PASSED");
    }

    #[test]
    fn physical_hvf_boundary_distinguishes_canceled_returns_maintenance_and_api_errors() {
        use crate::{RunPlatform, RunPurpose};
        let mut telemetry = crate::PhysicalTelemetry::default();
        let mut canceled = raw_exit(0, 0);
        canceled.reason = HV_EXIT_REASON_CANCELED;
        measured_entry(&mut telemetry, RunPurpose::Maintenance, || Ok(canceled)).unwrap();
        measured_entry(&mut telemetry, RunPurpose::Maintenance, || {
            Ok(raw_exit(0, 0))
        })
        .unwrap();
        assert!(
            measured_entry(&mut telemetry, RunPurpose::Guest, || Err(
                BackendError::Internal("scripted HVF API failure")
            ))
            .is_err()
        );
        let maintenance = telemetry.cost(RunPlatform::HvfArm64, RunPurpose::Maintenance);
        assert_eq!(
            (
                maintenance.entries,
                maintenance.returns,
                maintenance.errors,
                maintenance.interruptions
            ),
            (2, 2, 0, 0)
        );
        assert_eq!(maintenance.raw_exit_count(HV_EXIT_REASON_CANCELED), 1);
        assert_eq!(maintenance.raw_exit_count(HV_EXIT_REASON_EXCEPTION), 1);
        let guest = telemetry.cost(RunPlatform::HvfArm64, RunPurpose::Guest);
        assert_eq!((guest.entries, guest.returns, guest.errors), (1, 0, 1));
    }

    #[test]
    fn lower_el_aborts_inside_mapped_ram_are_tracked_writes() {
        let regions = [(0x4000_0000, 0x10_0000)];
        let fault = |ec: u64, ipa: u64| {
            tracked_write_fault(&raw_exit((ec << 26) | 0x1c1_8047, ipa).exception, &regions)
        };
        assert_eq!(fault(0x24, 0x4002_4ff8), Some(0x4002_4000));
        assert_eq!(fault(0x20, 0x400f_fff8), Some(0x400f_c000));
        assert_eq!(fault(0x24, 0x0900_0000), None, "outside RAM");
        assert_eq!(fault(0x24, 0x4010_0000), None, "one past RAM");
        assert_eq!(fault(0x25, 0x4002_4ff8), None);
    }

    #[test]
    fn written_pages_coalesce_into_contiguous_protect_runs() {
        let page = HV_PAGE_SIZE as u64;
        assert_eq!(
            contiguous_runs(&[0, page, 2 * page, 5 * page, 6 * page, 9 * page]),
            [
                (0, 3 * HV_PAGE_SIZE),
                (5 * page, 2 * HV_PAGE_SIZE),
                (9 * page, HV_PAGE_SIZE)
            ]
        );
        assert!(contiguous_runs(&[]).is_empty());
    }

    #[test]
    fn flush_stub_invalidates_translations_and_instruction_cache_then_exits() {
        let sys = |op0: u32, op1: u32, crn: u32, crm: u32, op2: u32, rt: u32| {
            0xd500_0000 | (op0 << 19) | (op1 << 16) | (crn << 12) | (crm << 8) | (op2 << 5) | rt
        };
        let barrier = |crm: u32, op2: u32| 0xd503_301f | (crm << 8) | (op2 << 5);
        assert_eq!(TLB_FLUSH_STUB[0], sys(1, 0, 8, 3, 0, 31));
        assert_eq!(TLB_FLUSH_STUB[1], sys(1, 0, 7, 1, 0, 31));
        assert_eq!(TLB_FLUSH_STUB[2], barrier(0b1011, 4));
        assert_eq!(TLB_FLUSH_STUB[3], barrier(0b1111, 6));
        assert_eq!(TLB_FLUSH_STUB[4], 0xd400_0002);
    }

    #[test]
    #[ignore = "live HVF guest-entry failure boundary; run on Apple silicon"]
    fn failed_guest_run_retires_uncounted_progress() {
        #[repr(C, align(16384))]
        struct TestPage([u8; 16384]);

        let mut page = Box::new(TestPage([0; 16384]));
        for (index, word) in [0xd282_4680u32, 0xd400_0002u32].iter().enumerate() {
            page.0[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
        }
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        // SAFETY: Box pins the aligned page, which remains live until after backend drop.
        unsafe { backend.map_memory(Gpa(0x4000_0000), &mut page.0) }.unwrap();
        backend.invalidate_instruction_cache(page.0.as_ptr() as usize, page.0.len());
        let sctlr = backend.sysreg(HV_SYS_REG_SCTLR_EL1).unwrap();
        backend
            .set_sysreg(HV_SYS_REG_SCTLR_EL1, sctlr & !SCTLR_M)
            .unwrap();
        backend
            .set_reg(HV_REG_CPSR, PSTATE_DAIF | PSTATE_MODE_EL1H)
            .unwrap();
        backend.set_reg(HV_REG_PC, 0x4000_0000).unwrap();
        let before = backend.save().unwrap();
        backend
            .cancel_run
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            backend.run(),
            Err(BackendError::Internal("HVF run canceled by host"))
        ));
        assert!(backend.save().is_ok());
        backend
            .cancel_run
            .store(false, std::sync::atomic::Ordering::Release);
        backend.fail_run_after_guest = true;
        assert!(matches!(
            backend.run(),
            Err(BackendError::Internal("injected post-run HVF failure"))
        ));
        assert_eq!(backend.reg(0).unwrap(), 0x1234);
        assert_eq!(backend.reg(HV_REG_PC).unwrap(), 0x4000_0008);
        assert_eq!(backend.exit_counts(), ExitCounts::default());
        for result in [
            backend.prepare_snapshot(),
            backend.save().map(|_| ()),
            backend.read_irq_mask().map(|_| ()),
            backend.run().map(|_| ()),
            backend.restore(&before),
        ] {
            assert!(matches!(
                result,
                Err(BackendError::Internal("backend state mutation failed"))
            ));
        }
    }

    #[test]
    #[ignore = "live HVF flush-origin boundary; run on Apple silicon"]
    fn restore_rejects_hvc_from_guest_exception_vector() {
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let mut target = backend.save().unwrap();
        target.core.pc = 0x1000;
        let stub_gpa = backend.ensure_flush_stub().unwrap();
        let stub_page = backend.flush_stub.as_ref().unwrap().page;
        // SAFETY: the owned flush allocation is a live 16 KiB page; both word
        // offsets lie within it, and the vCPU is stopped while it is changed.
        unsafe {
            stub_page.add(16).cast::<u32>().write(0);
            stub_page.add(0x200).cast::<u32>().write(0xd400_0002);
            sys_icache_invalidate(stub_page.cast(), HV_PAGE_SIZE);
        }
        backend.set_sysreg(HV_SYS_REG_VBAR_EL1, stub_gpa).unwrap();
        assert!(matches!(
            backend.restore(&target),
            Err(BackendError::Internal(
                "HVF translation flush stub did not reach its hypercall"
            ))
        ));
        assert_eq!(backend.reg(HV_REG_PC).unwrap(), stub_gpa + 0x204);
        assert!(backend.state_poisoned);
    }

    #[test]
    #[ignore = "live HVF failed-restore boundary; run on Apple silicon"]
    fn failed_translation_flush_retires_mutated_vcpu() {
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let mut target = backend.save().unwrap();
        target.core.pc = 0x1000;
        validate_restore_vcpu_state(&target).unwrap();
        let mut invalid = target;
        invalid.vtimer.masked = false;
        assert!(matches!(
            backend.restore(&invalid),
            Err(BackendError::InvalidState)
        ));
        assert!(backend.save().is_ok());
        backend.fail_flush_after_staging = true;
        assert!(matches!(
            backend.restore(&target),
            Err(BackendError::Internal("injected HVF flush failure"))
        ));
        assert_ne!(backend.reg(HV_REG_PC).unwrap(), target.core.pc);
        for result in [
            backend.prepare_snapshot(),
            backend.save().map(|_| ()),
            backend.read_irq_mask().map(|_| ()),
            backend.run().map(|_| ()),
            backend.restore(&target),
        ] {
            assert!(matches!(
                result,
                Err(BackendError::Internal("backend state mutation failed"))
            ));
        }
    }

    #[test]
    #[ignore = "live HVF fault-completion boundary; run on Apple silicon"]
    fn failed_fault_completion_retires_partly_changed_vcpu() {
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        backend.set_reg(HV_REG_PC, 0x1004).unwrap();
        backend.set_reg(HV_REG_CPSR, PSTATE_MODE_EL0T).unwrap();
        backend.set_sysreg(HV_SYS_REG_VBAR_EL1, 0x8000).unwrap();
        let original_pstate = backend.reg(HV_REG_CPSR).unwrap();
        backend.pending = Pending::SysregRead { reg: 0 };
        backend.fail_fault_after_pstate = true;
        assert!(matches!(
            backend.complete_fault(),
            Err(BackendError::Internal(
                "injected HVF fault completion failure"
            ))
        ));
        assert_eq!(
            backend.sysreg(HV_SYS_REG_SPSR_EL1).unwrap(),
            original_pstate
        );
        let changed_pstate = backend.reg(HV_REG_CPSR).unwrap();
        assert_ne!(changed_pstate, original_pstate);
        backend.fail_fault_after_pstate = false;
        assert!(matches!(
            backend.complete_fault(),
            Err(BackendError::Internal("backend state mutation failed"))
        ));
        assert_eq!(
            backend.sysreg(HV_SYS_REG_SPSR_EL1).unwrap(),
            original_pstate
        );
        assert!(matches!(
            backend.prepare_snapshot(),
            Err(BackendError::Internal("backend state mutation failed"))
        ));
        assert!(matches!(
            backend.save(),
            Err(BackendError::Internal("backend state mutation failed"))
        ));
    }

    #[test]
    #[ignore = "live HVF policy boundary; run on Apple silicon"]
    fn rejected_policy_preserves_live_identity() {
        let mut backend = HvfBackend::new().unwrap();
        let initial = Arm64Policy {
            id_regs: crate::arch::arm64::IdRegModel {
                regs: [(0xc030, 0)].into_iter().collect(),
            },
            ..Arm64Policy::default()
        };
        backend.set_policy(&initial).unwrap();
        assert_eq!(backend.sysreg(0xc030).unwrap(), 0);
        let rejected = Arm64Policy {
            id_regs: crate::arch::arm64::IdRegModel {
                regs: [(0xc030, 0x20), (0xc038, 0xf << 4)].into_iter().collect(),
            },
            ..Arm64Policy::default()
        };
        assert!(matches!(
            backend.set_policy(&rejected),
            Err(BackendError::IdRegisterAboveHost {
                encoding: 0xc038,
                ..
            })
        ));
        assert_eq!(backend.sysreg(0xc030).unwrap(), 0);
        assert!(backend.save().is_ok());
    }

    #[test]
    #[ignore = "live HVF policy boundary; run on Apple silicon"]
    fn failed_policy_write_retires_mutated_vcpu() {
        let mut backend = HvfBackend::new().unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let saved = backend.save().unwrap();
        backend.fail_policy_after_first_write = true;
        let policy = Arm64Policy {
            id_regs: crate::arch::arm64::IdRegModel {
                regs: [(0xc030, 0x20)].into_iter().collect(),
            },
            ..Arm64Policy::default()
        };
        assert!(matches!(
            backend.set_policy(&policy),
            Err(BackendError::Internal("injected HVF policy failure"))
        ));
        assert_eq!(backend.sysreg(0xc030).unwrap(), 0x20);
        for result in [
            backend.prepare_snapshot(),
            backend.save().map(|_| ()),
            backend.run().map(|_| ()),
            backend.restore(&saved),
            backend.set_policy(&policy),
        ] {
            assert!(matches!(
                result,
                Err(BackendError::Internal("backend state mutation failed"))
            ));
        }
    }

    #[test]
    fn instruction_cache_spans_stay_inside_mapped_host_ranges() {
        let ranges = [(0x10_000, 0x4000), (0x40_000, 0x8000)];
        let spans = |addr, len| mapped_host_spans(&ranges, addr, len).collect::<Vec<_>>();
        assert_eq!(spans(1, 64), []);
        assert_eq!(spans(0x10_000, 0), []);
        assert_eq!(spans(0x10_100, 0x100), [(0x10_100, 0x100)]);
        assert_eq!(spans(0xf_000, 0x2000), [(0x10_000, 0x1000)]);
        assert_eq!(
            spans(0x12_000, 0x40_000),
            [(0x12_000, 0x2000), (0x40_000, 0x8000)]
        );
        assert_eq!(spans(usize::MAX - 4, 64), []);
    }

    #[test]
    fn flush_stub_sits_above_every_mapped_region() {
        assert_eq!(flush_stub_gpa(&[]), 0);
        assert_eq!(flush_stub_gpa(&[(0x4000_0000, 0x8000_0000)]), 0xc000_0000);
        assert_eq!(
            flush_stub_gpa(&[(0x4000_0000, 0x4000), (0x1000_0000, 0x1000_0000)]),
            0x4000_4000
        );
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
    fn id_register_fields_must_not_exceed_the_host() {
        let host_mmfr0 = 0x0000_1000_0f10_0002;
        assert!(id_register_within_host(0xc038, host_mmfr0, host_mmfr0));
        assert!(!id_register_within_host(
            0xc038,
            host_mmfr0 | (2 << 4),
            host_mmfr0
        ));
        assert!(!id_register_within_host(0xc031, 1 << 8, 0));
        assert!(id_register_within_host(0xc031, 0, 1 << 8));
    }

    #[test]
    fn signed_id_fields_treat_all_ones_as_absent() {
        assert!(id_register_within_host(0xc028, 0xf << 36, 0));
        assert!(!id_register_within_host(0xc028, 0, 0xf << 36));
        assert!(!id_register_within_host(0xc031, 0xf << 36, 0));
    }

    #[test]
    fn hypervisor_and_emulated_id_fields_are_not_compared() {
        assert!(id_register_within_host(0xc020, 1 << 24, 0));
        assert!(id_register_within_host(0xc038, 0x111 << 32, 0));
        assert!(id_register_within_host(0xc039, 0x21 << 4, 0));
        assert!(id_register_within_host(0xc03a, (2 << 56) | (1 << 40), 0));
        assert!(id_register_within_host(0xc000, u64::MAX, 0));
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
