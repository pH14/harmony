// SPDX-License-Identifier: AGPL-3.0-or-later
//! Model-based (stateful) property test for [`snapshot_store::Store`].
//!
//! `proptest-state-machine` generates a precondition-satisfying sequence of
//! operations — `begin_base` (the initial state) plus `derive`/write/`seal`,
//! `read_page`, `materialize`, `retain`, `release`, and `gc` — and drives them
//! against both the real [`Store`] and a naive reference model. The model keeps,
//! per snapshot, the full logical image (one seed byte per page), its refcount,
//! its `owned_pages`/`chain_len`, and its parent link — everything needed to know
//! which transitions are valid and what every observation must yield.
//!
//! After every transition we assert that `read_page` for every gfn of every live
//! snapshot, the per-snapshot `stats`, the store-wide `store_stats`, and a
//! `materialize` round-trip all agree with the model, and that released snapshots
//! are uniformly unobservable. Indices into the model's `snaps` vector are stable
//! by construction (snapshots are only ever appended); the system-under-test keeps
//! a parallel vector of real `SnapshotId`s at the same indices.
//!
//! Ordered collections (`Vec`, `BTreeSet`) are used freely here: the determinism
//! rules constrain library code, not the test oracle.

use proptest::prelude::*;
use proptest::test_runner::Config;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig, StoreError};

/// Small logical image so per-snapshot full-image models stay cheap across long
/// operation sequences and many snapshots.
const MEM_PAGES: u64 = 16;

/// Materialize a page's content from its one-byte seed. The tiny content space
/// (256 values, the all-zero page at seed 0) keeps store-wide dedup and zero-page
/// handling constantly exercised.
fn page(seed: u8) -> [u8; PAGE_SIZE] {
    [seed; PAGE_SIZE]
}

/// One snapshot in the reference model. `seeds[gfn]` is the seed of the content at
/// that gfn, so the full logical image is recoverable without storing 4 KiB/page.
#[derive(Clone, Debug)]
struct RefSnap {
    seeds: Vec<u8>,
    vm_state: Vec<u8>,
    refcount: u64,
    owned_pages: u64,
    chain_len: u32,
    /// Index of the parent layer, or `None` for the base. Stable for the run.
    parent: Option<usize>,
    /// Whether the store still holds this layer. A layer is resident while it is
    /// live (refcount > 0) or retained as the ancestor of a live layer; `gc`
    /// drops the rest. Mirrors membership of `Store`'s internal layer map.
    resident: bool,
}

/// The reference state: snapshots in creation order, indexes stable forever.
#[derive(Clone, Debug)]
struct RefState {
    snaps: Vec<RefSnap>,
}

impl RefState {
    fn live_indices(&self) -> Vec<usize> {
        (0..self.snaps.len())
            .filter(|&i| self.snaps[i].refcount > 0)
            .collect()
    }

    /// Layers the store still holds (live or retained as an ancestor).
    fn resident_indices(&self) -> Vec<usize> {
        (0..self.snaps.len())
            .filter(|&i| self.snaps[i].resident)
            .collect()
    }

    /// Insert the distinct non-zero page CONTENTS that layer `i` *owns* — the
    /// pages it dirtied relative to its parent (vs the implicit zero image for
    /// the base). The all-zero page is implicit and never interned, so it is
    /// excluded. The store interns exactly the union of these over all resident
    /// layers (content-deduplicated store-wide).
    fn owned_nonzero_contents(&self, i: usize, out: &mut std::collections::BTreeSet<u8>) {
        let snap = &self.snaps[i];
        match snap.parent {
            None => out.extend(snap.seeds.iter().copied().filter(|&s| s != 0)),
            Some(p) => {
                let parent = &self.snaps[p].seeds;
                for (gfn, &seed) in snap.seeds.iter().enumerate() {
                    if seed != 0 && seed != parent[gfn] {
                        out.insert(seed);
                    }
                }
            }
        }
    }

