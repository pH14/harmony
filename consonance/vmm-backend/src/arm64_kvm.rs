// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::arch::arm64::{
    Arm64, Arm64AsidBits, Arm64GicState, Arm64VcpuState, GicIntId, canonicalize_core_regs,
    has_noncanonical_core_regs,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts, StoreCompletions};
use crate::types::{Gpa, MpState};

pub(crate) const KVM_EXIT_MMIO: u32 = 6;
pub(crate) const KVM_EXIT_SYSTEM_EVENT: u32 = 24;
pub(crate) const KVM_EXIT_INTR: u32 = 10;
pub(crate) const KVM_EXIT_FAIL_ENTRY: u32 = 9;
pub(crate) const KVM_EXIT_INTERNAL_ERROR: u32 = 17;
pub(crate) const KVM_EXIT_HYPERCALL: u32 = 3;

pub(crate) const KVM_EXIT_ARM_WFX_PLACEHOLDER: u32 = 0xA001;
pub(crate) const KVM_EXIT_ARM_SYSREG_PLACEHOLDER: u32 = 0xA002;

pub(crate) const KVM_SYSTEM_EVENT_SHUTDOWN: u32 = 1;
pub(crate) const KVM_SYSTEM_EVENT_RESET: u32 = 2;
pub(crate) const KVM_SYSTEM_EVENT_CRASH: u32 = 3;

#[cfg(any(
    all(target_os = "linux", target_arch = "aarch64"),
    test,
    feature = "mock"
))]
pub(crate) const KVM_ARM_VCPU_PSCI_0_2: u32 = 2;

pub(crate) const KVM_DEV_ARM_VGIC_GRP_DIST_REGS: u32 = 1;
pub(crate) const KVM_DEV_ARM_VGIC_GRP_REDIST_REGS: u32 = 5;
pub(crate) const KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS: u32 = 6;
pub(crate) const KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO: u32 = 7;
const GICR_SGI_BASE: u64 = 0x1_0000;
const GICD_CTLR: u64 = 0x0000;
const GIC_IGROUPR: u64 = 0x0080;
const GIC_ISENABLER: u64 = 0x0100;
const GIC_ICENABLER: u64 = 0x0180;
const GIC_ISPENDR: u64 = 0x0200;
const GIC_ISACTIVER: u64 = 0x0300;
const GIC_ICACTIVER: u64 = 0x0380;
const GIC_IPRIORITYR: u64 = 0x0400;
const GIC_ICFGR: u64 = 0x0c00;
const GIC_IROUTER: u64 = 0x6000;
const GICR_ISACTIVER0: u64 = GICR_SGI_BASE + 0x0300;
const VGIC_LEVEL_INFO_LINE_LEVEL: u64 = 0;
const HARMONY_GIC_IMPL_SPIS: u32 = 64;
pub(crate) const HARMONY_GIC_NR_IRQS: u32 = 32 + HARMONY_GIC_IMPL_SPIS;
const HARMONY_TIMER_HZ: u64 = 62_500_000;
const HARMONY_TIMER_INTID: u32 = 27;
const GIC_STATE_VERSION: u32 = 5;

pub(crate) const KVM_MEM_LOG_DIRTY_PAGES: u32 = 1;
const KVM_PAGE_SIZE: u64 = 4096;
const KVM_DIRTY_BITMAP_WORD_PAGES: u64 = 64;

const fn vgic_sysreg(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
    op0 << 14 | op1 << 11 | crn << 7 | crm << 3 | op2
}

const ICC_PMR_EL1: u64 = vgic_sysreg(3, 0, 4, 6, 0);
const ICC_BPR0_EL1: u64 = vgic_sysreg(3, 0, 12, 8, 3);
const ICC_AP0R0_EL1: u64 = vgic_sysreg(3, 0, 12, 8, 4);
const ICC_AP1R0_EL1: u64 = vgic_sysreg(3, 0, 12, 9, 0);
const ICC_BPR1_EL1: u64 = vgic_sysreg(3, 0, 12, 12, 3);
const ICC_CTLR_EL1: u64 = vgic_sysreg(3, 0, 12, 12, 4);
const ICC_IGRPEN0_EL1: u64 = vgic_sysreg(3, 0, 12, 12, 6);
const ICC_IGRPEN1_EL1: u64 = vgic_sysreg(3, 0, 12, 12, 7);
const ICC_CTLR_GUEST_WRITABLE: u64 = 0b11;
const ICC_CTLR_PRIBITS_SHIFT: u32 = 8;

fn vgic_apr_count(ctlr: u64) -> Result<usize> {
    match ((ctlr >> ICC_CTLR_PRIBITS_SHIFT) & 0b111) + 1 {
        4 | 5 => Ok(1),
        6 => Ok(2),
        7 => Ok(4),
        8 => Err(BackendError::Unsupported {
            what: "KVM VGIC with 8 priority bits cannot expose all APR registers",
        }),
        _ => Err(BackendError::InvalidState),
    }
}

fn vgic_cpu_sysreg<K: Arm64Kvm + ?Sized>(k: &K, reg: u64) -> Result<u64> {
    k.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, reg, true)
}

#[cfg(any(
    all(target_os = "linux", target_arch = "aarch64"),
    test,
    feature = "mock"
))]
pub(crate) fn vcpu_init_features() -> [u32; 7] {
    let mut features = [0u32; 7];
    features[0] = 1 << KVM_ARM_VCPU_PSCI_0_2;
    features
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MmioView {
    pub phys_addr: u64,
    pub data: [u8; 8],
    pub len: u32,
    pub is_write: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KvmRunView {
    pub exit_reason: u32,
    pub mmio: MmioView,
    pub system_event_type: u32,
    pub hypercall_args: [u64; 4],
    pub sysreg: (u32, Option<u64>),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pending {
    None,
    MmioLoad { len: u32 },
    SysregRead,
    SysregWrite,
}

pub(crate) fn decode_exit(view: &KvmRunView) -> Result<Option<(Exit<Arm64>, Pending)>> {
    match view.exit_reason {
        KVM_EXIT_MMIO => {
            let m = &view.mmio;
            if !matches!(m.len, 1 | 2 | 4 | 8) {
                return Err(BackendError::Internal(
                    "KVM_EXIT_MMIO with a non-architectural access width (not 1/2/4/8)",
                ));
            }
            let gpa = Gpa(m.phys_addr);
            if m.is_write {
                let value = le_value(&m.data, m.len);
                Ok(Some((
                    CommonExit::Mmio {
                        gpa,
                        size: m.len as u8,
                        write: Some(value),
                    }
                    .into(),
                    Pending::None,
                )))
            } else {
                Ok(Some((
                    CommonExit::Mmio {
                        gpa,
                        size: m.len as u8,
                        write: None,
                    }
                    .into(),
                    Pending::MmioLoad { len: m.len },
                )))
            }
        }
        KVM_EXIT_SYSTEM_EVENT => match view.system_event_type {
            KVM_SYSTEM_EVENT_SHUTDOWN | KVM_SYSTEM_EVENT_RESET | KVM_SYSTEM_EVENT_CRASH => {
                Ok(Some((CommonExit::Shutdown.into(), Pending::None)))
            }
            _ => Err(BackendError::Internal(
                "KVM_EXIT_SYSTEM_EVENT with an unmodeled type",
            )),
        },

        KVM_EXIT_INTR => Ok(None),

        KVM_EXIT_FAIL_ENTRY => Err(BackendError::Internal("KVM_EXIT_FAIL_ENTRY")),
        KVM_EXIT_INTERNAL_ERROR => Err(BackendError::Internal("KVM_EXIT_INTERNAL_ERROR")),

        KVM_EXIT_ARM_WFX_PLACEHOLDER => Ok(Some((CommonExit::Idle.into(), Pending::None))),
        KVM_EXIT_HYPERCALL => Ok(Some((
            CommonExit::Hypercall(crate::exit::HypercallFrame {
                args: view.hypercall_args,
            })
            .into(),
            Pending::None,
        ))),
        KVM_EXIT_ARM_SYSREG_PLACEHOLDER => {
            let (sysreg, write) = view.sysreg;
            let pending = if write.is_some() {
                Pending::SysregWrite
            } else {
                Pending::SysregRead
            };
            Ok(Some((
                Exit::Arch(crate::arch::arm64::Arm64Exit::Sysreg { sysreg, write }),
                pending,
            )))
        }

        _ => Err(BackendError::Internal("unhandled KVM/arm64 exit reason")),
    }
}

fn le_value(data: &[u8; 8], len: u32) -> u64 {
    let mut buf = [0u8; 8];
    let n = (len as usize).min(8);
    buf[..n].copy_from_slice(&data[..n]);
    u64::from_le_bytes(buf)
}

fn le_data(value: u64, len: u32) -> [u8; 8] {
    let mut data = value.to_le_bytes();
    for b in data.iter_mut().skip((len as usize).min(8)) {
        *b = 0;
    }
    data
}

pub(crate) fn dirty_bitmap_words(size: u64) -> Result<usize> {
    if size == 0 || !size.is_multiple_of(KVM_PAGE_SIZE) {
        return Err(BackendError::InvalidState);
    }
    let pages = size / KVM_PAGE_SIZE;
    let words = pages.checked_add(63).ok_or(BackendError::InvalidState)? / 64;
    usize::try_from(words).map_err(|_| BackendError::InvalidState)
}

#[cfg(any(test, all(target_os = "linux", target_arch = "aarch64")))]
pub(crate) fn require_4k_host_page_size(page_size: i64) -> Result<()> {
    if page_size != KVM_PAGE_SIZE as i64 {
        return Err(BackendError::Capability {
            cap: "4 KiB host page size (_SC_PAGESIZE == 4096)",
        });
    }
    Ok(())
}

fn dirty_clear_max_words(max_pages: u64) -> Result<usize> {
    usize::try_from(max_pages / KVM_DIRTY_BITMAP_WORD_PAGES).map_err(|_| BackendError::InvalidState)
}

pub(crate) fn validate_clear_dirty_log(
    slot_size: u64,
    first_page: u64,
    num_pages: u32,
    bitmap: &[u64],
) -> Result<()> {
    let _ = dirty_bitmap_words(slot_size)?;
    let slot_pages = slot_size / KVM_PAGE_SIZE;
    if num_pages == 0 || !first_page.is_multiple_of(KVM_DIRTY_BITMAP_WORD_PAGES) {
        return Err(BackendError::InvalidState);
    }
    let num_pages = u64::from(num_pages);
    let end_page = first_page
        .checked_add(num_pages)
        .ok_or(BackendError::InvalidState)?;
    if first_page >= slot_pages || end_page > slot_pages {
        return Err(BackendError::InvalidState);
    }
    let expected_words = usize::try_from(
        num_pages
            .checked_add(KVM_DIRTY_BITMAP_WORD_PAGES - 1)
            .ok_or(BackendError::InvalidState)?
            / KVM_DIRTY_BITMAP_WORD_PAGES,
    )
    .map_err(|_| BackendError::InvalidState)?;
    if bitmap.len() != expected_words {
        return Err(BackendError::Internal(
            "KVM clear dirty bitmap has an unexpected size",
        ));
    }
    if !num_pages.is_multiple_of(KVM_DIRTY_BITMAP_WORD_PAGES) && end_page != slot_pages {
        return Err(BackendError::InvalidState);
    }
    Ok(())
}

fn decode_dirty_bitmap(
    slot_gpa: u64,
    slot_size: u64,
    bitmap: &[u64],
    out: &mut Vec<u64>,
) -> Result<()> {
    if !slot_gpa.is_multiple_of(KVM_PAGE_SIZE) {
        return Err(BackendError::InvalidState);
    }
    let words = dirty_bitmap_words(slot_size)?;
    if bitmap.len() != words {
        return Err(BackendError::Internal(
            "KVM dirty bitmap has an unexpected size",
        ));
    }
    let slot_pages = slot_size / KVM_PAGE_SIZE;
    let base_gfn = slot_gpa / KVM_PAGE_SIZE;
    for (word_idx, &word) in bitmap.iter().enumerate() {
        let word_idx = u64::try_from(word_idx).map_err(|_| BackendError::InvalidState)?;
        let word_base = word_idx.checked_mul(64).ok_or(BackendError::InvalidState)?;
        let mut bits = word;
        for _ in 0..word.count_ones() {
            if bits == 0 {
                break;
            }
            let bit = u64::from(bits.trailing_zeros());
            bits &= bits - 1;
            let page = word_base
                .checked_add(bit)
                .ok_or(BackendError::InvalidState)?;
            if page < slot_pages {
                let gfn = base_gfn
                    .checked_add(page)
                    .ok_or(BackendError::InvalidState)?;
                out.push(gfn);
            }
        }
    }
    Ok(())
}

#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "aarch64"))),
    allow(dead_code)
)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunOffsets {
    pub(crate) exit_reason: usize,
    pub(crate) mmio_phys_addr: usize,
    pub(crate) mmio_data: usize,
    pub(crate) mmio_len: usize,
    pub(crate) mmio_is_write: usize,
    pub(crate) system_event_type: usize,
}

#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "aarch64"))),
    allow(dead_code)
)]
pub(crate) struct RunPage {
    ptr: *mut u8,
    len: usize,
}

#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "aarch64"))),
    allow(dead_code)
)]
impl RunPage {
    /// # Safety
    /// `ptr` must point to at least `len` initialized bytes (the live `mmap`,
    /// or a test buffer), exclusively owned for the duration of use.
    pub(crate) unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    fn check(&self, off: usize, n: usize) -> Result<()> {
        match off.checked_add(n) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(BackendError::Internal("kvm_run field access out of bounds")),
        }
    }

    /// # Safety
    /// The constructor contract (a valid page of `len` bytes) must hold.
    unsafe fn read_array<const N: usize>(&self, off: usize) -> Result<[u8; N]> {
        self.check(off, N)?;
        let mut b = [0u8; N];
        // SAFETY: `off + N <= len` (checked); `ptr` is valid for `len` bytes and
        // `b` is a distinct local, so the copy is in-bounds and non-overlapping.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr.add(off), b.as_mut_ptr(), N) };
        Ok(b)
    }

    /// # Safety
    /// The constructor contract must hold; `off` must name fields of the mapped
    /// `kvm_run` (the box computes them via `offset_of!`).
    pub(crate) unsafe fn view(&self, off: &RunOffsets) -> Result<KvmRunView> {
        // SAFETY: forwarded to the constructor contract; every read is bounds-
        // checked. `mmio`/`system_event` overlap in the exit-info union; the
        // decode consults only the fields the exit reason selects.
        unsafe {
            Ok(KvmRunView {
                exit_reason: u32::from_le_bytes(self.read_array(off.exit_reason)?),
                mmio: MmioView {
                    phys_addr: u64::from_le_bytes(self.read_array(off.mmio_phys_addr)?),
                    data: self.read_array(off.mmio_data)?,
                    len: u32::from_le_bytes(self.read_array(off.mmio_len)?),
                    is_write: self.read_array::<1>(off.mmio_is_write)?[0] != 0,
                },
                system_event_type: u32::from_le_bytes(self.read_array(off.system_event_type)?),
                ..Default::default()
            })
        }
    }

    /// # Safety
    /// The constructor contract must hold; `off.mmio_data` must name the mapped
    /// `mmio.data`.
    pub(crate) unsafe fn write_mmio_data(&self, off: &RunOffsets, data: [u8; 8]) -> Result<()> {
        self.check(off.mmio_data, 8)?;
        // SAFETY: `mmio_data + 8 <= len` (checked); exclusive access during the
        // completion; `data` is a distinct local, so the copy is non-overlapping.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.add(off.mmio_data), 8) };
        Ok(())
    }
}

