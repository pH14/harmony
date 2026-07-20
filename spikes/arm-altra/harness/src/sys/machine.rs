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

use sha2::{Digest, Sha256};

use super::{
    ExecGuardExit, ExecGuardPageAudit, ExecGuardStats, KvmRun, PerfEventAttr, SysError,
    br_retired_attr, kvm,
};
use crate::linux_console::{LinuxClockeventState, LinuxPvclockVcpu, PvclockWrite};
use crate::run::{RunError, StepVcpu, Vcpu, VcpuExit, WorkCounter};

/// Guest RAM base — the QEMU `virt` / Altra map the payload runtime is linked for
/// (`payloads/linker.ld`: params page at `0x4000_0000`, image at `+512 KiB`).
pub const RAM_BASE: u64 = crate::linux_boot::RAM_BASE;

/// How much guest RAM the payloads need: the image loads 512 KiB in and its whole
/// footprint (code + rodata + data + bss + `__stack_top`, `payloads/linker.ld`) plus
/// the two harness pages live under ~1.5 MiB, so 4 MiB is an 8× margin.
///
/// This was `64 << 20` in the offline apparatus, whose comment assumed the slot could
/// be "hashed cheaply" for the state digest. AA-1(c) on real N1 measured that wrong:
/// `state_digest` reads the **whole** slot every sample, and faulting-in + hashing 64
/// MiB of freshly-`mmap`ed anonymous memory is ~0.45 s/sample — memory-bound, so
/// hardware SHA-256 codegen (`target-cpu=native`) did not move it. At 10⁶ armed
/// overflows that is ~5 days on one pinned core, and the aggregation rule forbids
/// spreading the four contamination conditions across cores. Shrinking the slot is the
/// only effective lever, and it is **evidence-preserving**: the 60+ MiB tail is
/// provably always-zero (no payload touches it — the ELF loader fails closed with
/// `RangeNotMapped` on any segment past the slot, and a guest write past the mapping
/// faults rather than corrupting silently), so hashing it adds no divergence-detection
/// power. The digest still covers every byte of guest state a payload can reach; only
/// its length (and thus its hex value) changes, and digests are compared only WITHIN a
/// run-set (replay identity), never across run-sets or against a golden. No measured
/// count, overflow, or skid is affected. Recorded as a SPIKE(arm-altra) apparatus
/// change in the AA-1(c) disposition. If AA-5's Linux guest (not yet built) needs more,
/// it takes its own larger slot; nothing in the bare-metal payload path exceeds this.
pub const RAM_SIZE: usize = 4 << 20;

/// Reserved digest-record id for the VM-owned Linux pvclock registration.
///
/// KVM register ids always carry a real architecture in their high byte; all ones is outside
/// that namespace. The insertion still fails closed on a collision so a future ABI extension
/// cannot silently alias device state with an architectural register.
const LINUX_PVCLOCK_DIGEST_STATE_ID: u64 = u64::MAX;
const LINUX_PVCLOCK_DIGEST_STATE_TAG: &[u8] = b"harmony-pvclock-v1";
/// Reserved digest-record id for the userspace-owned ARM clockevent input state.
const LINUX_CLOCKEVENT_DIGEST_STATE_ID: u64 = u64::MAX - 1;
const LINUX_CLOCKEVENT_DIGEST_STATE_TAG: &[u8] = b"harmony-clockevent-v1";
const KVM_ARM_IRQ_TYPE_PPI: u32 = 2;
const KVM_ARM_IRQ_TYPE_SHIFT: u32 = 24;
const HARMONY_CLOCKEVENT_IRQ: u32 =
    (KVM_ARM_IRQ_TYPE_PPI << KVM_ARM_IRQ_TYPE_SHIFT) | crate::linux_boot::HARMONY_CLOCKEVENT_PPI;
const HARMONY_CLOCKEVENT_LINE_MASK: u32 = 1 << crate::linux_boot::HARMONY_CLOCKEVENT_PPI;
/// vCPU0 MPIDR 0 | `VGIC_LEVEL_INFO_LINE_LEVEL` 0 | vINTID block base 0.
const HARMONY_CLOCKEVENT_LEVEL_INFO_ATTR: u64 = kvm::VGIC_LEVEL_INFO_LINE_LEVEL;
const _: () = assert!(HARMONY_CLOCKEVENT_IRQ == 0x0200_0014);
const _: () = assert!(HARMONY_CLOCKEVENT_LEVEL_INFO_ATTR == 0);

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

/// `struct kvm_irq_level` for `KVM_IRQ_LINE`.
#[repr(C)]
#[derive(Clone, Copy)]
struct KvmIrqLevel {
    irq: u32,
    level: u32,
}
const _: () = assert!(core::mem::size_of::<KvmIrqLevel>() == 8);

/// `struct kvm_arm_stage2_exec_guard`, the exact-generation VM-ioctl response.
#[repr(C)]
#[derive(Clone, Copy)]
struct KvmArmStage2ExecGuard {
    gpa: u64,
    generation: u64,
    action: u32,
    flags: u32,
}
const _: () = assert!(core::mem::size_of::<KvmArmStage2ExecGuard>() == 24);
const _: () = assert!(super::EXEC_GUARD_STALE_ERRNO == libc::EINVAL);

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

/// The arm64 `struct kvm_guest_debug_arch`: the hardware breakpoint/watchpoint control and
/// value registers. AA-2's single-step arms `SINGLESTEP` only, so this stays all-zero (no
/// hardware breakpoints programmed) — but it MUST be present and correctly sized, because it is
/// what makes `struct kvm_guest_debug` 0x208 bytes on arm64, and the ioctl number encodes that
/// size (`KVM_ARM_MAX_DBG_REGS == 16`, `arch/arm64/include/uapi/asm/kvm.h`).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct KvmGuestDebugArch {
    dbg_bcr: [u64; 16],
    dbg_bvr: [u64; 16],
    dbg_wcr: [u64; 16],
    dbg_wvr: [u64; 16],
}

/// `struct kvm_guest_debug` (arm64): `control` + `pad` + the arch breakpoint block.
///
/// 0x208 bytes on arm64, which the `KVM_SET_GUEST_DEBUG` ioctl number encodes — pinned by
/// [`tests::kvm_guest_debug_is_the_arm64_abi_size`].
#[repr(C)]
#[derive(Default)]
struct KvmGuestDebug {
    control: u32,
    pad: u32,
    arch: KvmGuestDebugArch,
}

// The arm64 ABI sizes, pinned at compile time: `kvm_guest_debug_arch` is 64×u64 = 0x200, and
// `kvm_guest_debug` (control + pad + arch) is 0x208 — the size the `KVM_SET_GUEST_DEBUG` ioctl
// number encodes (`super::kvm::SET_GUEST_DEBUG == _IOW(0x9b, 0x208)`). A struct-shuffle that
// broke either would send a differently-numbered ioctl the kernel does not recognise.
const _: () = assert!(core::mem::size_of::<KvmGuestDebugArch>() == 0x200);
const _: () = assert!(core::mem::size_of::<KvmGuestDebug>() == 0x208);

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