    /// Exact `stored_unique_pages`: distinct non-zero contents interned across
    /// every resident layer's owned set.
    fn stored_unique_pages_exact(&self) -> u64 {
        let mut contents = std::collections::BTreeSet::new();
        for i in self.resident_indices() {
            self.owned_nonzero_contents(i, &mut contents);
        }
        contents.len() as u64
    }

    /// Exact `bytes_resident`: interned unique page payload plus the vm_state
    /// blob of every resident layer. (The store counts no bookkeeping overhead.)
    fn bytes_resident_exact(&self) -> u64 {
        let page_bytes = self.stored_unique_pages_exact() * PAGE_SIZE as u64;
        let vm_bytes: u64 = self
            .resident_indices()
            .iter()
            .map(|&i| self.snaps[i].vm_state.len() as u64)
            .sum();
        page_bytes + vm_bytes
    }

    /// Replicate `Store::gc`: a layer survives iff it is reachable upward (via
    /// `parent`) from some live layer; the rest stop being resident.
    fn gc(&mut self) {
        let mut reachable = std::collections::BTreeSet::new();
        for i in 0..self.snaps.len() {
            if !self.snaps[i].resident || self.snaps[i].refcount == 0 {
                continue;
            }
            let mut cur = Some(i);
            while let Some(c) = cur {
                if !reachable.insert(c) {
                    break; // already walked from here up
                }
                cur = self.snaps[c].parent;
            }
        }
        for i in 0..self.snaps.len() {
            if self.snaps[i].resident && !reachable.contains(&i) {
                self.snaps[i].resident = false;
            }
        }
    }
}

/// A write batch is a list of (gfn, seed) pairs; last write to a gfn wins, matching
/// builder semantics.
type Writes = Vec<(u64, u8)>;

#[derive(Clone, Debug)]
enum Transition {
    /// Derive a child of a live snapshot, apply a write batch, seal with `vm_state`.
    Derive {
        parent: usize,
        writes: Writes,
        vm_state: Vec<u8>,
    },
    Read {
        snap: usize,
        gfn: u64,
    },
    Materialize {
        snap: usize,
    },
    Retain {
        snap: usize,
    },
    Release {
        snap: usize,
    },
    Gc,
}

/// Apply a write batch to a seed image (last write wins) and return the resulting
/// seeds plus the count of pages that end up differing from `parent` (the
/// `owned_pages` a seal would record — writes equal to the inherited content are
/// dropped).
fn apply_writes(parent: &[u8], writes: &[(u64, u8)]) -> (Vec<u8>, u64) {
    let mut seeds = parent.to_vec();
    for &(gfn, seed) in writes {
        seeds[gfn as usize] = seed;
    }
    let owned = (0..seeds.len()).filter(|&i| seeds[i] != parent[i]).count() as u64;
    (seeds, owned)
}

/// Reference state machine driving generation; see module docs.
struct StoreRef;

impl ReferenceStateMachine for StoreRef {
    type State = RefState;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<RefState> {
        // The base layer: a random sparse set of seeded pages over the zero image.
        prop::collection::vec((0..MEM_PAGES, any::<u8>()), 0..12)
            .prop_map(|base_writes| {
                let zero = vec![0u8; MEM_PAGES as usize];
                let (seeds, owned) = apply_writes(&zero, &base_writes);
                RefState {
                    snaps: vec![RefSnap {
                        seeds,
                        vm_state: b"base".to_vec(),
                        refcount: 1,
                        owned_pages: owned,
                        chain_len: 1,
                        parent: None,
                        resident: true,
                    }],
                }
            })
            .boxed()
    }

