// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **box-only** half of the stock KVM/arm64 backend (`tasks/112` M4): the
//! real ioctls behind the [`Arm64Kvm`] syscall seam, gated
//! `all(target_os = "linux", target_arch = "aarch64")`.
//!
//! It **has no local oracle** — the Mac has no `/dev/kvm` (`hm-8l3` REFUSE), so
//! this module is only ever *compiled* locally (the CI aarch64-linux
//! cross-check) and run natively on msr1 during M4 (`hm-7pb`). Its shape (ioctl
//! ordering, the register-ID set, the exit decode) is asserted portably against
//! [`FakeKvm`](crate::FakeKvm); this module wires that shape to the documented
//! kvm/arm64 ABI (`KVM_CREATE_VM` → `KVM_CREATE_VCPU` → `KVM_ARM_VCPU_INIT` with
//! `KVM_ARM_PREFERRED_TARGET`; `KVM_GET_ONE_REG`/`KVM_SET_ONE_REG`;
//! `KVM_SET_USER_MEMORY_REGION`; `KVM_RUN`). Like the x86 `kvm_sys`, it is
//! excluded from the coverage/mutation gates (it cannot run without the box).

use std::os::fd::AsRawFd;

use kvm_bindings::{
    kvm_clear_dirty_log, kvm_clear_dirty_log__bindgen_ty_1, kvm_create_device, kvm_device_attr,
    kvm_enable_cap, kvm_run, kvm_userspace_memory_region, kvm_vcpu_init,
};
use kvm_ioctls::{Cap, DeviceFd, Kvm, VcpuFd, VmFd};

use crate::arm64_kvm::{Arm64Kvm, KvmRunView, RunOffsets, RunPage};
use crate::error::{BackendError, Result};
use crate::types::MpState;

/// The byte offsets of the `kvm_run` fields the decode reads, computed from the
/// **arch-specific** `kvm_run` layout via `offset_of!` (so the portable
/// `RunPage` seam never hard-codes the layout). The MMIO sub-fields and
/// `system_event.type` live in the exit-info union `__bindgen_anon_1`.
const RUN_OFFSETS: RunOffsets = RunOffsets {
    exit_reason: core::mem::offset_of!(kvm_run, exit_reason),
    mmio_phys_addr: core::mem::offset_of!(kvm_run, __bindgen_anon_1.mmio.phys_addr),
    mmio_data: core::mem::offset_of!(kvm_run, __bindgen_anon_1.mmio.data),
    mmio_len: core::mem::offset_of!(kvm_run, __bindgen_anon_1.mmio.len),
    mmio_is_write: core::mem::offset_of!(kvm_run, __bindgen_anon_1.mmio.is_write),
    system_event_type: core::mem::offset_of!(kvm_run, __bindgen_anon_1.system_event.type_),
};

