// SPDX-License-Identifier: AGPL-3.0-or-later
//! The syscall half of the counter plumbing: `perf_event_open`, the overflow-ring
//! mapping, and thread pinning.
//!
//! Every function here issues a syscall that cannot run without perf on the chip,
//! so the coverage and mutation oracles never reach it; it is excluded from both
//! and verified on the chip instead. The configuration it hands the kernel and the
//! ring arithmetic it drives are the portable, gate-covered [`crate::perf`].

use crate::perf::{
    ATTR_SIZE_VER5, PerfEventAttr, RingScan, Scope, counting_attr, sampling_attr, scan_ring_at,
};

/// `PERF_FLAG_FD_CLOEXEC`.
const PERF_FLAG_FD_CLOEXEC: libc::c_ulong = 8;

// PERF_EVENT_IOC_*: `_IO('$', n)` for the argument-less commands, and
// `_IOW('$', 4, __u64)` for the period.
const IOC_ENABLE: libc::c_ulong = 0x2400;
const IOC_DISABLE: libc::c_ulong = 0x2401;
const IOC_REFRESH: libc::c_ulong = 0x2402;
const IOC_RESET: libc::c_ulong = 0x2403;
const IOC_PERIOD: libc::c_ulong = 0x4008_2404;

/// Data pages behind the overflow ring; a power of two, plus one control page.
/// Each sample is a header and one value, so a single data page holds hundreds
/// and the ring is drained after every arm.
const RING_DATA_PAGES: usize = 8;

/// A refusal from the counter plumbing.
#[derive(Debug, thiserror::Error)]
pub enum PerfError {
    /// `perf_event_open` failed.
    #[error("perf_event_open(config={config:#x}) failed: {source}")]
    Open {
        /// The event config that was requested.
        config: u64,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// An ioctl on the counter failed.
    #[error("{what} on the counter failed: {source}")]
    Ioctl {
        /// Which command failed.
        what: &'static str,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// Reading the counter failed or returned a short read.
    #[error("reading the counter failed: {0}")]
    Read(String),
    /// Mapping the overflow ring failed.
    #[error("mapping the overflow ring failed: {0}")]
    Mmap(std::io::Error),
    /// Pinning the measurement thread failed.
    #[error("pinning to core {core} failed: {source}")]
    Pin {
        /// The core that was requested.
        core: usize,
        /// The operating-system error.
        source: std::io::Error,
    },
    /// The thread is not where it asked to be.
    #[error("pin verification failed: asked for core {want}, running on core {got}")]
    PinVerify {
        /// The requested core.
        want: usize,
        /// The core the thread is actually on.
        got: i32,
    },
}

/// One read of a counting event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimedRead {
    /// The count.
    pub value: u64,
    /// Nanoseconds the event was enabled.
    pub time_enabled: u64,
    /// Nanoseconds the event was actually on the hardware.
    pub time_running: u64,
}

impl TimedRead {
    /// Whether the counter was time-shared with another event. A pinned counter
    /// must never be, so a true here invalidates the count.
    #[must_use]
    pub fn multiplexed(&self) -> bool {
        self.time_enabled != self.time_running
    }
}

/// A `perf_event` counter on the calling thread, and the overflow ring when it
/// was opened in sampling mode.
pub struct PerfCounter {
    fd: i32,
    ring: *mut libc::c_void,
    ring_len: usize,
    page: usize,
}

impl PerfCounter {
    /// Open a counting event on the calling thread.
    ///
    /// # Errors
    /// [`PerfError::Open`] when the kernel refuses the event.
    pub fn open_counting(config: u64, scope: Scope) -> Result<PerfCounter, PerfError> {
        let attr = counting_attr(config, scope);
        let fd = open_fd(&attr, config)?;
        Ok(PerfCounter {
            fd,
            ring: libc::MAP_FAILED,
            ring_len: 0,
            page: 0,
        })
    }