fn map_guest_ram(len: usize) -> Result<*mut u8, SysError> {
    // SAFETY: a fresh anonymous private mapping; `len` is a VMM-owned board constant.
    let mem = unsafe {
        libc::mmap(
            core::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if mem == libc::MAP_FAILED {
        return Err(err("mmap(guest RAM)"));
    }
    Ok(mem.cast::<u8>())
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
    /// Start churning `tid`'s affinity across `cores` until dropped or
    /// [`MigrationChurner::stop`]ped.
    ///
    /// # Errors
    /// [`SysError::Protocol`] on an empty core list (the modulo below would panic) or any
    /// core at/past `CPU_SETSIZE` (libc's `CPU_SET` panics rather than erroring). Callers
    /// normally pass [`allowed_cores`] output, but library code must not panic on a caller
    /// contract violation.
    pub fn start(tid: libc::pid_t, cores: Vec<u32>) -> Result<MigrationChurner, SysError> {
        if cores.is_empty() {
            return Err(SysError::Protocol(
                "migration churner needs a non-empty core list".to_owned(),
            ));
        }
        let cpu_setsize = core::mem::size_of::<libc::cpu_set_t>() * 8;
        if let Some(bad) = cores.iter().find(|&&c| (c as usize) >= cpu_setsize) {
            return Err(SysError::Protocol(format!(
                "churner core {bad} is at or past CPU_SETSIZE ({cpu_setsize}): out of range \
                 for an affinity mask"
            )));
        }
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
        Ok(MigrationChurner {
            stop,
            moves,
            handle: Some(handle),
        })
    }

    /// How many affinity moves the churner has successfully issued so far.
    #[must_use]
    pub fn moves(&self) -> u64 {
        self.moves.load(Ordering::Relaxed)
    }

    /// A clonable handle to the live move counter, so the run loop can bound a move to a
    /// sample's armed interval (`arm_overflow` → landing) rather than the whole sample. See
    /// [`crate::run::ArmedMigrationProbe`].
    #[must_use]
    pub fn moves_handle(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.moves)
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
        // Only a perf-sourced signal SETS the pending kick; a foreign SIGUSR1 must NOT clear
        // an already-pending `true`. The run loop is the sole consumer (it swaps the flag to
        // false). Storing `from_fd` unconditionally would let a foreign signal racing in after
        // a real overflow erase it — the EINTR is then absorbed and KVM_RUN re-entered with the
        // one-shot perf event already disabled, producing a hang or an apparent lost PMI.
        if from_fd {
            PERF_SOURCED_KICK.store(true, Ordering::SeqCst);
        }
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
    /// VM-lifetime one-shot pvclock registration. `None` for bare payloads and before the
    /// owned Linux guest's validated MMIO write.
    linux_pvclock_gpa: Option<u64>,
    /// `Some` only for the owned Linux board. The external level is VM state that KVM's
    /// register dump does not necessarily expose, so it is retained and digest-bound here.
    linux_clockevent: Option<LinuxClockeventState>,
    /// Present only for the guarded constructors. Counts every hidden synchronous
    /// guard transition so a live proof cannot pass without exercising the mechanism.
    exec_guard: Option<ExecGuardStats>,
    /// Optional one-page trace used by the planted self-modification proof. It is
    /// configured before the first vCPU entry and remains fixed for the VM lifetime.
    exec_guard_page_audit: Option<ExecGuardPageAudit>,
    /// Proof-only target on whose post-write scan one superseded generation is replayed.
    exec_guard_stale_probe_gpa: Option<u64>,
    /// The generation of the most recent execute-scan the guard serviced (0 before any).
    /// Used by the notifier-replacement proof to show a memslot update forced a fresh scan
    /// at a strictly newer generation.
    exec_guard_last_scan_generation: u64,
    /// Per-`KVM_RUN` watchdog budget in seconds; 0 disables it. See
    /// [`DEFAULT_WATCHDOG_SECS`].
    watchdog_secs: u64,
}

/// Bound transparent guard exits inside one [`Vcpu::run`] call. A guest that endlessly
/// rewrites and re-executes pages must surface as a failure instead of hiding an unbounded
/// exit loop below the caller's own exit budget.
const MAX_EXEC_GUARD_EXITS_PER_RUN: u64 = 1_000_000;

enum GuestBoot<'a> {
    Payload {
        image: &'a crate::elf::Elf,
        params: &'a ParamsPage,
    },
    Linux {
        image: &'a [u8],
        initramfs: &'a [u8],
        bootargs: &'a str,
    },
}

impl GuestBoot<'_> {
    fn ram_size(&self) -> usize {
        match self {
            Self::Payload { .. } => RAM_SIZE,
            Self::Linux { .. } => crate::linux_boot::RAM_SIZE,
        }
    }

    fn needs_psci_0_2(&self) -> bool {
        matches!(self, Self::Linux { .. })
    }
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
        Self::new_for(GuestBoot::Payload { image, params }, false)
    }

    /// Create a bare-payload VM with the AA-4 stage-2 execute guard enabled before
    /// vCPU creation. Used by the planted clean/exclusive runtime proof.
    ///
    /// # Errors
    /// [`SysError`] if the patched capability is absent, cannot be enabled, or construction fails.
    pub fn new_guarded(image: &crate::elf::Elf, params: &ParamsPage) -> Result<Machine, SysError> {
        Self::new_for(GuestBoot::Payload { image, params }, true)
    }

    /// Create the AA-5(c) Linux board, validate and load its flat Image,
    /// initramfs, and generated DTB, then establish the arm64 Linux entry state.
    ///
    /// This constructor supplies boot plumbing only. The page remains zero until the guest's
    /// one-shot registration is followed by an exact-work landing, so callers cannot treat a
    /// successful construction as AA-5 certification.
    ///
    /// # Errors
    /// [`SysError`] if an artifact is malformed or any KVM operation fails.
    pub fn new_linux(image: &[u8], initramfs: &[u8], bootargs: &str) -> Result<Machine, SysError> {
        Self::new_for(
            GuestBoot::Linux {
                image,
                initramfs,
                bootargs,
            },
            false,
        )
    }

    /// Create the owned Linux board with default-XN execute mediation enabled before
    /// vCPU creation. Guard exits are scanned and serviced transparently by [`Machine::run`].
    ///
    /// # Errors
    /// [`SysError`] if the patched capability is absent, cannot be enabled, or construction fails.
    pub fn new_linux_guarded(
        image: &[u8],
        initramfs: &[u8],
        bootargs: &str,
    ) -> Result<Machine, SysError> {
        Self::new_for(
            GuestBoot::Linux {
                image,
                initramfs,
                bootargs,
            },
            true,
        )
    }

    fn new_for(boot: GuestBoot<'_>, exec_guard: bool) -> Result<Machine, SysError> {
        let kvm_fd = open_kvm()?;
        let linux_clockevent = boot
            .needs_psci_0_2()
            .then_some(LinuxClockeventState::default());
        let mut m = Machine {
            kvm_fd,
            vm_fd: -1,
            vcpu_fd: -1,
            vgic_fd: -1,
            run: core::ptr::null_mut(),
            run_size: 0,
            mem: core::ptr::null_mut(),
            mem_size: 0,
            linux_pvclock_gpa: None,
            linux_clockevent,
            exec_guard: exec_guard.then_some(ExecGuardStats::default()),
            exec_guard_page_audit: None,
            exec_guard_stale_probe_gpa: None,
            exec_guard_last_scan_generation: 0,
            watchdog_secs: DEFAULT_WATCHDOG_SECS,
        };
        m.build(boot)?;
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
    fn build(&mut self, boot: GuestBoot<'_>) -> Result<(), SysError> {
        let needs_psci_0_2 = boot.needs_psci_0_2();
        // SAFETY: `kvm_fd` is a valid /dev/kvm descriptor. KVM_CREATE_VM takes a
        // machine type (0 = default) and returns a VM fd.
        self.vm_fd = unsafe { libc::ioctl(self.kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
        if self.vm_fd < 0 {
            return Err(err("ioctl(KVM_CREATE_VM)"));
        }

        // The kernel contract requires the guard opt-in before any vCPU exists.
        // Enabling it here also makes the VMM's controlled boundary explicit: this
        // board has one anonymous private slot and no assigned/DMA-capable device.
        if self.exec_guard.is_some() {
            self.enable_stage2_exec_guard()?;
        }

        let ram_size = boot.ram_size();

        // Guest RAM: one anonymous mapping, one memory slot.
        self.mem = map_guest_ram(ram_size)?;
        self.mem_size = ram_size;

        let memory_size = u64::try_from(ram_size)
            .map_err(|_| SysError::Protocol("guest RAM size does not fit u64".into()))?;

        let region = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size,
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
        let forbidden_timer_owners =
            (1 << kvm::VCPU_FEATURE_PMU_V3) | (1 << kvm::VCPU_FEATURE_HAS_EL2);
        if init.features[0] & forbidden_timer_owners != 0 {
            return Err(SysError::Protocol(format!(
                "KVM preferred target unexpectedly enables PMU/nested feature bits {:#x}; \
                 dedicated PPI 20 ownership is no longer proven",
                init.features[0] & forbidden_timer_owners
            )));
        }
        // The generated Linux DTB advertises PSCI 0.2 over HVC. Without this
        // feature bit KVM exposes only legacy PSCI 0.1, so standardized calls
        // such as SYSTEM_OFF/RESET are reported unsupported. Preserve the
        // historical bare-payload init bitmap byte-for-byte; only Linux opts in.
        if needs_psci_0_2 {
            init.features[0] |= 1 << kvm::ARM_VCPU_PSCI_0_2;
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

        match boot {
            GuestBoot::Payload { image, params } => {
                self.load_image(image)?;
                self.write_params(params);
                self.publish_pvclock_page();
                self.set_pc(image.entry())?;
            }
            GuestBoot::Linux {
                image,
                initramfs,
                bootargs,
            } => {
                // SAFETY: `self.mem` is the unique writable `ram_size`-byte mmap
                // established above, and the vCPU has not run yet.
                let ram = unsafe { core::slice::from_raw_parts_mut(self.mem, self.mem_size) };
                let loaded = crate::linux_boot::load(image, initramfs, bootargs, ram)
                    .map_err(|e| SysError::Protocol(format!("Linux boot layout: {e}")))?;
                self.publish_pvclock_page();
                self.set_linux_entry(loaded.entry_gpa, loaded.dtb_gpa)?;
            }
        }

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

    /// Enable the AA-4 execute guard before vCPU creation, refusing an absent marker
    /// rather than letting an unknown enable-cap failure masquerade as patched coverage.
    fn enable_stage2_exec_guard(&self) -> Result<(), SysError> {
        // SAFETY: `vm_fd` is valid and KVM_CHECK_EXTENSION takes the scalar cap number.
        let present = unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::CHECK_EXTENSION as libc::c_ulong,
                kvm::CAP_ARM_STAGE2_EXEC_GUARD,
            )
        };
        if present < 0 {
            return Err(err("ioctl(KVM_CHECK_EXTENSION, ARM_STAGE2_EXEC_GUARD)"));
        }
        if present == 0 {
            return Err(SysError::Protocol(
                "the AA-4 stage-2 execute guard was requested but the running kernel does not \
                 advertise KVM_CAP_ARM_STAGE2_EXEC_GUARD"
                    .into(),
            ));
        }

        let cap = KvmEnableCap {
            cap: kvm::CAP_ARM_STAGE2_EXEC_GUARD as u32,
            ..Default::default()
        };
        // SAFETY: `vm_fd` is valid; `cap` is fully initialized and no vCPU exists yet.
        if unsafe { libc::ioctl(self.vm_fd, kvm::ENABLE_CAP as libc::c_ulong, &raw const cap) } < 0
        {
            return Err(err("ioctl(KVM_ENABLE_CAP, ARM_STAGE2_EXEC_GUARD)"));
        }
        Ok(())
    }

    /// Counts proving which execute-guard transitions this VM actually exercised.
    #[must_use]
    pub fn exec_guard_stats(&self) -> Option<ExecGuardStats> {
        self.exec_guard
    }

    /// Select one guarded RAM page for bounded write/rescan audit before first entry.
    ///
    /// # Errors
    /// [`SysError::Protocol`] if this is not a guarded VM, any guard exit already
    /// occurred, or `gpa` is not one complete page in the single RAM slot.
    pub fn audit_exec_guard_page(&mut self, gpa: u64) -> Result<(), SysError> {
        let Some(stats) = self.exec_guard else {
            return Err(SysError::Protocol(
                "cannot audit an execute-guard page on an unguarded VM".into(),
            ));
        };
        if stats.exits != 0 {
            return Err(SysError::Protocol(
                "execute-guard page audit must be selected before first vCPU entry".into(),
            ));
        }

        // SAFETY: construction completed, the vCPU has never entered, and this machine
        // exclusively owns the complete live RAM mapping.
        let ram = unsafe { super::guest_ram(self.mem, self.mem_size) };
        let page = super::exec_guard_page(ram, RAM_BASE, gpa).ok_or_else(|| {
            SysError::Protocol(format!(
                "execute-guard audit page {gpa:#x} is not one aligned page inside [{:#x}, {:#x})",
                RAM_BASE,
                RAM_BASE.saturating_add(self.mem_size as u64)
            ))
        })?;
        self.exec_guard_page_audit = Some(ExecGuardPageAudit {
            gpa,
            initial_sha256: Sha256::digest(page).into(),
            exec_scans: 0,
            first_exec_generation: 0,
            first_exec_sha256: [0; 32],
            write_revocations: 0,
            write_generation: 0,
            pre_write_sha256: [0; 32],
            post_write_exec_generation: 0,
            post_write_exec_sha256: [0; 32],
            backing_replacements: 0,
            pre_replace_sha256: [0; 32],
            replacement_sha256: [0; 32],
            post_replace_exec_generation: 0,
            post_replace_exec_sha256: [0; 32],
            stale_reply_attempts: 0,
            stale_reply_generation: 0,
            stale_reply_errno: 0,
        });
        Ok(())
    }

    /// Arm one deliberate replay of the audited page's first approved generation.
    ///
    /// The replay occurs only while the same page is frozen for its post-write scan;
    /// `EINVAL` is required before the exact current generation is approved normally.
    /// This is a planted proof hook, not part of production guard mediation.
    ///
    /// # Errors
    /// [`SysError::Protocol`] unless `gpa` is the page selected by
    /// [`Machine::audit_exec_guard_page`] and no guard exit has occurred yet.
    pub fn probe_stale_exec_guard_generation(&mut self, gpa: u64) -> Result<(), SysError> {
        if !matches!(self.exec_guard, Some(stats) if stats.exits == 0) {
            return Err(SysError::Protocol(
                "stale-generation probe must be armed on a guarded VM before first vCPU entry"
                    .into(),
            ));
        }
        if !matches!(self.exec_guard_page_audit, Some(audit) if audit.gpa == gpa) {
            return Err(SysError::Protocol(format!(
                "stale-generation probe page {gpa:#x} is not the selected guard audit page"
            )));
        }
        self.exec_guard_stale_probe_gpa = Some(gpa);
        Ok(())
    }

    /// Page-specific observations configured by [`Machine::audit_exec_guard_page`].
    #[must_use]
    pub fn exec_guard_page_audit(&self) -> Option<ExecGuardPageAudit> {
        self.exec_guard_page_audit
    }

    /// The generation of the most recent execute-scan the guard serviced (0 before any).
    #[must_use]
    pub fn exec_guard_last_scan_generation(&self) -> u64 {
        self.exec_guard_last_scan_generation
    }

    /// Re-establish RAM slot 0 (same GPA, size, and anonymous backing) to fire KVM's mmu
    /// notifier — the invalidation a live memslot update causes — clearing the guard's
    /// stage-2 execute approvals. Guest RAM survives (the backing mapping is untouched); only
    /// the KVM slot and its approvals are torn down and rebuilt, so the next execute of an
    /// already-approved page must re-scan at a fresh generation.
    ///
    /// # Errors
    /// [`SysError`] if either the delete or the re-add ioctl fails, or on an unguarded VM.
    pub fn notifier_replace_slot0(&mut self) -> Result<(), SysError> {
        if self.exec_guard.is_none() {
            return Err(SysError::Protocol(
                "notifier-replacement needs the stage-2 execute guard".into(),
            ));
        }
        let memory_size = self.mem_size as u64;
        // Delete slot 0 (memory_size 0), then re-add it with the identical backing.
        let delete = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size: 0,
            userspace_addr: self.mem as u64,
        };
        // SAFETY: `vm_fd` is valid; `delete` is a fully initialised memory-region struct.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::SET_USER_MEMORY_REGION as libc::c_ulong,
                &raw const delete,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_SET_USER_MEMORY_REGION delete)"));
        }
        let add = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size,
            userspace_addr: self.mem as u64,
        };
        // SAFETY: `vm_fd` is valid; `add` reinstates the identical mapping.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::SET_USER_MEMORY_REGION as libc::c_ulong,
                &raw const add,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_SET_USER_MEMORY_REGION re-add)"));
        }
        Ok(())
    }

    /// Move RAM slot 0 to a DISTINCT anonymous backing whose contents are byte-identical to
    /// the current one. Unlike [`Machine::notifier_replace_slot0`] (which reinstates the same
    /// backing), this proves the guard re-scans even when the page content is unchanged — the
    /// approval is keyed to the mapping, not to a content hash. The old backing is unmapped.
    ///
    /// # Errors
    /// [`SysError`] if the fresh mapping or either ioctl fails, or on an unguarded VM.
    pub fn backing_replace_slot0(&mut self) -> Result<(), SysError> {
        if self.exec_guard.is_none() {
            return Err(SysError::Protocol(
                "backing-replacement needs the stage-2 execute guard".into(),
            ));
        }
        let len = self.mem_size;
        let fresh = map_guest_ram(len)?;
        // SAFETY: `self.mem` and `fresh` are both live, uniquely-owned, `len`-byte mappings
        // that do not overlap (distinct anonymous mmaps); copy the full guest RAM verbatim.
        unsafe { core::ptr::copy_nonoverlapping(self.mem, fresh, len) };

        let memory_size = len as u64;
        let delete = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size: 0,
            userspace_addr: self.mem as u64,
        };
        // SAFETY: `vm_fd` is valid; `delete` is a fully initialised memory-region struct.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::SET_USER_MEMORY_REGION as libc::c_ulong,
                &raw const delete,
            )
        } < 0
        {
            let e = err("ioctl(KVM_SET_USER_MEMORY_REGION delete)");
            // SAFETY: `fresh` is a live `len`-byte mapping owned here; nothing else aliases it.
            unsafe { libc::munmap(fresh.cast::<libc::c_void>(), len) };
            return Err(e);
        }
        let add = KvmUserspaceMemoryRegion {
            slot: 0,
            flags: 0,
            guest_phys_addr: RAM_BASE,
            memory_size,
            userspace_addr: fresh as u64,
        };
        // SAFETY: `vm_fd` is valid; `add` points slot 0 at the fresh identical backing.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::SET_USER_MEMORY_REGION as libc::c_ulong,
                &raw const add,
            )
        } < 0
        {
            let e = err("ioctl(KVM_SET_USER_MEMORY_REGION re-add fresh backing)");
            // SAFETY: `fresh` is a live `len`-byte mapping owned here; nothing else aliases it.
            unsafe { libc::munmap(fresh.cast::<libc::c_void>(), len) };
            return Err(e);
        }
        // The move succeeded: free the old backing and adopt the new one.
        // SAFETY: the old `self.mem` mapping is no longer referenced by the slot; this machine
        // owns it and nothing else aliases it.
        unsafe { libc::munmap(self.mem.cast::<libc::c_void>(), len) };
        self.mem = fresh;
        Ok(())
    }

    fn exec_guard_reply_generation(
        &self,
        gpa: u64,
        generation: u64,
        action: u32,
    ) -> Result<(), SysError> {
        let reply = KvmArmStage2ExecGuard {
            gpa,
            generation,
            action,
            flags: 0,
        };
        // SAFETY: `vm_fd` is valid; `reply` is the fully initialized 24-byte UAPI struct.
        if unsafe {
            libc::ioctl(
                self.vm_fd,
                kvm::ARM_STAGE2_EXEC_GUARD as libc::c_ulong,
                &raw const reply,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_ARM_STAGE2_EXEC_GUARD)"));
        }
        Ok(())
    }

    fn exec_guard_reply(&self, exit: ExecGuardExit, action: u32) -> Result<(), RunError> {
        self.exec_guard_reply_generation(exit.gpa, exit.generation, action)
            .map_err(|e| seam("ioctl(KVM_ARM_STAGE2_EXEC_GUARD)", e))
    }

    /// Service one synchronous execute-guard exit while the vCPU is stopped.
    ///
    /// The frozen page is borrowed only for the scan. A clean page is approved for
    /// execute/read-only; a hazardous page is rejected first, then surfaced as a hard
    /// error so no caller can resume it and mistake rejection for successful execution.
    fn service_exec_guard(&mut self, exit: ExecGuardExit) -> Result<(), RunError> {
        let Some(stats) = self.exec_guard.as_mut() else {
            return Err(RunError::UnexpectedExit(kvm::EXIT_ARM_STAGE2_EXEC_GUARD));
        };
        stats.exits = stats.exits.saturating_add(1);

        let known = kvm::ARM_STAGE2_EXEC_GUARD_EXIT_EXEC
            | kvm::ARM_STAGE2_EXEC_GUARD_EXIT_WRITE
            | kvm::ARM_STAGE2_EXEC_GUARD_EXIT_BLOCKED;
        let is_exec = exit.flags == kvm::ARM_STAGE2_EXEC_GUARD_EXIT_EXEC;
        let is_write = exit.flags == kvm::ARM_STAGE2_EXEC_GUARD_EXIT_WRITE;
        let is_blocked = exit.flags
            == (kvm::ARM_STAGE2_EXEC_GUARD_EXIT_WRITE | kvm::ARM_STAGE2_EXEC_GUARD_EXIT_BLOCKED);
        if exit.flags & !known != 0 || !(is_exec || is_write || is_blocked) {
            return Err(RunError::Seam {
                context: "decode KVM_EXIT_ARM_STAGE2_EXEC_GUARD",
                message: format!("invalid execute-guard flags {:#x}", exit.flags),
            });
        }
        if exit.generation == 0 || exit.gpa & 0xfff != 0 {
            return Err(RunError::Seam {
                context: "validate KVM_EXIT_ARM_STAGE2_EXEC_GUARD",
                message: format!(
                    "kernel supplied gpa {:#x}, generation {} (page alignment and nonzero \
                     generation are mandatory)",
                    exit.gpa, exit.generation
                ),
            });
        }

        if is_exec {
            stats.scans = stats.scans.saturating_add(1);
            self.exec_guard_last_scan_generation = exit.generation;
            // SAFETY: the vCPU is stopped at the synchronous exit and this machine owns the
            // entire live mapping. The bounded page helper rejects every out-of-slot GPA.
            let ram = unsafe { super::guest_ram(self.mem, self.mem_size) };
            let page =
                super::exec_guard_page(ram, RAM_BASE, exit.gpa).ok_or_else(|| RunError::Seam {
                    context: "scan KVM_EXIT_ARM_STAGE2_EXEC_GUARD page",
                    message: format!(
                        "page {:#x} is outside the single [{:#x}, {:#x}) RAM slot",
                        exit.gpa,
                        RAM_BASE,
                        RAM_BASE.saturating_add(self.mem_size as u64)
                    ),
                })?;
            let page_sha256: [u8; 32] = Sha256::digest(page).into();
            let stale_generation = if let Some(audit) = self
                .exec_guard_page_audit
                .as_mut()
                .filter(|audit| audit.gpa == exit.gpa)
            {
                audit.exec_scans = audit.exec_scans.saturating_add(1);
                if audit.first_exec_generation == 0 {
                    audit.first_exec_generation = exit.generation;
                    audit.first_exec_sha256 = page_sha256;
                }
                if audit.write_revocations > 0 && audit.post_write_exec_generation == 0 {
                    audit.post_write_exec_generation = exit.generation;
                    audit.post_write_exec_sha256 = page_sha256;
                    Some(audit.write_generation)
                } else {
                    None
                }
            } else {
                None
            };
            if self.exec_guard_stale_probe_gpa == Some(exit.gpa)
                && let Some(stale_generation) = stale_generation
            {
                if stale_generation == 0 || stale_generation == exit.generation {
                    return Err(RunError::Seam {
                        context: "plant stale execute-guard generation",
                        message: format!(
                            "prior generation {stale_generation} is not a nonzero predecessor of \
                             current generation {}",
                            exit.generation
                        ),
                    });
                }
                let stale_errno = match self.exec_guard_reply_generation(
                    exit.gpa,
                    stale_generation,
                    kvm::ARM_STAGE2_EXEC_GUARD_APPROVE_EXEC,
                ) {
                    Err(SysError::Errno { errno, .. }) if errno == libc::EINVAL => errno,
                    Err(e) => {
                        return Err(seam("replay stale KVM_ARM_STAGE2_EXEC_GUARD generation", e));
                    }
                    Ok(()) => {
                        return Err(RunError::Seam {
                            context: "replay stale KVM_ARM_STAGE2_EXEC_GUARD generation",
                            message: format!(
                                "kernel accepted superseded generation {stale_generation} while \
                                 page {:#x} was frozen for generation {}",
                                exit.gpa, exit.generation
                            ),
                        });
                    }
                };
                let Some(audit) = self.exec_guard_page_audit.as_mut() else {
                    return Err(RunError::Seam {
                        context: "record stale execute-guard generation",
                        message: "configured audit disappeared".into(),
                    });
                };
                audit.stale_reply_attempts = audit.stale_reply_attempts.saturating_add(1);
                audit.stale_reply_generation = stale_generation;
                audit.stale_reply_errno = stale_errno;
            }
            let hazards = super::exec_guard_hazards(exit.gpa, page);
            let action = if hazards.is_empty() {
                kvm::ARM_STAGE2_EXEC_GUARD_APPROVE_EXEC
            } else {
                kvm::ARM_STAGE2_EXEC_GUARD_REJECT_EXEC
            };
            self.exec_guard_reply(exit, action)?;
            let Some(stats) = self.exec_guard.as_mut() else {
                return Err(RunError::UnexpectedExit(kvm::EXIT_ARM_STAGE2_EXEC_GUARD));
            };
            if hazards.is_empty() {
                stats.approvals = stats.approvals.saturating_add(1);
                return Ok(());
            }

            stats.rejections = stats.rejections.saturating_add(1);
            let first: Vec<String> = hazards
                .iter()
                .take(8)
                .map(|hit| format!("{:#x}:{:?}", hit.addr, hit.kind))
                .collect();
            let exclusive_hazards = hazards
                .iter()
                .filter(|hit| matches!(hit.kind, crate::scan::HitKind::Exclusive))
                .count();
            return Err(RunError::ExecGuardRejected {
                gpa: exit.gpa,
                generation: exit.generation,
                hazards: hazards.len(),
                exclusive_hazards,
                live_counter_hazards: hazards.len() - exclusive_hazards,
                summary: first.join(", "),
            });
        }

        if is_blocked {
            stats.blocked_writes = stats.blocked_writes.saturating_add(1);
            return Err(RunError::Seam {
                context: "service blocked execute-guard write",
                message: format!(
                    "single-vCPU board reported a write blocked behind scan generation {} at \
                     {:#x}; there is no second vCPU that can own that scan",
                    exit.generation, exit.gpa
                ),
            });
        }

        // The kernel has already changed approved -> dirty/XN, synchronously unmapped
        // the old mapping, and exited before the store. No response ioctl is required.
        if matches!(self.exec_guard_page_audit, Some(audit) if audit.gpa == exit.gpa) {
            // SAFETY: the vCPU is stopped before the faulting store retries, and this
            // machine exclusively owns the complete live RAM mapping.
            let ram = unsafe { super::guest_ram(self.mem, self.mem_size) };
            let page =
                super::exec_guard_page(ram, RAM_BASE, exit.gpa).ok_or_else(|| RunError::Seam {
                    context: "audit KVM_EXIT_ARM_STAGE2_EXEC_GUARD write page",
                    message: format!("page {:#x} left the single RAM slot", exit.gpa),
                })?;
            let page_sha256: [u8; 32] = Sha256::digest(page).into();
            if let Some(audit) = self.exec_guard_page_audit.as_mut() {
                audit.write_revocations = audit.write_revocations.saturating_add(1);
                audit.write_generation = exit.generation;
                audit.pre_write_sha256 = page_sha256;
            }
        }
        stats.write_revocations = stats.write_revocations.saturating_add(1);
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

    /// Publish the managed **bootstrap placeholder** (`docs/PARAVIRT-CLOCK.md` ABI 1)
    /// at `PVCLOCK_GPA`, so bare AA-5 payloads read `CLOCKPAGE mode=managed-static`.
    ///
    /// Without this the page reads as zeroed RAM, the guest falls back to publishing
    /// its own static page, and reports `self-seeded` — which AA-5's acceptance forbids
    /// (`payloads/runtime/src/pvclock.rs`). This is the *minimum* the harness owes AA-5:
    /// a valid, materialized, deterministic page. Bare payload runs retain that historical
    /// static attestation. The Linux executor overwrites it canonically, with
    /// `FLAG_WORK_DERIVED` set and the actual `CNTFRQ_EL0`, after its guest-only perf counter
    /// establishes the work-zero anchor and before the first KVM entry.
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
        self.set_core_reg(kvm::REG_CORE_PC, pc)
    }

    /// Establish the register state required by the arm64 Linux boot protocol:
    /// enter the Image at EL1h with interrupts masked, pass the DTB in `x0`, and
    /// clear the three reserved argument registers.
    fn set_linux_entry(&mut self, entry_gpa: u64, dtb_gpa: u64) -> Result<(), SysError> {
        const PSTATE_EL1H_DAIF: u64 = 0x3c5;

        self.set_pc(entry_gpa)?;
        self.set_core_reg(kvm::REG_CORE_X0, dtb_gpa)?;
        for gpr in 1..=3 {
            self.set_core_reg(kvm::REG_CORE_X0 + gpr * kvm::REG_CORE_X_STRIDE, 0)?;
        }
        self.set_core_reg(kvm::REG_CORE_PSTATE, PSTATE_EL1H_DAIF)
    }

    fn set_core_reg(&mut self, index: u64, value: u64) -> Result<(), SysError> {
        let one = KvmOneReg {
            id: kvm::REG_ARM64_CORE_U64 | index,
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
            return Err(err("ioctl(KVM_SET_ONE_REG, core)"));
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
        let mut exec_guard_exits = 0_u64;
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
            if let Some(exit) = super::decode_exec_guard_exit(&snapshot) {
                exec_guard_exits = exec_guard_exits.saturating_add(1);
                if exec_guard_exits > MAX_EXEC_GUARD_EXITS_PER_RUN {
                    return Err(RunError::Seam {
                        context: "service KVM_EXIT_ARM_STAGE2_EXEC_GUARD",
                        message: format!(
                            "more than {MAX_EXEC_GUARD_EXITS_PER_RUN} guard exits occurred \
                             without one caller-visible exit"
                        ),
                    });
                }
                self.service_exec_guard(exit)?;
                continue;
            }
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
        // The volatile read-modify-write is the portable, Miri-exercised `super::write_mmio_read`
        // — this wrapper only supplies the mapped struct pointer.
        // SAFETY: `self.run` is a live MAP_SHARED mapping of at least size_of::<KvmRun>() bytes
        // (checked at construction) and the vCPU is stopped, so nothing else writes it.
        unsafe { super::write_mmio_read(self.run, data) };
        Ok(())
    }

    /// A digest of the landed state: every architectural register the kernel will
    /// hand over, VM-owned pvclock registration state, and the whole of guest RAM.
    ///
    /// This is what AA-3's replay-identity and AA-6's ≥1,000-rep bit-identity floors
    /// are *about*, and it is computed here rather than left empty — a rep floor
    /// that counts records without ever comparing their digests would be vacuous on
    /// the axis it exists for, so there must be a real digest to compare.
    ///
    /// Registers and the reserved device-state record are hashed in **sorted id order** (a
    /// `BTreeMap`, never a `HashMap`):
    /// iteration order must not reach a hashed byte. Conventions rule 4.
    fn state_digest(&mut self) -> Result<String, RunError> {
        let (regs, vgic) = self.registers_and_vgic()?;

        // SAFETY: `self.mem` is a live mapping of `self.mem_size` bytes and the vCPU
        // is not running (we are between exits), so nothing else writes it. The borrow is the
        // portable, Miri-exercised `super::guest_ram`; the hashing and sorted-order discipline
        // are the portable, Miri-tested `digest_state`.
        let ram = unsafe { super::guest_ram(self.mem, self.mem_size) };
        let digest = super::digest_state(&regs, ram, &vgic);

        // Diagnostic (env-gated, off by default): when replay-identity flags a divergent
        // landing, `AA3_DUMP_REGS=1` emits the regs-only and RAM sub-digests plus every
        // (id -> value) pair for THIS landing to stderr, tagged with the full digest. Diffing
        // two divergent reps' dumps isolates whether the divergence is a register (which one)
        // or guest RAM — the difference between a measurement artifact and a real anomaly.
        if std::env::var_os("AA3_DUMP_REGS").is_some() {
            let regs_only = super::digest_regs_only(&regs, &vgic);
            let ram_digest = super::digest_state(&BTreeMap::new(), ram, &[]);
            eprintln!("AA3REGS digest={digest} regs_only={regs_only} ram={ram_digest}");
            for (id, val) in &regs {
                let hexval: String = val.iter().map(|b| format!("{b:02x}")).collect();
                eprintln!("AA3REGS   id={id:#018x} val={hexval}");
            }
        }

        Ok(digest)
    }
}