    fn transitions(state: &RefState) -> BoxedStrategy<Transition> {
        let live = state.live_indices();
        // With nothing live, the only sensible thing is to garbage-collect.
        if live.is_empty() {
            return Just(Transition::Gc).boxed();
        }
        let writes = prop::collection::vec((0..MEM_PAGES, any::<u8>()), 0..6);
        let vm_state = prop::collection::vec(any::<u8>(), 0..12);
        let sel = || prop::sample::select(live.clone());
        prop_oneof![
            3 => (sel(), writes, vm_state).prop_map(|(parent, writes, vm_state)| {
                Transition::Derive { parent, writes, vm_state }
            }),
            4 => (sel(), 0..MEM_PAGES).prop_map(|(snap, gfn)| Transition::Read { snap, gfn }),
            1 => sel().prop_map(|snap| Transition::Materialize { snap }),
            1 => sel().prop_map(|snap| Transition::Retain { snap }),
            2 => sel().prop_map(|snap| Transition::Release { snap }),
            1 => Just(Transition::Gc),
        ]
        .boxed()
    }

    fn apply(mut state: RefState, transition: &Transition) -> RefState {
        match transition {
            Transition::Derive {
                parent,
                writes,
                vm_state,
            } => {
                let parent_snap = &state.snaps[*parent];
                let chain_len = parent_snap.chain_len.saturating_add(1);
                let (seeds, owned) = apply_writes(&parent_snap.seeds, writes);
                state.snaps.push(RefSnap {
                    seeds,
                    vm_state: vm_state.clone(),
                    refcount: 1,
                    owned_pages: owned,
                    chain_len,
                    parent: Some(*parent),
                    resident: true,
                });
            }
            Transition::Retain { snap } => state.snaps[*snap].refcount += 1,
            Transition::Release { snap } => state.snaps[*snap].refcount -= 1,
            Transition::Gc => state.gc(),
            Transition::Read { .. } | Transition::Materialize { .. } => {}
        }
        state
    }

    fn preconditions(state: &RefState, transition: &Transition) -> bool {
        let live = |i: usize| state.snaps.get(i).is_some_and(|s| s.refcount > 0);
        match transition {
            Transition::Gc => true,
            Transition::Derive { parent, .. } => live(*parent),
            Transition::Read { snap, gfn } => live(*snap) && *gfn < MEM_PAGES,
            Transition::Materialize { snap }
            | Transition::Retain { snap }
            | Transition::Release { snap } => live(*snap),
        }
    }
}

/// The system under test: the real store plus the index→id map mirroring the model.
struct StoreSut {
    store: Store,
    ids: Vec<SnapshotId>,
}

/// Read one page and assert it matches the model seed.
fn assert_page(store: &Store, id: SnapshotId, gfn: u64, seed: u8) {
    let mut out = [0xAAu8; PAGE_SIZE];
    store.read_page(id, gfn, &mut out).unwrap();
    assert_eq!(out, page(seed), "id {id:?} gfn {gfn} diverged from model");
}

struct StoreMachine;

impl StateMachineTest for StoreMachine {
    type SystemUnderTest = StoreSut;
    type Reference = StoreRef;

    fn init_test(ref_state: &RefState) -> StoreSut {
        let mut store = Store::new(StoreConfig {
            mem_pages: MEM_PAGES,
        });
        let base = &ref_state.snaps[0];
        let mut builder = store.begin_base();
        for (gfn, &seed) in base.seeds.iter().enumerate() {
            if seed != 0 {
                builder.write_page(gfn as u64, &page(seed)).unwrap();
            }
        }
        let id = builder.seal(base.vm_state.clone());
        StoreSut {
            store,
            ids: vec![id],
        }
    }

