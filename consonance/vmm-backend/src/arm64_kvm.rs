// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **stock KVM/arm64 backend** (`tasks/112` M4), split like the x86 backend
//! into a *pure* half (this module — the `KVM_RUN`⇄[`Exit`] decode, the register
//! save/restore table, and the [`Backend`] orchestration over a thin syscall
//! seam) and a *box-only* half (`arm64_kvm_sys` — the real ioctls, gated
//! `all(target_os = "linux", target_arch = "aarch64")`).
//!
//! **The syscall boundary is a trait** ([`Arm64Kvm`]), so the whole backend —
//! including the ioctl *ordering* (`KVM_ARM_VCPU_INIT` before the first
//! `KVM_SET_ONE_REG`, policy-before-run, map-before-restore) — is asserted
//! portably against a recording fake ([`FakeKvm`]) on the Mac and under Miri,
//! with no `/dev/kvm` (`docs/VM-EXIT-COUNT-VTIME.md`: mechanism attestation
//! without the hardware). The real ioctl path against `/dev/kvm` has
//! **no local oracle** — it runs natively on msr1 during M4 (`hm-7pb`; the
//! Mac has no local KVM loop, `hm-8l3` REFUSE).
//!
//! **The stock/patched split is load-bearing and honest** (mirroring x86, where
//! stock surfaces Io/Mmio/MSR/Shutdown and the Hypercall/Cpuid/instruction exits
//! are patched-only). On the **stock** backend `run` returns **only**
//! `Mmio`/`Shutdown`; every other decode arm is patched-ABI
//! (`// TODO(patched-abi)`, for the AA-3 backend) and the stock hardware never
//! reaches it. Interrupt injection, `run_until`, and the trap-group *enforcement*
//! of the policy are all `Unsupported`/AA-gated — the skeleton claims no
//! determinism (`capabilities()` reports every field honestly `false`).

use crate::arch::arm64::{
    Arm64, Arm64GicState, Arm64VcpuState, GicIntId, canonicalize_core_regs,
    has_noncanonical_core_regs,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, CommonExit, Exit, ExitCounts};
use crate::types::{Gpa, MpState};

// --- documented KVM ABI constants (the exit reasons the decode maps) ---------
// Values from the Linux `uapi/linux/kvm.h` `KVM_EXIT_*` / `KVM_SYSTEM_EVENT_*`
// enums — documented ABI facts, not measured constants.

/// `KVM_EXIT_MMIO` — a guest MMIO access (the entire stock userspace-device
/// surface on arm64: guest RAM is high, device frames fault out here).
pub(crate) const KVM_EXIT_MMIO: u32 = 6;
/// `KVM_EXIT_SYSTEM_EVENT` — a PSCI `SYSTEM_OFF`/`RESET`/`CRASH` (the stock
/// shutdown path).
pub(crate) const KVM_EXIT_SYSTEM_EVENT: u32 = 24;
/// `KVM_EXIT_INTR` — the run was interrupted by a host signal (re-enter).
pub(crate) const KVM_EXIT_INTR: u32 = 10;
/// `KVM_EXIT_FAIL_ENTRY` — the vCPU could not be entered (fail closed).
pub(crate) const KVM_EXIT_FAIL_ENTRY: u32 = 9;
/// `KVM_EXIT_INTERNAL_ERROR` — KVM-internal failure (fail closed).
pub(crate) const KVM_EXIT_INTERNAL_ERROR: u32 = 17;
/// `KVM_EXIT_HYPERCALL` — a guest `HVC` surfaced to userspace (uapi/linux
/// `kvm.h`: `3`; **not** `13`, which is `KVM_EXIT_S390_SIEIC`). **Patched-only**:
/// stock KVM/arm64 services guest `HVC`/PSCI in-kernel and never surfaces this.
pub(crate) const KVM_EXIT_HYPERCALL: u32 = 3;

/// A **patched-ABI** exit reason for a work-counter WFx / deterministic idle
/// (the arm64 mirror of the x86 `KVM_EXIT_HLT`→`Idle` path). Stock KVM/arm64
/// blocks WFI **in-kernel** and never surfaces it, so this arm is unreachable on
/// the stock backend. `// TODO(patched-abi)`: the concrete reason value is the
/// AA-3 0004-analogue patch's — this placeholder only shapes the decode.
pub(crate) const KVM_EXIT_ARM_WFX_PLACEHOLDER: u32 = 0xA001;
/// A **patched-ABI** exit reason for a trapped ID/PMU/timer sysreg (there is no
/// MSR-filter analogue on stock KVM/arm64 — it emulates/UNDEFs sysregs
/// in-kernel). `// TODO(patched-abi)`: the AA-3 backend's value.
pub(crate) const KVM_EXIT_ARM_SYSREG_PLACEHOLDER: u32 = 0xA002;

/// `KVM_SYSTEM_EVENT_SHUTDOWN` — PSCI `SYSTEM_OFF`.
pub(crate) const KVM_SYSTEM_EVENT_SHUTDOWN: u32 = 1;
/// `KVM_SYSTEM_EVENT_RESET` — PSCI `SYSTEM_RESET` (terminal for a single-shot
/// determinism guest, like shutdown).
pub(crate) const KVM_SYSTEM_EVENT_RESET: u32 = 2;
/// `KVM_SYSTEM_EVENT_CRASH` — a guest crash event.
pub(crate) const KVM_SYSTEM_EVENT_CRASH: u32 = 3;

/// `KVM_ARM_VCPU_PSCI_0_2` — the `KVM_ARM_VCPU_INIT` feature **bit index** (`2`,
/// `uapi/linux/kvm.h`) that selects KVM's in-kernel **PSCI 0.2+** implementation.
/// The DTB this vendor emits advertises `arm,psci-1.0` over `HVC` (`vmm-core`'s
/// `dtb`), so the guest issues PSCI `SYSTEM_OFF`/`SYSTEM_RESET`/`CPU_ON` as
/// `HVC`s; **without** this bit KVM runs *legacy* PSCI (0.1) and answers those
/// `NOT_SUPPORTED`, so a headless determinism guest can never cleanly power off.
/// Unlike the vGIC **delivery** fabric there is no AA-6 deferral rationale for
/// it — a guest that cannot `SYSTEM_OFF` cannot end a run.
pub(crate) const KVM_ARM_VCPU_PSCI_0_2: u32 = 2;

// The in-kernel vGICv3 migration/injection groups used by both the portable
// orchestration and the Linux+aarch64 syscall half. Values are pinned against
// `kvm-bindings` in `arm64_kvm_sys`.
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
const GICR_ISACTIVER0: u64 = GICR_SGI_BASE + 0x0300;
const VGIC_LEVEL_INFO_LINE_LEVEL: u64 = 0;
const HARMONY_GIC_IMPL_SPIS: u32 = 64;
pub(crate) const HARMONY_GIC_NR_IRQS: u32 = 32 + HARMONY_GIC_IMPL_SPIS;
const HARMONY_TIMER_HZ: u64 = 62_500_000;
const HARMONY_TIMER_INTID: u32 = 27;
const GIC_STATE_VERSION: u32 = 3;

const fn vgic_sysreg(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
    op0 << 14 | op1 << 11 | crn << 7 | crm << 3 | op2
}

const ICC_PMR_EL1: u64 = vgic_sysreg(3, 0, 4, 6, 0);
const ICC_IGRPEN1_EL1: u64 = vgic_sysreg(3, 0, 12, 12, 7);

