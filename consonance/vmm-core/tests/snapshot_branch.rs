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

use vm_state::VmState;
use vmm_backend::{
    Backend, CommonExit, Exit, MockBackend, VcpuEvents, VcpuState, X86, X86Exit, X86Policy,
};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vendor::x86::contract_vclock_config;
use vmm_core::vmm::{GuestRam, Step, Vmm, VtimeWiring};

const RAM: usize = 0x4000;

/// A configured, V-time-wired `Vmm<MockBackend>` over `RAM` bytes of guest memory.
fn vmm(exits: Vec<Exit<X86>>, _work_at: u64, seed: u64) -> Vmm<MockBackend> {
    let mut m = MockBackend::with_exits(exits);
    m.set_policy(&X86Policy {
        cpuid: vmm_backend::CpuidModel::default(),
        msr_filter: vmm_backend::MsrFilter::default(),
    })
    .unwrap();
    let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
    v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), seed).unwrap());
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
    let mut a = vmm(
        vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })],
        500,
        0xABCD,
    );
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued);

    let mut eng = SnapshotEngine::new(RAM);
    let vm_state = a.save_vm_state().unwrap();
    let blob = vm_state.encode().unwrap();
    let snap = eng.snapshot_base(a.guest_memory(), &blob).unwrap();

    assert_eq!(eng.stats(snap).unwrap().owned_pages, 2);

    let mut b = vmm(vec![], 9999, 0x0000);
    let mapping = eng.materialize(snap).unwrap();
    let decoded = eng.vm_state(snap).unwrap();
    b.restore_snapshot(mapping.as_slice(), &decoded).unwrap();

    assert_eq!(b.guest_memory(), a.guest_memory());
    assert_eq!(b.save_vm_state().unwrap(), vm_state);

    let mut b2 = vmm(vec![Exit::Common(CommonExit::Idle)], 9999, 0x0000);
    b2.restore_snapshot(eng.materialize(snap).unwrap().as_slice(), &decoded)
        .unwrap();
    assert!(matches!(b2.step().unwrap(), Step::Terminal(_)));
}

#[test]
fn non_quiescent_in_flight_events_round_trip_through_the_engine() {
    let in_flight = VcpuEvents {
        interrupt_injected: 1,
        interrupt_nr: 0x34,
        exception_injected: 1,
        exception_nr: 14,
        exception_has_error_code: 1,
        exception_error_code: 0xF00D,
        nmi_masked: 1,
        ..Default::default()
    };
    let mut m = MockBackend::with_exits(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
    m.set_policy(&X86Policy {
        cpuid: vmm_backend::CpuidModel::default(),
        msr_filter: vmm_backend::MsrFilter::default(),
    })
    .unwrap();
    m.set_state(VcpuState {
        events: in_flight,
        ..Default::default()
    });
    let mut a = Vmm::new(m, GuestRam::new(RAM).unwrap());
    a.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued);

    let mut eng = SnapshotEngine::new(RAM);
    let vm_state = a
        .save_vm_state()
        .expect("a non-quiescent (interrupt-in-flight) point is now snapshottable");
    let snap = eng
        .snapshot_base(a.guest_memory(), &vm_state.encode().unwrap())
        .unwrap();

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
fn rejected_in_flight_kvm_events_restore_is_state_hash_exact() {
    let in_flight = VcpuEvents {
        exception_injected: 1,
        exception_nr: 13,
        exception_has_error_code: 1,
        exception_error_code: 0x18,
        nmi_injected: 1,
        nmi_masked: 1,
        ..Default::default()
    };
    let mut a = {
        let mut m = MockBackend::with_exits(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        m.set_state(VcpuState {
            events: in_flight,
            ..Default::default()
        });
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&booted_image()).unwrap();
        v
    };
    assert_eq!(a.step().unwrap(), Step::Continued);

    assert!(
        a.has_inflight_event_injection(),
        "task 39's predicate fail-closed-rejected this point"
    );
    assert!(
        a.has_active_event_injection(),
        "a genuine in-flight injection, not an inert residual"
    );

    let mut q = vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 500, 7);
    q.wire_snapshot_hashing();
    q.restore_guest_memory(&booted_image()).unwrap();
    q.step().unwrap();
    assert_ne!(
        a.state_hash().unwrap(),
        q.state_hash().unwrap(),
        "the in-flight kvm_vcpu_events state is reflected in the full state_hash"
    );

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

    assert_eq!(
        b.state_hash().unwrap(),
        a.state_hash().unwrap(),
        "restored full state_hash == source at the genuine in-flight point"
    );
    assert_eq!(
        b.save_vm_state().unwrap(),
        vm_state,
        "the full in-flight events round-trip through the engine"
    );
}

#[test]
fn vmst_chunk_masks_an_unusable_segments_type() {
    let hash_of = |unusable_type: u8| -> [u8; 32] {
        let mut m = MockBackend::with_exits(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
        .unwrap();
        let mut st = VcpuState::default();
        st.sregs.ds = vmm_backend::Segment {
            type_: unusable_type,
            unusable: 1,
            ..Default::default()
        };
        m.set_state(st);
        let mut v = Vmm::new(m, GuestRam::new(RAM).unwrap());
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&booted_image()).unwrap();
        v.step().unwrap();
        v.state_hash().unwrap()
    };
    assert_eq!(
        hash_of(0),
        hash_of(5),
        "an unusable segment's type must not move the state_hash through the VMST (or VCPU) chunk"
    );
    let usable_hash = |t: u8| -> [u8; 32] {
        let mut m = MockBackend::with_exits(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
        m.set_policy(&X86Policy {
            cpuid: vmm_backend::CpuidModel::default(),
            msr_filter: vmm_backend::MsrFilter::default(),
        })
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
        v.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
        v.wire_snapshot_hashing();
        v.restore_guest_memory(&booted_image()).unwrap();
        v.step().unwrap();
        v.state_hash().unwrap()
    };
    assert_ne!(
        usable_hash(0),
        usable_hash(5),
        "a usable segment's type reaches the hash (the unusable-mask is not masking everything)"
    );
}

#[test]
fn snapshot_hashing_makes_restore_reproduce_the_state_hash() {
    let mut a = vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 321, 0x77);
    a.wire_snapshot_hashing();
    a.restore_guest_memory(&booted_image()).unwrap();
    a.step().unwrap();
    let hash_a = a.state_hash().unwrap();

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
        b.state_hash().unwrap(),
        hash_a,
        "a restored VM hashes identically to the snapshot source (same state)"
    );
}

