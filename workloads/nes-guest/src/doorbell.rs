// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
mod arch {
    pub use hypercall_doorbell::linux::DeviceTransport;

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

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
mod arch {
    use std::{fs::OpenOptions, os::fd::AsRawFd};

    use hypercall_doorbell::{MmioDoorbell, PAGE_SIZE, REQ_GPA, RESP_GPA, VmcallTransport};

    const DOORBELL_GPA: u64 = 0x0A00_0000;

    type Transport = VmcallTransport<MmioDoorbell>;

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

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod arch {
    pub struct UnsupportedTransport;

    #[derive(Debug)]
    pub struct UnsupportedTransportError;

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
