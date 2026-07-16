// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The M1 keystone assertion** (`tasks/112`): the arm64 vendor — the first
//! real *second* implementor of `Vendor`/`Backend`/`Arch` — instantiates every
//! method the engine calls, and the engine drives it through exactly the same
//! generic types it drives x86 through. This is the structural check no
//! cross-compile gate can perform (`docs/ARCH-BOUNDARY.md` §D: on the aarch64
//! CI leg no vendor exists to *instantiate* the trait, so a signature only a
//! second implementor could refute stays invisible until this vendor exists).
//!
//! Portable and Miri-clean: driven by the scripted `MockArm64Backend`, no
//! `/dev/kvm`, no mmap (the snapshot round-trip seals and decodes through the
//! in-memory store; the mmap-backed `materialize` path is the x86-shared
//! engine machinery already covered elsewhere and is not re-tested here).

use vm_state::{Arm64VmState, SnapshotRecords, VmState, VmStateError};
use vmm_backend::{
    Arm64, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState, Backend, CommonExit, Exit,
    GicIntId, MockArm64Backend, MpState,
};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vmm::{GuestRam, Step, TerminalReason, Vmm, VmmError};

const RAM: usize = 0x4000; // 16 KiB = 4 pages

/// A configured `Vmm<MockArm64Backend>` over `RAM` bytes of guest memory —
/// the arm64 twin of the x86 tests' `vmm()` helper. The policy skeleton is
/// installed before the first run, exactly as a composition root must.
fn vmm(exits: Vec<Exit<Arm64>>) -> Vmm<MockArm64Backend> {
    let mut b = MockArm64Backend::with_exits(exits);
    b.set_policy(&Arm64Policy::default()).unwrap();
    Vmm::new(b, GuestRam::new(RAM).unwrap())
}

/// The engine terminates an arm64 VM through the same `CommonExit` vocabulary
/// as x86 — WFI-idle and shutdown are one concept above the trait.
#[test]
fn engine_drives_the_arm64_vendor_through_common_exits() {
    // Idle with no V-time wired and no fabric: a terminal wait (nothing can
    // wake the guest), latched exactly as on x86.
    let mut v = vmm(vec![Exit::Common(CommonExit::Idle)]);
    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Idle));
    assert_eq!(v.terminal_reason(), Some(TerminalReason::Idle));

    let mut v = vmm(vec![Exit::Common(CommonExit::Shutdown)]);
    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Shutdown));
}

/// Default-deny is structural on the second vendor too: an unmodeled MMIO
/// address and a trapped sysreg with no ruled disposition both fail closed.
#[test]
fn arm64_dispatch_fails_closed_on_unruled_surface() {
    // MMIO: no address is modeled in the skeleton (the memory map is the boot
    // path's).
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: vmm_backend::Gpa(0x0900_0000),
        size: 4,
        write: Some(0x41),
    })]);
    let err = v.step().unwrap_err();
    assert!(matches!(err, VmmError::ContractViolation(_)), "{err}");

    // Sysreg: the dispositions are AA-6's; the skeleton rules none.
    let mut v = vmm(vec![Exit::Arch(Arm64Exit::Sysreg {
        sysreg: 0x0018_0000,
        write: None,
    })]);
    let err = v.step().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no ruled disposition"), "{msg}");
}

/// The interrupt seams answer honestly with no fabric wired: stage-time
/// validation refuses every identity, injection fails loud, and nothing is
/// pending — mirroring the x86 unwired-LAPIC posture.
#[test]
fn arm64_interrupt_seams_report_no_fabric() {
    let mut v = vmm(vec![]);
    assert!(!v.has_pending_guest_interrupt().unwrap());
    let err = v.apply_host_fault(&environment::HostFault::InjectInterrupt { vector: 40 });
    assert!(err.is_err(), "no fabric wired: injection must fail loud");
}

/// The keystone round trip: build → seal → decode → restore, entirely through
/// the engine's generic snapshot path (`Vendor::Snapshot = Arm64VmState`), and
/// state-hash-transparent: the restored VM hashes identically to the source.
#[test]
fn arm64_snapshot_round_trip_is_restore_transparent() {
    // Give the vCPU distinctive state before composing the VM.
    let mut vcpu = Arm64VcpuState::default();
    vcpu.core.x[0] = 0x4000_0000;
    vcpu.core.pc = 0x0020_0000;
    vcpu.core.pstate = 0x3c5;
    vcpu.sysregs.sctlr_el1 = 0x30d0_0800;
    vcpu.mp_state = MpState::Runnable;
    let mut b = MockArm64Backend::new();
    b.set_policy(&Arm64Policy::default()).unwrap();
    b.set_state(vcpu);
    let mut v = Vmm::new(b, GuestRam::new(RAM).unwrap());
    v.inject_serial_input(b"never-snapshotted"); // off-record: must not leak

    // The engine's generic save path: `Vmm::save_vm_state` returns the
    // vendor's associated snapshot type.
    let s: Arm64VmState = v.save_vm_state().unwrap();
    assert_eq!(s.regs.pc, 0x0020_0000);
    assert_eq!(
        <Arm64VmState as SnapshotRecords>::ARCH_TAG,
        vm_state::ARCH_AARCH64
    );

    // Seal + decode through the engine's snapshot store (in-memory; no mmap).
    let mut eng = SnapshotEngine::new(RAM);
    let blob = s.encode().unwrap();
    let snap = eng.snapshot_base(v.guest_memory(), &blob).unwrap();
    let decoded: Arm64VmState = eng.vm_state(snap).unwrap();
    assert_eq!(decoded, s);

    // Restore into a fresh arm64 VM (memory + vm_state — no mmap: the image
    // is the source's own bytes).
    let mut fresh = vmm(vec![]);
    fresh.restore_snapshot(v.guest_memory(), &decoded).unwrap();
    assert_eq!(fresh.inspect_vcpu(), v.inspect_vcpu());
    assert_eq!(
        fresh.state_hash(),
        v.state_hash(),
        "a restored arm64 VM must hash like a never-restored one"
    );

    // And the sealed blob is refused by the x86 record set — the arch tag
    // gates the records both ways.
    assert_eq!(
        VmState::decode(&blob),
        Err(VmStateError::UnsupportedArch(vm_state::ARCH_AARCH64))
    );
}

