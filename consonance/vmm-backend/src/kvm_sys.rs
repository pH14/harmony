// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::{BTreeMap, VecDeque};
use std::os::fd::AsRawFd;

use kvm_bindings::{
    CpuId, KVM_CAP_X86_USER_SPACE_MSR, KVM_MSR_EXIT_REASON_FILTER, KVM_MSR_EXIT_REASON_INVAL,
    KVM_MSR_EXIT_REASON_UNKNOWN, KVM_MSR_FILTER_MAX_RANGES, Msrs, kvm_enable_cap, kvm_interrupt,
    kvm_mp_state, kvm_msr_entry, kvm_msr_filter, kvm_msr_filter_range, kvm_run, kvm_sregs2,
    kvm_userspace_memory_region, kvm_xsave,
};
use kvm_ioctls::{Cap, Kvm, VcpuFd, VmFd};

use crate::arch::x86::Injection;
use crate::arch::x86::VcpuState;
#[cfg(test)]
use crate::arch::x86::X86Exit;
use crate::arch::x86::{CpuidModel, MsrFilter, X86, X86Caps, X86Completion, X86Policy};
use crate::arch::x86::{
    canonicalize_regs, canonicalize_sregs, canonicalize_xsave_with_restore_bv, restore_xsave_image,
};
use crate::backend::Backend;
use crate::error::{BackendError, Result};
use crate::exit::{Capabilities, Exit, ExitCounts};
use crate::kvm::*;
use crate::region::{MemRegions, split_around_hole};
use crate::types::Gpa;

const KVM_MSR_FILTER_READ: u32 = 1 << 0;
const KVM_MSR_FILTER_WRITE: u32 = 1 << 1;
const KVM_MSR_FILTER_DEFAULT_DENY: u32 = 1 << 0;

const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}

const KVM_RUN: u64 = ioc(0, 0xAE, 0x80, 0);
const KVM_INTERRUPT: u64 = ioc(1, 0xAE, 0x86, size_of::<kvm_interrupt>() as u64);
const KVM_X86_SET_MSR_FILTER: u64 = ioc(1, 0xAE, 0xC6, size_of::<kvm_msr_filter>() as u64);
const KVM_GET_SREGS2: u64 = ioc(2, 0xAE, 0xCC, size_of::<kvm_sregs2>() as u64);
const KVM_SET_SREGS2: u64 = ioc(1, 0xAE, 0xCD, size_of::<kvm_sregs2>() as u64);
const KVM_GET_XSAVE2: u64 = ioc(2, 0xAE, 0xCF, size_of::<kvm_xsave>() as u64);
const KVM_SET_XSAVE: u64 = ioc(1, 0xAE, 0xA5, size_of::<kvm_xsave>() as u64);