const KVM_REG_ARM_COPROC_SHIFT: u64 = 16;
pub(crate) const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
const KVM_REG_SIZE_U32: u64 = 0x0020_0000_0000_0000;
pub(crate) const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
const KVM_REG_SIZE_U128: u64 = 0x0040_0000_0000_0000;
pub(crate) const KVM_REG_ARM_CORE: u64 = 0x0010 << KVM_REG_ARM_COPROC_SHIFT;
pub(crate) const KVM_REG_ARM64_SYSREG: u64 = 0x0013 << KVM_REG_ARM_COPROC_SHIFT;
pub(crate) const KVM_REG_ARM_FW: u64 = 0x0014 << KVM_REG_ARM_COPROC_SHIFT;
const KVM_REG_ARM_PSCI_VERSION: u64 = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_FW;
const KVM_ARM_PSCI_1_0: u64 = 0x0001_0000;
const KVM_REG_ARM_FW_FEAT_BMAP: u64 = 0x0016 << KVM_REG_ARM_COPROC_SHIFT;
const KVM_REG_ARM_STD_BMAP: u64 = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_FW_FEAT_BMAP;
const KVM_REG_ARM_STD_HYP_BMAP: u64 = KVM_REG_ARM_STD_BMAP | 1;
const KVM_REG_ARM_VENDOR_HYP_BMAP: u64 = KVM_REG_ARM_STD_BMAP | 2;
const KVM_REG_ARM_VENDOR_HYP_BMAP_2: u64 = KVM_REG_ARM_STD_BMAP | 3;
const OPTIONAL_FIRMWARE_BITMAPS: [u64; 4] = [
    KVM_REG_ARM_STD_BMAP,
    KVM_REG_ARM_STD_HYP_BMAP,
    KVM_REG_ARM_VENDOR_HYP_BMAP,
    KVM_REG_ARM_VENDOR_HYP_BMAP_2,
];
const KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_1: u64 = KVM_REG_ARM_PSCI_VERSION | 1;
const KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_2: u64 = KVM_REG_ARM_PSCI_VERSION | 2;
const KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_3: u64 = KVM_REG_ARM_PSCI_VERSION | 3;
const SMCCC_WORKAROUND_REGISTERS: [u64; 3] = [
    KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_1,
    KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_2,
    KVM_REG_ARM_SMCCC_ARCH_WORKAROUND_3,
];

const fn core_reg(index: u64) -> u64 {
    KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE | index
}

const fn core_reg_sized(size: u64, index: u64) -> u64 {
    KVM_REG_ARM64 | size | KVM_REG_ARM_CORE | index
}

const fn sysreg_id(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
    KVM_REG_ARM64
        | KVM_REG_SIZE_U64
        | KVM_REG_ARM64_SYSREG
        | (op0 << 14 | op1 << 11 | crn << 7 | crm << 3 | op2)
}

const CORE_SP: u64 = 62;
const CORE_PC: u64 = 64;
const CORE_PSTATE: u64 = 66;
const PSTATE_I: u64 = 1 << 7;
const CORE_SP_EL1: u64 = 68;
const CORE_ELR_EL1: u64 = 70;
const CORE_SPSR_EL1: u64 = 72;
const CORE_FP_BASE: u64 = 84;
const CORE_FPSR: u64 = 212;
const CORE_FPCR: u64 = 213;

const CNTV_CTL_EL0: u64 = sysreg_id(3, 3, 14, 3, 1);
const CNTV_CVAL_EL0: u64 = sysreg_id(3, 3, 14, 0, 2);
const CNTVCT_EL0: u64 = sysreg_id(3, 3, 14, 3, 2);
const MDSCR_EL1: u64 = sysreg_id(2, 0, 0, 2, 2);

const CNTV_CTL_WRITABLE_BITS: u64 = 0b11;

const fn dbgbvr(index: u64) -> u64 {
    sysreg_id(2, 0, 0, index, 4)
}

const fn dbgbcr(index: u64) -> u64 {
    sysreg_id(2, 0, 0, index, 5)
}

const fn dbgwvr(index: u64) -> u64 {
    sysreg_id(2, 0, 0, index, 6)
}

const fn dbgwcr(index: u64) -> u64 {
    sysreg_id(2, 0, 0, index, 7)
}

#[derive(Clone, Copy)]
enum SysSel {
    Sctlr,
    Ttbr0,
    Ttbr1,
    Tcr,
    Mair,
    Vbar,
    Cpacr,
    Esr,
    Far,
    TpidrEl0,
    TpidrroEl0,
    TpidrEl1,
    Cntkctl,
    Contextidr,
    Amair,
    Par,
    Csselr,
    Afsr0,
    Afsr1,
}

const SYSREGS: &[(u64, SysSel)] = &[
    (sysreg_id(3, 0, 1, 0, 0), SysSel::Sctlr),
    (sysreg_id(3, 0, 2, 0, 0), SysSel::Ttbr0),
    (sysreg_id(3, 0, 2, 0, 1), SysSel::Ttbr1),
    (sysreg_id(3, 0, 2, 0, 2), SysSel::Tcr),
    (sysreg_id(3, 0, 10, 2, 0), SysSel::Mair),
    (sysreg_id(3, 0, 12, 0, 0), SysSel::Vbar),
    (sysreg_id(3, 0, 1, 0, 2), SysSel::Cpacr),
    (sysreg_id(3, 0, 5, 2, 0), SysSel::Esr),
    (sysreg_id(3, 0, 6, 0, 0), SysSel::Far),
    (sysreg_id(3, 3, 13, 0, 2), SysSel::TpidrEl0),
    (sysreg_id(3, 3, 13, 0, 3), SysSel::TpidrroEl0),
    (sysreg_id(3, 0, 13, 0, 4), SysSel::TpidrEl1),
    (sysreg_id(3, 0, 14, 1, 0), SysSel::Cntkctl),
    (sysreg_id(3, 0, 13, 0, 1), SysSel::Contextidr),
    (sysreg_id(3, 0, 10, 3, 0), SysSel::Amair),
    (sysreg_id(3, 0, 7, 4, 0), SysSel::Par),
    (sysreg_id(3, 2, 0, 0, 0), SysSel::Csselr),
    (sysreg_id(3, 0, 5, 1, 0), SysSel::Afsr0),
    (sysreg_id(3, 0, 5, 1, 1), SysSel::Afsr1),
];

fn sys_field(f: &mut crate::arch::arm64::Arm64SysregFile, sel: SysSel) -> &mut u64 {
    match sel {
        SysSel::Sctlr => &mut f.sctlr_el1,
        SysSel::Ttbr0 => &mut f.ttbr0_el1,
        SysSel::Ttbr1 => &mut f.ttbr1_el1,
        SysSel::Tcr => &mut f.tcr_el1,
        SysSel::Mair => &mut f.mair_el1,
        SysSel::Vbar => &mut f.vbar_el1,
        SysSel::Cpacr => &mut f.cpacr_el1,
        SysSel::Esr => &mut f.esr_el1,
        SysSel::Far => &mut f.far_el1,
        SysSel::TpidrEl0 => &mut f.tpidr_el0,
        SysSel::TpidrroEl0 => &mut f.tpidrro_el0,
        SysSel::TpidrEl1 => &mut f.tpidr_el1,
        SysSel::Cntkctl => &mut f.cntkctl_el1,
        SysSel::Contextidr => &mut f.contextidr_el1,
        SysSel::Amair => &mut f.amair_el1,
        SysSel::Par => &mut f.par_el1,
        SysSel::Csselr => &mut f.csselr_el1,
        SysSel::Afsr0 => &mut f.afsr0_el1,
        SysSel::Afsr1 => &mut f.afsr1_el1,
    }
}

fn sys_value(f: &crate::arch::arm64::Arm64SysregFile, sel: SysSel) -> u64 {
    match sel {
        SysSel::Sctlr => f.sctlr_el1,
        SysSel::Ttbr0 => f.ttbr0_el1,
        SysSel::Ttbr1 => f.ttbr1_el1,
        SysSel::Tcr => f.tcr_el1,
        SysSel::Mair => f.mair_el1,
        SysSel::Vbar => f.vbar_el1,
        SysSel::Cpacr => f.cpacr_el1,
        SysSel::Esr => f.esr_el1,
        SysSel::Far => f.far_el1,
        SysSel::TpidrEl0 => f.tpidr_el0,
        SysSel::TpidrroEl0 => f.tpidrro_el0,
        SysSel::TpidrEl1 => f.tpidr_el1,
        SysSel::Cntkctl => f.cntkctl_el1,
        SysSel::Contextidr => f.contextidr_el1,
        SysSel::Amair => f.amair_el1,
        SysSel::Par => f.par_el1,
        SysSel::Csselr => f.csselr_el1,
        SysSel::Afsr0 => f.afsr0_el1,
        SysSel::Afsr1 => f.afsr1_el1,
    }
}

fn vgic_bitmap_attr(word: usize, base: u64) -> (u32, u64) {
    if word == 0 {
        (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, GICR_SGI_BASE + base)
    } else {
        (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, base + (word as u64) * 4)
    }
}

fn vgic_priority_attr(word: usize) -> (u32, u64) {
    if word < 8 {
        (
            KVM_DEV_ARM_VGIC_GRP_REDIST_REGS,
            GICR_SGI_BASE + GIC_IPRIORITYR + (word as u64) * 4,
        )
    } else {
        (
            KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
            GIC_IPRIORITYR + (word as u64) * 4,
        )
    }
}

fn vgic_config_attr(word: usize) -> (u32, u64) {
    if word < 2 {
        (
            KVM_DEV_ARM_VGIC_GRP_REDIST_REGS,
            GICR_SGI_BASE + GIC_ICFGR + (word as u64) * 4,
        )
    } else {
        (
            KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
            GIC_ICFGR + (word as u64) * 4,
        )
    }
}

fn vgic_router_attr(spi: usize, half: usize) -> u64 {
    GIC_IROUTER + ((32 + spi) as u64) * 8 + (half as u64) * 4
}

fn require_zero_vgic_routes<K: Arm64Kvm + ?Sized>(k: &K) -> Result<()> {
    for spi in 0..HARMONY_GIC_IMPL_SPIS as usize {
        for half in 0..2 {
            if k.get_vgic_attr(
                KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                vgic_router_attr(spi, half),
                false,
            )? != 0
            {
                return Err(BackendError::Unsupported {
                    what: "vGIC with nonzero SPI routing affinity",
                });
            }
        }
    }
    Ok(())
}

fn clear_vgic_routes<K: Arm64Kvm + ?Sized>(k: &mut K) -> Result<()> {
    for spi in 0..HARMONY_GIC_IMPL_SPIS as usize {
        for half in 0..2 {
            k.set_vgic_attr(
                KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                vgic_router_attr(spi, half),
                false,
                0,
            )?;
        }
    }
    require_zero_vgic_routes(k)
}

fn vgic_level_attr(block: u32) -> u64 {
    VGIC_LEVEL_INFO_LINE_LEVEL + u64::from(block)
}

fn has_pending_or_asserted_spi(s: &Arm64GicState) -> bool {
    let words = HARMONY_GIC_NR_IRQS as usize / 32;
    s.pending[1..words]
        .iter()
        .chain(s.line_level[1..words].iter())
        .any(|&word| word != 0)
}

fn read_vgic_state<K: Arm64Kvm + ?Sized>(k: &K) -> Result<Arm64GicState> {
    require_zero_vgic_routes(k)?;
    let mut s = Arm64GicState {
        impl_spis: HARMONY_GIC_IMPL_SPIS,
        timer_hz: HARMONY_TIMER_HZ,
        timer_intid: HARMONY_TIMER_INTID,
        ..Arm64GicState::default()
    };
    s.gicd_ctlr =
        u32::try_from(k.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_DIST_REGS, GICD_CTLR, false)? & 0b10)
            .map_err(|_| BackendError::InvalidState)?;
    s.ctlr = vgic_cpu_sysreg(k, ICC_CTLR_EL1)?;
    if s.ctlr & 1 != 0 {
        return Err(BackendError::Unsupported {
            what: "save vGIC with ICC_CTLR_EL1.CBPR set",
        });
    }
    let apr_count = vgic_apr_count(s.ctlr)?;

    let words = (HARMONY_GIC_NR_IRQS / 32) as usize;
    for word in 0..words {
        for (base, file) in [
            (GIC_IGROUPR, &mut s.group),
            (GIC_ISENABLER, &mut s.enable),
            (GIC_ISPENDR, &mut s.pending),
            (GIC_ISACTIVER, &mut s.active),
        ] {
            let (group, attr) = vgic_bitmap_attr(word, base);
            file[word] = u32::try_from(k.get_vgic_attr(group, attr, false)?)
                .map_err(|_| BackendError::InvalidState)?;
        }
        s.line_level[word] = u32::try_from(k.get_vgic_attr(
            KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
            vgic_level_attr(word as u32 * 32),
            false,
        )?)
        .map_err(|_| BackendError::InvalidState)?;
    }
    for word in 0..(HARMONY_GIC_NR_IRQS as usize / 4) {
        let (group, attr) = vgic_priority_attr(word);
        let bytes = u32::try_from(k.get_vgic_attr(group, attr, false)?)
            .map_err(|_| BackendError::InvalidState)?
            .to_le_bytes();
        s.priority[word * 4..word * 4 + 4].copy_from_slice(&bytes);
    }
    for word in 0..(HARMONY_GIC_NR_IRQS as usize / 16) {
        let (group, attr) = vgic_config_attr(word);
        s.config[word] = u32::try_from(k.get_vgic_attr(group, attr, false)?)
            .map_err(|_| BackendError::InvalidState)?;
    }
    s.pmr =
        u8::try_from(vgic_cpu_sysreg(k, ICC_PMR_EL1)?).map_err(|_| BackendError::InvalidState)?;
    s.bpr0 =
        u8::try_from(vgic_cpu_sysreg(k, ICC_BPR0_EL1)?).map_err(|_| BackendError::InvalidState)?;
    s.bpr1 =
        u8::try_from(vgic_cpu_sysreg(k, ICC_BPR1_EL1)?).map_err(|_| BackendError::InvalidState)?;
    s.igrpen0 = match vgic_cpu_sysreg(k, ICC_IGRPEN0_EL1)? {
        0 => false,
        1 => true,
        _ => return Err(BackendError::InvalidState),
    };
    s.igrpen1 = match vgic_cpu_sysreg(k, ICC_IGRPEN1_EL1)? {
        0 => false,
        1 => true,
        _ => return Err(BackendError::InvalidState),
    };
    for index in 0..apr_count {
        s.ap0r[index] = u32::try_from(vgic_cpu_sysreg(k, ICC_AP0R0_EL1 + index as u64)?)
            .map_err(|_| BackendError::InvalidState)?;
        s.ap1r[index] = u32::try_from(vgic_cpu_sysreg(k, ICC_AP1R0_EL1 + index as u64)?)
            .map_err(|_| BackendError::InvalidState)?;
    }
    Ok(s)
}

fn save_vgic<K: Arm64Kvm + ?Sized>(k: &K) -> Result<Arm64GicState> {
    let s = read_vgic_state(k)?;
    if has_pending_or_asserted_spi(&s) {
        return Err(BackendError::Unsupported {
            what: "save vGIC with pending or asserted SPI",
        });
    }
    Ok(s)
}

fn validate_vgic_state(s: &Arm64GicState) -> Result<()> {
    if s.version != GIC_STATE_VERSION
        || s.impl_spis != HARMONY_GIC_IMPL_SPIS
        || s.timer_hz != HARMONY_TIMER_HZ
        || s.timer_intid != HARMONY_TIMER_INTID
        || s.gicd_ctlr & !0b10 != 0
        || s.cntv_ctl != 0
        || s.cntv_cval != 0
        || s.timer_fired
        || s.ctlr & 1 != 0
        || s.bpr0 > 7
        || s.bpr1 > 7
        || has_pending_or_asserted_spi(s)
    {
        return Err(BackendError::InvalidState);
    }
    let apr_count = vgic_apr_count(s.ctlr)?;
    if s.ap0r[apr_count..].iter().any(|&value| value != 0)
        || s.ap1r[apr_count..].iter().any(|&value| value != 0)
    {
        return Err(BackendError::InvalidState);
    }
    let words = (HARMONY_GIC_NR_IRQS / 32) as usize;
    for file in [&s.group, &s.enable, &s.pending, &s.active, &s.line_level] {
        if file[words..].iter().any(|&word| word != 0) {
            return Err(BackendError::InvalidState);
        }
    }
    if s.priority[HARMONY_GIC_NR_IRQS as usize..]
        .iter()
        .any(|&byte| byte != 0)
    {
        return Err(BackendError::InvalidState);
    }
    let config_words = HARMONY_GIC_NR_IRQS as usize / 16;
    if s.config[..config_words]
        .iter()
        .any(|&word| word & !0xaaaa_aaaa != 0)
        || s.config[config_words..].iter().any(|&word| word != 0)
    {
        return Err(BackendError::InvalidState);
    }
    Ok(())
}

fn validate_vgic_host<K: Arm64Kvm + ?Sized>(k: &K, s: &Arm64GicState) -> Result<()> {
    validate_vgic_state(s)?;
    let fixed = vgic_cpu_sysreg(k, ICC_CTLR_EL1)? & !ICC_CTLR_GUEST_WRITABLE;
    if s.ctlr & !ICC_CTLR_GUEST_WRITABLE != fixed {
        return Err(BackendError::InvalidState);
    }
    for word in 0..2 {
        let (group, attr) = vgic_config_attr(word);
        if u64::from(s.config[word]) != k.get_vgic_attr(group, attr, false)? {
            return Err(BackendError::InvalidState);
        }
    }
    Ok(())
}

