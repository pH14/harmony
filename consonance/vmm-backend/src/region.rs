// SPDX-License-Identifier: AGPL-3.0-or-later

use core::ptr;

use crate::error::{BackendError, Result};

const PAGE: u64 = 4096;

struct MemRegion {
    gpa: u64,
    host: *mut u8,
    len: u64,
}

#[derive(Default)]
pub(crate) struct MemRegions {
    slots: Vec<MemRegion>,
}

impl MemRegions {
    pub(crate) fn new() -> Self {
        Self::default()
    }

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

    pub(crate) fn rollback_last(&mut self) {
        self.slots.pop();
    }

    fn translate(&self, gpa: u64, len: u64) -> Result<*mut u8> {
        let end = gpa
            .checked_add(len)
            .ok_or(BackendError::Memory("guest access wraps the address space"))?;
        for r in &self.slots {
            let r_end = r.gpa + r.len;
            if gpa >= r.gpa && end <= r_end {
                let off = gpa - r.gpa;
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct MemSlotPart {
    pub(crate) gpa: u64,
    pub(crate) size: u64,
    pub(crate) host_off: u64,
}

fn split_parts(base: u64, len: u64, hole_base: u64, hole_len: u64) -> [Option<MemSlotPart>; 2] {
    debug_assert!(
        base.checked_add(len).is_some() && hole_base.checked_add(hole_len).is_some(),
        "split_parts: caller must pass non-wrapping intervals (MemRegions::insert enforces base+len)"
    );
    let end = base.saturating_add(len);
    let hole_end = hole_base.saturating_add(hole_len);
    let ov_lo = hole_base.max(base);
    let ov_hi = hole_end.min(end);
    if len == 0 || ov_lo >= ov_hi {
        return [
            (len != 0).then_some(MemSlotPart {
                gpa: base,
                size: len,
                host_off: 0,
            }),
            None,
        ];
    }
    let left = (ov_lo > base).then_some(MemSlotPart {
        gpa: base,
        size: ov_lo - base,
        host_off: 0,
    });
    let right = (ov_hi < end).then_some(MemSlotPart {
        gpa: ov_hi,
        size: end - ov_hi,
        host_off: ov_hi - base,
    });
    [left, right]
}

pub(crate) fn split_around_hole(
    base: u64,
    len: u64,
    hole_base: u64,
    hole_len: u64,
) -> impl Iterator<Item = MemSlotPart> {
    split_parts(base, len, hole_base, hole_len)
        .into_iter()
        .flatten()
}

pub(crate) fn decode_dirty_bitmap(
    slot_gpa: u64,
    slot_size: u64,
    bitmap: &[u64],
    out: &mut Vec<u64>,
) {
    let slot_pages = slot_size / PAGE;
    let base_gfn = slot_gpa / PAGE;
    for (word_idx, &word) in bitmap.iter().enumerate() {
        let mut w = word;
        while w != 0 {
            let bit = u64::from(w.trailing_zeros());
            w &= w - 1;
            let page = (word_idx as u64) * 64 + bit;
            if page < slot_pages {
                out.push(base_gfn + page);
            }
        }
    }
}

#[cfg(kani)]
#[path = "region_proofs.rs"]
mod proofs;

#[cfg(test)]
mod tests {

    use super::*;
    use std::alloc::{Layout, alloc_zeroed, dealloc};

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

        assert_eq!(regions.insert(0, a.ptr, 2 * PAGE).unwrap(), 0);
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);

        assert!(regions.insert(0x8000, b.ptr, 0).is_err());
        assert!(regions.insert(0x8001, b.ptr, PAGE).is_err());
        assert!(regions.insert(0x8000, b.ptr, 0x801).is_err());
        assert!(regions.insert(0x1000, b.ptr, PAGE).is_err());
    }

    #[test]
    fn insert_rejects_a_wrapping_region() {
        let a = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();
        let gpa = u64::MAX - PAGE + 1;
        assert!(
            regions.insert(gpa, a.ptr, 2 * PAGE).is_err(),
            "gpa + len wrapping u64 must be rejected before any split"
        );
    }

    #[test]
    fn rollback_last_removes_the_just_inserted_region() {
        let a = Backing::new(PAGE as usize);
        let b = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();
        regions.insert(0, a.ptr, PAGE).unwrap();
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);

        regions.rollback_last();
        assert!(regions.read(0x4000, &mut [0u8; 1]).is_err());
        assert_eq!(regions.insert(0x4000, b.ptr, PAGE).unwrap(), 1);
        regions.read(0, &mut [0u8; 1]).unwrap();
    }

    #[test]
    fn insert_overlap_and_adjacency_boundaries() {
        let backing = Backing::new(8 * PAGE as usize);
        let p = backing.ptr;

        let mut r = MemRegions::new();
        r.insert(0x1000, p, 2 * PAGE).unwrap();
        r.insert(0x3000, p, PAGE).unwrap();
        r.insert(0, p, PAGE).unwrap();

        let mut r2 = MemRegions::new();
        r2.insert(0x2000, p, 2 * PAGE).unwrap();
        assert!(r2.insert(0x1000, p, 2 * PAGE).is_err());
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

        regions.read(0x9999_9999, &mut []).unwrap();
        regions.write(0x9999_9999, &[]).unwrap();
    }

    #[test]
    fn out_of_range_access_is_an_error_never_ub() {
        let backing = Backing::new(PAGE as usize);
        let mut regions = MemRegions::new();
        regions.insert(0x2000, backing.ptr, PAGE).unwrap();

        assert!(regions.read(0x1FFF, &mut [0u8; 1]).is_err());
        assert!(regions.read(0x3000, &mut [0u8; 1]).is_err());
        let mut straddle = [0u8; 16];
        assert!(regions.read(0x2FF8, &mut straddle).is_err());
        assert!(regions.write(u64::MAX, &[0u8; 8]).is_err());

        assert!(regions.read(0x2FF8, &mut [0u8; 8]).is_ok());
    }

    use proptest::prelude::*;

    const LAPIC_PAGE: u64 = 0xFEE0_0000;

    fn parts(base: u64, len: u64, hole: u64, hole_len: u64) -> Vec<MemSlotPart> {
        split_around_hole(base, len, hole, hole_len).collect()
    }

    fn cases(native: u32) -> ProptestConfig {
        let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { native });
        if cfg!(miri) {
            cfg.failure_persistence = None;
        }
        cfg
    }