pub struct KvmBackend {
    vcpu: VcpuFd,
    vm: VmFd,
    run: *mut kvm_run,
    mmap_size: usize,
    xsave2_size: Option<usize>,
    regions: MemRegions,
    mem_slot_count: u32,
    dirty_log: bool,
    dirty_slots: Vec<(u32, u64, u64)>,
    unlogged_slot: bool,
    msr_filter: Option<MsrFilter>,
    cpuid_installed: bool,
    msr_filter_installed: bool,
    pending: Pending,
    completion: Stage,
    completion_exit: Option<Exit<X86>>,
    pending_irq: Option<u8>,
    readiness_current: bool,
    accepted_irq: VecDeque<u8>,
    counts: ExitCounts,
    #[cfg(feature = "xsave-diagnostics")]
    diagnostic_rip: Option<u64>,
    #[cfg(feature = "xsave-diagnostics")]
    diagnostic_hits: Vec<u64>,
    cancel_run: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl KvmBackend {
    pub fn new() -> Result<KvmBackend> {
        crate::kvm_affinity::check_host_affinity()?;
        let kvm = Kvm::new().map_err(kvm_err)?;
        if !kvm.check_extension(Cap::ImmediateExit) {
            return Err(BackendError::Capability {
                cap: "KVM_CAP_IMMEDIATE_EXIT",
            });
        }
        let vm = kvm.create_vm().map_err(kvm_err)?;
        vm.enable_cap(&kvm_enable_cap {
            cap: kvm_bindings::KVM_CAP_EXCEPTION_PAYLOAD,
            args: [1, 0, 0, 0],
            ..Default::default()
        })
        .map_err(kvm_err)?;
        let vcpu = vm.create_vcpu(0).map_err(kvm_err)?;
        let mmap_size = kvm.get_vcpu_mmap_size().map_err(kvm_err)?;
        if mmap_size < size_of::<kvm_run>() {
            return Err(BackendError::Internal("kvm_run mmap size too small"));
        }
        let xsave2 = vm.check_extension_int(Cap::Xsave2);
        let xsave2_size = (xsave2 > 0).then_some(xsave2 as usize);
        let run = unsafe { mmap_kvm_run(vcpu.as_raw_fd(), mmap_size)? };
        Ok(KvmBackend {
            vcpu,
            vm,
            run,
            mmap_size,
            xsave2_size,
            regions: MemRegions::new(),
            mem_slot_count: 0,
            dirty_log: true,
            dirty_slots: Vec::new(),
            unlogged_slot: false,
            msr_filter: None,
            cpuid_installed: false,
            msr_filter_installed: false,
            pending: Pending::None,
            completion: Stage::CLEAR,
            completion_exit: None,
            pending_irq: None,
            readiness_current: true,
            accepted_irq: VecDeque::new(),
            counts: ExitCounts::default(),
            #[cfg(feature = "xsave-diagnostics")]
            diagnostic_rip: None,
            #[cfg(feature = "xsave-diagnostics")]
            diagnostic_hits: Vec::new(),
            cancel_run: std::sync::Arc::default(),
        })
    }

    pub fn set_dirty_log_enabled(&mut self, enabled: bool) {
        self.dirty_log = enabled;
    }

    pub fn write_guest(&mut self, gpa: Gpa, bytes: &[u8]) -> Result<()> {
        self.regions.write(gpa.0, bytes)
    }

    pub fn read_guest(&self, gpa: Gpa, buf: &mut [u8]) -> Result<()> {
        self.regions.read(gpa.0, buf)
    }

    fn configured(&self) -> bool {
        self.cpuid_installed && self.msr_filter_installed
    }

    fn run_page(&self) -> RunPage {
        // SAFETY: `self.run` is the live `mmap` of `self.mmap_size` bytes, owned
        // by this backend and not aliased by any live reference.
        unsafe { RunPage::new(self.run, self.mmap_size) }
    }

    fn finish_staged_exit(&mut self) -> Result<Option<Exit<X86>>> {
        let page = self.run_page();
        let fd = self.vcpu.as_raw_fd();
        let next = finish_staged_completion(page, &mut self.pending, &mut self.completion, || {
            // SAFETY: the owned vCPU fd references the mapped run page and
            // this entry is restricted to consuming its staged completion.
            let rc = unsafe { raw_kvm_run(fd) };
            if rc < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        })?;
        if let Some(exit) = &next {
            self.counts.bump(exit.reason());
        }
        Ok(next)
    }

    fn finish_or_queue_exit(&mut self) -> Result<()> {
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        self.completion_exit = self.finish_staged_exit()?;
        Ok(())
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        if self.completion_exit.is_some() || self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        self.finish_or_queue_exit()?;
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    fn enter_guest(&mut self) -> Result<Exit<X86>> {
        loop {
            if self.cancel_run.load(std::sync::atomic::Ordering::Acquire) {
                return Err(BackendError::Internal("KVM run canceled by host"));
            }
            match plan_irq_entry(self.run_page(), self.pending_irq, self.readiness_current) {
                IrqEntry::Queue(vector) => {
                    unsafe { raw_interrupt(self.vcpu.as_raw_fd(), u32::from(vector))? };
                    self.pending_irq = None;
                    self.accepted_irq.push_back(vector);
                }
                IrqEntry::Run => {}
            }
            let rc = unsafe { raw_kvm_run(self.vcpu.as_raw_fd()) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                return Err(BackendError::Io(err));
            }
            self.completion = Stage::CLEAR;
            self.readiness_current = true;
            #[cfg(feature = "xsave-diagnostics")]
            {
                let reason = self.run_page().exit_reason();
                if reason == kvm_bindings::KVM_EXIT_DEBUG {
                    let expected = self
                        .diagnostic_rip
                        .take()
                        .ok_or(BackendError::Internal("unexpected diagnostic debug exit"))?;
                    let regs = self.vcpu.get_regs().map_err(kvm_err)?;
                    if regs.rip != expected {
                        return Err(BackendError::Internal("diagnostic breakpoint RIP mismatch"));
                    }
                    self.vcpu
                        .set_guest_debug(&Default::default())
                        .map_err(kvm_err)?;
                    self.diagnostic_hits.push(regs.rip);
                    continue;
                }
            }
            match decode_exit(self.run_page())? {
                Some((exit, pending)) => {
                    self.counts.bump(exit.reason());
                    self.pending = pending;
                    self.completion = Stage::of(&exit, pending);
                    return Ok(exit);
                }
                None => continue,
            }
        }
    }

    fn save_msrs(&self) -> Result<BTreeMap<u32, u64>> {
        let Some(filter) = &self.msr_filter else {
            return Ok(BTreeMap::new());
        };
        let indices: Vec<u32> = filter.allow_indices().collect();
        if indices.is_empty() {
            return Ok(BTreeMap::new());
        }
        let entries: Vec<kvm_msr_entry> = indices
            .iter()
            .map(|&index| kvm_msr_entry {
                index,
                ..Default::default()
            })
            .collect();
        let mut kmsrs = Msrs::from_entries(&entries)
            .map_err(|_| BackendError::Internal("MSR list too large"))?;
        let got = self.vcpu.get_msrs(&mut kmsrs).map_err(kvm_err)?;
        saved_msrs(kmsrs.as_slice(), got, indices.len())
    }

    fn restore_msrs(&self, state: &VcpuState) -> Result<()> {
        if state.msrs.is_empty() {
            return Ok(());
        }
        let entries: Vec<kvm_msr_entry> = state
            .msrs
            .iter()
            .map(|(&index, &data)| kvm_msr_entry {
                index,
                data,
                ..Default::default()
            })
            .collect();
        let kmsrs = Msrs::from_entries(&entries)
            .map_err(|_| BackendError::Internal("MSR list too large"))?;
        let set = self.vcpu.set_msrs(&kmsrs).map_err(kvm_err)?;
        ensure_full_msr_count(set, entries.len())
    }

    fn save_xsave(&self) -> Result<(Vec<u8>, Option<u64>)> {
        let mut bytes = match self.xsave2_size {
            // SAFETY: `vcpu` is a valid vCPU fd; `raw_get_xsave2` allocates and
            // fills exactly `n` bytes (`n >= size_of::<kvm_xsave>()`). Miri-excluded.
            Some(n) => unsafe { raw_get_xsave2(self.vcpu.as_raw_fd(), n)? },
            None => xsave_to_bytes(&self.vcpu.get_xsave().map_err(kvm_err)?),
        };
        let restore_bv = canonicalize_xsave_with_restore_bv(&mut bytes);
        Ok((bytes, restore_bv))
    }

    fn restore_xsave(&self, bytes: &[u8]) -> Result<()> {
        match self.xsave2_size {
            // SAFETY: `vcpu` is valid; `raw_set_xsave` reads `bytes` (the validated
            // host XSAVE2 size). Miri-excluded.
            Some(_) => unsafe { raw_set_xsave(self.vcpu.as_raw_fd(), bytes) },
            None => {
                let xsave = xsave_from_bytes(bytes)?;
                // SAFETY: `xsave` is a validated, fully-initialized 4 KiB
                // `kvm_xsave`; `set_xsave` only reads it into the vCPU.
                unsafe { self.vcpu.set_xsave(&xsave).map_err(kvm_err) }
            }
        }
    }
}

/// # Safety
/// `fd` must be a valid vCPU fd and `len` its `KVM_GET_VCPU_MMAP_SIZE`.
#[cfg(not(miri))]
unsafe fn mmap_kvm_run(fd: std::os::fd::RawFd, len: usize) -> Result<*mut kvm_run> {
    // SAFETY: standard shared mapping of the vCPU fd at offset 0; `len` is the
    // kernel-reported size.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(p.cast::<kvm_run>())
}

#[cfg(miri)]
unsafe fn mmap_kvm_run(_fd: std::os::fd::RawFd, _len: usize) -> Result<*mut kvm_run> {
    Err(BackendError::Internal("mmap unavailable under miri"))
}

/// # Safety
/// `fd` must be a valid vCPU fd whose `kvm_run` is currently mapped.
#[cfg(not(miri))]
unsafe fn raw_kvm_run(fd: std::os::fd::RawFd) -> libc::c_int {
    // SAFETY: `KVM_RUN` takes no argument; the kernel uses the mapped `kvm_run`.
    unsafe { libc::ioctl(fd, KVM_RUN as libc::c_ulong, 0) }
}

#[cfg(miri)]
unsafe fn raw_kvm_run(_fd: std::os::fd::RawFd) -> libc::c_int {
    -1
}

/// # Safety
/// `fd` must be a valid VM fd; `filter`'s range bitmap pointers must be valid for
/// the duration of the call (KVM copies them in).
#[cfg(not(miri))]
unsafe fn raw_set_msr_filter(fd: std::os::fd::RawFd, filter: &kvm_msr_filter) -> Result<()> {
    // SAFETY: `filter` is a valid `kvm_msr_filter`; the ioctl reads it (and the
    // bitmaps it points to) and copies them into the kernel.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_X86_SET_MSR_FILTER as libc::c_ulong,
            filter as *const kvm_msr_filter,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(miri)]
unsafe fn raw_set_msr_filter(_fd: std::os::fd::RawFd, _filter: &kvm_msr_filter) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_interrupt(fd: std::os::fd::RawFd, vector: u32) -> Result<()> {
    let irq = kvm_interrupt { irq: vector };
    // SAFETY: the ioctl reads a `kvm_interrupt` from `&irq` (valid for the call)
    // and copies it into the kernel.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_INTERRUPT as libc::c_ulong,
            &irq as *const kvm_interrupt,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(miri)]
unsafe fn raw_interrupt(_fd: std::os::fd::RawFd, _vector: u32) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_get_sregs2(fd: std::os::fd::RawFd) -> Result<kvm_sregs2> {
    let mut sregs2 = kvm_sregs2::default();
    // SAFETY: the ioctl writes a full `kvm_sregs2` into our out-param.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_GET_SREGS2 as libc::c_ulong,
            &mut sregs2 as *mut kvm_sregs2,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(sregs2)
}

#[cfg(miri)]
unsafe fn raw_get_sregs2(_fd: std::os::fd::RawFd) -> Result<kvm_sregs2> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// # Safety
/// `fd` must be a valid vCPU fd.
#[cfg(not(miri))]
unsafe fn raw_set_sregs2(fd: std::os::fd::RawFd, sregs2: &kvm_sregs2) -> Result<()> {
    // SAFETY: the ioctl reads a full `kvm_sregs2` from `sregs2`.
    let rc = unsafe {
        libc::ioctl(
            fd,
            KVM_SET_SREGS2 as libc::c_ulong,
            sregs2 as *const kvm_sregs2,
        )
    };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(miri)]
unsafe fn raw_set_sregs2(_fd: std::os::fd::RawFd, _sregs2: &kvm_sregs2) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

/// # Safety
/// `fd` must be a valid vCPU fd and `len` the host's `KVM_CAP_XSAVE2` size, so the
/// kernel's `copy_to_user` of `len` bytes stays within the buffer.
#[cfg(not(miri))]
unsafe fn raw_get_xsave2(fd: std::os::fd::RawFd, len: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; len];
    // SAFETY: the ioctl writes exactly `len` bytes into `buf` (its capacity).
    let rc = unsafe { libc::ioctl(fd, KVM_GET_XSAVE2 as libc::c_ulong, buf.as_mut_ptr()) };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(buf)
}

#[cfg(miri)]
unsafe fn raw_get_xsave2(_fd: std::os::fd::RawFd, len: usize) -> Result<Vec<u8>> {
    Ok(vec![0u8; len])
}

/// # Safety
/// `fd` must be a valid vCPU fd and `bytes` at least the host's XSAVE image size,
/// so the kernel's `copy_from_user` stays within the buffer.
#[cfg(not(miri))]
unsafe fn raw_set_xsave(fd: std::os::fd::RawFd, bytes: &[u8]) -> Result<()> {
    // SAFETY: the ioctl reads the host XSAVE size from `bytes` (its length is the
    // validated `KVM_CAP_XSAVE2` size).
    let rc = unsafe { libc::ioctl(fd, KVM_SET_XSAVE as libc::c_ulong, bytes.as_ptr()) };
    if rc < 0 {
        return Err(BackendError::Io(std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(miri)]
unsafe fn raw_set_xsave(_fd: std::os::fd::RawFd, _bytes: &[u8]) -> Result<()> {
    Err(BackendError::Internal("ioctl unavailable under miri"))
}

impl Drop for KvmBackend {
    fn drop(&mut self) {
        // SAFETY: `self.run` came from `mmap_kvm_run(.., self.mmap_size)` and is
        // unmapped exactly once here. Excluded under Miri (never mapped there).
        #[cfg(not(miri))]
        unsafe {
            libc::munmap(self.run.cast::<libc::c_void>(), self.mmap_size);
        }
    }
}

impl KvmBackend {
    fn install_cpuid(&mut self, model: &CpuidModel) -> Result<()> {
        let entries = cpuid_entries(model);
        let cpuid = CpuId::from_entries(&entries)
            .map_err(|_| BackendError::Internal("CPUID table too large for KVM"))?;
        self.vcpu.set_cpuid2(&cpuid).map_err(kvm_err)?;
        self.cpuid_installed = true;
        Ok(())
    }

    fn install_msr_filter(&mut self, filter: &MsrFilter) -> Result<()> {
        if filter.allow_inkernel.len() > KVM_MSR_FILTER_MAX_RANGES as usize {
            return Err(BackendError::Memory("too many MSR filter ranges"));
        }
        let mut cap = kvm_enable_cap {
            cap: KVM_CAP_X86_USER_SPACE_MSR,
            ..Default::default()
        };
        cap.args[0] = u64::from(
            KVM_MSR_EXIT_REASON_FILTER | KVM_MSR_EXIT_REASON_UNKNOWN | KVM_MSR_EXIT_REASON_INVAL,
        );
        self.vm.enable_cap(&cap).map_err(kvm_err)?;

        let mut bitmaps: Vec<Vec<u8>> = Vec::with_capacity(filter.allow_inkernel.len());
        let mut ranges = [kvm_msr_filter_range::default(); KVM_MSR_FILTER_MAX_RANGES as usize];
        for (i, r) in filter.allow_inkernel.iter().enumerate() {
            let nbytes = r.count.div_ceil(8) as usize;
            bitmaps.push(vec![0xFFu8; nbytes]);
            ranges[i] = kvm_msr_filter_range {
                flags: KVM_MSR_FILTER_READ | KVM_MSR_FILTER_WRITE,
                nmsrs: r.count,
                base: r.base,
                bitmap: bitmaps[i].as_mut_ptr(),
            };
        }
        let kfilter = kvm_msr_filter {
            flags: KVM_MSR_FILTER_DEFAULT_DENY,
            ranges,
        };
        // SAFETY: `kfilter` and its bitmap pointers are valid for this call; KVM
        // copies them in. Excluded under Miri.
        unsafe { raw_set_msr_filter(self.vm.as_raw_fd(), &kfilter)? };

        self.msr_filter = Some(filter.clone());
        self.msr_filter_installed = true;
        Ok(())
    }

    fn check_completion_clear(&self) -> Result<()> {
        check_completion_clear(
            self.pending,
            self.completion,
            self.completion_exit.is_some(),
        )
    }

    fn run_guarded_entry(&mut self) -> Result<()> {
        self.check_completion_clear()?;
        // SAFETY: the owned vCPU is stopped and the ioctl writes one complete SREGS2 value.
        let sregs = unsafe { raw_get_sregs2(self.vcpu.as_raw_fd())? };
        let fd = self.vcpu.as_raw_fd();
        let continuation = prepare_snapshot_run(
            self.run_page(),
            sregs.cr8,
            &mut self.pending,
            &mut self.completion,
            || {
                // SAFETY: the owned vCPU has a run page requesting immediate exit and,
                // when a completion was staged, no pending value left to supply.
                let rc = unsafe { raw_kvm_run(fd) };
                if rc < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            },
        )?;
        if let Some(exit) = continuation {
            self.counts.bump(exit.reason());
            self.completion_exit = Some(exit);
            return Err(BackendError::PendingCompletion);
        }
        Ok(())
    }

    fn drain_staged_completion(&mut self) -> Result<()> {
        if !self.completion.drains_on_state_read() {
            return Ok(());
        }
        self.run_guarded_entry()
    }
}

impl Backend for KvmBackend {
    type A = X86;

    fn set_policy(&mut self, policy: &X86Policy) -> Result<()> {
        self.install_cpuid(&policy.cpuid)?;
        self.install_msr_filter(&policy.msr_filter)
    }

    unsafe fn map_memory(&mut self, gpa: Gpa, host: &mut [u8]) -> Result<()> {
        self.regions
            .insert(gpa.0, host.as_mut_ptr(), host.len() as u64)?;
        const LAPIC_MMIO_PAGE: u64 = 0xFEE0_0000;
        let host_base = host.as_ptr() as u64;
        let base_slot = self.mem_slot_count;
        let mut registered = 0u32;
        let dirty_slots_before = self.dirty_slots.len();
        let flags = if self.dirty_log {
            kvm_bindings::KVM_MEM_LOG_DIRTY_PAGES
        } else {
            0
        };
        for (i, part) in
            split_around_hole(gpa.0, host.len() as u64, LAPIC_MMIO_PAGE, 0x1000).enumerate()
        {
            let region = kvm_userspace_memory_region {
                slot: base_slot + i as u32,
                flags,
                guest_phys_addr: part.gpa,
                memory_size: part.size,
                userspace_addr: host_base + part.host_off,
            };
            if let Err(e) = unsafe { self.vm.set_user_memory_region(region) }.map_err(kvm_err) {
                self.regions.rollback_last();
                self.dirty_slots.truncate(dirty_slots_before);
                for j in 0..registered {
                    let undo = kvm_userspace_memory_region {
                        slot: base_slot + j,
                        flags: 0,
                        guest_phys_addr: 0,
                        memory_size: 0,
                        userspace_addr: 0,
                    };
                    let _ = unsafe { self.vm.set_user_memory_region(undo) };
                }
                return Err(e);
            }
            if self.dirty_log {
                self.dirty_slots
                    .push((base_slot + i as u32, part.gpa, part.size));
            }
            registered += 1;
        }
        self.mem_slot_count += registered;
        if !self.dirty_log {
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
        let mut gfns = Vec::new();
        for &(slot, gpa, size) in &self.dirty_slots {
            let bitmap = self
                .vm
                .get_dirty_log(slot, size as usize)
                .map_err(kvm_err)?;
            crate::region::decode_dirty_bitmap(gpa, size, &bitmap, &mut gfns);
        }
        gfns.sort_unstable();
        gfns.dedup();
        Ok(gfns)
    }

    #[cfg(feature = "xsave-diagnostics")]
    fn diagnostic_breakpoint(&mut self, rip: u64) -> Result<()> {
        if self.diagnostic_rip.is_some() || !self.diagnostic_hits.is_empty() {
            return Err(BackendError::Internal("diagnostic breakpoint already used"));
        }
        let mut debug = kvm_bindings::kvm_guest_debug {
            control: kvm_bindings::KVM_GUESTDBG_ENABLE | kvm_bindings::KVM_GUESTDBG_USE_HW_BP,
            ..Default::default()
        };
        debug.arch.debugreg[0] = rip;
        debug.arch.debugreg[7] = 0x401;
        self.vcpu.set_guest_debug(&debug).map_err(kvm_err)?;
        self.diagnostic_rip = Some(rip);
        Ok(())
    }

    #[cfg(feature = "xsave-diagnostics")]
    fn diagnostic_debug_hits(&self) -> Vec<u64> {
        self.diagnostic_hits.clone()
    }

    fn run(&mut self) -> Result<Exit<X86>> {
        if !self.configured() {
            return Err(BackendError::NotConfigured);
        }
        self.check_completion_clear()?;
        self.enter_guest()
    }

    fn inject(&mut self, event: Injection) -> Result<()> {
        match event {
            Injection::Interrupt { vector } => {
                self.pending_irq = Some(vector);
                Ok(())
            }
            Injection::Nmi => self.vcpu.nmi().map_err(kvm_err),
        }
    }

    fn set_pending_irq(&mut self, vector: Option<u8>) -> Result<()> {
        self.pending_irq = vector;
        Ok(())
    }

    fn take_accepted_interrupt(&mut self) -> Option<u8> {
        self.accepted_irq.pop_front()
    }

    fn complete_read(&mut self, value: u64) -> Result<()> {
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        let scalar_completion = matches!(self.pending, Pending::IoIn { .. } | Pending::Rdmsr);
        apply_complete_read(self.run_page(), self.pending, value)?;
        self.pending = Pending::None;
        self.completion = Stage::AWAITING_FINISH;
        if scalar_completion {
            self.finish_or_queue_exit()?;
        }
        Ok(())
    }

    fn complete_fault(&mut self) -> Result<()> {
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        apply_complete_fault(self.run_page(), self.pending)?;
        self.pending = Pending::None;
        self.completion = Stage::AWAITING_FINISH;
        self.finish_or_queue_exit()
    }

    fn complete_ok(&mut self) -> Result<()> {
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        apply_complete_ok(self.run_page(), self.pending)?;
        self.pending = Pending::None;
        self.completion = Stage::AWAITING_FINISH;
        self.finish_or_queue_exit()
    }

    fn complete_hypercall(&mut self, _ret: u64) -> Result<()> {
        Err(BackendError::NoPendingRead)
    }

    fn complete_arch(&mut self, _completion: X86Completion) -> Result<()> {
        Err(BackendError::BadCompletion)
    }

    fn retire_pending_completion(&mut self) -> Result<()> {
        KvmBackend::retire_pending_completion(self)
    }

    fn finish_exit(&mut self) -> Result<Option<Exit<X86>>> {
        if let Some(exit) = self.completion_exit.take() {
            return Ok(Some(exit));
        }
        if self.pending != Pending::None {
            return Err(BackendError::PendingCompletion);
        }
        if self.completion.acknowledged {
            return Ok(None);
        }
        self.finish_staged_exit()
    }

    fn prepare_snapshot(&mut self) -> Result<()> {
        if !self.configured() {
            return Err(BackendError::NotConfigured);
        }
        self.run_guarded_entry()
    }

    fn save(&mut self) -> Result<VcpuState> {
        self.drain_staged_completion()?;
        let regs = self.vcpu.get_regs().map_err(kvm_err)?;
        // SAFETY: `vcpu` is a valid vCPU fd; `raw_get_sregs2` writes a full
        // `kvm_sregs2` (incl. flags/PDPTRs). Excluded under Miri.
        let sregs2 = unsafe { raw_get_sregs2(self.vcpu.as_raw_fd())? };
        let dregs = self.vcpu.get_debug_regs().map_err(kvm_err)?;
        let kevents = self.vcpu.get_vcpu_events().map_err(kvm_err)?;
        let mp = self.vcpu.get_mp_state().map_err(kvm_err)?;
        let xcrs = self.vcpu.get_xcrs().map_err(kvm_err)?;
        let (xsave, xsave_restore_bv) = self.save_xsave()?;
        let msrs = self.save_msrs()?;

        let mut sregs = from_kvm_sregs2(&sregs2);
        canonicalize_sregs(&mut sregs);
        let mut regs = from_kvm_regs(&regs);
        canonicalize_regs(&mut regs);
        Ok(VcpuState {
            regs,
            sregs,
            xcr0: xcr0_of(&xcrs),
            debugregs: from_kvm_debugregs(&dregs),
            events: from_kvm_events(&kevents),
            mp_state: mp_from_kvm(mp.mp_state),
            msrs,
            xsave,
            xsave_restore_bv,
        })
    }

    fn validate_restore_state(&self, state: &VcpuState) -> Result<()> {
        let xsave_len = self.xsave2_size.unwrap_or(size_of::<kvm_xsave>());
        validate_restore_shape(state, self.msr_filter.as_ref(), xsave_len)?;
        restore_xsave_image(&state.xsave, state.xsave_restore_bv).map(|_| ())
    }

    fn restore(&mut self, state: &VcpuState) -> Result<()> {
        self.check_completion_clear()?;
        self.drain_staged_completion()?;
        self.validate_restore_state(state)?;
        let xsave = restore_xsave_image(&state.xsave, state.xsave_restore_bv)?;

        self.vcpu
            .set_regs(&to_kvm_regs(&state.regs))
            .map_err(kvm_err)?;
        restore_sregs2_with_flush(&state.sregs, |sregs| {
            // SAFETY: the owned vCPU is stopped and `sregs` is a complete live
            // kvm_sregs2 value, including saved flags/PDPTRs. No KVM_RUN occurs
            // between the transient WP write and exact target write. Pure
            // sequencing is Miri-tested; the ioctl runs in hardware acceptance.
            unsafe { raw_set_sregs2(self.vcpu.as_raw_fd(), sregs) }
        })?;
        self.vcpu
            .set_debug_regs(&to_kvm_debugregs(&state.debugregs))
            .map_err(kvm_err)?;
        self.vcpu
            .set_vcpu_events(&to_kvm_restore_events(&state.events))
            .map_err(kvm_err)?;
        let mp = kvm_mp_state {
            mp_state: mp_to_kvm(state.mp_state),
        };
        self.vcpu.set_mp_state(mp).map_err(kvm_err)?;
        self.vcpu.set_xcrs(&xcrs_of(state.xcr0)).map_err(kvm_err)?;
        self.restore_xsave(&xsave)?;
        self.restore_msrs(state)?;

        self.run_page().set_cr8(state.sregs.cr8);
        self.pending_irq = None;
        self.accepted_irq.clear();
        self.readiness_current = false;
        let _ = plan_irq_entry(self.run_page(), None, false);
        Ok(())
    }

    fn exit_counts(&self) -> ExitCounts {
        self.counts
    }

    fn reset_exit_counts(&mut self) {
        self.counts = ExitCounts::default();
    }

    fn capabilities(&self) -> Capabilities<X86Caps> {
        kvm_capabilities()
    }

    fn cancellation_flag(&self) -> Option<std::sync::Arc<std::sync::atomic::AtomicBool>> {
        Some(std::sync::Arc::clone(&self.cancel_run))
    }
}

#[cfg(test)]
mod xsave_diagnostic {
    use super::*;

    use crate::arch::x86::{CpuidEntry, MsrRange};
    use crate::exit::CommonExit;
    use std::fmt::Write as _;
    use std::fs;
    use std::path::{Path, PathBuf};

    const ACKNOWLEDGED_WRITE: Stage = Stage {
        staged: true,
        acknowledged: true,
    };

    #[test]
    #[ignore = "live x86 KVM snapshot preparation; requires /dev/kvm"]
    fn snapshot_preparation_preserves_state_except_raw_presence() {
        check_snapshot_preparation(false);
    }

    #[test]
    #[ignore = "raw XSAVE presence characterization; requires /dev/kvm"]
    fn snapshot_preparation_raw_presence_stability() {
        check_snapshot_preparation(true);
    }

    fn check_snapshot_preparation(require_raw_stability: bool) {
        let mut raw_stable = true;
        for restore_bv in [0, 2, 3] {
            let mut ram = MmapRam::new(RAM_LEN).unwrap();
            ram.as_mut_bytes()[CODE_GPA] = 0xf4;
            let mut backend = KvmBackend::new().unwrap();
            // SAFETY: the aligned owned RAM mapping outlives the backend and remains fixed during entry.
            unsafe { backend.map_memory(Gpa(0), ram.as_mut_bytes()).unwrap() };
            backend.set_policy(&diagnostic_policy()).unwrap();
            let mut state = backend.save().unwrap();
            state.regs.rip = CODE_GPA as u64;
            state.regs.rflags = 2 | (1 << 16);
            state.sregs.cs.base = 0;
            state.sregs.cs.selector = 0;
            state.sregs.cr4 |= (1 << 9) | (1 << 18);
            state.sregs.cr8 = 7;
            state.xcr0 = 3;
            state.xsave_restore_bv = Some(restore_bv);
            backend.restore(&state).unwrap();
            backend.pending_irq = Some(0x40);
            let before = backend.save().unwrap();
            let memory = ram.as_mut_bytes().to_vec();
            let counts = backend.exit_counts();
            let readiness = backend.readiness_current;
            let mut after = before.clone();
            for preparation in 1..=2 {
                backend.prepare_snapshot().unwrap();
                let observed = backend.save().unwrap();
                println!(
                    "SNAPSHOT_PREPARATION seed_bv={restore_bv} preparation={preparation} raw_before={:?} raw_after={:?}",
                    after.xsave_restore_bv, observed.xsave_restore_bv
                );
                if preparation == 2 {
                    raw_stable &= observed.xsave_restore_bv == after.xsave_restore_bv;
                }
                let mut without_presence_change = observed.clone();
                without_presence_change.xsave_restore_bv = before.xsave_restore_bv;
                assert_eq!(without_presence_change, before);
                assert_eq!(ram.as_mut_bytes(), memory);
                assert_eq!(backend.exit_counts(), counts);
                assert_eq!(backend.pending_irq, Some(0x40));
                assert_eq!(backend.readiness_current, readiness);
                assert_eq!(backend.pending, Pending::None);
                assert_eq!(backend.completion, Stage::CLEAR);
                assert!(backend.completion_exit.is_none());
                assert_eq!(observed.sregs.cr8, 7);
                println!(
                    "SNAPSHOT_PREPARATION seed_bv={restore_bv} preparation={preparation} cpu_equal_except_raw_presence=true ram_equal=true counts_equal=true readiness_equal=true pending_equal=true"
                );
                after = observed;
            }

            backend.pending = Pending::IoIn {
                data_offset: 0,
                size: 1,
            };
            assert!(matches!(
                backend.prepare_snapshot(),
                Err(BackendError::PendingCompletion)
            ));
            assert_eq!(backend.save().unwrap(), after);
            backend.pending = Pending::None;
            backend.completion = Stage::AWAITING_FINISH;
            assert!(matches!(
                backend.prepare_snapshot(),
                Err(BackendError::PendingCompletion)
            ));
            assert_eq!(backend.save().unwrap(), after);
            assert_eq!(backend.completion, Stage::AWAITING_FINISH);
            backend.completion = ACKNOWLEDGED_WRITE;
            backend.prepare_snapshot().unwrap();
            assert_eq!(backend.completion, Stage::CLEAR);
            after = backend.save().unwrap();
            backend.pending_irq = None;
            assert!(matches!(
                backend.run().unwrap(),
                Exit::Common(CommonExit::Idle)
            ));
            let continued = backend.save().unwrap();
            println!(
                "SNAPSHOT_PREPARATION_HLT seed_bv={restore_bv} raw_before={:?} raw_after={:?}",
                after.xsave_restore_bv, continued.xsave_restore_bv
            );
            raw_stable &= continued.xsave_restore_bv == after.xsave_restore_bv;
            assert_eq!(continued.xsave, after.xsave);
        }
        if require_raw_stability {
            assert!(
                raw_stable,
                "raw XSAVE presence changed after repeated preparation or guest HLT; see retained transitions"
            );
        }
    }

    #[test]
    #[ignore = "native KVM restore boundary guards; requires /dev/kvm"]
    fn snapshot_restore_rejects_each_pending_state_without_mutation() {
        let mut backend = KvmBackend::new().unwrap();
        backend.set_policy(&diagnostic_policy()).unwrap();
        let before = backend.save().unwrap();
        let mut replacement = before.clone();
        replacement.regs.rip ^= 0x100;
        let cases = [
            (
                Pending::IoIn {
                    data_offset: 0,
                    size: 1,
                },
                false,
                false,
            ),
            (Pending::MmioLoad { len: 4 }, false, false),
            (Pending::Rdmsr, false, false),
            (Pending::Wrmsr, false, false),
            (Pending::None, true, false),
            (Pending::None, false, true),
        ];
        for (pending, staged, queued) in cases {
            backend.pending = pending;
            backend.completion = if staged {
                Stage::AWAITING_FINISH
            } else {
                Stage::CLEAR
            };
            backend.completion_exit = queued.then_some(Exit::Common(CommonExit::Idle));
            backend.pending_irq = Some(0x40);
            let counts = backend.exit_counts();
            let readiness = backend.readiness_current;
            let queued_before = format!("{:?}", backend.completion_exit);
            assert_eq!(backend.save().unwrap(), before);
            assert!(matches!(
                backend.restore(&replacement),
                Err(BackendError::PendingCompletion)
            ));
            assert!(matches!(
                backend.prepare_snapshot(),
                Err(BackendError::PendingCompletion)
            ));
            assert_eq!(backend.pending, pending);
            assert_eq!(backend.completion.staged, staged);
            assert_eq!(format!("{:?}", backend.completion_exit), queued_before);
            assert_eq!(backend.pending_irq, Some(0x40));
            assert_eq!(backend.exit_counts(), counts);
            assert_eq!(backend.readiness_current, readiness);
            backend.pending = Pending::None;
            backend.completion = Stage::CLEAR;
            backend.completion_exit = None;
            assert_eq!(backend.save().unwrap(), before);
        }
        backend.restore(&replacement).unwrap();
        assert_eq!(backend.save().unwrap(), replacement);
    }

    #[test]
    #[ignore = "native KVM restore boundary guards; requires /dev/kvm"]
    fn snapshot_restore_drains_an_acknowledged_write_completion() {
        let mut backend = KvmBackend::new().unwrap();
        backend.set_policy(&diagnostic_policy()).unwrap();
        let before = backend.save().unwrap();
        let mut replacement = before.clone();
        replacement.regs.rip ^= 0x100;

        backend.pending = Pending::None;
        backend.completion = ACKNOWLEDGED_WRITE;
        let counts = backend.exit_counts();
        assert_eq!(backend.save().unwrap(), before);
        assert_eq!(backend.completion, Stage::CLEAR);
        assert_eq!(backend.exit_counts(), counts);

        backend.completion = ACKNOWLEDGED_WRITE;
        backend.restore(&replacement).unwrap();
        assert_eq!(backend.completion, Stage::CLEAR);
        assert_eq!(backend.pending, Pending::None);
        assert_eq!(backend.exit_counts(), counts);
        assert_eq!(backend.save().unwrap(), replacement);
    }

    struct EntryFixture {
        backend: KvmBackend,
        ram: MmapRam,
    }

    fn prefault_entry_memory(backend: &mut KvmBackend, size: usize) {
        assert!(
            backend
                .vm
                .check_extension_raw(kvm_bindings::KVM_CAP_PRE_FAULT_MEMORY.into())
                > 0,
            "XSAVE_PREFAULT_UNSUPPORTED capability=KVM_CAP_PRE_FAULT_MEMORY"
        );
        let mut range = kvm_bindings::kvm_pre_fault_memory {
            gpa: 0,
            size: size as u64,
            ..Default::default()
        };
        let request = ioc(
            3,
            0xae,
            0xd5,
            size_of::<kvm_bindings::kvm_pre_fault_memory>() as u64,
        );
        while range.size != 0 {
            let remaining = range.size;
            // SAFETY: the live vCPU fd receives the matching ioctl's initialized, writable ABI struct, which remains valid for the call.
            let result = unsafe {
                libc::ioctl(
                    backend.vcpu.as_raw_fd(),
                    request as libc::c_ulong,
                    &mut range,
                )
            };
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EINTR) {
                    continue;
                }
                if error.raw_os_error() == Some(libc::EOPNOTSUPP) {
                    panic!("XSAVE_PREFAULT_UNSUPPORTED vcpu_mode error={error}");
                }
                panic!(
                    "XSAVE_PREFAULT_FAILED gpa={} remaining={} error={error}",
                    range.gpa, range.size
                );
            }
            assert!(range.size < remaining, "XSAVE_PREFAULT_FAILED no progress");
            assert_eq!(range.gpa + range.size, size as u64);
        }
        println!("XSAVE_PREFAULT_COMPLETE bytes={size} guest_instructions=0");
    }

    fn entry_fixture(program: &[u8], seed: u64, xcr0: u64) -> EntryFixture {
        let long_mode = std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some();
        let mut ram = MmapRam::new(RAM_LEN * 2).unwrap();
        if std::env::var_os("XSAVE_ENTRY_PREFAULT").is_some() {
            for byte in ram.as_mut_bytes().iter_mut().step_by(4096) {
                // SAFETY: each byte is a valid exclusive reference into the live RAM mapping; volatile writes materialize host pages before read-prefaulting them.
                unsafe { std::ptr::write_volatile(byte, 0) };
            }
        }
        ram.as_mut_bytes()[CODE_GPA..CODE_GPA + program.len()].copy_from_slice(program);
        ram.as_mut_bytes()[GUEST_XRSTOR_GPA + 24..GUEST_XRSTOR_GPA + 28]
            .copy_from_slice(&0x1f80u32.to_le_bytes());
        let mut backend = KvmBackend::new().unwrap();
        if std::env::var_os("XSAVE_ENTRY_PREFAULT").is_some() {
            backend.set_dirty_log_enabled(false);
        }
        // SAFETY: the owned aligned RAM mapping stays fixed and outlives the backend in EntryFixture.
        unsafe { backend.map_memory(Gpa(0), ram.as_mut_bytes()).unwrap() };
        backend.set_policy(&diagnostic_policy()).unwrap();
        let mut state = backend.save().unwrap();
        state.regs.rip = CODE_GPA as u64;
        state.regs.rflags = 2 | (1 << 16);
        state.sregs.cs.base = 0;
        state.sregs.cs.selector = 0;
        state.sregs.ds.base = 0;
        state.sregs.ds.selector = 0;
        state.sregs.cr0 &= !((1 << 2) | (1 << 3));
        state.sregs.cr4 |= (1 << 9) | (1 << 18);
        state.sregs.cr8 = 7;
        if long_mode {
            for (address, entry) in [(0x4000, 0x5003u64), (0x5000, 0x6003), (0x6000, 0x83)] {
                ram.as_mut_bytes()[address..address + 8].copy_from_slice(&entry.to_le_bytes());
            }
            state.sregs.cr0 |= (1 << 31) | 1;
            state.sregs.cr4 |= 1 << 5;
            state.sregs.cr3 = 0x4000;
            state.sregs.efer |= (1 << 8) | (1 << 10);
            state.sregs.cs.selector = 8;
            state.sregs.cs.l = 1;
            state.sregs.cs.db = 0;
            state.sregs.cs.type_ = 11;
            state.sregs.cs.s = 1;
            state.sregs.cs.present = 1;
            state.sregs.cs.limit = u32::MAX;
            state.sregs.cs.g = 1;
        }
        state.xcr0 = xcr0;
        state.xsave_restore_bv = Some(seed);
        backend.restore(&state).unwrap();
        if std::env::var_os("XSAVE_ENTRY_PREFAULT").is_some() {
            prefault_entry_memory(&mut backend, RAM_LEN * 2);
        }
        EntryFixture { backend, ram }
    }

    fn entry_program(mode: &str, xcr0: u64) -> Vec<u8> {
        if mode == "hlt" {
            return vec![0xf4];
        }
        if std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some() {
            let mut program = vec![0xb8];
            program.extend_from_slice(&(xcr0 as u32).to_le_bytes());
            program.extend_from_slice(&[0x31, 0xd2]);
            if mode == "xrstor" {
                program.extend_from_slice(&[0x48, 0x0f, 0xae, 0x2c, 0x25]);
                program.extend_from_slice(&(GUEST_XRSTOR_GPA as u32).to_le_bytes());
            }
            program.extend_from_slice(&[0x48, 0x0f, 0xae, 0x24, 0x25]);
            program.extend_from_slice(&(GUEST_XSAVE_GPA as u32).to_le_bytes());
            program.push(0xf4);
            return program;
        }
        let mut program = vec![0x66, 0xb8];
        program.extend_from_slice(&(xcr0 as u32).to_le_bytes());
        program.extend_from_slice(&[0x66, 0x31, 0xd2]);
        if mode == "xrstor" {
            program.extend_from_slice(&[0x67, 0x0f, 0xae, 0x2d]);
            program.extend_from_slice(&(GUEST_XRSTOR_GPA as u32).to_le_bytes());
        }
        program.extend_from_slice(&[0x67, 0x0f, 0xae, 0x25]);
        program.extend_from_slice(&(GUEST_XSAVE_GPA as u32).to_le_bytes());
        program.push(0xf4);
        program
    }

    #[derive(Debug, PartialEq, Eq)]
    struct EntryEndpoint {
        state: VcpuState,
        ram: Vec<u8>,
        exits: ExitCounts,
    }

    fn canonical_entry_setup_matches(
        left: &EntryEndpoint,
        right: &EntryEndpoint,
        left_raw: &[u8],
        right_raw: &[u8],
    ) -> bool {
        let (Some(left_bv), Some(right_bv)) =
            (left.state.xsave_restore_bv, right.state.xsave_restore_bv)
        else {
            return false;
        };
        if left.state.xsave.len() < 520 || left_raw.len() < 520 || right_raw.len() != left_raw.len()
        {
            return false;
        }
        let canonical_bv = u64::from_le_bytes(left.state.xsave[512..520].try_into().unwrap());
        let allowed = 3 & !canonical_bv;
        if (left_bv ^ right_bv) & !allowed != 0
            || u64::from_le_bytes(left_raw[512..520].try_into().unwrap()) != left_bv
            || u64::from_le_bytes(right_raw[512..520].try_into().unwrap()) != right_bv
            || left_raw[..512] != right_raw[..512]
            || left_raw[520..] != right_raw[520..]
        {
            return false;
        }
        let mut right_state = right.state.clone();
        right_state.xsave_restore_bv = Some(left_bv);
        left.state == right_state && left.ram == right.ram && left.exits == right.exits
    }

    #[test]
    fn canonical_entry_setup_allows_only_init_x87_sse_presence() {
        let endpoint = |presence: u64| EntryEndpoint {
            state: VcpuState {
                xsave: vec![0; 576],
                xsave_restore_bv: Some(presence),
                ..Default::default()
            },
            ram: vec![0; 16],
            exits: ExitCounts::default(),
        };
        let raw = |presence: u64| {
            let mut bytes = vec![0; 576];
            bytes[512..520].copy_from_slice(&presence.to_le_bytes());
            bytes
        };
        let left = endpoint(0);
        for presence in [0, 1, 2, 3] {
            let right = endpoint(presence);
            assert!(canonical_entry_setup_matches(
                &left,
                &right,
                &raw(0),
                &raw(presence)
            ));
            assert_eq!(left == right, presence == 0);
        }
        assert!(!canonical_entry_setup_matches(
            &left,
            &endpoint(4),
            &raw(0),
            &raw(4)
        ));
        for mutation in 0..6 {
            let mut right = endpoint(2);
            let mut right_raw = raw(2);
            match mutation {
                0 => right.state.xsave[160] = 1,
                1 => right.state.regs.rbx = 1,
                2 => right.ram[0] = 1,
                3 => right.exits.idle = 1,
                4 => right_raw[160] = 1,
                _ => right_raw[520] = 1,
            }
            assert!(!canonical_entry_setup_matches(
                &left,
                &right,
                &raw(0),
                &right_raw
            ));
        }
        for active in [1, 2] {
            let mut active_left = endpoint(0);
            let mut active_right = endpoint(active);
            active_left.state.xsave[512] = active as u8;
            active_right.state.xsave[512] = active as u8;
            active_left.state.xsave[160] = 1;
            active_right.state.xsave[160] = 1;
            assert!(!canonical_entry_setup_matches(
                &active_left,
                &active_right,
                &raw(0),
                &raw(active)
            ));
        }
    }

    fn retain_entry(fixture: &mut EntryFixture, directory: &Path, phase: &str) -> EntryEndpoint {
        let state = fixture.backend.save().unwrap();
        let path = directory.join(phase);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("state.txt"), format!("{state:#?}")).unwrap();
        let length = fixture.backend.xsave2_size.expect("XSAVE2 required");
        // SAFETY: the fixture owns a stopped vCPU and the ioctl allocates its capability-sized buffer.
        let raw = unsafe { raw_get_xsave2(fixture.backend.vcpu.as_raw_fd(), length).unwrap() };
        write_image(&path, "raw-xsave.bin", &raw);
        write_image(&path, "ram.bin", fixture.ram.as_mut_bytes());
        EntryEndpoint {
            state,
            ram: fixture.ram.as_mut_bytes().to_vec(),
            exits: fixture.backend.exit_counts(),
        }
    }

    fn entry_endpoint(fixture: &mut EntryFixture, directory: &Path, phase: &str) -> EntryEndpoint {
        if let Some(path) = std::env::var_os("XSAVE_TRACE_MARKER") {
            fs::write(path, format!("XSAVE_PHASE_BEGIN {phase}\n")).unwrap();
        }
        fixture.backend.reset_exit_counts();
        assert!(matches!(
            fixture.backend.run().unwrap(),
            Exit::Common(CommonExit::Idle)
        ));
        if let Some(path) = std::env::var_os("XSAVE_TRACE_MARKER") {
            fs::write(path, format!("XSAVE_PHASE_END {phase}\n")).unwrap();
        }
        retain_entry(fixture, directory, phase)
    }

    fn restore_entry(fixture: &mut EntryFixture, saved: &EntryEndpoint) {
        fixture.ram.as_mut_bytes().copy_from_slice(&saved.ram);
        fixture.backend.restore(&saved.state).unwrap();
        fixture.backend.prepare_snapshot().unwrap();
        fixture.backend.reset_exit_counts();
    }

    #[test]
    #[ignore = "requires Linux KVM with AVX and a fresh XSAVE_MXCSR_REPORT_DIR"]
    fn ymm_without_sse_uabi_mxcsr_bytes_survive_capture_and_restore() {
        let root =
            PathBuf::from(std::env::var_os("XSAVE_MXCSR_REPORT_DIR").expect("report directory"));
        fs::create_dir(&root).unwrap();
        let program = if std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some() {
            [0x0f, 0xae, 0x1c, 0x25, 0x00, 0x20, 0x00, 0x00, 0xf4]
        } else {
            [0x67, 0x0f, 0xae, 0x1d, 0x00, 0x20, 0x00, 0x00, 0xf4]
        };
        let mut original = entry_fixture(&program, 0, 7);
        let mut state = original.backend.save().unwrap();
        state.xsave[XSTATE_BV].copy_from_slice(&4u64.to_le_bytes());
        state.xsave[SSE_MXCSR].copy_from_slice(&0x3f80u32.to_le_bytes());
        state.xsave[576] = 0x5a;
        state.xsave_restore_bv = Some(4);
        original.backend.restore(&state).unwrap();
        let saved = retain_entry(&mut original, &root, "captured");
        assert_eq!(saved.state.xsave[SSE_MXCSR], 0x3f80u32.to_le_bytes());
        let continued = entry_endpoint(&mut original, &root, "continued");
        let mut cold = entry_fixture(&program, 0, 7);
        restore_entry(&mut cold, &saved);
        let restored = entry_endpoint(&mut cold, &root, "restored");
        assert_eq!(restored.state.xsave[SSE_MXCSR], 0x3f80u32.to_le_bytes());
        assert_eq!(continued.state.xsave[SSE_MXCSR], 0x3f80u32.to_le_bytes());
        fs::write(
            root.join("guest-mxcsr.txt"),
            format!(
                "continued={:02x?} restored={:02x?}\n",
                &continued.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + 4],
                &restored.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + 4]
            ),
        )
        .unwrap();
        assert_eq!(continued.ram, restored.ram);
    }

