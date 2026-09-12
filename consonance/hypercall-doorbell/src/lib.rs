// SPDX-License-Identifier: AGPL-3.0-or-later
#![no_std]

#[cfg(feature = "linux-device")]
extern crate std;

use core::{mem::size_of, ptr};

use hypercall_proto::HEADER_LEN;

#[cfg(all(feature = "linux-device", target_os = "linux"))]
pub mod linux {
    use core::mem::size_of;
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;

    const DEVICE: &str = "/dev/harmony";
    const MAX_FRAME: usize = hypercall_proto::MAX_FRAME;
    const HARMONY_IOC_EXCHANGE: u64 = 0xc020_4801;

    #[repr(C)]
    struct Exchange {
        request: u64,
        response: u64,
        request_len: u32,
        response_capacity: u32,
        response_len: u32,
        reserved: u32,
    }

    const _: () = assert!(size_of::<Exchange>() == 32);

    pub struct DeviceTransport {
        file: File,
    }

    impl DeviceTransport {
        pub fn open() -> io::Result<Self> {
            File::options()
                .read(true)
                .write(true)
                .open(DEVICE)
                .map(|file| Self { file })
        }
    }

    impl hypercall_proto::Transport for DeviceTransport {
        type Error = io::Error;

        fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
            exchange_with(req, resp, |exchange| {
                // SAFETY: `exchange` is the fixed-width UAPI structure. The
                // request and response slices outlive this synchronous ioctl,
                // are non-null whenever their lengths are non-zero, and have
                // already passed the one-frame bounds checks in
                // `exchange_with`. The kernel copies only those declared
                // lengths under the device's synchronous transaction lock.
                let result = unsafe {
                    libc::ioctl(
                        self.file.as_raw_fd(),
                        HARMONY_IOC_EXCHANGE as _,
                        exchange as *mut Exchange,
                    )
                };
                if result == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            })
        }
    }

    fn exchange_with<F>(req: &[u8], resp: &mut [u8], mut ioctl: F) -> io::Result<usize>
    where
        F: FnMut(&mut Exchange) -> io::Result<()>,
    {
        if req.len() > MAX_FRAME || resp.len() > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "hypercall frame exceeds one page",
            ));
        }
        let request_len = u32::try_from(req.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "request length exceeds u32")
        })?;
        let response_capacity = u32::try_from(resp.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "response capacity exceeds u32")
        })?;
        let mut exchange = Exchange {
            request: req.as_ptr() as usize as u64,
            response: resp.as_mut_ptr() as usize as u64,
            request_len,
            response_capacity,
            response_len: 0,
            reserved: 0,
        };
        ioctl(&mut exchange)?;
        let response_len = exchange.response_len as usize;
        if response_len > resp.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "driver returned an out-of-range response length",
            ));
        }
        Ok(response_len)
    }

    #[cfg(test)]
    mod tests {
        use std::vec;

        use super::*;

        fn assert_transport<T: hypercall_proto::Transport>() {}

        #[test]
        fn device_transport_implements_protocol_transport() {
            assert_transport::<DeviceTransport>();
            assert_eq!(DEVICE, "/dev/harmony");
            assert_eq!(HARMONY_IOC_EXCHANGE, 0xc020_4801);
            assert_eq!(size_of::<Exchange>(), 32);
        }

        #[test]
        fn oversized_frames_are_rejected_before_ioctl() {
            let request = vec![0; MAX_FRAME + 1];
            let mut response = [0; 8];
            let mut called = false;
            let error = exchange_with(&request, &mut response, |_| {
                called = true;
                Ok(())
            })
            .expect_err("oversized request must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!called);

            let request = [0; 8];
            let mut response = vec![0; MAX_FRAME + 1];
            let error = exchange_with(&request, &mut response, |_| {
                called = true;
                Ok(())
            })
            .expect_err("oversized response must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(!called);
        }

        #[test]
        fn response_length_is_bounded_after_ioctl() {
            let request = [1, 2, 3];
            let mut response = [0; 4];
            let response_capacity = response.len();
            let error = exchange_with(&request, &mut response, |exchange| {
                assert_eq!(exchange.request_len, request.len() as u32);
                assert_eq!(exchange.response_capacity, response_capacity as u32);
                exchange.response_len = response_capacity as u32 + 1;
                Ok(())
            })
            .expect_err("driver length above capacity must fail");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }

        #[test]
        fn exact_page_boundary_preserves_uapi_fields_and_is_accepted() {
            let request = vec![0xA5; MAX_FRAME];
            let mut response = vec![0; MAX_FRAME];
            let request_ptr = request.as_ptr() as usize as u64;
            let response_ptr = response.as_mut_ptr() as usize as u64;
            let mut called = false;
            let length = exchange_with(&request, &mut response, |exchange| {
                called = true;
                assert_eq!(exchange.request, request_ptr);
                assert_eq!(exchange.response, response_ptr);
                assert_eq!(exchange.request_len, MAX_FRAME as u32);
                assert_eq!(exchange.response_capacity, MAX_FRAME as u32);
                assert_eq!(exchange.response_len, 0);
                assert_eq!(exchange.reserved, 0);
                exchange.response_len = MAX_FRAME as u32;
                Ok(())
            })
            .expect("one-page request and response must be accepted");
            assert!(called, "the ioctl must run at the inclusive boundary");
            assert_eq!(length, MAX_FRAME);
        }

        #[test]
        fn bounded_response_length_is_returned() {
            let request = [1, 2, 3];
            let mut response = [0; 4];
            let length = exchange_with(&request, &mut response, |exchange| {
                assert_eq!(exchange.request_len, 3);
                assert_eq!(exchange.response_capacity, 4);
                exchange.response_len = 4;
                Ok(())
            })
            .expect("bounded response length should pass");
            assert_eq!(length, response.len());
        }

        #[test]
        fn ioctl_error_is_preserved() {
            let request = [0; 1];
            let mut response = [0; 1];
            let error = exchange_with(&request, &mut response, |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "test denial",
                ))
            })
            .expect_err("fake ioctl error should propagate");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }

        #[cfg(not(miri))]
        #[test]
        fn device_transport_preserves_a_nonzero_ioctl_result() {
            let mut transport = DeviceTransport {
                file: File::open("/dev/null").expect("Linux test device exists"),
            };
            let mut response = [0_u8; 1];
            let error = hypercall_proto::Transport::exchange(&mut transport, &[0], &mut response)
                .expect_err("an unsupported ioctl must remain an error");
            assert!(
                error.raw_os_error().is_some(),
                "the ioctl errno must be preserved"
            );
        }
    }
}

