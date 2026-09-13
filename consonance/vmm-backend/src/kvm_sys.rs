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

#[cfg(test)]
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
    const GUEST_XSAVE_GPA: usize = 0x2000;
    const GUEST_XSAVE_PAGE_LEN: usize = 0x1000;
    const GUEST_XSAVE_LEN: usize = 0x340;
    const X87_FCW: std::ops::Range<usize> = 0..2;
    const X87_FSW: std::ops::Range<usize> = 2..4;
    const X87_FTW: std::ops::Range<usize> = 4..5;
    const X87_ST: std::ops::Range<usize> = 32..160;
    const X87_ST_SEED: [u8; 10] = [0xA7; 10];
    const X87_ST_ZERO: [u8; 10] = [0; 10];
    const _: () = assert!(GUEST_XSAVE_GPA + GUEST_XSAVE_PAGE_LEN <= RAM_LEN);
    const ACTIVE_XMM0: [u8; 16] = [
        0xA5, 0x5A, 0x3C, 0xC3, 0x96, 0x69, 0x78, 0x87, 0x12, 0x21, 0x34, 0x43, 0x56, 0x65, 0xAB,
        0xBA,
    ];
    const ZERO_XMM0: [u8; 16] = [0; 16];

    #[derive(Clone, Copy)]
    enum BoundaryObservation {
        LegacyGetSetGet,
        Unobserved,
        GetOnly,
        GetSetGet,
    }

    impl BoundaryObservation {
        fn name(self) -> &'static str {
            match self {
                Self::LegacyGetSetGet => "get-set-get",
                Self::Unobserved => "unobserved",
                Self::GetOnly => "get-only",
                Self::GetSetGet => "get-set-get",
            }
        }

        fn guest_program(self) -> bool {
            !matches!(self, Self::LegacyGetSetGet)
        }
    }

    #[derive(Clone, Copy)]
    enum GuestX87Cohort {
        PayloadPreservation,
        ZeroState,
    }

    impl GuestX87Cohort {
        fn name(self) -> &'static str {
            match self {
                Self::PayloadPreservation => "payload-preservation",
                Self::ZeroState => "zero-state",
            }
        }

        fn st_seed(self) -> &'static [u8; 10] {
            match self {
                Self::PayloadPreservation => &X87_ST_SEED,
                Self::ZeroState => &X87_ST_ZERO,
            }
        }

        fn st_seed_description(self) -> &'static str {
            match self {
                Self::PayloadPreservation => "8-slots-of-10-bytes-0xa7-with-6-zero-padding-bytes",
                Self::ZeroState => "8-slots-of-10-zero-bytes-with-6-zero-padding-bytes",
            }
        }

        fn phase_label(self, observation: BoundaryObservation) -> &'static str {
            match (self, observation) {
                (Self::PayloadPreservation, BoundaryObservation::Unobserved) => {
                    "guest-fninit-unobserved"
                }
                (Self::PayloadPreservation, BoundaryObservation::GetOnly) => {
                    "guest-fninit-get-only"
                }
                (Self::PayloadPreservation, BoundaryObservation::GetSetGet) => {
                    "guest-fninit-get-set-get"
                }
                (Self::ZeroState, BoundaryObservation::Unobserved) => {
                    "guest-fninit-zero-unobserved"
                }
                (Self::ZeroState, BoundaryObservation::GetOnly) => "guest-fninit-zero-get-only",
                (Self::ZeroState, BoundaryObservation::GetSetGet) => {
                    "guest-fninit-zero-get-set-get"
                }
                (_, BoundaryObservation::LegacyGetSetGet) => {
                    panic!("legacy observation has no guest x87 cohort")
                }
            }
        }

        fn comparison_file(self) -> &'static str {
            match self {
                Self::PayloadPreservation => "guest-fninit-comparison.txt",
                Self::ZeroState => "guest-fninit-zero-comparison.txt",
            }
        }
    }

    struct GuestPhaseResult {
        raw_endpoint: Vec<u8>,
        canonical_endpoint: Vec<u8>,
        endpoint_bv: u64,
        endpoint_xcomp_bv: u64,
        endpoint_restore_bv: Option<u64>,
        guest_xsave: Vec<u8>,
        canonical_guest_xsave: Vec<u8>,
        guest_bv: u64,
        guest_xcomp_bv: u64,
        guest_restore_bv: Option<u64>,
    }

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

    fn guest_fninit_xsave_program() -> Vec<u8> {
        let mmio = mmio_program();
        let mut program = Vec::with_capacity(33);
        program.extend_from_slice(&[0xDB, 0xE3]);
        program.extend_from_slice(&mmio[..mmio.len() - 1]);
        program.extend_from_slice(&[
            0x66,
            0xB8,
            0x03,
            0x00,
            0x00,
            0x00,
            0x66,
            0x31,
            0xD2,
            0x67,
            0x0F,
            0xAE,
            0x25,
            GUEST_XSAVE_GPA as u8,
            (GUEST_XSAVE_GPA >> 8) as u8,
            (GUEST_XSAVE_GPA >> 16) as u8,
            (GUEST_XSAVE_GPA >> 24) as u8,
            0xF4,
        ]);
        program
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

    fn fail_with_buffered_error(
        report_dir: &Path,
        label: &str,
        buffered_images: &[(&str, Vec<u8>)],
        phase: &str,
        error: impl std::fmt::Display,
    ) -> ! {
        for (name, image) in buffered_images {
            write_image(report_dir, name, image);
        }
        let failure = format!("mode={label}\nphase={phase}\nerror={error}\n");
        fs::write(report_dir.join("failure.txt"), failure).unwrap_or_else(|e| {
            panic!(
                "write {} failed while retaining {phase}: {e}",
                report_dir.join("failure.txt").display()
            )
        });
        panic!("{label} {phase} failed: {error}");
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

    fn range_equal(a: &[u8], b: &[u8], range: std::ops::Range<usize>) -> bool {
        a[range.clone()] == b[range]
    }

    fn x87_equal(a: &[u8], b: &[u8]) -> bool {
        range_equal(a, b, 0..24) && range_equal(a, b, 32..160)
    }

    fn restore_bv_text(value: Option<Option<u64>>) -> String {
        value.map_or_else(|| "not-observed".to_owned(), |value| format!("{value:?}"))
    }

    fn append_pairwise<F>(
        output: &mut String,
        key: &str,
        unobserved: &GuestPhaseResult,
        get_only: &GuestPhaseResult,
        get_set_get: &GuestPhaseResult,
        equal: F,
    ) where
        F: Fn(&GuestPhaseResult, &GuestPhaseResult) -> bool,
    {
        writeln!(
            output,
            "unobserved_vs_get_only_{key}={}",
            equal(unobserved, get_only)
        )
        .expect("comparison write");
        writeln!(
            output,
            "unobserved_vs_get_set_get_{key}={}",
            equal(unobserved, get_set_get)
        )
        .expect("comparison write");
        writeln!(
            output,
            "get_only_vs_get_set_get_{key}={}",
            equal(get_only, get_set_get)
        )
        .expect("comparison write");
    }

    fn run_variant(report_root: &Path, active_sse: bool) {
        let label = if active_sse {
            "active-initialized-sse"
        } else {
            "zero-sse-control"
        };
        run_variant_with_observation(
            report_root,
            active_sse,
            BoundaryObservation::LegacyGetSetGet,
            None,
            label,
        );
    }

    fn run_guest_phase(
        report_root: &Path,
        cohort: GuestX87Cohort,
        observation: BoundaryObservation,
    ) -> GuestPhaseResult {
        let label = cohort.phase_label(observation);
        run_variant_with_observation(report_root, true, observation, Some(cohort), label)
            .expect("guest phase must return a result")
    }

    fn run_variant_with_observation(
        report_root: &Path,
        active_sse: bool,
        observation: BoundaryObservation,
        cohort: Option<GuestX87Cohort>,
        label: &'static str,
    ) -> Option<GuestPhaseResult> {
        let report_dir = report_root.join(label);
        if observation.guest_program() {
            fs::create_dir(&report_dir)
                .unwrap_or_else(|e| panic!("create fresh {} failed: {e}", report_dir.display()));
        } else {
            fs::create_dir_all(&report_dir)
                .unwrap_or_else(|e| panic!("create {} failed: {e}", report_dir.display()));
        }

        let mut ram =
            MmapRam::new(RAM_LEN).unwrap_or_else(|e| panic!("guest RAM mmap failed: {e}"));
        let program = if observation.guest_program() {
            guest_fninit_xsave_program()
        } else {
            mmio_program().to_vec()
        };
        ram.as_mut_bytes()[CODE_GPA..CODE_GPA + program.len()].copy_from_slice(&program);
        if observation.guest_program() {
            ram.as_mut_bytes()[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_PAGE_LEN].fill(0);
        }

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
        state.xsave_restore_bv = if observation.guest_program() {
            Some(3)
        } else {
            None
        };
        if observation.guest_program() {
            assert!(cohort.is_some(), "guest phase requires an x87 cohort");
        } else {
            assert!(
                cohort.is_none(),
                "MMIO-only phase cannot have an x87 cohort"
            );
        }

        if observation.guest_program() {
            state.xsave[0..24].fill(0);
            state.xsave[X87_ST].fill(0);
            state.xsave[X87_FCW].copy_from_slice(&[0x7f, 0x03]);
            state.xsave[X87_FSW].fill(0);
            state.xsave[X87_FTW].copy_from_slice(&[0xff]);
            let st_seed = cohort.expect("guest phase cohort checked above").st_seed();
            for slot in state.xsave[X87_ST].chunks_exact_mut(16) {
                slot[..st_seed.len()].copy_from_slice(st_seed);
            }
        }
        state.xsave[SSE_XMM0].fill(0);
        state.xsave[XCOMP_BV].fill(0);
        state.xsave[XSTATE_BV].copy_from_slice(&if active_sse {
            if observation.guest_program() {
                3u64.to_le_bytes()
            } else {
                2u64.to_le_bytes()
            }
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
        assert_eq!(
            cpuid_d0[2] as usize, GUEST_XSAVE_LEN,
            "diagnostic guest CPUID XSAVE size changed"
        );
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
        let mut buffered_images: Vec<(&str, Vec<u8>)> = Vec::new();
        let mut raw_before_1 = None;
        let mut raw_before_2 = None;
        let mut canonical_before_1 = None;
        let mut canonical_before_2 = None;
        let mut canonical_restore_bv_1 = None;
        let mut canonical_restore_bv_2 = None;
        let mut raw_after_set = None;
        let mut canonical_restore_bv_after_set = None;
        if !matches!(observation, BoundaryObservation::Unobserved) {
            let raw = unsafe {
                // SAFETY: the owned vCPU is stopped at the checked MMIO boundary and the capability
                // sized buffer is retained until KVM finishes the direct ioctl.
                raw_get_xsave2(fd, xsave_len)
            };
            let raw = match raw {
                Ok(raw) => raw,
                Err(error) => fail_with_buffered_error(
                    &report_dir,
                    label,
                    &buffered_images,
                    "first raw KVM_GET_XSAVE2",
                    error,
                ),
            };
            let mut canonical = raw.clone();
            let restore_bv = canonicalize_xsave_with_restore_bv(&mut canonical);
            buffered_images.push(("raw-before-1.bin", raw.clone()));
            buffered_images.push(("canonical-before-1.bin", canonical.clone()));
            raw_before_1 = Some(raw);
            canonical_before_1 = Some(canonical);
            canonical_restore_bv_1 = Some(restore_bv);

            let raw = unsafe {
                // SAFETY: the vCPU remains stopped and the second independent buffer has the same
                // kernel-reported capability size as the first direct read.
                raw_get_xsave2(fd, xsave_len)
            };
            let raw = match raw {
                Ok(raw) => raw,
                Err(error) => fail_with_buffered_error(
                    &report_dir,
                    label,
                    &buffered_images,
                    "second raw KVM_GET_XSAVE2",
                    error,
                ),
            };
            let mut canonical = raw.clone();
            let restore_bv = canonicalize_xsave_with_restore_bv(&mut canonical);
            buffered_images.push(("raw-before-2.bin", raw.clone()));
            buffered_images.push(("canonical-before-2.bin", canonical.clone()));
            raw_before_2 = Some(raw);
            canonical_before_2 = Some(canonical);
            canonical_restore_bv_2 = Some(restore_bv);
        }

        if matches!(
            observation,
            BoundaryObservation::LegacyGetSetGet | BoundaryObservation::GetSetGet
        ) {
            let before = raw_before_1
                .as_ref()
                .expect("SET phase requires a preceding XSAVE GET");
            unsafe {
                // SAFETY: the stopped vCPU owns the direct KVM XSAVE target and `before` has the
                // complete capability-sized image returned by KVM_GET_XSAVE2.
                raw_set_xsave(fd, before)
            }
            .unwrap_or_else(|error| {
                fail_with_buffered_error(
                    &report_dir,
                    label,
                    &buffered_images,
                    "raw KVM_SET_XSAVE",
                    error,
                )
            });
            let raw = unsafe {
                // SAFETY: the vCPU is still stopped after KVM_SET_XSAVE and the output buffer is
                // capability-sized and exclusively owned by this test.
                raw_get_xsave2(fd, xsave_len)
            };
            let raw = match raw {
                Ok(raw) => raw,
                Err(error) => fail_with_buffered_error(
                    &report_dir,
                    label,
                    &buffered_images,
                    "raw post-SET KVM_GET_XSAVE2",
                    error,
                ),
            };
            let mut canonical = raw.clone();
            let restore_bv = canonicalize_xsave_with_restore_bv(&mut canonical);
            buffered_images.push(("raw-after-set.bin", raw.clone()));
            buffered_images.push(("canonical-after-set.bin", canonical.clone()));
            raw_after_set = Some(raw);
            canonical_restore_bv_after_set = Some(restore_bv);
        }

        if let Err(error) = backend.retire_pending_completion() {
            fail_with_buffered_error(
                &report_dir,
                label,
                &buffered_images,
                "retire MMIO write completion",
                error,
            );
        }

        let endpoint = match backend.run() {
            Ok(endpoint) => endpoint,
            Err(error) => fail_with_buffered_error(
                &report_dir,
                label,
                &buffered_images,
                "bounded continuation run",
                error,
            ),
        };
        let guest_xsave = observation.guest_program().then(|| {
            ram.as_mut_bytes()[GUEST_XSAVE_GPA..GUEST_XSAVE_GPA + GUEST_XSAVE_LEN].to_vec()
        });
        let mut canonical_guest_xsave = None;
        let guest_restore_bv = if let Some(image) = guest_xsave.as_ref() {
            let mut canonical = image.clone();
            let restore_bv = canonicalize_xsave_with_restore_bv(&mut canonical);
            canonical_guest_xsave = Some(canonical);
            restore_bv
        } else {
            None
        };
        if let Some(image) = guest_xsave.as_ref() {
            buffered_images.push(("guest-xsave.bin", image.clone()));
        }
        if let Some(image) = canonical_guest_xsave.as_ref() {
            buffered_images.push(("canonical-guest-xsave.bin", image.clone()));
        }
        let raw_endpoint = unsafe {
            // SAFETY: the vCPU is stopped after the bounded continuation and the output buffer
            // is capability-sized.
            raw_get_xsave2(fd, xsave_len)
        };
        let raw_endpoint = match raw_endpoint {
            Ok(raw_endpoint) => raw_endpoint,
            Err(error) => fail_with_buffered_error(
                &report_dir,
                label,
                &buffered_images,
                "endpoint raw KVM_GET_XSAVE2",
                error,
            ),
        };
        let mut canonical_endpoint = raw_endpoint.clone();
        let canonical_restore_bv_endpoint =
            canonicalize_xsave_with_restore_bv(&mut canonical_endpoint);
        for (name, image) in buffered_images {
            write_image(&report_dir, name, &image);
        }
        write_image(&report_dir, "raw-endpoint.bin", &raw_endpoint);
        write_image(&report_dir, "canonical-endpoint.bin", &canonical_endpoint);

        let xcr0_endpoint = backend_xcr0_value(&backend);
        let before_1_header = raw_before_1.as_ref().map(|image| header(image));
        let before_2_header = raw_before_2.as_ref().map(|image| header(image));
        let after_set_header = raw_after_set.as_ref().map(|image| header(image));
        let (endpoint_bv, endpoint_xcomp) = header(&raw_endpoint);
        let guest_header = guest_xsave.as_ref().map(|image| header(image));
        let mut metadata = String::new();
        writeln!(
            metadata,
            "phase=stopped-at-mmio-write-exit-before-retirement"
        )
        .expect("metadata write");
        writeln!(metadata, "continuation=retire-mmio-then-one-run-to-hlt").expect("metadata write");
        writeln!(metadata, "mode={label}").expect("metadata write");
        writeln!(metadata, "boundary_observation={}", observation.name()).expect("metadata write");
        writeln!(
            metadata,
            "guest_program={}",
            if observation.guest_program() {
                "fninit-before-mmio-xsave-after-retirement"
            } else {
                "mmio-only"
            }
        )
        .expect("metadata write");
        if observation.guest_program() {
            let cohort = cohort.expect("guest phase cohort checked above");
            writeln!(metadata, "pre_guest_xsave_restore_bv=Some(3)").expect("metadata write");
            writeln!(metadata, "pre_guest_xsave_raw_bv=0x3").expect("metadata write");
            writeln!(metadata, "pre_guest_xsave_raw_xcomp_bv=0x0").expect("metadata write");
            writeln!(metadata, "pre_guest_x87_cohort={}", cohort.name()).expect("metadata write");
            writeln!(metadata, "pre_guest_x87_fcw=[7f, 03]").expect("metadata write");
            writeln!(metadata, "pre_guest_x87_fsw=[00, 00]").expect("metadata write");
            writeln!(metadata, "pre_guest_x87_ftw=[ff]").expect("metadata write");
            writeln!(
                metadata,
                "pre_guest_x87_st_seed={}",
                cohort.st_seed_description()
            )
            .expect("metadata write");
            writeln!(
                metadata,
                "pre_guest_x87_st_seed_bytes={:02x?}",
                cohort.st_seed()
            )
            .expect("metadata write");
            writeln!(
                metadata,
                "pre_guest_x87_st_seed_len={}",
                cohort.st_seed().len()
            )
            .expect("metadata write");
        }
        writeln!(
            metadata,
            "boundary_io_until_continuation={}",
            match observation {
                BoundaryObservation::LegacyGetSetGet => "raw-get-get-set-get",
                BoundaryObservation::Unobserved => "none",
                BoundaryObservation::GetOnly => "raw-get-get",
                BoundaryObservation::GetSetGet => "raw-get-get-set-get",
            }
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "continuation_guest_sequence={}",
            if observation.guest_program() {
                "integer-setup,xsave,halt"
            } else {
                "halt"
            }
        )
        .expect("metadata write");
        writeln!(metadata, "xsave_len={xsave_len}").expect("metadata write");
        writeln!(metadata, "xcr0_before_run={xcr0_before_run:#x}").expect("metadata write");
        writeln!(metadata, "xcr0_endpoint={xcr0_endpoint:#x}").expect("metadata write");
        writeln!(metadata, "xcr0_source=KVM_GET_XCRS").expect("metadata write");
        writeln!(metadata, "cpuid_source=KVM_GET_CPUID2").expect("metadata write");
        writeln!(metadata, "guest_visible_cpuid1={}", words_hex(cpuid1)).expect("metadata write");
        writeln!(metadata, "guest_visible_cpuid_d0={}", words_hex(cpuid_d0))
            .expect("metadata write");
        writeln!(
            metadata,
            "before1_raw_bv={}",
            before_1_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.0)
            )
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "before1_raw_xcomp_bv={}",
            before_1_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.1)
            )
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "before2_raw_bv={}",
            before_2_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.0)
            )
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "before2_raw_xcomp_bv={}",
            before_2_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.1)
            )
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "after_set_raw_bv={}",
            after_set_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.0)
            )
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "after_set_raw_xcomp_bv={}",
            after_set_header.map_or_else(
                || "not-observed".to_owned(),
                |header| format!("{:#x}", header.1)
            )
        )
        .expect("metadata write");
        writeln!(metadata, "endpoint_raw_bv={endpoint_bv:#x}").expect("metadata write");
        writeln!(metadata, "endpoint_raw_xcomp_bv={endpoint_xcomp:#x}").expect("metadata write");
        writeln!(
            metadata,
            "before_reads_equal={}",
            raw_before_1
                .as_ref()
                .zip(raw_before_2.as_ref())
                .map(|(first, second)| first == second)
                .map_or("not-observed", |equal| if equal { "true" } else { "false" })
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_before_reads_equal={}",
            canonical_before_1
                .as_ref()
                .zip(canonical_before_2.as_ref())
                .map(|(first, second)| first == second)
                .map_or("not-observed", |equal| if equal { "true" } else { "false" })
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_1={}",
            restore_bv_text(canonical_restore_bv_1)
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_2={}",
            restore_bv_text(canonical_restore_bv_2)
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_after_set={}",
            restore_bv_text(canonical_restore_bv_after_set)
        )
        .expect("metadata write");
        writeln!(
            metadata,
            "canonical_restore_bv_endpoint={canonical_restore_bv_endpoint:?}"
        )
        .expect("metadata write");
        if let Some(image) = guest_xsave.as_ref() {
            let cohort = cohort.expect("guest phase cohort checked above");
            let (guest_bv, guest_xcomp) = header(image);
            writeln!(metadata, "guest_xsave_bv={guest_bv:#x}").expect("metadata write");
            writeln!(metadata, "guest_xsave_xcomp_bv={guest_xcomp:#x}").expect("metadata write");
            writeln!(metadata, "guest_xsave_restore_bv={guest_restore_bv:?}")
                .expect("metadata write");
            writeln!(metadata, "guest_xsave_x87_fcw={:02x?}", &image[X87_FCW])
                .expect("metadata write");
            writeln!(metadata, "guest_xsave_x87_fsw={:02x?}", &image[X87_FSW])
                .expect("metadata write");
            writeln!(
                metadata,
                "guest_xsave_x87_ftw_empty={}",
                image[X87_FTW].iter().all(|&byte| byte == 0)
            )
            .expect("metadata write");
            writeln!(
                metadata,
                "guest_xsave_x87_payload_matches_seed={}",
                image[X87_ST]
                    .chunks_exact(16)
                    .all(|slot| slot[..10] == cohort.st_seed()[..])
            )
            .expect("metadata write");
            writeln!(
                metadata,
                "guest_xsave_x87_st_seed_bytes={:02x?}",
                cohort.st_seed()
            )
            .expect("metadata write");
            writeln!(metadata, "guest_xsave_x87_st_payload_len={}", X87_ST.len())
                .expect("metadata write");
            writeln!(metadata, "guest_xsave_xmm0={:02x?}", &image[SSE_XMM0])
                .expect("metadata write");
        }
        writeln!(
            metadata,
            "guest_xsave_source={}",
            if observation.guest_program() {
                "guest XSAVE immediately after MMIO retirement"
            } else {
                "none"
            }
        )
        .expect("metadata write");
        fs::write(report_dir.join("metadata.txt"), metadata).unwrap_or_else(|e| {
            panic!(
                "write {} failed: {e}",
                report_dir.join("metadata.txt").display()
            )
        });

        let expected_xmm0 = if active_sse { &ACTIVE_XMM0 } else { &ZERO_XMM0 };
        if let Some(raw) = raw_before_1.as_ref() {
            assert_xmm0_bytes(raw, expected_xmm0, "first boundary read", label);
        }
        if let Some(raw) = raw_before_2.as_ref() {
            assert_xmm0_bytes(raw, expected_xmm0, "second boundary read", label);
        }
        if let Some(raw) = raw_after_set.as_ref() {
            assert_xmm0_bytes(raw, expected_xmm0, "raw SET round trip", label);
        }
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
        if let Some(image) = guest_xsave.as_ref() {
            assert_eq!(
                &image[X87_FCW],
                &[0x7f, 0x03],
                "{label} guest XSAVE did not retain FNINIT's control word"
            );
            assert!(
                image[X87_FSW].iter().all(|&byte| byte == 0),
                "{label} guest XSAVE did not retain FNINIT's status word"
            );
            assert!(
                image[X87_FTW].iter().all(|&byte| byte == 0),
                "{label} guest XSAVE did not retain FNINIT's empty x87 tag word"
            );
            assert_xmm0_bytes(image, expected_xmm0, "guest XSAVE", label);
        }

        if observation.guest_program() {
            Some(GuestPhaseResult {
                raw_endpoint,
                canonical_endpoint,
                endpoint_bv,
                endpoint_xcomp_bv: endpoint_xcomp,
                endpoint_restore_bv: canonical_restore_bv_endpoint,
                guest_xsave: guest_xsave.expect("guest program must capture guest XSAVE"),
                canonical_guest_xsave: canonical_guest_xsave
                    .expect("guest program must canonicalize guest XSAVE"),
                guest_bv: guest_header
                    .expect("guest program must capture guest XSAVE header")
                    .0,
                guest_xcomp_bv: guest_header
                    .expect("guest program must capture guest XSAVE header")
                    .1,
                guest_restore_bv,
            })
        } else {
            None
        }
    }

    fn run_guest_cohort(report_root: &Path, cohort: GuestX87Cohort) {
        let unobserved = run_guest_phase(report_root, cohort, BoundaryObservation::Unobserved);
        let get_only = run_guest_phase(report_root, cohort, BoundaryObservation::GetOnly);
        let get_set_get = run_guest_phase(report_root, cohort, BoundaryObservation::GetSetGet);
        let mut comparison = String::new();
        writeln!(comparison, "guest_x87_cohort={}", cohort.name()).expect("comparison write");
        writeln!(
            comparison,
            "guest_program=fninit-before-mmio-xsave-after-retirement"
        )
        .expect("comparison write");
        writeln!(comparison, "phases=unobserved,get-only,get-set-get").expect("comparison write");
        append_pairwise(
            &mut comparison,
            "guest_xsave_raw_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.guest_xsave == second.guest_xsave,
        );
        append_pairwise(
            &mut comparison,
            "guest_xsave_canonical_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.canonical_guest_xsave == second.canonical_guest_xsave,
        );
        append_pairwise(
            &mut comparison,
            "guest_x87_raw_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| x87_equal(&first.guest_xsave, &second.guest_xsave),
        );
        append_pairwise(
            &mut comparison,
            "guest_x87_canonical_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| x87_equal(&first.canonical_guest_xsave, &second.canonical_guest_xsave),
        );
        append_pairwise(
            &mut comparison,
            "endpoint_raw_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.raw_endpoint == second.raw_endpoint,
        );
        append_pairwise(
            &mut comparison,
            "endpoint_canonical_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.canonical_endpoint == second.canonical_endpoint,
        );
        append_pairwise(
            &mut comparison,
            "guest_restore_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.guest_restore_bv == second.guest_restore_bv,
        );
        append_pairwise(
            &mut comparison,
            "endpoint_restore_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.endpoint_restore_bv == second.endpoint_restore_bv,
        );
        append_pairwise(
            &mut comparison,
            "guest_raw_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.guest_bv == second.guest_bv,
        );
        append_pairwise(
            &mut comparison,
            "guest_raw_xcomp_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.guest_xcomp_bv == second.guest_xcomp_bv,
        );
        append_pairwise(
            &mut comparison,
            "endpoint_raw_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.endpoint_bv == second.endpoint_bv,
        );
        append_pairwise(
            &mut comparison,
            "endpoint_raw_xcomp_bv_equal",
            &unobserved,
            &get_only,
            &get_set_get,
            |first, second| first.endpoint_xcomp_bv == second.endpoint_xcomp_bv,
        );
        let comparison_path = report_root.join(cohort.comparison_file());
        fs::write(&comparison_path, comparison)
            .unwrap_or_else(|e| panic!("write {} failed: {e}", comparison_path.display()));
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
        run_guest_cohort(&report_root, GuestX87Cohort::PayloadPreservation);
        run_guest_cohort(&report_root, GuestX87Cohort::ZeroState);
    }

    const PAE_RAM_LEN: usize = 4 * 1024 * 1024;
    const PAE_PAGE_SIZE: usize = 4096;
    const PAE_CODE_GPA: usize = 0x1000;
    const PAE_DATA_GPA: usize = 0x8000;
    const PAE_GDT_GPA: usize = 0x7000;
    const PAE_PDPT_GPA: usize = 0x4000;
    const PAE_PD_A_GPA: usize = 0x5000;
    const PAE_PD_B_GPA: usize = 0x6000;
    const PAE_REMAPPED_CODE_GPA: usize = 0x201000;
    const PAE_REMAPPED_DATA_GPA: usize = 0x208000;
    const PAE_PDPT_A_ENTRY: u64 = 0x5001;
    const PAE_PDPT_B_ENTRY: u64 = 0x6001;
    const PAE_SREGS2_FLAGS_PDPTRS_VALID: u64 = 1;
    const PAE_IA32_EFER: u32 = 0xC000_0080;
    const PAE_WARMUP_MARKER: u8 = 0xA5;
    const PAE_GUEST_PDPT_WRITE_LEN: usize = 10;
    const PAE_BASE_PROGRAM_LEN: usize = 21;
    const PAE_GUEST_PDPT_WRITE: [u8; PAE_GUEST_PDPT_WRITE_LEN] =
        [0xC7, 0x05, 0x00, 0x40, 0x00, 0x00, 0x01, 0x60, 0x00, 0x00];

    #[derive(Clone, Copy, Debug)]
    enum PaePhase {
        NoReadAtB,
        OneGetAtB,
        GetThenSetPrewriteA,
    }

    impl PaePhase {
        const fn name(self) -> &'static str {
            match self {
                Self::NoReadAtB => "no-read-at-b",
                Self::OneGetAtB => "one-get-at-b",
                Self::GetThenSetPrewriteA => "get-then-set-prewrite-a",
            }
        }

        const fn reads_at_b(self) -> u64 {
            match self {
                Self::NoReadAtB => 0,
                Self::OneGetAtB | Self::GetThenSetPrewriteA => 1,
            }
        }

        const fn sets_at_b(self) -> u64 {
            match self {
                Self::GetThenSetPrewriteA => 1,
                Self::NoReadAtB | Self::OneGetAtB => 0,
            }
        }
    }

    struct PaeObservation {
        phase: PaePhase,
        prewrite_a: kvm_sregs2,
        at_b: Option<kvm_sregs2>,
        endpoint: kvm_sregs2,
        pdpt_b: Vec<u8>,
        endpoint_byte: u8,
        endpoint_rip: u64,
        boundary_exit: &'static str,
        continuation_exit: &'static str,
        endpoint_exit: &'static str,
    }

    fn pae_policy() -> X86Policy {
        let mut policy = diagnostic_policy();
        policy.msr_filter.allow_inkernel.push(MsrRange {
            base: PAE_IA32_EFER,
            count: 1,
        });
        policy
    }

    fn pae_put_u64(bytes: &mut [u8], gpa: usize, value: u64) {
        bytes[gpa..gpa + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn pae_put_bytes(bytes: &mut [u8], gpa: usize, value: &[u8]) {
        bytes[gpa..gpa + value.len()].copy_from_slice(value);
    }

    fn pae_guest_ram() -> MmapRam {
        let mut ram =
            MmapRam::new(PAE_RAM_LEN).unwrap_or_else(|e| panic!("PAE RAM mmap failed: {e}"));
        let bytes = ram.as_mut_bytes();
        let program = [
            0xBA,
            0xF8,
            0x03,
            0x00,
            0x00,
            0xB0,
            PAE_WARMUP_MARKER,
            0xEE,
            0xA0,
            0x00,
            0x80,
            0x00,
            0x00,
            0xBA,
            0xF8,
            0x03,
            0x00,
            0x00,
            0xEE,
            0x43,
            0xF4,
        ];
        pae_put_bytes(bytes, PAE_CODE_GPA, &program);
        pae_put_bytes(bytes, PAE_REMAPPED_CODE_GPA, &program);
        bytes.copy_within(
            PAE_CODE_GPA..PAE_CODE_GPA + PAE_BASE_PROGRAM_LEN,
            PAE_CODE_GPA + PAE_GUEST_PDPT_WRITE_LEN,
        );
        bytes.copy_within(
            PAE_REMAPPED_CODE_GPA..PAE_REMAPPED_CODE_GPA + PAE_BASE_PROGRAM_LEN,
            PAE_REMAPPED_CODE_GPA + PAE_GUEST_PDPT_WRITE_LEN,
        );
        pae_put_bytes(bytes, PAE_CODE_GPA, &PAE_GUEST_PDPT_WRITE);
        pae_put_bytes(bytes, PAE_REMAPPED_CODE_GPA, &PAE_GUEST_PDPT_WRITE);
        bytes[PAE_DATA_GPA] = 0x42;
        bytes[PAE_REMAPPED_DATA_GPA] = 0x99;
        pae_put_u64(bytes, PAE_PDPT_GPA, PAE_PDPT_A_ENTRY);
        pae_put_u64(bytes, PAE_PD_A_GPA, 0x83);
        pae_put_u64(bytes, PAE_PD_B_GPA, 0x20_00_83);
        pae_put_u64(bytes, PAE_GDT_GPA, 0);
        pae_put_u64(bytes, PAE_GDT_GPA + 8, 0x00CF_9B00_0000_FFFF);
        pae_put_u64(bytes, PAE_GDT_GPA + 16, 0x00CF_9300_0000_FFFF);
        ram
    }

    fn pae_code_segment() -> crate::arch::x86::Segment {
        crate::arch::x86::Segment {
            base: 0,
            limit: u32::MAX,
            selector: 0x8,
            type_: 0xB,
            present: 1,
            dpl: 0,
            db: 1,
            s: 1,
            l: 0,
            g: 1,
            avl: 0,
            unusable: 0,
        }
    }

    fn pae_data_segment() -> crate::arch::x86::Segment {
        crate::arch::x86::Segment {
            base: 0,
            limit: u32::MAX,
            selector: 0x10,
            type_: 0x3,
            present: 1,
            dpl: 0,
            db: 1,
            s: 1,
            l: 0,
            g: 1,
            avl: 0,
            unusable: 0,
        }
    }

    fn pae_backend() -> (MmapRam, KvmBackend, kvm_sregs2) {
        let mut ram = pae_guest_ram();
        let mut backend = KvmBackend::new().unwrap_or_else(|e| panic!("PAE KVM setup failed: {e}"));
        unsafe {
            // SAFETY: `ram` is page-aligned, pinned for this scope, and outlives `backend`.
            backend
                .map_memory(Gpa(0), ram.as_mut_bytes())
                .unwrap_or_else(|e| panic!("PAE map_memory failed: {e}"));
        }
        backend
            .set_policy(&pae_policy())
            .unwrap_or_else(|e| panic!("PAE set_policy failed: {e}"));
        let mut state = backend
            .save()
            .unwrap_or_else(|e| panic!("PAE entry save failed: {e}"));
        let data = pae_data_segment();
        state.regs.rip = PAE_CODE_GPA as u64;
        state.regs.rsp = (PAE_RAM_LEN - PAE_PAGE_SIZE) as u64;
        state.regs.rbx = 0;
        state.regs.rflags = 0x2;
        state.sregs.cs = pae_code_segment();
        state.sregs.ds = data;
        state.sregs.es = data;
        state.sregs.fs = data;
        state.sregs.gs = data;
        state.sregs.ss = data;
        state.sregs.gdt = crate::arch::x86::DescriptorTable {
            base: PAE_GDT_GPA as u64,
            limit: 0x17,
        };
        state.sregs.cr0 = 0x8000_0011;
        state.sregs.cr2 = 0;
        state.sregs.cr3 = PAE_PDPT_GPA as u64;
        state.sregs.cr4 = 0x30;
        state.sregs.efer = 0;
        state.sregs.flags = PAE_SREGS2_FLAGS_PDPTRS_VALID;
        state.sregs.pdptrs = [PAE_PDPT_A_ENTRY, 0, 0, 0];
        state.msrs.insert(PAE_IA32_EFER, 0);
        state.mp_state = MpState::Runnable;
        backend
            .restore(&state)
            .unwrap_or_else(|e| panic!("PAE entry restore failed: {e}"));
        let prewrite_a = unsafe {
            // SAFETY: the vCPU is stopped before the first guest instruction and the ioctl writes
            // a complete kernel-sized `kvm_sregs2` into the returned value.
            raw_get_sregs2(backend.vcpu.as_raw_fd())
        }
        .unwrap_or_else(|e| panic!("PAE pre-write A KVM_GET_SREGS2 failed: {e}"));
        (ram, backend, prewrite_a)
    }

    fn pae_exit_label(exit: &Exit<X86>, phase: &str) -> &'static str {
        match exit {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(_),
            }) => "KVM_EXIT_IO",
            Exit::Common(CommonExit::Idle) => "KVM_EXIT_HLT",
            other => panic!("{phase} returned unexpected KVM_RUN exit: {other:?}"),
        }
    }

    fn pae_expect_output(exit: &Exit<X86>, expected: u8, phase: &str) {
        match exit {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(value),
            }) => assert_eq!(*value, u32::from(expected), "{phase} UART byte"),
            other => panic!("{phase} returned unexpected KVM_RUN exit: {other:?}"),
        }
    }

    fn pae_sregs2_bytes(value: &kvm_sregs2) -> Vec<u8> {
        assert_eq!(
            size_of::<kvm_sregs2>(),
            320,
            "kvm_sregs2 ABI size must match the byte evidence contract"
        );
        // SAFETY: `kvm_sregs2` is the repr(C) 320-byte kernel ABI value; it has no implicit
        // padding, and its explicit segment/dtable padding fields are initialized by Default and
        // retained by raw KVM_GET_SREGS2 before all bytes are copied here.
        unsafe {
            std::slice::from_raw_parts(
                (value as *const kvm_sregs2).cast::<u8>(),
                size_of::<kvm_sregs2>(),
            )
            .to_vec()
        }
    }

    #[test]
    fn pae_sregs2_bytes_preserves_initialized_abi_fields() {
        let mut value = kvm_sregs2::default();
        value.cs.base = 0x0123_4567_89AB_CDEF;
        value.cs.padding = 0xA7;
        value.gdt.base = 0xFEDC_BA98_7654_3210;
        value.gdt.limit = 0x1357;
        value.gdt.padding = [0x2468, 0x369A, 0x48AC];
        value.flags = PAE_SREGS2_FLAGS_PDPTRS_VALID;
        value.pdptrs = [PAE_PDPT_A_ENTRY, 0x1111, 0x2222, 0x3333];
        let bytes = pae_sregs2_bytes(&value);
        assert_eq!(bytes.len(), 320);
        assert_eq!(&bytes[0..8], &value.cs.base.to_ne_bytes());
        assert_eq!(bytes[23], value.cs.padding);
        assert_eq!(&bytes[192..200], &value.gdt.base.to_ne_bytes());
        assert_eq!(&bytes[200..202], &value.gdt.limit.to_ne_bytes());
        assert_eq!(&bytes[202..208], &[0x68, 0x24, 0x9A, 0x36, 0xAC, 0x48]);
        assert_eq!(&bytes[280..288], &value.flags.to_ne_bytes());
        assert_eq!(&bytes[288..296], &value.pdptrs[0].to_ne_bytes());
    }

    fn pae_hash(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x1000_0000_01b3)
        })
    }

    fn pae_sregs_summary(label: &str, value: &kvm_sregs2, metadata: &mut String) {
        writeln!(
            metadata,
            "{label}_flags={:#x} {label}_pdptr0={:#x} {label}_pdptr1={:#x} {label}_pdptr2={:#x} {label}_pdptr3={:#x} {label}_cr0={:#x} {label}_cr3={:#x} {label}_cr4={:#x}",
            value.flags,
            value.pdptrs[0],
            value.pdptrs[1],
            value.pdptrs[2],
            value.pdptrs[3],
            value.cr0,
            value.cr3,
            value.cr4,
        )
        .expect("PAE metadata write");
    }

    fn run_pae_phase(
        report_root: &Path,
        phase: PaePhase,
        host_vendor: &str,
        host_paging: &str,
        host_paging_setting: &str,
    ) -> PaeObservation {
        let report_dir = report_root.join(phase.name());
        assert!(
            !report_dir.exists(),
            "{} must be a fresh PAE phase report directory",
            report_dir.display()
        );
        fs::create_dir_all(&report_dir)
            .unwrap_or_else(|e| panic!("create {} failed: {e}", report_dir.display()));
        let (mut ram, mut backend, prewrite_a) = pae_backend();
        assert_eq!(prewrite_a.flags, PAE_SREGS2_FLAGS_PDPTRS_VALID);
        assert_eq!(prewrite_a.pdptrs[0], PAE_PDPT_A_ENTRY);
        let pdpt_a = ram.as_mut_bytes()[PAE_PDPT_GPA..PAE_PDPT_GPA + 8].to_vec();
        assert_eq!(pdpt_a, PAE_PDPT_A_ENTRY.to_le_bytes());
        write_image(
            &report_dir,
            "prewrite-a-sregs2.bin",
            &pae_sregs2_bytes(&prewrite_a),
        );
        write_image(&report_dir, "pdpt-a-prewrite.bin", &pdpt_a);

        let boundary = backend
            .run()
            .unwrap_or_else(|e| panic!("{phase:?} boundary KVM_RUN failed: {e}"));
        pae_expect_output(&boundary, PAE_WARMUP_MARKER, "PAE B boundary");
        assert_eq!(pae_exit_label(&boundary, "PAE B boundary"), "KVM_EXIT_IO");
        let boundary_rip = backend
            .vcpu
            .get_regs()
            .unwrap_or_else(|e| panic!("PAE B KVM_GET_REGS failed: {e}"))
            .rip;
        assert_eq!(
            boundary_rip,
            (PAE_CODE_GPA + PAE_GUEST_PDPT_WRITE_LEN + 8) as u64
        );
        let pdpt_b = ram.as_mut_bytes()[PAE_PDPT_GPA..PAE_PDPT_GPA + 8].to_vec();
        let mut at_b = None;
        let mut endpoint_for_failure = None;
        let continuation_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            assert_eq!(pdpt_b, PAE_PDPT_B_ENTRY.to_le_bytes());
            at_b = match phase {
                PaePhase::NoReadAtB => None,
                PaePhase::OneGetAtB | PaePhase::GetThenSetPrewriteA => {
                    let value = unsafe {
                        // SAFETY: the vCPU is stopped at the checked PIO boundary and KVM writes
                        // the complete kernel-sized `kvm_sregs2` into this independent value.
                        raw_get_sregs2(backend.vcpu.as_raw_fd())
                    }
                    .unwrap_or_else(|e| panic!("{} KVM_GET_SREGS2 at B failed: {e}", phase.name()));
                    at_b = Some(value);
                    match phase {
                        PaePhase::GetThenSetPrewriteA => {
                            unsafe {
                                // SAFETY: the vCPU is stopped at the checked PIO boundary; the
                                // source is a complete pre-write KVM SREGS2 record and no guest
                                // run is interposed before the next continuation.
                                raw_set_sregs2(backend.vcpu.as_raw_fd(), &prewrite_a)
                            }
                            .unwrap_or_else(|e| panic!("PAE raw KVM_SET_SREGS2 at B failed: {e}"));
                            assert_eq!(
                                ram.as_mut_bytes()[PAE_PDPT_GPA..PAE_PDPT_GPA + 8],
                                PAE_PDPT_B_ENTRY.to_le_bytes()
                            );
                        }
                        PaePhase::OneGetAtB | PaePhase::NoReadAtB => {}
                    }
                    Some(value)
                }
            };
            backend.retire_pending_completion().unwrap_or_else(|e| {
                panic!(
                    "{} boundary completion retirement failed: {e}",
                    phase.name()
                )
            });
            let continuation = backend
                .run()
                .unwrap_or_else(|e| panic!("{} continuation KVM_RUN failed: {e}", phase.name()));
            let endpoint_byte = match &continuation {
                Exit::Arch(X86Exit::Io {
                    port: 0x3F8,
                    size: 1,
                    write: Some(value),
                }) => u8::try_from(*value).expect("PAE UART value fits in a byte"),
                other => panic!(
                    "{} returned unexpected continuation KVM_RUN exit: {other:?}",
                    phase.name()
                ),
            };
            assert!(matches!(endpoint_byte, 0x42 | 0x99));
            let continuation_exit = pae_exit_label(&continuation, "PAE continuation");
            backend.retire_pending_completion().unwrap_or_else(|e| {
                panic!(
                    "{} continuation completion retirement failed: {e}",
                    phase.name()
                )
            });
            let endpoint_exit_value = backend
                .run()
                .unwrap_or_else(|e| panic!("{} endpoint KVM_RUN failed: {e}", phase.name()));
            let endpoint_exit = pae_exit_label(&endpoint_exit_value, "PAE endpoint");
            assert_eq!(endpoint_exit, "KVM_EXIT_HLT");
            let endpoint = unsafe {
                // SAFETY: the vCPU is stopped at the checked HLT endpoint and KVM writes a
                // complete kernel-sized `kvm_sregs2` into this diagnostic value.
                raw_get_sregs2(backend.vcpu.as_raw_fd())
            }
            .unwrap_or_else(|e| panic!("{} endpoint KVM_GET_SREGS2 failed: {e}", phase.name()));
            endpoint_for_failure = Some(endpoint);
            let endpoint_rip = backend
                .vcpu
                .get_regs()
                .unwrap_or_else(|e| panic!("{} endpoint KVM_GET_REGS failed: {e}", phase.name()))
                .rip;
            assert_eq!(
                endpoint_rip,
                (PAE_CODE_GPA + PAE_GUEST_PDPT_WRITE_LEN + PAE_BASE_PROGRAM_LEN) as u64
            );
            assert_eq!(backend.exit_counts().total(), 3);
            assert_eq!(backend.exit_counts().io, 2);
            assert_eq!(backend.exit_counts().idle, 1);
            (
                continuation_exit,
                endpoint_exit,
                endpoint,
                endpoint_rip,
                endpoint_byte,
            )
        }));
        let (continuation_exit, endpoint_exit, endpoint, endpoint_rip, endpoint_byte) =
            match continuation_result {
                Ok(value) => value,
                Err(payload) => {
                    write_image(&report_dir, "pdpt-b.bin", &pdpt_b);
                    if let Some(value) = &at_b {
                        write_image(&report_dir, "at-b-sregs2.bin", &pae_sregs2_bytes(value));
                    }
                    if let Some(value) = &endpoint_for_failure {
                        write_image(&report_dir, "endpoint-sregs2.bin", &pae_sregs2_bytes(value));
                    }
                    fs::write(
                        report_dir.join("continuation-failed.txt"),
                        format!("phase={}\ncontinuation=panic\n", phase.name()),
                    )
                    .unwrap_or_else(|e| panic!("write continuation failure report failed: {e}"));
                    std::panic::resume_unwind(payload);
                }
            };
        write_image(&report_dir, "pdpt-b.bin", &pdpt_b);
        if let Some(value) = &at_b {
            write_image(&report_dir, "at-b-sregs2.bin", &pae_sregs2_bytes(value));
        }
        write_image(
            &report_dir,
            "endpoint-sregs2.bin",
            &pae_sregs2_bytes(&endpoint),
        );
        let mut metadata = String::new();
        writeln!(metadata, "phase={}", phase.name()).expect("PAE metadata write");
        writeln!(metadata, "host_vendor={host_vendor}").expect("PAE metadata write");
        writeln!(metadata, "host_paging={host_paging}").expect("PAE metadata write");
        writeln!(metadata, "host_paging_setting={host_paging_setting}")
            .expect("PAE metadata write");
        writeln!(
            metadata,
            "phase_arm=after-visible-boundary-after-backend-auto-completion"
        )
        .expect("PAE metadata write");
        writeln!(
            metadata,
            "backend_visible_exit_labels=KVM_EXIT_IO,KVM_EXIT_IO,KVM_EXIT_HLT"
        )
        .expect("PAE metadata write");
        writeln!(
            metadata,
            "backend_visible_exit_count={}",
            backend.exit_counts().total()
        )
        .expect("PAE metadata write");
        writeln!(
            metadata,
            "completion_retirement_checkpoints=before-continuation,before-endpoint"
        )
        .expect("PAE metadata write");
        writeln!(metadata, "sregs2_gets_at_b={}", phase.reads_at_b()).expect("PAE metadata write");
        writeln!(metadata, "sregs2_sets_at_b={}", phase.sets_at_b()).expect("PAE metadata write");
        writeln!(metadata, "boundary_kvm_run_exit=KVM_EXIT_IO").expect("PAE metadata write");
        writeln!(metadata, "boundary_rip={boundary_rip:#x}").expect("PAE metadata write");
        writeln!(metadata, "continuation_kvm_run_exit={continuation_exit}")
            .expect("PAE metadata write");
        writeln!(metadata, "endpoint_kvm_run_exit={endpoint_exit}").expect("PAE metadata write");
        writeln!(metadata, "pdpt_a_bytes={pdpt_a:02x?}").expect("PAE metadata write");
        writeln!(metadata, "pdpt_a_hash={:#018x}", pae_hash(&pdpt_a)).expect("PAE metadata write");
        writeln!(metadata, "pdpt_b_bytes={pdpt_b:02x?}").expect("PAE metadata write");
        writeln!(metadata, "pdpt_b_hash={:#018x}", pae_hash(&pdpt_b)).expect("PAE metadata write");
        writeln!(metadata, "endpoint_uart_byte={endpoint_byte:#04x}").expect("PAE metadata write");
        writeln!(metadata, "endpoint_rip={endpoint_rip:#x}").expect("PAE metadata write");
        pae_sregs_summary("prewrite_a", &prewrite_a, &mut metadata);
        if let Some(value) = &at_b {
            pae_sregs_summary("at_b", value, &mut metadata);
        } else {
            writeln!(metadata, "at_b_sregs2=not-read").expect("PAE metadata write");
        }
        pae_sregs_summary("endpoint", &endpoint, &mut metadata);
        fs::write(report_dir.join("metadata.txt"), metadata).unwrap_or_else(|e| {
            panic!(
                "write {} failed: {e}",
                report_dir.join("metadata.txt").display()
            )
        });
        println!(
            "PAE_PHASE_OBSERVATION phase={} sregs2_gets_at_b={} sregs2_sets_at_b={} endpoint_uart_byte={endpoint_byte:#04x} endpoint_rip={endpoint_rip:#x} pdpt_b_hash={:#018x}",
            phase.name(),
            phase.reads_at_b(),
            phase.sets_at_b(),
            pae_hash(&pdpt_b),
        );
        PaeObservation {
            phase,
            prewrite_a,
            at_b,
            endpoint,
            pdpt_b,
            endpoint_byte,
            endpoint_rip,
            boundary_exit: "KVM_EXIT_IO",
            continuation_exit,
            endpoint_exit,
        }
    }

    #[test]
    #[ignore = "live x86 KVM NPT/EPT PAE phase observation; set PAE_PHASE_REPORT_DIR"]
    fn x86_pae_sregs2_phase_observations() {
        assert!(
            Path::new("/dev/kvm").exists(),
            "/dev/kvm is required for this ignored PAE hardware diagnostic"
        );
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").expect("read /proc/cpuinfo");
        let host_vendor = cpuinfo
            .lines()
            .find_map(|line| {
                line.split_once(':')
                    .filter(|(key, _)| key.trim() == "vendor_id")
                    .map(|(_, value)| value.trim())
            })
            .expect("/proc/cpuinfo must report a CPU vendor");
        let (host_paging, host_paging_setting) = match host_vendor {
            "AuthenticAMD" => {
                let setting = fs::read_to_string("/sys/module/kvm_amd/parameters/npt")
                    .expect("read kvm_amd NPT setting");
                ("NPT", setting)
            }
            "GenuineIntel" => {
                let setting = fs::read_to_string("/sys/module/kvm_intel/parameters/ept")
                    .expect("read kvm_intel EPT setting");
                ("EPT", setting)
            }
            other => panic!("PAE phase observation does not support CPU vendor {other}"),
        };
        let host_paging_setting = host_paging_setting.trim();
        assert!(
            matches!(host_paging_setting, "Y" | "1"),
            "{host_paging} must be enabled"
        );
        let report_root = PathBuf::from(
            std::env::var_os("PAE_PHASE_REPORT_DIR")
                .expect("PAE_PHASE_REPORT_DIR must capture complete PAE evidence"),
        );
        fs::create_dir_all(&report_root)
            .unwrap_or_else(|e| panic!("create {} failed: {e}", report_root.display()));
        let observations = [
            run_pae_phase(
                &report_root,
                PaePhase::NoReadAtB,
                host_vendor,
                host_paging,
                host_paging_setting,
            ),
            run_pae_phase(
                &report_root,
                PaePhase::OneGetAtB,
                host_vendor,
                host_paging,
                host_paging_setting,
            ),
            run_pae_phase(
                &report_root,
                PaePhase::GetThenSetPrewriteA,
                host_vendor,
                host_paging,
                host_paging_setting,
            ),
        ];
        for observation in observations {
            assert_eq!(observation.boundary_exit, "KVM_EXIT_IO");
            assert_eq!(observation.continuation_exit, "KVM_EXIT_IO");
            assert_eq!(observation.endpoint_exit, "KVM_EXIT_HLT");
            assert_eq!(observation.pdpt_b, PAE_PDPT_B_ENTRY.to_le_bytes());
            assert_eq!(observation.prewrite_a.pdptrs[0], PAE_PDPT_A_ENTRY);
            assert_eq!(
                observation.endpoint_rip,
                (PAE_CODE_GPA + PAE_GUEST_PDPT_WRITE_LEN + PAE_BASE_PROGRAM_LEN) as u64
            );
            assert_eq!(observation.endpoint.cr0, 0x8000_0011);
            assert_eq!(observation.endpoint.cr3, PAE_PDPT_GPA as u64);
            assert_eq!(observation.endpoint.cr4, 0x30);
            assert!(matches!(observation.endpoint_byte, 0x42 | 0x99));
            match observation.phase {
                PaePhase::NoReadAtB => assert!(observation.at_b.is_none()),
                PaePhase::OneGetAtB | PaePhase::GetThenSetPrewriteA => {
                    assert!(observation.at_b.is_some())
                }
            }
        }
    }
}