pub(crate) fn validate_restore_vcpu_state(s: &Arm64VcpuState) -> Result<()> {
    if has_noncanonical_core_regs(&s.core)
        || s.debug.trap_debug_exceptions
        || s.debug.trap_debug_reg_accesses
        || !s.vtimer.masked
        || s.vtimer.cntv_ctl_el0 & !CNTV_CTL_WRITABLE_BITS != 0
        || s.vtimer.cntv_ctl_el0 & 1 != 0
        || s.interrupts.irq
        || s.interrupts.fiq
        || u32::try_from(s.simd_fp.fpsr).is_err()
        || u32::try_from(s.simd_fp.fpcr).is_err()
    {
        return Err(BackendError::InvalidState);
    }
    validate_vgic_state(s.gic.as_ref().ok_or(BackendError::InvalidState)?)
}

fn restore_vgic<K: Arm64Kvm + ?Sized>(k: &mut K, s: &Arm64GicState) -> Result<()> {
    validate_vgic_host(k, s)?;
    clear_vgic_routes(k)?;
    for word in 2..(HARMONY_GIC_NR_IRQS as usize / 16) {
        let (group, attr) = vgic_config_attr(word);
        k.set_vgic_attr(group, attr, false, u64::from(s.config[word]))?;
    }
    let words = (HARMONY_GIC_NR_IRQS / 32) as usize;
    for word in 0..words {
        let (group, attr) = vgic_bitmap_attr(word, GIC_IGROUPR);
        k.set_vgic_attr(group, attr, false, u64::from(s.group[word]))?;

        let (group, clear) = vgic_bitmap_attr(word, GIC_ICENABLER);
        k.set_vgic_attr(group, clear, false, u64::from(u32::MAX))?;
        let (group, set) = vgic_bitmap_attr(word, GIC_ISENABLER);
        k.set_vgic_attr(group, set, false, u64::from(s.enable[word]))?;

        let (group, attr) = vgic_bitmap_attr(word, GIC_ISPENDR);
        k.set_vgic_attr(group, attr, false, u64::from(s.pending[word]))?;

        let (group, clear) = vgic_bitmap_attr(word, GIC_ICACTIVER);
        k.set_vgic_attr(group, clear, false, u64::from(u32::MAX))?;
        let (group, set) = vgic_bitmap_attr(word, GIC_ISACTIVER);
        k.set_vgic_attr(group, set, false, u64::from(s.active[word]))?;

        k.set_vgic_attr(
            KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
            vgic_level_attr(word as u32 * 32),
            false,
            u64::from(s.line_level[word]),
        )?;
    }
    for word in 0..(HARMONY_GIC_NR_IRQS as usize / 4) {
        let (group, attr) = vgic_priority_attr(word);
        let first = word * 4;
        let value = u32::from_le_bytes([
            s.priority[first],
            s.priority[first + 1],
            s.priority[first + 2],
            s.priority[first + 3],
        ]);
        k.set_vgic_attr(group, attr, false, u64::from(value))?;
    }
    k.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, s.ctlr)?;
    for (reg, value) in [
        (ICC_BPR0_EL1, u64::from(s.bpr0)),
        (ICC_BPR1_EL1, u64::from(s.bpr1)),
        (ICC_IGRPEN0_EL1, u64::from(s.igrpen0)),
    ] {
        k.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, reg, true, value)?;
    }
    for index in 0..vgic_apr_count(s.ctlr)? {
        for (base, value) in [
            (ICC_AP0R0_EL1, s.ap0r[index]),
            (ICC_AP1R0_EL1, s.ap1r[index]),
        ] {
            k.set_vgic_attr(
                KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS,
                base + index as u64,
                true,
                u64::from(value),
            )?;
        }
    }
    k.set_vgic_attr(
        KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS,
        ICC_PMR_EL1,
        true,
        u64::from(s.pmr),
    )?;
    k.set_vgic_attr(
        KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS,
        ICC_IGRPEN1_EL1,
        true,
        u64::from(s.igrpen1),
    )?;
    k.set_vgic_attr(
        KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
        GICD_CTLR,
        false,
        u64::from(s.gicd_ctlr | (1 << 4)),
    )?;
    Ok(())
}

pub(crate) fn save_vcpu<K: Arm64Kvm + ?Sized>(k: &K) -> Result<Arm64VcpuState> {
    let mut s = Arm64VcpuState::default();
    for i in 0..31u64 {
        s.core.x[i as usize] = k.get_one_reg(core_reg(i * 2))?;
    }
    s.core.sp = k.get_one_reg(core_reg(CORE_SP))?;
    s.core.pc = k.get_one_reg(core_reg(CORE_PC))?;
    s.core.pstate = k.get_one_reg(core_reg(CORE_PSTATE))?;
    s.core.sp_el1 = k.get_one_reg(core_reg(CORE_SP_EL1))?;
    s.core.elr_el1 = k.get_one_reg(core_reg(CORE_ELR_EL1))?;
    s.core.spsr_el1 = k.get_one_reg(core_reg(CORE_SPSR_EL1))?;
    canonicalize_core_regs(&mut s.core);
    for &(id, sel) in SYSREGS {
        *sys_field(&mut s.sysregs, sel) = k.get_one_reg(id)?;
    }
    for (index, q) in s.simd_fp.q.iter_mut().enumerate() {
        *q = k.get_one_reg128(core_reg_sized(
            KVM_REG_SIZE_U128,
            CORE_FP_BASE + (index as u64) * 4,
        ))?;
    }
    s.simd_fp.fpsr = u64::from(k.get_one_reg32(core_reg_sized(KVM_REG_SIZE_U32, CORE_FPSR))?);
    s.simd_fp.fpcr = u64::from(k.get_one_reg32(core_reg_sized(KVM_REG_SIZE_U32, CORE_FPCR))?);
    for index in 0..16u64 {
        s.debug.breakpoint_value[index as usize] = k.get_one_reg(dbgbvr(index))?;
        s.debug.breakpoint_control[index as usize] = k.get_one_reg(dbgbcr(index))?;
        s.debug.watchpoint_value[index as usize] = k.get_one_reg(dbgwvr(index))?;
        s.debug.watchpoint_control[index as usize] = k.get_one_reg(dbgwcr(index))?;
    }
    s.debug.mdscr_el1 = k.get_one_reg(MDSCR_EL1)?;
    s.vtimer.cntv_ctl_el0 = k.get_one_reg(CNTV_CTL_EL0)? & CNTV_CTL_WRITABLE_BITS;
    if s.vtimer.cntv_ctl_el0 & 1 != 0 {
        return Err(BackendError::InvalidState);
    }
    s.vtimer.cntv_cval_el0 = k.get_one_reg(CNTV_CVAL_EL0)?;
    s.vtimer.counter = k.get_one_reg(CNTVCT_EL0)?;
    s.vtimer.masked = true;
    s.mp_state = k.get_mp_state()?;
    s.gic = Some(save_vgic(k)?);
    Ok(s)
}

pub(crate) fn restore_vcpu<K: Arm64Kvm + ?Sized>(k: &mut K, s: &Arm64VcpuState) -> Result<()> {
    validate_restore_vcpu_state(s)?;
    validate_vgic_host(k, s.gic.as_ref().ok_or(BackendError::InvalidState)?)?;
    for i in 0..31u64 {
        k.set_one_reg(core_reg(i * 2), s.core.x[i as usize])?;
    }
    k.set_one_reg(core_reg(CORE_SP), s.core.sp)?;
    k.set_one_reg(core_reg(CORE_PC), s.core.pc)?;
    k.set_one_reg(core_reg(CORE_PSTATE), s.core.pstate)?;
    k.set_one_reg(core_reg(CORE_SP_EL1), s.core.sp_el1)?;
    k.set_one_reg(core_reg(CORE_ELR_EL1), s.core.elr_el1)?;
    k.set_one_reg(core_reg(CORE_SPSR_EL1), s.core.spsr_el1)?;
    for &(id, sel) in SYSREGS {
        k.set_one_reg(id, sys_value(&s.sysregs, sel))?;
    }
    for (index, q) in s.simd_fp.q.iter().enumerate() {
        k.set_one_reg128(
            core_reg_sized(KVM_REG_SIZE_U128, CORE_FP_BASE + (index as u64) * 4),
            *q,
        )?;
    }
    k.set_one_reg32(
        core_reg_sized(KVM_REG_SIZE_U32, CORE_FPSR),
        u32::try_from(s.simd_fp.fpsr).map_err(|_| BackendError::InvalidState)?,
    )?;
    k.set_one_reg32(
        core_reg_sized(KVM_REG_SIZE_U32, CORE_FPCR),
        u32::try_from(s.simd_fp.fpcr).map_err(|_| BackendError::InvalidState)?,
    )?;
    for index in 0..16u64 {
        k.set_one_reg(dbgbvr(index), s.debug.breakpoint_value[index as usize])?;
        k.set_one_reg(dbgbcr(index), s.debug.breakpoint_control[index as usize])?;
        k.set_one_reg(dbgwvr(index), s.debug.watchpoint_value[index as usize])?;
        k.set_one_reg(dbgwcr(index), s.debug.watchpoint_control[index as usize])?;
    }
    k.set_one_reg(MDSCR_EL1, s.debug.mdscr_el1)?;
    k.set_one_reg(CNTVCT_EL0, s.vtimer.counter)?;
    k.set_one_reg(CNTV_CVAL_EL0, s.vtimer.cntv_cval_el0)?;
    k.set_one_reg(CNTV_CTL_EL0, s.vtimer.cntv_ctl_el0)?;
    k.set_mp_state(s.mp_state)?;
    let gic = s.gic.as_ref().ok_or(BackendError::InvalidState)?;
    restore_vgic(k, gic)?;
    Ok(())
}

pub trait Arm64Kvm {
    fn vcpu_init(&mut self) -> Result<()>;

    fn asid_bits(&self) -> Arm64AsidBits;

    /// # Safety
    /// `host` must point to `len` bytes of pinned, page-aligned backing that
    /// stays live and unaliased for the backend's lifetime (the
    /// [`Backend::map_memory`] contract). The fake ignores the pointer.
    unsafe fn set_user_memory_region(
        &mut self,
        slot: u32,
        gpa: u64,
        host: *mut u8,
        len: u64,
    ) -> Result<()>;

    /// # Safety
    /// The `host` pointer has the same pinned-backing lifetime requirements as
    /// [`Self::set_user_memory_region`].
    #[doc(hidden)]
    unsafe fn set_user_memory_region_with_flags(
        &mut self,
        slot: u32,
        gpa: u64,
        host: *mut u8,
        len: u64,
        _flags: u32,
    ) -> Result<()> {
        // SAFETY: this default forwards the caller's memory-registration
        // contract unchanged to the legacy seam method.
        unsafe { self.set_user_memory_region(slot, gpa, host, len) }
    }

    fn get_dirty_log(&mut self, slot: u32, size: u64) -> Result<Vec<u64>>;

    #[doc(hidden)]
    fn clear_dirty_log(
        &mut self,
        slot: u32,
        size: u64,
        first_page: u64,
        num_pages: u32,
        bitmap: &[u64],
    ) -> Result<()>;

    fn get_one_reg(&self, id: u64) -> Result<u64>;
    fn set_one_reg(&mut self, id: u64, value: u64) -> Result<()>;
    fn get_one_reg32(&self, id: u64) -> Result<u32>;
    fn set_one_reg32(&mut self, id: u64, value: u32) -> Result<()>;
    fn get_one_reg128(&self, id: u64) -> Result<[u8; 16]>;
    fn set_one_reg128(&mut self, id: u64, value: [u8; 16]) -> Result<()>;

    fn get_mp_state(&self) -> Result<MpState>;
    fn set_mp_state(&mut self, mp: MpState) -> Result<()>;

    fn set_irq_line(&mut self, id: GicIntId, level: bool) -> Result<()>;

    fn get_vgic_attr(&self, group: u32, attr: u64, width64: bool) -> Result<u64>;

    fn set_vgic_attr(&mut self, group: u32, attr: u64, width64: bool, value: u64) -> Result<()>;

    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()>;

    fn complete_mmio_exit(&mut self) -> Result<()>;

    fn run(&mut self) -> Result<KvmRunView>;

    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        None
    }
}

pub struct Arm64KvmBackend<K: Arm64Kvm> {
    kvm: K,
    configured: bool,
    pending: Pending,
    completion_staged: bool,
    regions: Vec<(u64, u64)>,
    dirty_slots: Vec<(u32, u64, u64)>,
    dirty_log: bool,
    unlogged_slot: bool,
    dirty_log_poisoned: bool,
    pending_irq: Option<GicIntId>,
    applied_irq: Option<GicIntId>,
    accepted_irq: Option<GicIntId>,
    reported_active_irq: Option<GicIntId>,
    counts: ExitCounts,
    completions: StoreCompletions,
    mmio_owed: bool,
    smccc_workaround_levels: [u64; 3],
    gic_ctlr_fixed_bits: u64,
}

impl<K: Arm64Kvm> Arm64KvmBackend<K> {
    pub fn new(kvm: K) -> Self {
        Self {
            kvm,
            configured: false,
            pending: Pending::None,
            completion_staged: false,
            regions: Vec::new(),
            dirty_slots: Vec::new(),
            dirty_log: true,
            unlogged_slot: false,
            dirty_log_poisoned: false,
            pending_irq: None,
            applied_irq: None,
            accepted_irq: None,
            reported_active_irq: None,
            counts: ExitCounts::default(),
            completions: StoreCompletions::default(),
            mmio_owed: false,
            smccc_workaround_levels: [u64::MAX; 3],
            gic_ctlr_fixed_bits: 0,
        }
    }

    pub fn kvm(&self) -> &K {
        &self.kvm
    }

    fn ensure_runnable(&self) -> Result<()> {
        if !self.configured {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        if self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    fn apply_pending_irq(&mut self) -> Result<()> {
        if self.applied_irq == self.pending_irq {
            return Ok(());
        }
        if let Some(old) = self.applied_irq {
            self.kvm.set_irq_line(old, false)?;
        }
        if let Some(new) = self.pending_irq {
            self.kvm.set_irq_line(new, true)?;
        }
        self.applied_irq = self.pending_irq;
        Ok(())
    }

    fn observe_irq_acceptance(&mut self) -> Result<()> {
        let Some(id) = self.applied_irq else {
            self.reported_active_irq = None;
            return Ok(());
        };
        let (group, attr, bit) = if id.is_ppi() {
            (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, GICR_ISACTIVER0, id.0)
        } else if id.is_spi() {
            let word = id.0 / 32;
            (
                KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                0x0300 + u64::from(word) * 4,
                id.0 % 32,
            )
        } else {
            return Err(BackendError::InvalidState);
        };
        let active = self.kvm.get_vgic_attr(group, attr, false)? & (1u64 << bit) != 0;
        if active {
            if self.reported_active_irq != Some(id) {
                self.accepted_irq = Some(id);
                self.reported_active_irq = Some(id);
            }
        } else if self.reported_active_irq == Some(id) {
            self.reported_active_irq = None;
        }
        Ok(())
    }

    fn settle_mmio(&mut self) -> Result<()> {
        if !self.mmio_owed {
            return Ok(());
        }
        #[allow(clippy::disallowed_methods)]
        let started = std::time::Instant::now();
        self.kvm.complete_mmio_exit()?;
        self.mmio_owed = false;
        self.completions.runs = self.completions.runs.saturating_add(1);
        self.completions.nanos = self
            .completions
            .nanos
            .saturating_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX));
        Ok(())
    }

    fn enter_guest(&mut self) -> Result<Exit<Arm64>> {
        loop {
            self.apply_pending_irq()?;
            let view = self.kvm.run()?;
            self.mmio_owed = false;
            self.observe_irq_acceptance()?;
            if let Some((exit, pending)) = decode_exit(&view)? {
                self.mmio_owed = view.exit_reason == KVM_EXIT_MMIO && view.mmio.is_write;
                self.counts.bump(exit.reason());
                self.pending = pending;
                return Ok(exit);
            }
        }
    }