impl StepVcpu for Machine {
    /// Arm `KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP` once (no hardware breakpoints), so
    /// every subsequent `KVM_RUN` returns `KVM_EXIT_DEBUG` after a single guest instruction.
    fn arm_single_step(&mut self) -> Result<(), RunError> {
        let dbg = KvmGuestDebug {
            control: kvm::GUESTDBG_SINGLESTEP_CONTROL,
            pad: 0,
            arch: KvmGuestDebugArch::default(),
        };
        // SAFETY: `vcpu_fd` is valid; `dbg` is a fully-initialised kvm_guest_debug on this
        // frame, exactly the shape (and arm64 size) KVM_SET_GUEST_DEBUG reads.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::SET_GUEST_DEBUG as libc::c_ulong,
                &raw const dbg,
            )
        } < 0
        {
            return Err(seam(
                "ioctl(KVM_SET_GUEST_DEBUG, single-step)",
                err("ioctl(KVM_SET_GUEST_DEBUG)"),
            ));
        }
        Ok(())
    }

    /// Disarm guest single-step: `KVM_SET_GUEST_DEBUG` with an all-zero `kvm_guest_debug`
    /// (control 0 = debug disabled, no `KVM_GUESTDBG_ENABLE`), returning the vCPU to ordinary
    /// `KVM_RUN` execution. AA-3's exact-landing loop steps `BR_RETIRED` up to the target, then
    /// disarms here before resuming the guest to `MARK_END`.
    fn disarm_single_step(&mut self) -> Result<(), RunError> {
        let dbg = KvmGuestDebug::default();
        // SAFETY: `vcpu_fd` is valid; `dbg` is a fully-initialised kvm_guest_debug (control 0 =
        // debug disabled), exactly the shape (and arm64 size) KVM_SET_GUEST_DEBUG reads.
        if unsafe {
            libc::ioctl(
                self.vcpu_fd,
                kvm::SET_GUEST_DEBUG as libc::c_ulong,
                &raw const dbg,
            )
        } < 0
        {
            return Err(seam(
                "ioctl(KVM_SET_GUEST_DEBUG, disarm)",
                err("ioctl(KVM_SET_GUEST_DEBUG)"),
            ));
        }
        Ok(())
    }

    fn pc(&mut self) -> Result<u64, RunError> {
        self.get_one_reg_u64(kvm::REG_ARM64_CORE_U64 | kvm::REG_CORE_PC)
            .map_err(|e| seam("ioctl(KVM_GET_ONE_REG, pc)", e))
    }

    fn opcode_at(&mut self, addr: u64) -> Result<Option<u32>, RunError> {
        // SAFETY: `self.mem` is a live mapping of `self.mem_size` bytes and the vCPU is stopped
        // between exits, so nothing else writes it. The borrow and the bounded 4-byte decode
        // are the portable, Miri-exercised `super::guest_ram` / `super::guest_word`.
        let ram = unsafe { super::guest_ram(self.mem, self.mem_size) };
        Ok(super::guest_word(ram, RAM_BASE, addr))
    }

    fn vbar(&mut self) -> Result<u64, RunError> {
        self.get_one_reg_u64(kvm::REG_VBAR_EL1)
            .map_err(|e| seam("ioctl(KVM_GET_ONE_REG, VBAR_EL1)", e))
    }

    /// The registers-only digest AA-2 stamps on every step but the last: the vCPU registers,
    /// VM-owned pvclock registration, and in-kernel vGIC state [`Vcpu::state_digest`] reads,
    /// hashed **without** the 4 MiB guest-RAM slice. `state_digest` faults in and hashes the
    /// whole slot every call, so calling it per single step is infeasible — the whole
    /// reason `RAM_SIZE` was shrunk in the AA-1(c) disposition. This is the cheap
    /// per-step replay key; only the run's final step pays the full-RAM cost, catching
    /// memory divergence across the stepped window end-to-end.
    fn regs_digest(&mut self) -> Result<String, RunError> {
        let (regs, vgic) = self.registers_and_vgic()?;
        Ok(super::digest_regs_only(&regs, &vgic))
    }
}

