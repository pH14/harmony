// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{fs::File, io, os::fd::AsRawFd, ptr::NonNull};

const CREATE: libc::c_ulong = 0xc008_4802;

#[repr(C)]
struct Request {
    length: u32,
    handle: u32,
}

pub struct Observation {
    pointer: NonNull<u8>,
    length: usize,
    handle: u32,
    _file: File,
}

impl Observation {
    pub fn create(length: usize) -> io::Result<Self> {
        let mut request = request(length)?;
        let file = File::options()
            .read(true)
            .write(true)
            .open("/dev/harmony")?;
        // SAFETY: the initialized fixed-width UAPI structure lives through the
        // synchronous ioctl; the descriptor owns the returned kernel allocation.
        if unsafe { libc::ioctl(file.as_raw_fd(), CREATE, &mut request) } < 0 {
            return Err(io::Error::last_os_error());
        }
        if request.handle == 0 || request.length as usize != length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid observation allocation",
            ));
        }
        // SAFETY: the descriptor owns an initialized region of the requested
        // length. The mapping is checked and retained with the file until Drop.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                file.as_raw_fd(),
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        let Some(pointer) = NonNull::new(pointer.cast()) else {
            // SAFETY: mmap succeeded at address zero; release that mapping
            // before rejecting a pointer that cannot back a Rust slice.
            unsafe { libc::munmap(pointer, length) };
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "null observation mapping",
            ));
        };
        Ok(Self {
            pointer,
            length,
            handle: request.handle,
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

impl Drop for Observation {
    fn drop(&mut self) {
        // SAFETY: the pointer and length describe this object's live mapping;
        // no borrowed slice survives this exclusive drop and the file is open.
        unsafe { libc::munmap(self.pointer.as_ptr().cast(), self.length) };
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
    use super::*;

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