// --- compile-time UAPI pin ---------------------------------------------------
// `docs/VM-EXIT-COUNT-VTIME.md`: verify knowable UAPI surfaces against
// the pinned kernel, never take a constant on faith. The portable `arm64_kvm`
// exit-reason and register-class constants MUST equal the pinned kernel's
// `uapi/linux/kvm.h` (reached here through `kvm-bindings`, generated from those
// headers). This block is **compile-checked** on the aarch64-linux cross-check,
// so a drift — the r3 class-shift (`<< 48` vs `<< 16`) and hypercall-reason
// (`13` = `S390_SIEIC` vs `3`) errors, or any future one — fails the build here
// rather than EINVAL-ing on the box. (The register-class bindings are `u32`;
// widen for the `u64` ID space.)
const _UAPI_PIN: () = {
    assert!(crate::arm64_kvm::KVM_EXIT_MMIO == kvm_bindings::KVM_EXIT_MMIO);
    assert!(crate::arm64_kvm::KVM_EXIT_SYSTEM_EVENT == kvm_bindings::KVM_EXIT_SYSTEM_EVENT);
    assert!(crate::arm64_kvm::KVM_EXIT_INTR == kvm_bindings::KVM_EXIT_INTR);
    assert!(crate::arm64_kvm::KVM_EXIT_FAIL_ENTRY == kvm_bindings::KVM_EXIT_FAIL_ENTRY);
    assert!(crate::arm64_kvm::KVM_EXIT_INTERNAL_ERROR == kvm_bindings::KVM_EXIT_INTERNAL_ERROR);
    assert!(crate::arm64_kvm::KVM_EXIT_HYPERCALL == kvm_bindings::KVM_EXIT_HYPERCALL);
    assert!(crate::arm64_kvm::KVM_REG_ARM64 == kvm_bindings::KVM_REG_ARM64);
    assert!(crate::arm64_kvm::KVM_REG_SIZE_U64 == kvm_bindings::KVM_REG_SIZE_U64);
    assert!(crate::arm64_kvm::KVM_REG_ARM_CORE == kvm_bindings::KVM_REG_ARM_CORE as u64);
    assert!(crate::arm64_kvm::KVM_REG_ARM64_SYSREG == kvm_bindings::KVM_REG_ARM64_SYSREG as u64);
    assert!(crate::arm64_kvm::KVM_REG_ARM_FW == kvm_bindings::KVM_REG_ARM_FW as u64);
    assert!(0x0016 << 16 == kvm_bindings::KVM_REG_ARM_FW_FEAT_BMAP);
    assert!(crate::arm64_kvm::KVM_ARM_VCPU_PSCI_0_2 == kvm_bindings::KVM_ARM_VCPU_PSCI_0_2);
    assert!(crate::arm64_kvm::KVM_MEM_LOG_DIRTY_PAGES == kvm_bindings::KVM_MEM_LOG_DIRTY_PAGES);
    assert!(kvm_bindings::KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2 == 168);
    assert!(kvm_bindings::KVM_DIRTY_LOG_MANUAL_PROTECT_ENABLE == 1);
    assert!(
        crate::arm64_kvm::KVM_DEV_ARM_VGIC_GRP_DIST_REGS
            == kvm_bindings::KVM_DEV_ARM_VGIC_GRP_DIST_REGS
    );
    assert!(
        crate::arm64_kvm::KVM_DEV_ARM_VGIC_GRP_REDIST_REGS
            == kvm_bindings::KVM_DEV_ARM_VGIC_GRP_REDIST_REGS
    );
    assert!(
        crate::arm64_kvm::KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS
            == kvm_bindings::KVM_DEV_ARM_VGIC_GRP_CPU_SYSREGS
    );
    assert!(7 == kvm_bindings::KVM_DEV_ARM_VGIC_GRP_LEVEL_INFO);
};

const GICD_BASE: u64 = 0x0800_0000;
const GICR_BASE: u64 = 0x080a_0000;
/// Unused PPI to which KVM's host-time virtual timer is quarantined. The DTB
/// deliberately advertises only PPI27 for the guest virtual timer, and the
/// Harmony clockevent owns that line through `KVM_IRQ_LINE`; PPI20 is therefore
/// unregistered and masked in the guest.
const QUARANTINED_VTIMER_PPI: u32 = 20;

/// Build a Linux ioctl request number (`_IOC` encoding): direction bits 30-31,
/// size bits 16-29, type bits 8-15, and number bits 0-7.
const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}

/// `_IOW(KVMIO, 0xa3, struct kvm_enable_cap)` from `linux/kvm.h`.
///
/// `kvm-ioctls` 0.25 exposes `VmFd::enable_cap` only on architectures which
/// used this ioctl when that crate's cfg list was written; arm64's newer
/// writable-implementation-ID capability is nevertheless a VM capability and
/// uses the same UAPI ioctl. Keep the request derived from the pinned 104-byte
/// binding, and compile-time-pin both the capability number and structure size.
const KVM_ENABLE_CAP_IOCTL: libc::c_ulong = 0x4068_aea3;

