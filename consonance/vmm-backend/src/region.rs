// SPDX-License-Identifier: AGPL-3.0-or-later
//! `MemRegions` — the guest-physical memslot table and GPA→host translation.
//!
//! The second of the crate's two **Miri-driveable pointer seams** (the other is
//! [`crate::run_buf`]). It owns the `map_memory` bookkeeping the task spec's Miri
//! section names — "slot tracking, the alignment/overlap/bounds checks
//! `map_memory` enforces" — plus bounds-checked GPA→host copies. On the box
//! `KvmBackend::map_memory` validates and records each region here (then hands
//! the pointer to `KVM_SET_USER_MEMORY_REGION`), and `read_guest`/`write_guest`
//! copy through it; in tests and under Miri the same code is driven over an
//! `alloc_zeroed` page with **no syscall**.
//!
//! Every translation bound-checks `[gpa, gpa + len)` against one recorded region
//! **before** any pointer arithmetic or dereference. That check is the
//! load-bearing safety property: no GPA can make a copy read or write past a
//! region or trigger UB — an out-of-range access is a [`BackendError::Memory`],
//! never undefined behavior. The host pointers are stored raw (the caller's
//! `unsafe map_memory` contract retains them past the `&mut [u8]` borrow).

use core::ptr;

use crate::error::{BackendError, Result};

/// Page size for the alignment invariants (`KVM_SET_USER_MEMORY_REGION` requires
/// 4 KiB alignment of the GPA, the length, and the userspace address).
const PAGE: u64 = 4096;

/// One registered guest-physical region: a `[gpa, gpa + len)` range backed by a
/// host pointer. `len > 0` and `gpa + len` does not overflow (both enforced at
/// [`MemRegions::insert`]).
struct MemRegion {
    gpa: u64,
    host: *mut u8,
    len: u64,
}

/// The single-vCPU memslot table. Bring-up uses one slot, but the table supports
/// several non-overlapping regions so the loader and a separate MMIO/scratch
/// region can coexist.
#[derive(Default)]
pub(crate) struct MemRegions {
    slots: Vec<MemRegion>,
}

impl MemRegions {
    /// An empty table.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Validate the alignment / non-zero / non-overlap invariants and record the
    /// region, returning its assigned KVM slot index. **Pure bookkeeping — no
    /// syscall.** The host pointer is retained (the caller's `unsafe map_memory`
    /// contract: it must stay live and pinned until the backend is dropped).
    pub(crate) fn insert(&mut self, gpa: u64, host: *mut u8, len: u64) -> Result<u32> {
        if len == 0 {
            return Err(BackendError::Memory("zero-length memory region"));
        }
        if !gpa.is_multiple_of(PAGE) {
            return Err(BackendError::Memory("gpa is not 4 KiB-aligned"));
        }
        if !len.is_multiple_of(PAGE) {
            return Err(BackendError::Memory("region length is not 4 KiB-aligned"));
        }
        if !(host as usize).is_multiple_of(PAGE as usize) {
            return Err(BackendError::Memory("host address is not 4 KiB-aligned"));
        }
        let end = gpa
            .checked_add(len)
            .ok_or(BackendError::Memory("region wraps the address space"))?;
        for r in &self.slots {
            // Each stored region is non-wrapping (this check ran at its insert),
            // so `r.gpa + r.len` cannot overflow.
            let r_end = r.gpa + r.len;
            let overlaps = gpa < r_end && r.gpa < end;
            if overlaps {
                return Err(BackendError::Memory("region overlaps an existing map"));
            }
        }
        let slot = self.slots.len() as u32;
        self.slots.push(MemRegion { gpa, host, len });
        Ok(slot)
    }

    /// Undo the most recent [`MemRegions::insert`]. Used by `map_memory` to keep
    /// the table consistent when the backend's registration of the just-recorded
    /// region (`KVM_SET_USER_MEMORY_REGION`) fails — so a failed map leaves no
    /// stale host pointer behind for `read`/`write` to dereference.
    pub(crate) fn rollback_last(&mut self) {
        self.slots.pop();
    }

    /// Bounds-checked GPA→host translation: the host pointer for the start of
    /// `[gpa, gpa + len)` if a single recorded region contains the whole range,
    /// else [`BackendError::Memory`]. The check precedes any pointer arithmetic.
    fn translate(&self, gpa: u64, len: u64) -> Result<*mut u8> {
        let end = gpa
            .checked_add(len)
            .ok_or(BackendError::Memory("guest access wraps the address space"))?;
        for r in &self.slots {
            let r_end = r.gpa + r.len; // non-wrapping (see `insert`)
            if gpa >= r.gpa && end <= r_end {
                let off = gpa - r.gpa; // <= r.len, fits an isize within the region
                // SAFETY: `off <= r.len` and the region is `r.len` contiguous
                // bytes from `r.host`, so the resulting pointer is in-bounds (or
                // one-past-the-end only when `len == 0`, which callers never pass
                // to a copy). No dereference happens here.
                return Ok(unsafe { r.host.add(off as usize) });
            }
        }
        Err(BackendError::Memory(
            "guest access is not within a mapped region",
        ))
    }

