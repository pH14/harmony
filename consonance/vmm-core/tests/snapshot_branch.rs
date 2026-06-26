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

use vmm_backend::{Backend, Exit, MockBackend, VcpuEvents, VcpuState};
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
fn non_quiescent_in_flight_events_round_trip_through_the_engine() {
    // Task 41 (the substrate unlock): a snapshot taken at a **non-quiescent** point —
    // an interrupt + exception **in flight** in `kvm_vcpu_events` — survives the WHOLE
    // engine path (vm_state encode → snapshot_base → materialize → restore_snapshot),
    // and the restored VM's backend carries the exact in-flight events. Task 39 would
    // have fail-closed-rejected the save here (0/8392 snapshottable on the live guest).
    let in_flight = VcpuEvents {
        interrupt_injected: 1,
        interrupt_nr: 0x34,
        exception_injected: 1,
        exception_nr: 14,
        exception_has_payload: 1,
        exception_payload: 0xDEAD_BEEF_F00D,
        nmi_masked: 1,
        ..Default::default()
    };
    // A's backend reports an in-flight vCPU; advance V-time to a synchronized boundary.
    let mut m = MockBackend::with_exits(vec![Exit::Rdtsc]);
    m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
    m.set_msr_filter(&vmm_backend::MsrFilter::default())
        .unwrap();
    m.set_state(VcpuState {
        events: in_flight,
        ..Default::default()
    });
    let mut a = Vmm::new(m, GuestRam::new(RAM).unwrap());
    a.wire_vtime(
        VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 7).unwrap(),
    );
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → synchronized

    // Save succeeds AT the non-quiescent point and the engine seals it.
    let mut eng = SnapshotEngine::new(RAM);
    let vm_state = a
        .save_vm_state()
        .expect("a non-quiescent (interrupt-in-flight) point is now snapshottable");
    let snap = eng
        .snapshot_base(a.guest_memory(), &vm_state.encode().unwrap())
        .unwrap();

    // Restore into a fresh VM and confirm the in-flight events reached its backend
    // (the KVM_SET_VCPU_EVENTS-equivalent), bit for bit.
    let mut b = vmm(vec![], 9999, 0);
    b.restore_snapshot(
        eng.materialize(snap).unwrap().as_slice(),
        &eng.vm_state(snap).unwrap(),
    )
    .unwrap();
    assert_eq!(
        b.save_vm_state().unwrap(),
        vm_state,
        "the full vm_state (incl. the in-flight events) round-trips through the engine"
    );
}

#[test]
fn task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact() {
    // THE definitive task-41 unlock proof — independent of any live run. Build a GENUINE
    // in-flight `kvm_vcpu_events` state that task 39 fail-closed-REJECTED and could not
    // represent (an injected #GP-with-error-code + an injected NMI — `has_inflight` AND
    // `has_active`, NOT an inert residual), snapshot it through the FULL engine path with
    // the canonical-blob hash wired, restore into a fresh VM, and prove the restored FULL
    // `state_hash` equals the source's. Task 39 dropped this state (0/N snapshottable);
    // task 41 captures every field and round-trips it bit-for-bit — proven here WITHOUT
    // relying on the live workload ever presenting such a point at a synchronized boundary
    // (it reliably does not; see `live_nonquiescent_snapshot.rs` + IMPLEMENTATION.md).
    let in_flight = VcpuEvents {
        exception_injected: 1,
        exception_nr: 13, // #GP
        exception_has_error_code: 1,
        exception_error_code: 0x18,
        nmi_injected: 1,
        nmi_masked: 1,
        ..Default::default()
    };
    // Source VM `a`: the in-flight events, V-time wired, canonical-blob hash wired.
    let mut a = {
        let mut m = MockBackend::with_exits(vec![Exit::Rdtsc]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        m.set_state(VcpuState {
            events: in_flight,
            ..Default::default()
        });
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 7).unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&booted_image()).unwrap();
        v
    };
    assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → synchronized

    // It is exactly the state task 39's predicate rejected, and it is GENUINE (not a residual).
    assert!(
        a.has_inflight_event_injection(),
        "task 39's predicate fail-closed-rejected this point"
    );
    assert!(
        a.has_active_event_injection(),
        "a genuine in-flight injection, not an inert residual"
    );

    // The in-flight event REACHES the hash: a VM carrying it hashes differently from an
    // otherwise-identical quiescent one — so 'identical hash on restore' is a meaningful
    // claim, not a no-op on an all-zero events record.
    let mut q = vmm(vec![Exit::Rdtsc], 500, 7);
    q.wire_snapshot_hashing();
    q.restore_guest_memory(&booted_image()).unwrap();
    q.step().unwrap();
    assert_ne!(
        a.state_hash(),
        q.state_hash(),
        "the in-flight kvm_vcpu_events state is reflected in the full state_hash"
    );

    // Snapshot through the FULL engine path; restore into a FRESH VM.
    let mut eng = SnapshotEngine::new(RAM);
    let vm_state = a
        .save_vm_state()
        .expect("a genuine in-flight (task-39-rejected) point is now snapshottable");
    let snap = eng
        .snapshot_base(a.guest_memory(), &vm_state.encode().unwrap())
        .unwrap();
    let mut b = vmm(vec![], 9999, 0);
    b.wire_snapshot_hashing();
    b.restore_snapshot(
        eng.materialize(snap).unwrap().as_slice(),
        &eng.vm_state(snap).unwrap(),
    )
    .unwrap();

    // EXACT restore: the restored VM's FULL state_hash equals the source's at the in-flight
    // point — capture → restore of the genuine in-flight kvm_vcpu_events is bit-for-bit.
    assert_eq!(
        b.state_hash(),
        a.state_hash(),
        "restored full state_hash == source at the genuine in-flight point"
    );
    // And the full kvm_vcpu_events round-tripped through save → restore → save.
    assert_eq!(
        b.save_vm_state().unwrap(),
        vm_state,
        "the full in-flight events round-trip through the engine"
    );
}