pub const DOORBELL_PORT: u16 = 0x0CA1;

pub const REQ_GPA: u64 = 0x0000_E000;

pub const RESP_GPA: u64 = 0x0000_F000;

pub const PAGE_SIZE: usize = 4096;

const FRAME_MAGIC: u32 = 0x3150_4348;

const _: () = assert!(PAGE_SIZE == hypercall_proto::MAX_FRAME);

const _: () = assert!(HEADER_LEN <= PAGE_SIZE);

const _: () = assert!(REQ_GPA.is_multiple_of(PAGE_SIZE as u64));
const _: () = assert!(RESP_GPA.is_multiple_of(PAGE_SIZE as u64));
const _: () = assert!(REQ_GPA != RESP_GPA);

/// # Safety
/// `dst` must be aligned for `u64`, valid for `len` initialized writable bytes,
/// and not overlap the `len` readable bytes at `src`.
unsafe fn copy_to_shared_page(dst: *mut u8, src: *const u8, len: usize) {
    let mut offset = 0;
    while offset + size_of::<u64>() <= len {
        // SAFETY: the caller grants both ranges for `len` bytes. `src` need not
        // be aligned, while `dst + offset` is u64-aligned because the page base
        // is aligned and `offset` advances by whole u64 words.
        let word = unsafe { ptr::read_unaligned(src.add(offset).cast::<u64>()) };
        // SAFETY: same range/alignment grant; volatile keeps this as a single
        // shared-page access rather than a libc/vectorized memory operation.
        unsafe { ptr::write_volatile(dst.add(offset).cast::<u64>(), word) };
        offset += size_of::<u64>();
    }
    while offset < len {
        // SAFETY: the remaining byte lies inside both caller-granted ranges.
        let byte = unsafe { ptr::read(src.add(offset)) };
        // SAFETY: the destination byte is in-range; volatile is required for
        // an arm64 `/dev/mem` device mapping.
        unsafe { ptr::write_volatile(dst.add(offset), byte) };
        offset += 1;
    }
}