    #[test]
    fn lapic_hole_splits_8gib_guest_into_two_slots() {
        let ram = 8u64 << 30;
        assert_eq!(
            parts(0, ram, LAPIC_PAGE, 0x1000),
            vec![
                MemSlotPart {
                    gpa: 0,
                    size: LAPIC_PAGE,
                    host_off: 0,
                },
                MemSlotPart {
                    gpa: LAPIC_PAGE + 0x1000,
                    size: ram - (LAPIC_PAGE + 0x1000),
                    host_off: LAPIC_PAGE + 0x1000,
                },
            ],
        );
    }

    #[test]
    fn no_overlap_returns_the_single_full_region() {
        let ram = 8u64 << 20;
        assert_eq!(
            parts(0, ram, LAPIC_PAGE, 0x1000),
            vec![MemSlotPart {
                gpa: 0,
                size: ram,
                host_off: 0,
            }],
        );
        assert_eq!(parts(0x10_0000, 0x10_0000, 0x100_0000, 0x1000).len(), 1);
        assert_eq!(parts(0x100_0000, 0x10_0000, 0, 0x1000).len(), 1);
    }

    #[test]
    fn hole_at_an_edge_drops_the_empty_remainder() {
        assert_eq!(
            parts(0x1000, 0x4000, 0x1000, 0x1000),
            vec![MemSlotPart {
                gpa: 0x2000,
                size: 0x3000,
                host_off: 0x1000,
            }],
        );
        assert_eq!(
            parts(0x1000, 0x4000, 0x4000, 0x1000),
            vec![MemSlotPart {
                gpa: 0x1000,
                size: 0x3000,
                host_off: 0,
            }],
        );
    }

    #[test]
    fn degenerate_regions_yield_no_slots() {
        assert!(parts(0x1000, 0, LAPIC_PAGE, 0x1000).is_empty());
        assert!(parts(0x1000, 0x1000, 0, 0x1_0000).is_empty());
    }

    fn decoded(slot_gpa: u64, slot_size: u64, bitmap: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        decode_dirty_bitmap(slot_gpa, slot_size, bitmap, &mut out);
        out
    }