impl LinuxPvclockVcpu for Machine {
    fn linux_pvclock_gpa(&self) -> Option<u64> {
        self.linux_pvclock_gpa
    }

    fn register_linux_pvclock_gpa(&mut self, gpa: u64) -> Result<(), RunError> {
        if let Some(registered) = self.linux_pvclock_gpa {
            return Err(RunError::Seam {
                context: "register the Linux pvclock page",
                message: format!("one-shot already pinned to {registered:#x}; rejected {gpa:#x}"),
            });
        }
        // Validation precedes mutation so an invalid guest value cannot consume the VM-lifetime
        // one-shot even if this trait method is called outside the ordinary MMIO dispatcher.
        crate::linux_console::linux_pvclock_page_range(gpa, RAM_BASE, self.mem_size)?;
        self.linux_pvclock_gpa = Some(gpa);
        Ok(())
    }

    fn publish_linux_pvclock(
        &mut self,
        vns: u64,
        guest_clock: u64,
        guest_clock_hz: u64,
        write: PvclockWrite,
    ) -> Result<(), RunError> {
        let gpa = self.linux_pvclock_gpa.ok_or_else(|| RunError::Seam {
            context: "publish the Linux pvclock page",
            message: "no VM-owned pvclock registration exists".into(),
        })?;
        let page_range =
            crate::linux_console::linux_pvclock_page_range(gpa, RAM_BASE, self.mem_size)?;

        // SAFETY: `self.mem` is the unique live mapping of exactly `self.mem_size` bytes and
        // this method is called only after a KVM exit, while the sole vCPU is stopped. The
        // pure range validator bounds the complete page before either shared stamping function
        // runs.
        let ram = unsafe { core::slice::from_raw_parts_mut(self.mem, self.mem_size) };
        let page = ram.get_mut(page_range).ok_or_else(|| RunError::Seam {
            context: "bound the Linux pvclock page after validation",
            message: "validated pvclock page disappeared from guest RAM".into(),
        })?;
        match write {
            PvclockWrite::Canonical => {
                vtime::pvclock::stamp_canonical(page, vns, guest_clock, guest_clock_hz);
            }
            PvclockWrite::Refresh => {
                vtime::pvclock::stamp(page, vns, guest_clock, guest_clock_hz);
            }
        }

        let fields = vtime::pvclock::read(page).ok_or_else(|| RunError::Seam {
            context: "read back the Linux pvclock page",
            message: "stamping did not leave a stable ABI-v1 page".into(),
        })?;
        if fields.vns != vns
            || fields.guest_clock != guest_clock
            || fields.guest_clock_hz != guest_clock_hz
            || fields.flags != vtime::pvclock::PVCLOCK_FLAGS_V1
            || fields.vcpu_index != 0
        {
            return Err(RunError::Seam {
                context: "read back the Linux pvclock page",
                message: format!(
                    "published ({vns}, {guest_clock}, {guest_clock_hz}) but read back \
                     ({}, {}, {}, flags {:#x}, vcpu {})",
                    fields.vns,
                    fields.guest_clock,
                    fields.guest_clock_hz,
                    fields.flags,
                    fields.vcpu_index
                ),
            });
        }
        Ok(())
    }

