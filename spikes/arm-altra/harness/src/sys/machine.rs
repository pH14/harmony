//! The single-vCPU KVM machine and the raw `BR_RETIRED` counter: the Linux half of
//! the seam that [`crate::run`] programs against.
//!
//! **Untested on silicon, and unrunnable off it.** Every ioctl here compiles for
//! `aarch64-unknown-linux-gnu` and none of them has ever executed: the Altra is not
//! in hand. It is written out — rather than left as a comment saying arrival day
//! will write it — because the task's whole purpose is that arrival day is spent
//! measuring, not authoring. What *is* validated pre-silicon is everything the seam
//! does not hide: the ABI constants and struct offsets (unit-tested in the parent
//! module) and the orchestration loop above it (driven natively against a scripted
//! seam in [`crate::run`]).
//!
//! # The two mechanisms, and why neither can pretend to be the other
//!
//! An armed overflow leaves `KVM_RUN` one of two ways, and the harness must never
//! blur them (`docs/ARM-ALTRA.md` §Evidence integrity #4):
//!
//! - **Stock (AA-1(c)):** the PMU overflow raises a signal on the vCPU thread;
//!   `KVM_RUN` returns `EINTR`. This is [`Mechanism::SignalKick`].
//! - **Patched (AA-3):** the 0004-analogue patch draft (`host/patches/`) converts
//!   the guest-mode overflow into a deterministic in-kernel exit with reason
//!   `KVM_EXIT_PREEMPT` (42), armed one-shot per sample by the `KVM_ARM_PREEMPT_EXIT`
//!   vcpu ioctl. This is [`Mechanism::Preempt`].
//!
//! [`PerfCounter`] arms exactly the one it was constructed for, and [`Machine::run`]
//! reports exactly what the kernel returned. A stock kernel cannot emit
//! `KVM_EXIT_PREEMPT` at all, so an AA-3 run-set whose records carry `SignalKick` is
//! visibly the fallback — which is what the floor checker's mechanism check rejects.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use super::{KvmRun, PerfEventAttr, SysError, br_retired_attr, kvm};
use crate::evidence::hex_lower;
use crate::run::{RunError, Vcpu, VcpuExit, WorkCounter};

/// Guest RAM base — the QEMU `virt` / Altra map the payload runtime is linked for
/// (`payloads/linker.ld`: params page at `0x4000_0000`, image at `+512 KiB`).
pub const RAM_BASE: u64 = 0x4000_0000;

/// How much guest RAM the payloads need: the image loads 512 KiB in and carries a
/// 64 KiB stack, so 64 MiB is generous and lets the whole slot be hashed cheaply
/// for the state digest.
pub const RAM_SIZE: usize = 64 << 20;

/// The signal used for the stock overflow kick. `SIGUSR1` rather than `SIGIO`: the
/// handler must not be one the runtime installs for anything else, and the only
/// thing it does is exist, so `KVM_RUN` returns `EINTR`.
const KICK_SIGNAL: i32 = libc::SIGUSR1;

/// `F_SETSIG` — the signal a file descriptor's `O_ASYNC` notification raises. The
/// `libc` crate does not export it; it is 10 on every Linux ABI (`asm-generic/fcntl.h`,
/// and the arch-specific headers that override `F_*` do not override this one).
const F_SETSIG: libc::c_int = 10;

/// `PERF_EVENT_IOC_ENABLE` — `_IO('$', 0)`.
const PERF_IOC_ENABLE: u64 = 0x2400;
/// `PERF_EVENT_IOC_RESET` — `_IO('$', 3)`.
const PERF_IOC_RESET: u64 = 0x2403;
/// `PERF_EVENT_IOC_REFRESH` — `_IO('$', 2)`. Re-arms the event for N overflows.
const PERF_IOC_REFRESH: u64 = 0x2402;
/// `PERF_EVENT_IOC_PERIOD` — `_IOW('$', 4, __u64)`. Sets the overflow deadline.
const PERF_IOC_PERIOD: u64 = 0x4008_2404;

