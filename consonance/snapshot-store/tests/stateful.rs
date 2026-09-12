// SPDX-License-Identifier: AGPL-3.0-or-later

use proptest::prelude::*;
use proptest::test_runner::Config;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig, StoreError};

const MEM_PAGES: u64 = 16;

fn page(seed: u8) -> [u8; PAGE_SIZE] {
    [seed; PAGE_SIZE]
}

#[derive(Clone, Debug)]
struct RefSnap {
    seeds: Vec<u8>,
    vm_state: Vec<u8>,
    refcount: u64,
    owned_pages: u64,
    chain_len: u32,
    parent: Option<usize>,
    resident: bool,
}

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

    fn resident_indices(&self) -> Vec<usize> {
        (0..self.snaps.len())
            .filter(|&i| self.snaps[i].resident)
            .collect()
    }

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

    fn stored_unique_pages_exact(&self) -> u64 {
        let mut contents = std::collections::BTreeSet::new();
        for i in self.resident_indices() {
            self.owned_nonzero_contents(i, &mut contents);
        }
        contents.len() as u64
    }

    fn bytes_resident_exact(&self) -> u64 {
        let page_bytes = self.stored_unique_pages_exact() * PAGE_SIZE as u64;
        let vm_bytes: u64 = self
            .resident_indices()
            .iter()
            .map(|&i| self.snaps[i].vm_state.len() as u64)
            .sum();
        page_bytes + vm_bytes
    }

    fn gc(&mut self) {
        let mut reachable = std::collections::BTreeSet::new();
        for i in 0..self.snaps.len() {
            if !self.snaps[i].resident || self.snaps[i].refcount == 0 {
                continue;
            }
            let mut cur = Some(i);
            while let Some(c) = cur {
                if !reachable.insert(c) {
                    break;
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

type Writes = Vec<(u64, u8)>;

#[derive(Clone, Debug)]
enum Transition {
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

fn apply_writes(parent: &[u8], writes: &[(u64, u8)]) -> (Vec<u8>, u64) {
    let mut seeds = parent.to_vec();
    for &(gfn, seed) in writes {
        seeds[gfn as usize] = seed;
    }
    let owned = (0..seeds.len()).filter(|&i| seeds[i] != parent[i]).count() as u64;
    (seeds, owned)
}

struct StoreRef;

impl ReferenceStateMachine for StoreRef {
    type State = RefState;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<RefState> {
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

struct StoreSut {
    store: Store,
    ids: Vec<SnapshotId>,
}

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
                if MEM_PAGES > 0 {
                    mapping.as_mut_slice()[..PAGE_SIZE].fill(0x5C);
                    assert_page(&sut.store, sut.ids[snap], 0, model.seeds[0]);
                }
            }
            Transition::Retain { snap } => sut.store.retain(sut.ids[snap]).unwrap(),
            Transition::Release { snap } => {
                sut.store.release(sut.ids[snap]).unwrap();
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
                assert_eq!(sut.store.gc(), 0, "second gc freed bytes");
            }
        }
        sut
    }

    fn check_invariants(sut: &StoreSut, ref_state: &RefState) {
        let live = ref_state.live_indices();

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
                assert!(matches!(
                    sut.store.stats(id),
                    Err(StoreError::UnknownSnapshot(_))
                ));
                continue;
            }
            let s = sut.store.stats(id).unwrap();
            assert_eq!(s.logical_pages, MEM_PAGES);
            assert_eq!(
                s.owned_pages, model.owned_pages,
                "owned_pages for id {id:?}"
            );
            assert_eq!(s.chain_len, model.chain_len, "chain_len for id {id:?}");
            assert_eq!(sut.store.vm_state(id).unwrap(), &model.vm_state[..]);
            for (gfn, &seed) in model.seeds.iter().enumerate() {
                assert_page(&sut.store, id, gfn as u64, seed);
            }
        }
    }
}

prop_state_machine! {
    #![proptest_config(Config { cases: 256, ..Config::default() })]

    #[test]
    fn store_matches_model(sequential 1..40 => StoreMachine);
}