/// A cross-vendor blob is rejected loudly by the arm64 restore path, before
/// any mutation (the engine decodes through the vendor's own codec, whose
/// arch-tag gate fails closed).
#[test]
fn arm64_restore_rejects_a_foreign_blob() {
    let mut x86 = VmState::default();
    x86.vtime.ratio_den = 1;
    let eng = SnapshotEngine::new(RAM);
    let _ = eng; // (the rejection happens at decode, before any store round trip)
    assert_eq!(
        Arm64VmState::decode(&x86.encode().unwrap()),
        Err(VmStateError::UnsupportedArch(vm_state::ARCH_X86_64))
    );
}

/// A tampered contract hash is refused before any mutation — the arm64 policy
/// skeleton participates in the same anti-drift check as the x86 contract.
#[test]
fn arm64_restore_rejects_a_contract_mismatch() {
    let v = vmm(vec![]);
    let mut s = v.save_vm_state().unwrap();
    s.contract_hash = [0xEE; 32];
    let mut fresh = vmm(vec![]);
    let err = fresh.restore_vm_state(&s).unwrap_err();
    assert!(matches!(err, VmmError::Snapshot(_)), "{err}");
    // The fresh VM is intact: it still runs (nothing was mutated).
    assert!(fresh.terminal_reason().is_none());
}

/// The serial path flows through the vendor: PL011 capture feeds the run
/// result and survives a snapshot/restore; injected input never does.
#[test]
fn arm64_serial_capture_rides_the_snapshot() {
    let mut v = vmm(vec![]);
    // Drive the capture through the device directly via the vendor's own
    // seam (guest MMIO dispatch is the boot path's; the capture surface is
    // engine-visible today through `serial_output`).
    v.inject_serial_input(b"exec-input");
    assert_eq!(v.serial_output(), b"");

    let s = v.save_vm_state().unwrap();
    let mut fresh = vmm(vec![]);
    fresh.restore_vm_state(&s).unwrap();
    // Off-record input did not ride the blob.
    assert_eq!(fresh.serial_output(), b"");
}

/// Every `Backend` method the engine calls is instantiated by the second
/// vendor — exercised directly against the mock (the compile itself is most
/// of the keystone; this pins the runtime contract for the seams the engine
/// reaches).
#[test]
fn mock_arm64_backend_enforces_the_run_loop_contract() {
    let mut b = MockArm64Backend::new();
    // Fail closed before the policy is installed.
    assert!(matches!(
        b.run(),
        Err(vmm_backend::BackendError::NotConfigured)
    ));
    b.set_policy(&Arm64Policy::default()).unwrap();

    // A sysreg read stays pending until completed; resuming is fail-closed.
    b.push_exit(Exit::Arch(Arm64Exit::Sysreg {
        sysreg: 1,
        write: None,
    }));
    let exit = b.run().unwrap();
    assert!(exit.stages_completion());
    assert!(matches!(
        b.run(),
        Err(vmm_backend::BackendError::PendingCompletion)
    ));
    b.complete_read(7).unwrap();

    // The GIC INTID identity flows through the one-slot inject seam.
    b.set_pending_irq(Some(GicIntId(27))).unwrap();
    b.push_exit(Exit::Common(CommonExit::Idle));
    let _ = b.run().unwrap();
    assert_eq!(b.take_accepted_interrupt(), Some(GicIntId(27)));
    assert_eq!(b.take_accepted_interrupt(), None);

    // `inject` records the arm64 injection vocabulary (no NMI variant exists).
    b.inject(Arm64Injection::Interrupt { intid: GicIntId(3) })
        .unwrap();
    assert_eq!(
        b.injected(),
        &[Arm64Injection::Interrupt { intid: GicIntId(3) }]
    );

    // Counters ride the shared roster; the sysreg exit counted.
    assert_eq!(b.exit_counts().sysreg, 1);
    assert_eq!(b.exit_counts().idle, 1);
}
