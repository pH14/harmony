// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PmuBranchCounter` — the **box-only** backend-owned retired-conditional-branch
//! counter that drives `Backend::run_until`'s overflow-early phase
//! (`#[cfg(target_os = "linux")]`).
//!
//! It is the same `perf_event` counter as vmm-core's
//! [`PerfWorkCounter`](../../vmm-core/src/work_perf.rs) — `BR_INST_RETIRED.CONDITIONAL`
//! (`PERF_TYPE_RAW` config `0x1c4`), **`exclude_host = 1`** (count guest only, so
//! VM-exits/host work between exits add zero branches — the count-neutral-exit
//! property), **`pinned = 1`** (a counter that fails to schedule is a hard error,
//! never silent multiplexing), opened on the calling (CPU-pinned) vCPU thread
//! (`pid = 0`) — but with two additions the work-only counter lacks:
//!
//! 1. **sampling mode** (`sample_period` > 0) so the counter can *overflow*, and
//! 2. **async overflow notification** (`O_ASYNC` + `F_SETOWN_EX` to the vCPU TID +
//!    a no-op `SIGIO` handler installed without `SA_RESTART`) so an overflow
//!    delivers a signal that **`EINTR`s the in-flight `KVM_RUN`** on this thread —
//!    the host-side "kick" that breaks a busy-spinning guest out at the armed
//!    branch count. (This is *not* a guest PMI; signal-delivery latency is part of
//!    the skid the `skid_margin` bounds.)
//!
//! ## Central design decision (see IMPLEMENTATION.md)
//!
//! `run_until` is a `vmm-backend` method but the V-time work counter historically
//! lives in `vmm-core` (`work_perf`), and `vmm-backend` must not depend upward on
//! `vmm-core`. Rather than move/share that counter across the trait boundary (which
//! the layering forbids), the backend owns **this** counter. It is opened with the
//! identical event/flags/baseline as vmm-core's, on the same thread, so on a
//! deterministic guest stream the two read identical values — a deadline expressed
//! on vmm-core's work axis is honoured exactly here. That equality (same event,
//! same `exclude_host`, same reset discipline) is the key **box-validation
//! invariant**.
//!
//! ## Box-validation notes (cannot be exercised on the macOS dev host)
//!
//! The raw syscalls sit behind `#[cfg(not(miri))]` seams with `#[cfg(miri)]` stubs
//! (this counter is never *opened* under Miri). The overflow mechanism — that
//! `PERF_EVENT_IOC_PERIOD` re-arms the next overflow immediately (true on the
//! box's 5.17+ kernel), that the `SIGIO` reliably `EINTR`s `KVM_RUN`, and that the
//! ring buffer must be drained so long runs (gate 3: runc + Postgres) never stall
//! — is the foreman's box gate, called out in IMPLEMENTATION.md.

use crate::error::{BackendError, Result};

/// `PERF_TYPE_RAW`.
const PERF_TYPE_RAW: u32 = 4;
/// `BR_INST_RETIRED.CONDITIONAL` (event `0xC4`, umask `0x01`), Coffee Lake-S
/// (i9-9900K) — the exact event task 07 validated, identical to `work_perf`.
const RAW_BR_COND: u64 = 0x1c4;
/// `perf_event_attr` version-5 size (112 bytes).
const ATTR_SIZE_VER5: u32 = 112;
/// `PERF_FLAG_FD_CLOEXEC`.
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;

// perf_event_attr flag-word bits (include/uapi/linux/perf_event.h).
const F_DISABLED: u64 = 1 << 0;
const F_PINNED: u64 = 1 << 2;
const F_EXCLUDE_HOST: u64 = 1 << 19;

// read_format bits.
const FORMAT_TOTAL_TIME_ENABLED: u64 = 1 << 0;
const FORMAT_TOTAL_TIME_RUNNING: u64 = 1 << 1;

// PERF_EVENT_IOC_* (x86_64). `_IO('$', n)` for the arg-less ones, `_IOW('$', 4,
// __u64)` for PERIOD (dir=1, size=8, type=0x24, nr=4).
const IOC_ENABLE: libc::c_ulong = 0x2400;
const IOC_RESET: libc::c_ulong = 0x2403;
const IOC_PERIOD: libc::c_ulong = 0x4008_2404;

