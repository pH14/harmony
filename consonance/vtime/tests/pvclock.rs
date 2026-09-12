// SPDX-License-Identifier: AGPL-3.0-or-later

use proptest::prelude::*;
use vtime::pvclock::{
    PVCLOCK_ABI_VERSION, PVCLOCK_FLAGS_V1, PVCLOCK_PAGE_LEN, published, read, stamp,
    stamp_canonical,
};

#[derive(Debug, Clone)]
struct Refresh {
    vns: u64,
    guest_clock: u64,
    redundant: u8,
}

fn refresh_strategy() -> impl Strategy<Value = Refresh> {
    (any::<u64>(), any::<u64>(), 0u8..4).prop_map(|(vns, guest_clock, redundant)| Refresh {
        vns,
        guest_clock,
        redundant,
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn stamp_read_round_trip(vns in any::<u64>(), gc in any::<u64>(), hz in any::<u64>()) {
        let mut page = vec![0u8; PVCLOCK_PAGE_LEN];
        stamp(&mut page, vns, gc, hz);
        let f = read(&page).expect("stable frame after a completed stamp");
        prop_assert_eq!(f.vns, vns);
        prop_assert_eq!(f.guest_clock, gc);
        prop_assert_eq!(f.guest_clock_hz, hz);
        prop_assert_eq!(f.abi_version, PVCLOCK_ABI_VERSION);
        prop_assert_eq!(f.flags, PVCLOCK_FLAGS_V1);
        prop_assert_eq!(f.vcpu_index, 0);
        prop_assert_eq!(f.seq & 1, 0);
        prop_assert!(published(&page, vns, gc, hz));
    }

    #[test]
    fn page_bytes_are_schedule_independent(
        history in prop::collection::vec(refresh_strategy(), 1..20),
        hz in 1u64..u64::MAX,
    ) {
        let mut a = vec![0u8; PVCLOCK_PAGE_LEN];
        let mut b = vec![0u8; PVCLOCK_PAGE_LEN];
        for r in &history {
            stamp(&mut a, r.vns, r.guest_clock, hz);
            stamp(&mut b, r.vns, r.guest_clock, hz);
            for _ in 0..r.redundant {
                stamp(&mut b, r.vns, r.guest_clock, hz);
            }
        }
        prop_assert_eq!(a, b);
    }

    #[test]
    fn canonical_erases_history(
        ha in prop::collection::vec(refresh_strategy(), 0..12),
        hb in prop::collection::vec(refresh_strategy(), 0..12),
        seal_vns in any::<u64>(),
        seal_gc in any::<u64>(),
        next_vns in any::<u64>(),
        next_gc in any::<u64>(),
        hz in 1u64..u64::MAX,
    ) {
        let mut a = vec![0u8; PVCLOCK_PAGE_LEN];
        let mut b = vec![0u8; PVCLOCK_PAGE_LEN];
        for r in &ha { stamp(&mut a, r.vns, r.guest_clock, hz); }
        for r in &hb { stamp(&mut b, r.vns, r.guest_clock, hz); }
        stamp_canonical(&mut a, seal_vns, seal_gc, hz);
        stamp_canonical(&mut b, seal_vns, seal_gc, hz);
        prop_assert_eq!(&a, &b);
        prop_assert_eq!(read(&a).unwrap().seq, 0);
        stamp(&mut a, next_vns, next_gc, hz);
        stamp(&mut b, next_vns, next_gc, hz);
        prop_assert_eq!(a, b);
    }

    #[test]
    fn short_slices_never_panic(len in 0usize..PVCLOCK_PAGE_LEN, v in any::<u64>()) {
        let mut short = vec![0u8; len];
        prop_assert!(!stamp(&mut short, v, v, v));
        prop_assert!(!stamp_canonical(&mut short, v, v, v));
        prop_assert!(read(&short).is_none());
        prop_assert!(short.iter().all(|&b| b == 0));
    }
}