/// # Safety
/// `src` must be aligned for `u64`, valid for `len` initialized readable bytes,
/// and not overlap the `len` writable bytes at `dst`.
unsafe fn copy_from_shared_page(dst: *mut u8, src: *const u8, len: usize) {
    let mut offset = 0;
    while offset + size_of::<u64>() <= len {
        // SAFETY: the source word is aligned and inside the caller-granted
        // shared-page range. Volatile prevents paired/vectorized device reads.
        let word = unsafe { ptr::read_volatile(src.add(offset).cast::<u64>()) };
        // SAFETY: the destination has `len` writable bytes but need not be
        // aligned, hence the explicit unaligned ordinary-memory store.
        unsafe { ptr::write_unaligned(dst.add(offset).cast::<u64>(), word) };
        offset += size_of::<u64>();
    }
    while offset < len {
        // SAFETY: the remaining source byte is inside the granted shared page.
        let byte = unsafe { ptr::read_volatile(src.add(offset)) };
        // SAFETY: the corresponding ordinary-memory destination byte is live.
        unsafe { ptr::write(dst.add(offset), byte) };
        offset += 1;
    }
}

/// # Safety
/// `page` must be `u64`-aligned and valid for `PAGE_SIZE` initialized writable
/// bytes.
unsafe fn zero_shared_page(page: *mut u8) {
    let mut offset = 0;
    while offset < PAGE_SIZE {
        // SAFETY: PAGE_SIZE is a multiple of u64 and the caller grants an
        // aligned complete page, so every word is aligned and in-range.
        unsafe { ptr::write_volatile(page.add(offset).cast::<u64>(), 0) };
        offset += size_of::<u64>();
    }
}