// `fcntl` commands / flags not all exported by `libc` — defined here like the
// `work_perf` PMU constants (no perf/fcntl wrapper crate is whitelisted).
/// `F_SETOWN_EX` (direct fd-owner to a specific thread).
const F_SETOWN_EX: libc::c_int = 15;
/// `f_owner_ex.type = F_OWNER_TID` (the owner is a thread, named by TID).
const F_OWNER_TID: libc::c_int = 0;
/// The signal an overflow delivers. `O_ASYNC` with the default `F_SETSIG`
/// generates `SIGIO`; a no-op handler (no `SA_RESTART`) turns it into a `KVM_RUN`
/// `EINTR` kick. (Repurposing `SIGIO` is safe in the single-vCPU VMM, which does
/// no other async I/O on this thread.)
const OVERFLOW_SIGNAL: libc::c_int = libc::SIGIO;

/// The sampling period installed when **disarmed**: large enough that no overflow
/// fires during plain counting / single-stepping, but a valid (non-zero) sampling
/// period so the event stays in sampling mode and `PERF_EVENT_IOC_PERIOD` keeps
/// working. (`2^56` guest branches ≈ never.)
const DISARM_PERIOD: u64 = 1 << 56;

/// Ring-buffer data pages (must be a power of two); plus one control page. Small
/// (overflow records with `sample_type = 0` are header-only) but drained on every
/// disarm so it never fills on a long run.
const RING_DATA_PAGES: usize = 8;

/// `perf_event_mmap_page` control-field byte offsets (include/uapi/linux/perf_event.h):
/// the data-ring head/tail are at fixed offsets within the first page. Draining =
/// `data_tail := data_head`. **Box-validation:** these offsets are the documented
/// uapi layout (head @ 1024, tail @ 1032); a uapi change would desync them.
const DATA_HEAD_OFF: usize = 1024;
const DATA_TAIL_OFF: usize = 1032;

/// `perf_event_attr`, version-5 layout (112 bytes); fields beyond what we set are
/// left zero. Identical layout to `vmm-core`'s `work_perf` (no perf wrapper crate
/// is whitelisted).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PerfEventAttr {
    type_: u32,
    size: u32,
    config: u64,
    sample_period: u64,
    sample_type: u64,
    read_format: u64,
    flags: u64,
    wakeup_events: u32,
    bp_type: u32,
    config1: u64,
    config2: u64,
    branch_sample_type: u64,
    sample_regs_user: u64,
    sample_stack_user: u32,
    clockid: i32,
    sample_regs_intr: u64,
    aux_watermark: u32,
    sample_max_stack: u16,
    reserved_2: u16,
}

const _: () = assert!(std::mem::size_of::<PerfEventAttr>() == ATTR_SIZE_VER5 as usize);

/// A guest-only retired-conditional-branch counter on the calling (vCPU) thread,
/// in **sampling** mode so it can overflow-kick `KVM_RUN`. Owns one `perf_event`
/// fd and its overflow ring-buffer mmap.
pub(crate) struct PmuBranchCounter {
    fd: i32,
    /// The overflow ring-buffer mapping (1 control + `RING_DATA_PAGES` data pages).
    ring: *mut libc::c_void,
    ring_len: usize,
}

