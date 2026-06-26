// SPDX-License-Identifier: AGPL-3.0-or-later
//! Portable (Mac + Linux) integration test for the live snapshot/branch glue
//! (task 39): the `SnapshotEngine` (layered CoW store) wired to a `Vmm`'s memory
//! and `vm_state` adapter, driven against a scripted `MockBackend`. It exercises
//! the full path — `save_vm_state` + `snapshot_base`/`snapshot_derive` →
//! `materialize` → `restore_snapshot` → `reseed_entropy` — with no `/dev/kvm`.
//!
//! The box-only gates (bit-identical *execution* after restore, restore latency)
//! live in `tests/live_snapshot_branch.rs`; this test proves the wiring round-trips
//! the captured state and that N branches share one base.
//!
//! `#![cfg(not(miri))]`: every test here materializes a snapshot, which `mmap`s a
//! CoW view (`snapshot_store::Store::materialize`) — a syscall Miri cannot execute.
//! The pure parse/convert/store logic Miri *does* validate lives in the
//! `src/snapshot.rs` unit tests (device-blob byte parsing, the vCPU conversions).
#![cfg(not(miri))]

use vmm_backend::{Backend, Exit, MockBackend};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vmm::{GuestRam, Step, Vmm, VtimeWiring, contract_vclock_config};
use vmm_core::work::ScriptedWork;

const RAM: usize = 0x4000; // 16 KiB = 4 pages

/// A configured, V-time-wired `Vmm<MockBackend>` over `RAM` bytes of guest memory.
fn vmm(exits: Vec<Exit>, work_at: u64, seed: u64) -> Vmm<MockBackend> {
    let mut m = MockBackend::with_exits(exits);
    m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
    m.set_msr_filter(&vmm_backend::MsrFilter::default())
        .unwrap();
    let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
    v.wire_vtime(
        VtimeWiring::new(
            contract_vclock_config(),
            Box::new(ScriptedWork::at(work_at)),
            seed,
        )
        .unwrap(),
    );
    v
}

/// A distinctive guest-memory image: page 0 a banner, page 2 a marker, rest zero.
fn booted_image() -> Vec<u8> {
    let mut mem = vec![0u8; RAM];
    mem[..13].copy_from_slice(b"GUEST_BOOTED\n");
    mem[2 * 4096] = 0x5A;
    mem
}

#[test]
fn snapshot_then_restore_round_trips_a_running_vm() {
    // A's "boot": install a memory image, advance V-time to a clean (post-RDTSC)
    // boundary, and snapshot memory + vm_state into the engine.
    let mut a = vmm(vec![Exit::Rdtsc], 500, 0xABCD);
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → synchronized

    let mut eng = SnapshotEngine::new(RAM);
    let vm_state = a.save_vm_state().unwrap();
    let blob = vm_state.encode().unwrap();
    let snap = eng.snapshot_base(a.guest_memory(), &blob).unwrap();

    // The base interned only the non-zero pages (page 0 + page 2 = 2 owned).
    assert_eq!(eng.stats(snap).unwrap().owned_pages, 2);

    // Restore into a fresh, equivalently-wired VM B: memory + vm_state.
    let mut b = vmm(vec![], 9999, 0x0000);
    let mapping = eng.materialize(snap).unwrap();
    let decoded = eng.vm_state(snap).unwrap();
    b.restore_snapshot(mapping.as_slice(), &decoded).unwrap();

    // B is now indistinguishable from A at the snapshot point: same guest memory
    // and the same canonical vm_state blob.
    assert_eq!(b.guest_memory(), a.guest_memory());
    assert_eq!(b.save_vm_state().unwrap(), vm_state);

    // And B can run forward (terminal latch cleared by restore).
    let mut b2 = vmm(vec![Exit::Hlt], 9999, 0x0000);
    b2.restore_snapshot(eng.materialize(snap).unwrap().as_slice(), &decoded)
        .unwrap();
    assert!(matches!(b2.step().unwrap(), Step::Terminal(_)));
}