/// Which mechanism a run arms the overflow through.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mechanism {
    /// AA-1(c): a host-side signal kicks the vCPU out of `KVM_RUN` (`EINTR`).
    /// Legitimate pre-patch, and AA-3's forbidden fallback.
    SignalKick,
    /// AA-3: the patched in-kernel force-exit, `KVM_EXIT_PREEMPT`. Requires
    /// [`Capability::DeterministicIntercepts`], which only the patched kernel
    /// advertises.
    Preempt,
}

/// `struct kvm_userspace_memory_region`.
#[repr(C)]
#[derive(Default)]
struct KvmUserspaceMemoryRegion {
    slot: u32,
    flags: u32,
    guest_phys_addr: u64,
    memory_size: u64,
    userspace_addr: u64,
}

/// `struct kvm_vcpu_init`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct KvmVcpuInit {
    target: u32,
    features: [u32; 7],
}

/// `struct kvm_one_reg`.
#[repr(C)]
struct KvmOneReg {
    id: u64,
    addr: u64,
}

fn errno() -> i32 {
    // SAFETY: `__errno_location` returns a valid pointer to this thread's errno.
    unsafe { *libc::__errno_location() }
}

fn err(call: &'static str) -> SysError {
    SysError::Errno {
        call,
        errno: errno(),
    }
}

/// Adapt a seam failure into the run loop's error type. The loop never turns a
/// failed syscall into a record with a plausible zero in it.
fn seam(context: &'static str, e: SysError) -> RunError {
    RunError::Seam {
        context,
        message: e.to_string(),
    }
}

/// Hard-pin the calling thread to one core.
///
/// Pinning is a **correctness** condition on this lineage, not hygiene: the N1/V1
/// arm64 kernel can miss PMU overflow interrupts on core migration (rr #3607), and
/// a missed overflow means `run_until` never breaks out of `KVM_RUN`
/// (`docs/ARM-ALTRA.md` §2). The one sanctioned unpinned run is AA-1's bounded
/// migration probe, which simply does not call this.
///
/// # Errors
/// [`SysError::Errno`] if `sched_setaffinity` failed.
pub fn pin_to_core(core: u32) -> Result<(), SysError> {
    // SAFETY: a zeroed cpu_set_t is a valid empty set; CPU_SET writes one bit of it.
    unsafe {
        let mut set: libc::cpu_set_t = core::mem::zeroed();
        libc::CPU_SET(core as usize, &mut set);
        if libc::sched_setaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &raw const set) != 0
        {
            return Err(err("sched_setaffinity"));
        }
    }
    Ok(())
}

/// Install the no-op handler for the stock overflow kick.
///
/// Deliberately **without** `SA_RESTART`: the whole point is that the signal makes
/// `KVM_RUN` return `EINTR` rather than being transparently resumed.
///
/// # Errors
/// [`SysError::Errno`] if `sigaction` failed.
pub fn install_kick_signal() -> Result<(), SysError> {
    extern "C" fn on_kick(_sig: i32) {}

    // SAFETY: a zeroed sigaction is valid; we set a handler with an empty body and
    // no flags (notably no SA_RESTART), which is exactly the semantics required.
    unsafe {
        let mut act: libc::sigaction = core::mem::zeroed();
        act.sa_sigaction = on_kick as *const () as usize;
        act.sa_flags = 0;
        libc::sigemptyset(&raw mut act.sa_mask);
        if libc::sigaction(KICK_SIGNAL, &raw const act, core::ptr::null_mut()) != 0 {
            return Err(err("sigaction(SIGUSR1)"));
        }
    }
    Ok(())
}

/// The single-vCPU KVM machine: `/dev/kvm`, a VM, one memory slot, one vCPU.
///
/// **Untested on silicon.**
pub struct Machine {
    kvm_fd: i32,
    vm_fd: i32,
    vcpu_fd: i32,
    run: *mut KvmRun,
    run_size: usize,
    mem: *mut u8,
    mem_size: usize,
}

