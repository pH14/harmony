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
//! with no `/dev/kvm` (`docs/ARM-ALTRA.md` §Evidence-integrity: mechanism
//! attestation without the hardware). The real ioctl path against `/dev/kvm` has
//! **no local oracle** — it is arrival-day only, on the Altra (`hm-7pb`; the
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

use crate::arch::arm64::{Arm64, Arm64VcpuState, GicIntId};
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
/// `KVM_EXIT_HYPERCALL` — a guest `HVC` surfaced to userspace. **Patched-only**:
/// stock KVM/arm64 services guest `HVC`/PSCI in-kernel and never surfaces this.
pub(crate) const KVM_EXIT_HYPERCALL: u32 = 13;

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
            if m.len > 8 {
                return Err(BackendError::Internal("KVM_EXIT_MMIO len > 8"));
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

// --- the register-ID table (`KVM_GET_ONE_REG`/`KVM_SET_ONE_REG`) -------------
// arm64 KVM register IDs are documented encodings (Documentation/virt/kvm/
// api.rst, `arch/arm64/include/uapi/asm/kvm.h`). These are ABI facts.

const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
const KVM_REG_ARM_CORE: u64 = 0x0010_0000_0000_0000;
const KVM_REG_ARM64_SYSREG: u64 = 0x0013_0000_0000_0000;

/// A **core** register ID: `struct kvm_regs` field offset ÷ 4.
const fn core_reg(index: u64) -> u64 {
    KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE | index
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
    for &(id, sel) in SYSREGS {
        *sys_field(&mut s.sysregs, sel) = k.get_one_reg(id)?;
    }
    s.mp_state = k.get_mp_state()?;
    Ok(s)
}

/// Restore the full skeleton vCPU state over the reg-ID table.
pub(crate) fn restore_vcpu<K: Arm64Kvm + ?Sized>(k: &mut K, s: &Arm64VcpuState) -> Result<()> {
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
    k.set_mp_state(s.mp_state)?;
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

    /// `KVM_GET_MP_STATE`.
    fn get_mp_state(&self) -> Result<MpState>;
    /// `KVM_SET_MP_STATE`.
    fn set_mp_state(&mut self, mp: MpState) -> Result<()>;

    /// Stage the data an MMIO **load** completes with, written into the mmap'd
    /// `kvm_run.mmio.data` before the next `run` (the x86 `complete_read`
    /// equivalent, below the trait).
    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()>;

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

    /// Enter the guest: apply any staged read completion, then `KVM_RUN`, then
    /// decode. Re-enters on control exits (`None`).
    fn enter_guest(&mut self) -> Result<Exit<Arm64>> {
        loop {
            if let Some(data) = self.staged_read.take() {
                self.kvm.write_mmio_data(data)?;
            }
            let view = self.kvm.run()?;
            if let Some((exit, pending)) = decode_exit(&view)? {
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
        // SAFETY: the caller upholds `map_memory`'s contract (pinned,
        // page-aligned, unaliased backing live for the backend's lifetime); we
        // forward the same guarantee to the seam. One memslot — arm64 device
        // frames sit below RAM, so there is no hole to split (unlike x86's
        // xAPIC page).
        unsafe {
            self.kvm
                .set_user_memory_region(0, gpa.0, host.as_mut_ptr(), host.len() as u64)?;
        }
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

    fn inject(&mut self, _event: crate::arch::arm64::Arm64Injection) -> Result<()> {
        // The stock backend has no delivery path into the guest for a userspace
        // GIC (the CPU interface + timer PPI couple to the in-kernel vGICv3);
        // real delivery is AA-6-gated (the vGICv3 round-trip verdict). Mirrors
        // stock x86 `KvmBackend::inject` at bring-up.
        Err(BackendError::Unsupported { what: "inject" })
    }

    fn set_pending_irq(&mut self, _id: Option<GicIntId>) -> Result<()> {
        Err(BackendError::Unsupported {
            what: "set_pending_irq",
        })
    }

    fn take_accepted_interrupt(&mut self) -> Option<GicIntId> {
        // No maskable IRQ is ever accepted (no delivery path).
        None
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        match self.pending {
            Pending::MmioLoad { len } => {
                self.staged_read = Some(le_data(value, len));
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
        restore_vcpu(&mut self.kvm, state)
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
            name: "kvm-arm64-stock",
            deterministic_rng: false,
            arch: crate::arch::arm64::Arm64Caps {
                deterministic_cntvct: false,
                enforces_cntv_cval: false,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// A recording fake syscall seam — the portable + Miri test double that asserts
// ioctl *shape* (ordering, the reg-ID set) with no `/dev/kvm` (`docs/ARM-ALTRA`
// §Evidence-integrity: mechanism attestation). Behind `cfg(any(test, ...))` so
// it never ships in a non-test build.
// ---------------------------------------------------------------------------

/// A recording fake [`Arm64Kvm`]: it holds a register map, a scripted queue of
/// `KVM_RUN` views, and an ordered log of every ioctl the backend issued (so a
/// test can assert `vcpu_init` precedes the first `set_one_reg`, etc.).
#[cfg(any(test, feature = "mock"))]
#[derive(Debug, Default)]
pub struct FakeKvm {
    regs: std::collections::BTreeMap<u64, u64>,
    mp_state: MpState,
    run_queue: std::collections::VecDeque<KvmRunView>,
    /// The ordered ioctl log — e.g. `"vcpu_init"`, `"set_one_reg"`, `"run"`.
    pub calls: Vec<&'static str>,
    /// The last MMIO-load data the backend staged (for completion assertions).
    pub last_mmio_data: Option<[u8; 8]>,
    /// Recorded `(slot, gpa, len)` memslots.
    pub memslots: Vec<(u32, u64, u64)>,
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
}

#[cfg(any(test, feature = "mock"))]
impl Arm64Kvm for FakeKvm {
    fn vcpu_init(&mut self) -> Result<()> {
        self.calls.push("vcpu_init");
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

    fn get_mp_state(&self) -> Result<MpState> {
        Ok(self.mp_state)
    }

    fn set_mp_state(&mut self, mp: MpState) -> Result<()> {
        self.calls.push("set_mp_state");
        self.mp_state = mp;
        Ok(())
    }

    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()> {
        self.calls.push("write_mmio_data");
        self.last_mmio_data = Some(data);
        Ok(())
    }

    fn run(&mut self) -> Result<KvmRunView> {
        self.calls.push("run");
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

        // A policy with one ID-reg freeze row → one config-time set_one_reg.
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
        s.mp_state = MpState::Halted;

        b.restore(&s).unwrap();
        assert_eq!(b.save().unwrap(), s);
    }

    /// The MMIO read/completion round-trip: a load stays pending until
    /// `complete_read`, which stages the little-endian data the next `run`
    /// writes back.
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
        // The next run stages the LE data and reaches shutdown.
        let exit = b.run().unwrap();
        assert_eq!(exit, CommonExit::Shutdown.into());
        assert_eq!(b.kvm().last_mmio_data, Some(le_data(0x90, 4)));
    }

    #[test]
    fn stock_is_unsupported_where_it_must_be_and_honestly_undeterministic() {
        let mut fake = FakeKvm::new();
        fake.vcpu_init().unwrap();
        let mut b = Arm64KvmBackend::new(fake);
        b.set_policy(&Arm64Policy::default()).unwrap();

        assert!(matches!(
            b.run_until(crate::types::Moment(0)),
            Err(BackendError::Unsupported { what: "run_until" })
        ));
        assert!(matches!(
            b.inject(crate::arch::arm64::Arm64Injection::Interrupt {
                intid: GicIntId(30)
            }),
            Err(BackendError::Unsupported { what: "inject" })
        ));
        assert!(matches!(
            b.set_pending_irq(Some(GicIntId(30))),
            Err(BackendError::Unsupported { .. })
        ));
        assert_eq!(b.take_accepted_interrupt(), None);

        let caps = b.capabilities();
        assert_eq!(caps.name, "kvm-arm64-stock");
        assert!(!caps.deterministic_rng);
        assert!(!caps.arch.deterministic_cntvct);
        assert!(!caps.arch.enforces_cntv_cval);
    }
}