/// The `kvm_vcpu_init.features` bitmap the backend requests at
/// `KVM_ARM_VCPU_INIT`: **PSCI 0.2 selected** (see [`KVM_ARM_VCPU_PSCI_0_2`]),
/// every other feature left default-off (the skeleton opts into nothing else —
/// `EL1_32BIT`, `PMU_V3`, `SVE`, `PTRAUTH*` are AA-6 / port decisions). A pure
/// function so `LiveKvm` and the portable [`FakeKvm`] derive the **identical**
/// bitmap — the live path's request is exactly what the fake pins.
pub(crate) fn vcpu_init_features() -> [u32; 7] {
    let mut features = [0u32; 7];
    features[0] = 1 << KVM_ARM_VCPU_PSCI_0_2;
    features
}

// --- the plain-data view of a `kvm_run` the decode operates on ---------------

/// The MMIO payload of a `KVM_EXIT_MMIO` (`kvm_run.mmio`), as plain data.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct MmioView {
    /// The guest-physical address accessed.
    pub phys_addr: u64,
    /// Up to 8 little-endian data bytes (the low `len` are meaningful).
    pub data: [u8; 8],
    /// Access width in bytes (1/2/4/8).
    pub len: u32,
    /// `true` = store (the guest wrote `data`); `false` = load (awaits a
    /// completion the VMM writes back into `data`).
    pub is_write: bool,
}

/// A plain-data snapshot of the fields of `kvm_run` the [`decode_exit`] logic
/// reads, filled by the box layer from the real mmap'd `kvm_run` (so the decode
/// never touches `kvm_bindings` and stays portable + Miri-testable).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct KvmRunView {
    /// The `kvm_run.exit_reason`.
    pub exit_reason: u32,
    /// The MMIO payload (meaningful iff `exit_reason == KVM_EXIT_MMIO`).
    pub mmio: MmioView,
    /// The `kvm_run.system_event.type` (iff `KVM_EXIT_SYSTEM_EVENT`).
    pub system_event_type: u32,
    /// The `HVC` argument frame (iff the patched `KVM_EXIT_HYPERCALL`).
    pub hypercall_args: [u64; 4],
    /// The trapped sysreg encoding + write value (iff the patched sysreg exit):
    /// `(encoding, Some(value_written) | None_for_read)`.
    pub sysreg: (u32, Option<u64>),
}

/// What the last returned exit awaits, if anything (the completion-discipline
/// bookkeeping, mirroring the x86 `Pending`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Pending {
    /// Nothing pending; `run` may resume.
    None,
    /// An MMIO **load** of `len` bytes: `complete_read` writes the value back.
    MmioLoad {
        /// The access width in bytes.
        len: u32,
    },
    /// A **patched** trapped sysreg **read**: `complete_read` or
    /// `complete_fault` (deny-UNDEF).
    SysregRead,
    /// A **patched** trapped sysreg **write**: `complete_ok` or `complete_fault`.
    SysregWrite,
}

