// SPDX-License-Identifier: AGPL-3.0-or-later
//! Acceptance gates 2-5 from the task spec: dedup, zero pages (including the sparse
//! 1 GiB materialize), mapping copy-on-write, and gc, plus error-path coverage.

use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig, StoreError};

fn store(mem_pages: u64) -> Store {
    Store::new(StoreConfig { mem_pages })
}

fn page(seed: u8) -> [u8; PAGE_SIZE] {
    [seed; PAGE_SIZE]
}

/// Base of N distinct pages plus children whose writes don't change anything.
fn base_of_n(store: &mut Store, n: u8) -> SnapshotId {
    let mut b = store.begin_base();
    for i in 0..n {
        b.write_page(u64::from(i), &page(i + 1)).unwrap();
    }
    b.seal(b"base-vm-state".to_vec())
}

/// Gate 2: a base of N distinct pages, plus 10 children each rewriting the same pages
/// with identical content => stored_unique_pages stays N and children own nothing.
#[test]
fn dedup_identical_rewrites() {
    const N: u8 = 8;
    let mut s = store(64);
    let base = base_of_n(&mut s, N);
    assert_eq!(s.store_stats().stored_unique_pages, u64::from(N));
    assert_eq!(s.stats(base).unwrap().owned_pages, u64::from(N));

    for _ in 0..10 {
        let mut d = s.derive(base).unwrap();
        for i in 0..N {
            d.write_page(u64::from(i), &page(i + 1)).unwrap(); // identical rewrite
        }
        let child = d.seal(vec![]);
        assert_eq!(s.stats(child).unwrap().owned_pages, 0);
        assert_eq!(s.store_stats().stored_unique_pages, u64::from(N));
    }
    assert_eq!(s.store_stats().snapshots, 11);

    // Sanity check the other direction: one genuinely new content is stored once.
    let mut d = s.derive(base).unwrap();
    d.write_page(0, &page(200)).unwrap();
    let child = d.seal(vec![]);
    assert_eq!(s.stats(child).unwrap().owned_pages, 1);
    assert_eq!(s.store_stats().stored_unique_pages, u64::from(N) + 1);

    // And identical content across *different* gfns is also stored once store-wide.
    let mut d = s.derive(base).unwrap();
    d.write_page(20, &page(1)).unwrap(); // same bytes the base has at gfn 0
    let child = d.seal(vec![]);
    assert_eq!(s.stats(child).unwrap().owned_pages, 1); // gfn 20 wasn't provided before
    assert_eq!(s.store_stats().stored_unique_pages, u64::from(N) + 1);
}

/// Gate 3a: never-written pages read as zeros at every chain depth and in materialize.
#[test]
fn zero_pages_at_every_depth() {
    let mut s = store(16);
    let mut b = s.begin_base();
    b.write_page(0, &page(9)).unwrap();
    let mut snaps = vec![b.seal(vec![])];
    for depth in 1..6 {
        let mut d = s.derive(snaps[depth - 1]).unwrap();
        if depth == 3 {
            d.write_page(2, &page(33)).unwrap();
        }
        snaps.push(d.seal(vec![]));
    }

    let zero = page(0);
    let mut out = page(0xFF);
    for (depth, &snap) in snaps.iter().enumerate() {
        for gfn in [1u64, 7, 15] {
            s.read_page(snap, gfn, &mut out).unwrap();
            assert_eq!(out, zero, "depth {depth} gfn {gfn} not zero");
        }
        s.read_page(snap, 2, &mut out).unwrap();
        assert_eq!(out, if depth >= 3 { page(33) } else { zero });

        let mapping = s.materialize(snap).unwrap();
        let image = mapping.as_slice();
        assert_eq!(&image[PAGE_SIZE..2 * PAGE_SIZE], &zero[..]);
        assert_eq!(&image[15 * PAGE_SIZE..], &zero[..]);
        assert_eq!(&image[..PAGE_SIZE], &page(9)[..]);
    }
}