    fn linux_clockevent_state(&self) -> LinuxClockeventState {
        self.linux_clockevent.unwrap_or_default()
    }

    fn program_linux_clockevent(&mut self, deadline_ticks: u64) -> Result<(), RunError> {
        let state = self.linux_clockevent.ok_or_else(|| RunError::Seam {
            context: "program the Linux deterministic clockevent",
            message: "the VM is not the owned Linux board".into(),
        })?;
        if state.irq_asserted {
            return Err(RunError::Seam {
                context: "program the Linux deterministic clockevent",
                message: "guest replaced its deadline while PPI 20 was still asserted".into(),
            });
        }
        self.linux_clockevent
            .as_mut()
            .ok_or_else(|| RunError::Seam {
                context: "program the Linux deterministic clockevent",
                message: "Linux clockevent state disappeared".into(),
            })?
            .deadline_ticks = Some(deadline_ticks);
        Ok(())
    }

    fn disarm_linux_clockevent(&mut self) -> Result<(), RunError> {
        let state = self.linux_clockevent.ok_or_else(|| RunError::Seam {
            context: "disarm the Linux deterministic clockevent",
            message: "the VM is not the owned Linux board".into(),
        })?;
        if state.irq_asserted {
            self.set_linux_clockevent_irq(false)?;
            if self.linux_clockevent_line_asserted()? {
                return Err(RunError::Seam {
                    context: "disarm the Linux deterministic clockevent",
                    message: "vGIC PPI 20 input line remained high after deassertion".into(),
                });
            }
        }
        let state = self
            .linux_clockevent
            .as_mut()
            .ok_or_else(|| RunError::Seam {
                context: "disarm the Linux deterministic clockevent",
                message: "Linux clockevent state disappeared".into(),
            })?;
        state.deadline_ticks = None;
        state.irq_asserted = false;
        Ok(())
    }

    fn acknowledge_linux_clockevent(&mut self) -> Result<(), RunError> {
        let state = self.linux_clockevent.ok_or_else(|| RunError::Seam {
            context: "acknowledge the Linux deterministic clockevent",
            message: "the VM is not the owned Linux board".into(),
        })?;
        if !state.irq_asserted {
            return Err(RunError::Seam {
                context: "acknowledge the Linux deterministic clockevent",
                message: "guest ACK arrived while PPI 20 was low".into(),
            });
        }
        let acknowledgements =
            state
                .acknowledgements
                .checked_add(1)
                .ok_or_else(|| RunError::Seam {
                    context: "acknowledge the Linux deterministic clockevent",
                    message: "acknowledgement counter overflow".into(),
                })?;
        self.set_linux_clockevent_irq(false)?;
        if self.linux_clockevent_line_asserted()? {
            return Err(RunError::Seam {
                context: "acknowledge the Linux deterministic clockevent",
                message: "vGIC PPI 20 input line remained high after ACK deassertion".into(),
            });
        }
        let state = self
            .linux_clockevent
            .as_mut()
            .ok_or_else(|| RunError::Seam {
                context: "acknowledge the Linux deterministic clockevent",
                message: "Linux clockevent state disappeared".into(),
            })?;
        state.irq_asserted = false;
        state.acknowledgements = acknowledgements;
        Ok(())
    }

    fn fire_due_linux_clockevent(&mut self, now_ticks: u64) -> Result<Option<u64>, RunError> {
        let state = self.linux_clockevent.ok_or_else(|| RunError::Seam {
            context: "assert the Linux deterministic clockevent",
            message: "the VM is not the owned Linux board".into(),
        })?;
        let Some(deadline) = state.deadline_ticks else {
            return Ok(None);
        };
        if now_ticks < deadline {
            return Ok(None);
        }
        if state.irq_asserted {
            return Err(RunError::Seam {
                context: "assert the Linux deterministic clockevent",
                message: "a due deadline found PPI 20 already asserted".into(),
            });
        }
        let assertions = state
            .assertions
            .checked_add(1)
            .ok_or_else(|| RunError::Seam {
                context: "assert the Linux deterministic clockevent",
                message: "assertion counter overflow".into(),
            })?;
        if self.linux_clockevent_line_asserted()? {
            return Err(RunError::Seam {
                context: "assert the Linux deterministic clockevent",
                message: "vGIC reports PPI 20 input high before userspace raises its line".into(),
            });
        }

        self.set_linux_clockevent_irq(true)?;
        // Mutate immediately after the successful ioctl. Even if readback fails, retained state
        // still truthfully records that userspace drove the external line high.
        let state = self
            .linux_clockevent
            .as_mut()
            .ok_or_else(|| RunError::Seam {
                context: "assert the Linux deterministic clockevent",
                message: "Linux clockevent state disappeared".into(),
            })?;
        state.deadline_ticks = None;
        state.irq_asserted = true;
        state.assertions = assertions;

        if !self.linux_clockevent_line_asserted()? {
            return Err(RunError::Seam {
                context: "verify the Linux deterministic clockevent assertion",
                message: "KVM_IRQ_LINE succeeded but vGIC PPI 20 input stayed low".into(),
            });
        }
        Ok(Some(now_ticks - deadline))
    }
}

/// The seam-read pair [`Machine::registers_and_vgic`] returns: every architectural register plus
/// VM-owned digest-bound device state (id → bytes, sorted), and the in-kernel vGIC injection
/// state. Named so the state/vGIC read type has one home (and the tuple-of-collections does not
/// trip `clippy::type_complexity` on the `cfg(target_os = "linux")` box seam).
type RegsAndVgic = (BTreeMap<u64, Vec<u8>>, Vec<u8>);

impl Machine {
    fn set_linux_clockevent_irq(&self, asserted: bool) -> Result<(), RunError> {
        let irq = KvmIrqLevel {
            irq: HARMONY_CLOCKEVENT_IRQ,
            level: u32::from(asserted),
        };
        // SAFETY: `vm_fd` is live and `irq` is the exact fixed-width kvm_irq_level value KVM
        // reads synchronously. The encoded target is vCPU 0's dedicated PPI INTID 20.
        if unsafe { libc::ioctl(self.vm_fd, kvm::IRQ_LINE as libc::c_ulong, &raw const irq) } < 0 {
            return Err(seam(
                "ioctl(KVM_IRQ_LINE, Harmony PPI 20)",
                err("ioctl(KVM_IRQ_LINE)"),
            ));
        }
        Ok(())
    }

    fn linux_clockevent_line_asserted(&self) -> Result<bool, RunError> {
        // `KVM_IRQ_LINE` changes `irq->line_level`; GICR_ISPENDR0's userspace accessor
        // reports the distinct pending latch and can remain zero for a valid level assertion.
        // LEVEL_INFO therefore both verifies delivery and catches KVM's owner-mismatch no-op.
        let levels = self
            .vgic_reg(
                kvm::DEV_ARM_VGIC_GRP_LEVEL_INFO,
                HARMONY_CLOCKEVENT_LEVEL_INFO_ATTR,
            )
            .map_err(|error| seam("read back vGIC PPI 20 input-line level", error))?;
        Ok(levels & HARMONY_CLOCKEVENT_LINE_MASK != 0)
    }

    /// Borrow the guest RAM mapping between exits (diagnostic surface for the AA-5
    /// RAM-divergence dump; the vCPU must be stopped).
    #[must_use]
    pub fn guest_ram_bytes(&self) -> &[u8] {
        // SAFETY: `self.mem` is a live mapping of `self.mem_size` bytes and the vCPU is
        // stopped between exits, so nothing else writes it; the borrow is the portable,
        // Miri-exercised `super::guest_ram`.
        unsafe { super::guest_ram(self.mem, self.mem_size) }
    }

    /// Read the guest-visible constant generic-counter frequency (`CNTFRQ_EL0`).
    ///
    /// The AA-5 page carries this exact value and the owned kernel fails closed on a mismatch;
    /// no host-frequency guess or DT constant is accepted. KVM exposes no `CNTFRQ_EL0`
    /// one-reg (verified against the pinned 6.18.35 sysreg table on-box, 2026-07-20 — a
    /// `KVM_GET_ONE_REG` of encoding (3,3,14,0,0) is ENOENT on 6.8 and 6.18 alike): the
    /// guest observes the host's own frequency, which EL0 reads directly.
    pub fn linux_cntfrq_hz(&self) -> Result<u64, SysError> {
        #[cfg(target_arch = "aarch64")]
        {
            let hz: u64;
            // SAFETY: CNTFRQ_EL0 is architecturally EL0-readable (Linux leaves EL0 counter
            // access enabled on the host); the read has no side effects.
            unsafe { core::arch::asm!("mrs {hz}, cntfrq_el0", hz = out(reg) hz) };
            if hz == 0 {
                return Err(SysError::Protocol(
                    "host CNTFRQ_EL0 reads 0 — no usable guest counter frequency".to_owned(),
                ));
            }
            Ok(hz)
        }
        #[cfg(not(target_arch = "aarch64"))]
        {
            Err(SysError::Protocol(
                "guest CNTFRQ_EL0 is the host counter frequency, readable only on aarch64"
                    .to_owned(),
            ))
        }
    }

    /// Read every architectural register (`KVM_GET_REG_LIST` + `KVM_GET_ONE_REG`, in
    /// sorted id order), add any VM-owned pvclock registration, and read the in-kernel vGIC
    /// injection state — the seam-read inputs
    /// shared by [`Vcpu::state_digest`] (which adds guest RAM) and [`StepVcpu::regs_digest`]
    /// (which does not). Factored out so the register/vGIC read discipline has one home.
    fn registers_and_vgic(&self) -> Result<RegsAndVgic, RunError> {
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
        if let Some(gpa) = self.linux_pvclock_gpa {
            let mut encoded = Vec::with_capacity(
                LINUX_PVCLOCK_DIGEST_STATE_TAG.len() + core::mem::size_of::<u64>(),
            );
            encoded.extend_from_slice(LINUX_PVCLOCK_DIGEST_STATE_TAG);
            encoded.extend_from_slice(&gpa.to_le_bytes());
            if regs
                .insert(LINUX_PVCLOCK_DIGEST_STATE_ID, encoded)
                .is_some()
            {
                return Err(RunError::Seam {
                    context: "bind the Linux pvclock registration into the state digest",
                    message: format!(
                        "reserved state id {LINUX_PVCLOCK_DIGEST_STATE_ID:#x} collides with a KVM register"
                    ),
                });
            }
        }
        if let Some(state) = self.linux_clockevent {
            let mut encoded = Vec::with_capacity(
                LINUX_CLOCKEVENT_DIGEST_STATE_TAG.len() + 2 + 3 * core::mem::size_of::<u64>(),
            );
            encoded.extend_from_slice(LINUX_CLOCKEVENT_DIGEST_STATE_TAG);
            encoded.push(u8::from(state.deadline_ticks.is_some()));
            encoded.extend_from_slice(&state.deadline_ticks.unwrap_or(0).to_le_bytes());
            encoded.push(u8::from(state.irq_asserted));
            encoded.extend_from_slice(&state.assertions.to_le_bytes());
            encoded.extend_from_slice(&state.acknowledgements.to_le_bytes());
            if regs
                .insert(LINUX_CLOCKEVENT_DIGEST_STATE_ID, encoded)
                .is_some()
            {
                return Err(RunError::Seam {
                    context: "bind the Linux clockevent into the state digest",
                    message: format!(
                        "reserved state id {LINUX_CLOCKEVENT_DIGEST_STATE_ID:#x} collides with a KVM register"
                    ),
                });
            }
        }
        let vgic = self
            .vgic_state()
            .map_err(|e| seam("ioctl(KVM_GET_DEVICE_ATTR, vGIC)", e))?;
        Ok((regs, vgic))
    }

