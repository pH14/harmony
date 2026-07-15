// SPDX-License-Identifier: AGPL-3.0-or-later
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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{KvmRun, PerfEventAttr, SysError, br_retired_attr, kvm};
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

/// Whether the last `KICK_SIGNAL` was **sourced by the armed perf fd**, not injected
/// externally. Set by the `SA_SIGINFO` handler from `si_code`: a signal from a file
/// descriptor's `O_ASYNC` notification carries a `POLL_*` code (positive), while a
/// `kill(SIGUSR1)` from anywhere else carries `SI_USER` (0) or `SI_QUEUE` (negative).
/// The run loop only counts a stock delivery when this is set, so a stray external
/// `SIGUSR1` cannot masquerade as the overflow kick and certify a broken counter.
static PERF_SOURCED_KICK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The watchdog signal. `SIGALRM` (raised by `setitimer(ITIMER_REAL)`) so a wedged
/// `KVM_RUN` — a guest stuck in WFI with no wake, a lost PMI, a livelocked exclusive —
/// returns `EINTR` and the run loop regains control instead of hanging forever.
const WATCHDOG_SIGNAL: i32 = libc::SIGALRM;

/// Set by the `SIGALRM` handler when the per-sample deadline fires. The run loop
/// checks it on `EINTR` and turns a wedge into [`RunError::Watchdog`] rather than
/// re-entering the guest. Distinct from [`PERF_SOURCED_KICK`]: a watchdog `EINTR` is a
/// failure to record, a perf kick is a delivery to count.
static WATCHDOG_FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The default per-`KVM_RUN` watchdog budget — re-exported from the portable run
/// module ([`crate::run::DEFAULT_WATCHDOG_SECS`]) so the value has one home the CLI can
/// reach on any target. It is a liveness backstop, not a measurement parameter, so a
/// sane default (overridable) is appropriate; nothing in the acceptance criteria rests
/// on its value.
pub use crate::run::DEFAULT_WATCHDOG_SECS;

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

/// The "parked" sampling period — large enough that the event never overflows within a
/// window (used to open an armed event so it is sampling from the start, and to resume it
/// after a landing), yet with **bit 63 CLEAR**. Linux rejects a `sample_period` whose top
/// bit is set: `perf_event_open` and `PERF_EVENT_IOC_PERIOD` both return `EINVAL`, so a
/// `u64::MAX` sentinel EINVALs every armed run before the first sample (the r8 sampling-mode
/// fix regressed here). `i64::MAX` is the largest period the kernel accepts.
const PARKED_PERIOD: u64 = i64::MAX as u64;
// The invariant the whole armed path depends on, pinned at compile time: bit 63 clear, so
// Linux does not EINVAL the period. A regression back to `u64::MAX` fails the build here.
const _: () = assert!(PARKED_PERIOD & (1u64 << 63) == 0);

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

/// `struct kvm_enable_cap` (104 bytes: `cap`, `flags`, `args[4]`, `pad[64]`).
#[repr(C)]
struct KvmEnableCap {
    cap: u32,
    flags: u32,
    args: [u64; 4],
    pad: [u8; 64],
}

impl Default for KvmEnableCap {
    fn default() -> KvmEnableCap {
        // `[u8; 64]` has no derivable Default; the struct is all-zero otherwise.
        KvmEnableCap {
            cap: 0,
            flags: 0,
            args: [0; 4],
            pad: [0; 64],
        }
    }
}

/// `struct kvm_create_device`.
#[repr(C)]
#[derive(Default)]
struct KvmCreateDevice {
    type_: u32,
    fd: u32,
    flags: u32,
}