/// `_IOWR(KVMIO, 0xc0, struct kvm_clear_dirty_log)` from `linux/kvm.h`.
/// `kvm-ioctls` 0.25 does not expose this newer VM ioctl on arm64, so keep the
/// request derived from and pinned to the generated 24-byte UAPI structure.
const KVM_CLEAR_DIRTY_LOG_IOCTL: libc::c_ulong =
    ioc(3, 0xAE, 0xC0, size_of::<kvm_clear_dirty_log>() as u64) as libc::c_ulong;

const _: () = {
    assert!(kvm_bindings::KVM_CAP_ARM_WRITABLE_IMP_ID_REGS == 239);
    assert!(size_of::<kvm_enable_cap>() == 104);
    assert!(size_of::<kvm_clear_dirty_log>() == 24);
    assert!(KVM_CLEAR_DIRTY_LOG_IOCTL == 0xc018_aec0);
};

/// Map a `kvm-ioctls` error to the crate's portable [`BackendError`].
fn kvm_err(e: kvm_ioctls::Error) -> BackendError {
    BackendError::Io(std::io::Error::from_raw_os_error(e.errno()))
}

/// The live KVM/arm64 syscall seam: the VM/vCPU fds and the retained pointer to
/// the mmap'd `kvm_run` shared page (so an MMIO-load completion can be written
/// back into `kvm_run.mmio.data` before the next `KVM_RUN`, exactly as the x86
/// `KvmBackend` does).
pub struct LiveKvm {
    // Field order matters for `Drop`: the vCPU must outlive nothing that borrows
    // it; `kvm` is kept alive so its fd outlives the VM/vCPU.
    vcpu: VcpuFd,
    vgic: Option<DeviceFd>,
    _vm: VmFd,
    _kvm: Kvm,
    run: *mut kvm_run,
    mmap_size: usize,
}

