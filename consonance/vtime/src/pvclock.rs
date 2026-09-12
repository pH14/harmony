// SPDX-License-Identifier: AGPL-3.0-or-later

pub const PVCLOCK_ABI_VERSION: u32 = 1;

pub const PVCLOCK_PAGE_LEN: usize = 4096;

pub const PVCLOCK_FLAG_MATERIALIZED: u32 = 1;

pub const PVCLOCK_FLAG_EXIT_COUNT_DERIVED: u32 = 1 << 1;

pub const PVCLOCK_FLAGS_V1: u32 = PVCLOCK_FLAG_MATERIALIZED | PVCLOCK_FLAG_EXIT_COUNT_DERIVED;

pub const ABI_VERSION_OFF: usize = 0x00;
pub const SEQ_OFF: usize = 0x04;
pub const VNS_OFF: usize = 0x08;
pub const GUEST_CLOCK_OFF: usize = 0x10;
pub const GUEST_CLOCK_HZ_OFF: usize = 0x18;
pub const FLAGS_OFF: usize = 0x20;
pub const VCPU_INDEX_OFF: usize = 0x24;
pub const RESERVED_OFF: usize = 0x28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PvclockFields {
    pub abi_version: u32,
    pub seq: u32,
    pub vns: u64,
    pub guest_clock: u64,
    pub guest_clock_hz: u64,
    pub flags: u32,
    pub vcpu_index: u32,
}