impl Machine {
    /// Create the VM, map guest RAM, create and initialise the vCPU, load the
    /// payload's `PT_LOAD` segments, publish the params page, and set `PC` to the
    /// image's entry point.
    ///
    /// `params` is the params page the guest will read (`payloads/runtime/src/params.rs`):
    /// publishing it is what makes the guest print `PARAMS mode=managed`. A harness
    /// that forgot to publish it would run the smoke scale under a 1e8 claim — so
    /// the guest attests the mode in-band and [`crate::run::run_sample`] refuses a
    /// record without it.
    ///
    /// # Errors
    /// [`SysError`] if any ioctl or mapping failed. Nothing is half-built: a failure
    /// closes what it opened.
    pub fn new(image: &crate::elf::Elf, params: &ParamsPage) -> Result<Machine, SysError> {
        let kvm_fd = open_kvm()?;
        let mut m = Machine {
            kvm_fd,
            vm_fd: -1,
            vcpu_fd: -1,
            run: core::ptr::null_mut(),
            run_size: 0,
            mem: core::ptr::null_mut(),
            mem_size: 0,
        };
        m.build(image, params)?;
        Ok(m)
    }

    /// The build sequence, factored out so [`Machine`]'s `Drop` cleans up a partial
    /// construction rather than leaking fds on the error path.
    fn build(&mut self, image: &crate::elf::Elf, params: &ParamsPage) -> Result<(), SysError> {
        // SAFETY: `kvm_fd` is a valid /dev/kvm descriptor. KVM_CREATE_VM takes a
        // machine type (0 = default) and returns a VM fd.
        self.vm_fd = unsafe { libc::ioctl(self.kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
        if self.vm_fd < 0 {
            return Err(err("ioctl(KVM_CREATE_VM)"));
        }

        // Guest RAM: one anonymous mapping, one memory slot.
        // SAFETY: a fresh anonymous private mapping; no pointer is derived from
        // untrusted data and the length is our own constant.
        let mem = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                RAM_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        if mem == libc::MAP_FAILED {
            return Err(err("mmap(guest RAM)"));
        }
        self.mem = mem.cast::<u8>();
        self.mem_size = RAM_SIZE;

        let region = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size: RAM_SIZE as u64,
            userspace_addr: self.mem as u64,
        };
        // SAFETY: `vm_fd` is valid and `region` is a fully initialised
        // kvm_userspace_memory_region living on this frame.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::SET_USER_MEMORY_REGION as libc::c_ulong,
                &raw const region,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_SET_USER_MEMORY_REGION)"));
        }

        // The single vCPU.
        // SAFETY: `vm_fd` is valid; KVM_CREATE_VCPU takes a vcpu index.
        self.vcpu_fd = unsafe { libc::ioctl(self.vm_fd, kvm::CREATE_VCPU as libc::c_ulong, 0_u64) };
        if self.vcpu_fd < 0 {
            return Err(err("ioctl(KVM_CREATE_VCPU)"));
        }

        // The shared `kvm_run` mapping.
        // SAFETY: `kvm_fd` is valid; KVM_GET_VCPU_MMAP_SIZE takes no argument.
        let run_size =
            unsafe { libc::ioctl(self.kvm_fd, kvm::GET_VCPU_MMAP_SIZE as libc::c_ulong, 0_u64) };
        if run_size < 0 {
            return Err(err("ioctl(KVM_GET_VCPU_MMAP_SIZE)"));
        }
        let run_size = run_size as usize;
        if run_size < core::mem::size_of::<KvmRun>() {
            return Err(SysError::Protocol(format!(
                "KVM_GET_VCPU_MMAP_SIZE returned {run_size} bytes, smaller than the \
                 kvm_run prefix this harness reads ({}): refusing to read past the mapping",
                core::mem::size_of::<KvmRun>()
            )));
        }
        // SAFETY: mapping exactly the size the kernel told us, on the vcpu fd, as
        // the KVM ABI prescribes.
        let run = unsafe {
            libc::mmap(
                core::ptr::null_mut(),
                run_size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.vcpu_fd,
                0,
            )
        };
        if run == libc::MAP_FAILED {
            return Err(err("mmap(kvm_run)"));
        }
        self.run = run.cast::<KvmRun>();
        self.run_size = run_size;