impl PmuBranchCounter {
    /// Open + enable the pinned, guest-only, **sampling** branch counter on the
    /// calling thread (which must be CPU-pinned, `docs/BOX-PINNING.md`), wired for
    /// async overflow signals.
    ///
    /// # Errors
    /// [`BackendError::Io`] if `perf_event_open` / `mmap` / `fcntl` / the enable
    /// ioctl fails (e.g. `kernel.perf_event_paranoid` too high without root, or no
    /// Intel PMU). Opening is **non-fatal to the rest of the backend**: the caller
    /// stores the failure and only `run_until` surfaces it (a `run`-only path —
    /// M1/M2/corpus — never opens this counter).
    pub(crate) fn open() -> Result<PmuBranchCounter> {
        install_overflow_handler();
        let attr = PerfEventAttr {
            type_: PERF_TYPE_RAW,
            size: ATTR_SIZE_VER5,
            config: RAW_BR_COND,
            // Sampling mode (non-zero period) so the counter can overflow; disarmed
            // far out until `arm_overflow` sets the real period.
            sample_period: DISARM_PERIOD,
            // We never read sample records (only want the wakeup), so no sample_type.
            sample_type: 0,
            read_format: FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
            flags: F_DISABLED | F_PINNED | F_EXCLUDE_HOST,
            // Wake (and signal) after a single overflow record.
            wakeup_events: 1,
            ..Default::default()
        };
        // SAFETY (task-21 P3 grant, extended for task 47): a properly sized, fully
        // initialized attr; pid=0/cpu=-1 attaches to the calling thread.
        let fd = unsafe { perf_event_open(&attr, PERF_FLAG_FD_CLOEXEC) };
        if fd < 0 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        let page = page_size();
        let ring_len = page * (1 + RING_DATA_PAGES);
        // SAFETY: map the perf overflow ring buffer at offset 0; `ring_len` is
        // (1 control + 2^n data) pages. Returns Err (never MAP_FAILED) on failure.
        let ring = match unsafe { mmap_ring(fd, ring_len) } {
            Ok(p) => p,
            Err(e) => {
                // SAFETY: close the fd we just opened on the mmap-failure path.
                unsafe { close_fd(fd) };
                return Err(e);
            }
        };
        let counter = PmuBranchCounter { fd, ring, ring_len };
        // Route overflow signals to THIS thread; arm async generation.
        counter.wire_overflow_signal()?;
        counter.ioctl_none(IOC_ENABLE)?;
        Ok(counter)
    }

    /// Direct the fd's async (`SIGIO`) overflow notification to the calling thread.
    fn wire_overflow_signal(&self) -> Result<()> {
        // SAFETY: fcntl on our owned perf fd; `gettid()` names the current thread,
        // `f_owner_ex` is fully initialized and valid for the call.
        let rc = unsafe {
            let owner = FOwnerEx {
                type_: F_OWNER_TID,
                pid: gettid(),
            };
            if fcntl_setown_ex(self.fd, &owner) < 0 {
                return Err(BackendError::Io(std::io::Error::last_os_error()));
            }
            fcntl_set_async(self.fd)
        };
        if rc < 0 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Issue an argument-less perf ioctl (`ENABLE` / `RESET`).
    fn ioctl_none(&self, req: libc::c_ulong) -> Result<()> {
        // SAFETY: argument-less perf ioctl on our owned, valid perf fd.
        let rc = unsafe { perf_ioctl_none(self.fd, req) };
        if rc < 0 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Set the sampling period (events until the next overflow).
    fn set_period(&self, period: u64) -> Result<()> {
        // SAFETY: `IOC_PERIOD` reads a `__u64` from `&period` (valid for the call).
        let rc = unsafe { perf_ioctl_u64(self.fd, IOC_PERIOD, period) };
        if rc < 0 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// The current cumulative guest-branch count (V-time *work*). Verifies the
    /// pinned counter scheduled (`time_enabled == time_running`).
    ///
    /// # Errors
    /// [`BackendError::Io`] on a short read; [`BackendError::Internal`] if the
    /// counter multiplexed (its read is not a trustworthy guest-branch count).
    pub(crate) fn work(&self) -> Result<u64> {
        let mut buf = [0u64; 3]; // value, time_enabled, time_running
        // SAFETY: reading 24 bytes into a 24-byte buffer from our owned perf fd.
        let n = unsafe { perf_read(self.fd, buf.as_mut_ptr(), 24) };
        if n != 24 {
            return Err(BackendError::Io(std::io::Error::last_os_error()));
        }
        if buf[1] != buf[2] {
            return Err(BackendError::Internal(
                "perf counter multiplexed (time_enabled != time_running)",
            ));
        }
        Ok(buf[0])
    }

    /// Reset the cumulative count to 0 (snapshot restore: the hardware counter
    /// restarts at 0 and the restored VClock carries the effective V-time in
    /// `vns_base`, matching vmm-core's work-counter reset).
    pub(crate) fn reset(&mut self) -> Result<()> {
        self.ioctl_none(IOC_RESET)
    }

    /// Arm the next overflow to fire at absolute work count `armed_at`: program the
    /// period to `armed_at − work()` more events (≥ 1). The counter keeps counting
    /// (it is **not** disabled), so `work()` and `single_step` stay accurate.
    ///
    /// # Errors
    /// As [`Self::work`] / the period ioctl.
    pub(crate) fn arm_overflow(&self, armed_at: u64) -> Result<()> {
        let cur = self.work()?;
        // The planner only arms strictly ahead of `now`; clamp to ≥1 defensively
        // (a 0 period is invalid and `armed_at <= cur` should overflow at once).
        let period = armed_at.saturating_sub(cur).max(1);
        self.set_period(period)
    }

    /// Disarm: push the overflow far out and drain the ring buffer so a long run
    /// never stalls on a full buffer.
    pub(crate) fn disarm(&self) -> Result<()> {
        self.set_period(DISARM_PERIOD)?;
        self.drain_ring();
        Ok(())
    }

    /// Drain consumed overflow records: `data_tail := data_head`. A plain volatile
    /// store is sufficient here (single producer = the kernel, single consumer =
    /// this thread; we only need the tail to advance so the buffer never fills).
    fn drain_ring(&self) {
        // SAFETY: `ring` maps `ring_len` ≥ one control page; the head/tail live in
        // that first page at the documented uapi offsets. Volatile to defeat
        // reordering against the kernel's writes.
        unsafe {
            let base = self.ring.cast::<u8>();
            let head = std::ptr::read_volatile(base.add(DATA_HEAD_OFF).cast::<u64>());
            std::ptr::write_volatile(base.add(DATA_TAIL_OFF).cast::<u64>(), head);
        }
    }
}

impl Drop for PmuBranchCounter {
    fn drop(&mut self) {
        // SAFETY: unmap the ring and close the fd exactly once each. Excluded under
        // Miri (neither is ever created there).
        #[cfg(not(miri))]
        unsafe {
            libc::munmap(self.ring, self.ring_len);
            libc::close(self.fd);
        }
    }
}

/// Install the process-wide no-op `SIGIO` handler exactly once. Without
/// `SA_RESTART`, a delivered `SIGIO` makes the interrupted `KVM_RUN` ioctl return
/// `EINTR` (the kick) instead of auto-restarting.
fn install_overflow_handler() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: install a minimal handler for `OVERFLOW_SIGNAL`; the `sigaction`
        // struct is zero-initialized then fully populated, the handler is a valid
        // `extern "C"` fn, and we pass a null `oldact`.
        #[cfg(not(miri))]
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = noop_handler as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            sa.sa_flags = 0; // crucially NOT SA_RESTART
            libc::sigaction(OVERFLOW_SIGNAL, &sa, std::ptr::null_mut());
        }
    });
}

