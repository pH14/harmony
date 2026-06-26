// SPDX-License-Identifier: AGPL-3.0-or-later
//! Oracle property test (the core gate): drive the store and a naive model — one full
//! `Vec<u8>` image per snapshot — with arbitrary operation sequences and assert
//! byte-equality on every read and materialize. Deep-chain (>= 64) and wide-fan-out
//! (>= 32) shapes get dedicated oracle-checked tests below the proptest.

use proptest::prelude::*;
use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig, StoreError};

/// Small logical image so full-image models stay cheap across many snapshots.
const MEM_PAGES: u64 = 24;

/// Page content derived from a one-byte seed. The tiny content space (256 values,
/// including the all-zero page at seed 0) makes store-wide dedup and zero-page
/// handling constantly exercised.
fn page(seed: u8) -> [u8; PAGE_SIZE] {
    [seed; PAGE_SIZE]
}

/// The naive model: a full image per snapshot plus the refcount bookkeeping needed to
/// know which ops are valid. A snapshot with refcount 0 is dead: the store must treat
/// its id as unknown from that point on.
struct ModelSnap {
    id: SnapshotId,
    image: Vec<u8>,
    vm_state: Vec<u8>,
    refcount: u64,
}

struct Model {
    snaps: Vec<ModelSnap>, // creation order; indexes are stable
}

impl Model {
    fn live_indices(&self) -> Vec<usize> {
        (0..self.snaps.len())
            .filter(|&i| self.snaps[i].refcount > 0)
            .collect()
    }

    /// Distinct non-zero page contents across all live images. Every one of these is
    /// returned by some `read_page`, so the store must hold at least this many unique
    /// pages; and at most the number of distinct non-zero contents ever written.
    fn live_distinct_nonzero_pages(&self) -> usize {
        let zero = [0u8; PAGE_SIZE];
        let mut set = std::collections::BTreeSet::new();
        for i in self.live_indices() {
            for chunk in self.snaps[i].image.chunks_exact(PAGE_SIZE) {
                if chunk != zero {
                    set.insert(chunk.to_vec());
                }
            }
        }
        set.len()
    }
}

#[derive(Debug, Clone)]
enum Op {
    /// Derive from a live snapshot (selector reduced modulo the live count), write a
    /// batch of pages, seal with the given vm_state.
    Derive {
        parent_sel: usize,
        writes: Vec<(u64, u8)>,
        vm_state: Vec<u8>,
    },
    Read {
        snap_sel: usize,
        gfn_sel: u64,
    },
    /// Full-image compare, plus a copy-on-write probe: scribble on the mapping and
    /// confirm the store still reads the original bytes.
    Materialize {
        snap_sel: usize,
    },
    Retain {
        snap_sel: usize,
    },
    Release {
        snap_sel: usize,
    },
    Gc,
}

fn op_strategy() -> impl Strategy<Value = Op> {
    let writes = prop::collection::vec((any::<u64>(), any::<u8>()), 0..8);
    let vm_state = prop::collection::vec(any::<u8>(), 0..16);
    prop_oneof![
        3 => (any::<usize>(), writes, vm_state).prop_map(|(parent_sel, writes, vm_state)| {
            Op::Derive { parent_sel, writes, vm_state }
        }),
        4 => (any::<usize>(), any::<u64>())
            .prop_map(|(snap_sel, gfn_sel)| Op::Read { snap_sel, gfn_sel }),
        1 => any::<usize>().prop_map(|snap_sel| Op::Materialize { snap_sel }),
        1 => any::<usize>().prop_map(|snap_sel| Op::Retain { snap_sel }),
        2 => any::<usize>().prop_map(|snap_sel| Op::Release { snap_sel }),
        1 => Just(Op::Gc),
    ]
}

fn assert_page_eq(store: &Store, model: &ModelSnap, gfn: u64) {
    let mut out = [0xAAu8; PAGE_SIZE];
    store.read_page(model.id, gfn, &mut out).unwrap();
    let off = gfn as usize * PAGE_SIZE;
    assert_eq!(
        &out[..],
        &model.image[off..off + PAGE_SIZE],
        "snapshot {:?} gfn {gfn} diverged from model",
        model.id
    );
}

fn assert_snapshot_eq(store: &Store, model: &ModelSnap) {
    for gfn in 0..MEM_PAGES {
        assert_page_eq(store, model, gfn);
    }
}