        // arm64 requires an explicit vCPU init against the host's preferred target.
        let mut init = KvmVcpuInit::default();
        // SAFETY: `vm_fd` is valid; KVM_ARM_PREFERRED_TARGET fills `init`.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::ARM_PREFERRED_TARGET as libc::c_ulong,
                &raw mut init,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_ARM_PREFERRED_TARGET)"));
        }
        // SAFETY: `vcpu_fd` is valid; `init` is fully initialised by the call above.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::ARM_VCPU_INIT as libc::c_ulong,
                &raw const init,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_ARM_VCPU_INIT)"));
        }

        self.load_image(image)?;
        self.write_params(params);
        self.set_pc(image.entry())?;
        Ok(())
    }

    /// Copy the image's loadable segments into guest RAM at their link addresses.
    fn load_image(&mut self, image: &crate::elf::Elf) -> Result<(), SysError> {
        for seg in image.load_segments() {
            let offset = seg
                .vaddr
                .checked_sub(RAM_BASE)
                .and_then(|o| usize::try_from(o).ok())
                .ok_or_else(|| {
                    SysError::Protocol(format!(
                        "segment at {:#x} is below guest RAM base {RAM_BASE:#x}",
                        seg.vaddr
                    ))
                })?;
            let end = offset
                .checked_add(seg.mem_size)
                .filter(|end| *end <= self.mem_size)
                .ok_or_else(|| {
                    SysError::Protocol(format!(
                        "segment at {:#x} (+{} bytes) does not fit in {} bytes of guest RAM",
                        seg.vaddr, seg.mem_size, self.mem_size
                    ))
                })?;
            let _ = end;
            // SAFETY: `self.mem` is a live RW mapping of `self.mem_size` bytes, and
            // the bounds check above proved `offset + seg.mem_size` is inside it.
            // The destination is uniquely owned here (the vCPU is not running).
            unsafe {
                let dst = self.mem.add(offset);
                core::ptr::copy_nonoverlapping(seg.bytes.as_ptr(), dst, seg.bytes.len());
                // .bss and friends: the rest of the segment is zero, and RAM from a
                // fresh anonymous mapping already is — but a Machine may be reused,
                // so zero it explicitly rather than relying on that.
                let tail = seg.mem_size.saturating_sub(seg.bytes.len());
                if tail > 0 {
                    core::ptr::write_bytes(dst.add(seg.bytes.len()), 0, tail);
                }
            }
        }
        Ok(())
    }

    /// Publish the params page, so the guest runs the scale and seed the plan asked
    /// for — and prints `PARAMS mode=managed` saying so.
    fn write_params(&mut self, params: &ParamsPage) {
        let bytes = params.to_bytes();
        // SAFETY: the params page is the first page of guest RAM (`PARAMS_GPA ==
        // RAM_BASE`), well inside the mapping, and the vCPU is not running.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), self.mem, bytes.len());
        }
    }

    /// Set the vCPU's `PC` to the image entry point.
    fn set_pc(&mut self, pc: u64) -> Result<(), SysError> {
        let value: u64 = pc;
        let one = KvmOneReg {
            id: kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC,
            addr: (&raw const value) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `one.addr` points at a live u64 on this frame,
        // which is exactly what KVM_SET_ONE_REG's contract requires.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::SET_ONE_REG as libc::c_ulong,
                &raw const one,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_SET_ONE_REG, pc)"));
        }
        Ok(())
    }

    /// The vCPU fd, for the counter's patched-mechanism arming.
    #[must_use]
    pub fn vcpu_fd(&self) -> i32 {
        self.vcpu_fd
    }

    /// Whether the running kernel advertises the 0004-analogue capability on *this*
    /// VM — the positive proof that the patched kernel is the one running.
    ///
    /// # Errors
    /// [`SysError`] if the check could not be issued.
    pub fn patch_marker_observed(&self) -> Result<bool, SysError> {
        // SAFETY: `vm_fd` is valid; KVM_CHECK_EXTENSION takes a capability number
        // and returns 0/positive.
        let rc = unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::CHECK_EXTENSION as libc::c_ulong,
                kvm::CAP_ARM_DETERMINISTIC_INTERCEPTS,
            )
        };
        if rc < 0 {
            return Err(err("ioctl(KVM_CHECK_EXTENSION, DETERMINISTIC_INTERCEPTS)"));
        }
        Ok(rc > 0)
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        // SAFETY: each resource is unmapped/closed exactly once, and only if it was
        // successfully acquired (the sentinels are -1 / null).
        unsafe {
            if !self.run.is_null() {
                libc::munmap(self.run.cast::<libc::c_void>(), self.run_size);
            }
            if !self.mem.is_null() {
                libc::munmap(self.mem.cast::<libc::c_void>(), self.mem_size);
            }
            for fd in [self.vcpu_fd, self.vm_fd, self.kvm_fd] {
                if fd >= 0 {
                    libc::close(fd);
                }
            }
        }
    }
}