    #[test]
    #[ignore = "requires Linux KVM AVX, long mode and a fresh XSAVE_MXCSR_REPORT_DIR"]
    fn natural_avx_mxcsr_survives_capture_and_restore() {
        assert!(std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some());
        let root =
            PathBuf::from(std::env::var_os("XSAVE_MXCSR_REPORT_DIR").expect("report directory"));
        fs::create_dir(&root).unwrap();
        let program = [
            0xc5, 0xfd, 0x76, 0xc0, 0xc5, 0xf0, 0x57, 0xc9, 0xc4, 0xe3, 0x7d, 0x18, 0xc1, 0x00,
            0x0f, 0xae, 0x14, 0x25, 0x1c, 0x30, 0x00, 0x00, 0xe6, 0x80, 0x0f, 0xae, 0x1c, 0x25,
            0x00, 0x20, 0x00, 0x00, 0xf4,
        ];
        let mut original = entry_fixture(&program, 0, 7);
        original.ram.as_mut_bytes()[0x301c..0x3020].copy_from_slice(&0x3f80u32.to_le_bytes());
        let exit = original.backend.run().unwrap();
        assert!(matches!(
            exit,
            Exit::Arch(X86Exit::Io {
                port: 0x80,
                size: 1,
                write: Some(0),
            })
        ));
        original.backend.prepare_snapshot().unwrap();
        let saved = retain_entry(&mut original, &root, "captured");
        assert_eq!(saved.state.regs.rip, CODE_GPA as u64 + 24);
        assert_eq!(saved.state.xsave[SSE_MXCSR], 0x3f80u32.to_le_bytes());
        let continued = entry_endpoint(&mut original, &root, "continued");
        let mut cold = entry_fixture(&program, 0, 7);
        restore_entry(&mut cold, &saved);
        let restored = entry_endpoint(&mut cold, &root, "restored");
        for endpoint in [&continued, &restored] {
            assert_eq!(
                endpoint.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + 4],
                0x3f80u32.to_le_bytes()
            );
        }
        assert_eq!(continued.ram, restored.ram);
    }

    #[test]
    #[ignore = "bounded raw XSAVE entry differential; KVM and XSAVE_ENTRY_REPORT_DIR required"]
    #[cfg(not(miri))]
    fn snapshot_entry_restores_match_uninterrupted_execution() {
        check_entry_restores(false);
    }

    #[test]
    #[ignore = "canonicalized raw-entry witness; KVM, long mode and XSAVE_ENTRY_REPORT_DIR required"]
    #[cfg(not(miri))]
    fn snapshot_canonical_entry_restores_match_uninterrupted_execution() {
        assert!(std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some());
        check_entry_restores(true);
    }

    #[cfg(not(miri))]
    fn check_entry_restores(canonical: bool) {
        let root = PathBuf::from(
            std::env::var_os("XSAVE_ENTRY_REPORT_DIR").expect("report directory required"),
        );
        fs::create_dir(&root).unwrap();
        let mut failures = Vec::new();
        let selected = std::env::var("XSAVE_ENTRY_CASE").ok();
        if let Some(selected) = &selected {
            assert!(
                [3, 7]
                    .into_iter()
                    .any(
                        |xcr0| [0, 2, 3].into_iter().any(|seed| ["hlt", "xsave", "xrstor"]
                            .into_iter()
                            .any(|mode| *selected == format!("xcr{xcr0}-seed{seed}-{mode}")))
                    )
            );
        }
        for xcr0 in [3, 7] {
            for seed in [0, 2, 3] {
                for mode in ["hlt", "xsave", "xrstor"] {
                    if canonical && mode == "hlt" {
                        continue;
                    }
                    let label = format!("xcr{xcr0}-seed{seed}-{mode}");
                    if selected.as_ref().is_some_and(|selected| selected != &label) {
                        continue;
                    }
                    let directory = root.join(&label);
                    fs::create_dir(&directory).unwrap();
                    let mut program = entry_program(mode, xcr0);
                    if canonical {
                        canonical::append_canonicalization(&mut program);
                    }
                    fs::write(directory.join("guest-program.bin"), &program).unwrap();
                    let mut reference = entry_fixture(&program, seed, xcr0);
                    reference.backend.prepare_snapshot().unwrap();
                    let initial = retain_entry(&mut reference, &directory, "initial");
                    let expected = entry_endpoint(&mut reference, &directory, "reference");
                    let mut source = entry_fixture(&program, seed, xcr0);
                    source.backend.prepare_snapshot().unwrap();
                    let saved = retain_entry(&mut source, &directory, "captured");
                    let repeated = retain_entry(&mut source, &directory, "repeated");
                    let continued = entry_endpoint(&mut source, &directory, "continued");
                    let mut cold = entry_fixture(&program, seed, xcr0);
                    restore_entry(&mut cold, &saved);
                    let cold_initial = retain_entry(&mut cold, &directory, "cold-initial");
                    let cold_endpoint = entry_endpoint(&mut cold, &directory, "cold");
                    let mut poison = saved.state.clone();
                    poison.regs.rbx ^= 1;
                    poison.xsave[SSE_XMM0].copy_from_slice(&ACTIVE_XMM0);
                    let canonical_bv = header(&poison.xsave).0 | 2;
                    poison.xsave[XSTATE_BV].copy_from_slice(&canonical_bv.to_le_bytes());
                    poison.xsave_restore_bv = Some(poison.xsave_restore_bv.unwrap() | 2);
                    source.backend.restore(&poison).unwrap();
                    if canonical && mode == "xrstor" {
                        source.ram.as_mut_bytes()[GUEST_XRSTOR_GPA + 24..GUEST_XRSTOR_GPA + 28]
                            .copy_from_slice(&0x3f80u32.to_le_bytes());
                    }
                    let negative = entry_endpoint(&mut source, &directory, "negative");
                    restore_entry(&mut source, &saved);
                    let reused_initial = retain_entry(&mut source, &directory, "reused-initial");
                    let reused = entry_endpoint(&mut source, &directory, "reused");
                    let checks = [
                        ("independent-initial", initial == saved),
                        ("repeated-capture", saved == repeated),
                        ("captured-continuation", expected == continued),
                        ("cold-initial", saved == cold_initial),
                        ("cold-continuation", expected == cold_endpoint),
                        ("negative-detected", expected != negative),
                        (
                            "negative-guest-output-detected",
                            mode != "xsave"
                                || expected.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN]
                                    != negative.ram
                                        [GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN],
                        ),
                        ("reused-initial", saved == reused_initial),
                        ("reused-continuation", expected == reused),
                    ];
                    let mut report = format!(
                        "label={label}\ncanonical={canonical}\ninitial_bv={:?}\nendpoint_bv={:?}\n",
                        initial.state.xsave_restore_bv, expected.state.xsave_restore_bv
                    );
                    if mode != "hlt" {
                        writeln!(
                            report,
                            "guest_bv={}",
                            header(
                                &expected.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN]
                            )
                            .0
                        )
                        .unwrap();
                    }
                    if canonical {
                        for (name, left, right, left_phase, right_phase) in [
                            ("independent-setup", &initial, &saved, "initial", "captured"),
                            (
                                "cold-setup",
                                &saved,
                                &cold_initial,
                                "captured",
                                "cold-initial",
                            ),
                            (
                                "reused-setup",
                                &saved,
                                &reused_initial,
                                "captured",
                                "reused-initial",
                            ),
                        ] {
                            let left_raw =
                                fs::read(directory.join(left_phase).join("raw-xsave.bin")).unwrap();
                            let right_raw =
                                fs::read(directory.join(right_phase).join("raw-xsave.bin"))
                                    .unwrap();
                            let passed =
                                canonical_entry_setup_matches(left, right, &left_raw, &right_raw);
                            writeln!(report, "required:{name}={passed}").unwrap();
                            if !passed {
                                failures.push(format!("{label}: {name}"));
                            }
                        }
                        if mode == "xrstor" {
                            let output = GUEST_XSAVE_GPA + 24..GUEST_XSAVE_GPA + 28;
                            let passed = negative.ram[output.clone()] == 0x3f80u32.to_le_bytes()
                                && expected.ram[output] == 0x1f80u32.to_le_bytes();
                            writeln!(report, "required:negative-xrstor-mxcsr-output={passed}")
                                .unwrap();
                            if !passed {
                                failures.push(format!("{label}: negative-xrstor-mxcsr-output"));
                            }
                        }
                    }
                    for (check, passed) in checks {
                        let diagnostic = canonical
                            && matches!(
                                check,
                                "independent-initial" | "cold-initial" | "reused-initial"
                            );
                        let scope = if diagnostic {
                            "diagnostic-pre-entry-identity"
                        } else {
                            "required"
                        };
                        writeln!(report, "{scope}:{check}={passed}").unwrap();
                        if !passed && !diagnostic {
                            failures.push(format!("{label}: {check}"));
                        }
                    }
                    fs::write(directory.join("report.txt"), &report).unwrap();
                    println!("{report}");
                }
            }
        }
        assert!(
            failures.is_empty(),
            "entry differential failures: {failures:?}"
        );
    }

    #[test]
    #[ignore = "hardware debug exit differential; KVM and XSAVE_REENTRY_REPORT_DIR required"]
    fn snapshot_entry_debug_reentry_preserves_guest_observation() {
        let root = PathBuf::from(
            std::env::var_os("XSAVE_REENTRY_REPORT_DIR").expect("report directory required"),
        );
        fs::create_dir(&root).unwrap();
        let mut failures = Vec::new();
        for xcr0 in [3, 7] {
            for seed in [0, 2, 3] {
                let label = format!("xcr{xcr0}-seed{seed}");
                let directory = root.join(&label);
                fs::create_dir(&directory).unwrap();
                let program = entry_program("xrstor", xcr0);
                let stop_rip = CODE_GPA
                    + if std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some() {
                        16
                    } else {
                        17
                    };
                let mut reference = entry_fixture(&program, seed, xcr0);
                reference.backend.prepare_snapshot().unwrap();
                let expected = entry_endpoint(&mut reference, &directory, "reference");
                let mut interrupted = entry_fixture(&program, seed, xcr0);
                interrupted.backend.prepare_snapshot().unwrap();
                let mut debug = kvm_bindings::kvm_guest_debug {
                    control: kvm_bindings::KVM_GUESTDBG_ENABLE
                        | kvm_bindings::KVM_GUESTDBG_USE_HW_BP,
                    ..Default::default()
                };
                debug.arch.debugreg[0] = stop_rip as u64;
                debug.arch.debugreg[7] = 0x401;
                interrupted.backend.vcpu.set_guest_debug(&debug).unwrap();
                // SAFETY: the fixture owns the live vCPU and run mapping; the debug exit is read only after ioctl return.
                assert_eq!(
                    unsafe { raw_kvm_run(interrupted.backend.vcpu.as_raw_fd()) },
                    0
                );
                // SAFETY: the stopped vCPU's mapped run page is live and the exit reason is initialized by KVM_RUN.
                let reason = unsafe { (*interrupted.backend.run).exit_reason };
                assert_eq!(reason, kvm_bindings::KVM_EXIT_DEBUG);
                let stopped = retain_entry(&mut interrupted, &directory, "debug-stop");
                assert_eq!(stopped.state.regs.rip, stop_rip as u64);
                assert!(
                    stopped.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN]
                        .iter()
                        .all(|byte| *byte == 0)
                );
                interrupted
                    .backend
                    .vcpu
                    .set_guest_debug(&Default::default())
                    .unwrap();
                let actual = entry_endpoint(&mut interrupted, &directory, "resumed");
                let report = format!(
                    "label={label}\nreference_guest_bv={}\nresumed_guest_bv={}\ncomplete_endpoint_equal={}\nram_equal={}\nstate_equal={}\n",
                    header(&expected.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN]).0,
                    header(&actual.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN]).0,
                    expected == actual,
                    expected.ram == actual.ram,
                    expected.state == actual.state
                );
                fs::write(directory.join("report.txt"), &report).unwrap();
                println!("{report}");
                if expected != actual {
                    failures.push(label);
                }
            }
        }
        assert!(
            failures.is_empty(),
            "debug reentry differences: {failures:?}"
        );
    }

    #[cfg(not(miri))]
    mod canonical {
        use super::*;
        core::arch::global_asm!(include_str!("xsave_canonical_guest.S"));

        unsafe extern "C" {
            static harmony_xsave_canonical_start: u8;
            static harmony_xsave_canonical_standard: u8;
            static harmony_xsave_canonical_compacted: u8;
            static harmony_xsave_canonical_saved: u8;
            static harmony_xsave_canonical_padding: u8;
            static harmony_xsave_canonical_end: u8;
        }

        fn canonical_guest() -> (&'static [u8], [usize; 4]) {
            let start = std::ptr::addr_of!(harmony_xsave_canonical_start) as usize;
            let end = std::ptr::addr_of!(harmony_xsave_canonical_end) as usize;
            assert!(end > start && end - start < GUEST_XSAVE_GPA - CODE_GPA);
            // SAFETY: the assembly labels delimit one immutable linked text blob; no host execution or relocation is required.
            let program = unsafe { std::slice::from_raw_parts(start as *const u8, end - start) };
            let stops = [
                std::ptr::addr_of!(harmony_xsave_canonical_standard) as usize,
                std::ptr::addr_of!(harmony_xsave_canonical_compacted) as usize,
                std::ptr::addr_of!(harmony_xsave_canonical_saved) as usize,
                std::ptr::addr_of!(harmony_xsave_canonical_padding) as usize,
            ]
            .map(|address| CODE_GPA + address - start);
            (program, stops)
        }

        pub(super) fn append_canonicalization(program: &mut Vec<u8>) {
            assert_eq!(program.pop(), Some(0xf4));
            let (canonical, stops) = canonical_guest();
            program.extend_from_slice(&[0x41, 0xbd, 1, 0, 0, 0, 0x45, 0x31, 0xf6]);
            program.extend_from_slice(&canonical[stops[2] - CODE_GPA..]);
            assert!(program.len() < GUEST_XSAVE_GPA - CODE_GPA);
        }

        fn canonical_fixture(mode: u64, compacted: bool, canonical: bool) -> EntryFixture {
            let (program, _) = canonical_guest();
            let mut fixture = entry_fixture(program, 0, 7);
            let mut policy = diagnostic_policy();
            policy
                .cpuid
                .entries
                .iter_mut()
                .find(|entry| entry.leaf == 0xd && entry.subleaf == 1)
                .unwrap()
                .eax = 2;
            fixture.backend.install_cpuid(&policy.cpuid).unwrap();
            fixture.ram.as_mut_bytes()[GUEST_XRSTOR_GPA + 28..GUEST_XRSTOR_GPA + 32]
                .copy_from_slice(&0x3f80u32.to_le_bytes());
            let mut state = fixture.backend.save().unwrap();
            state.regs.r12 = mode;
            state.regs.r13 = u64::from(canonical);
            state.regs.r14 = u64::from(compacted);
            fixture.backend.restore(&state).unwrap();
            fixture.backend.prepare_snapshot().unwrap();
            fixture
        }

        fn canonical_expected(mode: u64, compacted: bool) -> Vec<u8> {
            let mut image = vec![0; GUEST_XSAVE_LEN];
            image[0..2].copy_from_slice(&0x37fu16.to_le_bytes());
            image[SSE_MXCSR]
                .copy_from_slice(&if mode == 3 { 0x3f80u32 } else { 0x1f80u32 }.to_le_bytes());
            image[SSE_MXCSR_MASK].copy_from_slice(&0xffffu32.to_le_bytes());
            let mut bv = 0u64;
            if matches!(mode, 1 | 2) {
                image[SSE_XMM0].fill(0xff);
                bv |= 2;
            }
            if mode == 2 {
                image[576..592].fill(0xff);
                bv |= 4;
            }
            image[XSTATE_BV].copy_from_slice(&bv.to_le_bytes());
            image[XCOMP_BV]
                .copy_from_slice(&if compacted { (1u64 << 63) | 7 } else { 0 }.to_le_bytes());
            image
        }

        fn canonical_debug_stop(fixture: &mut EntryFixture, rip: usize) {
            let mut debug = kvm_bindings::kvm_guest_debug {
                control: kvm_bindings::KVM_GUESTDBG_ENABLE | kvm_bindings::KVM_GUESTDBG_USE_HW_BP,
                ..Default::default()
            };
            debug.arch.debugreg[0] = rip as u64;
            debug.arch.debugreg[7] = 0x401;
            fixture.backend.vcpu.set_guest_debug(&debug).unwrap();
            // SAFETY: the fixture exclusively owns the live vCPU and its run mapping throughout this synchronous ioctl.
            assert_eq!(unsafe { raw_kvm_run(fixture.backend.vcpu.as_raw_fd()) }, 0);
            // SAFETY: KVM_RUN initialized the exit reason in the still-live run mapping before returning.
            assert_eq!(
                unsafe { (*fixture.backend.run).exit_reason },
                kvm_bindings::KVM_EXIT_DEBUG
            );
            assert_eq!(fixture.backend.vcpu.get_regs().unwrap().rip, rip as u64);
            fixture
                .backend
                .vcpu
                .set_guest_debug(&Default::default())
                .unwrap();
        }

        #[test]
        #[ignore = "D3 whole-buffer guest canonicalization; fixed-CPU KVM, long mode and XSAVE_CANONICAL_REPORT_DIR required"]
        fn snapshot_guest_canonicalization_preserves_complete_endpoints() {
            assert!(std::env::var_os("XSAVE_ENTRY_LONG_MODE").is_some());
            let root = PathBuf::from(
                std::env::var_os("XSAVE_CANONICAL_REPORT_DIR").expect("report directory required"),
            );
            fs::create_dir(&root).unwrap();
            let supported = Kvm::new()
                .unwrap()
                .get_supported_cpuid(kvm_bindings::KVM_MAX_CPUID_ENTRIES)
                .unwrap();
            assert!(
                supported
                    .as_slice()
                    .iter()
                    .any(|entry| entry.function == 1 && entry.ecx & (1 << 28) != 0),
                "D3 unsupported host: AVX required"
            );
            assert!(
                supported
                    .as_slice()
                    .iter()
                    .any(|entry| entry.function == 0xd && entry.index == 1 && entry.eax & 2 != 0),
                "D3 unsupported host: XSAVEC required"
            );
            fs::write(
                root.join("host-supported-cpuid.txt"),
                format!("{supported:#?}"),
            )
            .unwrap();
            let (program, stops) = canonical_guest();
            fs::write(root.join("guest-program.bin"), program).unwrap();
            fs::write(root.join("guest-stops.txt"), format!("{stops:#x?}\n")).unwrap();
            let mut failures = Vec::new();
            let mut raw_divergences = 0;
            for compacted in [false, true] {
                for mode in 0..4 {
                    for canonical in [false, true] {
                        let label = format!("compacted{compacted}-mode{mode}-canonical{canonical}");
                        let directory = root.join(&label);
                        fs::create_dir(&directory).unwrap();
                        let mut reference = canonical_fixture(mode, compacted, canonical);
                        let expected = entry_endpoint(&mut reference, &directory, "reference");
                        let mut source = canonical_fixture(mode, compacted, canonical);
                        let saved = retain_entry(&mut source, &directory, "capture");
                        let continued = entry_endpoint(&mut source, &directory, "continued");
                        let mut cold = canonical_fixture(mode, compacted, canonical);
                        restore_entry(&mut cold, &saved);
                        let cold_endpoint = entry_endpoint(&mut cold, &directory, "cold");
                        let mut poison = saved.state.clone();
                        poison.regs.r12 = if mode == 2 { 0 } else { 2 };
                        source.backend.restore(&poison).unwrap();
                        let poisoned = entry_endpoint(&mut source, &directory, "poisoned");
                        assert_ne!(expected.ram, poisoned.ram);
                        restore_entry(&mut source, &saved);
                        let reused = entry_endpoint(&mut source, &directory, "reused");
                        let mut report = String::new();
                        let mut endpoints = vec![
                            ("continued".to_string(), continued),
                            ("cold".to_string(), cold_endpoint),
                            ("reused".to_string(), reused),
                        ];
                        for (name, rip) in [
                            ("before-save", stops[usize::from(compacted)]),
                            ("after-save", stops[2]),
                            ("padding", stops[3]),
                        ] {
                            if name == "padding" && !canonical {
                                continue;
                            }
                            let mut interrupted = canonical_fixture(mode, compacted, canonical);
                            canonical_debug_stop(&mut interrupted, rip);
                            let actual = entry_endpoint(&mut interrupted, &directory, name);
                            endpoints.push((name.to_string(), actual));
                        }
                        for (name, actual) in endpoints {
                            let equal = expected == actual;
                            let ram_equal = expected.ram == actual.ram;
                            let state_equal = expected.state == actual.state;
                            writeln!(report, "{name}: endpoint_equal={equal} ram_equal={ram_equal} state_equal={state_equal}").unwrap();
                            if canonical && !equal {
                                failures.push(format!("{label}/{name}"));
                            }
                            if !canonical && !ram_equal {
                                raw_divergences += 1;
                            }
                        }
                        if canonical {
                            let actual =
                                &expected.ram[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN];
                            let materialized = canonical_expected(mode, compacted);
                            fs::write(directory.join("expected-image.bin"), &materialized).unwrap();
                            let correct = actual == materialized;
                            writeln!(report, "whole_buffer_expected={correct}").unwrap();
                            if !correct {
                                failures.push(format!("{label}/whole-buffer-values"));
                            }
                        }
                        fs::write(directory.join("report.txt"), &report).unwrap();
                        println!("D3 {label}\n{report}");
                    }
                }
            }
            println!("D3 raw_negative_control_ram_divergences={raw_divergences}");
            fs::write(root.join("summary.txt"), format!("raw_negative_control_ram_divergences={raw_divergences}\ncanonical_failures={failures:?}\n")).unwrap();
            assert!(failures.is_empty(), "D3 canonical failures: {failures:?}");
        }
    }

    const RAM_LEN: usize = 0x4000;
    const CODE_GPA: usize = 0x1000;
    const XSTATE_BV: std::ops::Range<usize> = 512..520;
    const XCOMP_BV: std::ops::Range<usize> = 520..528;
    const SSE_MXCSR: std::ops::Range<usize> = 24..28;
    const SSE_MXCSR_MASK: std::ops::Range<usize> = 28..32;
    const SSE_XMM0: std::ops::Range<usize> = 160..176;
    const GUEST_XSAVE_GPA: usize = 0x2000;
    const GUEST_XRSTOR_GPA: usize = 0x3000;
    const GUEST_XSAVE_PAGE_LEN: usize = 0x1000;
    const GUEST_XSAVE_LEN: usize = 0x340;
    const _: () = assert!(GUEST_XSAVE_GPA + GUEST_XSAVE_PAGE_LEN <= RAM_LEN);
    const _: () = assert!(GUEST_XSAVE_GPA + GUEST_XSAVE_PAGE_LEN <= GUEST_XRSTOR_GPA);
    const _: () = assert!(GUEST_XRSTOR_GPA + GUEST_XSAVE_PAGE_LEN <= RAM_LEN);
    const ACTIVE_XMM0: [u8; 16] = [
        0xA5, 0x5A, 0x3C, 0xC3, 0x96, 0x69, 0x78, 0x87, 0x12, 0x21, 0x34, 0x43, 0x56, 0x65, 0xAB,
        0xBA,
    ];

    struct MmapRam {
        ptr: *mut libc::c_void,
        len: usize,
    }

    impl MmapRam {
        fn new(len: usize) -> std::io::Result<Self> {
            let ptr = unsafe {
                // SAFETY: `len` is a nonzero page-sized mapping length selected by this test.
                libc::mmap(
                    std::ptr::null_mut(),
                    len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                    -1,
                    0,
                )
            };
            if ptr == libc::MAP_FAILED {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(Self { ptr, len })
            }
        }

        fn as_mut_bytes(&mut self) -> &mut [u8] {
            unsafe {
                // SAFETY: `ptr` is a live private mapping owned by this value, and the returned
                // slice is the only mutable borrow used while the guest is stopped.
                std::slice::from_raw_parts_mut(self.ptr.cast::<u8>(), self.len)
            }
        }
    }

    impl Drop for MmapRam {
        fn drop(&mut self) {
            unsafe {
                // SAFETY: `ptr` and `len` came from one successful mmap and are unmapped once.
                libc::munmap(self.ptr, self.len);
            }
        }
    }

    fn diagnostic_policy() -> X86Policy {
        X86Policy {
            cpuid: CpuidModel {
                entries: vec![
                    CpuidEntry {
                        leaf: 0,
                        eax: 0xD,
                        ebx: 0x756E_6547,
                        ecx: 0x6C65_746E,
                        edx: 0x4965_6E69,
                        ..Default::default()
                    },
                    CpuidEntry {
                        leaf: 1,
                        eax: 0x0009_06EC,
                        ebx: 0x0001_0800,
                        ecx: 0x36DA_3203,
                        edx: 0x0F8B_BB7F,
                        ..Default::default()
                    },
                    CpuidEntry {
                        leaf: 0xD,
                        subleaf_significant: true,
                        eax: 7,
                        ebx: 0x340,
                        ecx: 0x340,
                        ..Default::default()
                    },
                    CpuidEntry {
                        leaf: 0xD,
                        subleaf: 1,
                        subleaf_significant: true,
                        ..Default::default()
                    },
                    CpuidEntry {
                        leaf: 0xD,
                        subleaf: 2,
                        subleaf_significant: true,
                        eax: 0x100,
                        ebx: 0x240,
                        ..Default::default()
                    },
                ],
            },
            msr_filter: MsrFilter {
                allow_inkernel: vec![MsrRange {
                    base: 0x174,
                    count: 3,
                }],
            },
        }
    }

    fn header(image: &[u8]) -> (u64, u64) {
        assert!(
            image.len() >= XCOMP_BV.end,
            "XSAVE image is shorter than its header"
        );
        let xstate_bv = u64::from_le_bytes(
            image[XSTATE_BV]
                .try_into()
                .expect("XSAVE XSTATE_BV has eight bytes"),
        );
        let xcomp_bv = u64::from_le_bytes(
            image[XCOMP_BV]
                .try_into()
                .expect("XSAVE XCOMP_BV has eight bytes"),
        );
        (xstate_bv, xcomp_bv)
    }

    fn write_image(dir: &Path, name: &str, image: &[u8]) {
        fs::write(dir.join(name), image)
            .unwrap_or_else(|e| panic!("write {} failed: {e}", dir.join(name).display()));
    }
}