/// Gate 3b: a sparse 1 GiB-logical base with 10 written pages materializes without
/// allocating ~1 GiB of resident memory (asserted via bytes_resident).
#[test]
fn sparse_one_gib_materialize_stays_sparse() {
    const GIB: u64 = 1 << 30;
    const MEM_PAGES: u64 = GIB / PAGE_SIZE as u64; // 262,144 pages
    let mut s = store(MEM_PAGES);
    let mut b = s.begin_base();
    let written: Vec<u64> = (0..10).map(|i| i * 26_000 + 13).collect();
    for (i, &gfn) in written.iter().enumerate() {
        b.write_page(gfn, &page(i as u8 + 1)).unwrap();
    }
    let base = b.seal(b"vm".to_vec());

    let stats = s.store_stats();
    assert_eq!(stats.stored_unique_pages, 10);
    assert!(
        stats.bytes_resident < 1 << 20,
        "store should hold ~10 pages, found {} bytes resident",
        stats.bytes_resident
    );

    let mapping = s.materialize(base).unwrap();
    assert_eq!(mapping.len() as u64, GIB);
    // Materializing must not have inflated the store either.
    assert!(
        s.store_stats().bytes_resident < 1 << 20,
        "materialize inflated bytes_resident to {}",
        s.store_stats().bytes_resident
    );

    // Spot-check written pages and holes through the mapping.
    let image = mapping.as_slice();
    for (i, &gfn) in written.iter().enumerate() {
        let off = gfn as usize * PAGE_SIZE;
        assert_eq!(&image[off..off + PAGE_SIZE], &page(i as u8 + 1)[..]);
    }
    for gfn in [0u64, 1000, MEM_PAGES - 1] {
        let off = gfn as usize * PAGE_SIZE;
        assert_eq!(&image[off..off + PAGE_SIZE], &page(0)[..]);
    }
}

/// Gate 4: write to a materialized mapping, then re-read via read_page and a fresh
/// materialize => original content intact.
#[test]
fn mapping_writes_never_reach_the_store() {
    let mut s = store(8);
    let base = base_of_n(&mut s, 4);

    let mut mapping = s.materialize(base).unwrap();
    mapping.as_mut_slice().fill(0xEE); // scribble over everything, holes included
    assert_eq!(&mapping.as_slice()[..PAGE_SIZE], &page(0xEE)[..]); // visible locally

    let mut out = page(0);
    for i in 0..4u8 {
        s.read_page(base, u64::from(i), &mut out).unwrap();
        assert_eq!(out, page(i + 1), "read_page sees mapping writes");
    }
    s.read_page(base, 7, &mut out).unwrap();
    assert_eq!(out, page(0));

    let fresh = s.materialize(base).unwrap();
    for i in 0..4usize {
        assert_eq!(
            &fresh.as_slice()[i * PAGE_SIZE..(i + 1) * PAGE_SIZE],
            &page(i as u8 + 1)[..],
            "fresh materialize sees mapping writes"
        );
    }
    assert_eq!(&fresh.as_slice()[7 * PAGE_SIZE..], &page(0)[..]);
}

/// Gate 5: chain A->B->C; releasing B frees nothing C needs; releasing C then gc
/// shrinks stats accordingly; releasing everything frees everything.
#[test]
fn gc_preserves_live_chains_then_reclaims() {
    let mut s = store(8);
    let mut b = s.begin_base();
    b.write_page(0, &page(1)).unwrap();
    let a = b.seal(b"A".to_vec());
    let mut d = s.derive(a).unwrap();
    d.write_page(1, &page(2)).unwrap();
    let b_snap = d.seal(b"B".to_vec());
    let mut d = s.derive(b_snap).unwrap();
    d.write_page(2, &page(3)).unwrap();
    let c = d.seal(b"C".to_vec());

    s.release(b_snap).unwrap();
    assert_eq!(
        s.gc(),
        0,
        "B is an ancestor of live C; nothing may be freed"
    );
    assert_eq!(s.store_stats().snapshots, 2);
    assert_eq!(s.store_stats().stored_unique_pages, 3);

    // C still resolves pages provided by A, by the released B, and by itself.
    let mut out = page(0);
    for (gfn, seed) in [(0u64, 1u8), (1, 2), (2, 3)] {
        s.read_page(c, gfn, &mut out).unwrap();
        assert_eq!(out, page(seed), "C lost gfn {gfn} after releasing B");
    }

    s.release(c).unwrap();
    // B's and C's layers are now unreachable: their unique pages and vm blobs go.
    let freed = s.gc();
    assert_eq!(freed, 2 * PAGE_SIZE as u64 + 2);
    let stats = s.store_stats();
    assert_eq!(stats.snapshots, 1);
    assert_eq!(stats.stored_unique_pages, 1);
    assert_eq!(stats.bytes_resident, PAGE_SIZE as u64 + 1);

    // A still reads fine on its own.
    s.read_page(a, 0, &mut out).unwrap();
    assert_eq!(out, page(1));

    s.release(a).unwrap();
    assert_eq!(s.gc(), PAGE_SIZE as u64 + 1);
    let stats = s.store_stats();
    assert_eq!(stats.snapshots, 0);
    assert_eq!(stats.stored_unique_pages, 0);
    assert_eq!(stats.bytes_resident, 0);
    assert_eq!(stats.logical_pages_total, 0);
}