impl Vcpu for Machine {
    /// Enter the guest; return at the next exit, **as the kernel described it**.
    ///
    /// No interpretation, no retry, no smoothing: `EINTR` is a
    /// [`VcpuExit::SignalKick`] and reason 42 is a [`VcpuExit::Preempt`], and a
    /// stock kernel can never produce the latter. That is what lets a record attest
    /// its mechanism honestly.
    fn run(&mut self) -> Result<VcpuExit, RunError> {
        // SAFETY: `vcpu_fd` is valid; KVM_RUN takes no argument and returns 0 or -1.
        let rc = unsafe { libc::ioctl(self.vcpu_fd, kvm::RUN as libc::c_ulong, 0_u64) };
        if rc < 0 {
            let e = errno();
            if e == libc::EINTR {
                // The stock mechanism: a host signal kicked the vCPU out.
                return Ok(VcpuExit::SignalKick);
            }
            return Err(RunError::Seam {
                context: "ioctl(KVM_RUN)",
                message: format!("errno {e}"),
            });
        }

        // SAFETY: `self.run` is a live MAP_SHARED mapping of at least
        // size_of::<KvmRun>() bytes (checked at construction); the kernel writes it
        // and we only read. Volatile because the writer is outside this program.
        let reason = unsafe { core::ptr::read_volatile(&raw const (*self.run).exit_reason) };
        match reason {
            kvm::EXIT_MMIO => {
                // SAFETY: as above; the mmio arm is valid exactly when the exit
                // reason says so, which is what this match established.
                let mmio = unsafe { core::ptr::read_volatile(&raw const (*self.run).mmio) };
                let len = (mmio.len as usize).min(mmio.data.len());
                Ok(VcpuExit::Mmio {
                    addr: mmio.phys_addr,
                    data: mmio.data[..len].to_vec(),
                    is_write: mmio.is_write != 0,
                })
            }
            kvm::EXIT_PREEMPT => Ok(VcpuExit::Preempt),
            kvm::EXIT_INTR => Ok(VcpuExit::SignalKick),
            kvm::EXIT_DEBUG => Ok(VcpuExit::Debug),
            other => Ok(VcpuExit::Other(other)),
        }
    }

