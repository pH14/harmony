// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{fs::File, io, os::fd::AsRawFd, ptr::NonNull};

const CREATE: libc::Ioctl = 0xc008_4802_u32 as libc::Ioctl;

type OpenFn = fn() -> io::Result<File>;
type IoctlFn = fn(libc::c_int, libc::Ioctl, *mut Request) -> libc::c_int;
type MmapFn = fn(
    *mut libc::c_void,
    usize,
    libc::c_int,
    libc::c_int,
    libc::c_int,
    libc::off_t,
) -> *mut libc::c_void;
type UnmapFn = fn(*mut libc::c_void, usize);

#[repr(C)]
struct Request {
    length: u32,
    handle: u32,
}

pub struct Observation {
    pointer: NonNull<u8>,
    length: usize,
    handle: u32,
    unmap: UnmapFn,
    _file: File,
}

impl Observation {
    pub fn create(length: usize) -> io::Result<Self> {
        Self::create_with(length, open_device, real_ioctl, real_mmap, real_munmap)
    }

    fn create_with(
        length: usize,
        open: OpenFn,
        ioctl: IoctlFn,
        mmap: MmapFn,
        unmap: UnmapFn,
    ) -> io::Result<Self> {
        let mut request = request(length)?;
        let file = open()?;
        if ioctl(file.as_raw_fd(), CREATE, &mut request) < 0 {
            return Err(io::Error::last_os_error());
        }
        if request.handle == 0 || request.length as usize != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid observation allocation",
            ));
        }
        let pointer = mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        );
        if pointer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(pointer) = NonNull::new(pointer.cast()) else {
            unmap(pointer, length);
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "null observation mapping",
            ));
        };
        Ok(Self {
            pointer,
            length,
            handle: request.handle,
            unmap,
            _file: file,
        })
    }

    pub fn handle(&self) -> u32 {
        self.handle
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: this object uniquely owns a live, initialized mapping of
        // length bytes. The borrow prevents any simultaneous Rust slice access.
        unsafe { region_slice(self.pointer, self.length) }
    }
}

fn open_device() -> io::Result<File> {
    File::options().read(true).write(true).open("/dev/harmony")
}

fn real_ioctl(fd: libc::c_int, command: libc::Ioctl, request: *mut Request) -> libc::c_int {
    // SAFETY: the initialized fixed-width UAPI structure lives through the
    // synchronous ioctl; the descriptor owns the returned kernel allocation.
    unsafe { libc::ioctl(fd, command, request) }
}

fn real_mmap(
    address: *mut libc::c_void,
    length: usize,
    protection: libc::c_int,
    flags: libc::c_int,
    fd: libc::c_int,
    offset: libc::off_t,
) -> *mut libc::c_void {
    // SAFETY: the descriptor owns an initialized region of the requested
    // length. The mapping is checked and retained with the file until Drop.
    unsafe { libc::mmap(address, length, protection, flags, fd, offset) }
}

fn real_munmap(address: *mut libc::c_void, length: usize) {
    // SAFETY: the pointer and length describe a live mapping returned by the
    // paired real_mmap call or the zero-address rejection path.
    unsafe { libc::munmap(address, length) };
}

impl Drop for Observation {
    fn drop(&mut self) {
        (self.unmap)(self.pointer.as_ptr().cast(), self.length);
    }
}

fn request(length: usize) -> io::Result<Request> {
    if length == 0 || length > hypercall_proto::observation::MAX_LEN as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "observation length out of range",
        ));
    }
    Ok(Request {
        length: length as u32,
        handle: 0,
    })
}

unsafe fn region_slice<'a>(pointer: NonNull<u8>, length: usize) -> &'a mut [u8] {
    // SAFETY: the caller guarantees unique access to length initialized bytes
    // at pointer and keeps that allocation alive for the returned borrow.
    unsafe { std::slice::from_raw_parts_mut(pointer.as_ptr(), length) }
}

#[cfg(test)]
mod tests {
    use std::{boxed::Box, vec};

    #[cfg(not(miri))]
    use std::string::ToString;

    use super::*;

    fn assert_ioctl_args(fd: libc::c_int, command: libc::Ioctl) {
        assert!(fd >= 0);
        assert_eq!(command, CREATE);
    }

    fn assert_mmap_args(
        address: *mut libc::c_void,
        protection: libc::c_int,
        flags: libc::c_int,
        fd: libc::c_int,
        offset: libc::off_t,
    ) {
        assert!(address.is_null());
        assert_eq!(protection, libc::PROT_READ | libc::PROT_WRITE);
        assert_eq!(flags, libc::MAP_SHARED);
        assert!(fd >= 0);
        assert_eq!(offset, 0);
    }