/// Decode a `KVM_RUN` result into an [`Exit`] (`None` = a control exit the run
/// loop re-enters on). Total; an unrecognized reason fails closed
/// (default-deny), never a silent continue.
///
/// **Stock surface = `Mmio` + `Shutdown` only.** Every other arm is
/// patched-ABI (`// TODO(patched-abi)`) and the stock hardware never reaches it.
pub(crate) fn decode_exit(view: &KvmRunView) -> Result<Option<(Exit<Arm64>, Pending)>> {
    match view.exit_reason {
        // --- reachable on the STOCK backend ---------------------------------
        KVM_EXIT_MMIO => {
            let m = &view.mmio;
            // The access width MUST be one of the architectural byte-access
            // sizes {1,2,4,8}. Fail closed on anything else (a `len == 0` would
            // otherwise stage a zero-byte load; a non-power-of-two width is a
            // malformed exit) — never a truncated/extended completion.
            if !matches!(m.len, 1 | 2 | 4 | 8) {
                return Err(BackendError::Internal(
                    "KVM_EXIT_MMIO with a non-architectural access width (not 1/2/4/8)",
                ));
            }
            let gpa = Gpa(m.phys_addr);
            if m.is_write {
                // A store carries its value in `data`; no completion. (The
                // reserved-GPA hypercall doorbell store lands here too — the
                // vendor's `dispatch_mmio` recognizes the GPA.)
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

        // --- control exits: re-enter ----------------------------------------
        KVM_EXIT_INTR => Ok(None),

        // --- fail closed ----------------------------------------------------
        KVM_EXIT_FAIL_ENTRY => Err(BackendError::Internal("KVM_EXIT_FAIL_ENTRY")),
        KVM_EXIT_INTERNAL_ERROR => Err(BackendError::Internal("KVM_EXIT_INTERNAL_ERROR")),

        // --- PATCHED-ABI ONLY (stock never returns these) -------------------
        // TODO(patched-abi): the AA-3 0004-analogue backend surfaces these; the
        // decode arms exist so that backend drops in without reshaping this
        // function, exactly as the x86 decode carries its patched arms.
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

/// Read the low `len` bytes of `data` as a little-endian `u64` (`len ≤ 8`).
fn le_value(data: &[u8; 8], len: u32) -> u64 {
    let mut buf = [0u8; 8];
    let n = (len as usize).min(8);
    buf[..n].copy_from_slice(&data[..n]);
    u64::from_le_bytes(buf)
}

/// The low `len` bytes of `value` as an 8-byte little-endian MMIO data buffer
/// (the completion the VMM writes back for an MMIO load).
fn le_data(value: u64, len: u32) -> [u8; 8] {
    let mut data = value.to_le_bytes();
    // Zero the bytes past `len` so a completion never smuggles high bytes.
    for b in data.iter_mut().skip((len as usize).min(8)) {
        *b = 0;
    }
    data
}

// --- the portable `kvm_run` pointer seam (the arm64 `RunPage`) ----------------
// The raw mmap'd-`kvm_run` reads live HERE, in the portable module, so the
// unsafe pointer logic is compiled + Miri-tested on the x86 host (the box-only
// `arm64_kvm_sys`, where the real ioctls live, is `cfg`'d out of the x86 Miri
// job — so its reads would otherwise sit outside the unsafe⇒Miri UB gate, the
// x86 `RunPage` precedent). The box provides the field byte offsets (via
// `offset_of!` on the arch-specific `kvm_run`), so this seam never depends on
// the `kvm_bindings` layout and stays portable.

/// The byte offsets of the `kvm_run` fields the decode reads, computed by the
/// box layer from the arch-specific `kvm_run` (`offset_of!`). The MMIO
/// sub-fields overlap `system_event` in the exit-info union, exactly as in the
/// real `kvm_run`.
// Constructed by the box `arm64_kvm_sys` (aarch64-linux only) and the tests;
// dead on a non-test build off that leg, hence the conditional allow (the
// `region`/`run_buf` seam precedent).
#[cfg_attr(
    not(any(test, all(target_os = "linux", target_arch = "aarch64"))),
    allow(dead_code)
)]
#[derive(Clone, Copy, Debug)]
pub(crate) struct RunOffsets {
    /// `kvm_run.exit_reason` (`u32`).
    pub(crate) exit_reason: usize,
    /// `mmio.phys_addr` (`u64`).
    pub(crate) mmio_phys_addr: usize,
    /// `mmio.data` (`[u8; 8]`) — also the MMIO-load completion write-back slot.
    pub(crate) mmio_data: usize,
    /// `mmio.len` (`u32`).
    pub(crate) mmio_len: usize,
    /// `mmio.is_write` (`u8`).
    pub(crate) mmio_is_write: usize,
    /// `system_event.type` (`u32`).
    pub(crate) system_event_type: usize,
}

/// A raw view over the mmap'd `kvm_run` shared page (the arm64 analogue of the
/// x86 `RunPage`): bounds-checked reads of the plain fields the decode needs,
/// and the MMIO-load completion write-back into `mmio.data`. All accesses are
/// bounds-checked against `len` and fail closed — never an out-of-bounds read.
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

    /// Fail closed unless `[off, off + n)` is within the page.
    fn check(&self, off: usize, n: usize) -> Result<()> {
        match off.checked_add(n) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(BackendError::Internal("kvm_run field access out of bounds")),
        }
    }

    /// Read `N` bytes at `off`.
    ///
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

    /// Read the plain `kvm_run` fields at `off` into a [`KvmRunView`].
    ///
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

    /// Write an MMIO-load completion's 8 data bytes into `mmio.data` (read back
    /// by the kernel on the next `KVM_RUN`).
    ///
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

// --- the register-ID table (`KVM_GET_ONE_REG`/`KVM_SET_ONE_REG`) -------------
// arm64 KVM register IDs are documented encodings (Documentation/virt/kvm/
// api.rst, `arch/arm64/include/uapi/asm/kvm.h`). These are ABI facts.

/// The register-class shift (`KVM_REG_ARM_COPROC_SHIFT`): the class selector
/// (`ARM_CORE`, `ARM64_SYSREG`) lives at bits 16..28, **not** the high bits.
const KVM_REG_ARM_COPROC_SHIFT: u64 = 16;
pub(crate) const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
const KVM_REG_SIZE_U32: u64 = 0x0020_0000_0000_0000;
pub(crate) const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
const KVM_REG_SIZE_U128: u64 = 0x0040_0000_0000_0000;
/// `KVM_REG_ARM_CORE = 0x0010 << KVM_REG_ARM_COPROC_SHIFT` (= `0x10_0000`), the
/// class of `struct kvm_regs` fields (uapi/linux `.../asm/kvm.h`). At bits 16+,
/// so it never collides with the field index in bits 0..15.
pub(crate) const KVM_REG_ARM_CORE: u64 = 0x0010 << KVM_REG_ARM_COPROC_SHIFT;
/// `KVM_REG_ARM64_SYSREG = 0x0013 << KVM_REG_ARM_COPROC_SHIFT` (= `0x13_0000`),
/// the class of EL1 system registers; the `op0:op1:CRn:CRm:op2` encoding fills
/// bits 0..15 below it.
pub(crate) const KVM_REG_ARM64_SYSREG: u64 = 0x0013 << KVM_REG_ARM_COPROC_SHIFT;
/// KVM-as-firmware pseudo-register class (`uapi/asm/kvm.h`).
pub(crate) const KVM_REG_ARM_FW: u64 = 0x0014 << KVM_REG_ARM_COPROC_SHIFT;
/// Writable PSCI-version pseudo-register. This must be set before first entry.
const KVM_REG_ARM_PSCI_VERSION: u64 = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_FW;
/// PSCI 1.0, encoded by the architectural `PSCI_VERSION(1, 0)` macro.
const KVM_ARM_PSCI_1_0: u64 = 0x0001_0000;
/// Firmware-feature bitmap pseudo-register class (`uapi/asm/kvm.h`).
const KVM_REG_ARM_FW_FEAT_BMAP: u64 = 0x0016 << KVM_REG_ARM_COPROC_SHIFT;
/// Optional Standard Secure Service bitmap: bit 0 is SMCCC TRNG v1.0.
///
/// Stock KVM enables this bit by default and services `TRNG_RND64` from the
/// host kernel's live RNG. The owned Linux guest probes it before
/// `random_init_early()` and would mix those bytes into full-RAM state before
/// `/chosen/rng-seed` makes the CRNG ready. The deterministic baseline writes
/// zero before first entry.
const KVM_REG_ARM_STD_BMAP: u64 = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_FW_FEAT_BMAP;
/// Optional Standard Hypervisor Service bitmap (PV time).
const KVM_REG_ARM_STD_HYP_BMAP: u64 = KVM_REG_ARM_STD_BMAP | 1;
/// Optional KVM vendor-hypercall bitmap (including host PTP).
const KVM_REG_ARM_VENDOR_HYP_BMAP: u64 = KVM_REG_ARM_STD_BMAP | 2;
/// Second optional KVM vendor-hypercall bitmap.
const KVM_REG_ARM_VENDOR_HYP_BMAP_2: u64 = KVM_REG_ARM_STD_BMAP | 3;
const OPTIONAL_FIRMWARE_BITMAPS: [u64; 4] = [
    KVM_REG_ARM_STD_BMAP,
    KVM_REG_ARM_STD_HYP_BMAP,
    KVM_REG_ARM_VENDOR_HYP_BMAP,
    KVM_REG_ARM_VENDOR_HYP_BMAP_2,
];

/// A **core** register ID: `struct kvm_regs` field offset ÷ 4.
const fn core_reg(index: u64) -> u64 {
    KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE | index
}

const fn core_reg_sized(size: u64, index: u64) -> u64 {
    KVM_REG_ARM64 | size | KVM_REG_ARM_CORE | index
}

/// An EL1 **system** register ID from its `op0:op1:CRn:CRm:op2` encoding.
const fn sysreg_id(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
    KVM_REG_ARM64
        | KVM_REG_SIZE_U64
        | KVM_REG_ARM64_SYSREG
        | (op0 << 14 | op1 << 11 | crn << 7 | crm << 3 | op2)
}

// Core-reg indices into `struct kvm_regs` (offset/4): regs[i] at i*2, then sp
// (62), pc (64), pstate (66), sp_el1 (68), elr_el1 (70), spsr[0]=SPSR_EL1 (72).
const CORE_SP: u64 = 62;
const CORE_PC: u64 = 64;
const CORE_PSTATE: u64 = 66;
const CORE_SP_EL1: u64 = 68;
const CORE_ELR_EL1: u64 = 70;
const CORE_SPSR_EL1: u64 = 72;
const CORE_FP_BASE: u64 = 84;
const CORE_FPSR: u64 = 212;
const CORE_FPCR: u64 = 213;

const CNTV_CTL_EL0: u64 = sysreg_id(3, 3, 14, 3, 1);
// Linux's stable KVM one-register ABI accidentally swapped the IDs for
// CNTV_CVAL_EL0 and CNTVCT_EL0. The UAPI explicitly requires callers to use
// the historical ID below (the CNTVCT architectural encoding) for CVAL.
const CNTV_CVAL_EL0: u64 = sysreg_id(3, 3, 14, 0, 2);
const MDSCR_EL1: u64 = sysreg_id(2, 0, 0, 2, 2);

// The admitted ID-register baseline makes EPAN and ITFSB architecturally
// unsupported, but stock KVM retains Linux's writes to those SCTLR bits while
// HVF reads them as RES0. Likewise, KVM retains TCR.AS while this HVF substrate
// exposes only the common 8-bit-ASID behavior. Strip those substrate residues
// from the portable boundary; restore refuses a non-canonical snapshot.
const KVM_SCTLR_NONPORTABLE_BITS: u64 = (1 << 57) | (1 << 37);
const KVM_TCR_NONPORTABLE_BITS: u64 = 1 << 36;
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

/// The EL1 sysreg IDs of the skeleton [`Arm64SysregFile`](crate::Arm64SysregFile),
/// paired with a selector so save/restore is one table walk. Full record set is
/// `TODO(AA-6)`; this is the minimal round-trippable subset.
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
    TpidrEl1,
    Cntkctl,
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
    (sysreg_id(3, 0, 13, 0, 4), SysSel::TpidrEl1),
    (sysreg_id(3, 0, 14, 1, 0), SysSel::Cntkctl),
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
        SysSel::TpidrEl1 => &mut f.tpidr_el1,
        SysSel::Cntkctl => &mut f.cntkctl_el1,
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
        SysSel::TpidrEl1 => f.tpidr_el1,
        SysSel::Cntkctl => f.cntkctl_el1,
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

/// Capture the in-kernel vGIC through KVM's migration API and normalize it to
/// the same architectural record used by the userspace model.
fn save_vgic<K: Arm64Kvm + ?Sized>(k: &K) -> Result<Arm64GicState> {
    let mut s = Arm64GicState {
        version: GIC_STATE_VERSION,
        impl_spis: HARMONY_GIC_IMPL_SPIS,
        timer_hz: HARMONY_TIMER_HZ,
        timer_intid: HARMONY_TIMER_INTID,
        ..Arm64GicState::default()
    };
    s.gicd_ctlr =
        u32::try_from(k.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_DIST_REGS, GICD_CTLR, false)? & 0b10)
            .map_err(|_| BackendError::InvalidState)?;

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
            VGIC_LEVEL_INFO_LINE_LEVEL | (word as u64 * 32),
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
    s.pmr = u8::try_from(k.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_PMR_EL1, true)?)
        .map_err(|_| BackendError::InvalidState)?;
    s.igrpen1 = match k.get_vgic_attr(KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS, ICC_IGRPEN1_EL1, true)? {
        0 => false,
        1 => true,
        _ => return Err(BackendError::InvalidState),
    };
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
    Ok(())
}

