// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "aarch64")
))]
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
        fn linux_selects_shared_kernel_device_transport() {
            assert_transport::<DeviceTransport>();
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