    #[cfg(not(miri))]
    fn open_zero() -> io::Result<File> {
        File::options().read(true).write(true).open("/dev/zero")
    }

    fn set_handle(request: *mut Request, handle: u32) {
        // SAFETY: create_with passes a non-null pointer to its live request for
        // the duration of this synchronous test syscall.
        unsafe { (*request).handle = handle };
    }

    fn ioctl_success(fd: libc::c_int, command: libc::Ioctl, request: *mut Request) -> libc::c_int {
        assert_ioctl_args(fd, command);
        set_handle(request, 0x1234_5678);
        0
    }

    #[cfg(not(miri))]
    fn ioctl_failure(fd: libc::c_int, command: libc::Ioctl, request: *mut Request) -> libc::c_int {
        assert_ioctl_args(fd, command);
        set_handle(request, 0x1234_5678);
        -1
    }

    #[cfg(not(miri))]
    fn ioctl_zero_handle(
        fd: libc::c_int,
        command: libc::Ioctl,
        request: *mut Request,
    ) -> libc::c_int {
        assert_ioctl_args(fd, command);
        set_handle(request, 0);
        0
    }

    #[cfg(not(miri))]
    fn ioctl_wrong_length(
        fd: libc::c_int,
        command: libc::Ioctl,
        request: *mut Request,
    ) -> libc::c_int {
        assert_ioctl_args(fd, command);
        set_handle(request, 0x1234_5678);
        // SAFETY: create_with passes a non-null pointer to its live request for
        // the duration of this synchronous test syscall.
        unsafe { (*request).length = 1 };
        0
    }

    fn unexpected_ioctl(_: libc::c_int, _: libc::Ioctl, _: *mut Request) -> libc::c_int {
        panic!("ioctl must not run after an open failure")
    }

    fn unexpected_mmap(
        _: *mut libc::c_void,
        _: usize,
        _: libc::c_int,
        _: libc::c_int,
        _: libc::c_int,
        _: libc::off_t,
    ) -> *mut libc::c_void {
        panic!("mmap must not run after an invalid allocation")
    }

    fn open_regular_file() -> io::Result<File> {
        File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
    }

    #[cfg(not(miri))]
    fn mmap_failed(
        address: *mut libc::c_void,
        _: usize,
        protection: libc::c_int,
        flags: libc::c_int,
        fd: libc::c_int,
        offset: libc::off_t,
    ) -> *mut libc::c_void {
        assert_mmap_args(address, protection, flags, fd, offset);
        libc::MAP_FAILED
    }

    #[cfg(not(miri))]
    fn mmap_null(
        address: *mut libc::c_void,
        _: usize,
        protection: libc::c_int,
        flags: libc::c_int,
        fd: libc::c_int,
        offset: libc::off_t,
    ) -> *mut libc::c_void {
        assert_mmap_args(address, protection, flags, fd, offset);
        std::ptr::null_mut()
    }

    fn mmap_box(
        address: *mut libc::c_void,
        length: usize,
        protection: libc::c_int,
        flags: libc::c_int,
        fd: libc::c_int,
        offset: libc::off_t,
    ) -> *mut libc::c_void {
        assert_mmap_args(address, protection, flags, fd, offset);
        Box::into_raw(vec![0_u8; length].into_boxed_slice())
            .cast::<u8>()
            .cast()
    }

    fn unmap_box(pointer: *mut libc::c_void, length: usize) {
        // SAFETY: mmap_box allocated exactly length bytes and transfers their
        // ownership to Observation until this paired unmap call.
        let bytes: &mut [u8] =
            unsafe { std::slice::from_raw_parts_mut(pointer.cast::<u8>(), length) };
        // SAFETY: bytes is the unique allocation transferred by mmap_box.
        unsafe { drop(Box::from_raw(bytes)) };
    }

    fn count_unmap(pointer: *mut libc::c_void, length: usize) {
        UNMAP_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        unmap_box(pointer, length);
    }

