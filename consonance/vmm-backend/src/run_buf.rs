// SPDX-License-Identifier: AGPL-3.0-or-later

use core::ptr;

use crate::error::{BackendError, Result};

pub(crate) struct RunBuf {
    ptr: *mut u8,
    len: usize,
}

impl RunBuf {
    /// # Safety
    /// `ptr` must point to `len` contiguous, initialized, exclusively-owned bytes
    /// that stay live and at a fixed address for the lifetime of this `RunBuf`,
    /// and that are not aliased by any live `&`/`&mut` while it is used. (On the
    /// box this is the `mmap`-ed `kvm_run`; in tests, a page-aligned
    /// `alloc_zeroed` allocation reached only through this pointer.)
    pub(crate) unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    fn check(&self, off: usize, size: usize) -> Result<()> {
        match off.checked_add(size) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(BackendError::Memory("kvm_run offset out of bounds")),
        }
    }

    pub(crate) fn read_bytes(&self, off: usize, dst: &mut [u8]) -> Result<()> {
        self.check(off, dst.len())?;
        // SAFETY: `check` proved `off + dst.len() <= len`; the read stays
        // in-bounds. `dst` is a distinct caller buffer, so the copy is
        // non-overlapping.
        unsafe { ptr::copy_nonoverlapping(self.ptr.add(off), dst.as_mut_ptr(), dst.len()) };
        Ok(())
    }

    pub(crate) fn write_bytes(&mut self, off: usize, src: &[u8]) -> Result<()> {
        self.check(off, src.len())?;
        // SAFETY: `check` proved `off + src.len() <= len`; the write stays
        // in-bounds. `src` is a distinct caller buffer, so the copy is
        // non-overlapping.
        unsafe { ptr::copy_nonoverlapping(src.as_ptr(), self.ptr.add(off), src.len()) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    struct Scratch {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Scratch {
        fn new(len: usize) -> Self {
            let layout = Layout::from_size_align(len, 4096).expect("valid layout");
            // SAFETY: `len` is non-zero in every test below; align is a power of two.
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "alloc failed");
            Self { ptr, layout }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            // SAFETY: `ptr`/`layout` came from `alloc_zeroed` above; freed once.
            unsafe { dealloc(self.ptr, self.layout) };
        }
    }

    #[test]
    fn byte_round_trip() {
        let page = Scratch::new(64);
        // SAFETY: 64 live, owned, page-aligned bytes reached only via this ptr.
        let mut buf = unsafe { RunBuf::new(page.ptr, 64) };

        buf.write_bytes(0, &[0xDE, 0xAD, 0xBE, 0xEF]).unwrap();
        buf.write_bytes(16, &[1, 2, 3, 4, 5]).unwrap();

        let mut head = [0u8; 4];
        buf.read_bytes(0, &mut head).unwrap();
        assert_eq!(head, [0xDE, 0xAD, 0xBE, 0xEF]);
        let mut mid = [0u8; 5];
        buf.read_bytes(16, &mut mid).unwrap();
        assert_eq!(mid, [1, 2, 3, 4, 5]);

        buf.read_bytes(64, &mut []).unwrap();
        buf.write_bytes(64, &[]).unwrap();
    }

    #[test]
    fn out_of_bounds_is_an_error_never_ub() {
        let page = Scratch::new(16);
        // SAFETY: 16 live, owned, page-aligned bytes reached only via this ptr.
        let mut buf = unsafe { RunBuf::new(page.ptr, 16) };

        let mut big = [0u8; 17];
        assert!(buf.read_bytes(0, &mut big).is_err());
        assert!(buf.write_bytes(1, &[0u8; 16]).is_err());
        assert!(buf.read_bytes(13, &mut [0u8; 4]).is_err());
        assert!(buf.write_bytes(usize::MAX - 1, &[0u8; 4]).is_err());

        assert!(buf.read_bytes(12, &mut [0u8; 4]).is_ok());
        assert!(buf.write_bytes(0, &[0u8; 16]).is_ok());
    }
}
