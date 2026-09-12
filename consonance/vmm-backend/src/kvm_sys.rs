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
use crate::arch::x86::{CpuidModel, MsrFilter, X86, X86Caps, X86Completion, X86Exit, X86Policy};
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
    completion_staged: bool,
    completion_exit: Option<Exit<X86>>,
    pending_irq: Option<u8>,
    readiness_current: bool,
    accepted_irq: VecDeque<u8>,
    counts: ExitCounts,
    cancel_run: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl KvmBackend {
    pub fn new() -> Result<KvmBackend> {
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
            completion_staged: false,
            completion_exit: None,
            pending_irq: None,
            readiness_current: true,
            accepted_irq: VecDeque::new(),
            counts: ExitCounts::default(),
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
        let next =
            finish_staged_completion(page, &mut self.pending, &mut self.completion_staged, || {
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
            self.completion_staged = false;
            self.readiness_current = true;
            match decode_exit(self.run_page())? {
                Some((exit, pending)) => {
                    self.counts.bump(exit.reason());
                    self.pending = pending;
                    self.completion_staged = decoded_exit_stages_completion(&exit, pending);
                    if matches!(exit, Exit::Arch(X86Exit::Io { write: Some(_), .. })) {
                        self.finish_or_queue_exit()?;
                    }
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

    fn run(&mut self) -> Result<Exit<X86>> {
        if !self.configured() {
            return Err(BackendError::NotConfigured);
        }
        if self.pending != Pending::None || self.completion_exit.is_some() || self.completion_staged
        {
            return Err(BackendError::PendingCompletion);
        }
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
        self.completion_staged = true;
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
        self.completion_staged = true;
        self.finish_or_queue_exit()
    }

    fn complete_ok(&mut self) -> Result<()> {
        if self.completion_exit.is_some() {
            return Err(BackendError::PendingCompletion);
        }
        apply_complete_ok(self.run_page(), self.pending)?;
        self.pending = Pending::None;
        self.completion_staged = true;
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
        self.finish_staged_exit()
    }

    fn save(&self) -> Result<VcpuState> {
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
        if self.pending != Pending::None || self.completion_staged {
            return Err(BackendError::PendingCompletion);
        }
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

#[cfg(all(test, not(miri)))]
mod xsave_diagnostic {
    use super::*;

    use crate::arch::x86::{CpuidEntry, MsrRange};
    use crate::exit::CommonExit;
    use crate::types::MpState;
    use std::fmt::Write as _;
    use std::fs;
    use std::path::{Path, PathBuf};

    const RAM_LEN: usize = 0x4000;
    const CODE_GPA: usize = 0x1000;
    const MMIO_GPA: u64 = 0xFEE0_0080;
    const MMIO_VALUE: u8 = 5;
    const XSTATE_BV: std::ops::Range<usize> = 512..520;
    const XCOMP_BV: std::ops::Range<usize> = 520..528;
    const SSE_XMM0: std::ops::Range<usize> = 160..176;
    const ACTIVE_XMM0: [u8; 16] = [
        0xA5, 0x5A, 0x3C, 0xC3, 0x96, 0x69, 0x78, 0x87, 0x12, 0x21, 0x34, 0x43, 0x56, 0x65, 0xAB,
        0xBA,
    ];
    const ZERO_XMM0: [u8; 16] = [0; 16];

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

    fn mmio_program() -> [u8; 13] {
        [
            0x67,
            0x66,
            0xC7,
            0x05,
            MMIO_GPA as u8,
            (MMIO_GPA >> 8) as u8,
            (MMIO_GPA >> 16) as u8,
            (MMIO_GPA >> 24) as u8,
            MMIO_VALUE,
            0,
            0,
            0,
            0xF4,
        ]
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

    fn cpuid_words(backend: &KvmBackend, leaf: u32, subleaf: u32) -> [u32; 4] {
        let cpuid = backend
            .vcpu
            .get_cpuid2(kvm_bindings::KVM_MAX_CPUID_ENTRIES)
            .unwrap_or_else(|e| panic!("KVM_GET_CPUID2 failed: {e}"));
        let entry = cpuid
            .as_slice()
            .iter()
            .find(|entry| entry.function == leaf && entry.index == subleaf)
            .unwrap_or_else(|| panic!("guest CPUID leaf {leaf:#x}/{subleaf} missing"));
        [entry.eax, entry.ebx, entry.ecx, entry.edx]
    }

    fn backend_xcr0_value(backend: &KvmBackend) -> u64 {
        let xcrs = backend
            .vcpu
            .get_xcrs()
            .unwrap_or_else(|e| panic!("KVM_GET_XCRS failed: {e}"));
        xcrs.xcrs
            .iter()
            .find(|xcr| xcr.xcr == 0)
            .map(|xcr| xcr.value)
            .unwrap_or_else(|| panic!("guest XCR0 missing from KVM_GET_XCRS"))
    }

    fn write_image(dir: &Path, name: &str, image: &[u8]) {
        fs::write(dir.join(name), image)
            .unwrap_or_else(|e| panic!("write {} failed: {e}", dir.join(name).display()));
    }

    fn assert_xmm0_bytes(image: &[u8], expected: &[u8; 16], phase: &str, label: &str) {
        assert_eq!(
            &image[SSE_XMM0], expected,
            "{label} XMM0 changed during {phase}"
        );
    }

    fn words_hex(words: [u32; 4]) -> String {
        format!(
            "{:08x},{:08x},{:08x},{:08x}",
            words[0], words[1], words[2], words[3]
        )
    }

    fn run_variant(report_root: &Path, active_sse: bool) {
        let label = if active_sse {
            "active-initialized-sse"
        } else {
            "zero-sse-control"
        };
        let report_dir = report_root.join(label);
        fs::create_dir_all(&report_dir)
            .unwrap_or_else(|e| panic!("create {} failed: {e}", report_dir.display()));

        let mut ram =
            MmapRam::new(RAM_LEN).unwrap_or_else(|e| panic!("guest RAM mmap failed: {e}"));
        ram.as_mut_bytes()[CODE_GPA..CODE_GPA + mmio_program().len()]
            .copy_from_slice(&mmio_program());

        let mut backend =
            KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed for {label}: {e}"));
        let xsave_len = backend
            .xsave2_size
            .unwrap_or_else(|| panic!("KVM_CAP_XSAVE2 is unavailable for {label}"));
        assert!(
            xsave_len >= size_of::<kvm_xsave>(),
            "KVM_CAP_XSAVE2 returned a short image"
        );

        unsafe {
            // SAFETY: `ram` is page-aligned, pinned for this scope, and outlives `backend`.
            backend
                .map_memory(Gpa(0), ram.as_mut_bytes())
                .unwrap_or_else(|e| panic!("map_memory failed for {label}: {e}"));
        }
        backend
            .set_policy(&diagnostic_policy())
            .unwrap_or_else(|e| panic!("set_policy failed for {label}: {e}"));

        let mut state = backend
            .save()
            .unwrap_or_else(|e| panic!("save entry state failed for {label}: {e}"));
        state.sregs.cs.base = 0;
        state.sregs.cs.selector = 0;
        state.sregs.ds.base = 0;
        state.sregs.ds.selector = 0;
        state.sregs.ds.limit = u32::MAX;
        state.sregs.ds.g = 1;
        state.sregs.cr4 |= 1 << 18;
        state.sregs.cr0 &= !((1 << 2) | (1 << 3));
        state.regs.rip = CODE_GPA as u64;
        state.regs.rflags = 2;
        state.regs.rbx = 0;
        state.mp_state = MpState::Runnable;
        state.xcr0 = 3;
        state.xsave_restore_bv = None;
        state.xsave[SSE_XMM0].fill(0);
        state.xsave[XCOMP_BV].fill(0);
        state.xsave[XSTATE_BV].copy_from_slice(&if active_sse {
            2u64.to_le_bytes()
        } else {
            0u64.to_le_bytes()
        });
        if active_sse {
            state.xsave[SSE_XMM0].copy_from_slice(&ACTIVE_XMM0);
        }
        backend
            .restore(&state)
            .unwrap_or_else(|e| panic!("restore entry state failed for {label}: {e}"));

        let cpuid1 = cpuid_words(&backend, 1, 0);
        let cpuid_d0 = cpuid_words(&backend, 0xD, 0);
        let xcr0_before_run = backend_xcr0_value(&backend);
        assert_eq!(
            xcr0_before_run, 3,
            "configured guest XCR0 was not retained for {label}"
        );

        let boundary = backend
            .run()
            .unwrap_or_else(|e| panic!("boundary run failed for {label}: {e}"));
        assert!(
            matches!(
                boundary,
                Exit::Common(CommonExit::Mmio {
                    gpa: Gpa(MMIO_GPA),
                    size: 4,
                    write: Some(_),
                })
            ),
            "{label} did not stop at the expected MMIO boundary: {boundary:?}"
        );
        assert!(
            backend.exit_counts().total() > 0,
            "{label} reported zero executed exits"
        );

        let fd = backend.vcpu.as_raw_fd();
        let raw_before_1 = unsafe {
            // SAFETY: the owned vCPU is stopped at the checked MMIO boundary and the capability
            // sized buffer is retained until KVM finishes the direct ioctl.
            raw_get_xsave2(fd, xsave_len)
        }
        .unwrap_or_else(|e| panic!("first raw KVM_GET_XSAVE2 failed for {label}: {e}"));
        let raw_before_2 = unsafe {
            // SAFETY: the vCPU remains stopped and the second independent buffer has the same
            // kernel-reported capability size as the first direct read.
            raw_get_xsave2(fd, xsave_len)
        }
        .unwrap_or_else(|e| panic!("second raw KVM_GET_XSAVE2 failed for {label}: {e}"));
        let mut canonical_before_1 = raw_before_1.clone();
        let mut canonical_before_2 = raw_before_2.clone();
        let canonical_restore_bv_1 = canonicalize_xsave_with_restore_bv(&mut canonical_before_1);
        let canonical_restore_bv_2 = canonicalize_xsave_with_restore_bv(&mut canonical_before_2);
        write_image(&report_dir, "raw-before-1.bin", &raw_before_1);
        write_image(&report_dir, "raw-before-2.bin", &raw_before_2);
        write_image(&report_dir, "canonical-before-1.bin", &canonical_before_1);
        write_image(&report_dir, "canonical-before-2.bin", &canonical_before_2);

        unsafe {
            // SAFETY: the stopped vCPU owns the direct KVM XSAVE target and `raw_before_1` has
            // the complete capability-sized image returned by KVM_GET_XSAVE2.
            raw_set_xsave(fd, &raw_before_1)
        }
        .unwrap_or_else(|e| panic!("raw KVM_SET_XSAVE failed for {label}: {e}"));
        let raw_after_set = unsafe {
            // SAFETY: the vCPU is still stopped after KVM_SET_XSAVE and the output buffer is
            // capability-sized and exclusively owned by this test.
            raw_get_xsave2(fd, xsave_len)
        }
        .unwrap_or_else(|e| panic!("raw post-SET KVM_GET_XSAVE2 failed for {label}: {e}"));
        let mut canonical_after_set = raw_after_set.clone();
        let canonical_restore_bv_after_set =
            canonicalize_xsave_with_restore_bv(&mut canonical_after_set);
        write_image(&report_dir, "raw-after-set.bin", &raw_after_set);
        write_image(&report_dir, "canonical-after-set.bin", &canonical_after_set);

        backend
            .retire_pending_completion()
            .unwrap_or_else(|e| panic!("retire MMIO write completion failed for {label}: {e}"));

        let endpoint = backend
            .run()
            .unwrap_or_else(|e| panic!("bounded continuation run failed for {label}: {e}"));
        let raw_endpoint = unsafe {
            // SAFETY: the vCPU is stopped after the bounded continuation and the output buffer
            // is capability-sized.
            raw_get_xsave2(fd, xsave_len)
        }
        .unwrap_or_else(|e| panic!("endpoint raw KVM_GET_XSAVE2 failed for {label}: {e}"));
        let mut canonical_endpoint = raw_endpoint.clone();
        let canonical_restore_bv_endpoint =
            canonicalize_xsave_with_restore_bv(&mut canonical_endpoint);
        write_image(&report_dir, "raw-endpoint.bin", &raw_endpoint);
        write_image(&report_dir, "canonical-endpoint.bin", &canonical_endpoint);

        let expected_xmm0 = if active_sse { &ACTIVE_XMM0 } else { &ZERO_XMM0 };
        assert_xmm0_bytes(&raw_before_1, expected_xmm0, "first boundary read", label);
        assert_xmm0_bytes(&raw_before_2, expected_xmm0, "second boundary read", label);
        assert_xmm0_bytes(&raw_after_set, expected_xmm0, "raw SET round trip", label);
        assert_xmm0_bytes(
            &raw_endpoint,
            expected_xmm0,
            "one guest continuation",
            label,
        );
        assert!(
            matches!(endpoint, Exit::Common(CommonExit::Idle)),
            "{label} continuation did not reach HLT: {endpoint:?}"
        );
        assert_eq!(
            backend.exit_counts().total(),
            2,
            "{label} executed more than the boundary and one continuation"
        );
        let xcr0_endpoint = backend_xcr0_value(&backend);

        let (before_bv_1, before_xcomp_1) = header(&raw_before_1);
        let (before_bv_2, before_xcomp_2) = header(&raw_before_2);
        let (after_set_bv, after_set_xcomp) = header(&raw_after_set);
        let (endpoint_bv, endpoint_xcomp) = header(&raw_endpoint);
        let mut metadata = String::new();
        writeln!(
            metadata,
            "phase=stopped-at-mmio-write-exit-before-retirement"
        )
        .expect("metadata write");
        writeln!(metadata, "continuation=retire-mmio-then-one-run-to-hlt").expect("metadata write");
        writeln!(metadata, "mode={label}").expect("metadata write");
        writeln!(metadata, "xsave_len={xsave_len}").expect("metadata write");
        writeln!(metadata, "xcr0_before_run={xcr0_before_run:#x}").expect("metadata write");
        writeln!(metadata, "xcr0_endpoint={xcr0_endpoint:#x}").expect("metadata write");
        writeln!(metadata, "xcr0_source=KVM_GET_XCRS").expect("metadata write");
        writeln!(metadata, "cpuid_source=KVM_GET_CPUID2").expect("metadata write");
        writeln!(metadata, "guest_visible_cpuid1={}", words_hex(cpuid1)).expect("metadata write");
        writeln!(metadata, "guest_visible_cpuid_d0={}", words_hex(cpuid_d0))
            .expect("metadata write");
        writeln!(metadata, "before1_raw_bv={before_bv_1:#x}").expect("metadata write");
        writeln!(metadata, "before1_raw_xcomp_bv={before_xcomp_1:#x}").expect("metadata write");
        writeln!(metadata, "before2_raw_bv={before_bv_2:#x}").expect("metadata write");
        writeln!(metadata, "before2_raw_xcomp_bv={before_xcomp_2:#x}").expect("metadata write");
        writeln!(metadata, "after_set_raw_bv={after_set_bv:#x}").expect("metadata write");
        writeln!(metadata, "after_set_raw_xcomp_bv={after_set_xcomp:#x}").expect("metadata write");
        writeln!(metadata, "endpoint_raw_bv={endpoint_bv:#x}").expect("metadata write");
        writeln!(metadata, "endpoint_raw_xcomp_bv={endpoint_xcomp:#x}").expect("metadata write");
        writeln!(
            metadata,
            "before_reads_equal={}",
            raw_before_1 == raw_before_2
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_before_reads_equal={}",
            canonical_before_1 == canonical_before_2
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_1={canonical_restore_bv_1:?}"
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_2={canonical_restore_bv_2:?}"
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_after_set={canonical_restore_bv_after_set:?}"
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_endpoint={canonical_restore_bv_endpoint:?}"
        )
        .expect("metadata write");
        fs::write(report_dir.join("metadata.txt"), metadata).unwrap_or_else(|e| {
            panic!(
                "write {} failed: {e}",
                report_dir.join("metadata.txt").display()
            )
        });
    }

    #[test]
    #[ignore = "live Linux x86 KVM XSAVE provenance diagnostic; set XSAVE_RAW_REPORT_DIR"]
    fn raw_xsave_presence_phases() {
        assert!(
            Path::new("/dev/kvm").exists(),
            "/dev/kvm is required for this ignored hardware diagnostic"
        );
        let report_root = PathBuf::from(
            std::env::var_os("XSAVE_RAW_REPORT_DIR")
                .expect("XSAVE_RAW_REPORT_DIR must capture complete raw evidence"),
        );
        fs::create_dir_all(&report_root)
            .unwrap_or_else(|e| panic!("create {} failed: {e}", report_root.display()));
        run_variant(&report_root, true);
        run_variant(&report_root, false);
    }
}
