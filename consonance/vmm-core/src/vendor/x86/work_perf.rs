// SPDX-License-Identifier: AGPL-3.0-or-later
//! `PerfWorkCounter` — the **box-only** [`WorkSource`](crate::work::WorkSource):
//! a `perf_event` counter of guest retired conditional branches
//! (`BR_INST_RETIRED.CONDITIONAL`, event `0xC4` umask `0x01`), the V-time work
//! source (`#[cfg(target_os = "linux")]`).
//!
//! Productionizes task 07's measured config (`spikes/pmu-count/`, GO verdict):
//! `PERF_TYPE_RAW` config `0x1c4`, **`exclude_host = 1`** (count only guest
//! mode, both CPLs — so VM-exits and all host work between exits add **zero**
//! guest branches, i.e. the counter is count-neutral across exits, the property
//! the spike's experiment 3 proved), **`pinned = 1`** (a counter that fails to
//! schedule is a hard error, never silent multiplexing), and a read that
//! verifies `time_enabled == time_running` on every call. Opened on the calling
//! (vCPU) thread — `pid = 0` — which must be CPU-pinned per `docs/BOX-PINNING.md`.
//!
//! This task wires only the **work read** (V-time advance, RDTSC); the
//! overflow-arm + single-step precise-injection path (`vtime::CpuBackend`'s
//! `run_until_overflow`/`single_step`, the `Backend::run_until` Phase 2) is not
//! built here — it needs the lapic injection seam and is deferred.
//!
//! Like `vmm-backend`'s `kvm_sys`, this module is box-only syscall orchestration
//! (it cannot run without `perf_event` on bare-metal Intel) and is excluded from
//! the coverage + mutation gates; the portable seam it implements
//! ([`WorkSource`](crate::work::WorkSource)) is unit-tested via
//! [`ScriptedWork`](crate::work::ScriptedWork). The raw syscalls sit behind
//! `#[cfg(not(miri))]` seams with `#[cfg(miri)]` stubs so the crate still
//! compiles under `cargo miri test`.

use crate::work::{WorkError, WorkSource};

/// `PERF_TYPE_RAW`.
const PERF_TYPE_RAW: u32 = 4;
/// `BR_INST_RETIRED.CONDITIONAL` (event `0xC4`, umask `0x01`), Skylake-family
/// (incl. the box's Coffee Lake-S i9-9900K). The exact event task 07 validated.
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

// PERF_EVENT_IOC_* (x86_64): _IO('$', 0) ENABLE, _IO('$', 3) RESET.
const IOC_ENABLE: libc::c_ulong = 0x2400;
const IOC_RESET: libc::c_ulong = 0x2403;

/// `perf_event_attr`, version-5 layout (112 bytes); fields beyond what we set are
/// left zero. Defined manually (no perf wrapper crate is whitelisted), matching
/// `spikes/pmu-count/src/perf.rs`.
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

/// A guest-only retired-conditional-branch counter on the calling (vCPU) thread.
/// Owns one `perf_event` fd; reads the cumulative count as V-time *work*.
pub struct PerfWorkCounter {
    fd: i32,
}

impl PerfWorkCounter {
    /// Open + enable the pinned, guest-only `BR_INST_RETIRED.CONDITIONAL` counter
    /// on the calling thread (which must be CPU-pinned, `docs/BOX-PINNING.md`).
    ///
    /// # Errors
    /// [`WorkError::Io`] if `perf_event_open` / `PERF_EVENT_IOC_ENABLE` fails
    /// (e.g. `kernel.perf_event_paranoid` too high without root, or no Intel PMU).
    pub fn open() -> Result<PerfWorkCounter, WorkError> {
        let attr = PerfEventAttr {
            type_: PERF_TYPE_RAW,
            size: ATTR_SIZE_VER5,
            config: RAW_BR_COND,
            read_format: FORMAT_TOTAL_TIME_ENABLED | FORMAT_TOTAL_TIME_RUNNING,
            // disabled at open, pinned, guest-only (exclude_host): host work
            // between exits adds zero guest branches (count-neutral exits).
            flags: F_DISABLED | F_PINNED | F_EXCLUDE_HOST,
            ..Default::default()
        };
        // SAFETY (task-21 P3 grant: perf_event_open): a properly sized, fully
        // initialized attr; pid=0/cpu=-1 attaches to the calling thread.
        let fd = unsafe { perf_event_open(&attr, PERF_FLAG_FD_CLOEXEC) };
        if fd < 0 {
            return Err(WorkError::Io(std::io::Error::last_os_error()));
        }
        let counter = PerfWorkCounter { fd };
        counter.ioctl_none(IOC_ENABLE)?;
        Ok(counter)
    }