pub trait IoDoorbell {
    /// # Safety
    /// The fixed request/response pages the host services out-of-band must name distinct,
    /// page-aligned, `PAGE_SIZE`, guest-owned pages valid for the duration of the call (the host
    /// reads the request page and writes the response page while the doorbell is in flight).
    unsafe fn ring(&mut self, port: u16, req_len: u32);
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RealIoDoorbell;

impl RealIoDoorbell {
    pub const fn new() -> Self {
        Self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MmioDoorbell {
    register: *mut u32,
}

impl MmioDoorbell {
    /// # Safety
    /// `register` must be non-null, naturally aligned, mapped read/write as a
    /// device register, and valid for every later [`IoDoorbell::ring`] call.
    /// The mapping must name the VMM's hypercall doorbell and be exclusively
    /// owned for volatile stores by this transport.
    pub unsafe fn new(register: *mut u32) -> Self {
        Self { register }
    }
}

impl IoDoorbell for MmioDoorbell {
    unsafe fn ring(&mut self, port: u16, req_len: u32) {
        if port != DOORBELL_PORT {
            return;
        }
        // SAFETY: the constructor requires a valid, aligned device mapping for
        // this register and the IoDoorbell caller guarantees it remains live
        // across this single atomic exchange. Volatile is required for MMIO.
        unsafe { ptr::write_volatile(self.register, req_len) };
    }
}

impl IoDoorbell for RealIoDoorbell {
    #[cfg(all(target_arch = "x86_64", not(miri)))]
    unsafe fn ring(&mut self, port: u16, req_len: u32) {
        // SAFETY: a single `out` traps to the host, which reads the request page and writes the
        // response page *out-of-band* by GPA — invisible to the compiler — before resuming the
        // guest at the next instruction. One exit ⇒ the host holds no pending state across a guest
        // resume, so the exchange is atomic w.r.t. injected interrupts. The default (no `nomem`/
        // `readonly`/`pure`) "may read or write any memory" semantics are required and intentional:
        // they keep the request-page stores (in `exchange`) from sinking past this `out`.
        // `preserves_flags` is deliberately omitted — the host owns guest state across an exit
        // except as specified, so we do not assume RFLAGS survives. `nostack` is accurate (`out`
        // touches no guest stack). The port (`> 0xFF`) is carried in DX; the request length in EAX.
        unsafe {
            core::arch::asm!(
                "out dx, eax",
                in("dx") port,
                in("eax") req_len,
                options(nostack),
            );
        }
    }

    #[cfg(any(not(target_arch = "x86_64"), miri))]
    unsafe fn ring(&mut self, _port: u16, _req_len: u32) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportError {
    RequestTooLarge,
    HostRejected,
    BadResponseLength,
}

impl core::fmt::Display for TransportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let text = match self {
            Self::RequestTooLarge => "request frame larger than page",
            Self::HostRejected => "host rejected the hypercall",
            Self::BadResponseLength => "host-returned response length out of bounds",
        };
        f.write_str(text)
    }
}

#[derive(Debug)]
pub struct VmcallTransport<D: IoDoorbell = RealIoDoorbell> {
    req_page: *mut u8,
    resp_page: *mut u8,
    doorbell: D,
}

impl VmcallTransport<RealIoDoorbell> {
    /// # Safety
    /// The ABI pages must satisfy the [`from_gpas`](VmcallTransport::from_gpas) page contract for
    /// `req_gpa = REQ_GPA`, `resp_gpa = RESP_GPA` (mapped read+write, identity-mapped,
    /// zero-initialized, exclusively owned, valid for the transport's lifetime).
    pub unsafe fn new() -> Self {
        // SAFETY: forwarded to `from_gpas` with the ABI-fixed GPAs; the caller upholds the page
        // contract for those GPAs.
        unsafe { Self::from_gpas(REQ_GPA, RESP_GPA) }
    }

    /// # Safety
    /// `req_gpa` and `resp_gpa` must each name a distinct, page-aligned, `PAGE_SIZE`, guest-owned
    /// page mapped read+write for the lifetime of the transport. Each GPA must be **non-null and
    /// dereferenceable as a Rust pointer for `PAGE_SIZE` bytes** (GPA `0`, though it can be
    /// page-aligned and hardware-mapped, is not a valid Rust pointer). Because the transport
    /// dereferences these values directly (it accesses the pages by address), **each GPA must also
    /// equal the page's linear/virtual address** — i.e. the pages are identity-mapped, as under
    /// the payload map; a GPA that is not a valid linear address is UB. The pages must be
    /// **initialized byte storage** (real memory or a device-typed `/dev/mem`
    /// mapping, not `MaybeUninit` — e.g. zeroed at reservation: the host may
    /// write a response shorter than the page, and step 3 zeroes the page so
    /// the untouched tail and a rejected page read as zeros) and **exclusively
    /// owned** by this transport for its lifetime — no other live reference may
    /// alias them (the `req` and `resp` slices passed to `exchange` must not
    /// overlap them), since the host writes the response page out-of-band. Page
    /// access uses aligned volatile scalar words so this contract is valid for
    /// either memory type.
    pub unsafe fn from_gpas(req_gpa: u64, resp_gpa: u64) -> Self {
        // SAFETY: forwarded to `with_doorbell`; `RealIoDoorbell` carries no invariants of its own
        // and is consistent with any GPAs (it only executes the `OUT` doorbell).
        unsafe { Self::with_doorbell(req_gpa, resp_gpa, RealIoDoorbell::new()) }
    }
}

impl<D: IoDoorbell> VmcallTransport<D> {
    /// # Safety
    /// Same page requirements as [`VmcallTransport::from_gpas`]; `doorbell` must be consistent
    /// with those GPAs (it services the same fixed pages out-of-band).
    pub unsafe fn with_doorbell(req_gpa: u64, resp_gpa: u64, doorbell: D) -> Self {
        Self {
            req_page: req_gpa as *mut u8,
            resp_page: resp_gpa as *mut u8,
            doorbell,
        }
    }
}

impl<D: IoDoorbell> hypercall_proto::Transport for VmcallTransport<D> {
    type Error = TransportError;