    /// A digest of the landed state: every architectural register the kernel will
    /// hand over, plus the whole of guest RAM.
    ///
    /// This is what AA-3's replay-identity and AA-6's ≥1,000-rep bit-identity floors
    /// are *about*, and it is computed here rather than left empty — a rep floor
    /// that counts records without ever comparing their digests would be vacuous on
    /// the axis it exists for, so there must be a real digest to compare.
    ///
    /// Registers are hashed in **sorted id order** (a `BTreeMap`, never a `HashMap`):
    /// iteration order must not reach a hashed byte. Conventions rule 4.
    fn state_digest(&mut self) -> Result<String, RunError> {
        let ids = self
            .reg_list()
            .map_err(|e| seam("ioctl(KVM_GET_REG_LIST)", e))?;

        let mut regs: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        for id in ids {
            let value = self
                .read_reg(id)
                .map_err(|e| seam("ioctl(KVM_GET_ONE_REG)", e))?;
            regs.insert(id, value);
        }

        let mut h = Sha256::new();
        h.update(b"arm-spike-state-v1");
        for (id, value) in &regs {
            h.update(id.to_le_bytes());
            h.update(value);
        }
        // SAFETY: `self.mem` is a live mapping of `self.mem_size` bytes and the vCPU
        // is not running (we are between exits), so nothing else writes it.
        let ram = unsafe { core::slice::from_raw_parts(self.mem, self.mem_size) };
        h.update(ram);

        Ok(format!("sha256:{}", hex_lower(&h.finalize())))
    }
}

impl Machine {
    /// `KVM_GET_REG_LIST`: ask for the count, then the ids.
    fn reg_list(&self) -> Result<Vec<u64>, SysError> {
        // The ioctl takes a `struct kvm_reg_list { __u64 n; __u64 reg[n]; }` and, when
        // `n` is too small, fails with E2BIG after writing the required `n`.
        let mut n: u64 = 0;
        // SAFETY: `vcpu_fd` is valid; with n == 0 the kernel writes only the count.
        let rc =
            unsafe { libc::ioctl(self.vcpu_fd, kvm::GET_REG_LIST as libc::c_ulong, &raw mut n) };
        if rc == 0 {
            // A vCPU with no registers is not a thing; refuse rather than hash nothing.
            return Err(SysError::Protocol(
                "KVM_GET_REG_LIST reported no registers: refusing to digest an empty state".into(),
            ));
        }
        if errno() != libc::E2BIG {
            return Err(err("ioctl(KVM_GET_REG_LIST, count)"));
        }
        let count = usize::try_from(n).map_err(|_| {
            SysError::Protocol("KVM_GET_REG_LIST returned an implausible register count".into())
        })?;

        // One u64 for `n`, then `count` ids.
        let mut buf: Vec<u64> = vec![0; count + 1];
        buf[0] = n;
        // SAFETY: `buf` is `count + 1` u64s long, exactly the layout kvm_reg_list
        // wants for this `n`; the kernel writes at most that many.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::GET_REG_LIST as libc::c_ulong,
                buf.as_mut_ptr(),
            )
        } < 0
        {
            return Err(err("ioctl(KVM_GET_REG_LIST)"));
        }
        Ok(buf[1..].to_vec())
    }

    /// `KVM_GET_ONE_REG` — the register's bytes, sized by its own id encoding.
    fn read_reg(&self, id: u64) -> Result<Vec<u8>, SysError> {
        let shift = (id & kvm::REG_SIZE_MASK) >> kvm::REG_SIZE_SHIFT;
        // The encoding is log2(size in bytes); anything past 2^7 is not a register
        // shape this ABI defines, and is refused rather than guessed at.
        if shift > 7 {
            return Err(SysError::Protocol(format!(
                "register {id:#x} declares an unknown size class {shift}"
            )));
        }
        let size = 1usize << shift;
        let mut value = vec![0u8; size];
        let one = KvmOneReg {
            id,
            addr: value.as_mut_ptr() as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `one.addr` points at `size` writable bytes,
        // which is exactly the width the register id declares.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::GET_ONE_REG as libc::c_ulong,
                &raw const one,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_GET_ONE_REG)"));
        }
        Ok(value)
    }
}

/// The params page the harness publishes for one sample.
///
/// Mirrors `payloads/runtime/src/params.rs`'s `ParamsPage` — the wire between the
/// harness and the guest. Written by the harness before the vCPU runs; the guest
/// reads it and prints `PARAMS mode=managed`.
#[derive(Clone, Copy, Debug)]
pub struct ParamsPage {
    /// The scale index (`oracle_model::Scale::index`).
    pub scale_index: u32,
    /// The PRNG seed.
    pub seed: u64,
}