    /// Open a sampling event on the calling thread and map its overflow ring.
    ///
    /// # Errors
    /// [`PerfError::Open`] when the kernel refuses the event,
    /// [`PerfError::Mmap`] when the ring cannot be mapped.
    pub fn open_sampling(config: u64, scope: Scope, period: u64) -> Result<PerfCounter, PerfError> {
        let attr = sampling_attr(config, scope, period);
        let fd = open_fd(&attr, config)?;
        // SAFETY: a plain sysconf query with no pointer arguments.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap_or(4096);
        let ring_len = page * (1 + RING_DATA_PAGES);
        // SAFETY: mapping the kernel's ring for this fd at a kernel-chosen
        // address, with the length the uapi requires (one control page plus a
        // power-of-two count of data pages).
        let ring = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                ring_len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                0,
            )
        };
        if std::ptr::eq(ring, libc::MAP_FAILED) {
            let err = std::io::Error::last_os_error();
            // SAFETY: `fd` is the descriptor this function just opened and has
            // not handed to anyone.
            unsafe { libc::close(fd) };
            return Err(PerfError::Mmap(err));
        }
        Ok(PerfCounter {
            fd,
            ring,
            ring_len,
            page,
        })
    }

    /// Zero the counter.
    ///
    /// # Errors
    /// [`PerfError::Ioctl`] when the kernel refuses.
    pub fn reset(&self) -> Result<(), PerfError> {
        self.ioctl_bare(IOC_RESET, "reset")
    }

    /// Start counting.
    ///
    /// # Errors
    /// [`PerfError::Ioctl`] when the kernel refuses.
    pub fn enable(&self) -> Result<(), PerfError> {
        self.ioctl_bare(IOC_ENABLE, "enable")
    }

    /// Stop counting.
    ///
    /// # Errors
    /// [`PerfError::Ioctl`] when the kernel refuses.
    pub fn disable(&self) -> Result<(), PerfError> {
        self.ioctl_bare(IOC_DISABLE, "disable")
    }

    /// Install a sampling period, which also resets the countdown to it, so an
    /// arm fires after exactly `period` and not after whatever was left over.
    ///
    /// # Errors
    /// [`PerfError::Ioctl`] when the kernel refuses.
    pub fn set_period(&self, period: u64) -> Result<(), PerfError> {
        // SAFETY: the period command takes a pointer to one u64, which is what
        // is passed.
        let rc = unsafe { libc::ioctl(self.fd, IOC_PERIOD, std::ptr::from_ref(&period)) };
        if rc < 0 {
            return Err(PerfError::Ioctl {
                what: "set period",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Enable the counter for exactly `count` overflows, after which the kernel
    /// disables it. This is what makes one arm one overflow.
    ///
    /// # Errors
    /// [`PerfError::Ioctl`] when the kernel refuses.
    pub fn refresh(&self, count: i32) -> Result<(), PerfError> {
        // SAFETY: the refresh command takes an int by value.
        let rc = unsafe { libc::ioctl(self.fd, IOC_REFRESH, count) };
        if rc < 0 {
            return Err(PerfError::Ioctl {
                what: "refresh",
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Read the count with its enabled and running times.
    ///
    /// # Errors
    /// [`PerfError::Read`] on a failed or short read.
    pub fn read_timed(&self) -> Result<TimedRead, PerfError> {
        let mut buf = [0u64; 3];
        // SAFETY: reading exactly the three u64s the timed read format returns
        // into a buffer of that size.
        let got = unsafe {
            libc::read(
                self.fd,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                std::mem::size_of_val(&buf),
            )
        };
        if got != std::mem::size_of_val(&buf) as isize {
            return Err(PerfError::Read(format!(
                "wanted {} bytes, got {got}: {}",
                std::mem::size_of_val(&buf),
                std::io::Error::last_os_error()
            )));
        }
        Ok(TimedRead {
            value: buf[0],
            time_enabled: buf[1],
            time_running: buf[2],
        })
    }

    /// Walk and drain the overflow ring. Returns nothing counted when the
    /// counter was opened in counting mode, which has no ring.
    #[must_use]
    pub fn scan_ring(&self) -> RingScan {
        if std::ptr::eq(self.ring, libc::MAP_FAILED) || self.ring_len == 0 {
            return RingScan::default();
        }
        // SAFETY: `ring` is this counter's live mapping of `ring_len` bytes,
        // laid out as one control page of `page` bytes plus the data area, which
        // is what the walk requires.
        unsafe { scan_ring_at(self.ring.cast::<u8>(), self.page, self.ring_len - self.page) }
    }

    /// Issue an argument-less ioctl.
    fn ioctl_bare(&self, request: libc::c_ulong, what: &'static str) -> Result<(), PerfError> {
        // SAFETY: these commands take no argument; a zero is passed for the
        // variadic slot as the uapi expects.
        let rc = unsafe { libc::ioctl(self.fd, request, 0) };
        if rc < 0 {
            return Err(PerfError::Ioctl {
                what,
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }
}

impl Drop for PerfCounter {
    fn drop(&mut self) {
        if !std::ptr::eq(self.ring, libc::MAP_FAILED) && self.ring_len > 0 {
            // SAFETY: unmapping this counter's own mapping, once.
            unsafe { libc::munmap(self.ring, self.ring_len) };
        }
        // SAFETY: closing this counter's own descriptor, once.
        unsafe { libc::close(self.fd) };
    }
}

/// Hand one attribute to `perf_event_open` for the calling thread on whatever
/// core it is pinned to.
fn open_fd(attr: &PerfEventAttr, config: u64) -> Result<i32, PerfError> {
    // SAFETY: `attr` is a fully-initialized version-5 attribute of the size its
    // own `size` field declares; pid 0 is the calling thread and cpu -1 follows
    // it, which is why the caller pins the thread itself.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_perf_event_open,
            std::ptr::from_ref(attr),
            0,
            -1,
            -1,
            PERF_FLAG_FD_CLOEXEC,
        )
    };
    let fd = i32::try_from(rc).unwrap_or(-1);
    if fd < 0 {
        return Err(PerfError::Open {
            config,
            source: std::io::Error::last_os_error(),
        });
    }
    debug_assert_eq!(u32::from(attr_size(attr)), ATTR_SIZE_VER5);
    Ok(fd)
}

/// The attribute's declared size, read back through its own layout so the
/// assertion above cannot be optimized into nothing.
fn attr_size(attr: &PerfEventAttr) -> u16 {
    // SAFETY: reading the second u32 of a `repr(C)` structure whose first two
    // fields are u32, through a pointer to the structure itself.
    let size = unsafe { std::ptr::read_unaligned(std::ptr::from_ref(attr).cast::<u32>().add(1)) };
    u16::try_from(size).unwrap_or(0)
}

/// Pin the calling thread to one core and verify it landed there. Pinning is a
/// correctness condition for every measurement here, so a failed verification is
/// an error rather than a warning.
///
/// # Errors
/// [`PerfError::Pin`] when the affinity call fails, [`PerfError::PinVerify`]
/// when the thread is somewhere else afterwards.
pub fn pin_to_core(core: usize) -> Result<(), PerfError> {
    // SAFETY: a zeroed cpu set is a valid empty set; `CPU_SET` writes inside it.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: `core` indexes inside `cpu_set_t` for any core the kernel reports.
    unsafe { libc::CPU_SET(core, &mut set) };
    // SAFETY: pid 0 is the calling thread; the set is fully initialized above.
    let rc = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    if rc != 0 {
        return Err(PerfError::Pin {
            core,
            source: std::io::Error::last_os_error(),
        });
    }
    let got = current_core();
    if got != i32::try_from(core).unwrap_or(-1) {
        return Err(PerfError::PinVerify { want: core, got });
    }
    Ok(())
}

/// The core the calling thread is running on.
#[must_use]
pub fn current_core() -> i32 {
    // SAFETY: a plain query with no arguments.
    unsafe { libc::sched_getcpu() }
}

/// How many cores the calling thread may run on. One means it is pinned.
///
/// # Errors
/// [`PerfError::Pin`] when the affinity cannot be read.
pub fn allowed_core_count() -> Result<usize, PerfError> {
    // SAFETY: a zeroed cpu set is a valid empty set the kernel fills in.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: pid 0 is the calling thread; the buffer is the size declared.
    let rc =
        unsafe { libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) };
    if rc != 0 {
        return Err(PerfError::Pin {
            core: 0,
            source: std::io::Error::last_os_error(),
        });
    }
    // SAFETY: counting the set the kernel just filled in.
    Ok(usize::try_from(unsafe { libc::CPU_COUNT(&set) }).unwrap_or(0))
}