    /// Read a 64-bit register (`KVM_GET_ONE_REG` into a `u64`) — the core `pc`, a system
    /// register like `VBAR_EL1`, sized by the caller's id.
    fn get_one_reg_u64(&self, id: u64) -> Result<u64, SysError> {
        let mut value: u64 = 0;
        let one = KvmOneReg {
            id,
            addr: (&raw mut value) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `one.addr` points at a live u64 the kernel writes.
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

    /// Do one single step: read `PC` + `BR_RETIRED`, one `KVM_RUN` (expecting
    /// `KVM_EXIT_DEBUG`), read `PC` + `BR_RETIRED` again — returning `(pc_before, pc_after,
    /// br_retired_delta)`. `insn_retired` is 1 by construction of a single step; the box
    /// confirms it against the oracle (that is the AA-2 measurement).
    ///
    /// The single-step primitive for a caller stepping instructions that do NOT touch the
    /// console: a non-`Debug` exit (an MMIO console access, a mechanism kick) is refused here.
    /// [`crate::run::step_run`] is the full run loop that also services the console; this is
    /// the direct primitive the plan (`AA2-BUILD.md`) names.
    ///
    /// # Errors
    /// [`RunError`] if a register/counter read failed, the exit was not `KVM_EXIT_DEBUG`, or
    /// `BR_RETIRED` went backwards across the step.
    pub fn step_once(
        &mut self,
        counter: &mut impl WorkCounter,
    ) -> Result<(u64, u64, u64), RunError> {
        let pc_before = self.pc()?;
        let work_before = counter.read()?;
        match Vcpu::run(self)? {
            VcpuExit::Debug => {}
            other => {
                return Err(RunError::Seam {
                    context: "step_once expected KVM_EXIT_DEBUG",
                    message: format!("got a non-debug exit: {other:?}"),
                });
            }
        }
        let pc_after = self.pc()?;
        let work_after = counter.read()?;
        let delta =
            work_after
                .checked_sub(work_before)
                .ok_or(RunError::StepCounterWentBackwards {
                    before: work_before,
                    after: work_after,
                })?;
        Ok((pc_before, pc_after, delta))
    }

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

        // The CPU-INTERFACE state (ICC_* system registers): the priority mask, group
        // enables, and active-priority registers that decide HOW a pending interrupt is
        // delivered. These live in KVM's CPU_SYSREGS save group — not the redistributor or
        // distributor groups, and not the generic vCPU register list — so two runs differing
        // only in CPU-interface interrupt state would otherwise share an AA-6 digest. The
        // attr low bits are the register's `(op0,op1,crn,crm,op2)` encoding (mpidr 0). Only
        // the always-present registers are read (AP0R1.. depend on the priority-bit count).
        const fn icc(op0: u64, op1: u64, crn: u64, crm: u64, op2: u64) -> u64 {
            (op0 << 14) | (op1 << 11) | (crn << 7) | (crm << 3) | op2
        }
        let cpu_sysregs: &[u64] = &[
            icc(3, 0, 4, 6, 0),   // ICC_PMR_EL1     — priority mask
            icc(3, 0, 12, 8, 3),  // ICC_BPR0_EL1
            icc(3, 0, 12, 8, 4),  // ICC_AP0R0_EL1   — active priorities (group 0)
            icc(3, 0, 12, 9, 0),  // ICC_AP1R0_EL1   — active priorities (group 1)
            icc(3, 0, 12, 12, 3), // ICC_BPR1_EL1
            icc(3, 0, 12, 12, 4), // ICC_CTLR_EL1
            icc(3, 0, 12, 12, 5), // ICC_SRE_EL1
            icc(3, 0, 12, 12, 6), // ICC_IGRPEN0_EL1 — group 0 enable
            icc(3, 0, 12, 12, 7), // ICC_IGRPEN1_EL1 — group 1 enable
        ];

        let mut out = Vec::with_capacity((redist.len() + dist.len()) * 4 + cpu_sysregs.len() * 8);
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
        for &attr in cpu_sysregs {
            out.extend_from_slice(
                &self
                    .vgic_reg64(kvm::DEV_ARM_VGIC_GRP_CPU_SYSREGS, attr)?
                    .to_le_bytes(),
            );
        }
        Ok(out)
    }

    /// Read one 64-bit vGIC CPU-interface register through `KVM_GET_DEVICE_ATTR`. The
    /// CPU_SYSREGS group's registers are 64-bit EL1 system registers (unlike the 32-bit
    /// DIST/REDIST offsets [`Machine::vgic_reg`] reads).
    fn vgic_reg64(&self, group: u32, attr: u64) -> Result<u64, SysError> {
        let mut value: u64 = 0;
        let da = KvmDeviceAttr {
            flags: 0,
            group,
            attr,
            addr: (&raw mut value) as u64,
        };
        // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u64 on this frame, which is
        // what KVM_GET_DEVICE_ATTR's CPU_SYSREGS accessor writes.
        if unsafe {
            libc::ioctl(
                self.vgic_fd,
                kvm::GET_DEVICE_ATTR as libc::c_ulong,
                &raw const da,
            )
        } < 0
        {
            return Err(err("ioctl(KVM_GET_DEVICE_ATTR, vGIC CPU sysregs)"));
        }
        Ok(value)
    }

    /// Read one 32-bit vGIC device attribute through `KVM_GET_DEVICE_ATTR`.
    ///
    /// The DIST/REDIST and LEVEL_INFO accessors write a 32-bit value into the buffer the
    /// attribute's `addr` points at. DIST/REDIST encode `mpidr(63:32) | offset(31:0)`;
    /// LEVEL_INFO encodes `mpidr(63:32) | info(31:10) | vINTID(9:0)`.
    fn vgic_reg(&self, group: u32, attr: u64) -> Result<u32, SysError> {
        let mut value: u32 = 0;
        let da = KvmDeviceAttr {
            flags: 0,
            group,
            attr,
            addr: (&raw mut value) as u64,
        };
        // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u32 on this frame,
        // which is what KVM_GET_DEVICE_ATTR's 32-bit vGIC accessors write.
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

    // AA-6 installs a BELOW-HOST synthetic feature model across the WHOLE `ID_AA64*` surface,
    // not just PFR0: a host where one PFR0 nibble is writable but `ISAR0`/`MMFR*`/`DFR0` are
    // frozen cannot provide that model. So enumerate EVERY relevant register and require each
    // to accept a reduced, read-back-confirmed feature value — returning `true` on the first
    // PFR0 success (as an earlier draft did) green-lights a host that cannot actually freeze
    // the surface AA-6 needs.
    //
    // `(register, skip_low_nibbles)`: PFR0's low four nibbles are EL0..3 support — lowering EL
    // support breaks the VM — so they are skipped; the other registers' low nibbles are
    // ordinary feature fields.
    // The full ID_AA64* feature surface AA-6's shrunk-feature model spans — NOT a subset:
    // enumerating six (r22) let a host with those six reducible but `ID_AA64ISAR1_EL1` /
    // `ID_AA64PFR1_EL1` frozen still read TRUE, though the whole-surface model cannot install.
    // A register that carries NO reducible feature on this host (all fields absent/max — SVE's
    // ZFR0 on an N1, say) has nothing to freeze and does not gate; a register with real fields
    // that are FROZEN does. That distinction is why the probe is tri-state, not a bare bool.
    let relevant: &[(u64, u32, &str)] = &[
        (kvm::REG_ID_AA64PFR0_EL1, 4, "ID_AA64PFR0_EL1"),
        (kvm::REG_ID_AA64PFR1_EL1, 0, "ID_AA64PFR1_EL1"),
        (kvm::REG_ID_AA64ISAR0_EL1, 0, "ID_AA64ISAR0_EL1"),
        (kvm::REG_ID_AA64ISAR1_EL1, 0, "ID_AA64ISAR1_EL1"),
        (kvm::REG_ID_AA64ISAR2_EL1, 0, "ID_AA64ISAR2_EL1"),
        (kvm::REG_ID_AA64MMFR0_EL1, 0, "ID_AA64MMFR0_EL1"),
        (kvm::REG_ID_AA64MMFR1_EL1, 0, "ID_AA64MMFR1_EL1"),
        (kvm::REG_ID_AA64MMFR2_EL1, 0, "ID_AA64MMFR2_EL1"),
        (kvm::REG_ID_AA64DFR0_EL1, 0, "ID_AA64DFR0_EL1"),
        (kvm::REG_ID_AA64DFR1_EL1, 0, "ID_AA64DFR1_EL1"),
    ];
    // Per-register stderr diagnostics: the row's evidence stays the bool, but an
    // `absent` verdict over a 10-register conjunction is undiagnosable without knowing
    // WHICH register refused (day-one lesson: harmony-arm's 6.8 kernel failed the row
    // and the probe could not say where). Diagnostics go to stderr, never into the
    // truth table — the table records what the probe concluded, not its trace.
    let mut all_writable = true;
    for &(reg, skip, name) in relevant {
        // `Some(false)` = this register HAS reducible feature fields but none is writable — the
        // surface AA-6 needs is not fully writable, so the row is FALSE. `Some(true)` (writable)
        // and `None` (nothing to freeze here) both pass this register.
        match reduce_and_readback_id_field(vcpu_fd, reg, skip)? {
            Some(true) => {
                eprintln!("writable-id-registers: {name}: writable (reduced + read back)")
            }
            Some(false) => {
                eprintln!(
                    "writable-id-registers: {name}: FROZEN (has reducible fields, none accepted \
                     a reduced write that read back)"
                );
                all_writable = false;
            }
            None => eprintln!("writable-id-registers: {name}: no reducible field (does not gate)"),
        }
    }
    Ok(all_writable)
}

/// Try to reduce ONE feature nibble of an `ID_AA64*` register by 1, write it, and READ IT
/// BACK. `Ok(true)` if some field was both accepted AND observed reduced; `Ok(false)` if no
/// field could be. Nibbles `[0, skip_low_nibbles)` are skipped (PFR0's EL-support fields).
///
/// The row is about a CHANGED, reduced value — NOT an identity write: some KVM versions accept
/// an identity `SET_ONE_REG` (for migration compatibility) while rejecting any changed
/// invariant/ID value, so writing the value just read would false-green. Absent (0) or
/// not-implemented (0xF) fields cannot be cleanly lowered and are skipped.
/// Returns `Some(true)` if a field was accepted AND observed reduced (the register is
/// writable), `Some(false)` if the register HAS reducible feature fields but none could be
/// (frozen or silently clamped), and `None` if the register carries no reducible feature at all
/// (every field absent (0) or not-implemented (0xF)) — nothing to freeze, so it does not gate.
#[cfg(target_os = "linux")]
fn reduce_and_readback_id_field(
    vcpu_fd: libc::c_int,
    reg: u64,
    skip_low_nibbles: u32,
) -> Result<Option<bool>, SysError> {
    let mut orig: u64 = 0;
    let get = KvmOneReg {
        id: reg,
        addr: (&raw mut orig) as u64,
    };
    // SAFETY: `vcpu_fd` is valid; `get.addr` points at a live u64 the kernel writes.
    if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get) } < 0 {
        return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64*)"));
    }
    let mut had_candidate = false;
    for nibble in skip_low_nibbles..16u32 {
        let shift = nibble * 4;
        let field = (orig >> shift) & 0xF;
        if field == 0 || field == 0xF {
            continue;
        }
        had_candidate = true;
        let reduced = (orig & !(0xFu64 << shift)) | ((field - 1) << shift);
        let set = KvmOneReg {
            id: reg,
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
            return Err(err("ioctl(KVM_SET_ONE_REG, ID_AA64*)"));
        }
        // The SET was accepted — confirm the reduction actually took, rather than being
        // silently clamped back to the host value (accepting the ioctl but ignoring the
        // change is exactly the identity-write false-green this probe exists to defeat).
        let mut readback: u64 = 0;
        let get2 = KvmOneReg {
            id: reg,
            addr: (&raw mut readback) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `get2.addr` points at a live u64 the kernel writes.
        if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get2) } < 0 {
            return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64* readback)"));
        }
        if readback == reduced {
            return Ok(Some(true));
        }
        // Accepted but unchanged: not a real feature write. Try the next field.
    }
    // No field could be reduced: `Some(false)` if there WERE reducible candidates (frozen),
    // `None` if the register carries no reducible feature at all (nothing to freeze).
    Ok(if had_candidate { Some(false) } else { None })
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
    /// Whether the reading vCPU was initialised with `KVM_ARM_VCPU_PMU_V3`. When
    /// `false`, `id_aa64dfr0`'s PMUVer field is KVM's featureless-vCPU mask (0), not
    /// the host PMU version — the truth-table raw must say which read happened.
    pub pmu_v3_enabled: bool,
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
    // Init WITH the vPMU feature, falling back to a plain init if the host refuses it.
    //
    // KVM masks the guest-visible `ID_AA64DFR0_EL1.PMUVer` to 0 on a vCPU created
    // without `KVM_ARM_VCPU_PMU_V3` — the first real box (harmony-arm, N1 r3p1,
    // Ubuntu 6.8) reported `pmuver = 0x0` through a featureless vCPU while its host
    // PMU is plainly PMUv3 (the perf rows count). The truth-table row is about the
    // PMU behind the work-clock bet, so the read must go through a PMU-enabled vCPU
    // to see the sanitised host value. The MEASUREMENT vCPU (`Machine`) deliberately
    // keeps the feature off — the guest contract denies the guest its own PMU; only
    // this disposable ID-reading vCPU differs. If the host cannot init a PMU-enabled
    // vCPU at all, the plain-init value (PMUVer masked to 0) is the honest fallback,
    // and `pmu_v3_enabled: false` records which read happened.
    let mut pmu_init = init;
    pmu_init.features[0] |= 1 << kvm::VCPU_FEATURE_PMU_V3;
    // SAFETY: `vcpu_fd` is valid; `pmu_init` is fully initialised above.
    let pmu_v3_enabled = unsafe {
        libc::ioctl(
            vcpu_fd,
            kvm::ARM_VCPU_INIT as libc::c_ulong,
            &raw const pmu_init,
        )
    } >= 0;
    if !pmu_v3_enabled {
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
        pmu_v3_enabled,
    })
}