impl ParamsPage {
    /// The page's 24 on-wire bytes, little-endian.
    #[must_use]
    pub fn to_bytes(self) -> [u8; 24] {
        let mut b = [0u8; 24];
        b[0..4].copy_from_slice(&oracle_model::PARAMS_MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&oracle_model::PARAMS_ABI.to_le_bytes());
        b[8..12].copy_from_slice(&self.scale_index.to_le_bytes());
        // b[12..16] is the reserved word: zero.
        b[16..24].copy_from_slice(&self.seed.to_le_bytes());
        b
    }
}

/// The work counter: raw `BR_RETIRED`, pinned and guest-only.
///
/// The patched mechanism arms through the **vCPU fd** (`KVM_ARM_PREEMPT_EXIT`), so
/// the counter keeps a copy of it. It is a borrowed descriptor, not an owned one:
/// the counter must not outlive its [`Machine`], and the orchestrator builds and
/// drops them together, per sample. Getting that wrong is not a memory-safety
/// question — the ioctl would fail `EBADF`, loudly, which is the seam behaving
/// exactly as it should.
///
/// **Untested on silicon.**
pub struct PerfCounter {
    fd: i32,
    vcpu_fd: i32,
    mechanism: Mechanism,
    attr: PerfEventAttr,
}

impl PerfCounter {
    /// Open raw `BR_RETIRED` on the calling thread, armed per `mechanism`.
    ///
    /// The calling thread must be the one that will call `KVM_RUN` (the counter
    /// follows the *thread*), and it must already be pinned ([`pin_to_core`]).
    ///
    /// A [`Mechanism::Preempt`] counter refuses to open on a kernel that does not
    /// advertise [`Capability::DeterministicIntercepts`] — the patched mechanism
    /// cannot be *silently* downgraded to the stock kick, because there is no code
    /// path from here to that fallback (`docs/ARM-ALTRA.md` §Evidence integrity #4).
    ///
    /// # Errors
    /// [`SysError`] if the event could not be opened, or if the patched mechanism
    /// was asked for on a kernel that does not have it.
    pub fn open(
        machine: &Machine,
        mechanism: Mechanism,
        sample_period: Option<u64>,
    ) -> Result<PerfCounter, SysError> {
        if mechanism == Mechanism::Preempt && !machine.patch_marker_observed()? {
            return Err(SysError::Protocol(
                "the patched mechanism (KVM_EXIT_PREEMPT) was requested but this kernel does not \
                 advertise KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS: refusing to fall back to the \
                 stock signal kick, which is AA-3's forbidden fallback"
                    .into(),
            ));
        }

        let attr = br_retired_attr(sample_period);
        // SAFETY: `attr` is a fully initialised perf_event_attr on this frame.
        // Counting the calling thread (pid 0) wherever it runs (-1) — the thread is
        // pinned, so "wherever" is the one core.
        let fd = unsafe { super::imp::perf_event_open(&raw const attr, 0, -1, -1, 0) };
        if fd < 0 {
            return Err(err("perf_event_open(BR_RETIRED)"));
        }
        let fd = fd as i32;

        let counter = PerfCounter {
            fd,
            vcpu_fd: machine.vcpu_fd(),
            mechanism,
            attr,
        };
        counter.setup()?;
        Ok(counter)
    }

    /// Signal plumbing (stock mechanism only) and enable.
    fn setup(&self) -> Result<(), SysError> {
        if self.mechanism == Mechanism::SignalKick {
            install_kick_signal()?;
            // Route the overflow to this thread as KICK_SIGNAL. Without O_ASYNC the
            // overflow is silent and `KVM_RUN` never returns — a lost PMI by
            // construction rather than by hardware.
            // SAFETY: `self.fd` is a valid perf event descriptor; these fcntls take
            // integer arguments.
            unsafe {
                if libc::fcntl(self.fd, libc::F_SETFL, libc::O_ASYNC) < 0 {
                    return Err(err("fcntl(F_SETFL, O_ASYNC)"));
                }
                if libc::fcntl(self.fd, F_SETSIG, KICK_SIGNAL) < 0 {
                    return Err(err("fcntl(F_SETSIG)"));
                }
                if libc::fcntl(self.fd, libc::F_SETOWN, libc::getpid()) < 0 {
                    return Err(err("fcntl(F_SETOWN)"));
                }
            }
        }
        self.ioctl(PERF_IOC_RESET, 0, "PERF_EVENT_IOC_RESET")?;
        self.ioctl(PERF_IOC_ENABLE, 0, "PERF_EVENT_IOC_ENABLE")?;
        Ok(())
    }

