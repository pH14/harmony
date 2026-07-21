// SPDX-License-Identifier: AGPL-3.0-or-later
//! `RunBuf` — a bounds-checked window over a raw byte region.
//!
//! This is one of the crate's two **Miri-driveable pointer seams** (the other is
//! [`crate::region`]). On the box it wraps the `mmap`-ed `kvm_run` shared page so
//! the run loop can read a PIO `OUT` value out of the data buffer and write an
//! `IN` value back; in tests and under Miri it wraps a fake `alloc_zeroed` page
//! so **all the offset math is exercised with no syscall** (the
//! `hypercall-doorbell` precedent, `tasks/00-CONVENTIONS.md` / `AGENTS.md`).
//!
//! Both accessors bound-check `offset + len <= len` **before** any pointer
//! arithmetic or copy. That check is the load-bearing safety property: no offset,
//! however large, can read or write past the region or trigger UB — an
//! out-of-bounds request is a [`BackendError::Memory`], never undefined behavior.
//! Construction is the only `unsafe`: the caller vouches that `ptr` names `len`
//! live, exclusively-owned bytes (on the box, the kernel-owned `kvm_run`). The
//! accessors themselves are safe to call with any arguments.

use core::ptr;

use crate::error::{BackendError, Result};

/// A bounds-checked window over `len` raw bytes at `ptr`.
///
/// Held as a raw pointer (never a `&mut [u8]` field) because on the box the
/// kernel writes the page out-of-band across `KVM_RUN`, exactly as
/// `hypercall-doorbell` holds its shared pages: a reference live across that write
/// would be aliasing UB.
pub(crate) struct RunBuf {
    ptr: *mut u8,
    len: usize,
}

impl RunBuf {
    /// Wrap `len` bytes at `ptr`.
    ///
    /// # Safety
    /// `ptr` must point to `len` contiguous, initialized, exclusively-owned bytes
    /// that stay live and at a fixed address for the lifetime of this `RunBuf`,
    /// and that are not aliased by any live `&`/`&mut` while it is used. (On the
    /// box this is the `mmap`-ed `kvm_run`; in tests, a page-aligned
    /// `alloc_zeroed` allocation reached only through this pointer.)
    pub(crate) unsafe fn new(ptr: *mut u8, len: usize) -> Self {
        Self { ptr, len }
    }

    /// Bound check: `off + size <= len`, computed without overflow.
    fn check(&self, off: usize, size: usize) -> Result<()> {
        match off.checked_add(size) {
            Some(end) if end <= self.len => Ok(()),
            _ => Err(BackendError::Memory("kvm_run offset out of bounds")),
        }
    }

    /// Copy `dst.len()` bytes out of the region starting at `off`.
    pub(crate) fn read_bytes(&self, off: usize, dst: &mut [u8]) -> Result<()> {
        self.check(off, dst.len())?;
        // SAFETY: `check` proved `off + dst.len() <= len`; the read stays
        // in-bounds. `dst` is a distinct caller buffer, so the copy is
        // non-overlapping.
        unsafe { ptr::copy_nonoverlapping(self.ptr.add(off), dst.as_mut_ptr(), dst.len()) };
        Ok(())
    }

    /// Copy `src` into the region starting at `off`.
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
    //! These drive the unsafe pointer/offset logic with a fake page so the
    //! **Miri** gate (`cargo +nightly miri test -p vmm-backend`) scrutinizes it
    //! for UB with no syscall. The page is a raw `alloc_zeroed` allocation
    //! reached only through its pointer (the production shape: raw RAM, not a
    //! `Box` — mirrors `hypercall-doorbell`'s `Page`), so the seam is clean under
    //! the default Stacked-Borrows model.

    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    /// A 4 KiB-aligned, `len`-byte scratch page reached only by raw pointer.
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

        // empty copy is a no-op success regardless of offset.
        buf.read_bytes(64, &mut []).unwrap();
        buf.write_bytes(64, &[]).unwrap();
    }

    #[test]
    fn out_of_bounds_is_an_error_never_ub() {
        let page = Scratch::new(16);
        // SAFETY: 16 live, owned, page-aligned bytes reached only via this ptr.
        let mut buf = unsafe { RunBuf::new(page.ptr, 16) };

        // Reads/writes that would cross the end are rejected — not UB. Under Miri
        // a missed bound check here would be flagged as an out-of-bounds access.
        let mut big = [0u8; 17];
        assert!(buf.read_bytes(0, &mut big).is_err());
        assert!(buf.write_bytes(1, &[0u8; 16]).is_err());
        assert!(buf.read_bytes(13, &mut [0u8; 4]).is_err()); // 13 + 4 > 16
        assert!(buf.write_bytes(usize::MAX - 1, &[0u8; 4]).is_err()); // overflow path

        // Exact-fit accesses at the boundary succeed.
        assert!(buf.read_bytes(12, &mut [0u8; 4]).is_ok());
        assert!(buf.write_bytes(0, &[0u8; 16]).is_ok());
    }
}