impl LiveKvm {
    /// `KVM_CREATE_VM` → `KVM_CREATE_VCPU` (single vCPU) → mmap `kvm_run` →
    /// `KVM_ARM_PREFERRED_TARGET` + `KVM_ARM_VCPU_INIT`.
    ///
    /// # Errors
    /// [`BackendError::Capability`] when the host lacks immediate-exit, manual
    /// dirty-log protection, or writable implementation-ID registers;
    /// [`BackendError::Io`] wraps a failing KVM syscall.
    pub fn new() -> Result<Self> {
        // KVM dirty-log bitmaps are indexed in host pages, while the portable
        // backend contract and all GFN arithmetic are fixed at 4 KiB. Reject a
        // 16/64-KiB arm64 host before opening/configuring a VM rather than
        // silently decode its bitmap with the wrong geometry.
        // SAFETY: `sysconf` has no pointer arguments or memory-safety contract.
        let host_page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        crate::arm64_kvm::require_4k_host_page_size(host_page_size as i64)?;
        let kvm = Kvm::new().map_err(kvm_err)?;
        // MMIO loads are completed with one `KVM_RUN` whose
        // `kvm_run.immediate_exit` bit prevents execution of the following
        // guest instruction.  Kernels without this capability ignore that bit,
        // so accepting them would make restore boundaries depend on host signal
        // timing instead of the deterministic exit stream.
        if !kvm.check_extension(Cap::ImmediateExit) {
            return Err(BackendError::Capability {
                cap: "KVM_CAP_IMMEDIATE_EXIT",
            });
        }
        let vm = kvm.create_vm().map_err(kvm_err)?;
        Self::enable_manual_dirty_log(&vm)?;
        Self::enable_writable_imp_id_regs(&vm)?;
        let vcpu = vm.create_vcpu(0).map_err(kvm_err)?;

        let mmap_size = kvm.get_vcpu_mmap_size().map_err(kvm_err)?;
        if mmap_size < size_of::<kvm_run>() {
            return Err(BackendError::Internal("kvm_run mmap size too small"));
        }
        // SAFETY: map the per-vCPU shared `kvm_run` page; `vcpu`'s fd is valid
        // for `mmap`, offset 0 is the `kvm_run`, and the mapping is unmapped in
        // `Drop`. A `MAP_FAILED` return is converted to an error, never used.
        let run = unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                mmap_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                vcpu.as_raw_fd(),
                0,
            );
            if p == libc::MAP_FAILED {
                return Err(BackendError::Io(std::io::Error::last_os_error()));
            }
            p.cast::<kvm_run>()
        };

        let mut this = Self {
            vcpu,
            vgic: None,
            _vm: vm,
            _kvm: kvm,
            run,
            mmap_size,
        };
        this.vcpu_init()?;
        this.create_vgic()?;
        Ok(this)
    }

    /// Require manual dirty-log protection and enable only the manual mode
    /// bit in `kvm_enable_cap.args[0]`. In particular, do not request
    /// `KVM_DIRTY_LOG_INITIALLY_SET`: snapshots begin from the bitmap KVM
    /// actually reports after registration.
    fn enable_manual_dirty_log(vm: &VmFd) -> Result<()> {
        let capability = kvm_bindings::KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2;
        if vm.check_extension_raw(libc::c_ulong::from(capability)) <= 0 {
            return Err(BackendError::Capability {
                cap: "KVM_CAP_MANUAL_DIRTY_LOG_PROTECT2",
            });
        }
        let cap = kvm_enable_cap {
            cap: capability,
            flags: 0,
            args: [
                u64::from(kvm_bindings::KVM_DIRTY_LOG_MANUAL_PROTECT_ENABLE),
                0,
                0,
                0,
            ],
            pad: [0; 64],
        };
        // SAFETY: `vm` is a live KVM VM fd and `cap` is the pinned 104-byte
        // input structure required by KVM_ENABLE_CAP. The kernel only reads it
        // for this ioctl; the reference remains live for the entire call.
        let result = unsafe { libc::ioctl(vm.as_raw_fd(), KVM_ENABLE_CAP_IOCTL, &cap) };
        if result == 0 {
            Ok(())
        } else {
            Err(BackendError::Io(std::io::Error::last_os_error()))
        }
    }

    /// Make MIDR_EL1, REVIDR_EL1, and AIDR_EL1 VM-scoped writable values
    /// before the vCPU exists. Without this capability, KVM can accept a
    /// `KVM_SET_ONE_REG` whose value happens to equal the boot CPU while an
    /// in-guest `MRS` still exposes whichever physical CPU runs the vCPU.
    fn enable_writable_imp_id_regs(vm: &VmFd) -> Result<()> {
        let capability = kvm_bindings::KVM_CAP_ARM_WRITABLE_IMP_ID_REGS;
        if vm.check_extension_raw(libc::c_ulong::from(capability)) <= 0 {
            return Err(BackendError::Capability {
                cap: "KVM_CAP_ARM_WRITABLE_IMP_ID_REGS",
            });
        }
        let cap = kvm_enable_cap {
            cap: capability,
            flags: 0,
            args: [0; 4],
            pad: [0; 64],
        };
        // SAFETY: `vm` is a live KVM VM fd and `cap` is the pinned 104-byte
        // input structure required by KVM_ENABLE_CAP. The kernel only reads it
        // for this ioctl; the reference remains live for the entire call.
        let result = unsafe { libc::ioctl(vm.as_raw_fd(), KVM_ENABLE_CAP_IOCTL, &cap) };
        if result == 0 {
            Ok(())
        } else {
            Err(BackendError::Io(std::io::Error::last_os_error()))
        }
    }

    /// Move KVM's host-time-backed EL1 virtual-timer output away from PPI27,
    /// which is exclusively owned by Harmony's virtual-time clockevent. This
    /// attribute is write-once before the first `KVM_RUN`.
    fn quarantine_host_vtimer(&self) -> Result<()> {
        let irq = QUARANTINED_VTIMER_PPI;
        self.vcpu
            .set_device_attr(&kvm_device_attr {
                flags: 0,
                group: kvm_bindings::KVM_ARM_VCPU_TIMER_CTRL,
                attr: u64::from(kvm_bindings::KVM_ARM_VCPU_TIMER_IRQ_VTIMER),
                addr: std::ptr::from_ref(&irq) as u64,
            })
            .map_err(kvm_err)
    }

    /// Create the in-kernel GICv3 at the MMIO addresses advertised in the
    /// arm64 board DTB, then finalise it after vCPU initialisation.
    fn create_vgic(&mut self) -> Result<()> {
        let mut device = kvm_create_device {
            type_: kvm_bindings::kvm_device_type_KVM_DEV_TYPE_ARM_VGIC_V3,
            fd: 0,
            flags: 0,
        };
        let vgic = self._vm.create_device(&mut device).map_err(kvm_err)?;

        fn set_addr(vgic: &DeviceFd, attr: u64, value: &u64) -> Result<()> {
            vgic.set_device_attr(&kvm_device_attr {
                flags: 0,
                group: kvm_bindings::KVM_DEV_ARM_VGIC_GRP_ADDR,
                attr,
                addr: std::ptr::from_ref(value) as u64,
            })
            .map_err(kvm_err)
        }

        set_addr(
            &vgic,
            u64::from(kvm_bindings::KVM_VGIC_V3_ADDR_TYPE_DIST),
            &GICD_BASE,
        )?;
        set_addr(
            &vgic,
            u64::from(kvm_bindings::KVM_VGIC_V3_ADDR_TYPE_REDIST),
            &GICR_BASE,
        )?;
        let nr_irqs = crate::arm64_kvm::HARMONY_GIC_NR_IRQS;
        vgic.set_device_attr(&kvm_device_attr {
            flags: 0,
            group: kvm_bindings::KVM_DEV_ARM_VGIC_GRP_NR_IRQS,
            attr: 0,
            addr: std::ptr::from_ref(&nr_irqs) as u64,
        })
        .map_err(kvm_err)?;
        // Migration compatibility handshake: acknowledge this KVM vGIC
        // implementation revision before mutable state and before CTRL_INIT
        // makes the field read-only.
        let mut iidr = 0u32;
        let mut iidr_attr = kvm_device_attr {
            flags: 0,
            group: kvm_bindings::KVM_DEV_ARM_VGIC_GRP_DIST_REGS,
            attr: 0x0008,
            addr: std::ptr::from_mut(&mut iidr) as u64,
        };
        // SAFETY: `addr` points to a live writable `u32` for the duration of
        // this 32-bit distributor-register ioctl.
        unsafe { vgic.get_device_attr(&mut iidr_attr) }.map_err(kvm_err)?;
        vgic.set_device_attr(&kvm_device_attr {
            addr: std::ptr::from_ref(&iidr) as u64,
            ..iidr_attr
        })
        .map_err(kvm_err)?;
        // KVM accepts the per-vCPU timer routing only after the irqchip device
        // and its address windows exist, but before CTRL_INIT finalises it.
        self.quarantine_host_vtimer()?;
        vgic.set_device_attr(&kvm_device_attr {
            flags: 0,
            group: kvm_bindings::KVM_DEV_ARM_VGIC_GRP_CTRL,
            attr: u64::from(kvm_bindings::KVM_DEV_ARM_VGIC_CTRL_INIT),
            addr: 0,
        })
        .map_err(kvm_err)?;
        self.vgic = Some(vgic);
        Ok(())
    }

    fn vgic(&self) -> Result<&DeviceFd> {
        self.vgic
            .as_ref()
            .ok_or(BackendError::Internal("vGICv3 is not initialised"))
    }

    /// Read the current `kvm_run` into the portable [`KvmRunView`] through the
    /// [`RunPage`] seam (whose unsafe pointer logic is Miri-tested in
    /// `arm64_kvm`; this box wiring just supplies the real pointer + offsets).
    fn read_run_view(&self) -> Result<KvmRunView> {
        // SAFETY: `self.run` came from a successful `mmap` of `mmap_size` bytes
        // (≥ `size_of::<kvm_run>()`), live until `Drop`, and `RUN_OFFSETS` names
        // real `kvm_run` fields; every read inside is bounds-checked.
        unsafe { RunPage::new(self.run.cast::<u8>(), self.mmap_size).view(&RUN_OFFSETS) }
    }
}