#[test]
fn snapshot_hashing_makes_restore_reproduce_the_state_hash() {
    // With the canonical-blob hash wired, a VM restored from a snapshot has the
    // SAME state_hash as the snapshot source at that point — *same state* observable
    // through the determinism hash (the Mac proxy for the box's *same future*).
    let mut a = vmm(vec![Exit::Rdtsc], 321, 0x77);
    a.wire_snapshot_hashing();
    a.restore_guest_memory(&booted_image()).unwrap();
    a.step().unwrap();
    let hash_a = a.state_hash();

    let mut eng = SnapshotEngine::new(RAM);
    let blob = a.save_vm_state().unwrap().encode().unwrap();
    let snap = eng.snapshot_base(a.guest_memory(), &blob).unwrap();

    let mut b = vmm(vec![], 1, 0x99);
    b.wire_snapshot_hashing();
    b.restore_snapshot(
        eng.materialize(snap).unwrap().as_slice(),
        &eng.vm_state(snap).unwrap(),
    )
    .unwrap();
    assert_eq!(
        b.state_hash(),
        hash_a,
        "a restored VM hashes identically to the snapshot source (same state)"
    );
}

#[test]
fn derive_captures_only_pages_dirtied_since_the_parent() {
    let mut a = vmm(vec![Exit::Rdtsc, Exit::Rdtsc], 100, 1);
    a.restore_guest_memory(&booted_image()).unwrap();
    a.step().unwrap();

    let mut eng = SnapshotEngine::new(RAM);
    let base = eng
        .snapshot_base(
            a.guest_memory(),
            &a.save_vm_state().unwrap().encode().unwrap(),
        )
        .unwrap();

    // Dirty exactly one page (page 1, previously zero), advance V-time, snapshot.
    let mut dirtied = booted_image();
    dirtied[4096..2 * 4096].fill(0xCC);
    a.restore_guest_memory(&dirtied).unwrap();
    a.step().unwrap();
    let child = eng
        .snapshot_derive(
            base,
            a.guest_memory(),
            Some(&[1]),
            &a.save_vm_state().unwrap().encode().unwrap(),
        )
        .unwrap();
    assert_eq!(
        eng.stats(child).unwrap().owned_pages,
        1,
        "the derived snapshot owns only the one dirtied page"
    );
    // The child's materialized image carries the dirtied page over the shared base.
    let m = eng.materialize(child).unwrap();
    assert_eq!(&m.as_slice()[..13], b"GUEST_BOOTED\n"); // inherited from base
    assert_eq!(m.as_slice()[4096], 0xCC); // the child's own page
}

#[test]
fn n_branches_share_one_boot_image_and_fork_entropy() {
    // Gate 3 + Phase 4: one booted base, N branches that share it store-wide, each
    // reseeded to a divergent entropy stream.
    let mut boot = vmm(vec![Exit::Rdtsc], 0, 0xBEEF);
    boot.restore_guest_memory(&booted_image()).unwrap();
    boot.step().unwrap();

    let mut eng = SnapshotEngine::new(RAM);
    let base = eng
        .snapshot_base(
            boot.guest_memory(),
            &boot.save_vm_state().unwrap().encode().unwrap(),
        )
        .unwrap();
    let unique_after_base = eng.store_stats().stored_unique_pages;
    assert_eq!(unique_after_base, 2, "base interned 2 non-zero pages");

    const N: usize = 6;
    let mut hashes = Vec::new();
    for i in 0..N {
        // Each branch: derive (touching nothing) + materialize + restore + reseed.
        let branch = eng
            .snapshot_derive(
                base,
                boot.guest_memory(),
                Some(&[]),
                &eng.vm_state(base).unwrap().encode().unwrap(),
            )
            .unwrap();
        let mut v = vmm(vec![], 0, 0);
        v.wire_snapshot_hashing(); // fold the entropy position into the hash
        v.restore_snapshot(
            eng.materialize(branch).unwrap().as_slice(),
            &eng.vm_state(branch).unwrap(),
        )
        .unwrap();
        v.reseed_entropy(0x1000 + i as u64).unwrap();
        // With snapshot-hashing wired, the reseeded entropy position is in the hash,
        // so a distinct branch seed ⇒ a distinct state_hash (a divergent future).
        hashes.push(v.state_hash());
    }

    // The base is physically shared: N branches that touched nothing add NO unique
    // pages store-wide (not N× the base).
    assert_eq!(
        eng.store_stats().stored_unique_pages,
        unique_after_base,
        "N branches share one read-only base (pages stored once store-wide)"
    );
    // Every branch forked to a distinct entropy stream (distinct state_hash).
    let mut sorted = hashes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), N, "each branch's reseed diverged");
}