    fn apply(mut sut: StoreSut, ref_state: &RefState, transition: Transition) -> StoreSut {
        match transition {
            Transition::Derive {
                parent,
                writes,
                vm_state,
            } => {
                let mut builder = sut.store.derive(sut.ids[parent]).unwrap();
                for (gfn, seed) in writes {
                    builder.write_page(gfn, &page(seed)).unwrap();
                }
                let id = builder.seal(vm_state.clone());
                assert_eq!(sut.store.vm_state(id).unwrap(), &vm_state[..]);
                sut.ids.push(id);
            }
            Transition::Read { snap, gfn } => {
                assert_page(
                    &sut.store,
                    sut.ids[snap],
                    gfn,
                    ref_state.snaps[snap].seeds[gfn as usize],
                );
            }
            Transition::Materialize { snap } => {
                let model = &ref_state.snaps[snap];
                let mut mapping = sut.store.materialize(sut.ids[snap]).unwrap();
                assert_eq!(mapping.len(), MEM_PAGES as usize * PAGE_SIZE);
                for (gfn, &seed) in model.seeds.iter().enumerate() {
                    let off = gfn * PAGE_SIZE;
                    assert_eq!(
                        &mapping.as_slice()[off..off + PAGE_SIZE],
                        &page(seed)[..],
                        "materialize gfn {gfn} diverged"
                    );
                }
                // Copy-on-write probe: scribbling the mapping must not reach the store.
                if MEM_PAGES > 0 {
                    mapping.as_mut_slice()[..PAGE_SIZE].fill(0x5C);
                    assert_page(&sut.store, sut.ids[snap], 0, model.seeds[0]);
                }
            }
            Transition::Retain { snap } => sut.store.retain(sut.ids[snap]).unwrap(),
            Transition::Release { snap } => {
                sut.store.release(sut.ids[snap]).unwrap();
                // Reaching refcount 0 means immediately unobservable.
                if ref_state.snaps[snap].refcount == 0 {
                    let id = sut.ids[snap];
                    let mut out = [0u8; PAGE_SIZE];
                    assert!(matches!(
                        sut.store.read_page(id, 0, &mut out),
                        Err(StoreError::UnknownSnapshot(_)) | Err(StoreError::GfnOutOfRange { .. })
                    ));
                    assert!(matches!(
                        sut.store.stats(id),
                        Err(StoreError::UnknownSnapshot(_))
                    ));
                }
            }
            Transition::Gc => {
                sut.store.gc();
                // gc is idempotent: a second pass in a row frees nothing.
                assert_eq!(sut.store.gc(), 0, "second gc freed bytes");
            }
        }
        sut
    }

    fn check_invariants(sut: &StoreSut, ref_state: &RefState) {
        let live = ref_state.live_indices();

        // Store-wide statistics agree with the model exactly — not just as a
        // lower bound. `stored_unique_pages` and `bytes_resident` are computed
        // from the resident-layer set (live + retained ancestors, post-gc), so a
        // gc page-leak, an inflated unique-page count, or a wrong resident-byte
        // total all surface here.
        let stats = sut.store.store_stats();
        assert_eq!(stats.snapshots, live.len() as u64);
        assert_eq!(stats.logical_pages_total, live.len() as u64 * MEM_PAGES);
        assert_eq!(
            stats.stored_unique_pages,
            ref_state.stored_unique_pages_exact(),
            "stored_unique_pages diverged from the resident-layer model"
        );
        assert_eq!(
            stats.bytes_resident,
            ref_state.bytes_resident_exact(),
            "bytes_resident diverged from the resident-layer model"
        );

        for (i, model) in ref_state.snaps.iter().enumerate() {
            let id = sut.ids[i];
            if model.refcount == 0 {
                // Released snapshots are uniformly unknown.
                assert!(matches!(
                    sut.store.stats(id),
                    Err(StoreError::UnknownSnapshot(_))
                ));
                continue;
            }
            // Per-snapshot stats agree with the model.
            let s = sut.store.stats(id).unwrap();
            assert_eq!(s.logical_pages, MEM_PAGES);
            assert_eq!(
                s.owned_pages, model.owned_pages,
                "owned_pages for id {id:?}"
            );
            assert_eq!(s.chain_len, model.chain_len, "chain_len for id {id:?}");
            assert_eq!(sut.store.vm_state(id).unwrap(), &model.vm_state[..]);
            // Every page reads back its model content.
            for (gfn, &seed) in model.seeds.iter().enumerate() {
                assert_page(&sut.store, id, gfn as u64, seed);
            }
        }
    }
}

prop_state_machine! {
    #![proptest_config(Config { cases: 256, ..Config::default() })]

    /// Drive 1..40 operations against the store and the reference model.
    #[test]
    fn store_matches_model(sequential 1..40 => StoreMachine);
}