impl Arm64Kvm for LiveKvm {
    fn vcpu_init(&mut self) -> Result<()> {
        let mut kvi = kvm_vcpu_init::default();
        self._vm.get_preferred_target(&mut kvi).map_err(kvm_err)?;
        // Advertise PSCI 0.2 so KVM's in-kernel PSCI services the guest's HVC
        // PSCI calls (SYSTEM_OFF/RESET/CPU_ON) — the DTB advertises arm,psci-1.0
        // over HVC, and without this bit KVM runs legacy PSCI and returns
        // NOT_SUPPORTED (the guest could never cleanly power off). Set AFTER
        // get_preferred_target, which fills `features` (typically zero).
        kvi.features = crate::arm64_kvm::vcpu_init_features();
        debug_assert!(
            kvi.features[0] & (1 << crate::arm64_kvm::KVM_ARM_VCPU_PSCI_0_2) != 0,
            "vcpu_init must request PSCI 0.2"
        );
        // KVM_ARM_VCPU_INIT returns EINVAL for an unsupported feature, so a
        // successful init is the kernel's confirmation the bit took. (Live PSCI
        // conformance is an M4/msr1 gate; no /dev/kvm oracle on the Mac.)
        self.vcpu.vcpu_init(&kvi).map_err(kvm_err)?;
        Ok(())
    }