// ============================================================================
// AA-6(a) ID-register-freeze enforcement proof (standalone; own VM/vCPU, like
// `probe_writable_id_registers`). Additive: it touches no `Machine`, run-loop,
// or W^X code.
// ============================================================================

/// One register's freeze result in the AA-6(a) enforcement truth-table.
#[derive(Clone, Copy, Debug)]
pub struct IdFreezeRow {
    /// Register name (e.g. `ID_AA64ISAR0_EL1`).
    pub name: &'static str,
    /// The host-presented value before the freeze.
    pub host_value: u64,
    /// The below-host value installed via `KVM_SET_ONE_REG`.
    pub frozen_value: u64,
    /// The nibble (feature field) that was reduced.
    pub field_shift: u32,
    /// The value read back after the SET — what the guest's `mrs` observes (KVM
    /// emulates EL1 ID reads from exactly this).
    pub read_back: u64,
    /// The freeze held: `read_back == frozen_value` and the field is strictly below host.
    pub enforced: bool,
}

/// The AA-6(a) ID-register-freeze enforcement result.
#[derive(Clone, Debug)]
pub struct IdFreezeProof {
    /// One row per `ID_AA64*` register that carries a reducible feature.
    pub rows: Vec<IdFreezeRow>,
    /// A vCPU created WITHOUT `KVM_ARM_VCPU_PMU_V3` reads `ID_AA64DFR0_EL1.PMUVer` as 0 —
    /// KVM denies the guest its own PMU (the guest contract). The enforcement row for the
    /// counter/PMU surface.
    pub pmu_denied_without_feature: bool,
    /// `ID_AA64DFR0_EL1.PMUVer` a PMU-enabled disposable vCPU sees (the sanitised host PMU
    /// version), for contrast with the denied read.
    pub host_pmuver: u64,
}

/// Install a below-host reduced value in the first reducible feature field of `reg` and
/// confirm the guest-visible read-back holds it. `None` when the register carries no
/// reducible field (nothing to freeze).
#[cfg(target_os = "linux")]
fn install_id_freeze_field(
    vcpu_fd: libc::c_int,
    reg: u64,
    name: &'static str,
    skip_low_nibbles: u32,
) -> Result<Option<IdFreezeRow>, SysError> {
    let mut host_value: u64 = 0;
    let get = KvmOneReg {
        id: reg,
        addr: (&raw mut host_value) as u64,
    };
    // SAFETY: `vcpu_fd` is valid; `get.addr` points at a live u64 the kernel writes.
    if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get) } < 0 {
        return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64* freeze)"));
    }
    for nibble in skip_low_nibbles..16u32 {
        let shift = nibble * 4;
        let field = (host_value >> shift) & 0xF;
        if field == 0 || field == 0xF {
            continue;
        }
        let frozen = (host_value & !(0xFu64 << shift)) | ((field - 1) << shift);
        let set = KvmOneReg {
            id: reg,
            addr: (&raw const frozen) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `set.addr` points at a live u64 the kernel reads.
        let rc = unsafe { libc::ioctl(vcpu_fd, kvm::SET_ONE_REG as libc::c_ulong, &raw const set) };
        if rc < 0 {
            let e = errno();
            if e == libc::EINVAL || e == libc::EPERM || e == libc::ENOENT {
                continue;
            }
            return Err(err("ioctl(KVM_SET_ONE_REG, ID_AA64* freeze)"));
        }
        let mut read_back: u64 = 0;
        let get2 = KvmOneReg {
            id: reg,
            addr: (&raw mut read_back) as u64,
        };
        // SAFETY: `vcpu_fd` is valid; `get2.addr` points at a live u64 the kernel writes.
        if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get2) } < 0 {
            return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64* freeze readback)"));
        }
        if read_back == frozen {
            return Ok(Some(IdFreezeRow {
                name,
                host_value,
                frozen_value: frozen,
                field_shift: shift,
                read_back,
                enforced: true,
            }));
        }
        // Accepted but clamped back to host — not a real freeze; try the next field.
    }
    Ok(None)
}

/// Prove AA-6(a) ID-register freeze enforcement on a disposable VM+vCPU.
///
/// Installs a shrunk (below-host) synthetic model across the `ID_AA64*` surface and confirms
/// each frozen value survives read-back — the value a guest's EL1 `mrs` observes, since KVM
/// emulates ID reads from the vCPU's stored register. Also confirms the PMU is denied to a
/// vCPU without `KVM_ARM_VCPU_PMU_V3` (guest PMU reads mask to 0).
///
/// # Errors
/// [`SysError`] if a VM/vCPU could not be created/initialised or a register access failed.
#[cfg(target_os = "linux")]
pub fn id_freeze_proof() -> Result<IdFreezeProof, SysError> {
    let kvm_fd = open_kvm()?;
    // SAFETY: valid /dev/kvm fd; KVM_CREATE_VM returns a VM fd.
    let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
    if vm_fd < 0 {
        let e = err("ioctl(KVM_CREATE_VM)");
        // SAFETY: `kvm_fd` is valid and owned here.
        unsafe { libc::close(kvm_fd) };
        return Err(e);
    }
    let out = id_freeze_proof_on_vm(vm_fd);
    // SAFETY: both descriptors are valid and owned here.
    unsafe {
        libc::close(vm_fd);
        libc::close(kvm_fd);
    }
    out
}

#[cfg(target_os = "linux")]
fn id_freeze_proof_on_vm(vm_fd: libc::c_int) -> Result<IdFreezeProof, SysError> {
    // SAFETY: `vm_fd` is valid; KVM_CREATE_VCPU takes a vcpu index and returns a fd.
    let vcpu_fd = unsafe { libc::ioctl(vm_fd, kvm::CREATE_VCPU as libc::c_ulong, 0_u64) };
    if vcpu_fd < 0 {
        return Err(err("ioctl(KVM_CREATE_VCPU)"));
    }
    let out = id_freeze_proof_on_vcpu(vm_fd, vcpu_fd);
    // SAFETY: `vcpu_fd` is valid and owned here.
    unsafe { libc::close(vcpu_fd) };
    out
}

#[cfg(target_os = "linux")]
fn id_freeze_proof_on_vcpu(
    vm_fd: libc::c_int,
    vcpu_fd: libc::c_int,
) -> Result<IdFreezeProof, SysError> {
    // A featureless vCPU (NO PMU): the guest contract denies the guest its own PMU.
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
    // SAFETY: `vcpu_fd` is valid; `init` is fully initialised above.
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

    // Same surface as the writable-ID probe; PFR0's low four nibbles are EL0..3 support
    // (lowering them breaks the VM), so they are skipped.
    let relevant: &[(u64, u32, &str)] = &[
        (kvm::REG_ID_AA64PFR0_EL1, 4, "ID_AA64PFR0_EL1"),
        (kvm::REG_ID_AA64PFR1_EL1, 0, "ID_AA64PFR1_EL1"),
        (kvm::REG_ID_AA64ISAR0_EL1, 0, "ID_AA64ISAR0_EL1"),
        (kvm::REG_ID_AA64ISAR1_EL1, 0, "ID_AA64ISAR1_EL1"),
        (kvm::REG_ID_AA64ISAR2_EL1, 0, "ID_AA64ISAR2_EL1"),
        (kvm::REG_ID_AA64MMFR0_EL1, 0, "ID_AA64MMFR0_EL1"),
        (kvm::REG_ID_AA64MMFR1_EL1, 0, "ID_AA64MMFR1_EL1"),
        (kvm::REG_ID_AA64MMFR2_EL1, 0, "ID_AA64MMFR2_EL1"),
        (kvm::REG_ID_AA64DFR0_EL1, 0, "ID_AA64DFR0_EL1"),
        (kvm::REG_ID_AA64DFR1_EL1, 0, "ID_AA64DFR1_EL1"),
    ];
    let mut rows = Vec::new();
    for &(reg, skip, name) in relevant {
        if let Some(row) = install_id_freeze_field(vcpu_fd, reg, name, skip)? {
            rows.push(row);
        }
    }

    // PMU denial: this featureless vCPU reads ID_AA64DFR0_EL1.PMUVer (bits [11:8]) as 0.
    let mut dfr0: u64 = 0;
    let get = KvmOneReg {
        id: kvm::REG_ID_AA64DFR0_EL1,
        addr: (&raw mut dfr0) as u64,
    };
    // SAFETY: `vcpu_fd` is valid; `get.addr` points at a live u64 the kernel writes.
    if unsafe { libc::ioctl(vcpu_fd, kvm::GET_ONE_REG as libc::c_ulong, &raw const get) } < 0 {
        return Err(err("ioctl(KVM_GET_ONE_REG, ID_AA64DFR0_EL1 PMUVer)"));
    }
    let pmu_denied_without_feature = (dfr0 >> 8) & 0xF == 0;

    // For contrast: the sanitised host PMU version through a PMU-enabled vCPU.
    let host = read_host_id_registers()?;
    let host_pmuver = (host.id_aa64dfr0 >> 8) & 0xF;

    Ok(IdFreezeProof {
        rows,
        pmu_denied_without_feature,
        host_pmuver,
    })
}

