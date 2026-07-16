// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **box-only** half of the stock KVM/arm64 backend (`tasks/112` M4): the
//! real ioctls behind the [`Arm64Kvm`] syscall seam, gated
//! `all(target_os = "linux", target_arch = "aarch64")`.
//!
//! It **has no local oracle** — the Mac has no `/dev/kvm` (`hm-8l3` REFUSE), so
//! this module is only ever *compiled* locally (the CI aarch64-linux
//! cross-check) and *run* arrival-day on the Altra (`hm-7pb`). Its shape (ioctl
//! ordering, the register-ID set, the exit decode) is asserted portably against
//! [`FakeKvm`](crate::FakeKvm); this module wires that shape to the documented
//! kvm/arm64 ABI (`KVM_CREATE_VM` → `KVM_CREATE_VCPU` → `KVM_ARM_VCPU_INIT` with
//! `KVM_ARM_PREFERRED_TARGET`; `KVM_GET_ONE_REG`/`KVM_SET_ONE_REG`;
//! `KVM_SET_USER_MEMORY_REGION`; `KVM_RUN`). Like the x86 `kvm_sys`, it is
//! excluded from the coverage/mutation gates (it cannot run without the box).

use std::os::fd::AsRawFd;

use kvm_bindings::{kvm_run, kvm_userspace_memory_region, kvm_vcpu_init};
use kvm_ioctls::{Kvm, VcpuFd, VmFd};

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
// `docs/ARM-ALTRA.md` §Evidence-integrity: verify knowable UAPI surfaces against
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
    /// A [`BackendError::Io`] wrapping the failing ioctl's errno.
    pub fn new() -> Result<Self> {
        let kvm = Kvm::new().map_err(kvm_err)?;
        let vm = kvm.create_vm().map_err(kvm_err)?;
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
            _vm: vm,
            _kvm: kvm,
            run,
            mmap_size,
        };
        this.vcpu_init()?;
        Ok(this)
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
        let region = kvm_userspace_memory_region {
            slot,
            flags: 0,
            guest_phys_addr: gpa,
            memory_size: len,
            userspace_addr: host as u64,
        };
        // SAFETY: the caller upholds `map_memory`'s contract (the backing is
        // pinned, page-aligned, and unaliased for the backend's lifetime), so
        // registering it as a memslot is sound.
        unsafe { self._vm.set_user_memory_region(region) }.map_err(kvm_err)
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

    fn write_mmio_data(&mut self, data: [u8; 8]) -> Result<()> {
        // SAFETY: `self.run` is a live mmap of the `kvm_run`; the pending exit is
        // an MMIO load, so writing its `data` staging buffer (through the
        // bounds-checked `RunPage` seam) is the kernel's documented completion
        // path, read back on the next `KVM_RUN`.
        unsafe {
            RunPage::new(self.run.cast::<u8>(), self.mmap_size).write_mmio_data(&RUN_OFFSETS, data)
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