    /// Issue an argument-less perf ioctl (`ENABLE` / `RESET`).
    fn ioctl_none(&self, req: libc::c_ulong) -> Result<(), WorkError> {
        // SAFETY: argument-less perf ioctl on our owned, valid perf fd.
        let rc = unsafe { perf_ioctl(self.fd, req) };
        if rc < 0 {
            return Err(WorkError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

impl WorkSource for PerfWorkCounter {
    fn work(&self) -> Result<u64, WorkError> {
        let mut buf = [0u64; 3]; // value, time_enabled, time_running
        // SAFETY: reading 24 bytes into a 24-byte aligned buffer from our perf fd.
        let n = unsafe { perf_read(self.fd, buf.as_mut_ptr(), 24) };
        if n != 24 {
            // A pinned counter that failed to schedule returns 0 bytes — never a
            // trustworthy guest-branch count, so reject rather than use it.
            return Err(WorkError::Untrustworthy(
                "perf read short (pinned counter failed to schedule)",
            ));
        }
        if buf[1] != buf[2] {
            return Err(WorkError::Untrustworthy(
                "counter multiplexed (time_enabled != time_running)",
            ));
        }
        Ok(buf[0])
    }

    fn reset(&mut self) -> Result<(), WorkError> {
        // Snapshot restore: the hardware counter restarts at 0; the restored
        // VClock carries the snapshot's effective V-time in vns_base.
        self.ioctl_none(IOC_RESET)
    }

    fn start_run(&mut self) -> Result<(), WorkError> {
        // Run start: the counter is enabled at open and counts guest branches on the
        // shared vCPU thread, so it may have accumulated a *coexisting* VM's branches
        // since this VM was spawned (e.g. `compare_runs` spawns both machines, then
        // runs each in turn — the second VM's counter was live throughout the first's
        // run). Zero it here so work measures only this run's guest execution. In the
        // single-VM case the counter is already ~0 (exclude_host: no guest ran), so
        // this is a no-op and leaves P6/M2 byte-identical.
        self.ioctl_none(IOC_RESET)
    }
}

impl Drop for PerfWorkCounter {
    fn drop(&mut self) {
        // SAFETY: closing our owned fd exactly once. Excluded under Miri (the fd
        // is never opened there).
        #[cfg(not(miri))]
        unsafe {
            libc::close(self.fd);
        }
    }
}

// ---------------------------------------------------------------------------
// Raw syscall seams (un-Miri-able): real on the box, stubbed under Miri so the
// crate compiles for `cargo miri test` (PerfWorkCounter is never opened there).
// ---------------------------------------------------------------------------

/// `perf_event_open(attr, pid=0, cpu=-1, group_fd=-1, flags)`.
///
/// # Safety
/// `attr` must point to a valid, fully-initialized `perf_event_attr`.
#[cfg(not(miri))]
unsafe fn perf_event_open(attr: *const PerfEventAttr, flags: libc::c_ulong) -> i32 {
    // SAFETY: the perf_event_open syscall reads `*attr`; pid=0/cpu=-1 attach to
    // the calling thread on any core.
    unsafe { libc::syscall(libc::SYS_perf_event_open, attr, 0, -1, -1, flags) as i32 }
}

/// Miri stub (never reached: the live counter is opened only on the box).
#[cfg(miri)]
unsafe fn perf_event_open(_attr: *const PerfEventAttr, _flags: libc::c_ulong) -> i32 {
    -1
}

/// `ioctl(fd, req, 0)`.
///
/// # Safety
/// `fd` must be a valid perf-event fd; `req` an argument-less `PERF_EVENT_IOC_*`.
#[cfg(not(miri))]
unsafe fn perf_ioctl(fd: i32, req: libc::c_ulong) -> libc::c_int {
    // SAFETY: argument-less perf ioctl on a valid fd.
    unsafe { libc::ioctl(fd, req, 0) }
}

/// Miri stub.
#[cfg(miri)]
unsafe fn perf_ioctl(_fd: i32, _req: libc::c_ulong) -> libc::c_int {
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

/// Miri stub.
#[cfg(miri)]
unsafe fn perf_read(_fd: i32, _buf: *mut u64, _count: usize) -> isize {
    -1
}