    #[test]
    fn dirty_bitmap_decodes_bit_positions_to_absolute_gfns() {
        let bm = [(1u64 << 0) | (1 << 3) | (1 << 63), 1u64 << 1];
        assert_eq!(decoded(0, 128 * PAGE, &bm), vec![0, 3, 63, 65]);
        assert_eq!(decoded(0x1_0000, 128 * PAGE, &bm), vec![16, 19, 79, 81]);
        assert_eq!(decoded(0, 128 * PAGE, &[0, 0]), Vec::<u64>::new());
    }

    #[test]
    fn dirty_bitmap_ignores_padding_bits_past_the_slot() {
        let bm = [(1u64 << 0) | (1 << 2) | (1 << 3) | (1 << 63)];
        assert_eq!(decoded(0x2000, 3 * PAGE, &bm), vec![2, 4]);
    }

    #[test]
    fn dirty_bitmap_two_split_slots_translate_back_disjointly() {
        let hole = LAPIC_PAGE;
        let ram = 8u64 << 30;
        let ps: Vec<MemSlotPart> = parts(0, ram, hole, 0x1000);
        assert_eq!(ps.len(), 2);
        let mut out = Vec::new();
        decode_dirty_bitmap(ps[0].gpa, ps[0].size, &[1u64], &mut out);
        decode_dirty_bitmap(ps[1].gpa, ps[1].size, &[1u64], &mut out);
        let hole_gfn = hole / PAGE;
        assert_eq!(out, vec![0, hole_gfn + 1]);
        assert!(
            !out.contains(&hole_gfn),
            "the hole page is never a dirty gfn"
        );
    }

    proptest! {
        #![proptest_config(cases(512))]

        #[test]
        fn dirty_bitmap_decode_matches_a_naive_bit_scan(
            base_pages in 0u64..0x10_0000,
            slot_pages in 1u64..192,
            words in proptest::collection::vec(proptest::num::u64::ANY, 1..4),
        ) {
            let out = decoded(base_pages * PAGE, slot_pages * PAGE, &words);
            let mut expect = Vec::new();
            for page in 0..slot_pages.min(words.len() as u64 * 64) {
                let (w, b) = ((page / 64) as usize, page % 64);
                if words[w] & (1 << b) != 0 {
                    expect.push(base_pages + page);
                }
            }
            prop_assert_eq!(&out, &expect);
            prop_assert!(out.windows(2).all(|p| p[0] < p[1]), "strictly ascending");
        }
    }

    proptest! {
        #![proptest_config(cases(512))]

        #[test]
        fn split_around_hole_holds_its_contract(
            base_pages in 0u64..0x2000,
            len_pages in 0u64..0x2000,
            hole_pages in 0u64..0x2000,
            hole_len_pages in 0u64..4u64,
        ) {
            let base = base_pages * PAGE;
            let len = len_pages * PAGE;
            let hole = hole_pages * PAGE;
            let hole_len = hole_len_pages * PAGE;
            let end = base + len;
            let ps = parts(base, len, hole, hole_len);

            let mut covered = 0u64;
            let mut prev_end = base;
            for p in &ps {
                prop_assert!(p.size > 0, "non-empty");
                prop_assert_eq!(p.gpa % PAGE, 0);
                prop_assert_eq!(p.size % PAGE, 0);
                prop_assert_eq!(p.host_off % PAGE, 0);
                prop_assert_eq!(p.host_off, p.gpa - base, "host offset tracks the gpa");
                prop_assert!(p.gpa >= base && p.gpa + p.size <= end, "within the region");
                prop_assert!(p.gpa >= prev_end, "ordered, non-overlapping");
                prev_end = p.gpa + p.size;
                prop_assert!(
                    hole_len == 0 || p.gpa + p.size <= hole || p.gpa >= hole + hole_len,
                    "a part never overlaps the hole"
                );
                covered += p.size;
            }

            if hole >= base && hole + hole_len <= end {
                prop_assert_eq!(covered, len - hole_len);
            }
        }

        #[test]
        fn sub_hole_guest_is_one_region(len_pages in 1u64..0x4000) {
            let len = len_pages * PAGE;
            prop_assume!(len <= LAPIC_PAGE);
            prop_assert_eq!(
                parts(0, len, LAPIC_PAGE, 0x1000),
                vec![MemSlotPart { gpa: 0, size: len, host_off: 0 }],
            );
        }
    }
}