// ============================================================================
// AA-6(b) vGIC save/restore round-trip proof (standalone; own VM/vCPU/vGIC).
// Additive: it touches no `Machine`, run-loop, or W^X code.
// ============================================================================

/// The redistributor SGI-frame private-interrupt (SGI/PPI, IDs 0–31) registers whose values
/// carry a guest's injection state — enable, pending, active, group, config, priority. Saved
/// and restored through `KVM_DEV_ARM_VGIC_GRP_REDIST_REGS`, whose GET/SET give absolute
/// migration-grade access (not the write-1-to-set MMIO semantics). Offsets are RD_base
/// relative; the SGI frame is one 64 KiB frame past RD_base.
#[cfg(target_os = "linux")]
const VGIC_REDIST_PRIVATE_REGS: &[(u64, &str)] = &[
    (0x1_0000 + 0x0080, "GICR_IGROUPR0"),
    (0x1_0000 + 0x0D00, "GICR_IGRPMODR0"),
    (0x1_0000 + 0x0100, "GICR_ISENABLER0"),
    (0x1_0000 + 0x0200, "GICR_ISPENDR0"),
    (0x1_0000 + 0x0300, "GICR_ISACTIVER0"),
    (0x1_0000 + 0x0C00, "GICR_ICFGR0"),
    (0x1_0000 + 0x0C04, "GICR_ICFGR1"),
    (0x1_0000 + 0x0400, "GICR_IPRIORITYR0"),
    (0x1_0000 + 0x0404, "GICR_IPRIORITYR1"),
    (0x1_0000 + 0x0408, "GICR_IPRIORITYR2"),
    (0x1_0000 + 0x040C, "GICR_IPRIORITYR3"),
    (0x1_0000 + 0x0410, "GICR_IPRIORITYR4"),
    (0x1_0000 + 0x0414, "GICR_IPRIORITYR5"),
    (0x1_0000 + 0x0418, "GICR_IPRIORITYR6"),
    (0x1_0000 + 0x041C, "GICR_IPRIORITYR7"),
];

/// The private-interrupt ID the round-trip injects (a PPI, matching AA-5's dedicated
/// clockevent PPI 20). Its ISENABLER0/ISPENDR0 bit distinguishes an injected state from a
/// fresh vGIC's quiescent default.
#[cfg(target_os = "linux")]
const VGIC_ROUNDTRIP_INTID: u32 = 20;

/// The AA-6(b) vGIC save/restore round-trip result.
#[derive(Clone, Debug)]
pub struct VgicRoundtrip {
    /// The injected private interrupt (enabled + pending on machine A).
    pub injected_intid: u32,
    /// Register labels saved, in order (parallel to the value vectors).
    pub labels: Vec<&'static str>,
    /// Machine A's saved private-IRQ register values.
    pub saved: Vec<u32>,
    /// A fresh machine B's values BEFORE restore.
    pub fresh_before: Vec<u32>,
    /// Machine B's values AFTER restoring A's save.
    pub fresh_after: Vec<u32>,
    /// Negative control: the fresh vGIC differed from the injected save before restore (so a
    /// match after restore is transfer, not coincidence).
    pub negative_control_differs: bool,
    /// The round-trip held: B after restore is byte-identical to A's save.
    pub roundtrip_identical: bool,
}

#[cfg(target_os = "linux")]
fn vgic_device_get(vgic_fd: libc::c_int, group: u32, attr: u64) -> Result<u32, SysError> {
    let mut value: u32 = 0;
    let da = KvmDeviceAttr {
        flags: 0,
        group,
        attr,
        addr: (&raw mut value) as u64,
    };
    // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u32 the GET accessor writes.
    if unsafe {
        libc::ioctl(
            vgic_fd,
            kvm::GET_DEVICE_ATTR as libc::c_ulong,
            &raw const da,
        )
    } < 0
    {
        return Err(err("ioctl(KVM_GET_DEVICE_ATTR, vGIC redist reg)"));
    }
    Ok(value)
}

#[cfg(target_os = "linux")]
fn vgic_device_set(
    vgic_fd: libc::c_int,
    group: u32,
    attr: u64,
    value: u32,
) -> Result<(), SysError> {
    let da = KvmDeviceAttr {
        flags: 0,
        group,
        attr,
        addr: (&raw const value) as u64,
    };
    // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u32 the SET accessor reads.
    if unsafe {
        libc::ioctl(
            vgic_fd,
            kvm::SET_DEVICE_ATTR as libc::c_ulong,
            &raw const da,
        )
    } < 0
    {
        return Err(err("ioctl(KVM_SET_DEVICE_ATTR, vGIC redist reg)"));
    }
    Ok(())
}

/// Build a disposable VM + single vCPU + initialised in-kernel vGICv3, run `f(vgic_fd)`, and
/// tear all three fds down on every path. Mirrors [`Machine::create_vgic`]'s ioctl sequence
/// without touching `Machine`.
#[cfg(target_os = "linux")]
fn with_vm_vcpu_vgic<T>(f: impl FnOnce(libc::c_int) -> Result<T, SysError>) -> Result<T, SysError> {
    let kvm_fd = open_kvm()?;
    // SAFETY: valid /dev/kvm fd; KVM_CREATE_VM returns a VM fd.
    let vm_fd = unsafe { libc::ioctl(kvm_fd, kvm::CREATE_VM as libc::c_ulong, 0_u64) };
    if vm_fd < 0 {
        let e = err("ioctl(KVM_CREATE_VM)");
        // SAFETY: `kvm_fd` is valid and owned here.
        unsafe { libc::close(kvm_fd) };
        return Err(e);
    }
    let body = || -> Result<T, SysError> {
        // SAFETY: `vm_fd` is valid; KVM_CREATE_VCPU takes a vcpu index and returns a fd.
        let vcpu_fd = unsafe { libc::ioctl(vm_fd, kvm::CREATE_VCPU as libc::c_ulong, 0_u64) };
        if vcpu_fd < 0 {
            return Err(err("ioctl(KVM_CREATE_VCPU)"));
        }
        let inner = || -> Result<T, SysError> {
            // The vCPU must be initialised before the vGIC's CTRL_INIT.
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
            // SAFETY: `vcpu_fd` is valid; `init` is fully initialised above.
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

            let mut dev = KvmCreateDevice {
                type_: kvm::DEV_TYPE_ARM_VGIC_V3,
                fd: 0,
                flags: 0,
            };
            // SAFETY: `vm_fd` is valid; KVM_CREATE_DEVICE fills `dev.fd`.
            if unsafe { libc::ioctl(vm_fd, kvm::CREATE_DEVICE as libc::c_ulong, &raw mut dev) } < 0
            {
                return Err(err("ioctl(KVM_CREATE_DEVICE, vGICv3)"));
            }
            let vgic_fd = dev.fd as libc::c_int;
            let with_vgic = || -> Result<T, SysError> {
                let dist = kvm::GICD_BASE;
                let redist = kvm::GICR_BASE;
                let set_addr = |attr: u64, value: &u64| -> Result<(), SysError> {
                    let da = KvmDeviceAttr {
                        flags: 0,
                        group: kvm::DEV_ARM_VGIC_GRP_ADDR,
                        attr,
                        addr: (value as *const u64) as u64,
                    };
                    // SAFETY: `vgic_fd` is valid; `da.addr` points at a live u64 the ADDR
                    // group reads.
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
                set_addr(kvm::VGIC_V3_ADDR_TYPE_DIST, &dist)?;
                set_addr(kvm::VGIC_V3_ADDR_TYPE_REDIST, &redist)?;
                let ctrl = KvmDeviceAttr {
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
                        &raw const ctrl,
                    )
                } < 0
                {
                    return Err(err("ioctl(KVM_DEV_ARM_VGIC_CTRL_INIT)"));
                }
                f(vgic_fd)
            };
            let out = with_vgic();
            // SAFETY: `vgic_fd` is valid and owned here.
            unsafe { libc::close(vgic_fd) };
            out
        };
        let out = inner();
        // SAFETY: `vcpu_fd` is valid and owned here.
        unsafe { libc::close(vcpu_fd) };
        out
    };
    let out = body();
    // SAFETY: both descriptors are valid and owned here.
    unsafe {
        libc::close(vm_fd);
        libc::close(kvm_fd);
    }
    out
}

/// Read the private-IRQ register bank from a vGIC in fixed order.
#[cfg(target_os = "linux")]
fn vgic_save_private(vgic_fd: libc::c_int) -> Result<Vec<u32>, SysError> {
    VGIC_REDIST_PRIVATE_REGS
        .iter()
        .map(|&(off, _)| vgic_device_get(vgic_fd, kvm::DEV_ARM_VGIC_GRP_REDIST_REGS, off))
        .collect()
}

/// Prove AA-6(b) vGIC injection state round-trips through save/restore.
///
/// Machine A enables + sets pending on PPI [`VGIC_ROUNDTRIP_INTID`] and its private-IRQ bank
/// is saved. A fresh machine B is read (negative control: it differs), the save is restored
/// into it, and B is read again — a bit-identical match proves save→restore fidelity.
///
/// # Errors
/// [`SysError`] on any VM/vCPU/vGIC construction or device-attribute access failure.
#[cfg(target_os = "linux")]
pub fn vgic_roundtrip_proof() -> Result<VgicRoundtrip, SysError> {
    let intid = VGIC_ROUNDTRIP_INTID;
    let bit = 1u32 << intid; // PPI 20 is in the first 32-bit private-IRQ word.
    let g = kvm::DEV_ARM_VGIC_GRP_REDIST_REGS;
    let en = 0x1_0000 + 0x0100u64; // GICR_ISENABLER0
    let pend = 0x1_0000 + 0x0200u64; // GICR_ISPENDR0

    // Machine A: inject (enable + pending on the PPI), then save the private-IRQ bank.
    let saved = with_vm_vcpu_vgic(|vgic_fd| {
        let cur_en = vgic_device_get(vgic_fd, g, en)?;
        vgic_device_set(vgic_fd, g, en, cur_en | bit)?;
        let cur_pend = vgic_device_get(vgic_fd, g, pend)?;
        vgic_device_set(vgic_fd, g, pend, cur_pend | bit)?;
        vgic_save_private(vgic_fd)
    })?;

    // Machine B: read fresh, restore A's save, read again.
    let (fresh_before, fresh_after) = with_vm_vcpu_vgic(|vgic_fd| {
        let before = vgic_save_private(vgic_fd)?;
        for (&(off, _), &value) in VGIC_REDIST_PRIVATE_REGS.iter().zip(saved.iter()) {
            vgic_device_set(vgic_fd, g, off, value)?;
        }
        let after = vgic_save_private(vgic_fd)?;
        Ok((before, after))
    })?;

    Ok(VgicRoundtrip {
        injected_intid: intid,
        labels: VGIC_REDIST_PRIVATE_REGS.iter().map(|&(_, l)| l).collect(),
        negative_control_differs: fresh_before != saved,
        roundtrip_identical: fresh_after == saved,
        saved,
        fresh_before,
        fresh_after,
    })
}

#[cfg(test)]
mod churner_validation_tests {
    use super::*;

    #[test]
    fn churner_refuses_an_empty_core_list() {
        assert!(MigrationChurner::start(current_tid(), Vec::new()).is_err());
    }

    #[test]
    fn churner_refuses_an_out_of_range_core() {
        let cpu_setsize = core::mem::size_of::<libc::cpu_set_t>() * 8;
        let cores = vec![0, u32::try_from(cpu_setsize).expect("CPU_SETSIZE fits u32")];
        assert!(MigrationChurner::start(current_tid(), cores).is_err());
    }
}