#[test]
fn vmst_chunk_masks_an_unusable_segments_type() {
    // PR #12 round 4 — the determinism gap. With `wire_snapshot_hashing()` ON, the **VMST**
    // chunk (the typed `vm_state` record, packed by `pack_segment`) is folded into
    // `state_hash`. `pack_segment` must canonicalize an unusable segment's `type` to 0 —
    // mirroring `encode_segment` in the VCPU chunk — because KVM normalizes an unusable
    // segment's don't-care `type` `0 → 1` across `KVM_SET_SREGS → KVM_GET_SREGS`, so without
    // masking, a `save → restore → save` would perturb the VMST chunk and `state_hash` would
    // diverge **even though the VCPU chunk is canonical**. This catches exactly that: two VMs
    // differing ONLY in an unusable segment's raw `type` must hash identically through BOTH
    // chunks. (The real-KVM `save → restore → save` round-trip is gate 2 on the box.)
    let hash_of = |unusable_type: u8| -> [u8; 32] {
        let mut m = MockBackend::with_exits(vec![Exit::Rdtsc]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut st = VcpuState::default();
        // An UNUSABLE data segment carrying a (raw) non-zero hidden type — exactly the field
        // KVM perturbs across a SET→GET round-trip.
        st.sregs.ds = vmm_backend::Segment {
            type_: unusable_type,
            unusable: 1,
            ..Default::default()
        };
        m.set_state(st);
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 7).unwrap(),
        );
        v.wire_snapshot_hashing(); // fold the VMST chunk into state_hash
        v.restore_guest_memory(&booted_image()).unwrap();
        v.step().unwrap(); // RDTSC → synchronized
        v.state_hash()
    };
    assert_eq!(
        hash_of(0),
        hash_of(5),
        "an unusable segment's type must not move the state_hash through the VMST (or VCPU) chunk"
    );
    // Sanity: a *usable* segment's type DOES move the hash (so the assert above is a real
    // masking property, not a hash that ignores segment type wholesale).
    let usable_hash = |t: u8| -> [u8; 32] {
        let mut m = MockBackend::with_exits(vec![Exit::Rdtsc]);
        m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
        m.set_msr_filter(&vmm_backend::MsrFilter::default())
            .unwrap();
        let mut st = VcpuState::default();
        st.sregs.ds = vmm_backend::Segment {
            type_: t,
            unusable: 0,
            present: 1,
            ..Default::default()
        };
        m.set_state(st);
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(
            VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 7).unwrap(),
        );
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&booted_image()).unwrap();
        v.step().unwrap();
        v.state_hash()
    };
    assert_ne!(
        usable_hash(0),
        usable_hash(5),
        "a usable segment's type reaches the hash (the unusable-mask is not masking everything)"
    );
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
fn snapshot_hashing_round_trips_at_a_residual_events_point() {
    // P2 (PR #12): with `wire_snapshot_hashing()` ON, a snapshot taken where the live
    // `kvm_vcpu_events` carries INERT modifier residuals (a stale exception vector /
    // error-code / `interrupt.nr` with neither `injected` nor `pending`, plus the GET-only
    // validity `flags`) must round-trip save→restore→`state_hash` **bit-identically**. The
    // restore re-establishes the *canonical* events, so unless BOTH the device blob AND the
    // typed `VmState.events` record (which rides the VMST hash chunk) are canonicalized, the
    // restored `state_hash` would diverge from the source at a residual point — the
    // "clean full-hash match" would hold only where a test misses residuals.
    let residual = VcpuEvents {
        exception_nr: 13,
        exception_error_code: 0xABCD,
        exception_has_error_code: 1,
        interrupt_nr: 0x34,
        flags: 0x0D,
        ..Default::default() // injected / pending all 0 ⇒ inert
    };
    let mut m = MockBackend::with_exits(vec![Exit::Rdtsc]);
    m.set_cpuid(&vmm_backend::CpuidModel::default()).unwrap();
    m.set_msr_filter(&vmm_backend::MsrFilter::default())
        .unwrap();
    m.set_state(VcpuState {
        events: residual,
        ..Default::default()
    });
    let mut a = Vmm::new(m, GuestRam::new(RAM).unwrap());
    a.wire_vtime(
        VtimeWiring::new(contract_vclock_config(), Box::new(ScriptedWork::at(500)), 7).unwrap(),
    );
    a.wire_snapshot_hashing();
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued); // RDTSC → synchronized
    let hash_a = a.state_hash();

    let mut eng = SnapshotEngine::new(RAM);
    let snap = eng
        .snapshot_base(
            a.guest_memory(),
            &a.save_vm_state().unwrap().encode().unwrap(),
        )
        .unwrap();

    let mut b = vmm(vec![], 9999, 0);
    b.wire_snapshot_hashing();
    b.restore_snapshot(
        eng.materialize(snap).unwrap().as_slice(),
        &eng.vm_state(snap).unwrap(),
    )
    .unwrap();
    assert_eq!(
        b.state_hash(),
        hash_a,
        "a restored VM hashes identically to a residual-events snapshot source — both the \
         device blob AND the typed VmState.events record are canonicalized, so the VMST hash \
         chunk carries no raw residual"
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
