// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeSet;

use proptest::prelude::*;
use snapshot_store::{PAGE_SIZE, SnapshotId, Store, StoreConfig};

const PAGES: usize = 8;

struct Snapshot {
    id: SnapshotId,
    parent: Option<usize>,
    contents: [u8; PAGES],
    owned: BTreeSet<u64>,
}

fn ancestry(snapshots: &[Snapshot], start: usize) -> Vec<usize> {
    let mut path = Vec::new();
    let mut current = Some(start);
    while let Some(index) = current {
        path.push(index);
        current = snapshots[index].parent;
    }
    path
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn diff_matches_an_independent_forest_model(
        steps in prop::collection::vec(
            (any::<u8>(), 0u8..4, prop::collection::vec((0u64..PAGES as u64, 0u8..5), 0..8)),
            1..24,
        ),
        dirty in prop::collection::vec(0u64..PAGES as u64, 0..16),
    ) {
        let mut store = Store::new(StoreConfig { mem_pages: PAGES as u64 });
        let root = store.begin_base().seal(vec![]);
        let mut snapshots = vec![Snapshot {
            id: root,
            parent: None,
            contents: [0; PAGES],
            owned: BTreeSet::new(),
        }];
        for (selector, kind, writes) in steps {
            let parent = (kind != 0).then_some(selector as usize % snapshots.len());
            let inherited = parent.map_or([0; PAGES], |index| snapshots[index].contents);
            let mut contents = inherited;
            for &(gfn, value) in &writes {
                contents[gfn as usize] = value;
            }
            let owned = (0..PAGES)
                .filter(|&gfn| contents[gfn] != inherited[gfn])
                .map(|gfn| gfn as u64)
                .collect();
            let id = if let Some(index) = parent {
                let mut builder = store.derive(snapshots[index].id).unwrap();
                for &(gfn, value) in &writes {
                    builder.write_page(gfn, &[value; PAGE_SIZE]).unwrap();
                }
                builder.seal(vec![])
            } else {
                let mut builder = store.begin_base();
                for &(gfn, value) in &writes {
                    builder.write_page(gfn, &[value; PAGE_SIZE]).unwrap();
                }
                builder.seal(vec![])
            };
            snapshots.push(Snapshot { id, parent, contents, owned });
        }
        for (index, snapshot) in snapshots.iter().enumerate() {
            if index % 3 == 0 {
                store.release(snapshot.id).unwrap();
            }
        }
        store.gc();

        for from in (0..snapshots.len()).filter(|index| index % 3 != 0) {
            for to in (0..snapshots.len()).filter(|index| index % 3 != 0) {
                let left = ancestry(&snapshots, from);
                let right = ancestry(&snapshots, to);
                let common = left.iter().find(|index| right.contains(index)).copied();
                let mut touched = BTreeSet::new();
                for path in [&left, &right] {
                    for &index in path.iter().take_while(|&&index| Some(index) != common) {
                        touched.extend(snapshots[index].owned.iter().copied());
                    }
                }
                let expected: Vec<_> = touched.into_iter()
                    .map(|gfn| (gfn, [snapshots[to].contents[gfn as usize]; PAGE_SIZE]))
                    .collect();
                prop_assert_eq!(
                    store.diff_pages(Some(snapshots[from].id), snapshots[to].id).unwrap().into_iter().map(|(gfn, page)| (gfn, *page)).collect::<Vec<_>>(),
                    expected,
                    "from model node {} to {}", from, to,
                );
                let mut memory: Vec<_> = snapshots[from].contents.iter()
                    .map(|&value| [value; PAGE_SIZE]).collect();
                for &gfn in &dirty {
                    memory[gfn as usize].fill(0xFF);
                }
                let pages = store.restore_pages(Some(snapshots[from].id), snapshots[to].id, &dirty).unwrap();
                prop_assert!(pages.windows(2).all(|pair| pair[0].0 < pair[1].0));
                for (gfn, page) in pages {
                    memory[gfn as usize].copy_from_slice(page);
                }
                let expected: Vec<_> = snapshots[to].contents.iter()
                    .map(|&value| [value; PAGE_SIZE]).collect();
                prop_assert_eq!(memory, expected, "restored model node {} from {}", to, from);
            }
        }
    }
}