    unsafe fn set_user_memory_region(
        &mut self,
        slot: u32,
        gpa: u64,
        host: *mut u8,
        len: u64,
    ) -> Result<()> {
        // SAFETY: this compatibility entry point uses the same pinned backing
        // contract as the flagged registration path and requests logging by
        // default, matching the backend's normal ARM behavior.
        unsafe {
            self.set_user_memory_region_with_flags(
                slot,
                gpa,
                host,
                len,
                kvm_bindings::KVM_MEM_LOG_DIRTY_PAGES,
            )
        }
    }

    unsafe fn set_user_memory_region_with_flags(
        &mut self,
        slot: u32,
        gpa: u64,
        host: *mut u8,
        len: u64,
        flags: u32,
    ) -> Result<()> {
        let region = kvm_userspace_memory_region {
            slot,
            flags,
            guest_phys_addr: gpa,
            memory_size: len,
            userspace_addr: host as u64,
        };
        // SAFETY: the caller upholds `map_memory`'s contract (the backing is
        // pinned, page-aligned, and unaliased for the backend's lifetime), so
        // registering it as a memslot is sound.
        unsafe { self._vm.set_user_memory_region(region) }.map_err(kvm_err)
    }

    fn get_dirty_log(&mut self, slot: u32, size: u64) -> Result<Vec<u64>> {
        let expected = crate::arm64_kvm::dirty_bitmap_words(size)?;
        let size = usize::try_from(size).map_err(|_| BackendError::InvalidState)?;
        let bitmap = self._vm.get_dirty_log(slot, size).map_err(kvm_err)?;
        if bitmap.len() != expected {
            return Err(BackendError::Internal(
                "KVM dirty bitmap has an unexpected size",
            ));
        }
        Ok(bitmap)
    }