#[inline]
fn put_u32(page: &mut [u8], off: usize, v: u32) {
    page[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn put_u64(page: &mut [u8], off: usize, v: u64) {
    page[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn get_u32(page: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([page[off], page[off + 1], page[off + 2], page[off + 3]])
}

#[inline]
fn get_u64(page: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&page[off..off + 8]);
    u64::from_le_bytes(b)
}

pub fn published(page: &[u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    get_u32(page, SEQ_OFF) & 1 == 0
        && get_u32(page, ABI_VERSION_OFF) == PVCLOCK_ABI_VERSION
        && get_u64(page, VNS_OFF) == vns
        && get_u64(page, GUEST_CLOCK_OFF) == guest_clock
        && get_u64(page, GUEST_CLOCK_HZ_OFF) == guest_clock_hz
        && get_u32(page, FLAGS_OFF) == PVCLOCK_FLAGS_V1
        && get_u32(page, VCPU_INDEX_OFF) == 0
}

pub fn stamp(page: &mut [u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    if published(page, vns, guest_clock, guest_clock_hz) {
        return false;
    }
    let odd = get_u32(page, SEQ_OFF) | 1;
    put_u32(page, SEQ_OFF, odd);
    put_u32(page, ABI_VERSION_OFF, PVCLOCK_ABI_VERSION);
    put_u64(page, VNS_OFF, vns);
    put_u64(page, GUEST_CLOCK_OFF, guest_clock);
    put_u64(page, GUEST_CLOCK_HZ_OFF, guest_clock_hz);
    put_u32(page, FLAGS_OFF, PVCLOCK_FLAGS_V1);
    put_u32(page, VCPU_INDEX_OFF, 0);
    put_u32(page, SEQ_OFF, odd.wrapping_add(1));
    true
}

pub fn stamp_canonical(page: &mut [u8], vns: u64, guest_clock: u64, guest_clock_hz: u64) -> bool {
    if page.len() < PVCLOCK_PAGE_LEN {
        return false;
    }
    let mut canonical = [0u8; PVCLOCK_PAGE_LEN];
    put_u32(&mut canonical, ABI_VERSION_OFF, PVCLOCK_ABI_VERSION);
    put_u64(&mut canonical, VNS_OFF, vns);
    put_u64(&mut canonical, GUEST_CLOCK_OFF, guest_clock);
    put_u64(&mut canonical, GUEST_CLOCK_HZ_OFF, guest_clock_hz);
    put_u32(&mut canonical, FLAGS_OFF, PVCLOCK_FLAGS_V1);
    if page[..PVCLOCK_PAGE_LEN] == canonical {
        return false;
    }
    page[..PVCLOCK_PAGE_LEN].copy_from_slice(&canonical);
    true
}

pub fn read(page: &[u8]) -> Option<PvclockFields> {
    if page.len() < PVCLOCK_PAGE_LEN {
        return None;
    }
    let seq = get_u32(page, SEQ_OFF);
    if seq & 1 != 0 {
        return None;
    }
    let abi_version = get_u32(page, ABI_VERSION_OFF);
    if abi_version != PVCLOCK_ABI_VERSION {
        return None;
    }
    Some(PvclockFields {
        abi_version,
        seq,
        vns: get_u64(page, VNS_OFF),
        guest_clock: get_u64(page, GUEST_CLOCK_OFF),
        guest_clock_hz: get_u64(page, GUEST_CLOCK_HZ_OFF),
        flags: get_u32(page, FLAGS_OFF),
        vcpu_index: get_u32(page, VCPU_INDEX_OFF),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pvclock_flag_bits_are_pinned() {
        assert_eq!(PVCLOCK_FLAG_MATERIALIZED, 0x1);
        assert_eq!(PVCLOCK_FLAG_EXIT_COUNT_DERIVED, 0x2);
    }

    fn fresh_page() -> Vec<u8> {
        vec![0u8; PVCLOCK_PAGE_LEN]
    }

    #[test]
    fn stamp_publishes_all_fields_le() {
        let mut page = fresh_page();
        assert!(stamp(
            &mut page,
            0x1122_3344_5566_7788,
            0xAABB_CCDD_EEFF_0011,
            2_000_000_000
        ));
        assert_eq!(
            page[ABI_VERSION_OFF..ABI_VERSION_OFF + 4],
            1u32.to_le_bytes()
        );
        assert_eq!(
            page[VNS_OFF..VNS_OFF + 8],
            0x1122_3344_5566_7788u64.to_le_bytes()
        );
        assert_eq!(
            page[GUEST_CLOCK_OFF..GUEST_CLOCK_OFF + 8],
            0xAABB_CCDD_EEFF_0011u64.to_le_bytes()
        );
        assert_eq!(
            page[GUEST_CLOCK_HZ_OFF..GUEST_CLOCK_HZ_OFF + 8],
            2_000_000_000u64.to_le_bytes()
        );
        assert_eq!(page[FLAGS_OFF..FLAGS_OFF + 4], 3u32.to_le_bytes());
        assert_eq!(page[VCPU_INDEX_OFF..VCPU_INDEX_OFF + 4], 0u32.to_le_bytes());
        let f = read(&page).expect("stable frame");
        assert_eq!(f.vns, 0x1122_3344_5566_7788);
        assert_eq!(f.guest_clock, 0xAABB_CCDD_EEFF_0011);
        assert_eq!(f.guest_clock_hz, 2_000_000_000);
        assert_eq!(f.flags, PVCLOCK_FLAGS_V1);
        assert_eq!(f.vcpu_index, 0);
        assert_eq!(f.seq, 2);
    }

    #[test]
    fn stamp_is_value_keyed_idempotent() {
        let mut page = fresh_page();
        assert!(stamp(&mut page, 100, 200, 1_000_000_000));
        let snapshot = page.clone();
        assert!(!stamp(&mut page, 100, 200, 1_000_000_000));
        assert_eq!(page, snapshot);
        assert!(stamp(&mut page, 101, 202, 1_000_000_000));
        assert_eq!(read(&page).unwrap().seq, 4);
    }

    #[test]
    fn stamp_epoch_forces_straddling_reader_retry() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 2, 3);
        let seq_before = read(&page).unwrap().seq;
        stamp(&mut page, 10, 20, 3);
        assert_ne!(read(&page).unwrap().seq, seq_before);
    }

    #[test]
    fn canonical_is_total_function_of_values() {
        let mut a = fresh_page();
        let mut b = fresh_page();
        for i in 0..7u64 {
            stamp(&mut a, i, 2 * i, 5);
        }
        stamp(&mut b, 999, 999, 5);
        b[RESERVED_OFF + 100] = 0xEE;
        assert!(stamp_canonical(&mut a, 42, 84, 5));
        assert!(stamp_canonical(&mut b, 42, 84, 5));
        assert_eq!(a, b);
        assert_eq!(read(&a).unwrap().seq, 0);
        let snap = a.clone();
        assert!(!stamp_canonical(&mut a, 42, 84, 5));
        assert_eq!(a, snap);
    }

    #[test]
    fn a_verbatim_sealed_page_keeps_restored_and_continued_runs_in_lockstep() {
        let mut parent = fresh_page();
        for i in 0..5u64 {
            stamp(&mut parent, i, i, 7);
        }
        let sealed = parent.clone();
        let mut child = sealed.clone();
        assert_eq!(parent, child);
        assert!(stamp(&mut parent, 9, 9, 7));
        assert!(stamp(&mut child, 9, 9, 7));
        assert_eq!(parent, child);
    }

    #[test]
    fn canonical_reset_would_be_an_aba_on_a_live_page() {
        let mut page = fresh_page();
        stamp_canonical(&mut page, 100, 200, 7);
        let reader_sampled_seq = read(&page).unwrap().seq;
        let reader_loaded_vns = read(&page).unwrap().vns;
        stamp(&mut page, 500, 600, 7);
        assert_ne!(
            read(&page).unwrap().seq,
            reader_sampled_seq,
            "mid-run the epoch moves, so the straddling reader retries — correct"
        );
        stamp_canonical(&mut page, 500, 600, 7);
        assert_eq!(
            read(&page).unwrap().seq,
            reader_sampled_seq,
            "the epoch is back where the reader sampled it: its re-read validates and it \
             accepts vns={reader_loaded_vns} even though the page now publishes 500 — the ABA \
             the seal path must not create"
        );
    }

    #[test]
    fn stamp_repairs_guest_scribbles_in_fixed_fields() {
        let mut page = fresh_page();
        stamp(&mut page, 5, 10, 3);
        put_u32(&mut page, FLAGS_OFF, 0xDEAD);
        assert!(stamp(&mut page, 5, 10, 3));
        assert_eq!(read(&page).unwrap().flags, PVCLOCK_FLAGS_V1);
    }

    #[test]
    fn read_refuses_odd_seq_and_bad_abi() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 2, 3);
        put_u32(&mut page, SEQ_OFF, 7);
        assert!(read(&page).is_none());
        put_u32(&mut page, SEQ_OFF, 8);
        put_u32(&mut page, ABI_VERSION_OFF, 2);
        assert!(read(&page).is_none());
    }

    #[test]
    fn short_slices_are_total_no_ops() {
        let mut short = vec![0u8; PVCLOCK_PAGE_LEN - 1];
        assert!(!stamp(&mut short, 1, 2, 3));
        assert!(!stamp_canonical(&mut short, 1, 2, 3));
        assert!(!published(&short, 1, 2, 3));
        assert!(read(&short).is_none());
        assert!(short.iter().all(|&b| b == 0), "no partial write");
    }

    #[test]
    fn seq_epoch_wraps_deterministically() {
        let mut page = fresh_page();
        stamp(&mut page, 1, 1, 1);
        put_u32(&mut page, SEQ_OFF, u32::MAX - 1);
        stamp(&mut page, 2, 2, 1);
        assert_eq!(read(&page).unwrap().seq, 0);
    }
}