#[test]
fn snapshot_hashing_round_trips_at_a_residual_events_point() {
    let residual = VcpuEvents {
        exception_nr: 13,
        exception_error_code: 0xABCD,
        exception_has_error_code: 1,
        interrupt_nr: 0x34,
        flags: 0x0D,
        ..Default::default()
    };
    let mut m = MockBackend::with_exits(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })]);
    m.set_policy(&X86Policy {
        cpuid: vmm_backend::CpuidModel::default(),
        msr_filter: vmm_backend::MsrFilter::default(),
    })
    .unwrap();
    m.set_state(VcpuState {
        events: residual,
        ..Default::default()
    });
    let mut a = Vmm::new(m, GuestRam::new(RAM).unwrap());
    a.wire_vtime(VtimeWiring::new_virtual_time(contract_vclock_config(), 7).unwrap());
    a.wire_snapshot_hashing();
    a.restore_guest_memory(&booted_image()).unwrap();
    assert_eq!(a.step().unwrap(), Step::Continued);
    let hash_a = a.state_hash().unwrap();

    let mut eng = SnapshotEngine::new(RAM);
    let blob = a.save_vm_state().unwrap().encode().unwrap();
    let snap = eng.snapshot_base(a.guest_memory(), &blob).unwrap();

    let mut b = vmm(vec![], 9999, 0);
    b.wire_snapshot_hashing();
    b.restore_snapshot(
        eng.materialize(snap).unwrap().as_slice(),
        &eng.vm_state(snap).unwrap(),
    )
    .unwrap();
    assert_eq!(
        b.state_hash().unwrap(),
        hash_a,
        "a restored VM hashes identically to a residual-events snapshot source — both the \
         device blob AND the typed VmState.events record are canonicalized, so the VMST hash \
         chunk carries no raw residual"
    );
}

#[test]
fn derive_captures_only_pages_dirtied_since_the_parent() {
    let mut a = vmm(
        vec![
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
            Exit::Arch(X86Exit::Rdmsr { index: 0x10 }),
        ],
        100,
        1,
    );
    a.restore_guest_memory(&booted_image()).unwrap();
    a.step().unwrap();

    let mut eng = SnapshotEngine::new(RAM);
    let blob = a.save_vm_state().unwrap().encode().unwrap();
    let base = eng.snapshot_base(a.guest_memory(), &blob).unwrap();

    let mut dirtied = booted_image();
    dirtied[4096..2 * 4096].fill(0xCC);
    a.restore_guest_memory(&dirtied).unwrap();
    a.step().unwrap();
    let blob2 = a.save_vm_state().unwrap().encode().unwrap();
    let child = eng
        .snapshot_derive(base, a.guest_memory(), Some(&[1]), &blob2)
        .unwrap();
    assert_eq!(
        eng.stats(child).unwrap().owned_pages,
        1,
        "the derived snapshot owns only the one dirtied page"
    );
    let m = eng.materialize(child).unwrap();
    assert_eq!(&m.as_slice()[..13], b"GUEST_BOOTED\n");
    assert_eq!(m.as_slice()[4096], 0xCC);
}

#[test]
fn n_branches_share_one_boot_image_and_fork_entropy() {
    let mut boot = vmm(vec![Exit::Arch(X86Exit::Rdmsr { index: 0x10 })], 0, 0xBEEF);
    boot.restore_guest_memory(&booted_image()).unwrap();
    boot.step().unwrap();

    let mut eng = SnapshotEngine::new(RAM);
    let boot_blob = boot.save_vm_state().unwrap().encode().unwrap();
    let base = eng.snapshot_base(boot.guest_memory(), &boot_blob).unwrap();
    let unique_after_base = eng.store_stats().stored_unique_pages;
    assert_eq!(unique_after_base, 2, "base interned 2 non-zero pages");

    const N: usize = 6;
    let mut hashes = Vec::new();
    for i in 0..N {
        let branch = eng
            .snapshot_derive(
                base,
                boot.guest_memory(),
                Some(&[]),
                &eng.vm_state::<VmState>(base).unwrap().encode().unwrap(),
            )
            .unwrap();
        let mut v = vmm(vec![], 0, 0);
        v.wire_snapshot_hashing();
        v.restore_snapshot(
            eng.materialize(branch).unwrap().as_slice(),
            &eng.vm_state(branch).unwrap(),
        )
        .unwrap();
        v.reseed_entropy(0x1000 + i as u64).unwrap();
        hashes.push(v.state_hash().unwrap());
    }

    assert_eq!(
        eng.store_stats().stored_unique_pages,
        unique_after_base,
        "N branches share one read-only base (pages stored once store-wide)"
    );
    let mut sorted = hashes.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), N, "each branch's reseed diverged");
}