/// `struct kvm_device_attr`.
#[repr(C)]
#[derive(Default)]
struct KvmDeviceAttr {
    flags: u32,
    group: u32,
    attr: u64,
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
    // The core number is CLI input, and `CPU_SET` indexes a fixed-size bit array —
    // libc's Rust `CPU_SET` panics on an index at or past `CPU_SETSIZE` rather than
    // returning an error. Library code must not panic on untrusted input, so bound it
    // here and return a clean error instead.
    let cpu_setsize = core::mem::size_of::<libc::cpu_set_t>() * 8;
    if (core as usize) >= cpu_setsize {
        return Err(SysError::Protocol(format!(
            "core {core} is at or past CPU_SETSIZE ({cpu_setsize}): out of range for an affinity mask"
        )));
    }
    // SAFETY: a zeroed cpu_set_t is a valid empty set; CPU_SET writes one bit of it,
    // and `core` is now proven in range.
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

/// The CPUs this thread is permitted to run on (`sched_getaffinity`), ascending.
///
/// This is the cpuset **lease** the operator granted, not an invented core list. AA-1's
/// migration probe rotates the vCPU thread across these to FORCE cross-core movement,
/// rather than merely leaving it unpinned and trusting a quiet scheduler to move it (the
/// r13 review's point: on an idle host the thread can sit on one CPU for the whole run,
/// so an unpinned probe may exercise no migration at all).
///
/// # Errors
/// [`SysError`] if `sched_getaffinity` failed.
pub fn allowed_cores() -> Result<Vec<u32>, SysError> {
    // SAFETY: a zeroed cpu_set_t is a valid empty set; sched_getaffinity fills it, and
    // CPU_ISSET only reads it.
    let cores = unsafe {
        let mut set: libc::cpu_set_t = core::mem::zeroed();
        if libc::sched_getaffinity(0, core::mem::size_of::<libc::cpu_set_t>(), &raw mut set) != 0 {
            return Err(err("sched_getaffinity"));
        }
        let cpu_setsize = core::mem::size_of::<libc::cpu_set_t>() * 8;
        (0..cpu_setsize)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .map(|c| c as u32)
            .collect()
    };
    Ok(cores)
}

/// This thread's kernel task id (`gettid`), for a churner to target with
/// `sched_setaffinity`.
#[must_use]
pub fn current_tid() -> libc::pid_t {
    // SAFETY: SYS_gettid takes no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

/// A background thread that continuously moves a target thread across a set of cores.
///
/// AA-1's migration probe must migrate the vCPU thread **while its perf overflow is armed**
/// — that is the rr #3607 missed-PMI failure mode. Re-pinning between samples (r13) never
/// does this: the counter is opened AFTER the move and dropped before the next, so no armed
/// context ever migrates. This churner instead rotates the *live* vCPU thread's affinity
/// underneath it, so an armed `KVM_RUN` in progress is forced across cores mid-run. It runs
/// for the whole probe, targeting the vCPU thread's tid; the main loop opens/arms/reads the
/// counter as usual. **Untested on silicon.**
pub struct MigrationChurner {
    stop: Arc<AtomicBool>,
    /// How many affinity moves the churner has issued — evidence the probe actually churned
    /// (a zero here means the background move never ran, and the "migration" was a no-op).
    moves: Arc<AtomicU64>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl MigrationChurner {
    /// Start churning `tid`'s affinity across `cores` (which must be non-empty and in range,
    /// as [`allowed_cores`] guarantees) until dropped or [`MigrationChurner::stop`]ped.
    #[must_use]
    pub fn start(tid: libc::pid_t, cores: Vec<u32>) -> MigrationChurner {
        let stop = Arc::new(AtomicBool::new(false));
        let moves = Arc::new(AtomicU64::new(0));
        let (stop_t, moves_t) = (Arc::clone(&stop), Arc::clone(&moves));
        let handle = std::thread::spawn(move || {
            // Block the vCPU signals on THIS helper thread. `PerfCounter::setup` uses
            // process-directed `F_SETOWN(getpid())` for the stock overflow kick, and the
            // watchdog uses process-wide `ITIMER_REAL` — both process-directed, so the kernel
            // is free to run either handler on whichever thread has the signal unblocked. If it
            // picked this churner, the handler would set its global atomic without interrupting
            // the vCPU thread blocked in `KVM_RUN` — a real overflow reported lost, or a
            // watchdog that never breaks a wedge. Blocking them here forces delivery to the
            // vCPU thread (which leaves them unblocked).
            // SAFETY: a zeroed sigset is valid; sigaddset/pthread_sigmask take valid pointers.
            unsafe {
                let mut mask: libc::sigset_t = core::mem::zeroed();
                libc::sigemptyset(&raw mut mask);
                libc::sigaddset(&raw mut mask, libc::SIGUSR1);
                libc::sigaddset(&raw mut mask, libc::SIGALRM);
                libc::pthread_sigmask(libc::SIG_BLOCK, &raw const mask, core::ptr::null_mut());
            }
            let mut i = 0usize;
            while !stop_t.load(Ordering::Relaxed) {
                let core = cores[i % cores.len()];
                // SAFETY: a zeroed cpu_set_t is a valid empty set; `core` is an allowed-cpuset
                // index (< CPU_SETSIZE) so CPU_SET cannot go out of range; setaffinity targets
                // the vCPU thread by tid. A failed move is best-effort (the thread may be mid
                // guest-exit); only a SUCCESSFUL one is counted, so `moves` is honest.
                unsafe {
                    let mut set: libc::cpu_set_t = core::mem::zeroed();
                    libc::CPU_SET(core as usize, &mut set);
                    let rc = libc::sched_setaffinity(
                        tid,
                        core::mem::size_of::<libc::cpu_set_t>(),
                        &raw const set,
                    );
                    if rc == 0 {
                        moves_t.fetch_add(1, Ordering::Relaxed);
                    }
                }
                i += 1;
                // Fast enough that any armed KVM_RUN sees at least one move, without busy-spin.
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        });
        MigrationChurner {
            stop,
            moves,
            handle: Some(handle),
        }
    }

    /// How many affinity moves the churner has successfully issued so far.
    #[must_use]
    pub fn moves(&self) -> u64 {
        self.moves.load(Ordering::Relaxed)
    }

    /// Stop churning and join the thread, returning the total successful moves.
    pub fn stop(mut self) -> u64 {
        self.shutdown();
        self.moves()
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl Drop for MigrationChurner {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Install the no-op handler for the stock overflow kick.
///
/// Deliberately **without** `SA_RESTART`: the whole point is that the signal makes
/// `KVM_RUN` return `EINTR` rather than being transparently resumed.
///
/// # Errors
/// [`SysError::Errno`] if `sigaction` failed.
pub fn install_kick_signal() -> Result<(), SysError> {
    // `SA_SIGINFO` so the handler can read `si_code` and record whether this signal
    // came from a file descriptor (the armed perf event) or was injected externally.
    // The handler does the classification; the run loop reads the flag. Async-signal
    // safe: it only reads an integer field and stores to an atomic.
    extern "C" fn on_kick(_sig: i32, info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        use std::sync::atomic::Ordering;
        // A POLL/fd-sourced signal carries a positive `si_code` (POLL_IN == 1, …); a
        // `kill()` carries SI_USER (0) or SI_QUEUE (negative). Treat only the former as
        // the perf overflow kick.
        // SAFETY: `info` is the kernel-provided siginfo for this delivery; `si_code`
        // is a plain int field always present.
        let from_fd = !info.is_null() && unsafe { (*info).si_code } > 0;
        PERF_SOURCED_KICK.store(from_fd, Ordering::SeqCst);
    }

    // SAFETY: a zeroed sigaction is valid; SA_SIGINFO selects the three-argument
    // handler above, and no SA_RESTART means the signal makes KVM_RUN return EINTR.
    unsafe {
        let mut act: libc::sigaction = core::mem::zeroed();
        act.sa_sigaction = on_kick as *const () as usize;
        act.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&raw mut act.sa_mask);
        if libc::sigaction(KICK_SIGNAL, &raw const act, core::ptr::null_mut()) != 0 {
            return Err(err("sigaction(SIGUSR1)"));
        }
    }
    Ok(())
}

/// Install the watchdog `SIGALRM` handler — likewise **without** `SA_RESTART`, so a
/// fired deadline makes the blocked `KVM_RUN` return `EINTR`.
///
/// # Errors
/// [`SysError::Errno`] if `sigaction` failed.
pub fn install_watchdog_signal() -> Result<(), SysError> {
    // Async-signal safe: it stores to one atomic and nothing else.
    extern "C" fn on_alarm(_sig: i32, _info: *mut libc::siginfo_t, _ctx: *mut libc::c_void) {
        WATCHDOG_FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // SAFETY: a zeroed sigaction is valid; no SA_RESTART means SIGALRM makes KVM_RUN
    // return EINTR rather than resuming transparently.
    unsafe {
        let mut act: libc::sigaction = core::mem::zeroed();
        act.sa_sigaction = on_alarm as *const () as usize;
        act.sa_flags = libc::SA_SIGINFO;
        libc::sigemptyset(&raw mut act.sa_mask);
        if libc::sigaction(WATCHDOG_SIGNAL, &raw const act, core::ptr::null_mut()) != 0 {
            return Err(err("sigaction(SIGALRM)"));
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
    vgic_fd: i32,
    run: *mut KvmRun,
    run_size: usize,
    mem: *mut u8,
    mem_size: usize,
    /// Per-`KVM_RUN` watchdog budget in seconds; 0 disables it. See
    /// [`DEFAULT_WATCHDOG_SECS`].
    watchdog_secs: u64,
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
            vgic_fd: -1,
            run: core::ptr::null_mut(),
            run_size: 0,
            mem: core::ptr::null_mut(),
            mem_size: 0,
            watchdog_secs: DEFAULT_WATCHDOG_SECS,
        };
        m.build(image, params)?;
        Ok(m)
    }

    /// Override the per-`KVM_RUN` watchdog budget (seconds; 0 disables). A wedged guest
    /// past this deadline surfaces as [`RunError::Watchdog`] instead of a hang.
    pub fn set_watchdog_secs(&mut self, secs: u64) {
        self.watchdog_secs = secs;
    }

    /// Arm `ITIMER_REAL` to raise `SIGALRM` after `watchdog_secs`, clearing any stale
    /// fired flag first.
    ///
    /// # Errors
    /// Returns a seam error if `watchdog_secs` does not fit `time_t` (an out-of-range
    /// value would cast negative and make `setitimer` fail `EINVAL`) or if `setitimer`
    /// itself fails. This MUST propagate: silently ignoring it would enter `KVM_RUN`
    /// believing a watchdog is armed when it is not, so a wedged guest would block
    /// forever with no partial evidence — the exact failure the watchdog exists to
    /// prevent.
    fn arm_watchdog(&self) -> Result<(), RunError> {
        let secs = libc::time_t::try_from(self.watchdog_secs).map_err(|_| RunError::Seam {
            context: "arm watchdog",
            message: format!("watchdog-secs {} exceeds time_t range", self.watchdog_secs),
        })?;
        WATCHDOG_FIRED.store(false, std::sync::atomic::Ordering::SeqCst);
        let it = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: secs,
                tv_usec: 0,
            },
        };
        // SAFETY: `it` is a fully-initialized itimerval; ITIMER_REAL is a valid timer.
        let rc =
            unsafe { libc::setitimer(libc::ITIMER_REAL, &raw const it, core::ptr::null_mut()) };
        if rc != 0 {
            return Err(seam("setitimer(ITIMER_REAL, arm)", err("setitimer")));
        }
        Ok(())
    }

    /// Disarm `ITIMER_REAL` so a pending deadline cannot fire during the non-`KVM_RUN`
    /// work (state digest, evidence assembly) that follows an exit.
    fn disarm_watchdog(&self) {
        let off = libc::itimerval {
            it_interval: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            it_value: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
        };
        // SAFETY: `off` is a fully-initialized zero itimerval; disarms ITIMER_REAL.
        unsafe {
            libc::setitimer(libc::ITIMER_REAL, &raw const off, core::ptr::null_mut());
        }
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

        // The in-kernel vGICv3. The payload runtime programs the GIC distributor at
        // 0x0800_0000 before it prints a byte; with no vGIC those accesses are MMIO
        // exits to userspace, which the measurement loop refuses as non-console — so
        // NO payload boots without this. Created after VCPU_INIT (the vGIC needs the
        // vCPU to exist) and before the guest runs.
        self.create_vgic()?;

        self.load_image(image)?;
        self.write_params(params);
        self.publish_pvclock_page();
        self.set_pc(image.entry())?;

        // The KVM_RUN watchdog handler — installed here so every Machine that can enter
        // the guest can also be pulled back out of a wedge. Orthogonal to the counter's
        // kick signal (which PerfCounter::setup installs only for the stock mechanism).
        install_watchdog_signal()?;
        Ok(())
    }

    /// Create and initialise the in-kernel GICv3: the device, its distributor and
    /// redistributor MMIO windows (at the addresses `payloads/runtime/src/gic.rs`
    /// expects), then `KVM_DEV_ARM_VGIC_CTRL_INIT`.
    fn create_vgic(&mut self) -> Result<(), SysError> {
        let mut dev = KvmCreateDevice {
            type_: kvm::DEV_TYPE_ARM_VGIC_V3,
            fd: 0,
            flags: 0,
        };
        // SAFETY: `vm_fd` is valid; KVM_CREATE_DEVICE fills `dev.fd` with the new
        // device descriptor.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::CREATE_DEVICE as libc::c_ulong,
                &raw mut dev,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_CREATE_DEVICE, vGICv3)"));
        }
        let vgic_fd = dev.fd as i32;
        self.vgic_fd = vgic_fd;

        let set_addr = |group: u32, attr: u64, value: &u64| -> Result<(), SysError> {
            let da = KvmDeviceAttr {
                flags: 0,
                group,
                attr,
                addr: (value as *const u64) as u64,
            };
            // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u64 on the
            // caller's frame, which is what KVM_SET_DEVICE_ATTR's ADDR group reads.
            if unsafe {
                libc::ioctl(
                    vgic_fd,
                    kvm::SET_DEVICE_ATTR as libc::c_ulong,
                    &raw const da,
                )
            } < 0
            {
                return Err(err("ioctl(KVM_SET_DEVICE_ATTR, vGIC addr)"));
            }
            Ok(())
        };

        let dist = kvm::GICD_BASE;
        let redist = kvm::GICR_BASE;
        set_addr(
            kvm::DEV_ARM_VGIC_GRP_ADDR,
            kvm::VGIC_V3_ADDR_TYPE_DIST,
            &dist,
        )?;
        set_addr(
            kvm::DEV_ARM_VGIC_GRP_ADDR,
            kvm::VGIC_V3_ADDR_TYPE_REDIST,
            &redist,
        )?;

        // KVM_DEV_ARM_VGIC_CTRL_INIT finalises the distributor.
        let init = KvmDeviceAttr {
            flags: 0,
            group: kvm::DEV_ARM_VGIC_GRP_CTRL,
            attr: kvm::DEV_ARM_VGIC_CTRL_INIT,
            addr: 0,
        };
        // SAFETY: `vgic_fd` is valid; the CTRL/INIT attr takes no address argument.
        if unsafe {
            libc::ioctl(
                vgic_fd,
                kvm::SET_DEVICE_ATTR as libc::c_ulong,
                &raw const init,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_SET_DEVICE_ATTR, vGIC CTRL_INIT)"));
        }
        Ok(())
    }

    /// Enable the 0004-analogue determinism opt-in on this VM.
    ///
    /// The patch gates `KVM_ARM_PREEMPT_EXIT` on `KVM_ARCH_FLAG_DETERMINISTIC_INTERCEPTS`,
    /// which is set only by `KVM_ENABLE_CAP(KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS)`.
    /// Advertising the capability (which `patch_marker_observed` checks) is not the
    /// same as enabling it: without this call every arm returns `EINVAL`, even on the
    /// patched kernel. Called only for the patched mechanism.
    ///
    /// # Errors
    /// [`SysError`] if the capability could not be enabled.
    pub fn enable_deterministic_intercepts(&self) -> Result<(), SysError> {
        let cap = KvmEnableCap {
            cap: kvm::CAP_ARM_DETERMINISTIC_INTERCEPTS as u32,
            ..Default::default()
        };
        // SAFETY: `vm_fd` is valid; `cap` is a fully initialised kvm_enable_cap on
        // this frame.
        if unsafe { libc::ioctl(self.vm_fd, kvm::ENABLE_CAP as libc::c_ulong, &raw const cap) } < 0
        {
            return Err(err("ioctl(KVM_ENABLE_CAP, DETERMINISTIC_INTERCEPTS)"));
        }
        Ok(())
    }

    /// Copy the image's loadable segments into guest RAM at their link addresses.
    ///
    /// The bounds-checked copy itself lives in [`crate::elf::Elf::load_into`], which
    /// operates on a `&mut [u8]` in **safe, Miri-checkable** code — so the memory
    /// safety of loading an untrusted image is verified by the interpreter, not
    /// asserted here. This function's only job is to hand that method a slice over the
    /// guest mapping; the one `unsafe` is forming the slice.
    fn load_image(&mut self, image: &crate::elf::Elf) -> Result<(), SysError> {
        // SAFETY: `self.mem` is a live RW mapping of exactly `self.mem_size` bytes,
        // and the vCPU is not running (we are in construction), so nothing else
        // aliases it. `load_into` never writes outside the slice it is given.
        let dst = unsafe { core::slice::from_raw_parts_mut(self.mem, self.mem_size) };
        image
            .load_into(dst, RAM_BASE)
            .map_err(|e| SysError::Protocol(format!("load payload image: {e}")))
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

    /// Publish a **managed** work-derived clock page (`docs/PARAVIRT-CLOCK.md` ABI 1)
    /// at `PVCLOCK_GPA`, so an AA-5 guest reads `CLOCKPAGE mode=managed`.
    ///
    /// Without this the page reads as zeroed RAM, the guest falls back to publishing
    /// its own static page, and reports `self-seeded` — which AA-5's acceptance forbids
    /// (`payloads/runtime/src/pvclock.rs`). This is the *minimum* the harness owes AA-5:
    /// a valid, materialized, deterministic page. The full work-derived refresh (the
    /// value advancing with `work`) is `hm-8h8`'s design, which AA-5 validates; a static
    /// materialized page is deterministic across same-seed reps and is what the
    /// pre-silicon apparatus needs to exercise the managed-vs-self-seeded attestation.
    /// The page is published for every stage — non-AA-5 payloads simply do not read it.
    fn publish_pvclock_page(&mut self) {
        // The page bytes are built by the shared, Miri-tested layout in oracle-model. A
        // materialized STATIC placeholder — `work_derived = false` — with V-time and
        // counter 0: it proves the page-publication plumbing (the guest reads a managed
        // page, not its self-seeded fallback), but it is NOT the work-derived, refreshed
        // clock AA-5 certifies. That mechanism is `hm-8h8`'s (`docs/PARAVIRT-CLOCK.md`),
        // so the spike leaves FLAG_WORK_DERIVED clear and the AA-5 floor reads unfulfilled
        // (`check_clockpage_mode`) rather than a static page passing for a real clock.
        let page = oracle_model::pvclock::materialize(0, 0, 1_000_000_000, false);
        let base = (oracle_model::PVCLOCK_GPA - RAM_BASE) as usize;

        // SAFETY: `PVCLOCK_GPA` is the second page of guest RAM, well inside the
        // `self.mem_size`-byte mapping, and the vCPU is not running.
        unsafe {
            core::ptr::copy_nonoverlapping(page.as_ptr(), self.mem.add(base), page.len());
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
            for fd in [self.vgic_fd, self.vcpu_fd, self.vm_fd, self.kvm_fd] {
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
        use std::sync::atomic::Ordering;
        loop {
            // A stock overflow can be DELIVERED — its SIGUSR1 handled, setting
            // PERF_SOURCED_KICK — in the narrow window AFTER a prior KVM_RUN already returned
            // a normal (MMIO) exit, which does not consume the flag. The one-shot counter
            // disabled itself on that overflow, so re-entering KVM_RUN would block for a second
            // signal that never comes (a lost PMI → the sample times out or records zero
            // deliveries). So consume a pending kick FIRST and surface it as the SignalKick it
            // is, before touching the ioctl. (On the patched path nothing sets this flag, so
            // this is inert there.)
            if PERF_SOURCED_KICK.swap(false, Ordering::SeqCst) {
                return Ok(VcpuExit::SignalKick);
            }
            // Arm the per-KVM_RUN watchdog: a guest that wedges (WFI with no wake, a
            // lost PMI, a livelocked exclusive) blocks the ioctl forever, and only a
            // signal can bring control back. SIGALRM after the budget makes it return
            // EINTR; below, a fired watchdog becomes RunError::Watchdog so the caller
            // records a failed attempt instead of hanging.
            if self.watchdog_secs > 0 {
                self.arm_watchdog()?;
            }
            // SAFETY: `vcpu_fd` is valid; KVM_RUN takes no argument and returns 0 or -1.
            let rc = unsafe { libc::ioctl(self.vcpu_fd, kvm::RUN as libc::c_ulong, 0_u64) };
            if rc < 0 {
                let e = errno();
                if e == libc::EINTR {
                    // The watchdog deadline fired: the vCPU stayed inside this one
                    // KVM_RUN past the budget. This is a wedge, not a delivery — never
                    // re-enter, never count it. Disarm and surface it.
                    if WATCHDOG_FIRED.swap(false, Ordering::SeqCst) {
                        self.disarm_watchdog();
                        return Err(RunError::Watchdog {
                            secs: self.watchdog_secs,
                        });
                    }
                    // A signal kicked the vCPU out — but only the stock overflow kick,
                    // sourced by the armed perf fd, is a SignalKick. An externally
                    // injected SIGUSR1 (kill/sigqueue) must NOT be counted as an
                    // overflow: it would certify a delivery the counter never made. The
                    // SA_SIGINFO handler classified the source; a foreign signal is
                    // absorbed by re-entering the guest.
                    if PERF_SOURCED_KICK.swap(false, Ordering::SeqCst) {
                        self.disarm_watchdog();
                        return Ok(VcpuExit::SignalKick);
                    }
                    continue;
                }
                self.disarm_watchdog();
                return Err(RunError::Seam {
                    context: "ioctl(KVM_RUN)",
                    message: format!("errno {e}"),
                });
            }

            // A real exit: disarm the deadline so it cannot fire during the state digest
            // and evidence assembly that follow before the next KVM_RUN.
            self.disarm_watchdog();
            // Snapshot the shared mapping once (volatile: the kernel is the external
            // writer, though it has finished by the time KVM_RUN returned and the vCPU
            // is stopped), then decode through the portable, Miri-tested seam. The
            // pointer read is here; the field logic is `super::decode_kvm_run`.
            // SAFETY: `self.run` is a live MAP_SHARED mapping of at least
            // size_of::<KvmRun>() bytes (checked at construction).
            let snapshot = unsafe { core::ptr::read_volatile(self.run) };
            return Ok(super::decode_kvm_run(&snapshot));
        }
    }

    /// Stage the value of an MMIO **read** into the shared `kvm_run.mmio.data`, so the
    /// next `KVM_RUN` resumes the guest with it. The KVM MMIO-read protocol: on a read
    /// exit the kernel expects userspace to fill the data buffer and re-enter.
    ///
    /// The bounded copy is `super::stage_mmio_read` — portable and Miri-tested; this
    /// wrapper only supplies the mapped struct and writes it back volatilely.
    fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), RunError> {
        // SAFETY: `self.run` is a live MAP_SHARED mapping of at least
        // size_of::<KvmRun>() bytes; snapshot it, stage the read into the snapshot via
        // the portable seam, and write the mmio.data bytes back. Volatile: the kernel
        // reads them on re-entry.
        unsafe {
            let mut snapshot = core::ptr::read_volatile(self.run);
            super::stage_mmio_read(&mut snapshot, data);
            let dst = (&raw mut (*self.run).mmio.data).cast::<u8>();
            for (i, b) in snapshot.mmio.data.iter().enumerate() {
                core::ptr::write_volatile(dst.add(i), *b);
            }
        }
        Ok(())
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

        let vgic = self
            .vgic_state()
            .map_err(|e| seam("ioctl(KVM_GET_DEVICE_ATTR, vGIC)", e))?;

        // SAFETY: `self.mem` is a live mapping of `self.mem_size` bytes and the vCPU
        // is not running (we are between exits), so nothing else writes it. The hashing
        // and the sorted-order discipline are the portable, Miri-tested `digest_state`.
        let ram = unsafe { core::slice::from_raw_parts(self.mem, self.mem_size) };
        Ok(super::digest_state(&regs, ram, &vgic))
    }
}

impl Machine {
    /// Dump the in-kernel vGIC's injection-relevant registers via
    /// `KVM_GET_DEVICE_ATTR`, reading **both** the redistributor (private interrupts)
    /// and the distributor (SPIs).
    ///
    /// The GICv3 split is load-bearing here: SGI/PPI (interrupt IDs 0–31) enable/
    /// pending/active state lives in the **redistributor's SGI frame**
    /// (`RD_base + 0x1_0000`), not the distributor — and the guest's timer interrupts
    /// are PPIs, so the injection state AA-6 exercises is in the redistributor. The
    /// distributor's `ISENABLER0`/`ISPENDR0`/`ISACTIVER0` are RAZ/WI on GICv3, so the
    /// distributor read starts at word 1 (SPIs, IDs 32+). Reading only the distributor
    /// words (as an earlier draft did) missed the timer-PPI and SGI state entirely, and
    /// used the wrong group number (5 is `REDIST_REGS`, not `DIST_REGS`).
    ///
    /// Both frames are read in a fixed, documented offset order so the concatenation is
    /// a stable digest input. Two AA-6 reps differing only in which interrupt is
    /// pending/active carry identical vCPU registers and RAM; this is the byte that
    /// makes that difference visible to the bit-identity floor. At AA-3 (no injection)
    /// everything here is quiescent and identical across reps, so it strengthens AA-6
    /// without disturbing AA-3's replay identity.
    fn vgic_state(&self) -> Result<Vec<u8>, SysError> {
        // No vGIC (should not happen after build, which always creates one): nothing to
        // hash, and hashing a fabricated zero would be worse than an honest empty.
        if self.vgic_fd < 0 {
            return Ok(Vec::new());
        }

        // The redistributor SGI frame sits one 64 KiB frame past RD_base on GICv3.
        const SGI_BASE: u64 = 0x1_0000;
        // Private-interrupt (SGI/PPI, IDs 0–31) state, in the redistributor. Enable,
        // pending and active alone do not determine how a pending interrupt is DELIVERED:
        // its group (secure/non-secure), priority, configuration (edge/level), and the
        // redistributor's wake state all change the injection. Two runs equal on the first
        // three but differing here would inject differently while carrying an identical
        // digest, so AA-6 replay identity must see all of it.
        let mut redist: Vec<u64> = vec![
            0x0000,            // GICR_CTLR (RD_base)
            0x0014,            // GICR_WAKER (RD_base) — wake/sleep gates delivery
            SGI_BASE + 0x0080, // GICR_IGROUPR0   — group
            SGI_BASE + 0x0D00, // GICR_IGRPMODR0  — group modifier
            SGI_BASE + 0x0100, // GICR_ISENABLER0 — enable
            SGI_BASE + 0x0200, // GICR_ISPENDR0   — pending
            SGI_BASE + 0x0300, // GICR_ISACTIVER0 — active
            SGI_BASE + 0x0C00, // GICR_ICFGR0     — config
            SGI_BASE + 0x0C04, // GICR_ICFGR1     — config
        ];
        // GICR_IPRIORITYR0..7 — one byte per private interrupt, 32 interrupts → 8 words.
        for w in 0..8u64 {
            redist.push(SGI_BASE + 0x0400 + 4 * w);
        }

        // SPI (IDs 32+) state, in the distributor, over the FULL IMPLEMENTED range. A fixed
        // 32..95 window (or IROUTER only for 32..47) let two states differing in a higher
        // SPI's routing/enable/pending produce the same digest — AA-6 replay identity passing
        // for non-equivalent machines. The implemented count comes from GICD_TYPER
        // (`32*(ITLinesNumber+1)` lines, max 1020); TYPER is hashed too, since the range is
        // itself part of the state.
        let typer = self.vgic_reg(kvm::DEV_ARM_VGIC_GRP_DIST_REGS, 0x0004)?;
        let num_lines = (32 * ((u64::from(typer) & 0x1f) + 1)).min(1020);

        let mut dist: Vec<u64> = vec![
            0x0000, // GICD_CTLR
            0x0004, // GICD_TYPER — the implemented range (state in its own right)
        ];
        // Bitmap registers: one word per 32 IDs; word n (n≥1) covers SPIs 32n..32n+31.
        for n in 1..(num_lines / 32) {
            let w = 4 * n;
            dist.push(0x0080 + w); // GICD_IGROUPR<n>   — group
            dist.push(0x0D00 + w); // GICD_IGRPMODR<n>  — group modifier
            dist.push(0x0100 + w); // GICD_ISENABLER<n> — enable
            dist.push(0x0200 + w); // GICD_ISPENDR<n>   — pending
            dist.push(0x0300 + w); // GICD_ISACTIVER<n> — active
        }
        // GICD_ICFGR: 2 bits per ID, 16 IDs per word; words 0..1 are private, 2.. are SPIs.
        for n in 2..(num_lines / 16) {
            dist.push(0x0C00 + 4 * n); // GICD_ICFGR<n> — config
        }
        // GICD_IPRIORITYR (1 byte/ID, per-word) and GICD_IROUTER (64-bit/ID), every SPI.
        for id in 32..num_lines {
            if id % 4 == 0 {
                dist.push(0x0400 + id); // GICD_IPRIORITYR word covering IDs id..id+3
            }
            dist.push(0x6000 + 8 * id); // GICD_IROUTER<id> low half
            dist.push(0x6000 + 8 * id + 4); // GICD_IROUTER<id> high half
        }

        let mut out = Vec::with_capacity((redist.len() + dist.len()) * 4);
        for &offset in &redist {
            out.extend_from_slice(
                &self
                    .vgic_reg(kvm::DEV_ARM_VGIC_GRP_REDIST_REGS, offset)?
                    .to_le_bytes(),
            );
        }
        for &offset in &dist {
            out.extend_from_slice(
                &self
                    .vgic_reg(kvm::DEV_ARM_VGIC_GRP_DIST_REGS, offset)?
                    .to_le_bytes(),
            );
        }
        Ok(out)
    }

    /// Read one 32-bit vGIC save-register through `KVM_GET_DEVICE_ATTR`.
    ///
    /// The DIST/REDIST accessors write the register value into the buffer the attr's
    /// `addr` points at. `attr` = mpidr(63:32) | offset(31:0); mpidr is 0 for the
    /// single-affinity spike guest.
    fn vgic_reg(&self, group: u32, offset: u64) -> Result<u32, SysError> {
        let mut value: u32 = 0;
        let da = KvmDeviceAttr {
            flags: 0,
            group,
            attr: offset,
            addr: (&raw mut value) as u64,
        };
        // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u32 on this frame,
        // which is what KVM_GET_DEVICE_ATTR's DIST/REDIST accessor writes.
        if unsafe {
            libc::ioctl(
                self.vgic_fd,
                kvm::GET_DEVICE_ATTR as libc::c_ulong,
                &raw const da,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_GET_DEVICE_ATTR, vGIC)"));
        }
        Ok(value)
    }

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
        // `n` is a host-supplied length and therefore untrusted. A vCPU's register
        // list is a few hundred entries; a value beyond a generous bound (or one whose
        // `+ 1` would overflow `usize`) is a malformed kernel/ABI, refused rather than
        // used to size an allocation that would abort the process.
        const MAX_REGS: u64 = 65_536;
        if n == 0 || n > MAX_REGS {
            return Err(SysError::Protocol(format!(
                "KVM_GET_REG_LIST reported {n} registers, outside the plausible bound \
                 (1..={MAX_REGS}): refusing to size an allocation on an untrusted count"
            )));
        }
        let count = usize::try_from(n).map_err(|_| {
            SysError::Protocol("KVM_GET_REG_LIST returned an implausible register count".into())
        })?;
        let buf_len = count
            .checked_add(1)
            .ok_or_else(|| SysError::Protocol("register count + 1 overflows usize".into()))?;

        // One u64 for `n`, then `count` ids.
        let mut buf: Vec<u64> = vec![0; buf_len];
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
    /// The perf configuration the manifest reports — carries the intended
    /// `sample_period` so the evidence says "this was a sampling run". The perf fd
    /// itself is opened in *counting* mode and only converted to sampling at the
    /// window mark; see [`PerfCounter::open`].
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

        if mechanism == Mechanism::Preempt {
            // Advertising the cap is not enabling it: the patch gates
            // KVM_ARM_PREEMPT_EXIT on a per-VM flag that only KVM_ENABLE_CAP sets.
            // Without this, every later arm returns EINVAL on the patched kernel.
            machine.enable_deterministic_intercepts()?;
        }

        // Open in the mode the run needs, and get the "don't overflow before the mark"
        // property from the PERIOD VALUE, not from being non-sampling:
        //
        // - A **counting** run (no targets, AA-1(b)) never arms an overflow, so it opens
        //   non-sampling (`sample_period == 0`) and just counts.
        // - An **armed** run (`--with-targets`) MUST open as a SAMPLING event, because
        //   `PERF_EVENT_IOC_PERIOD` — which `arm_overflow` uses to program the deadline
        //   at MARK_BEGIN — rejects a non-sampling event (`!is_sampling_event`, i.e.
        //   `sample_period == 0`) with `EINVAL`. Opening it non-sampling therefore breaks
        //   the very first arm on real hardware. It is opened with a period beyond any
        //   window's reach ([`PARKED_PERIOD`], `i64::MAX` — NOT `u64::MAX`, whose bit 63
        //   makes Linux EINVAL the open), so the event is sampling from the start yet does
        //   not overflow during the guest's boot; `arm_overflow` reprograms it to the
        //   real `delta` at the mark.
        let open_attr = br_retired_attr(sample_period.map(|_| PARKED_PERIOD));
        // SAFETY: `open_attr` is a fully initialised perf_event_attr on this frame.
        // Counting the calling thread (pid 0) wherever it runs (-1) — the thread is
        // pinned, so "wherever" is the one core.
        let fd = unsafe { super::imp::perf_event_open(&raw const open_attr, 0, -1, -1, 0) };
        if fd < 0 {
            return Err(err("perf_event_open(BR_RETIRED)"));
        }
        let fd = fd as i32;

        let counter = PerfCounter {
            fd,
            vcpu_fd: machine.vcpu_fd(),
            mechanism,
            // The manifest still reports the intended sampling configuration: the run
            // DOES arm overflows, at the mark. Reporting `None` here would make the
            // checker's perf/records cross-check disagree with the records.
            attr: br_retired_attr(sample_period),
        };
        counter.setup()?;
        Ok(counter)
    }

    /// Signal plumbing (stock mechanism only) and enable counting.
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

    /// A perf ioctl whose argument is an **integer** (ENABLE, RESET, REFRESH).
    fn ioctl(&self, request: u64, arg: u64, call: &'static str) -> Result<(), SysError> {
        // SAFETY: `self.fd` is a valid perf event descriptor; the requests used here
        // take an integer argument.
        if unsafe { libc::ioctl(self.fd, request as libc::c_ulong, arg) } < 0 {
            return Err(err(call));
        }
        Ok(())
    }

    /// Program the sampling period. `PERF_EVENT_IOC_PERIOD` is an `_IOW` whose third
    /// argument must **point at** a `u64`; passing the value directly makes the
    /// kernel treat the deadline as a userspace address and return `EFAULT`, so no
    /// overflow is ever armed.
    fn set_period(&self, period: u64) -> Result<(), SysError> {
        // SAFETY: `self.fd` is valid; `&period` points at a live u64 for the call,
        // which is exactly what PERF_EVENT_IOC_PERIOD's contract requires.
        if unsafe { libc::ioctl(self.fd, PERF_IOC_PERIOD as libc::c_ulong, &raw const period) } < 0
        {
            return Err(err("PERF_EVENT_IOC_PERIOD"));
        }
        Ok(())
    }

    /// Re-issue the patch's one-shot vCPU force-exit. The kernel clears
    /// `preempt_armed` when it fires (on any IRQ), so an advisory exit leaves it
    /// disarmed even though the perf overflow has not happened yet.
    fn arm_preempt_exit(&self) -> Result<(), RunError> {
        // SAFETY: `vcpu_fd` is the borrowed machine's valid vCPU descriptor;
        // KVM_ARM_PREEMPT_EXIT takes no argument.
        if unsafe { libc::ioctl(self.vcpu_fd, kvm::ARM_PREEMPT_EXIT as libc::c_ulong, 0_u64) } < 0 {
            return Err(seam(
                "ioctl(KVM_ARM_PREEMPT_EXIT)",
                err("ioctl(KVM_ARM_PREEMPT_EXIT)"),
            ));
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

    /// Arm a one-shot overflow `delta` events from now (i.e. from `MARK_BEGIN`),
    /// through the mechanism this counter was opened for — and only that one.
    fn arm_overflow(&mut self, delta: u64) -> Result<(), RunError> {
        // Program the deadline (by pointer), then REFRESH(1): exactly one overflow,
        // after which the event disables itself. A free-running overflow would
        // deliver an unbounded number of kicks and make per-record multiplicity
        // meaningless.
        self.set_period(delta)
            .map_err(|e| seam("PERF_EVENT_IOC_PERIOD", e))?;
        self.ioctl(PERF_IOC_REFRESH, 1, "PERF_EVENT_IOC_REFRESH")
            .map_err(|e| seam("PERF_EVENT_IOC_REFRESH", e))?;

        if self.mechanism == Mechanism::Preempt {
            // Arm the patch's one-shot in-kernel force-exit. Without this the overflow
            // IRQ is handled and the guest is re-entered — the exit never comes.
            self.arm_preempt_exit()?;
        }
        Ok(())
    }

    /// Re-arm after an advisory exit — one that fired before the counter reached the
    /// target (the arm64 "any IRQ" behaviour).
    ///
    /// The perf one-shot is untouched (it only counts real overflows, and no overflow
    /// happened), so this re-arms only what the kernel cleared: the patch's
    /// `preempt_armed` flag. For the stock signal path there is nothing to re-arm —
    /// the perf event is still armed and the next real overflow will signal.
    fn rearm(&mut self) -> Result<(), RunError> {
        if self.mechanism == Mechanism::Preempt {
            self.arm_preempt_exit()?;
        }
        Ok(())
    }

    /// Resume plain counting after a delivery, so the counter advances to `MARK_END`
    /// and `work_end` is the window's true end rather than the landing.
    ///
    /// The one-shot REFRESH disabled the event when it overflowed. The period cannot
    /// be set to zero (the kernel rejects it) or to `u64::MAX` (bit 63 → EINVAL), so it
    /// is set to [`PARKED_PERIOD`] — beyond any window's reach yet accepted — the count
    /// keeps advancing, and no further overflow fires before `MARK_END`, which is what
    /// stops a post-landing tick from being recorded as a second delivery.
    fn resume_counting(&mut self) -> Result<(), RunError> {
        self.set_period(PARKED_PERIOD)
            .map_err(|e| seam("PERF_EVENT_IOC_PERIOD (resume)", e))?;
        self.ioctl(PERF_IOC_ENABLE, 0, "PERF_EVENT_IOC_ENABLE (resume)")
            .map_err(|e| seam("PERF_EVENT_IOC_ENABLE (resume)", e))?;
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

/// AA-0 `vgicv3-creatable`: can an in-kernel GICv3 be created on this host?
///
/// A fresh VM and `KVM_CREATE_DEVICE(KVM_DEV_TYPE_ARM_VGIC_V3)` is the direct truth of
/// the row — the very device [`Machine::create_vgic`] needs, without which no payload
/// boots (its GIC stores become MMIO exits the measurement loop refuses). A kernel or
/// CPU without GICv3 support fails the creation with `ENODEV`/`EINVAL`/`EOPNOTSUPP` (a
/// clean "no"); any other errno is a failure to *probe*, not a "no", and must not be
/// flattened into one.
///
/// **Untested on silicon.**
///
/// # Errors
/// [`SysError`] if the probe could not be issued.
pub fn probe_vgicv3_creatable() -> Result<bool, SysError> {
    let kvm_fd = open_kvm()?;
    // SAFETY: `kvm_fd` is a valid /dev/kvm descriptor; KVM_CREATE_VM takes a machine
    // type (0 = default) and returns a VM fd.
    let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
    if vm_fd < 0 {
        let e = err("ioctl(KVM_CREATE_VM)");
        // SAFETY: `kvm_fd` is valid and owned here.
        unsafe { libc::close(kvm_fd) };
        return Err(e);
    }
    let mut dev = KvmCreateDevice {
        type_: kvm::DEV_TYPE_ARM_VGIC_V3,
        fd: 0,
        flags: 0,
    };
    // SAFETY: `vm_fd` is valid; KVM_CREATE_DEVICE fills `dev.fd` on success.
    let rc = unsafe { libc::ioctl(vm_fd, kvm::CREATE_DEVICE as libc::c_ulong, &raw mut dev) };
    let out = if rc < 0 {
        let e = errno();
        if e == libc::ENODEV || e == libc::EINVAL || e == libc::EOPNOTSUPP {
            Ok(false)
        } else {
            Err(err("ioctl(KVM_CREATE_DEVICE, vGICv3)"))
        }
    } else {
        // SAFETY: the device fd the kernel returned is valid and owned here.
        unsafe { libc::close(dev.fd as i32) };
        Ok(true)
    };
    // SAFETY: both descriptors are valid and owned here.
    unsafe {
        libc::close(vm_fd);
        libc::close(kvm_fd);
    }
    out
}

/// AA-0 `writable-id-registers`: will the kernel accept a write to a guest ID register?
///
/// The determinism model needs the guest's feature ID registers pinned to a controlled
/// value (AA-6(a) installs a synthetic ID-register model); a kernel that treats them as
/// strictly read-only cannot do that. The probe creates a VM + vCPU, `KVM_ARM_VCPU_INIT`s
/// it, reads `ID_AA64PFR0_EL1` with `KVM_GET_ONE_REG`, then writes the **same** value
/// back with `KVM_SET_ONE_REG`: a writable-ID kernel accepts the identity write, a
/// read-only one fails it with `EINVAL`. Writing back the value just read cannot change
/// any guest-visible feature, so the probe tests *writability* only — it does not smuggle
/// in a feature change.
///
/// **Untested on silicon.**
///
/// # Errors
/// [`SysError`] if the probe could not be issued.
pub fn probe_writable_id_registers() -> Result<bool, SysError> {
    let kvm_fd = open_kvm()?;
    // SAFETY: valid /dev/kvm fd; KVM_CREATE_VM returns a VM fd.
    let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
    if vm_fd < 0 {
        let e = err("ioctl(KVM_CREATE_VM)");
        // SAFETY: `kvm_fd` is valid and owned here.
        unsafe { libc::close(kvm_fd) };
        return Err(e);
    }

    // Run the fd-owning body, then close the VM and /dev/kvm fds on every path.
    let out = probe_writable_id_registers_on_vm(vm_fd);
    // SAFETY: both descriptors are valid and owned here.
    unsafe {
        libc::close(vm_fd);
        libc::close(kvm_fd);
    }
    out
}

/// The vCPU-owning body of [`probe_writable_id_registers`], split out so the vCPU fd is
/// closed on every exit path while the caller owns the VM/`/dev/kvm` fds.
fn probe_writable_id_registers_on_vm(vm_fd: libc::c_int) -> Result<bool, SysError> {
    // SAFETY: `vm_fd` is valid; KVM_CREATE_VCPU takes a vcpu index and returns a fd.
    let vcpu_fd = unsafe { libc::ioctl(vm_fd, kvm::CREATE_VCPU as libc::c_ulong, 0_u64) };
    if vcpu_fd < 0 {
        return Err(err("ioctl(KVM_CREATE_VCPU)"));
    }
    let out = probe_writable_id_registers_on_vcpu(vm_fd, vcpu_fd);
    // SAFETY: `vcpu_fd` is valid and owned here.
    unsafe { libc::close(vcpu_fd) };
    out
}

fn probe_writable_id_registers_on_vcpu(
    vm_fd: libc::c_int,
    vcpu_fd: libc::c_int,
) -> Result<bool, SysError> {
    // arm64 requires the vCPU be initialised against the host's preferred target before
    // any register can be read or written.
    let mut init = KvmVcpuInit::default();
    // SAFETY: `vm_fd` is valid; KVM_ARM_PREFERRED_TARGET fills `init`.
    if unsafe {
        libc::ioctl(
            vm_fd,
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
            vcpu_fd,
            kvm::ARM_VCPU_INIT as libc::c_ulong,
            &raw const init,
        )
    } < 0
    {
        return Err(err("ioctl(KVM_ARM_VCPU_INIT)"));
    }

    // Read the current (host) ID_AA64PFR0_EL1.
    let mut orig: u64 = 0;
    let get = KvmOneReg {
        id: kvm::REG_ID_AA64PFR0_EL1,
        addr: (&raw mut orig) as u64,
    };
    // SAFETY: `vcpu_fd` is valid; `get.addr` points at a live u64 the kernel writes.
    if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get) } < 0 {
        return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64PFR0_EL1)"));
    }

    // AA-6 installs a BELOW-HOST synthetic feature model, so the row is about writing a
    // CHANGED, reduced feature value — NOT an identity write. Some KVM versions accept an
    // identity `SET_ONE_REG` (for migration compatibility) while rejecting any changed
    // invariant/ID value, so writing the value just read would false-green this mandatory
    // row. Instead, reduce one feature nibble by 1, write it, and READ IT BACK: the row is
    // TRUE only if a reduced value is both accepted AND observed. The exception-level fields
    // (bits[15:0], nibbles 0..4) are skipped — lowering EL support breaks the VM — as are
    // absent (0) or not-implemented (0xF) fields, which cannot be cleanly lowered.
    for nibble in 4..16u32 {
        let shift = nibble * 4;
        let field = (orig >> shift) & 0xF;
        if field == 0 || field == 0xF {
            continue;
        }
        let reduced = (orig & !(0xFu64 << shift)) | ((field - 1) << shift);
        let set = KvmOneReg {
            id: kvm::REG_ID_AA64PFR0_EL1,
            addr: (&raw const reduced) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `set.addr` points at a live u64 the kernel reads.
        let rc = unsafe { libc::ioctl(vcpu_fd, kvm::SET_ONE_REG as libc::c_ulong, &raw const set) };
        if rc < 0 {
            let e = errno();
            // This field is read-only on this kernel — try another. A non-{EINVAL,EPERM,
            // ENOENT} errno is a failure to probe, not a clean "no".
            if e == libc::EINVAL || e == libc::EPERM || e == libc::ENOENT {
                continue;
            }
            return Err(err("ioctl(KVM_SET_ONE_REG, ID_AA64PFR0_EL1)"));
        }
        // The SET was accepted — confirm the reduction actually took, rather than being
        // silently clamped back to the host value (accepting the ioctl but ignoring the
        // change is exactly the identity-write false-green this probe exists to defeat).
        let mut readback: u64 = 0;
        let get2 = KvmOneReg {
            id: kvm::REG_ID_AA64PFR0_EL1,
            addr: (&raw mut readback) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `get2.addr` points at a live u64 the kernel writes.
        if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get2) } < 0 {
            return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64PFR0_EL1 readback)"));
        }
        if readback == reduced {
            return Ok(true);
        }
        // Accepted but unchanged: not a real feature write. Try the next field.
    }
    // No feature field could be reduced and read back: this kernel does not accept a
    // below-host ID-register model, so AA-6's synthetic feature install is impossible here.
    Ok(false)
}

/// The host feature ID registers a guest would see, read from a disposable VM's vCPU — the
/// values AA-0's `ecv`/`lse`/`pmuver`/`sve`/`nested-virt` rows and the `identity` block are
/// derived from. **Untested on silicon.**
#[derive(Clone, Copy, Debug)]
pub struct HostIdRegisters {
    /// `MIDR_EL1`.
    pub midr: u64,
    /// `ID_AA64ISAR0_EL1` (Atomic/LSE in bits[23:20]).
    pub id_aa64isar0: u64,
    /// `ID_AA64MMFR0_EL1` (ECV in bits[63:60]).
    pub id_aa64mmfr0: u64,
    /// `ID_AA64MMFR1_EL1` (VH/VHE in bits[11:8]).
    pub id_aa64mmfr1: u64,
    /// `ID_AA64MMFR2_EL1` (NV / nested virt in bits[35:32]).
    pub id_aa64mmfr2: u64,
    /// `ID_AA64DFR0_EL1` (PMUVer in bits[11:8]).
    pub id_aa64dfr0: u64,
    /// `ID_AA64PFR0_EL1` (SVE in bits[35:32]).
    pub id_aa64pfr0: u64,
}

/// Read [`HostIdRegisters`] from a fresh, disposable VM + vCPU.
///
/// # Errors
/// [`SysError`] if the VM/vCPU could not be created/initialised or a register could not be
/// read.
pub fn read_host_id_registers() -> Result<HostIdRegisters, SysError> {
    let kvm_fd = open_kvm()?;
    // SAFETY: `kvm_fd` is a valid /dev/kvm descriptor; KVM_CREATE_VM returns a VM fd.
    let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
    if vm_fd < 0 {
        let e = err("ioctl(KVM_CREATE_VM)");
        // SAFETY: `kvm_fd` is valid and owned here.
        unsafe { libc::close(kvm_fd) };
        return Err(e);
    }
    let out = read_host_id_registers_on_vm(vm_fd);
    // SAFETY: both descriptors are valid and owned here.
    unsafe {
        libc::close(vm_fd);
        libc::close(kvm_fd);
    }
    out
}

fn read_host_id_registers_on_vm(vm_fd: libc::c_int) -> Result<HostIdRegisters, SysError> {
    // SAFETY: `vm_fd` is valid; KVM_CREATE_VCPU takes a vcpu index and returns a fd.
    let vcpu_fd = unsafe { libc::ioctl(vm_fd, kvm::CREATE_VCPU as libc::c_ulong, 0_u64) };
    if vcpu_fd < 0 {
        return Err(err("ioctl(KVM_CREATE_VCPU)"));
    }
    let out = read_host_id_registers_on_vcpu(vm_fd, vcpu_fd);
    // SAFETY: `vcpu_fd` is valid and owned here.
    unsafe { libc::close(vcpu_fd) };
    out
}

fn read_host_id_registers_on_vcpu(
    vm_fd: libc::c_int,
    vcpu_fd: libc::c_int,
) -> Result<HostIdRegisters, SysError> {
    let mut init = KvmVcpuInit::default();
    // SAFETY: `vm_fd` is valid; KVM_ARM_PREFERRED_TARGET fills `init`.
    if unsafe {
        libc::ioctl(
            vm_fd,
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
            vcpu_fd,
            kvm::ARM_VCPU_INIT as libc::c_ulong,
            &raw const init,
        )
    } < 0
    {
        return Err(err("ioctl(KVM_ARM_VCPU_INIT)"));
    }

    let read = |id: u64, call: &'static str| -> Result<u64, SysError> {
        let mut value: u64 = 0;
        let get = KvmOneReg {
            id,
            addr: (&raw mut value) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `get.addr` points at a live u64 the kernel writes.
        if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get) } < 0 {
            return Err(err(call));
        }
        Ok(value)
    };

    Ok(HostIdRegisters {
        midr: read(kvm::REG_MIDR_EL1, "GET_ONE_REG(MIDR_EL1)")?,
        id_aa64isar0: read(kvm::REG_ID_AA64ISAR0_EL1, "GET_ONE_REG(ID_AA64ISAR0_EL1)")?,
        id_aa64mmfr0: read(kvm::REG_ID_AA64MMFR0_EL1, "GET_ONE_REG(ID_AA64MMFR0_EL1)")?,
        id_aa64mmfr1: read(kvm::REG_ID_AA64MMFR1_EL1, "GET_ONE_REG(ID_AA64MMFR1_EL1)")?,
        id_aa64mmfr2: read(kvm::REG_ID_AA64MMFR2_EL1, "GET_ONE_REG(ID_AA64MMFR2_EL1)")?,
        id_aa64dfr0: read(kvm::REG_ID_AA64DFR0_EL1, "GET_ONE_REG(ID_AA64DFR0_EL1)")?,
        id_aa64pfr0: read(kvm::REG_ID_AA64PFR0_EL1, "GET_ONE_REG(ID_AA64PFR0_EL1)")?,
    })
}
