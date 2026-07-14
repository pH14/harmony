//! The syscall seam: perf_event and KVM ioctls.
//!
//! This is the whole crate's `unsafe`, and it is Linux-only by construction — the
//! development Mac cannot run it. Everything above this line (scanning, ELF
//! reading, console decoding, planning, evidence emission, count bookkeeping) is
//! pure logic and is tested natively; this module is the thin, deliberately dumb
//! layer that turns a plan into ioctls on the box.
//!
//! # Untested on silicon, and unbuilt off it
//!
//! On macOS this module compiles to a single "unsupported" stub so the rest of the
//! crate builds and its logic tests run. On Linux it compiles the real syscalls —
//! but even there it has **never run**, because the target hardware is not yet in
//! hand. The perf/KVM syscalls that the apparatus exists to eventually issue are
//! written out so arrival day is `scp + run`, not authoring; they are marked as
//! untested throughout.
//!
//! # Why the seam is this thin
//!
//! `docs/ARM-ALTRA.md` §Evidence integrity #4 requires that a silent fallback
//! (signal-kick instead of the patched exit) be *structurally unable* to
//! masquerade as the mechanism under test. Keeping this layer to "issue the ioctl,
//! return exactly what the kernel returned" — with no interpretation, no retry, no
//! smoothing — is what lets the layers above attest the mechanism honestly: the
//! exit reason in an evidence record is the one the kernel actually returned, not
//! one this code decided was close enough.

// This module is the crate's sole `unsafe`. The crate is `deny(unsafe_code)`; this
// is the one explicit, audited opt-in.
#![allow(unsafe_code)]

/// The raw `BR_RETIRED` event on aarch64 PMUv3: retired *taken* branches
/// (`docs/ARM-PORT.md`, `docs/ARM-ALTRA.md` §2). Not invented here — it is the
/// event those documents name, surfaced as a constant so the harness cannot
/// silently arm a different one.
pub const BR_RETIRED_RAW: u64 = 0x21;

/// A capability the running kernel either has or does not.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Capability {
    /// `/dev/kvm` is present and openable.
    DevKvm,
    /// `perf_event_open` of raw `BR_RETIRED` as a pinned event succeeds.
    PerfBrRetired,
    /// `KVM_CAP_SET_GUEST_DEBUG` (single-step) is advertised.
    GuestDebug,
    /// The 0004-analogue determinism cap (`host/patches/`) is advertised — the
    /// positive probe that the patched kernel is actually running.
    DeterministicIntercepts,
}

#[cfg(not(target_os = "linux"))]
mod imp {
    use super::Capability;

    /// The error every syscall entry point returns off Linux.
    #[derive(Debug, thiserror::Error)]
    #[error(
        "the KVM/perf syscall layer is Linux-only and unimplemented on this host \
         (by design: the pure logic is tested here, the syscalls run on the Altra box)"
    )]
    pub struct Unsupported;

    /// Probe a capability. Off Linux: always unsupported.
    ///
    /// # Errors
    /// Always [`Unsupported`] on non-Linux hosts.
    pub fn probe(_cap: Capability) -> Result<bool, Unsupported> {
        Err(Unsupported)
    }
}

#[cfg(target_os = "linux")]
mod imp {
    //! The real Linux syscalls. **Untested on silicon.**
    //!
    //! Compiled on Linux so a cross-build gate proves it *builds* for
    //! aarch64-linux; it has not been *run*, because the box is not yet here.

    use super::{BR_RETIRED_RAW, Capability};

    /// Why a syscall probe failed.
    #[derive(Debug, thiserror::Error)]
    pub enum SysError {
        /// A libc call set errno.
        #[error("{call} failed: errno {errno}")]
        Errno {
            /// The call that failed.
            call: &'static str,
            /// The errno value.
            errno: i32,
        },
    }

    /// `struct perf_event_attr`, only the fields we set. Zeroed otherwise.
    #[repr(C)]
    #[derive(Default)]
    struct PerfEventAttr {
        type_: u32,
        size: u32,
        config: u64,
        sample_period_or_freq: u64,
        sample_type: u64,
        read_format: u64,
        flags: u64,
        wakeup: u32,
        bp_type: u32,
        bp_addr_or_config1: u64,
        bp_len_or_config2: u64,
        branch_sample_type: u64,
        sample_regs_user: u64,
        sample_stack_user: u32,
        clockid: i32,
        sample_regs_intr: u64,
        aux_watermark: u32,
        sample_max_stack: u16,
        __reserved_2: u16,
        aux_sample_size: u32,
        __reserved_3: u32,
    }