/// gc keeps shared structure alive while any branch needs it (tree shape, not chain).
#[test]
fn gc_with_shared_ancestor_fanout() {
    let mut s = store(8);
    let base = base_of_n(&mut s, 4);
    let mut kids = Vec::new();
    for k in 0..4u8 {
        let mut d = s.derive(base).unwrap();
        d.write_page(6, &page(100 + k)).unwrap();
        kids.push(d.seal(vec![]));
    }
    s.release(base).unwrap();
    assert_eq!(s.gc(), 0, "base is shared by 4 live children");

    let mut out = page(0);
    for (k, &kid) in kids.iter().enumerate() {
        s.read_page(kid, 0, &mut out).unwrap();
        assert_eq!(out, page(1)); // inherited from the released base
        s.read_page(kid, 6, &mut out).unwrap();
        assert_eq!(out, page(100 + k as u8));
    }

    for &kid in &kids[..3] {
        s.release(kid).unwrap();
    }
    let freed = s.gc();
    assert_eq!(freed, 3 * PAGE_SIZE as u64, "three children's unique pages");
    s.read_page(kids[3], 0, &mut out).unwrap();
    assert_eq!(out, page(1));
}

#[test]
fn error_paths() {
    let mut s = store(4);
    let base = base_of_n(&mut s, 2);

    let mut out = page(0);
    assert!(matches!(
        s.read_page(base, 4, &mut out),
        Err(StoreError::GfnOutOfRange {
            gfn: 4,
            mem_pages: 4
        })
    ));
    assert!(matches!(
        s.read_page(base, 0, &mut [0u8; 100]),
        Err(StoreError::BadPageLength { len: 100 })
    ));

    let mut b = s.derive(base).unwrap();
    assert!(matches!(
        b.write_page(99, &page(1)),
        Err(StoreError::GfnOutOfRange { gfn: 99, .. })
    ));
    assert!(matches!(
        b.write_page(0, &[1u8; 12]),
        Err(StoreError::BadPageLength { len: 12 })
    ));
    drop(b);

    // A fully released id is unknown to every entry point.
    let mut d = s.derive(base).unwrap();
    d.write_page(0, &page(50)).unwrap();
    let child = d.seal(vec![]);
    s.release(child).unwrap();
    assert!(matches!(
        s.read_page(child, 0, &mut out),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.derive(child),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.vm_state(child),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.materialize(child),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.retain(child),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.release(child),
        Err(StoreError::UnknownSnapshot(_))
    ));
    assert!(matches!(
        s.stats(child),
        Err(StoreError::UnknownSnapshot(_))
    ));

    // Errors render without panicking.
    assert!(!StoreError::UnknownSnapshot(base).to_string().is_empty());
}

/// Immutability under everything at once: later snapshots, gc, dedup, and mapping
/// writes leave a sealed snapshot's logical image bit-identical.
#[test]
fn sealed_images_are_immutable() {
    let mut s = store(8);
    let base = base_of_n(&mut s, 4);
    let mut before = Vec::new();
    for gfn in 0..8u64 {
        let mut out = page(0);
        s.read_page(base, gfn, &mut out).unwrap();
        before.push(out);
    }

    // Churn: children overwriting everything, retain/release cycles, gc, CoW writes.
    let mut kids = Vec::new();
    for k in 0..6u8 {
        let mut d = s.derive(base).unwrap();
        for gfn in 0..8u64 {
            d.write_page(gfn, &page(k.wrapping_mul(40).wrapping_add(gfn as u8)))
                .unwrap();
        }
        kids.push(d.seal(vec![k]));
    }
    s.retain(base).unwrap();
    for &k in &kids {
        s.release(k).unwrap();
    }
    s.gc();
    let mut mapping = s.materialize(base).unwrap();
    mapping.as_mut_slice().fill(0xBB);
    drop(mapping);

    for gfn in 0..8u64 {
        let mut out = page(0);
        s.read_page(base, gfn, &mut out).unwrap();
        assert_eq!(out, before[gfn as usize], "base image changed at gfn {gfn}");
    }
}