    static UNMAP_COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    #[test]
    fn open_failure_is_returned_before_ioctl() {
        let result = Observation::create_with(
            4096,
            || Err(io::Error::new(io::ErrorKind::NotFound, "missing device")),
            unexpected_ioctl,
            unexpected_mmap,
            real_munmap,
        );
        let error = match result {
            Ok(_) => panic!("an open failure must be returned"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(not(miri))]
    #[test]
    fn ioctl_failure_is_returned_before_mapping() {
        let result =
            Observation::create_with(4096, open_zero, ioctl_failure, unexpected_mmap, real_munmap);
        assert!(result.is_err(), "a negative ioctl result must be returned");
    }

    #[cfg(not(miri))]
    #[test]
    fn real_ioctl_propagates_kernel_failure() {
        let mut request = Request {
            length: 4096,
            handle: 0,
        };
        assert_eq!(real_ioctl(-1, CREATE, &mut request), -1);
    }

    #[cfg(not(miri))]
    #[test]
    fn invalid_driver_allocations_are_rejected_before_mapping() {
        for ioctl in [ioctl_zero_handle as IoctlFn, ioctl_wrong_length as IoctlFn] {
            let result =
                Observation::create_with(4096, open_zero, ioctl, unexpected_mmap, real_munmap);
            let error = match result {
                Ok(_) => panic!("an invalid allocation must be returned"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[cfg(not(miri))]
    #[test]
    fn mapping_failures_are_returned() {
        let result =
            Observation::create_with(4096, open_zero, ioctl_success, mmap_failed, real_munmap);
        assert!(result.is_err(), "MAP_FAILED must be returned as an error");

        let result =
            Observation::create_with(4096, open_zero, ioctl_success, mmap_null, real_munmap);
        let error = match result {
            Ok(_) => panic!("a null mapping must be returned"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "null observation mapping");
    }

    #[cfg(not(miri))]
    #[test]
    fn successful_creation_preserves_handle_and_mapping() {
        let mut observation =
            Observation::create_with(4096, open_zero, ioctl_success, real_mmap, real_munmap)
                .unwrap();
        assert_eq!(observation.handle(), 0x1234_5678);
        let bytes = observation.bytes();
        assert_eq!(bytes.len(), 4096);
        assert_eq!(bytes[0], 0);
        bytes[0] = 0xa5;
        assert_eq!(bytes[0], 0xa5);
    }

    #[cfg_attr(miri, ignore = "requires host filesystem access under Miri")]
    #[test]
    fn dropping_observation_releases_its_mapping() {
        UNMAP_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        let observation = Observation::create_with(
            4096,
            open_regular_file,
            ioctl_success,
            mmap_box,
            count_unmap,
        )
        .unwrap();
        assert_eq!(UNMAP_COUNT.load(std::sync::atomic::Ordering::Relaxed), 0);
        drop(observation);
        assert_eq!(UNMAP_COUNT.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[cfg(not(miri))]
    #[test]
    fn real_munmap_releases_a_native_mapping() {
        let length = 4096;
        let pointer = real_mmap(
            std::ptr::null_mut(),
            length,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        );
        assert_ne!(pointer, libc::MAP_FAILED);

        real_munmap(pointer, length);

        let mut residency = [0_u8];
        // SAFETY: pointer and length identify the page-aligned range returned
        // by mmap; residency is a valid one-byte result buffer. mincore
        // reports ENOMEM for the range after it has been unmapped.
        let result = unsafe { libc::mincore(pointer, length, residency.as_mut_ptr()) };
        assert_eq!(result, -1);
    }

    #[test]
    fn boxed_mapping_lifecycle_is_miri_safe() {
        let pointer = mmap_box(
            std::ptr::null_mut(),
            64,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            0,
            0,
        );
        let pointer = NonNull::new(pointer.cast()).unwrap();
        {
            // SAFETY: mmap_box allocated 64 initialized bytes and transfers
            // their ownership to this test until unmap_box below.
            let bytes = unsafe { region_slice(pointer, 64) };
            bytes.fill(0xa5);
            assert_eq!(bytes, &[0xa5; 64]);
        }
        unmap_box(pointer.as_ptr().cast(), 64);
    }

    #[test]
    fn invalid_lengths_are_rejected_before_allocation() {
        for length in [
            0,
            hypercall_proto::observation::MAX_LEN as usize + 1,
            usize::MAX,
        ] {
            assert!(request(length).is_err());
        }
        assert_eq!(request(1).unwrap().length, 1);
        assert_eq!(
            request(hypercall_proto::observation::MAX_LEN as usize)
                .unwrap()
                .handle,
            0
        );
        assert_eq!(std::mem::size_of::<Request>(), 8);
    }

    #[test]
    fn mapped_view_writes_only_the_owned_extent() {
        let mut storage = [0x7f_u8; 34];
        let pointer = NonNull::new(storage.as_mut_ptr().wrapping_add(1)).unwrap();
        // SAFETY: the interior pointer covers 32 initialized bytes of storage;
        // storage is exclusively borrowed and lives beyond the returned slice.
        let view = unsafe { region_slice(pointer, 32) };
        view.fill(0x42);
        assert_eq!(storage[0], 0x7f);
        assert_eq!(storage[33], 0x7f);
        assert_eq!(&storage[1..33], &[0x42; 32]);
    }
}