/// Restore a canonical record through KVM's vGIC migration API. Mutable files
/// are replaced (clear-before-set where the architectural register is
/// write-one-to-modify), and forwarding is enabled last. The implementation
/// IIDR handshake happens before vGIC initialization in the syscall layer.
fn restore_vgic<K: Arm64Kvm + ?Sized>(k: &mut K, s: &Arm64GicState) -> Result<()> {
    validate_vgic_state(s)?;
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
            VGIC_LEVEL_INFO_LINE_LEVEL | (word as u64 * 32),
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
    // The userspace model stores only the writable Group-1 enable bits and
    // models ARE as permanently enabled. Reconstitute that fixed bit here.
    k.set_vgic_attr(
        KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
        GICD_CTLR,
        false,
        u64::from(s.gicd_ctlr | (1 << 4)),
    )?;
    Ok(())
}

/// Read the full skeleton vCPU state over the reg-ID table (pure; drives the
/// [`Arm64Kvm`] seam).
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
    s.sysregs.sctlr_el1 &= !KVM_SCTLR_NONPORTABLE_BITS;
    s.sysregs.tcr_el1 &= !KVM_TCR_NONPORTABLE_BITS;
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
    s.vtimer.cntv_cval_el0 = k.get_one_reg(CNTV_CVAL_EL0)?;
    // KVM quarantines this host-backed timer by routing it to unused PPI20;
    // the canonical bit records the invariant, not a KVM mask ioctl.
    s.vtimer.masked = true;
    s.mp_state = k.get_mp_state()?;
    s.gic = Some(save_vgic(k)?);
    Ok(s)
}

/// Restore the full skeleton vCPU state over the reg-ID table.
pub(crate) fn restore_vcpu<K: Arm64Kvm + ?Sized>(k: &mut K, s: &Arm64VcpuState) -> Result<()> {
    if has_noncanonical_core_regs(&s.core)
        || s.debug.trap_debug_exceptions
        || s.debug.trap_debug_reg_accesses
        || !s.vtimer.masked
        || s.vtimer.offset != 0
        || s.vtimer.cntv_ctl_el0 & !CNTV_CTL_WRITABLE_BITS != 0
        || s.sysregs.sctlr_el1 & KVM_SCTLR_NONPORTABLE_BITS != 0
        || s.sysregs.tcr_el1 & KVM_TCR_NONPORTABLE_BITS != 0
        || s.interrupts.irq
        || s.interrupts.fiq
    {
        return Err(BackendError::InvalidState);
    }
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
    k.set_one_reg(CNTV_CVAL_EL0, s.vtimer.cntv_cval_el0)?;
    // Arm the timer only after its compare value is restored. Writing CTL
    // first can transiently assert the virtual-timer PPI against the old CVAL.
    k.set_one_reg(CNTV_CTL_EL0, s.vtimer.cntv_ctl_el0)?;
    k.set_mp_state(s.mp_state)?;
    let gic = s.gic.as_ref().ok_or(BackendError::InvalidState)?;
    restore_vgic(k, gic)?;
    Ok(())
}

// --- the thin syscall seam ---------------------------------------------------

/// The KVM/arm64 syscall boundary as a trait, so the [`Arm64KvmBackend`]
/// orchestration (ioctl ordering, completion discipline) is testable against a
/// recording fake with no `/dev/kvm`. The real impl (`arm64_kvm_sys::LiveKvm`)
/// is Linux+aarch64-gated; a portable [`FakeKvm`] backs the unit/Miri tests.
pub trait Arm64Kvm {
    /// `KVM_ARM_PREFERRED_TARGET` + `KVM_ARM_VCPU_INIT` — MUST precede the first
    /// `set_one_reg`/`run` (KVM rejects register access on an un-init'd vCPU).
    fn vcpu_init(&mut self) -> Result<()>;

    /// `KVM_SET_USER_MEMORY_REGION` for one RAM memslot.
    ///
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

    /// `KVM_GET_ONE_REG` (u64).
    fn get_one_reg(&self, id: u64) -> Result<u64>;
    /// `KVM_SET_ONE_REG` (u64). Also the config-time `ID_AA64*` freeze write
    /// (the ID registers are writable sysregs before the first run).
    fn set_one_reg(&mut self, id: u64, value: u64) -> Result<()>;
    /// `KVM_GET_ONE_REG` for a 32-bit core field.
    fn get_one_reg32(&self, id: u64) -> Result<u32>;
    /// `KVM_SET_ONE_REG` for a 32-bit core field.
    fn set_one_reg32(&mut self, id: u64, value: u32) -> Result<()>;
    /// `KVM_GET_ONE_REG` for a 128-bit SIMD register, in architectural byte
    /// order.
    fn get_one_reg128(&self, id: u64) -> Result<[u8; 16]>;
    /// `KVM_SET_ONE_REG` for a 128-bit SIMD register.
    fn set_one_reg128(&mut self, id: u64, value: [u8; 16]) -> Result<()>;

    /// `KVM_GET_MP_STATE`.
    fn get_mp_state(&self) -> Result<MpState>;
    /// `KVM_SET_MP_STATE`.
    fn set_mp_state(&mut self, mp: MpState) -> Result<()>;

    /// Drive one in-kernel vGICv3 input line through `KVM_IRQ_LINE`.
    fn set_irq_line(&mut self, id: GicIntId, level: bool) -> Result<()>;

    /// Read a vGICv3 migration attribute. `width64` selects the 64-bit
    /// CPU-interface groups; distributor, redistributor, and level groups are
    /// 32-bit and return a zero-extended value.
    fn get_vgic_attr(&self, group: u32, attr: u64, width64: bool) -> Result<u64>;

    /// Write a vGICv3 migration attribute using the same width rule as
    /// [`Self::get_vgic_attr`].
    fn set_vgic_attr(&mut self, group: u32, attr: u64, width64: bool, value: u64) -> Result<()>;

    /// Stage the data an MMIO **load** completes with, written into the mmap'd
    /// `kvm_run.mmio.data` before the next `run` (the x86 `complete_read`
    /// equivalent, below the trait).
    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()>;

    /// Re-enter only far enough for KVM to retire the pending MMIO instruction.
    ///
    /// The live implementation sets `kvm_run.immediate_exit` before
    /// `KVM_RUN`. KVM consumes the prior MMIO completion, updates the target
    /// register/PC, then returns `EINTR` without executing the next guest
    /// instruction. This turns the substrate-local in-flight exit into the
    /// fully serviced architectural boundary the VMM hashes and snapshots.
    fn complete_mmio_exit(&mut self) -> Result<()>;

    /// `KVM_RUN`, returning the plain-data view [`decode_exit`] consumes.
    fn run(&mut self) -> Result<KvmRunView>;
}