    fn clear_dirty_runs(&mut self, slot: u32, size: u64, bitmap: &[u64]) -> Result<()> {
        let words = dirty_bitmap_words(size)?;
        if bitmap.len() != words {
            return Err(BackendError::Internal(
                "KVM dirty bitmap has an unexpected size",
            ));
        }
        let slot_pages = size / KVM_PAGE_SIZE;
        let max_words = dirty_clear_max_words(u64::from(u32::MAX))?;
        let final_mask = if slot_pages.is_multiple_of(KVM_DIRTY_BITMAP_WORD_PAGES) {
            u64::MAX
        } else {
            (1u64 << (slot_pages % KVM_DIRTY_BITMAP_WORD_PAGES)) - 1
        };
        let nonzero = |index: usize| {
            let mask = if index == words.saturating_sub(1) {
                final_mask
            } else {
                u64::MAX
            };
            bitmap[index] & mask != 0
        };

        for start in 0..words {
            if !nonzero(start) {
                continue;
            }
            if start > 0 && nonzero(start.saturating_sub(1)) {
                continue;
            }
            let end = (start..words)
                .find(|&index| !nonzero(index))
                .unwrap_or(words);
            for chunk_start in (start..end).step_by(max_words) {
                let chunk_end = end.min(
                    chunk_start
                        .checked_add(max_words)
                        .ok_or(BackendError::InvalidState)?,
                );
                let first_page = u64::try_from(chunk_start)
                    .map_err(|_| BackendError::InvalidState)?
                    .checked_mul(KVM_DIRTY_BITMAP_WORD_PAGES)
                    .ok_or(BackendError::InvalidState)?;
                let chunk_end_page = u64::try_from(chunk_end)
                    .map_err(|_| BackendError::InvalidState)?
                    .checked_mul(KVM_DIRTY_BITMAP_WORD_PAGES)
                    .ok_or(BackendError::InvalidState)?;
                let end_page = chunk_end_page.min(slot_pages);
                let num_pages = end_page
                    .checked_sub(first_page)
                    .ok_or(BackendError::InvalidState)?;
                let num_pages = u32::try_from(num_pages).map_err(|_| BackendError::InvalidState)?;
                let slice = &bitmap[chunk_start..chunk_end];
                validate_clear_dirty_log(size, first_page, num_pages, slice)?;
                self.dirty_log_poisoned = true;
                self.kvm
                    .clear_dirty_log(slot, size, first_page, num_pages, slice)?;
            }
        }
        Ok(())
    }
}

impl<K: Arm64Kvm> Backend for Arm64KvmBackend<K> {
    type A = Arm64;

    fn set_policy(&mut self, policy: &crate::arch::arm64::Arm64Policy) -> Result<()> {
        self.kvm
            .set_one_reg(KVM_REG_ARM_PSCI_VERSION, KVM_ARM_PSCI_1_0)?;
        for id in OPTIONAL_FIRMWARE_BITMAPS {
            self.kvm.set_one_reg(id, 0)?;
        }
        for (&enc, &value) in &policy.id_regs.regs {
            let id = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG | u64::from(enc);
            self.kvm.set_one_reg(id, value)?;
        }
        for (index, register) in SMCCC_WORKAROUND_REGISTERS.into_iter().enumerate() {
            self.smccc_workaround_levels[index] = self.kvm.get_one_reg(register)?;
        }
        self.gic_ctlr_fixed_bits =
            vgic_cpu_sysreg(&self.kvm, ICC_CTLR_EL1)? & !ICC_CTLR_GUEST_WRITABLE;
        vgic_apr_count(self.gic_ctlr_fixed_bits)?;
        self.kvm.set_one_reg(CNTV_CTL_EL0, 0)?;
        if !self.configured {
            for word in 2..(HARMONY_GIC_NR_IRQS as usize / 16) {
                let (group, attr) = vgic_config_attr(word);
                self.kvm.set_vgic_attr(group, attr, false, 0)?;
                if self.kvm.get_vgic_attr(group, attr, false)? != 0 {
                    return Err(BackendError::Unsupported {
                        what: "KVM VGIC SPI level trigger baseline",
                    });
                }
            }
        }
        let _ = &policy.sysreg_traps;
        self.configured = true;
        Ok(())
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        if host.is_empty() {
            return Err(BackendError::Memory("zero-length memory region"));
        }
        if !gpa.0.is_multiple_of(4096) {
            return Err(BackendError::Memory("gpa is not 4 KiB-aligned"));
        }
        if !host.len().is_multiple_of(4096) {
            return Err(BackendError::Memory("region length is not 4 KiB-aligned"));
        }
        let len = host.len() as u64;
        let end = gpa
            .0
            .checked_add(len)
            .ok_or(BackendError::Memory("region wraps the address space"))?;
        for &(g, l) in &self.regions {
            let g_end = g + l;
            if gpa.0 < g_end && g < end {
                return Err(BackendError::Memory("region overlaps an existing map"));
            }
        }
        let slot = self.regions.len() as u32;
        // SAFETY: the caller upholds `map_memory`'s contract (pinned,
        // page-aligned, unaliased backing live for the backend's lifetime); we
        // forward the same guarantee to the seam. arm64 device frames sit below
        let flags = if self.dirty_log {
            KVM_MEM_LOG_DIRTY_PAGES
        } else {
            0
        };
        unsafe {
            self.kvm.set_user_memory_region_with_flags(
                slot,
                gpa.0,
                host.as_mut_ptr(),
                len,
                flags,
            )?;
        }
        self.regions.push((gpa.0, len));
        self.dirty_slots.push((slot, gpa.0, len));
        if flags == 0 {
            self.unlogged_slot = true;
        }
        Ok(())
    }

    fn drain_dirty_pages(&mut self) -> Result<Vec<u64>> {
        if !self.dirty_log {
            return Err(BackendError::Unsupported {
                what: "drain_dirty_pages (dirty logging disabled)",
            });
        }
        if self.unlogged_slot {
            return Err(BackendError::Unsupported {
                what: "drain_dirty_pages (a RAM slot was mapped without dirty logging)",
            });
        }
        if self.dirty_log_poisoned {
            return Err(BackendError::Internal(
                "arm64 dirty log is poisoned after a partial clear",
            ));
        }

        let mut gfns = Vec::new();
        for index in 0..self.dirty_slots.len() {
            let (slot, gpa, size) = self.dirty_slots[index];
            let bitmap = self.kvm.get_dirty_log(slot, size)?;
            decode_dirty_bitmap(gpa, size, &bitmap, &mut gfns)?;
            self.clear_dirty_runs(slot, size, &bitmap)?;
        }
        gfns.sort_unstable();
        gfns.dedup();
        self.dirty_log_poisoned = false;
        Ok(gfns)
    }