/// No-op signal handler: its only job is to make `KVM_RUN` return `EINTR`.
extern "C" fn noop_handler(_sig: libc::c_int) {}

/// `f_owner_ex` (fcntl.h): direct an fd's signals to a specific thread/process.
#[repr(C)]
struct FOwnerEx {
    type_: libc::c_int,
    pid: libc::c_int,
}

/// Default ring page size; 4 KiB on the box.
fn page_size() -> usize {
    // SAFETY: `sysconf` is always safe to call; clamp a nonsensical result.
    #[cfg(not(miri))]
    {
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as usize } else { 4096 }
    }
    #[cfg(miri)]
    {
        4096
    }
}

// ---------------------------------------------------------------------------
// Raw syscall seams (un-Miri-able): real on the box, stubbed under Miri so the
// crate compiles for `cargo miri test` (PmuBranchCounter is never opened there).
// ---------------------------------------------------------------------------

/// `perf_event_open(attr, pid=0, cpu=-1, group_fd=-1, flags)`.
///
/// # Safety
/// `attr` must point to a valid, fully-initialized `perf_event_attr`.
#[cfg(not(miri))]
unsafe fn perf_event_open(attr: *const PerfEventAttr, flags: libc::c_ulong) -> i32 {
    // SAFETY: the syscall reads `*attr`; pid=0/cpu=-1 attach to the calling thread.
    unsafe { libc::syscall(libc::SYS_perf_event_open, attr, 0, -1, -1, flags) as i32 }
}
#[cfg(miri)]
unsafe fn perf_event_open(_attr: *const PerfEventAttr, _flags: libc::c_ulong) -> i32 {
    -1
}