/// The stock KVM/arm64 [`Backend`], generic over the [`Arm64Kvm`] syscall seam
/// (`K` is `LiveKvm` in production, [`FakeKvm`] in tests).
pub struct Arm64KvmBackend<K: Arm64Kvm> {
    kvm: K,
    configured: bool,
    pending: Pending,
    /// The staged MMIO-load completion value, applied before the next `run`.
    staged_read: Option<[u8; 8]>,
    /// The registered `(gpa, len)` memslots, in insertion order — so a second
    /// `map_memory` that overlaps an existing region fails closed (the
    /// [`Backend::map_memory`] contract), rather than silently registering a
    /// duplicate `slot 0` that replaces the first.
    regions: Vec<(u64, u64)>,
    /// The one level input requested for the next entry. The userspace
    /// interrupt controller remains the queue; this is only its current
    /// arbitrated output (or the in-kernel clockevent line).
    pending_irq: Option<GicIntId>,
    /// The line currently driven into the in-kernel vGIC.
    applied_irq: Option<GicIntId>,
    /// One acceptance report awaiting the VMM's drain.
    accepted_irq: Option<GicIntId>,
    /// The active identity already reported, preventing duplicate reports
    /// while the guest remains in the handler.
    reported_active_irq: Option<GicIntId>,
    counts: ExitCounts,
}

impl<K: Arm64Kvm> Arm64KvmBackend<K> {
    /// Wrap an already-`vcpu_init`'d syscall seam. (Construction — `KVM_CREATE_VM`
    /// → `KVM_CREATE_VCPU` → `KVM_ARM_VCPU_INIT` — happens in the box
    /// constructor, `arm64_kvm_sys::LiveKvm::new`, which calls `vcpu_init`; the
    /// fake records it, so the ordering is asserted portably.)
    pub fn new(kvm: K) -> Self {
        Self {
            kvm,
            configured: false,
            pending: Pending::None,
            staged_read: None,
            regions: Vec::new(),
            pending_irq: None,
            applied_irq: None,
            accepted_irq: None,
            reported_active_irq: None,
            counts: ExitCounts::default(),
        }
    }

    /// Read-only access to the syscall seam (for test assertions).
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
        Ok(())
    }

    /// Apply the latest one-slot level request. A replacement is ordered
    /// low-before-high so two identities are never asserted by this backend
    /// simultaneously. `set_pending_irq` calls this at the serviced-exit
    /// boundary, making the canonical vGIC line state observable before a
    /// snapshot; the entry call is an idempotent safety net.
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

    /// Observe the in-kernel pending→active transition after an exit. The
    /// owned Linux clockevent ACKs through MMIO before EOI, so the first exit
    /// from its handler exposes the active bit and cannot be missed.
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

    /// Enter the guest: apply any staged read completion, then `KVM_RUN`, then
    /// decode. Re-enters on control exits (`None`).
    fn enter_guest(&mut self) -> Result<Exit<Arm64>> {
        loop {
            self.apply_pending_irq()?;
            if let Some(data) = self.staged_read.take() {
                self.kvm.write_mmio_data(data)?;
            }
            let view = self.kvm.run()?;
            self.observe_irq_acceptance()?;
            if let Some((exit, pending)) = decode_exit(&view)? {
                if view.exit_reason == KVM_EXIT_MMIO && view.mmio.is_write {
                    self.kvm.complete_mmio_exit()?;
                }
                self.counts.bump(exit.reason());
                self.pending = pending;
                return Ok(exit);
            }
        }
    }
}

impl<K: Arm64Kvm> Backend for Arm64KvmBackend<K> {
    type A = Arm64;

    fn set_policy(&mut self, policy: &crate::arch::arm64::Arm64Policy) -> Result<()> {
        // KVM otherwise exposes the host kernel's latest implemented PSCI
        // version. PSCI 1.1 adds SYSTEM_RESET2, which Linux probes and records
        // in guest RAM, so that default leaks the substrate into canonical
        // state. Pin the VM firmware to the DTB's `arm,psci-1.0` contract
        // before first entry; HVF reports the same version and service set.
        self.kvm
            .set_one_reg(KVM_REG_ARM_PSCI_VERSION, KVM_ARM_PSCI_1_0)?;
        // Stock KVM enables optional SMCCC services by default. In particular,
        // its TRNG service returns `get_random_long()` from the host kernel,
        // which the owned Linux guest consumes during `random_init_early()`.
        // Disable every optional firmware bitmap before first entry. PSCI is a
        // default-allowed service outside these bitmaps and remains available.
        // HVF already answers unknown non-PSCI HVCs with NOT_SUPPORTED, so this
        // makes the two substrates expose the same deterministic firmware
        // surface without changing the guest image or frozen contract hash.
        for id in OPTIONAL_FIRMWARE_BITMAPS {
            self.kvm.set_one_reg(id, 0)?;
        }
        // What actually works on stock: the `ID_AA64*` freeze — a config-time
        // `KVM_SET_ONE_REG` on the writable ID registers before the first run.
        // The IdRegModel is keyed by the packed sysreg encoding; write each
        // frozen value through the seam. (An empty skeleton model writes
        // nothing — the rows are AA-6's.)
        for (&enc, &value) in &policy.id_regs.regs {
            // The packed `op0:op1:CRn:CRm:op2` encoding → the KVM sysreg ID.
            let id = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG | u64::from(enc);
            self.kvm.set_one_reg(id, value)?;
        }
        // What is PATCHED-ONLY (recorded, not enforced here): the
        // `HCR_EL2`/`MDCR_EL2` trap-group enforcement that turns a denied
        // sysreg into a userspace `Sysreg` exit — the skeleton holds the trap
        // table shape (`policy.sysreg_traps`) but its runtime exits are AA-3's.
        // TODO(patched-abi): install the trap groups on the patched backend;
        // TODO(AA-6): the full row set.
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
        // The region must not wrap the address space, and must not overlap any
        // already-mapped region — a duplicate/overlapping map is a caller error
        // (the `Backend::map_memory` contract), NOT a silent replace of an
        // existing memslot.
        let end = gpa
            .0
            .checked_add(len)
            .ok_or(BackendError::Memory("region wraps the address space"))?;
        for &(g, l) in &self.regions {
            let g_end = g + l; // no wrap: validated when each region was inserted
            if gpa.0 < g_end && g < end {
                return Err(BackendError::Memory("region overlaps an existing map"));
            }
        }
        // A fresh, unique slot per region (never a reused `slot 0`).
        let slot = self.regions.len() as u32;
        // SAFETY: the caller upholds `map_memory`'s contract (pinned,
        // page-aligned, unaliased backing live for the backend's lifetime); we
        // forward the same guarantee to the seam. arm64 device frames sit below
        // RAM, so a RAM region needs no hole-split (unlike x86's xAPIC page).
        unsafe {
            self.kvm
                .set_user_memory_region(slot, gpa.0, host.as_mut_ptr(), len)?;
        }
        self.regions.push((gpa.0, len));
        Ok(())
    }

    fn run(&mut self) -> Result<Exit<Arm64>> {
        self.ensure_runnable()?;
        self.enter_guest()
    }