    fn exchange(&mut self, req: &[u8], resp: &mut [u8]) -> Result<usize, Self::Error> {
        if req.len() > PAGE_SIZE {
            return Err(TransportError::RequestTooLarge);
        }

        // SAFETY: `req_page`/`resp_page` name distinct, page-aligned,
        // `PAGE_SIZE`, exclusively owned pages (constructor contract); `req`
        // does not overlap them (same contract), so the copy is
        // non-overlapping. `req.len() <= PAGE_SIZE`, checked above.
        unsafe {
            zero_shared_page(self.req_page);
            zero_shared_page(self.resp_page);
            copy_to_shared_page(self.req_page, req.as_ptr(), req.len());
        }

        // SAFETY: the pages are distinct, page-aligned, `PAGE_SIZE`, guest-owned, and valid for the
        // duration of the call (constructor contract); the doorbell services exactly those pages.
        unsafe {
            self.doorbell.ring(DOORBELL_PORT, req.len() as u32);
        }

        // SAFETY: `resp_page` is a `PAGE_SIZE` page (constructor contract) and `HEADER_LEN <=
        let mut header = [0_u8; HEADER_LEN];
        unsafe {
            copy_from_shared_page(header.as_mut_ptr(), self.resp_page, HEADER_LEN);
        }

        let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if magic != FRAME_MAGIC {
            return Err(TransportError::HostRejected);
        }

        let payload_len = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
        let total = HEADER_LEN as u64 + payload_len as u64;
        if total > PAGE_SIZE as u64 || total > resp.len() as u64 {
            return Err(TransportError::BadResponseLength);
        }
        let len = total as usize;

        // SAFETY: `len <= PAGE_SIZE`, so the read stays within the response page; `len <=
        // resp.len()`, so the write stays within `resp`. `resp` does not overlap the response page
        // (constructor contract), so the copy is non-overlapping. No `&`/`&mut` to the page
        // outlives this block.
        unsafe {
            copy_from_shared_page(resp.as_mut_ptr(), self.resp_page, len);
        }
        Ok(len)
    }
}

#[cfg(test)]
mod tests {
    use super::{DOORBELL_PORT, IoDoorbell, MmioDoorbell};

    #[test]
    fn mmio_doorbell_writes_only_the_frozen_identity() {
        let mut register = 0_u32;
        // SAFETY: `register` is an aligned live u32 exclusively used by this
        // test for the lifetime of the primitive.
        let mut doorbell = unsafe { MmioDoorbell::new(core::ptr::addr_of_mut!(register)) };

        // SAFETY: the test-owned register satisfies the constructor and ring
        // contracts; no alias is accessed while either call is in progress.
        unsafe { doorbell.ring(DOORBELL_PORT ^ 1, 7) };
        assert_eq!(register, 0, "a foreign doorbell identity must be inert");

        // SAFETY: same live, exclusively-owned register.
        unsafe { doorbell.ring(DOORBELL_PORT, 0x1234_5678) };
        assert_eq!(register, 0x1234_5678);
    }
}