    const PERF_TYPE_RAW: u32 = 4;
    // exclude_hv | exclude_host in the packed flags word: exclude_kernel(bit5),
    // exclude_hv(bit6), exclude_host(bit9), exclude_guest(bit10), pinned(bit3).
    // We count guest-only: exclude_host, and pinned so the counter is never
    // multiplexed.
    const FLAG_PINNED: u64 = 1 << 3;
    const FLAG_EXCLUDE_HOST: u64 = 1 << 9;

    /// `perf_event_open` is not in libc; issue the raw syscall.
    ///
    /// # Safety
    /// `attr` must point at a valid, fully initialized `perf_event_attr`. This is
    /// the architected calling convention for the syscall.
    unsafe fn perf_event_open(
        attr: *const PerfEventAttr,
        pid: libc::pid_t,
        cpu: i32,
        group_fd: i32,
        flags: libc::c_ulong,
    ) -> libc::c_long {
        // SAFETY: SYS_perf_event_open with the architected argument order; `attr`
        // is a valid pointer per this function's contract.
        unsafe { libc::syscall(libc::SYS_perf_event_open, attr, pid, cpu, group_fd, flags) }
    }

    /// Probe whether raw `BR_RETIRED` can be opened as a pinned, guest-only event.
    ///
    /// This is AA-0's PMU row and the precondition for the entire work-clock bet.
    /// **Untested on silicon** — written so arrival day runs it, not writes it.
    fn probe_br_retired() -> Result<bool, SysError> {
        let mut attr = PerfEventAttr {
            type_: PERF_TYPE_RAW,
            config: BR_RETIRED_RAW,
            flags: FLAG_PINNED | FLAG_EXCLUDE_HOST,
            ..Default::default()
        };
        attr.size = core::mem::size_of::<PerfEventAttr>() as u32;

        // SAFETY: `attr` is a fully initialized perf_event_attr living on this
        // stack frame; the pointer is valid for the duration of the call. Counting
        // the calling thread (pid 0) on its current CPU (-1) with no group.
        let fd = unsafe { perf_event_open(&attr, 0, -1, -1, 0) };
        if fd < 0 {
            // errno of ENOENT/EOPNOTSUPP means the event is not implemented here —
            // a real "no", not an error to propagate. Any other errno is a genuine
            // failure to probe.
            // SAFETY: reading errno immediately after a failed libc call.
            let errno = unsafe { *libc::__errno_location() };
            if errno == libc::ENOENT || errno == libc::EOPNOTSUPP {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "perf_event_open(BR_RETIRED)",
                errno,
            });
        }
        // SAFETY: `fd` is a valid file descriptor returned just above.
        unsafe { libc::close(fd as i32) };
        Ok(true)
    }

    /// Whether `/dev/kvm` opens.
    fn probe_dev_kvm() -> Result<bool, SysError> {
        let path = c"/dev/kvm";
        // SAFETY: opening a device read-only with a valid NUL-terminated path.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            // SAFETY: reading errno immediately after a failed open.
            let errno = unsafe { *libc::__errno_location() };
            if errno == libc::ENOENT || errno == libc::EACCES {
                return Ok(false);
            }
            return Err(SysError::Errno {
                call: "open(/dev/kvm)",
                errno,
            });
        }
        // SAFETY: `fd` is valid.
        unsafe { libc::close(fd) };
        Ok(true)
    }

    /// Probe a capability.
    ///
    /// # Errors
    /// [`SysError`] if a probe could not be issued (as opposed to a clean "no",
    /// which is `Ok(false)`).
    ///
    /// The two KVM-cap probes ([`Capability::GuestDebug`],
    /// [`Capability::DeterministicIntercepts`]) require an open VM fd and a
    /// `KVM_CHECK_EXTENSION` ioctl; they are stubbed as `Ok(false)` here with an
    /// explicit `todo` marker rather than faked, because faking a capability probe
    /// is exactly the "green on a failed gate" pathology the evidence rules forbid.
    /// Arrival day wires them to the real ioctl.
    pub fn probe(cap: Capability) -> Result<bool, SysError> {
        match cap {
            Capability::DevKvm => probe_dev_kvm(),
            Capability::PerfBrRetired => probe_br_retired(),
            // UNTESTED / UNIMPLEMENTED: needs KVM_CHECK_EXTENSION on a VM fd.
            // Returns a hard "cannot probe" so it can never masquerade as a "yes".
            Capability::GuestDebug | Capability::DeterministicIntercepts => Err(SysError::Errno {
                call: "KVM_CHECK_EXTENSION (unimplemented pre-silicon)",
                errno: libc::ENOSYS,
            }),
        }
    }
}

#[cfg(target_os = "linux")]
pub use imp::SysError;
#[cfg(not(target_os = "linux"))]
pub use imp::Unsupported;
pub use imp::probe;