    fn run_until(&mut self, _deadline: crate::types::Moment) -> Result<Exit<Arm64>> {
        // The deterministic force-exit + single-step landing is the arm64
        // 0004/0005-analogue kernel patch (AA-3) plus the patched backend — a
        // later bead, not this one. designed-not-frozen (AA-3): arm64's
        // PMU-overflow-to-exit physics may pressure `run_until`'s late-only-stop
        // contract before the trait may be declared frozen.
        Err(BackendError::Unsupported { what: "run_until" })
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

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::MmioLoad { len } => {
                self.kvm.write_mmio_data(le_data(value, len))?;
                self.kvm.complete_mmio_exit()?;
                self.pending = Pending::None;
                Ok(())
            }
            // The patched sysreg-read completion path (stock never reaches it).
            Pending::SysregRead => {
                self.staged_read = Some(le_data(value, 8));
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::NoPendingRead),
        }
    }

    fn complete_fault(&mut self) -> Result<()> {
        // Deny-UNDEF for a patched sysreg exit (stock never reaches it).
        match self.pending {
            Pending::SysregRead | Pending::SysregWrite => {
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_ok(&mut self) -> Result<()> {
        match self.pending {
            Pending::SysregWrite => {
                self.pending = Pending::None;
                Ok(())
            }
            _ => Err(BackendError::BadCompletion),
        }
    }

    fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
        // Stock KVM/arm64 services guest HVC/PSCI in-kernel and never surfaces
        // a hypercall exit — so there is never one pending on the stock backend
        // (the patched HVC-doorbell path is a later bead).
        Err(BackendError::NoPendingRead)
    }

    fn complete_arch(&mut self, _completion: crate::arch::arm64::Arm64Completion) -> Result<()> {
        // `Arm64Completion` is uninhabited (no arch-payload completions).
        match _completion {}
    }

    fn save(&self) -> Result<Arm64VcpuState> {
        save_vcpu(&self.kvm)
    }

    fn restore(&mut self, state: &Arm64VcpuState) -> Result<()> {
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

    fn capabilities(&self) -> Capabilities<crate::arch::arm64::Arm64Caps> {
        // Stock claims NO determinism (mirrors stock x86 `KvmBackend`): the
        // work clock, the exact-landing, and the paravirt clock are all patched/
        // AA-gated. Every field honestly false.
        Capabilities {
            name: "kvm-arm64-vgicv3",
            deterministic_rng: false,
            arch: crate::arch::arm64::Arm64Caps {
                in_kernel_gic: true,
                deterministic_cntvct: false,
                enforces_cntv_cval: false,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// A recording fake syscall seam — the portable + Miri test double that asserts
// ioctl *shape* (ordering, the reg-ID set) with no `/dev/kvm`
// (`docs/VM-EXIT-COUNT-VTIME.md`: mechanism attestation). Behind
// `cfg(any(test, ...))` so it never ships in a non-test build.
// ---------------------------------------------------------------------------

/// A recording fake [`Arm64Kvm`]: it holds a register map, a scripted queue of
/// `KVM_RUN` views, and an ordered log of every ioctl the backend issued (so a
/// test can assert `vcpu_init` precedes the first `set_one_reg`, etc.).
#[cfg(any(test, feature = "mock"))]
#[derive(Debug, Default)]
pub struct FakeKvm {
    regs: std::collections::BTreeMap<u64, u64>,
    regs32: std::collections::BTreeMap<u64, u32>,
    regs128: std::collections::BTreeMap<u64, [u8; 16]>,
    mp_state: MpState,
    run_queue: std::collections::VecDeque<KvmRunView>,
    /// The ordered ioctl log — e.g. `"vcpu_init"`, `"set_one_reg"`, `"run"`.
    pub calls: Vec<&'static str>,
    /// The last MMIO-load data the backend staged (for completion assertions).
    pub last_mmio_data: Option<[u8; 8]>,
    /// Recorded `(slot, gpa, len)` memslots.
    pub memslots: Vec<(u32, u64, u64)>,
    /// Portable model of the vGIC device-attribute register file.
    vgic_attrs: std::collections::BTreeMap<(u32, u64, bool), u64>,
    /// Whether a scripted entry models the guest accepting an asserted IRQ.
    accept_irqs: bool,
    /// The `kvm_vcpu_init.features` bitmap `vcpu_init` requested (via the shared
    /// [`vcpu_init_features`], the same one `LiveKvm` sends) — so a test pins
    /// that PSCI 0.2 is advertised. Test-only observability.
    #[cfg_attr(not(test), allow(dead_code))]
    init_features: [u32; 7],
    initialized: bool,
}

#[cfg(any(test, feature = "mock"))]
impl FakeKvm {
    /// A fresh fake with an empty script.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueue a `KVM_RUN` view for a future `run`.
    pub fn push_run(&mut self, view: KvmRunView) -> &mut Self {
        self.run_queue.push_back(view);
        self
    }

    /// The recorded register value (for test assertions).
    pub fn reg(&self, id: u64) -> Option<u64> {
        self.regs.get(&id).copied()
    }

    /// Select whether the next scripted guest entry accepts an asserted line.
    pub fn set_accept_irqs(&mut self, accept: bool) -> &mut Self {
        self.accept_irqs = accept;
        self
    }
}

#[cfg(any(test, feature = "mock"))]
impl Arm64Kvm for FakeKvm {
    fn vcpu_init(&mut self) -> Result<()> {
        self.calls.push("vcpu_init");
        // Record the same feature bitmap the live path sends to KVM_ARM_VCPU_INIT.
        self.init_features = vcpu_init_features();
        self.initialized = true;
        Ok(())
    }

    unsafe fn set_user_memory_region(
        &mut self,
        slot: u32,
        gpa: u64,
        _host: *mut u8,
        len: u64,
    ) -> Result<()> {
        self.calls.push("set_user_memory_region");
        self.memslots.push((slot, gpa, len));
        Ok(())
    }

    fn get_one_reg(&self, id: u64) -> Result<u64> {
        Ok(self.regs.get(&id).copied().unwrap_or(0))
    }

    fn set_one_reg(&mut self, id: u64, value: u64) -> Result<()> {
        // Fail closed if a register is touched before init — exactly what KVM
        // does, so the ordering discipline is a real assertion, not decoration.
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
            VGIC_LEVEL_INFO_LINE_LEVEL | u64::from(block),
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
                .get(&(
                    KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO,
                    VGIC_LEVEL_INFO_LINE_LEVEL,
                    false,
                ))
                .copied()
                .unwrap_or(0);
            let private = levels & 0xffff_0000;
            self.vgic_attrs.insert(
                (KVM_DEV_ARM_VGIC_GRP_REDIST_REGS, GICR_ISACTIVER0, false),
                private,
            );
        }
        self.run_queue
            .pop_front()
            .ok_or(BackendError::Internal("fake KVM run-queue empty"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arch::arm64::{Arm64Policy, IdRegModel};

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

    /// Finding 2 (review r4): the raw mmap'd-`kvm_run` reads
    /// ([`RunPage::view`]/[`RunPage::write_mmio_data`]) are exercised over a
    /// synthetic heap buffer, so the unsafe pointer logic is **Miri-reachable**
    /// on the x86 host (the box-only `arm64_kvm_sys`, where the reads run
    /// against the real `kvm_run`, is `cfg`'d out of that Miri job — this is the
    /// seam that keeps the unsafe under the UB gate, the x86 `RunPage`
    /// precedent). Full loopback: build an MMIO exit in the buffer → `view` →
    /// `decode_exit` → `write_mmio_data` → read it back.
    #[test]
    fn run_page_loopback_over_a_synthetic_buffer() {
        // A compact `kvm_run`-shaped test layout (the box uses the real
        // `offset_of!`-derived offsets; here they are chosen so the MMIO
        // sub-fields overlap `system_event`, as they do in the union).
        let off = RunOffsets {
            exit_reason: 8,
            mmio_phys_addr: 32,
            mmio_data: 40,
            mmio_len: 48,
            mmio_is_write: 52,
            system_event_type: 32, // overlaps mmio.phys_addr — the union
        };
        let len = 128usize;
        let mut buf = vec![0u8; len];
        // A KVM_EXIT_MMIO **load** at a UARTFR-ish GPA, 4 bytes.
        buf[off.exit_reason..off.exit_reason + 4].copy_from_slice(&KVM_EXIT_MMIO.to_le_bytes());
        buf[off.mmio_phys_addr..off.mmio_phys_addr + 8]
            .copy_from_slice(&0x0900_0018u64.to_le_bytes());
        buf[off.mmio_len..off.mmio_len + 4].copy_from_slice(&4u32.to_le_bytes());
        buf[off.mmio_is_write] = 0;

        // SAFETY (test): `buf` (128 bytes) outlives `page`; all access is through
        // this one raw pointer, so nothing aliases it.
        let page = unsafe { RunPage::new(buf.as_mut_ptr(), len) };
        let view = unsafe { page.view(&off) }.unwrap();
        assert_eq!(view.exit_reason, KVM_EXIT_MMIO);
        assert_eq!(view.mmio.phys_addr, 0x0900_0018);
        assert_eq!(view.mmio.len, 4);
        assert!(!view.mmio.is_write);

        // The pure decode consumes the view → an MMIO load pending a completion.
        let (exit, pending) = decode_exit(&view).unwrap().unwrap();
        assert!(matches!(
            exit,
            Exit::Common(CommonExit::Mmio { write: None, .. })
        ));
        assert_eq!(pending, Pending::MmioLoad { len: 4 });

        // The completion write-back lands in mmio.data and reads back.
        unsafe { page.write_mmio_data(&off, le_data(0x90, 4)) }.unwrap();
        let view2 = unsafe { page.view(&off) }.unwrap();
        assert_eq!(view2.mmio.data, le_data(0x90, 4));

        // A SYSTEM_EVENT decodes from the same union bytes (system_event.type
        // overlaps mmio.phys_addr) → Shutdown.
        buf[off.exit_reason..off.exit_reason + 4]
            .copy_from_slice(&KVM_EXIT_SYSTEM_EVENT.to_le_bytes());
        buf[off.system_event_type..off.system_event_type + 4]
            .copy_from_slice(&KVM_SYSTEM_EVENT_SHUTDOWN.to_le_bytes());
        let sev = unsafe { page.view(&off) }.unwrap();
        assert_eq!(
            decode_exit(&sev).unwrap().unwrap().0,
            CommonExit::Shutdown.into()
        );

        // Bounds: an offset past the buffer fails closed (no OOB read).
        let bad = RunOffsets {
            exit_reason: len,
            ..off
        };
        assert!(unsafe { page.view(&bad) }.is_err());
    }

    /// Findings 1+2 (review r3): pin the KVM UAPI constants to the canonical
    /// `uapi/linux/kvm.h` values, portably. The compile-time pin in
    /// `arm64_kvm_sys` checks them against `kvm-bindings` on the aarch64-linux
    /// leg; this test additionally verifies — off the box, on any host — that
    /// the **full register IDs** the encoders emit equal the well-known KVM
    /// register IDs (so the class-shift lives at bits 16+, not 48+, and never
    /// collides with the field), and that the hypercall reason is 3, not 13.
    #[test]
    fn kvm_uapi_constants_match_the_headers() {
        // Exit reasons (uapi/linux/kvm.h).
        assert_eq!(
            KVM_EXIT_HYPERCALL, 3,
            "3, not 13 (13 = KVM_EXIT_S390_SIEIC)"
        );
        assert_eq!(KVM_EXIT_MMIO, 6);
        assert_eq!(KVM_EXIT_SYSTEM_EVENT, 24);
        assert_eq!(KVM_EXIT_INTR, 10);
        assert_eq!(KVM_EXIT_FAIL_ENTRY, 9);
        assert_eq!(KVM_EXIT_INTERNAL_ERROR, 17);

        // The register-class selectors live at bits 16..28, not 48+.
        assert_eq!(KVM_REG_ARM_CORE, 0x10_0000, "0x0010 << 16");
        assert_eq!(KVM_REG_ARM64_SYSREG, 0x13_0000, "0x0013 << 16");
        assert_eq!(KVM_REG_ARM_FW, 0x14_0000, "0x0014 << 16");

        // Full IDs vs the canonical KVM values (the strongest, non-circular
        // pin — verifies the whole encoding: class shift + field layout):
        //   x0    = ARM64 | SIZE_U64 | ARM_CORE | (offsetof(kvm_regs,regs[0])/4)
        //   pc    = ... | (offsetof(kvm_regs,regs.pc)/4 = 256/4 = 64)
        //   SCTLR_EL1 = ARM64 | SIZE_U64 | ARM64_SYSREG | (op0=3<<14 | crn=1<<7)
        assert_eq!(core_reg(0), 0x6030_0000_0010_0000, "x0");
        assert_eq!(core_reg(CORE_PC), 0x6030_0000_0010_0040, "pc");
        assert_eq!(
            KVM_REG_ARM_PSCI_VERSION, 0x6030_0000_0014_0000,
            "KVM firmware pseudo-register 0"
        );
        assert_eq!(KVM_ARM_PSCI_1_0, 0x0001_0000);
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
    }

    /// Finding 1 (review r1): a non-architectural MMIO access width is a
    /// malformed exit — fail closed on any `len ∉ {1,2,4,8}`, never a
    /// zero-byte load or a truncated completion.
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
        // The architectural widths are all accepted.
        for ok in [1u32, 2, 4, 8] {
            assert!(decode_exit(&mmio_load(0x0900_0000, ok)).is_ok());
        }
    }

    #[test]
    fn stock_surface_decodes_mmio_and_shutdown_only() {
        // MMIO store → Mmio{write:Some}, no pending.
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

        // MMIO load → Mmio{write:None}, pending a read.
        let (exit, pending) = decode_exit(&mmio_load(0x0900_0000, 4)).unwrap().unwrap();
        assert!(matches!(
            exit,
            Exit::Common(CommonExit::Mmio { write: None, .. })
        ));
        assert_eq!(pending, Pending::MmioLoad { len: 4 });

        // PSCI SYSTEM_OFF → Shutdown.
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
        // INTR re-enters (control).
        let view = KvmRunView {
            exit_reason: KVM_EXIT_INTR,
            ..Default::default()
        };
        assert_eq!(decode_exit(&view).unwrap(), None);
        // FAIL_ENTRY / INTERNAL_ERROR / unknown fail closed.
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
        // WFx → Idle (patched).
        let view = KvmRunView {
            exit_reason: KVM_EXIT_ARM_WFX_PLACEHOLDER,
            ..Default::default()
        };
        assert_eq!(
            decode_exit(&view).unwrap().unwrap().0,
            CommonExit::Idle.into()
        );
        // HVC → Hypercall (patched).
        let view = KvmRunView {
            exit_reason: KVM_EXIT_HYPERCALL,
            hypercall_args: [0x3150_4348, 1, 2, 3],
            ..Default::default()
        };
        assert!(matches!(
            decode_exit(&view).unwrap().unwrap().0,
            Exit::Common(CommonExit::Hypercall(_))
        ));
        // Trapped sysreg → Arm64Exit::Sysreg (patched).
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

    /// Review r8 (P1): the backend must advertise **PSCI 0.2** at
    /// `KVM_ARM_VCPU_INIT`. The DTB advertises `arm,psci-1.0` over `HVC`, so the
    /// guest issues PSCI as `HVC`s; without this feature bit KVM runs legacy PSCI
    /// and answers `SYSTEM_OFF` (which the boot path relies on for a clean
    /// poweroff) `NOT_SUPPORTED`. Pin the requested bitmap against the fake —
    /// which records exactly what `LiveKvm` sends (the shared
    /// [`vcpu_init_features`]). Live PSCI conformance is an M4/msr1 gate (the
    /// Mac has no `/dev/kvm` oracle; `hm-8l3` REFUSE).
    #[test]
    fn vcpu_init_advertises_psci_0_2() {
        // The shared bitmap both paths derive.
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

        // The fake records what vcpu_init requested — the live path's bitmap.
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        assert!(fake.calls.contains(&"vcpu_init"));
        assert_eq!(fake.init_features, vcpu_init_features());
        assert_ne!(fake.init_features[0] & (1 << KVM_ARM_VCPU_PSCI_0_2), 0);
    }

    /// The ioctl-ordering + policy discipline, asserted against the fake with no
    /// `/dev/kvm`: `vcpu_init` precedes the first `set_one_reg`, `set_policy`
    /// installs the ID-reg freeze, and `run` fails closed until configured.
    #[test]
    fn backend_orders_ioctls_and_installs_policy() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap(); // the box constructor does this
        let mut b = Arm64KvmBackend::new(fake);

        // Not configured yet: run fails closed.
        assert!(matches!(b.run(), Err(BackendError::NotConfigured)));

        // A policy with one ID-reg freeze row. Firmware and identity are all
        // installed through config-time set_one_reg calls.
        let mut policy = Arm64Policy {
            id_regs: IdRegModel::default(),
            ..Default::default()
        };
        // ID_AA64PFR0_EL1 = S3_0_C0_C4_0 → packed op0:op1:crn:crm:op2
        // (op0=3, crm=4; the op1/crn/op2 terms are zero).
        let enc = (3u32 << 14) | (4 << 3);
        policy.id_regs.regs.insert(enc, 0x1122_3344);
        b.set_policy(&policy).unwrap();

        // vcpu_init came before any set_one_reg (KVM ordering).
        let calls = &b.kvm().calls;
        let init_pos = calls.iter().position(|c| *c == "vcpu_init").unwrap();
        let first_set = calls.iter().position(|c| *c == "set_one_reg").unwrap();
        assert!(
            init_pos < first_set,
            "vcpu_init must precede set_one_reg: {calls:?}"
        );
        // The frozen ID value was written through the seam.
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
        let mut backend = Arm64KvmBackend::new(fake);

        backend.set_policy(&Arm64Policy::default()).unwrap();

        for id in OPTIONAL_FIRMWARE_BITMAPS {
            assert_eq!(backend.kvm().reg(id), Some(0));
        }
    }

    /// A save→restore round-trip over the reg-ID table reproduces the vCPU
    /// state bit-for-bit (the fake stores the reg map).
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
    fn save_strips_and_restore_rejects_host_pstate_residue() {
        const TCO: u64 = 1 << 25;
        const BTYPE: u64 = 0b11 << 10;

        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.set_one_reg(core_reg(CORE_PSTATE), 0xc5 | TCO | BTYPE)
            .unwrap();
        fake.set_one_reg(core_reg(CORE_SPSR_EL1), 0x6000_0005 | TCO | BTYPE)
            .unwrap();

        let saved = save_vcpu(&fake).unwrap();
        assert_eq!(saved.core.pstate, 0xc5);
        assert_eq!(saved.core.spsr_el1, 0x6000_0005);

        let mut noncanonical = saved;
        noncanonical.core.pstate |= TCO | BTYPE;
        assert!(matches!(
            restore_vcpu(&mut fake, &noncanonical),
            Err(BackendError::InvalidState)
        ));
    }

    /// The MMIO read/completion round-trip: a load stays pending until
    /// `complete_read`, which writes the little-endian data and performs a
    /// completion-only reentry before the backend exposes a sealable boundary.
    #[test]
    fn mmio_load_completion_stages_data_for_the_next_run() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        fake.push_run(mmio_load(0x0900_0018, 4)); // a UARTFR read
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
        // Resuming without completing is fail-closed.
        assert!(matches!(b.run(), Err(BackendError::PendingCompletion)));
        b.complete_read(0x90).unwrap();
        assert!(
            b.kvm()
                .calls
                .windows(2)
                .any(|calls| calls == ["write_mmio_data", "complete_mmio_exit"])
        );
        // The next ordinary run reaches shutdown; no completion remains.
        let exit = b.run().unwrap();
        assert_eq!(exit, CommonExit::Shutdown.into());
        assert_eq!(b.kvm().last_mmio_data, Some(le_data(0x90, 4)));
    }

    /// `map_memory` forwards the (validated, page-aligned) region through the
    /// `unsafe` seam. Driven against the fake so the `unsafe` block is
    /// Miri-reachable (the fake records but never dereferences the pointer —
    /// the real pointer work is `LiveKvm`'s, box-only); the alignment/overlap
    /// validation is exercised too.
    #[test]
    fn map_memory_forwards_a_validated_region_through_the_seam() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        // A page-aligned backing (an mmap-shaped allocation under Miri).
        let mut ram = vec![0u8; 2 * 4096];
        // SAFETY (test): `ram` outlives the `b`orrow here; the fake records the
        // region (slot/gpa/len) and never dereferences the pointer.
        unsafe { b.map_memory(Gpa(0x4000_0000), &mut ram).unwrap() };
        assert_eq!(b.kvm().memslots, vec![(0, 0x4000_0000, 8192)]);

        // Misaligned GPA and zero length fail closed (never reach the seam).
        let mut empty: Vec<u8> = Vec::new();
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x4000_0000), &mut empty) },
            Err(BackendError::Memory(_))
        ));
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x1), &mut ram) },
            Err(BackendError::Memory(_))
        ));

        // Finding 3 (review r1): a second map that overlaps the first must fail
        // closed — never a silent `slot 0` replace. An exact duplicate, a
        // straddling overlap, and a same-base map are all rejected.
        let mut ram2 = vec![0u8; 4096];
        assert!(matches!(
            unsafe { b.map_memory(Gpa(0x4000_0000), &mut ram2) }, // duplicate base
            Err(BackendError::Memory(_))
        ));
        assert!(
            matches!(
                unsafe { b.map_memory(Gpa(0x4000_1000), &mut ram2) }, // straddles the 2nd page of ram
                Err(BackendError::Memory(_))
            ),
            "an overlapping region must fail closed, not replace slot 0"
        );
        // A disjoint region above the first is accepted, with a FRESH slot id.
        unsafe { b.map_memory(Gpa(0x4000_2000), &mut ram2).unwrap() };
        assert_eq!(
            b.kvm().memslots,
            vec![(0, 0x4000_0000, 8192), (1, 0x4000_2000, 4096)],
            "each region gets a unique slot; nothing was replaced"
        );
    }

    #[test]
    fn vgic_irq_acceptance_positive_and_planted_negative() {
        // Meaningful positive: a level driven into PPI 27 becomes active on
        // guest entry, so the backend reports exactly one acceptance.
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
             a one-entry-late application is the planted negative"
        );
        assert!(matches!(
            b.run().unwrap(),
            Exit::Common(CommonExit::Mmio { .. })
        ));
        assert_eq!(b.take_accepted_interrupt(), Some(GicIntId(27)));
        assert_eq!(b.take_accepted_interrupt(), None);
        b.set_pending_irq(None).unwrap();
        let lowered = b.save().unwrap().gic.unwrap();
        assert_eq!(lowered.line_level[0] & (1 << 27), 0);

        // Planted negative: the same asserted line and exit script cannot pass
        // the oracle when the fake kernel deliberately withholds the
        // pending→active transition.
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
    fn stock_vgic_is_honest_about_remaining_nondeterminism() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        assert!(matches!(
            b.run_until(crate::types::Moment(0)),
            Err(BackendError::Unsupported { what: "run_until" })
        ));
        b.inject(crate::arch::arm64::Arm64Injection::Interrupt {
            intid: GicIntId(30),
        })
        .unwrap();
        b.set_pending_irq(Some(GicIntId(27))).unwrap();
        assert!(matches!(
            b.set_pending_irq(Some(GicIntId(1))),
            Err(BackendError::InvalidState)
        ));
        assert_eq!(b.take_accepted_interrupt(), None);

        let caps = b.capabilities();
        assert_eq!(caps.name, "kvm-arm64-vgicv3");
        assert!(!caps.deterministic_rng);
        assert!(caps.arch.in_kernel_gic);
        assert!(!caps.arch.deterministic_cntvct);
        assert!(!caps.arch.enforces_cntv_cval);
    }
}