    fn ioctl(&self, request: u64, arg: u64, call: &'static str) -> Result<(), SysError> {
        // SAFETY: `self.fd` is a valid perf event descriptor; every request used
        // here takes an integer argument.
        if unsafe { libc::ioctl(self.fd, request as libc::c_ulong, arg) } < 0 {
            return Err(err(call));
        }
        Ok(())
    }

    /// The `perf_event_attr` this counter was actually opened with — the source of
    /// the manifest's `perf` block ([`super::perf_config`]), so the evidence cannot
    /// describe an arming that did not happen.
    #[must_use]
    pub fn attr(&self) -> &PerfEventAttr {
        &self.attr
    }
}

impl Drop for PerfCounter {
    fn drop(&mut self) {
        // SAFETY: `fd` is owned by this counter and closed exactly once.
        unsafe {
            libc::close(self.fd);
        }
    }
}

impl WorkCounter for PerfCounter {
    fn read(&mut self) -> Result<u64, RunError> {
        let mut buf = [0u8; 8];
        // SAFETY: reading 8 bytes into an 8-byte buffer from a valid perf fd, which
        // is the read(2) format of a non-grouped counter.
        let n = unsafe { libc::read(self.fd, buf.as_mut_ptr().cast::<libc::c_void>(), 8) };
        if n != 8 {
            return Err(RunError::Seam {
                context: "read(perf counter)",
                message: format!("read returned {n} bytes, want 8 (errno {})", errno()),
            });
        }
        Ok(u64::from_le_bytes(buf))
    }

    /// Arm a one-shot overflow `delta` events from now, through the mechanism this
    /// counter was opened for — and only that one.
    fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError> {
        self.ioctl(PERF_IOC_PERIOD, delta, "PERF_EVENT_IOC_PERIOD")
            .map_err(|e| seam("PERF_EVENT_IOC_PERIOD", e))?;
        // REFRESH(1): exactly one overflow, then the event disables itself. A
        // free-running overflow would deliver an unbounded number of kicks and make
        // per-record multiplicity meaningless.
        self.ioctl(PERF_IOC_REFRESH, 1, "PERF_EVENT_IOC_REFRESH")
            .map_err(|e| seam("PERF_EVENT_IOC_REFRESH", e))?;

        if self.mechanism == Mechanism::Preempt {
            // Arm the patch's one-shot in-kernel force-exit. Without this the
            // overflow IRQ is handled and the guest is re-entered — the exit simply
            // never comes.
            // SAFETY: `vcpu_fd` is the borrowed machine's valid vCPU descriptor;
            // KVM_ARM_PREEMPT_EXIT takes no argument.
            if unsafe { libc::ioctl(self.vcpu_fd, kvm::ARM_PREEMPT_EXIT as libc::c_ulong, 0_u64) }
                < 0
            {
                return Err(seam(
                    "ioctl(KVM_ARM_PREEMPT_EXIT)",
                    err("ioctl(KVM_ARM_PREEMPT_EXIT)"),
                ));
            }
        }
        Ok(())
    }
}

/// Open `/dev/kvm`. (Duplicated from [`super::imp`] rather than exported from it:
/// the probe module is deliberately dependency-free.)
fn open_kvm() -> Result<i32, SysError> {
    let path = c"/dev/kvm";
    // SAFETY: opening a device with a valid NUL-terminated path.
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(err("open(/dev/kvm)"));
    }
    Ok(fd)
}