    fn run(&mut self) -> Result<Exit<Arm64>> {
        self.ensure_runnable()?;
        self.enter_guest()
    }

    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.kvm.cancellation_flag()
    }

    fn inject(&mut self, event: crate::arch::arm64::Arm64Injection) -> Result<()> {
        match event {
            crate::arch::arm64::Arm64Injection::Interrupt { intid } => {
                self.set_pending_irq(Some(intid))
            }
        }
    }

    fn set_pending_irq(&mut self, id: Option<GicIntId>) -> Result<()> {
        if id.is_some_and(|id| !(id.is_ppi() || id.is_spi())) {
            return Err(BackendError::InvalidState);
        }
        self.pending_irq = id;
        self.apply_pending_irq()
    }

    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        self.accepted_irq.take()
    }

    fn read_irq_mask(&mut self) -> Result<Option<bool>> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        self.settle_mmio()?;
        Ok(Some(
            self.kvm.get_one_reg(core_reg(CORE_PSTATE))? & PSTATE_I != 0,
        ))
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::MmioLoad { len } => {
                self.kvm.write_mmio_data(le_data(value, len))?;
                self.mmio_owed = true;
                self.pending = Pending::None;
                Ok(())
            }
            Pending::SysregRead => Err(BackendError::Unsupported {
                what: "complete_read (arm64 sysreg)",
            }),
            _ => Err(BackendError::NoPendingRead),
        }
    }

    fn complete_fault(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregRead | Pending::SysregWrite => {
                self.completion_staged = true;
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregWrite => {
                self.completion_staged = true;
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
        Err(BackendError::NoPendingRead)
    }

    fn complete_arch(&mut self, _completion: crate::arch::arm64::Arm64Completion) -> Result<()> {
        match _completion {}
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        if !self.completion_staged {
            return Ok(());
        }
        Err(BackendError::Unsupported {
            what: "retire_pending_completion (arm64 sysreg)",
        })
    }

    fn save(&mut self) -> Result<Arm64VcpuState> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        self.settle_mmio()?;
        save_vcpu(&self.kvm)
    }

    fn validate_restore_state(&self, state: &Arm64VcpuState) -> Result<()> {
        validate_restore_vcpu_state(state)?;
        validate_vgic_host(
            &self.kvm,
            state.gic.as_ref().ok_or(BackendError::InvalidState)?,
        )
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
        self.settle_mmio()?;
        restore_vcpu(&mut self.kvm, state)?;
        self.pending_irq = None;
        self.applied_irq = None;
        self.accepted_irq = None;
        self.reported_active_irq = None;
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn store_completions(&self) -> StoreCompletions {
        self.completions
    }

    fn capabilities(&self) -> Capabilities<crate::arch::arm64::Arm64Caps> {
        Capabilities {
            name: "kvm-arm64-vgicv3",
            arch: crate::arch::arm64::Arm64Caps {
                in_kernel_gic: true,
                asid_bits: self.kvm.asid_bits(),
                smccc_workaround_levels: self.smccc_workaround_levels,
                psci_profile: crate::arch::arm64::Arm64PsciProfile::KvmPsci10,
                gic_ctlr_fixed_bits: self.gic_ctlr_fixed_bits,
            },
        }
    }
}

#[cfg(any(test, feature = "mock"))]
#[derive(Debug, Default)]
pub struct FakeKvm {
    #[cfg(test)]
    state_reads: std::sync::atomic::AtomicUsize,
    cancellation: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    regs: std::collections::BTreeMap<u64, u64>,
    regs32: std::collections::BTreeMap<u64, u32>,
    regs128: std::collections::BTreeMap<u64, [u8; 16]>,
    mp_state: MpState,
    run_queue: std::collections::VecDeque<KvmRunView>,
    pub calls: Vec<&'static str>,
    pub last_mmio_data: Option<[u8; 8]>,
    pub memslots: Vec<(u32, u64, u64)>,
    pub memslot_flags: Vec<(u32, u32)>,
    pub dirty_bitmaps: std::collections::BTreeMap<u32, Vec<u64>>,
    pub dirty_log_calls: Vec<(u32, u64)>,
    pub dirty_log_fail_on_call: Option<usize>,
    pub dirty_clear_calls: Vec<(u32, u64, u64, u32, Vec<u64>)>,
    pub dirty_clear_fail_on_call: Option<usize>,
    vgic_attrs: std::collections::BTreeMap<(u32, u64, bool), u64>,
    accept_irqs: bool,
    asid_bits: Arm64AsidBits,
    #[cfg_attr(not(test), allow(dead_code))]
    init_features: [u32; 7],
    initialized: bool,
}

#[cfg(any(test, feature = "mock"))]
impl FakeKvm {
    pub fn new() -> Self {
        let mut fake = Self::default();
        for (id, level) in SMCCC_WORKAROUND_REGISTERS
            .into_iter()
            .zip(crate::arch::arm64::Arm64Caps::FIXED_SMCCC_WORKAROUND_LEVELS)
        {
            fake.regs.insert(id, level);
        }
        fake.vgic_attrs.insert(
            (KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true),
            0x8400,
        );
        fake
    }

    pub fn push_run(&mut self, view: KvmRunView) -> &mut Self {
        self.run_queue.push_back(view);
        self
    }

    pub fn reg(&self, id: u64) -> Option<u64> {
        self.regs.get(&id).copied()
    }

    pub fn set_asid_bits(&mut self, asid_bits: Arm64AsidBits) -> &mut Self {
        self.asid_bits = asid_bits;
        self
    }

    pub fn set_accept_irqs(&mut self, accept: bool) -> &mut Self {
        self.accept_irqs = accept;
        self
    }

    pub fn set_dirty_bitmap(&mut self, slot: u32, bitmap: Vec<u64>) -> &mut Self {
        self.dirty_bitmaps.insert(slot, bitmap);
        self
    }

    pub fn fail_dirty_log_on_call(&mut self, call: usize) -> &mut Self {
        self.dirty_log_fail_on_call = Some(call);
        self
    }

    pub fn fail_dirty_clear_on_call(&mut self, call: usize) -> &mut Self {
        self.dirty_clear_fail_on_call = Some(call);
        self
    }
}

#[cfg(any(test, feature = "mock"))]
impl Arm64Kvm for FakeKvm {
    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        self.cancellation.clone()
    }

    fn vcpu_init(&mut self) -> Result<()> {
        self.calls.push("vcpu_init");
        self.init_features = vcpu_init_features();
        self.initialized = true;
        Ok(())
    }

    fn asid_bits(&self) -> Arm64AsidBits {
        self.asid_bits
    }

    unsafe fn set_user_memory_region(
        &mut self,
        slot: u32,
        gpa: u64,
        _host: *mut u8,
        len: u64,
    ) -> Result<()> {
        // SAFETY: this compatibility entry point forwards the same raw
        // backing pointer and lifetime contract to the flagged implementation.
        unsafe {
            self.set_user_memory_region_with_flags(slot, gpa, _host, len, KVM_MEM_LOG_DIRTY_PAGES)
        }
    }

    unsafe fn set_user_memory_region_with_flags(
        &mut self,
        slot: u32,
        gpa: u64,
        _host: *mut u8,
        len: u64,
        flags: u32,
    ) -> Result<()> {
        self.calls.push("set_user_memory_region");
        self.memslots.push((slot, gpa, len));
        self.memslot_flags.push((slot, flags));
        Ok(())
    }

    fn get_dirty_log(&mut self, slot: u32, size: u64) -> Result<Vec<u64>> {
        self.calls.push("get_dirty_log");
        self.dirty_log_calls.push((slot, size));
        if self.dirty_log_fail_on_call == Some(self.dirty_log_calls.len()) {
            return Err(BackendError::Internal(
                "fake KVM dirty-log retrieval failed",
            ));
        }
        let words = dirty_bitmap_words(size)?;
        let bitmap = self
            .dirty_bitmaps
            .entry(slot)
            .or_insert_with(|| vec![0; words]);
        if bitmap.len() != words {
            return Err(BackendError::Internal(
                "fake KVM dirty bitmap has an unexpected size",
            ));
        }
        Ok(bitmap.clone())
    }

    fn clear_dirty_log(
        &mut self,
        slot: u32,
        size: u64,
        first_page: u64,
        num_pages: u32,
        bitmap: &[u64],
    ) -> Result<()> {
        validate_clear_dirty_log(size, first_page, num_pages, bitmap)?;
        let words = dirty_bitmap_words(size)?;
        let call = self.dirty_clear_calls.len() + 1;
        self.calls.push("clear_dirty_log");
        self.dirty_clear_calls
            .push((slot, size, first_page, num_pages, bitmap.to_vec()));
        if self.dirty_clear_fail_on_call == Some(call) {
            return Err(BackendError::Internal("fake KVM dirty-log clear failed"));
        }
        let dirty = self
            .dirty_bitmaps
            .entry(slot)
            .or_insert_with(|| vec![0; words]);
        if dirty.len() != words {
            return Err(BackendError::Internal(
                "fake KVM dirty bitmap has an unexpected size",
            ));
        }
        let first_word = usize::try_from(first_page / KVM_DIRTY_BITMAP_WORD_PAGES)
            .map_err(|_| BackendError::InvalidState)?;
        for (offset, &mask) in bitmap.iter().enumerate() {
            let index = first_word
                .checked_add(offset)
                .ok_or(BackendError::InvalidState)?;
            let word = dirty.get_mut(index).ok_or(BackendError::InvalidState)?;
            *word &= !mask;
        }
        Ok(())
    }

    fn get_one_reg(&self, id: u64) -> Result<u64> {
        #[cfg(test)]
        self.state_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.regs.get(&id).copied().unwrap_or(0))
    }

    fn set_one_reg(&mut self, id: u64, value: u64) -> Result<()> {
        if !self.initialized {
            return Err(BackendError::Internal(
                "set_one_reg before vcpu_init (KVM rejects register access on an un-init'd vCPU)",
            ));
        }
        self.calls.push("set_one_reg");
        self.regs.insert(id, value);
        Ok(())
    }

    fn get_one_reg32(&self, id: u64) -> Result<u32> {
        #[cfg(test)]
        self.state_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.regs32.get(&id).copied().unwrap_or(0))
    }

    fn set_one_reg32(&mut self, id: u64, value: u32) -> Result<()> {
        if !self.initialized {
            return Err(BackendError::Internal(
                "set_one_reg32 before vcpu_init (KVM rejects register access on an un-init'd vCPU)",
            ));
        }
        self.calls.push("set_one_reg32");
        self.regs32.insert(id, value);
        Ok(())
    }

    fn get_one_reg128(&self, id: u64) -> Result<[u8; 16]> {
        #[cfg(test)]
        self.state_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.regs128.get(&id).copied().unwrap_or([0; 16]))
    }

    fn set_one_reg128(&mut self, id: u64, value: [u8; 16]) -> Result<()> {
        if !self.initialized {
            return Err(BackendError::Internal(
                "set_one_reg128 before vcpu_init (KVM rejects register access on an un-init'd vCPU)",
            ));
        }
        self.calls.push("set_one_reg128");
        self.regs128.insert(id, value);
        Ok(())
    }

    fn get_mp_state(&self) -> Result<MpState> {
        #[cfg(test)]
        self.state_reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self.mp_state)
    }

    fn set_mp_state(&mut self, mp: MpState) -> Result<()> {
        self.calls.push("set_mp_state");
        self.mp_state = mp;
        Ok(())
    }

    fn set_irq_line(&mut self, id: GicIntId, level: bool) -> Result<()> {
        self.calls.push(if level {
            "set_irq_line_high"
        } else {
            "set_irq_line_low"
        });
        let block = id.0 / 32 * 32;
        let key = (
            KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
            vgic_level_attr(block),
            false,
        );
        let bit = 1u64 << (id.0 % 32);
        let value = self.vgic_attrs.get(&key).copied().unwrap_or(0);
        self.vgic_attrs
            .insert(key, if level { value | bit } else { value & !bit });
        Ok(())
    }

    fn get_vgic_attr(&self, group: u32, attr: u64, width64: bool) -> Result<u64> {
        Ok(self
            .vgic_attrs
            .get(&(group, attr, width64))
            .copied()
            .unwrap_or(0))
    }

    fn set_vgic_attr(&mut self, group: u32, attr: u64, width64: bool, value: u64) -> Result<()> {
        self.vgic_attrs.insert((group, attr, width64), value);
        Ok(())
    }

    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()> {
        self.calls.push("write_mmio_data");
        self.last_mmio_data = Some(data);
        Ok(())
    }

    fn complete_mmio_exit(&mut self) -> Result<()> {
        self.calls.push("complete_mmio_exit");
        Ok(())
    }

    fn run(&mut self) -> Result<KvmRunView> {
        self.calls.push("run");
        if self.accept_irqs {
            let levels = self
                .vgic_attrs
                .get(&(KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO, vgic_level_attr(0), false))
                .copied()
                .unwrap_or(0);
            let private = levels & 0xffff_0000;
            self.vgic_attrs.insert(
                (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, GICR_ISACTIVER0, false),
                private,
            );
            for block in [32, 64] {
                let levels = self
                    .vgic_attrs
                    .get(&(
                        KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
                        vgic_level_attr(block),
                        false,
                    ))
                    .copied()
                    .unwrap_or(0);
                self.vgic_attrs.insert(
                    (
                        KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                        GIC_ISACTIVER + u64::from(block / 32) * 4,
                        false,
                    ),
                    levels,
                );
            }
        }
        self.run_queue
            .pop_front()
            .ok_or(BackendError::Internal("fake KVM run-queue empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::arm64::{Arm64CoreRegs, Arm64Policy, Arm64SimdFpState, IdRegModel};

    #[test]
    fn backend_exposes_the_kvm_cancellation_latch() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let latch = Arc::new(AtomicBool::new(false));
        let backend = Arm64KvmBackend::new(FakeKvm {
            cancellation: Some(Arc::clone(&latch)),
            ..FakeKvm::default()
        });
        let exposed = Backend::cancellation_flag(&backend).expect("cancellation latch");
        assert!(Arc::ptr_eq(&exposed, &latch));
        exposed.store(true, Ordering::Release);
        assert!(latch.load(Ordering::Acquire));
        assert!(Backend::cancellation_flag(&Arm64KvmBackend::new(FakeKvm::new())).is_none());
    }

    fn mmio_store(gpa: u64, value: u64, len: u32) -> KvmRunView {
        KvmRunView {
            exit_reason: KVM_EXIT_MMIO,
            mmio: MmioView {
                phys_addr: gpa,
                data: le_data(value, len),
                len,
                is_write: true,
            },
            ..Default::default()
        }
    }

    fn mmio_load(gpa: u64, len: u32) -> KvmRunView {
        KvmRunView {
            exit_reason: KVM_EXIT_MMIO,
            mmio: MmioView {
                phys_addr: gpa,
                data: [0; 8],
                len,
                is_write: false,
            },
            ..Default::default()
        }
    }

    #[test]
    fn run_page_loopback_over_a_synthetic_buffer() {
        let off = RunOffsets {
            exit_reason: 8,
            mmio_phys_addr: 32,
            mmio_data: 40,
            mmio_len: 48,
            mmio_is_write: 52,
            system_event_type: 32,
        };
        let len = 128usize;
        let mut buf = vec![0u8; len];
        buf[off.exit_reason..off.exit_reason + 4].copy_from_slice(&KVM_EXIT_MMIO.to_le_bytes());
        buf[off.mmio_phys_addr..off.mmio_phys_addr + 8]
            .copy_from_slice(&0x0900_0018u64.to_le_bytes());
        buf[off.mmio_len..off.mmio_len + 4].copy_from_slice(&4u32.to_le_bytes());
        buf[off.mmio_is_write] = 0;

        let page = unsafe { RunPage::new(buf.as_mut_ptr(), len) };
        let view = unsafe { page.view(&off) }.unwrap();
        assert_eq!(view.exit_reason, KVM_EXIT_MMIO);
        assert_eq!(view.mmio.phys_addr, 0x0900_0018);
        assert_eq!(view.mmio.len, 4);
        assert!(!view.mmio.is_write);

        let (exit, pending) = decode_exit(&view).unwrap().unwrap();
        assert!(matches!(
            exit,
            Exit::Common(CommonExit::Mmio { write: None, .. })
        ));
        assert_eq!(pending, Pending::MmioLoad { len: 4 });

        unsafe { page.write_mmio_data(&off, le_data(0x90, 4)) }.unwrap();
        let view2 = unsafe { page.view(&off) }.unwrap();
        assert_eq!(view2.mmio.data, le_data(0x90, 4));

        buf[off.exit_reason..off.exit_reason + 4]
            .copy_from_slice(&KVM_EXIT_SYSTEM_EVENT.to_le_bytes());
        buf[off.system_event_type..off.system_event_type + 4]
            .copy_from_slice(&KVM_SYSTEM_EVENT_SHUTDOWN.to_le_bytes());
        let sev = unsafe { page.view(&off) }.unwrap();
        assert_eq!(
            decode_exit(&sev).unwrap().unwrap().0,
            CommonExit::Shutdown.into()
        );

        let bad = RunOffsets {
            exit_reason: len,
            ..off
        };
        assert!(unsafe { page.view(&bad) }.is_err());
    }

    #[test]
    fn kvm_uapi_constants_match_the_headers() {
        assert_eq!(
            KVM_EXIT_HYPERCALL, 3,
            "3, not 13 (13 = KVM_EXIT_S390_SIEIC)"
        );
        assert_eq!(KVM_EXIT_MMIO, 6);
        assert_eq!(KVM_EXIT_SYSTEM_EVENT, 24);
        assert_eq!(KVM_EXIT_INTR, 10);
        assert_eq!(KVM_EXIT_FAIL_ENTRY, 9);
        assert_eq!(KVM_EXIT_INTERNAL_ERROR, 17);

        assert_eq!(KVM_REG_ARM_CORE, 0x10_0000, "0x0010 << 16");
        assert_eq!(KVM_REG_ARM64_SYSREG, 0x13_0000, "0x0013 << 16");
        assert_eq!(KVM_REG_ARM_FW, 0x14_0000, "0x0014 << 16");
        assert_eq!(KVM_REG_ARM_FW_FEAT_BMAP, 0x16_0000, "0x0016 << 16");
        assert_eq!(HARMONY_GIC_NR_IRQS, 96);
        assert_eq!(Arm64GicState::default().version, GIC_STATE_VERSION);

        assert_eq!(core_reg(0), 0x6030_0000_0010_0000, "x0");
        assert_eq!(core_reg(CORE_PC), 0x6030_0000_0010_0040, "pc");
        assert_eq!(
            KVM_REG_ARM_PSCI_VERSION, 0x6030_0000_0014_0000,
            "KVM firmware pseudo-register 0"
        );
        assert_eq!(KVM_ARM_PSCI_1_0, 0x0001_0000);
        assert_eq!(KVM_REG_ARM_STD_BMAP, 0x6030_0000_0016_0000);
        assert_eq!(KVM_REG_ARM_STD_HYP_BMAP, 0x6030_0000_0016_0001);
        assert_eq!(KVM_REG_ARM_VENDOR_HYP_BMAP, 0x6030_0000_0016_0002);
        assert_eq!(KVM_REG_ARM_VENDOR_HYP_BMAP_2, 0x6030_0000_0016_0003);
        assert_eq!(
            core_reg_sized(KVM_REG_SIZE_U32, 0x155),
            0x6020_0000_0010_0155
        );
        assert_eq!(
            core_reg_sized(KVM_REG_SIZE_U128, 0x2aa),
            0x6040_0000_0010_02aa
        );
        assert_eq!(vgic_sysreg(3, 2, 11, 5, 6), 0xd5ae);
        assert_eq!(
            sysreg_id(3, 0, 1, 0, 0),
            0x6030_0000_0013_c080,
            "SCTLR_EL1 (S3_0_C1_C0_0)"
        );
        assert_eq!(
            CNTV_CVAL_EL0, 0x6030_0000_0013_df02,
            "KVM_REG_ARM_TIMER_CVAL uses the stable swapped UAPI ID"
        );
        assert_ne!(
            CNTV_CVAL_EL0,
            sysreg_id(3, 3, 14, 3, 2),
            "the architectural CVAL encoding is KVM_REG_ARM_TIMER_CNT"
        );
        assert_eq!(GICR_ISACTIVER0, 0x1_0300);

        assert_eq!(
            vgic_bitmap_attr(0, GIC_ISENABLER),
            (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, 0x1_0100)
        );
        assert_eq!(
            vgic_bitmap_attr(1, GIC_ISENABLER),
            (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, 0x0104)
        );
        assert_eq!(
            vgic_bitmap_attr(17, GIC_ISPENDR),
            (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, 0x0244)
        );
        assert_eq!(
            vgic_priority_attr(0),
            (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, 0x1_0400)
        );
        assert_eq!(
            vgic_priority_attr(7),
            (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, 0x1_041c)
        );
        assert_eq!(
            vgic_priority_attr(8),
            (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, 0x0420)
        );
        assert_eq!(
            vgic_priority_attr(23),
            (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, 0x045c)
        );

        let mut fake = FakeKvm::new();
        let test_irq = GicIntId(65);
        fake.set_irq_line(test_irq, true).unwrap();
        fake.set_irq_line(test_irq, true).unwrap();
        assert_eq!(
            fake.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO, vgic_level_attr(64), false,)
                .unwrap(),
            1 << 1,
        );
        fake.set_irq_line(test_irq, false).unwrap();
        fake.set_irq_line(test_irq, false).unwrap();
        assert_eq!(
            fake.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO, vgic_level_attr(64), false,)
                .unwrap(),
            0,
        );

        fake.set_vgic_attr(
            KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
            vgic_level_attr(32),
            false,
            0,
        )
        .unwrap();
        let (priority_group, priority_attr) = vgic_priority_attr(8);
        fake.set_vgic_attr(
            priority_group,
            priority_attr,
            false,
            u64::from(0x4433_2211_u32),
        )
        .unwrap();
        fake.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_PMR_EL1, true, 0x7f)
            .unwrap();
        for (reg, value) in [
            (ICC_BPR0_EL1, 7),
            (ICC_BPR1_EL1, 6),
            (ICC_CTLR_EL1, 0x8402),
            (ICC_IGRPEN0_EL1, 1),
            (ICC_AP0R0_EL1, 0x1234),
            (ICC_AP1R0_EL1, 0x5678),
        ] {
            fake.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, reg, true, value)
                .unwrap();
        }
        fake.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_IGRPEN1_EL1, true, 1)
            .unwrap();
        fake.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_DIST_REGS, GICD_CTLR, false, 0b10)
            .unwrap();
        let (config_group, config_attr) = vgic_config_attr(2);
        fake.set_vgic_attr(config_group, config_attr, false, 0x20000)
            .unwrap();
        let saved = save_vgic(&fake).unwrap();
        assert_eq!(saved.gicd_ctlr, 0b10);
        assert_eq!(saved.line_level[0], 0);
        assert_eq!(saved.line_level[1], 0);
        assert_eq!(saved.priority[32..36], [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(saved.config[2], 0x20000);
        assert_eq!(saved.pmr, 0x7f);
        assert_eq!(saved.bpr0, 7);
        assert_eq!(saved.bpr1, 6);
        assert_eq!(saved.ctlr, 0x8402);
        assert!(saved.igrpen0);
        assert!(saved.igrpen1);
        assert_eq!(saved.ap0r, [0x1234, 0, 0, 0]);
        assert_eq!(saved.ap1r, [0x5678, 0, 0, 0]);
        validate_vgic_state(&saved).unwrap();

        let mut restored = FakeKvm::new();
        restore_vgic(&mut restored, &saved).unwrap();
        assert_eq!(
            restored
                .get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_DIST_REGS, GICD_CTLR, false)
                .unwrap(),
            0b1_0010,
            "restore must reconstitute the fixed ARE bit without losing Group-1 enable"
        );
        assert_eq!(save_vgic(&restored).unwrap(), saved);

        for invalid in [
            Arm64GicState {
                version: saved.version + 1,
                ..saved
            },
            Arm64GicState {
                impl_spis: saved.impl_spis + 1,
                ..saved
            },
            Arm64GicState {
                timer_hz: saved.timer_hz + 1,
                ..saved
            },
            Arm64GicState {
                timer_intid: saved.timer_intid + 1,
                ..saved
            },
            Arm64GicState {
                gicd_ctlr: 1,
                ..saved
            },
            Arm64GicState {
                cntv_ctl: 1,
                ..saved
            },
            Arm64GicState {
                cntv_cval: 1,
                ..saved
            },
            Arm64GicState {
                timer_fired: true,
                ..saved
            },
            Arm64GicState {
                ctlr: saved.ctlr | 1,
                ..saved
            },
            Arm64GicState { bpr0: 8, ..saved },
            Arm64GicState { bpr1: 8, ..saved },
            Arm64GicState {
                config: {
                    let mut config = saved.config;
                    config[2] = 1;
                    config
                },
                ..saved
            },
        ] {
            assert!(matches!(
                validate_vgic_state(&invalid),
                Err(BackendError::InvalidState)
            ));
        }

        let mut invalid_bitmap = saved;
        invalid_bitmap.pending[3] = 1;
        assert!(matches!(
            validate_vgic_state(&invalid_bitmap),
            Err(BackendError::InvalidState)
        ));
        let mut invalid_priority = saved;
        invalid_priority.priority[96] = 1;
        assert!(matches!(
            validate_vgic_state(&invalid_priority),
            Err(BackendError::InvalidState)
        ));
        let mut invalid_apr = saved;
        invalid_apr.ap1r[1] = 1;
        assert!(matches!(
            validate_vgic_state(&invalid_apr),
            Err(BackendError::InvalidState)
        ));
    }

    #[test]
    fn vgic_cpu_interface_rejects_hidden_bpr_and_foreign_host_before_mutation() {
        let mut source = FakeKvm::new();
        source
            .set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, 0x8401)
            .unwrap();
        assert!(matches!(
            save_vgic(&source),
            Err(BackendError::Unsupported { .. })
        ));

        source
            .set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, 0x8400)
            .unwrap();
        let saved = save_vgic(&source).unwrap();
        let mut foreign = FakeKvm::new();
        foreign
            .set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, 0x9400)
            .unwrap();
        let state = Arm64VcpuState {
            gic: Some(saved),
            ..Arm64VcpuState::default()
        };
        assert!(matches!(
            restore_vcpu(&mut foreign, &state),
            Err(BackendError::InvalidState)
        ));
        assert!(foreign.calls.is_empty());
    }

    #[test]
    fn vgic_private_trigger_configuration_rejects_foreign_host_before_mutation() {
        let mut source = FakeKvm::new();
        let (group, attr) = vgic_config_attr(0);
        source
            .set_vgic_attr(group, attr, false, 0xaaaa_aaaa)
            .unwrap();
        let saved = save_vgic(&source).unwrap();
        let mut target = FakeKvm::new();
        let state = Arm64VcpuState {
            gic: Some(saved),
            ..Arm64VcpuState::default()
        };
        assert!(matches!(
            restore_vcpu(&mut target, &state),
            Err(BackendError::InvalidState)
        ));
        assert!(target.calls.is_empty());
    }

    #[test]
    fn policy_normalizes_spi_triggers_without_clobbering_later_guest_configuration() {
        let mut fake = FakeKvm::new();
        let (private_group, private_attr) = vgic_config_attr(0);
        fake.set_vgic_attr(private_group, private_attr, false, 0xaaaa_aaaa)
            .unwrap();
        for word in 2..(HARMONY_GIC_NR_IRQS as usize / 16) {
            let (group, attr) = vgic_config_attr(word);
            fake.set_vgic_attr(group, attr, false, 0xaaaa_aaaa).unwrap();
        }
        fake.vcpu_init().unwrap();
        let mut backend = Arm64KvmBackend::new(fake);
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let mut state = backend.save().unwrap();
        let gic = state.gic.as_mut().unwrap();
        assert_eq!(gic.config[..6], [0xaaaa_aaaa, 0, 0, 0, 0, 0]);
        gic.config[2] = 0x20000;
        backend.restore(&state).unwrap();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        assert_eq!(backend.save().unwrap().gic.unwrap().config[2], 0x20000);
    }

    #[test]
    fn nonzero_spi_route_rejects_capture_and_restore_clears_reused_target() {
        assert_eq!(vgic_router_attr(0, 0), 0x6100);
        assert_eq!(vgic_router_attr(0, 1), 0x6104);
        assert_eq!(vgic_router_attr(63, 0), 0x62f8);
        assert_eq!(vgic_router_attr(63, 1), 0x62fc);
        let source = FakeKvm::new();
        let saved = save_vgic(&source).unwrap();
        let mut target = FakeKvm::new();
        for (spi, half) in [(0, 0), (0, 1), (63, 0), (63, 1)] {
            target
                .set_vgic_attr(
                    KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                    vgic_router_attr(spi, half),
                    false,
                    1,
                )
                .unwrap();
            assert!(matches!(
                save_vgic(&target),
                Err(BackendError::Unsupported { .. })
            ));
            target
                .set_vgic_attr(
                    KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                    vgic_router_attr(spi, half),
                    false,
                    0,
                )
                .unwrap();
        }
        for half in 0..2 {
            target
                .set_vgic_attr(
                    KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                    vgic_router_attr(63, half),
                    false,
                    1,
                )
                .unwrap();
        }
        restore_vgic(&mut target, &saved).unwrap();
        assert_eq!(save_vgic(&target).unwrap(), saved);
        for half in 0..2 {
            assert_eq!(
                target
                    .get_vgic_attr(
                        KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
                        vgic_router_attr(63, half),
                        false,
                    )
                    .unwrap(),
                0
            );
        }
    }

    #[test]
    fn pending_or_asserted_spi_rejects_capture_and_restore_before_mutation() {
        let mut fake = FakeKvm::new();
        let baseline = save_vgic(&fake).unwrap();
        for (group, attr, value) in [
            (KVM_DEV_ARM_VGIC_GRP_DIST_REGS, GIC_ISPENDR + 4, 1),
            (KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO, vgic_level_attr(32), 1),
        ] {
            fake.set_vgic_attr(group, attr, false, value).unwrap();
            assert!(matches!(
                save_vgic(&fake),
                Err(BackendError::Unsupported { .. })
            ));
            fake.set_vgic_attr(group, attr, false, 0).unwrap();
        }
        assert_eq!(save_vgic(&fake).unwrap(), baseline);

        for (pending, line_level) in [(1, 0), (0, 1)] {
            let mut invalid = baseline;
            invalid.pending[1] = pending;
            invalid.line_level[1] = line_level;
            let state = Arm64VcpuState {
                gic: Some(invalid),
                ..Arm64VcpuState::default()
            };
            let mut target = FakeKvm::new();
            assert!(matches!(
                restore_vcpu(&mut target, &state),
                Err(BackendError::InvalidState)
            ));
            assert!(target.calls.is_empty());
        }
    }

    #[repr(C, align(16384))]
    struct LiveRouteMemory([u8; 16384]);

    fn map_route_memory<B: Backend<A = Arm64>>(
        backend: &mut B,
        memory: &mut Box<LiveRouteMemory>,
    ) -> Result<()> {
        // SAFETY: the boxed allocation is page aligned and callers retain it
        // until the backend is dropped, so its address stays valid for the map.
        unsafe { backend.map_memory(Gpa(0x4000_0000), &mut memory.0) }
    }

    #[test]
    fn route_memory_mapping_is_miri_reachable() {
        let mut backend = crate::MockArm64Backend::new();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        let mut memory = Box::new(LiveRouteMemory([0; 16384]));
        map_route_memory(&mut backend, &mut memory).unwrap();
        assert_eq!(backend.save().unwrap().core.pc, 0);
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
    struct LiveRouteGuest {
        backend: Arm64KvmBackend<crate::LiveKvm>,
        _memory: Box<LiveRouteMemory>,
    }

    #[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
    impl LiveRouteGuest {
        fn new(program: &[u32]) -> Self {
            let mut backend = Arm64KvmBackend::new(crate::LiveKvm::new().unwrap());
            let mut memory = Box::new(LiveRouteMemory([0; 16384]));
            for (slot, word) in memory.0.chunks_exact_mut(4).zip(program) {
                slot.copy_from_slice(&word.to_le_bytes());
            }
            backend.set_policy(&Arm64Policy::default()).unwrap();
            map_route_memory(&mut backend, &mut memory).unwrap();
            backend.invalidate_instruction_cache(memory.0.as_ptr() as usize, memory.0.len());
            let mut initial = backend.save().unwrap();
            initial.core.pc = 0x4000_0000;
            initial.core.pstate = 0x3c5;
            initial.sysregs.sctlr_el1 &= !1;
            backend.restore(&initial).unwrap();
            Self {
                backend,
                _memory: memory,
            }
        }

        fn next_store(&mut self) -> u64 {
            match self.backend.run() {
                Ok(Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(0x0900_0000),
                    write: Some(value),
                    ..
                })) => value,
                other => panic!("expected an ARM observation, got {other:?}"),
            }
        }
    }

    #[test]
    #[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
    #[ignore = "live ARM KVM; registered in HOST_TESTS"]
    fn routed_away_pending_spi_refuses_capture_and_restore() {
        let program = [
            0xd2a1_0001,
            0xf28c_2001,
            0xd2a1_0002,
            0xf280_1082,
            0xd2a1_0003,
            0xf280_2083,
            0xd2a1_0004,
            0xf280_4084,
            0xd2a1_0005,
            0xd2a1_2006,
            0xd2a1_0008,
            0xf281_8108,
            0xd2a1_0149,
            0xf280_0289,
            0x5280_000a,
            0xb900_012a,
            0xd2a1_000b,
            0xf280_840b,
            0x5280_140a,
            0xb900_016a,
            0xd280_0020,
            0xf900_0020,
            0xb900_0040,
            0x5280_0247,
            0xb900_0107,
            0xb900_00a7,
            0xb900_0060,
            0xb900_0080,
            0xd280_0000,
            0xf900_0020,
            0xd280_0020,
            0xd518_cca0,
            0xd280_1e00,
            0xd518_4600,
            0xd280_0020,
            0xd518_cce0,
            0xd503_3f9f,
            0xd503_3fdf,
            0xd538_cc40,
            0xf900_00c0,
            0xd538_cc40,
            0xf900_00c0,
            0x1400_0000,
        ];
        let mut routed_away = LiveRouteGuest::new(&program);
        assert_eq!(routed_away.next_store(), 1023);
        let hidden = read_vgic_state(routed_away.backend.kvm()).unwrap();
        assert_eq!(hidden.pending[1] & 1, 1);
        assert!(matches!(
            routed_away.backend.save(),
            Err(BackendError::Unsupported { .. })
        ));
        assert_eq!(routed_away.next_store(), 1023);

        let mut direct_program = Vec::from(program);
        direct_program.splice(22..22, [0xd280_0000, 0xf900_0020, 0xd280_0020]);
        let mut directly_pending = LiveRouteGuest::new(&direct_program);
        assert_eq!(directly_pending.next_store(), 32);
        let deliverable = read_vgic_state(directly_pending.backend.kvm()).unwrap();
        assert_eq!(hidden, deliverable);
        assert!(matches!(
            directly_pending.backend.save(),
            Err(BackendError::Unsupported { .. })
        ));
        assert_eq!(directly_pending.next_store(), 32);

        let mut route_cycle_program = direct_program.clone();
        route_cycle_program.insert(25, 0xf900_00c0);
        let mut route_cycle = LiveRouteGuest::new(&route_cycle_program);
        assert_eq!(route_cycle.next_store(), 1);
        let route_cycle_cut = route_cycle.backend.save().unwrap();
        assert_eq!(route_cycle.next_store(), 32);

        let mut fresh_zero_program = Vec::from(program);
        fresh_zero_program[20] = 0xd280_0000;
        fresh_zero_program.splice(22..22, [0xd280_0020]);
        fresh_zero_program.insert(23, 0xf900_00c0);
        let mut fresh_zero = LiveRouteGuest::new(&fresh_zero_program);
        assert_eq!(fresh_zero.next_store(), 1);
        let fresh_zero_cut = fresh_zero.backend.save().unwrap();
        assert_eq!(route_cycle_cut.gic, fresh_zero_cut.gic);
        assert_eq!(fresh_zero.next_store(), 32);
        let mut cold_zero = LiveRouteGuest::new(&fresh_zero_program);
        cold_zero.backend.restore(&fresh_zero_cut).unwrap();
        assert_eq!(cold_zero.next_store(), 32);

        let mut target = LiveRouteGuest::new(&[0x1400_0000]);
        let before = target.backend.save().unwrap();
        let mut incoming = before;
        incoming.gic = Some(hidden);
        assert!(matches!(
            target.backend.restore(&incoming),
            Err(BackendError::InvalidState)
        ));
        let after = target.backend.save().unwrap();
        assert_eq!(after.core, before.core);
        assert_eq!(after.gic, before.gic);
        eprintln!(
            "SPI32 route-history HPPIR=1023 direct HPPIR=32 captured-GIC-equal=true capture=refused restore=refused"
        );
    }

    #[test]
    fn vgic_apr_count_tracks_host_priority_width() {
        for (priority_bits, expected_count) in [(4, 1), (5, 1), (6, 2), (7, 4)] {
            let ctlr = 0x8000 | ((priority_bits - 1) << ICC_CTLR_PRIBITS_SHIFT);
            let mut fake = FakeKvm::new();
            fake.set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, ctlr)
                .unwrap();
            for index in 0..expected_count {
                fake.set_vgic_attr(
                    KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS,
                    ICC_AP1R0_EL1 + index as u64,
                    true,
                    (index + 1) as u64,
                )
                .unwrap();
            }
            let saved = save_vgic(&fake).unwrap();
            assert_eq!(
                saved.ap1r.iter().filter(|&&value| value != 0).count(),
                expected_count
            );
            let mut restored = FakeKvm::new();
            restored
                .set_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_CTLR_EL1, true, ctlr)
                .unwrap();
            restore_vgic(&mut restored, &saved).unwrap();
            assert_eq!(save_vgic(&restored).unwrap(), saved);
        }

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_vgic_attr(
            KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS,
            ICC_CTLR_EL1,
            true,
            0x8000 | (7 << ICC_CTLR_PRIBITS_SHIFT),
        )
        .unwrap();
        let mut backend = Arm64KvmBackend::new(fake);
        assert!(matches!(
            backend.set_policy(&Arm64Policy::default()),
            Err(BackendError::Unsupported { .. })
        ));
        assert!(!backend.configured);
    }

    #[test]
    fn mmio_rejects_non_architectural_widths() {
        for bad in [0u32, 3, 5, 6, 7, 9, 16] {
            assert!(
                matches!(
                    decode_exit(&mmio_load(0x0900_0000, bad)),
                    Err(BackendError::Internal(_))
                ),
                "MMIO len {bad} must fail closed"
            );
            assert!(
                matches!(
                    decode_exit(&mmio_store(0x0900_0000, 0, bad)),
                    Err(BackendError::Internal(_))
                ),
                "MMIO store len {bad} must fail closed"
            );
        }
        for ok in [1u32, 2, 4, 8] {
            assert!(decode_exit(&mmio_load(0x0900_0000, ok)).is_ok());
        }
    }

    #[test]
    fn stock_surface_decodes_mmio_and_shutdown_only() {
        let (exit, pending) = decode_exit(&mmio_store(0x0900_0000, 0x5A, 4))
            .unwrap()
            .unwrap();
        assert_eq!(
            exit,
            CommonExit::Mmio {
                gpa: Gpa(0x0900_0000),
                size: 4,
                write: Some(0x5A),
            }
            .into()
        );
        assert_eq!(pending, Pending::None);

        let (exit, pending) = decode_exit(&mmio_load(0x0900_0000, 4)).unwrap().unwrap();
        assert!(matches!(
            exit,
            Exit::Common(CommonExit::Mmio { write: None, .. })
        ));
        assert_eq!(pending, Pending::MmioLoad { len: 4 });

        let view = KvmRunView {
            exit_reason: KVM_EXIT_SYSTEM_EVENT,
            system_event_type: KVM_SYSTEM_EVENT_SHUTDOWN,
            ..Default::default()
        };
        let (exit, _) = decode_exit(&view).unwrap().unwrap();
        assert_eq!(exit, CommonExit::Shutdown.into());
    }

    #[test]
    fn control_and_failclosed_reasons() {
        let view = KvmRunView {
            exit_reason: KVM_EXIT_INTR,
            ..Default::default()
        };
        assert_eq!(decode_exit(&view).unwrap(), None);
        for reason in [KVM_EXIT_FAIL_ENTRY, KVM_EXIT_INTERNAL_ERROR, 0xDEAD] {
            let view = KvmRunView {
                exit_reason: reason,
                ..Default::default()
            };
            assert!(matches!(decode_exit(&view), Err(BackendError::Internal(_))));
        }
    }

    #[test]
    fn patched_arms_exist_but_are_never_stock() {
        let view = KvmRunView {
            exit_reason: KVM_EXIT_ARM_WFX_PLACEHOLDER,
            ..Default::default()
        };
        assert_eq!(
            decode_exit(&view).unwrap().unwrap().0,
            CommonExit::Idle.into()
        );
        let view = KvmRunView {
            exit_reason: KVM_EXIT_HYPERCALL,
            hypercall_args: [0x3150_4348, 1, 2, 3],
            ..Default::default()
        };
        assert!(matches!(
            decode_exit(&view).unwrap().unwrap().0,
            Exit::Common(CommonExit::Hypercall(_))
        ));
        let view = KvmRunView {
            exit_reason: KVM_EXIT_ARM_SYSREG_PLACEHOLDER,
            sysreg: (0x1234, Some(7)),
            ..Default::default()
        };
        let (exit, pending) = decode_exit(&view).unwrap().unwrap();
        assert!(matches!(
            exit,
            Exit::Arch(crate::arch::arm64::Arm64Exit::Sysreg { .. })
        ));
        assert_eq!(pending, Pending::SysregWrite);
    }

    #[test]
    fn vcpu_init_advertises_psci_0_2() {
        let f = vcpu_init_features();
        assert_eq!(
            f[0] & (1 << KVM_ARM_VCPU_PSCI_0_2),
            1 << KVM_ARM_VCPU_PSCI_0_2,
            "PSCI 0.2 feature bit must be set"
        );
        assert_eq!(
            f[1..],
            [0u32; 6],
            "the skeleton opts into no other vcpu feature"
        );

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        assert!(fake.calls.contains(&"vcpu_init"));
        assert_eq!(fake.init_features, vcpu_init_features());
        assert_ne!(fake.init_features[0] & (1 << KVM_ARM_VCPU_PSCI_0_2), 0);
    }

    #[test]
    fn backend_orders_ioctls_and_installs_policy() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);

        assert!(matches!(b.run(), Err(BackendError::NotConfigured)));

        let mut policy = Arm64Policy {
            id_regs: IdRegModel::default(),
            ..Default::default()
        };
        let enc = (3u32 << 14) | (4 << 3);
        policy.id_regs.regs.insert(enc, 0x1122_3344);
        b.set_policy(&policy).unwrap();

        let calls = &b.kvm().calls;
        let init_pos = calls.iter().position(|c| *c == "vcpu_init").unwrap();
        let first_set = calls.iter().position(|c| *c == "set_one_reg").unwrap();
        assert!(
            init_pos < first_set,
            "vcpu_init must precede set_one_reg: {calls:?}"
        );
        let id = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG | u64::from(enc);
        assert_eq!(b.kvm().reg(id), Some(0x1122_3344));
        assert_eq!(
            b.kvm().reg(KVM_REG_ARM_PSCI_VERSION),
            Some(KVM_ARM_PSCI_1_0),
            "PSCI must be pinned to the portable 1.0 firmware contract"
        );
        for fw_id in OPTIONAL_FIRMWARE_BITMAPS {
            assert_eq!(
                b.kvm().reg(fw_id),
                Some(0),
                "optional SMCCC firmware bitmap {fw_id:#x} must be denied"
            );
        }
    }

    #[test]
    fn default_policy_denies_host_rng_time_and_vendor_firmware_services() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_one_reg(CNTV_CTL_EL0, 1).unwrap();
        let mut backend = Arm64KvmBackend::new(fake);

        backend.set_policy(&Arm64Policy::default()).unwrap();

        for id in OPTIONAL_FIRMWARE_BITMAPS {
            assert_eq!(backend.kvm().reg(id), Some(0));
        }
        assert_eq!(backend.kvm().reg(CNTV_CTL_EL0), Some(0));
    }

    #[test]
    fn firmware_workaround_levels_do_not_restrict_host_admission() {
        for (index, id) in SMCCC_WORKAROUND_REGISTERS.into_iter().enumerate() {
            let expected = crate::arch::arm64::Arm64Caps::FIXED_SMCCC_WORKAROUND_LEVELS[index];
            let mut fake = FakeKvm::new();
            fake.vcpu_init().unwrap();
            let different = if expected == 0 { 3 } else { 0 };
            fake.set_one_reg(id, different).unwrap();
            let mut backend = Arm64KvmBackend::new(fake);
            backend.set_policy(&Arm64Policy::default()).unwrap();
            assert_eq!(
                backend.capabilities().arch.smccc_workaround_levels[index],
                different
            );
        }
    }

    #[test]
    fn save_restore_round_trips_through_the_reg_table() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        let mut s = Arm64VcpuState::default();
        s.core.x[0] = 0x4000_0000;
        s.core.x[30] = 0xDEAD;
        s.core.pc = 0x0020_0000;
        s.core.pstate = 0x3c5;
        s.core.sp_el1 = 0x8_0000;
        s.sysregs.sctlr_el1 = 0x30d0_0800;
        s.sysregs.cntkctl_el1 = 3;
        s.sysregs.contextidr_el1 = 4660;
        s.sysregs.amair_el1 = 4661;
        s.sysregs.par_el1 = 4662;
        s.sysregs.csselr_el1 = 4663;
        s.sysregs.afsr0_el1 = 4664;
        s.sysregs.afsr1_el1 = 4665;
        s.simd_fp.q[0] = [0xA5; 16];
        s.simd_fp.q[31] = [0x5A; 16];
        s.simd_fp.fpcr = 0x0040_0000;
        s.simd_fp.fpsr = 0x0800_0000;
        s.debug.breakpoint_value[0] = 0x1234;
        s.debug.breakpoint_control[0] = 1;
        s.debug.watchpoint_value[15] = 0x5678;
        s.debug.watchpoint_control[15] = 1;
        s.debug.mdscr_el1 = 0x8000;
        s.vtimer.cntv_ctl_el0 = 2;
        s.vtimer.cntv_cval_el0 = 0x1234_5678;
        s.vtimer.masked = true;
        s.mp_state = MpState::Halted;
        s.gic = Some(save_vgic(b.kvm()).unwrap());

        b.restore(&s).unwrap();
        assert_eq!(b.save().unwrap(), s);
    }

    #[test]
    fn restore_rejects_completion_state_and_clears_interrupt_latches_when_clean() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let state = b.save().unwrap();
        let intid = GicIntId(32);

        b.pending = Pending::SysregRead;
        b.pending_irq = Some(intid);
        b.applied_irq = Some(intid);
        b.accepted_irq = Some(intid);
        b.reported_active_irq = Some(intid);

        assert!(b.validate_restore_state(&state).is_ok());
        assert!(matches!(
            b.restore(&state),
            Err(BackendError::PendingCompletion)
        ));
        b.pending = Pending::None;
        b.completion_staged = true;
        assert!(b.validate_restore_state(&state).is_ok());
        assert!(matches!(
            b.restore(&state),
            Err(BackendError::PendingCompletion)
        ));
        b.completion_staged = false;
        b.restore(&state).unwrap();
        assert_eq!(b.pending, Pending::None);
        assert!(!b.completion_staged);
        assert_eq!(b.pending_irq, None);
        assert_eq!(b.applied_irq, None);
        assert_eq!(b.accepted_irq, None);
        assert_eq!(b.reported_active_irq, None);
    }

    #[test]
    fn save_rejects_pending_or_staged_completion_without_kvm_access() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let state = b.save().unwrap();
        let calls = b.kvm().calls.clone();
        let state_reads = b
            .kvm()
            .state_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(state_reads > 0);

        for pending in [Pending::SysregRead, Pending::SysregWrite] {
            b.pending = pending;
            b.completion_staged = false;
            assert!(matches!(b.save(), Err(BackendError::PendingCompletion)));
            assert_eq!(b.pending, pending);
            assert!(!b.completion_staged);
            assert_eq!(b.kvm().calls, calls);
            assert_eq!(
                b.kvm()
                    .state_reads
                    .load(std::sync::atomic::Ordering::Relaxed),
                state_reads
            );
        }

        b.pending = Pending::None;
        b.completion_staged = true;
        assert!(matches!(b.save(), Err(BackendError::PendingCompletion)));
        assert_eq!(b.pending, Pending::None);
        assert!(b.completion_staged);
        assert_eq!(b.kvm().calls, calls);
        assert_eq!(
            b.kvm()
                .state_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            state_reads
        );

        b.completion_staged = false;
        assert_eq!(b.save().unwrap(), state);
    }

    #[test]
    fn irq_mask_read_uses_one_register_and_observes_completed_mmio() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_one_reg(core_reg(CORE_PSTATE), 0x3c5).unwrap();
        let mut backend = Arm64KvmBackend::new(fake);
        let before = backend
            .kvm()
            .state_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(backend.read_irq_mask().unwrap(), Some(true));
        let after = backend
            .kvm()
            .state_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after - before, 1);
        assert!(backend.save().unwrap().core.pstate & PSTATE_I != 0);
        let after_save = backend
            .kvm()
            .state_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert!(after_save - after > 100);

        backend
            .kvm
            .set_one_reg(core_reg(CORE_PSTATE), 0x345)
            .unwrap();
        backend.mmio_owed = true;
        assert_eq!(backend.read_irq_mask().unwrap(), Some(false));
        assert_eq!(backend.kvm().calls.last(), Some(&"complete_mmio_exit"));
        assert!(!backend.mmio_owed);

        let reads = backend
            .kvm()
            .state_reads
            .load(std::sync::atomic::Ordering::Relaxed);
        backend.pending = Pending::SysregRead;
        assert!(matches!(
            backend.read_irq_mask(),
            Err(BackendError::PendingCompletion)
        ));
        backend.pending = Pending::None;
        backend.completion_staged = true;
        assert!(matches!(
            backend.read_irq_mask(),
            Err(BackendError::PendingCompletion)
        ));
        assert_eq!(
            backend
                .kvm()
                .state_reads
                .load(std::sync::atomic::Ordering::Relaxed),
            reads
        );
    }

    #[test]
    fn save_accepts_completed_mmio_and_eager_mmio_write_states() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_load(0x0900_0018, 4));
        fake.push_run(mmio_store(0x0900_0020, 0x5A, 1));
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        let initial = b.save().unwrap();
        assert!(matches!(
            b.run(),
            Ok(Exit::Common(CommonExit::Mmio { write: None, .. }))
        ));
        assert!(matches!(b.save(), Err(BackendError::PendingCompletion)));

        b.complete_read(0x90).unwrap();
        assert_eq!(b.save().unwrap(), initial);
        assert!(matches!(
            b.run(),
            Ok(Exit::Common(CommonExit::Mmio { write: Some(_), .. }))
        ));
        assert_eq!(b.save().unwrap(), initial);
    }

    #[test]
    fn save_strips_and_restore_rejects_host_pstate_residue() {
        const TCO: u64 = 1 << 25;
        const BTYPE: u64 = 0b11 << 10;
        const TCR_AS: u64 = 1 << 36;
        const SCTLR_EPAN: u64 = 1 << 57;
        const SCTLR_ITFSB: u64 = 1 << 37;

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_one_reg(core_reg(CORE_PSTATE), 0xc5 | TCO | BTYPE)
            .unwrap();
        fake.set_one_reg(core_reg(CORE_SPSR_EL1), 0x6000_0005 | TCO | BTYPE)
            .unwrap();
        fake.set_one_reg(SYSREGS[0].0, 0x1234 | SCTLR_EPAN | SCTLR_ITFSB)
            .unwrap();
        fake.set_one_reg(SYSREGS[3].0, 0x5678 | TCR_AS).unwrap();

        let saved = save_vcpu(&fake).unwrap();
        assert_eq!(saved.core.pstate, 0xc5);
        assert_eq!(saved.core.spsr_el1, 0x6000_0005);
        assert_eq!(saved.sysregs.sctlr_el1, 0x1234 | SCTLR_EPAN | SCTLR_ITFSB);
        assert_eq!(saved.sysregs.tcr_el1, 0x5678 | TCR_AS);

        let mut destination = FakeKvm::new();
        destination.vcpu_init().unwrap();
        restore_vcpu(&mut destination, &saved).unwrap();
        assert_eq!(save_vcpu(&destination).unwrap().sysregs, saved.sysregs);

        let mut noncanonical = saved;
        noncanonical.core.pstate |= TCO | BTYPE;
        assert!(matches!(
            restore_vcpu(&mut fake, &noncanonical),
            Err(BackendError::InvalidState)
        ));
    }

    #[test]
    fn save_rejects_an_enabled_architected_virtual_timer() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_one_reg(CNTV_CTL_EL0, 1).unwrap();

        assert!(matches!(save_vcpu(&fake), Err(BackendError::InvalidState)));
    }

    #[test]
    fn restore_vcpu_rejects_each_noncanonical_field_independently() {
        let mut source = FakeKvm::new();
        source.vcpu_init().unwrap();
        let valid = save_vcpu(&source).unwrap();

        let mut trap_debug_exceptions = valid;
        trap_debug_exceptions.debug.trap_debug_exceptions = true;
        let mut trap_debug_reg_accesses = valid;
        trap_debug_reg_accesses.debug.trap_debug_reg_accesses = true;
        let mut timer_unmasked = valid;
        timer_unmasked.vtimer.masked = false;
        let mut timer_ctl_reserved = valid;
        timer_ctl_reserved.vtimer.cntv_ctl_el0 = 1 << 2;
        let mut timer_ctl_enabled = valid;
        timer_ctl_enabled.vtimer.cntv_ctl_el0 = 1;
        let mut irq = valid;
        irq.interrupts.irq = true;
        let mut fiq = valid;
        fiq.interrupts.fiq = true;

        for (name, invalid) in [
            ("trap-debug-exceptions", trap_debug_exceptions),
            ("trap-debug-register-accesses", trap_debug_reg_accesses),
            ("host-timer-unmasked", timer_unmasked),
            ("host-timer-control-reserved", timer_ctl_reserved),
            ("host-timer-control-enabled", timer_ctl_enabled),
            ("pending IRQ", irq),
            ("pending FIQ", fiq),
        ] {
            let mut target = FakeKvm::new();
            target.vcpu_init().unwrap();
            assert!(
                matches!(
                    restore_vcpu(&mut target, &invalid),
                    Err(BackendError::InvalidState)
                ),
                "restore accepted noncanonical {name}"
            );
        }
    }

    #[test]
    fn backend_restore_validation_rejects_shape_without_kvm_access() {
        let mut source = FakeKvm::new();
        source.vcpu_init().unwrap();
        let valid = save_vcpu(&source).unwrap();

        let mut target = FakeKvm::new();
        target.vcpu_init().unwrap();
        target.calls.clear();
        let backend = Arm64KvmBackend::new(target);

        assert!(backend.validate_restore_state(&valid).is_ok());
        assert!(backend.kvm().calls.is_empty());

        for invalid in [
            Arm64VcpuState {
                core: Arm64CoreRegs {
                    pstate: 1 << 25,
                    ..valid.core
                },
                ..valid
            },
            Arm64VcpuState {
                simd_fp: Arm64SimdFpState {
                    fpsr: u64::from(u32::MAX) + 1,
                    ..valid.simd_fp
                },
                ..valid
            },
            Arm64VcpuState { gic: None, ..valid },
        ] {
            assert!(matches!(
                backend.validate_restore_state(&invalid),
                Err(BackendError::InvalidState)
            ));
        }
        assert!(backend.kvm().calls.is_empty());
    }

    #[test]
    fn mmio_load_completion_stages_data_for_the_next_run() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_load(0x0900_0018, 4));
        fake.push_run(KvmRunView {
            exit_reason: KVM_EXIT_SYSTEM_EVENT,
            system_event_type: KVM_SYSTEM_EVENT_SHUTDOWN,
            ..Default::default()
        });
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        let exit = b.run().unwrap();
        assert!(matches!(
            exit,
            Exit::Common(CommonExit::Mmio { write: None, .. })
        ));
        assert!(matches!(b.run(), Err(BackendError::PendingCompletion)));
        assert_eq!(
            b.kvm()
                .calls
                .iter()
                .filter(|&&call| call == "complete_mmio_exit")
                .count(),
            0,
            "an MMIO read is completed only after complete_read supplies data"
        );
        b.complete_read(0x90).unwrap();
        assert_eq!(b.kvm().calls.last(), Some(&"write_mmio_data"));
        let exit = b.run().unwrap();
        assert_eq!(exit, CommonExit::Shutdown.into());
        assert_eq!(b.kvm().last_mmio_data, Some(le_data(0x90, 4)));
        assert!(
            b.kvm()
                .calls
                .windows(2)
                .any(|calls| calls == ["write_mmio_data", "run"])
        );
        assert_eq!(completions(&b), 0, "the next run completes the load");
    }

    #[test]
    fn no_staged_arm64_completion_is_a_noop() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        b.retire_pending_completion().unwrap();
        assert_eq!(
            b.kvm().calls.iter().filter(|&&call| call == "run").count(),
            0,
            "a no-pending retirement must not enter KVM"
        );
    }

    #[test]
    fn staged_arm64_sysreg_completion_fails_closed_without_running_guest() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(KvmRunView {
            exit_reason: KVM_EXIT_ARM_SYSREG_PLACEHOLDER,
            sysreg: (0x1234, None),
            ..Default::default()
        });
        fake.push_run(KvmRunView {
            exit_reason: KVM_EXIT_SYSTEM_EVENT,
            system_event_type: KVM_SYSTEM_EVENT_SHUTDOWN,
            ..Default::default()
        });
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        assert!(matches!(
            b.run(),
            Ok(Exit::Arch(crate::arch::arm64::Arm64Exit::Sysreg {
                write: None,
                ..
            }))
        ));
        assert!(matches!(
            b.complete_read(0x90),
            Err(BackendError::Unsupported {
                what: "complete_read (arm64 sysreg)"
            })
        ));
        assert_eq!(b.pending, Pending::SysregRead);
        assert!(matches!(b.retire_pending_completion(), Ok(())));
        assert_eq!(
            b.kvm().calls.iter().filter(|&&call| call == "run").count(),
            1,
            "an unretirable sysreg completion must not run the guest"
        );
        assert!(matches!(b.run(), Err(BackendError::PendingCompletion)));
    }

    #[test]
    fn patched_sysreg_fault_and_ok_completions_resolve_their_pending_exit() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        b.pending = Pending::SysregRead;
        b.complete_fault().unwrap();
        assert_eq!(b.pending, Pending::None);
        assert!(b.completion_staged);

        b.pending = Pending::SysregWrite;
        b.completion_staged = false;
        b.complete_ok().unwrap();
        assert_eq!(b.pending, Pending::None);
        assert!(b.completion_staged);
    }

    #[test]
    fn arm64_retirement_rejects_a_staged_sysreg_completion() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        b.completion_staged = true;

        assert!(matches!(
            b.retire_pending_completion(),
            Err(BackendError::Unsupported {
                what: "retire_pending_completion (arm64 sysreg)"
            })
        ));
    }

    #[test]
    fn ensure_runnable_rejects_a_staged_completion_latch() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        b.completion_staged = true;
        assert!(matches!(
            b.ensure_runnable(),
            Err(BackendError::PendingCompletion)
        ));
    }

    fn completions(b: &Arm64KvmBackend<FakeKvm>) -> usize {
        b.kvm()
            .calls
            .iter()
            .filter(|&&call| call == "complete_mmio_exit")
            .count()
    }

    #[test]
    fn an_mmio_write_completes_in_the_next_run_or_before_state_is_read() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'!'), 1));
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'?'), 1));
        fake.push_run(KvmRunView {
            exit_reason: KVM_EXIT_SYSTEM_EVENT,
            system_event_type: KVM_SYSTEM_EVENT_SHUTDOWN,
            ..Default::default()
        });
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        b.run().unwrap();
        b.run().unwrap();
        assert_eq!(
            completions(&b),
            0,
            "the second run completed the first store"
        );
        let state = b.save().unwrap();
        assert_eq!(completions(&b), 1);
        b.save().unwrap();
        b.restore(&state).unwrap();
        assert_eq!(completions(&b), 1, "one completion per store");
        assert_eq!(b.store_completions().runs, 1);
        assert_eq!(b.run().unwrap(), CommonExit::Shutdown.into());
        assert_eq!(completions(&b), 1);
    }

    #[test]
    fn a_restore_completes_an_owed_store_before_writing_state() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'!'), 1));
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let state = b.save().unwrap();

        b.run().unwrap();
        b.restore(&state).unwrap();
        let calls = &b.kvm().calls;
        let completed = calls
            .iter()
            .position(|&call| call == "complete_mmio_exit")
            .unwrap();
        let restored = calls
            .iter()
            .rposition(|&call| call == "set_one_reg")
            .unwrap();
        assert!(completed < restored);
    }

    #[test]
    fn only_mmio_writes_are_completed_during_exit_decode() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'!'), 1));
        fake.push_run(KvmRunView {
            exit_reason: KVM_EXIT_SYSTEM_EVENT,
            system_event_type: KVM_SYSTEM_EVENT_SHUTDOWN,
            ..Default::default()
        });
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        assert!(matches!(
            b.run().unwrap(),
            Exit::Common(CommonExit::Mmio { write: Some(_), .. })
        ));
        assert_eq!(b.run().unwrap(), CommonExit::Shutdown.into());
        b.save().unwrap();
        assert_eq!(
            completions(&b),
            0,
            "non-MMIO exits must not complete a stale MMIO transaction"
        );
    }

    #[test]
    fn map_memory_forwards_a_validated_region_through_the_seam() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        let mut ram = vec![0u8; 2 * 4096];
        unsafe { b.map_memory(Gpa(0x4000_0000), &mut ram).unwrap() };
        assert_eq!(b.kvm().memslots, vec![(0, 0x4000_0000, 8192)]);

        let mut empty: Vec<u8> = Vec::new();
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x4000_0000), &mut empty) },
            Err(BackendError::Memory(_))
        ));
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x1), &mut ram) },
            Err(BackendError::Memory(_))
        ));

        let mut ram2 = vec![0u8; 4096];
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x4000_0000), &mut ram2) },
            Err(BackendError::Memory(_))
        ));
        assert!(
            matches!(
                unsafe { b.map_memory(Gpa(0x4000_1000), &mut ram2) },
                Err(BackendError::Memory(_))
            ),
            "an overlapping region must fail closed, not replace slot 0"
        );
        unsafe { b.map_memory(Gpa(0x4000_2000), &mut ram2).unwrap() };
        assert_eq!(
            b.kvm().memslots,
            vec![(0, 0x4000_0000, 8192), (1, 0x4000_2000, 4096)],
            "each region gets a unique slot; nothing was replaced"
        );
    }

    #[test]
    fn vgic_irq_acceptance_positive_and_planted_negative() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_accept_irqs(true);
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'!'), 1));
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        b.set_pending_irq(Some(GicIntId(27))).unwrap();
        let asserted = b.save().unwrap().gic.unwrap();
        assert_ne!(
            asserted.line_level[0] & (1 << 27),
            0,
            "the serviced-exit boundary must expose the asserted architectural line; \
             a one-entry-late application is the negative control"
        );
        assert!(matches!(
            b.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(b.take_accepted_interrupt(), Some(GicIntId(27)));
        assert_eq!(b.take_accepted_interrupt(), None);
        assert_eq!(
            b.save().unwrap().gic.unwrap().active[0],
            1 << 27,
            "the fake kernel must promote only the asserted private line"
        );

        b.kvm.accept_irqs = false;
        b.kvm.vgic_attrs.insert(
            (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, GICR_ISACTIVER0, false),
            0,
        );
        b.kvm.push_run(mmio_store(0x0900_0000, u64::from(b'-'), 1));
        assert!(matches!(
            b.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(b.take_accepted_interrupt(), None);
        b.kvm.accept_irqs = true;
        b.kvm.push_run(mmio_store(0x0900_0000, u64::from(b'+'), 1));
        assert!(matches!(
            b.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(b.take_accepted_interrupt(), Some(GicIntId(27)));

        b.set_pending_irq(None).unwrap();
        let lowered = b.save().unwrap().gic.unwrap();
        assert_eq!(lowered.line_level[0] & (1 << 27), 0);

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_accept_irqs(true);
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'S'), 1));
        let mut spi = Arm64KvmBackend::new(fake);
        spi.set_policy(&Arm64Policy::default()).unwrap();
        spi.set_pending_irq(Some(GicIntId(65))).unwrap();
        assert!(matches!(
            spi.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(spi.take_accepted_interrupt(), Some(GicIntId(65)));
        assert!(matches!(spi.save(), Err(BackendError::Unsupported { .. })));
        spi.set_pending_irq(None).unwrap();
        assert_eq!(spi.save().unwrap().gic.unwrap().active[2], 1 << 1);

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_accept_irqs(false);
        fake.push_run(mmio_store(0x0900_0000, u64::from(b'!'), 1));
        let mut mutant = Arm64KvmBackend::new(fake);
        mutant.set_policy(&Arm64Policy::default()).unwrap();
        mutant.set_pending_irq(Some(GicIntId(27))).unwrap();
        assert!(matches!(
            mutant.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(mutant.take_accepted_interrupt(), None);
    }

    #[test]
    fn stock_vgic_rejects_invalid_interrupts() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        b.inject(crate::arch::arm64::Arm64Injection::Interrupt {
            intid: GicIntId(30),
        })
        .unwrap();
        assert_ne!(
            b.save().unwrap().gic.unwrap().line_level[0] & (1 << 30),
            0,
            "inject must route through the same level-setting contract"
        );
        assert!(matches!(
            b.inject(crate::arch::arm64::Arm64Injection::Interrupt { intid: GicIntId(1) }),
            Err(BackendError::InvalidState)
        ));
        b.set_pending_irq(Some(GicIntId(27))).unwrap();
        assert!(matches!(
            b.set_pending_irq(Some(GicIntId(1))),
            Err(BackendError::InvalidState)
        ));
        assert_eq!(b.take_accepted_interrupt(), None);

        let caps = b.capabilities();
        assert_eq!(caps.name, "kvm-arm64-vgicv3");
        assert!(caps.arch.in_kernel_gic);
    }

    #[test]
    fn capabilities_report_the_host_asid_width() {
        for asid_bits in [Arm64AsidBits::Eight, Arm64AsidBits::Sixteen] {
            let mut fake = FakeKvm::new();
            fake.set_asid_bits(asid_bits);
            let b = Arm64KvmBackend::new(fake);
            assert_eq!(b.capabilities().arch.asid_bits, asid_bits);
        }
    }

    #[test]
    fn dirty_log_registration_sets_flags_and_tracks_arm_slot_metadata() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut first = vec![0u8; 2 * 4096];
        let mut second = vec![0u8; 4096];

        unsafe { b.map_memory(Gpa(0x2_0000_0000), &mut first).unwrap() };
        unsafe { b.map_memory(Gpa(0x4_0000_0000), &mut second).unwrap() };

        assert_eq!(
            b.kvm().memslots,
            vec![(0, 0x2_0000_0000, 2 * 4096), (1, 0x4_0000_0000, 4096)]
        );
        assert_eq!(
            b.kvm().memslot_flags,
            vec![(0, KVM_MEM_LOG_DIRTY_PAGES), (1, KVM_MEM_LOG_DIRTY_PAGES)]
        );
        assert_eq!(
            b.dirty_slots,
            vec![(0, 0x2_0000_0000, 2 * 4096), (1, 0x4_0000_0000, 4096)]
        );
    }

    #[test]
    fn dirty_bitmap_helpers_pin_zero_alignment_and_word_arithmetic() {
        assert!(matches!(
            dirty_bitmap_words(0),
            Err(BackendError::InvalidState)
        ));
        assert!(matches!(
            dirty_bitmap_words(4097),
            Err(BackendError::InvalidState)
        ));
        assert_eq!(dirty_bitmap_words(4096).unwrap(), 1);
        assert_eq!(dirty_bitmap_words(65 * 4096).unwrap(), 2);

        assert_eq!(dirty_clear_max_words(129).unwrap(), 2);
    }

    #[test]
    fn clear_dirty_log_rejects_each_independent_alignment_guard() {
        assert!(matches!(
            validate_clear_dirty_log(64 * 4096, 0, 0, &[]),
            Err(BackendError::InvalidState)
        ));
        assert!(matches!(
            validate_clear_dirty_log(64 * 4096, 1, 63, &[1]),
            Err(BackendError::InvalidState)
        ));
    }

    #[test]
    fn dirty_bitmap_decoder_ignores_padding_bits_in_the_final_word() {
        let mut gfns = Vec::new();
        decode_dirty_bitmap(
            0x1000,
            3 * 4096,
            &[(1 << 2) | (1 << 3) | (1 << 63)],
            &mut gfns,
        )
        .unwrap();
        assert_eq!(gfns, vec![1 + 2]);
    }

    #[test]
    fn dirty_clear_chunks_use_the_full_64_word_limit() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![u64::MAX; 64]);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut ram = vec![0u8; 4096 * 64 * 64];
        unsafe { b.map_memory(Gpa(0xA_0000_0000), &mut ram).unwrap() };

        let dirty = b.drain_dirty_pages().unwrap();
        assert_eq!(dirty.len(), 4096);
        assert_eq!(b.kvm().dirty_clear_calls.len(), 1);
        assert_eq!(b.kvm().dirty_clear_calls[0].2, 0);
        assert_eq!(b.kvm().dirty_clear_calls[0].3, 4096);
    }

    #[test]
    fn dirty_log_decodes_high_arm_gpas_sorted_and_deduplicated() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![0b101]);
        fake.set_dirty_bitmap(1, vec![0b001]);
        fake.set_dirty_bitmap(2, vec![0b101]);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut high = vec![0u8; 3 * 4096];
        let mut higher = vec![0u8; 4096];
        unsafe { b.map_memory(Gpa(0x5_0000_0000), &mut high).unwrap() };
        unsafe { b.map_memory(Gpa(0x4_0000_0000), &mut higher).unwrap() };
        b.dirty_slots.push((2, 0x4_0000_0000, 3 * 4096));

        assert_eq!(
            b.drain_dirty_pages().unwrap(),
            vec![
                0x4_0000_0000 / 4096,
                0x4_0000_0000 / 4096 + 2,
                0x5_0000_0000 / 4096,
                0x5_0000_0000 / 4096 + 2,
            ]
        );
    }

    #[test]
    fn dirty_log_manual_get_requires_clear_and_repeat_drain_is_empty() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![1 << 1]);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut ram = vec![0u8; 2 * 4096];
        unsafe { b.map_memory(Gpa(0x3_0000_0000), &mut ram).unwrap() };

        assert_eq!(
            b.drain_dirty_pages().unwrap(),
            vec![0x3_0000_0000 / 4096 + 1]
        );
        assert!(b.drain_dirty_pages().unwrap().is_empty());
        assert_eq!(b.kvm().dirty_log_calls, vec![(0, 2 * 4096), (0, 2 * 4096)]);
        assert_eq!(
            b.kvm().dirty_clear_calls,
            vec![(0, 2 * 4096, 0, 2, vec![1 << 1])]
        );
    }

    #[test]
    fn dirty_log_clears_only_maximal_sparse_runs_and_final_partial_word() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![1 << 1, 0, 1 << 3, 1]);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut ram = vec![0u8; 193 * 4096];
        unsafe { b.map_memory(Gpa(0x8_0000_0000), &mut ram).unwrap() };

        assert_eq!(
            b.drain_dirty_pages().unwrap(),
            vec![
                0x8_0000_0000 / 4096 + 1,
                0x8_0000_0000 / 4096 + 2 * 64 + 3,
                0x8_0000_0000 / 4096 + 3 * 64,
            ]
        );
        assert_eq!(
            b.kvm().dirty_clear_calls,
            vec![
                (0, 193 * 4096, 0, 64, vec![1 << 1]),
                (0, 193 * 4096, 128, 65, vec![1 << 3, 1]),
            ]
        );
        assert!(b.drain_dirty_pages().unwrap().is_empty());
        assert_eq!(b.kvm().dirty_clear_calls.len(), 2);
    }

    #[test]
    fn dirty_log_clear_rejects_malformed_shape_without_panicking() {
        let mut fake = FakeKvm::new();
        let size = 65 * 4096;
        assert!(matches!(
            fake.clear_dirty_log(0, size, 1, 1, &[1]),
            Err(BackendError::InvalidState)
        ));
        assert!(matches!(
            fake.clear_dirty_log(0, size, 64, 1, &[]),
            Err(BackendError::Internal(
                "KVM clear dirty bitmap has an unexpected size"
            ))
        ));
        assert!(matches!(
            fake.clear_dirty_log(0, size, 64, 64, &[1]),
            Err(BackendError::InvalidState)
        ));
        assert!(matches!(
            fake.clear_dirty_log(0, 130 * 4096, 0, 1, &[1]),
            Err(BackendError::InvalidState)
        ));
        assert!(fake.dirty_clear_calls.is_empty());
    }

    #[test]
    fn dirty_log_clear_error_never_exposes_partial_result() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![1, 0, 1]);
        fake.fail_dirty_clear_on_call(2);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut ram = vec![0u8; 129 * 4096];
        unsafe { b.map_memory(Gpa(0x9_0000_0000), &mut ram).unwrap() };

        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Internal("fake KVM dirty-log clear failed"))
        ));
        assert_eq!(b.kvm().dirty_clear_calls.len(), 2);
        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Internal(
                "arm64 dirty log is poisoned after a partial clear"
            ))
        ));
        assert_eq!(
            b.kvm().dirty_clear_calls.len(),
            2,
            "a poisoned drain must not touch a bitmap whose earlier bits were cleared"
        );
    }

    #[test]
    fn dirty_log_disabled_registration_rejects_all_future_drains() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        b.dirty_log = false;
        let mut ram = vec![0u8; 4096];
        unsafe { b.map_memory(Gpa(0x5_0000_0000), &mut ram).unwrap() };
        assert_eq!(b.kvm().memslot_flags, vec![(0, 0)]);
        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Unsupported { .. })
        ));

        b.dirty_log = true;
        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Unsupported { .. })
        ));
    }

    #[test]
    fn dirty_log_mid_drain_error_never_returns_partial_data() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_dirty_bitmap(0, vec![1]);
        fake.set_dirty_bitmap(1, vec![1]);
        fake.fail_dirty_log_on_call(2);
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();
        let mut first = vec![0u8; 4096];
        let mut second = vec![0u8; 4096];
        unsafe { b.map_memory(Gpa(0x6_0000_0000), &mut first).unwrap() };
        unsafe { b.map_memory(Gpa(0x7_0000_0000), &mut second).unwrap() };

        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Internal(
                "fake KVM dirty-log retrieval failed"
            ))
        ));
        assert_eq!(b.kvm().dirty_log_calls, vec![(0, 4096), (1, 4096)]);
        assert!(matches!(
            b.drain_dirty_pages(),
            Err(BackendError::Internal(
                "arm64 dirty log is poisoned after a partial clear"
            ))
        ));
        assert_eq!(b.kvm().dirty_log_calls, vec![(0, 4096), (1, 4096)]);
    }

    #[test]
    fn host_page_size_must_match_the_dirty_log_geometry() {
        assert!(require_4k_host_page_size(4096).is_ok());
        for page_size in [-1, 0, 16_384, 65_536] {
            assert!(matches!(
                require_4k_host_page_size(page_size),
                Err(BackendError::Capability {
                    cap: "4 KiB host page size (_SC_PAGESIZE == 4096)"
                })
            ));
        }
    }
}