    fn clear_dirty_log(
        &mut self,
        slot: u32,
        size: u64,
        first_page: u64,
        num_pages: u32,
        bitmap: &[u64],
    ) -> Result<()> {
        crate::arm64_kvm::validate_clear_dirty_log(size, first_page, num_pages, bitmap)?;
        let clear = kvm_clear_dirty_log {
            slot,
            num_pages,
            first_page,
            __bindgen_anon_1: kvm_clear_dirty_log__bindgen_ty_1 {
                dirty_bitmap: bitmap.as_ptr().cast_mut().cast(),
            },
        };
        // SAFETY: the VM fd is live; `bitmap` is a validated, live slice whose
        // exact word count covers `num_pages`; and KVM reads it only for this
        // synchronous ioctl while the vCPU is stopped at the drain boundary.
        let result = unsafe {
            libc::ioctl(
                self._vm.as_raw_fd(),
                KVM_CLEAR_DIRTY_LOG_IOCTL,
                std::ptr::from_ref(&clear),
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(BackendError::Io(std::io::Error::last_os_error()))
        }
    }

    fn get_one_reg(&self, id: u64) -> Result<u64> {
        let mut data = [0u8; 8];
        self.vcpu.get_one_reg(id, &mut data).map_err(kvm_err)?;
        Ok(u64::from_le_bytes(data))
    }

    fn set_one_reg(&mut self, id: u64, value: u64) -> Result<()> {
        self.vcpu
            .set_one_reg(id, &value.to_le_bytes())
            .map_err(kvm_err)?;
        Ok(())
    }

    fn get_one_reg32(&self, id: u64) -> Result<u32> {
        let mut data = [0u8; 4];
        self.vcpu.get_one_reg(id, &mut data).map_err(kvm_err)?;
        Ok(u32::from_le_bytes(data))
    }

    fn set_one_reg32(&mut self, id: u64, value: u32) -> Result<()> {
        self.vcpu
            .set_one_reg(id, &value.to_le_bytes())
            .map_err(kvm_err)?;
        Ok(())
    }

    fn get_one_reg128(&self, id: u64) -> Result<[u8; 16]> {
        let mut data = [0u8; 16];
        self.vcpu.get_one_reg(id, &mut data).map_err(kvm_err)?;
        Ok(data)
    }

    fn set_one_reg128(&mut self, id: u64, value: [u8; 16]) -> Result<()> {
        self.vcpu.set_one_reg(id, &value).map_err(kvm_err)?;
        Ok(())
    }

    fn get_mp_state(&self) -> Result<MpState> {
        let mp = self.vcpu.get_mp_state().map_err(kvm_err)?;
        // arm64 uses RUNNABLE / STOPPED (a WFI-halted vCPU stays RUNNABLE — KVM
        // blocks it in-kernel; STOPPED is a PSCI power-off). Map STOPPED to the
        // engine's `Halted`. (The exact MP-state contract is AA-6's; this is the
        // skeleton mapping.)
        Ok(if mp.mp_state == kvm_bindings::KVM_MP_STATE_STOPPED {
            MpState::Halted
        } else {
            MpState::Runnable
        })
    }

    fn set_mp_state(&mut self, mp: MpState) -> Result<()> {
        let mp_state = match mp {
            MpState::Runnable => kvm_bindings::KVM_MP_STATE_RUNNABLE,
            MpState::Halted => kvm_bindings::KVM_MP_STATE_STOPPED,
        };
        self.vcpu
            .set_mp_state(kvm_bindings::kvm_mp_state { mp_state })
            .map_err(kvm_err)?;
        Ok(())
    }

    fn set_irq_line(&mut self, id: crate::arch::arm64::GicIntId, level: bool) -> Result<()> {
        let (kind, number) = if id.is_ppi() {
            (kvm_bindings::KVM_ARM_IRQ_TYPE_PPI, id.0)
        } else if id.is_spi() {
            (kvm_bindings::KVM_ARM_IRQ_TYPE_SPI, id.0)
        } else {
            return Err(BackendError::InvalidState);
        };
        let irq = (kind << kvm_bindings::KVM_ARM_IRQ_TYPE_SHIFT)
            | (0 << kvm_bindings::KVM_ARM_IRQ_VCPU_SHIFT)
            | (number << kvm_bindings::KVM_ARM_IRQ_NUM_SHIFT);
        self._vm.set_irq_line(irq, level).map_err(kvm_err)
    }

    fn get_vgic_attr(&self, group: u32, attr: u64, width64: bool) -> Result<u64> {
        let vgic = self.vgic()?;
        if width64 {
            let mut value = 0u64;
            let mut device_attr = kvm_device_attr {
                flags: 0,
                group,
                attr,
                addr: std::ptr::from_mut(&mut value) as u64,
            };
            // SAFETY: `addr` points to the live, writable `u64` value for the
            // duration of the ioctl and the selected group uses a 64-bit ABI.
            unsafe { vgic.get_device_attr(&mut device_attr) }.map_err(kvm_err)?;
            Ok(value)
        } else {
            let mut value = 0u32;
            let mut device_attr = kvm_device_attr {
                flags: 0,
                group,
                attr,
                addr: std::ptr::from_mut(&mut value) as u64,
            };
            // SAFETY: `addr` points to the live, writable `u32` value for the
            // duration of the ioctl and the selected group uses a 32-bit ABI.
            unsafe { vgic.get_device_attr(&mut device_attr) }.map_err(kvm_err)?;
            Ok(u64::from(value))
        }
    }

    fn set_vgic_attr(&mut self, group: u32, attr: u64, width64: bool, value: u64) -> Result<()> {
        let vgic = self.vgic()?;
        if width64 {
            vgic.set_device_attr(&kvm_device_attr {
                flags: 0,
                group,
                attr,
                addr: std::ptr::from_ref(&value) as u64,
            })
            .map_err(kvm_err)
        } else {
            let value = u32::try_from(value).map_err(|_| BackendError::InvalidState)?;
            vgic.set_device_attr(&kvm_device_attr {
                flags: 0,
                group,
                attr,
                addr: std::ptr::from_ref(&value) as u64,
            })
            .map_err(kvm_err)
        }
    }

    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()> {
        // SAFETY: `self.run` is a live mmap of the `kvm_run`; the pending exit is
        // an MMIO load, so writing its `data` staging buffer (through the
        // bounds-checked `RunPage` seam) is the kernel's documented completion
        // path, read back on the next `KVM_RUN`.
        unsafe {
            RunPage::new(self.run.cast::<u8>(), self.mmap_size).write_mmio_data(&RUN_OFFSETS, data)
        }
    }

    fn complete_mmio_exit(&mut self) -> Result<()> {
        // KVM consumes the prior MMIO exit before checking `immediate_exit`.
        // The resulting EINTR therefore means the load/store instruction and
        // PC are architecturally complete while no following guest instruction
        // has executed.
        // SAFETY: `self.run` is this vCPU's live shared `kvm_run` mapping and
        // the vCPU is not concurrently running.
        unsafe { (*self.run).immediate_exit = 1 };
        let result = self.vcpu.run();
        // SAFETY: same exclusive mapping access as above; always clear the
        // one-shot flag before interpreting the ioctl result.
        unsafe { (*self.run).immediate_exit = 0 };
        match result {
            Err(error) if error.errno() == libc::EINTR => Ok(()),
            Err(error) => Err(kvm_err(error)),
            Ok(_) => Err(BackendError::Internal(
                "KVM immediate-exit MMIO completion executed guest code",
            )),
        }
    }

    fn run(&mut self) -> Result<KvmRunView> {
        // Issue `KVM_RUN` through kvm-ioctls' safe wrapper (it uses the mmap'd
        // `kvm_run` we also hold a pointer to), then read the shared page through
        // the `RunPage` seam. kvm-ioctls decodes into `VcpuExit`; we ignore that
        // decode and read the raw fields ourselves so the completion write-back
        // and the pure `decode_exit` stay the single source of truth.
        self.vcpu.run().map_err(kvm_err)?;
        self.read_run_view()
    }
}

impl Drop for LiveKvm {
    fn drop(&mut self) {
        // SAFETY: `self.run` came from `mmap(.., self.mmap_size, ..)` and is
        // unmapped exactly once here.
        unsafe {
            libc::munmap(self.run.cast(), self.mmap_size);
        }
    }
}
