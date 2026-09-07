// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linux transport selection for the in-guest play-agent.
//!
//! x86_64 uses the shared kernel-owned `/dev/harmony` transport from
//! `hypercall-doorbell`; arm64 retains the board MMIO transport because its
//! guest ABI maps the fixed pages directly.

/// x86 uses the shared kernel-owned synchronous transport.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod arch {
    pub use hypercall_doorbell::linux::DeviceTransport;

    /// Open `/dev/harmony`, adapting the shared I/O error to the guest
    /// binary's string-valued top-level error contract.
    pub fn open() -> Result<DeviceTransport, String> {
        DeviceTransport::open().map_err(|error| format!("/dev/harmony: {error}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn assert_transport<T: hypercall_proto::Transport>() {}

        #[test]
        fn x86_selects_shared_kernel_device_transport() {
            assert_transport::<DeviceTransport>();
        }
    }
}

/// Arm64's board-native transport. The request and response pages remain
/// the fixed hypercall-doorbell pages; only the one-exit doorbell changes
/// from x86 port I/O to the board's reserved 32-bit MMIO register.
#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod arch {
    use std::{fs::OpenOptions, os::fd::AsRawFd};

    use hypercall_doorbell::{MmioDoorbell, PAGE_SIZE, REQ_GPA, RESP_GPA, VmcallTransport};

    /// Board GPA of the reserved hypercall doorbell register (the same
    /// address used by the TetaNES arm64 guest agent).
    const DOORBELL_GPA: u64 = 0x0A00_0000;

    /// The arm64 production transport over the board's MMIO doorbell.
    type Transport = VmcallTransport<MmioDoorbell>;

    /// Open the fixed hypercall pages and the board's doorbell register.
    /// The mappings are intentionally retained for the process lifetime,
    /// as required by `VmcallTransport` and `MmioDoorbell`'s contracts.
    pub fn open() -> Result<Transport, String> {
        let req = map_phys(REQ_GPA, PAGE_SIZE)?;
        let resp = map_phys(RESP_GPA, PAGE_SIZE)?;
        let doorbell = map_phys(DOORBELL_GPA, PAGE_SIZE)?.cast::<u32>();
        // SAFETY: `doorbell` is a page mapping of the board's aligned,
        // 32-bit, store-only MMIO register and remains mapped for the
        // transport lifetime.
        let doorbell = unsafe { MmioDoorbell::new(doorbell) };
        // SAFETY: `req` and `resp` are distinct page-sized mappings of
        // the ABI control pages, exclusively owned here and remain mapped
        // for the transport lifetime.
        Ok(unsafe { VmcallTransport::with_doorbell(req as u64, resp as u64, doorbell) })
    }

    /// Map one page from the board's physical address space through the
    /// Linux `/dev/mem` interface. The returned mapping is intentionally
    /// leaked for the process lifetime.
    fn map_phys(gpa: u64, len: usize) -> Result<*mut u8, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/mem")
            .map_err(|error| format!("/dev/mem: {error}"))?;
        // SAFETY: standard shared mapping at a page-aligned physical
        // address; the result is checked before it is returned and the
        // file descriptor remains open until mmap has completed.
        let ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                gpa as libc::off_t,
            )
        };
        if ptr == libc::MAP_FAILED {
            return Err(format!(
                "mmap /dev/mem @ {gpa:#x}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(ptr.cast::<u8>())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn assert_transport<T: hypercall_proto::Transport>() {}

        #[test]
        fn arm64_selects_mmio_transport_at_board_doorbell() {
            assert_transport::<Transport>();
            assert_eq!(DOORBELL_GPA, 0x0A00_0000);
            assert_eq!(PAGE_SIZE, 4096);
            assert_ne!(REQ_GPA, RESP_GPA);
        }
    }
}

/// Keep unsupported Linux architectures buildable while making the
/// missing architecture-specific transport an explicit runtime error.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    /// No production transport is defined for this Linux architecture.
    pub struct UnsupportedTransport;

    /// Error returned when an unsupported Linux architecture attempts a
    /// hypercall exchange.
    #[derive(Debug)]
    pub struct UnsupportedTransportError;

    /// Fail loudly rather than silently running a guest without a
    /// deterministic hypercall transport.
    pub fn open() -> Result<UnsupportedTransport, String> {
        Err("play-agent transport requires Linux x86_64 or aarch64".to_string())
    }

    impl hypercall_proto::Transport for UnsupportedTransport {
        type Error = UnsupportedTransportError;

        fn exchange(&mut self, _req: &[u8], _resp: &mut [u8]) -> Result<usize, Self::Error> {
            Err(UnsupportedTransportError)
        }
    }
}

#[cfg(target_os = "linux")]
pub use arch::open;