fn apply(store: &mut Store, model: &mut Model, op: Op) {
    match op {
        Op::Derive {
            parent_sel,
            writes,
            vm_state,
        } => {
            let live = model.live_indices();
            let Some(&pidx) = live.get(parent_sel % live.len().max(1)) else {
                return; // nothing live to derive from
            };
            let parent_id = model.snaps[pidx].id;
            let mut image = model.snaps[pidx].image.clone();
            let mut builder = store.derive(parent_id).unwrap();
            for (gfn_sel, seed) in writes {
                let gfn = gfn_sel % MEM_PAGES;
                let content = page(seed);
                builder.write_page(gfn, &content).unwrap();
                let off = gfn as usize * PAGE_SIZE;
                image[off..off + PAGE_SIZE].copy_from_slice(&content);
            }
            let id = builder.seal(vm_state.clone());
            assert_eq!(store.vm_state(id).unwrap(), &vm_state[..]);
            model.snaps.push(ModelSnap {
                id,
                image,
                vm_state,
                refcount: 1,
            });
        }
        Op::Read { snap_sel, gfn_sel } => {
            let live = model.live_indices();
            let Some(&idx) = live.get(snap_sel % live.len().max(1)) else {
                return;
            };
            assert_page_eq(store, &model.snaps[idx], gfn_sel % MEM_PAGES);
        }
        Op::Materialize { snap_sel } => {
            let live = model.live_indices();
            let Some(&idx) = live.get(snap_sel % live.len().max(1)) else {
                return;
            };
            let snap = &model.snaps[idx];
            let mut mapping = store.materialize(snap.id).unwrap();
            assert_eq!(mapping.len(), snap.image.len());
            assert_eq!(mapping.as_slice(), &snap.image[..], "materialize diverged");
            // CoW probe: scribbling on the mapping must never reach the store.
            mapping.as_mut_slice()[..PAGE_SIZE].fill(0x5C);
            assert_page_eq(store, snap, 0);
        }
        Op::Retain { snap_sel } => {
            let live = model.live_indices();
            let Some(&idx) = live.get(snap_sel % live.len().max(1)) else {
                return;
            };
            store.retain(model.snaps[idx].id).unwrap();
            model.snaps[idx].refcount += 1;
        }
        Op::Release { snap_sel } => {
            let live = model.live_indices();
            let Some(&idx) = live.get(snap_sel % live.len().max(1)) else {
                return;
            };
            store.release(model.snaps[idx].id).unwrap();
            model.snaps[idx].refcount -= 1;
            if model.snaps[idx].refcount == 0 {
                // Dead means unobservable, immediately.
                let id = model.snaps[idx].id;
                let mut out = [0u8; PAGE_SIZE];
                assert!(matches!(
                    store.read_page(id, 0, &mut out),
                    Err(StoreError::UnknownSnapshot(_)) | Err(StoreError::GfnOutOfRange { .. })
                ));
                assert!(matches!(
                    store.stats(id),
                    Err(StoreError::UnknownSnapshot(_))
                ));
            }
        }
        Op::Gc => {
            store.gc();
            // gc is idempotent: an immediate second pass finds nothing more.
            assert_eq!(store.gc(), 0, "second gc in a row freed bytes");
        }
    }

    // Store-wide invariants, cheap enough to hold after every op.
    let stats = store.store_stats();
    assert_eq!(stats.snapshots, model.live_indices().len() as u64);
    assert_eq!(
        stats.logical_pages_total,
        stats.snapshots * MEM_PAGES,
        "logical_pages_total must be live snapshots x mem_pages"
    );
    assert!(
        stats.stored_unique_pages as usize >= model.live_distinct_nonzero_pages(),
        "store must hold every distinct content live images expose"
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn oracle(
        base_writes in prop::collection::vec((any::<u64>(), any::<u8>()), 0..20),
        ops in prop::collection::vec(op_strategy(), 0..40),
    ) {
        let mut store = Store::new(StoreConfig { mem_pages: MEM_PAGES });
        let mut model = Model { snaps: Vec::new() };

        // Base with random sparse pages.
        let mut builder = store.begin_base();
        let mut image = vec![0u8; (MEM_PAGES as usize) * PAGE_SIZE];
        for (gfn_sel, seed) in base_writes {
            let gfn = gfn_sel % MEM_PAGES;
            let content = page(seed);
            builder.write_page(gfn, &content).unwrap();
            let off = gfn as usize * PAGE_SIZE;
            image[off..off + PAGE_SIZE].copy_from_slice(&content);
        }
        let id = builder.seal(b"base".to_vec());
        model.snaps.push(ModelSnap { id, image, vm_state: b"base".to_vec(), refcount: 1 });

        for op in ops {
            apply(&mut store, &mut model, op);
        }

        // Final sweep: every live snapshot still matches the model, page by page and
        // through a fresh materialize.
        store.gc();
        for idx in model.live_indices() {
            let snap = &model.snaps[idx];
            assert_snapshot_eq(&store, snap);
            let mapping = store.materialize(snap.id).unwrap();
            prop_assert_eq!(mapping.as_slice(), &snap.image[..]);
            prop_assert_eq!(store.vm_state(snap.id).unwrap(), &snap.vm_state[..]);
        }
    }
}

/// Deterministic page content for the shaped tests: a function of (layer, gfn) with
/// enough repetition to exercise dedup.
fn shaped_page(layer: usize, gfn: u64) -> [u8; PAGE_SIZE] {
    page((layer as u8).wrapping_mul(31).wrapping_add(gfn as u8) % 13)
}

#[test]
fn oracle_deep_chain() {
    const DEPTH: usize = 80; // >= 64 per the gate
    const PAGES: u64 = 16;
    let mut store = Store::new(StoreConfig { mem_pages: PAGES });
    let mut images: Vec<(SnapshotId, Vec<u8>)> = Vec::new();

    let mut builder = store.begin_base();
    let mut image = vec![0u8; PAGES as usize * PAGE_SIZE];
    for gfn in 0..4 {
        let content = shaped_page(0, gfn);
        builder.write_page(gfn, &content).unwrap();
        image[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE].copy_from_slice(&content);
    }
    images.push((builder.seal(vec![]), image));

    for layer in 1..DEPTH {
        let (parent_id, parent_image) = &images[layer - 1];
        let mut builder = store.derive(*parent_id).unwrap();
        let mut image = parent_image.clone();
        // 1-3 writes per layer, sliding across the gfn space.
        for k in 0..=(layer % 3) {
            let gfn = ((layer + 5 * k) as u64) % PAGES;
            let content = shaped_page(layer, gfn);
            builder.write_page(gfn, &content).unwrap();
            image[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE]
                .copy_from_slice(&content);
        }
        images.push((builder.seal(vec![]), image));
    }

    let (leaf_id, _) = images[DEPTH - 1];
    assert_eq!(store.stats(leaf_id).unwrap().chain_len, DEPTH as u32);

    // Every snapshot in the chain matches its model image, every page.
    let mut out = [0u8; PAGE_SIZE];
    for (id, image) in &images {
        for gfn in 0..PAGES {
            store.read_page(*id, gfn, &mut out).unwrap();
            let off = gfn as usize * PAGE_SIZE;
            assert_eq!(&out[..], &image[off..off + PAGE_SIZE]);
        }
    }
    let mapping = store.materialize(leaf_id).unwrap();
    assert_eq!(mapping.as_slice(), &images[DEPTH - 1].1[..]);
}

#[test]
fn oracle_wide_fanout() {
    const CHILDREN: usize = 40; // >= 32 per the gate
    const PAGES: u64 = 16;
    let mut store = Store::new(StoreConfig { mem_pages: PAGES });

    let mut builder = store.begin_base();
    let mut base_image = vec![0u8; PAGES as usize * PAGE_SIZE];
    for gfn in 0..8 {
        let content = shaped_page(0, gfn);
        builder.write_page(gfn, &content).unwrap();
        base_image[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE]
            .copy_from_slice(&content);
    }
    let base = builder.seal(vec![]);

    let mut children: Vec<(SnapshotId, Vec<u8>)> = Vec::new();
    for c in 1..=CHILDREN {
        let mut builder = store.derive(base).unwrap();
        let mut image = base_image.clone();
        // Two pages distinct per child, one rewritten identical to the base (must be
        // dropped at seal and not counted as owned).
        for (slot, gfn) in [(0u64, (c as u64) % PAGES), (1, (c as u64 + 7) % PAGES)] {
            let content = page((c as u8).wrapping_mul(2).wrapping_add(slot as u8 + 100));
            builder.write_page(gfn, &content).unwrap();
            image[gfn as usize * PAGE_SIZE..(gfn as usize + 1) * PAGE_SIZE]
                .copy_from_slice(&content);
        }
        let dup_gfn = (c as u64) % 8;
        let off = dup_gfn as usize * PAGE_SIZE;
        let dup: [u8; PAGE_SIZE] = base_image[off..off + PAGE_SIZE].try_into().unwrap();
        builder.write_page(dup_gfn, &dup).unwrap();
        let mut expect = image.clone();
        expect[off..off + PAGE_SIZE].copy_from_slice(&dup);
        let id = builder.seal(vec![]);
        children.push((id, expect));
    }

    let mut out = [0u8; PAGE_SIZE];
    for (id, image) in &children {
        assert_eq!(store.stats(*id).unwrap().chain_len, 2);
        for gfn in 0..PAGES {
            store.read_page(*id, gfn, &mut out).unwrap();
            let off = gfn as usize * PAGE_SIZE;
            assert_eq!(&out[..], &image[off..off + PAGE_SIZE]);
        }
    }
    assert_eq!(store.store_stats().snapshots, CHILDREN as u64 + 1);
}