/// `mmap(NULL, len, RW, SHARED, fd, 0)` for the perf overflow ring. Returns Err
/// (never MAP_FAILED) on failure.
///
/// # Safety
/// `fd` must be a valid perf-event fd and `len` a (1 + 2^n)-page size.
#[cfg(not(miri))]
unsafe fn mmap_ring(fd: i32, len: usize) -> Result<*mut libc::c_void> {
    // SAFETY: standard shared mapping of the perf fd at offset 0.
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
    Ok(p)
}
#[cfg(miri)]
unsafe fn mmap_ring(_fd: i32, _len: usize) -> Result<*mut libc::c_void> {
    Err(BackendError::Internal("mmap unavailable under miri"))
}

/// `fcntl(fd, F_SETOWN_EX, &owner)`.
///
/// # Safety
/// `fd` valid; `owner` points to a valid `f_owner_ex` for the call.
#[cfg(not(miri))]
unsafe fn fcntl_setown_ex(fd: i32, owner: *const FOwnerEx) -> libc::c_int {
    // SAFETY: variadic fcntl reading the `f_owner_ex` pointer arg.
    unsafe { libc::fcntl(fd, F_SETOWN_EX, owner) }
}
#[cfg(miri)]
unsafe fn fcntl_setown_ex(_fd: i32, _owner: *const FOwnerEx) -> libc::c_int {
    -1
}

/// `fcntl(fd, F_SETFL, fcntl(fd, F_GETFL) | O_ASYNC)`.
///
/// # Safety
/// `fd` must be a valid perf-event fd.
#[cfg(not(miri))]
unsafe fn fcntl_set_async(fd: i32) -> libc::c_int {
    // SAFETY: read then OR-in O_ASYNC on the fd's status flags.
    unsafe {
        let cur = libc::fcntl(fd, libc::F_GETFL);
        if cur < 0 {
            return cur;
        }
        libc::fcntl(fd, libc::F_SETFL, cur | libc::O_ASYNC)
    }
}
#[cfg(miri)]
unsafe fn fcntl_set_async(_fd: i32) -> libc::c_int {
    -1
}

/// `gettid()`.
#[cfg(not(miri))]
fn gettid() -> libc::c_int {
    // SAFETY: gettid is always safe.
    unsafe { libc::gettid() }
}
#[cfg(miri)]
fn gettid() -> libc::c_int {
    0
}

/// `ioctl(fd, req, 0)` — the arg-less perf ioctls.
///
/// # Safety
/// `fd` a valid perf-event fd; `req` an arg-less `PERF_EVENT_IOC_*`.
#[cfg(not(miri))]
unsafe fn perf_ioctl_none(fd: i32, req: libc::c_ulong) -> libc::c_int {
    // SAFETY: arg-less perf ioctl on a valid fd.
    unsafe { libc::ioctl(fd, req, 0) }
}
#[cfg(miri)]
unsafe fn perf_ioctl_none(_fd: i32, _req: libc::c_ulong) -> libc::c_int {
    -1
}

/// `ioctl(fd, PERF_EVENT_IOC_PERIOD, &period)`.
///
/// # Safety
/// `fd` a valid perf-event fd.
#[cfg(not(miri))]
unsafe fn perf_ioctl_u64(fd: i32, req: libc::c_ulong, value: u64) -> libc::c_int {
    // SAFETY: the ioctl reads a `__u64` from `&value` (valid for the call).
    unsafe { libc::ioctl(fd, req, &value as *const u64) }
}
#[cfg(miri)]
unsafe fn perf_ioctl_u64(_fd: i32, _req: libc::c_ulong, _value: u64) -> libc::c_int {
    -1
}

/// `read(fd, buf, count)`.
///
/// # Safety
/// `buf` must point to `count` writable bytes; `fd` a valid perf-event fd.
#[cfg(not(miri))]
unsafe fn perf_read(fd: i32, buf: *mut u64, count: usize) -> isize {
    // SAFETY: the kernel writes up to `count` bytes into `buf` (24 = 3×u64).
    unsafe { libc::read(fd, buf.cast::<libc::c_void>(), count) }
}
#[cfg(miri)]
unsafe fn perf_read(_fd: i32, _buf: *mut u64, _count: usize) -> isize {
    -1
}

/// `close(fd)`.
///
/// # Safety
/// `fd` must be an owned, open fd.
#[cfg(not(miri))]
unsafe fn close_fd(fd: i32) {
    // SAFETY: closing our owned fd once on an error path.
    unsafe {
        libc::close(fd);
    }
}
#[cfg(miri)]
unsafe fn close_fd(_fd: i32) {}