    /// Copy `buf.len()` bytes **out of** guest memory at `gpa` into `buf`.
    pub(crate) fn read(&self, gpa: u64, buf: &mut [u8]) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        let src = self.translate(gpa, buf.len() as u64)?;
        // SAFETY: `translate` proved `[gpa, gpa + buf.len())` lies within one
        // recorded region, so `src .. src + buf.len()` is in-bounds and
        // readable. `buf` is a distinct caller slice, so the copy is
        // non-overlapping.
        unsafe { ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len()) };
        Ok(())
    }

    /// Copy `bytes` **into** guest memory at `gpa`.
    pub(crate) fn write(&mut self, gpa: u64, bytes: &[u8]) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let dst = self.translate(gpa, bytes.len() as u64)?;
        // SAFETY: as `read` — `translate` proved the destination range lies
        // within one region, so `dst .. dst + bytes.len()` is in-bounds and
        // writable; `bytes` is a distinct caller slice (non-overlapping).
        unsafe { ptr::copy_nonoverlapping(bytes.as_ptr(), dst, bytes.len()) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    //! Drive the validation + the unsafe translation/copy logic with fake
    //! `alloc_zeroed` backing so the **Miri** gate scrutinizes it for UB with no
    //! syscall. The backing is reached only through its raw pointer (production
    //! shape: pinned identity-mapped RAM, not a `Box`), clean under the default
    //! Stacked-Borrows model.

    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

    /// A 4 KiB-aligned guest-RAM stand-in reached only by raw pointer.
    struct Backing {
        ptr: *mut u8,
        layout: Layout,
    }
    impl Backing {
        fn new(len: usize) -> Self {
            let layout = Layout::from_size_align(len, 4096).expect("valid layout");
            // SAFETY: `len` is a non-zero multiple of the page size in every test.
            let ptr = unsafe { alloc_zeroed(layout) };
            assert!(!ptr.is_null(), "alloc failed");
            Self { ptr, layout }
        }
    }
    impl Drop for Backing {
        fn drop(&mut self) {
            // SAFETY: `ptr`/`layout` came from `alloc_zeroed`; freed exactly once.
            unsafe { dealloc(self.ptr, self.layout) };
        }
    }

    #[test]
    fn insert_validates_and_assigns_slots() {
        let a = Backing::new(2 * PAGE as usize);
        let b = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();

        // insert returns sequential slot indices.
        assert_eq!(regions.insert(0, a.ptr, 2 * PAGE).unwrap(), 0);
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);

        // zero length / mis-aligned gpa / mis-aligned length all reject.
        assert!(regions.insert(0x8000, b.ptr, 0).is_err());
        assert!(regions.insert(0x8001, b.ptr, PAGE).is_err());
        assert!(regions.insert(0x8000, b.ptr, 0x801).is_err());
        // overlap with slot 0 ([0, 0x2000)) rejects.
        assert!(regions.insert(0x1000, b.ptr, PAGE).is_err());
    }

    #[test]
    fn rollback_last_removes_the_just_inserted_region() {
        let a = Backing::new(PAGE as usize);
        let b = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();
        regions.insert(0, a.ptr, PAGE).unwrap();
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);

        // Roll back slot 1: its range/pointer is gone, so it no longer translates
        // and its slot index frees up for re-use.
        regions.rollback_last();
        assert!(regions.read(0x4000, &mut [0u8; 1]).is_err());
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);
        // Slot 0 is untouched.
        regions.read(0, &mut [0u8; 1]).unwrap();
    }

    #[test]
    fn insert_overlap_and_adjacency_boundaries() {
        let backing = Backing::new(8 * PAGE as usize);
        let p = backing.ptr;

        // Adjacent regions (touching exactly at a boundary) are NOT overlaps.
        let mut r = MemRegions::new();
        r.insert(0x1000, p, 2 * PAGE).unwrap(); // [0x1000, 0x3000)
        r.insert(0x3000, p, PAGE).unwrap(); // adjacent above: gpa == existing end
        r.insert(0, p, PAGE).unwrap(); // adjacent below: new end == existing start

        // Genuine overlaps reject, exercising both terms of the overlap test.
        let mut r2 = MemRegions::new();
        r2.insert(0x2000, p, 2 * PAGE).unwrap(); // [0x2000, 0x4000)
        // straddles the existing start (the `r.gpa < end` term is the live one).
        assert!(r2.insert(0x1000, p, 2 * PAGE).is_err());
        // straddles the existing end (the `gpa < r_end` term is the live one).
        assert!(r2.insert(0x3000, p, 2 * PAGE).is_err());
    }

    #[test]
    fn read_write_round_trip_within_region() {
        let backing = Backing::new(2 * PAGE as usize);
        let mut regions = MemRegions::new();
        regions.insert(0x1_0000, backing.ptr, 2 * PAGE).unwrap();

        regions.write(0x1_0000, &[0xAB; 32]).unwrap();
        regions.write(0x1_0FF0, &[0xCD; 16]).unwrap();
        let mut out = [0u8; 32];
        regions.read(0x1_0000, &mut out).unwrap();
        assert_eq!(out, [0xAB; 32]);
        let mut tail = [0u8; 16];
        regions.read(0x1_0FF0, &mut tail).unwrap();
        assert_eq!(tail, [0xCD; 16]);

        // empty copy is a no-op success regardless of address.
        regions.read(0x9999_9999, &mut []).unwrap();
        regions.write(0x9999_9999, &[]).unwrap();
    }

    #[test]
    fn out_of_range_access_is_an_error_never_ub() {
        let backing = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();
        regions.insert(0x2000, backing.ptr, PAGE).unwrap();

        // Below the region, above it, and straddling the end — all rejected.
        // Under Miri a missed bound check here would surface as an OOB access.
        assert!(regions.read(0x1FFF, &mut [0u8; 1]).is_err());
        assert!(regions.read(0x3000, &mut [0u8; 1]).is_err());
        let mut straddle = [0u8; 16];
        assert!(regions.read(0x2FF8, &mut straddle).is_err()); // ends past 0x3000
        assert!(regions.write(u64::MAX, &[0u8; 8]).is_err()); // wrap path

        // Exact-fit at the upper boundary succeeds.
        assert!(regions.read(0x2FF8, &mut [0u8; 8]).is_ok());
    }
}
