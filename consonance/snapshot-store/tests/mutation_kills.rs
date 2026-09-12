// SPDX-License-Identifier: AGPL-3.0-or-later

use snapshot_store::{PAGE_SIZE, Store, StoreConfig};

fn store(mem_pages: u64) -> Store {
    Store::new(StoreConfig { mem_pages })
}

#[test]
fn seal_assigns_a_fresh_id_each_time() {
    let mut s = store(8);

    let a = s.begin_base().seal(vec![]);
    let b = s.begin_base().seal(vec![]);
    let c = s.begin_base().seal(vec![]);
    assert_ne!(a, b, "the second seal must advance next_id to a fresh id");
    assert_ne!(b, c, "the third seal must advance next_id again");
    assert_ne!(a, c, "no two seals share an id");

    let parent = s.begin_base().seal(vec![]);
    let child = s.derive(parent).unwrap().seal(vec![]);
    assert_ne!(parent, child, "a child's id differs from its parent's id");
}

#[test]
fn flatten_base_records_dirty_data_different_from_inherited_page() {
    let mut s = store(2);
    let inherited = [0x11u8; PAGE_SIZE];
    let replacement = [0x22u8; PAGE_SIZE];

    let mut base_builder = s.begin_base();
    base_builder.write_page(0, &inherited).unwrap();
    let base = base_builder.seal(vec![]);

    let mut memory = vec![0u8; 2 * PAGE_SIZE];
    memory[..PAGE_SIZE].copy_from_slice(&replacement);
    let flat = s.flatten_base(base, &memory, &[0], vec![]).unwrap();

    let mut out = [0u8; PAGE_SIZE];
    s.read_page(flat, 0, &mut out).unwrap();
    assert_eq!(
        out, replacement,
        "a dirty page whose bytes changed from the inherited page must be recorded"
    );
}
