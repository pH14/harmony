// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 35 — kill the `lib.rs:521` `BuilderCore::seal` mutant
//! (`self.store.next_id += 1` → `*= 1`) by assertion rather than by hang.
//!
//! With `*= 1` the id counter freezes at 0, so every `seal` hands out id 0. That
//! makes a derived child reuse its parent's id, leaving a self-parented layer
//! whose chain walk (`resolve`/`materialize`/`gc`) never terminates — which is
//! why the surviving mutant showed up only as a ~372 s *timeout*. The test below
//! seals several snapshots and asserts their ids are **distinct**, a check that
//! fails fast on the first comparison and performs **no** chain walk, so a frozen
//! counter is caught deterministically by an assertion, not by the hang.

use snapshot_store::{Store, StoreConfig};

fn store(mem_pages: u64) -> Store {
    Store::new(StoreConfig { mem_pages })
}

#[test]
fn seal_assigns_a_fresh_id_each_time() {
    let mut s = store(8);

    // Three independent base layers (parent = None, so no chain to walk). A
    // correct `+= 1` yields ids 0, 1, 2 — all distinct. A `*= 1` freezes the
    // counter, so all three collide on id 0 and the first `assert_ne!` fires.
    let a = s.begin_base().seal(vec![]);
    let b = s.begin_base().seal(vec![]);
    let c = s.begin_base().seal(vec![]);
    assert_ne!(a, b, "the second seal must advance next_id to a fresh id");
    assert_ne!(b, c, "the third seal must advance next_id again");
    assert_ne!(a, c, "no two seals share an id");

    // A derived child must also get an id distinct from its parent — otherwise the
    // child's `parent` link points at the child itself (the resolve-loop source).
    // The child writes no pages, so `seal` performs no parent resolution here:
    // the distinctness is observed without ever walking a chain (no hang).
    let parent = s.begin_base().seal(vec![]);
    let child = s.derive(parent).unwrap().seal(vec![]);
    assert_ne!(parent, child, "a child's id differs from its parent's id");
}
