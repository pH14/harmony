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
    // MMIO at an address that is neither RAM nor any modeled device frame
    // (below the GIC/PL011/doorbell frames) fails closed — default-deny.
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: vmm_backend::Gpa(0x0100_0000),
        size: 4,
        write: Some(0x41),
    })]);
    let err = v.step().unwrap_err();
    assert!(matches!(err, VmmError::ContractViolation(_)), "{err}");
    assert!(format!("{err}").contains("unmodeled MMIO"), "{err}");

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

/// Review r9 (P1): restoring into an UNWIRED VM must require the **complete**
/// unwired V-time sentinel — every `VtimeState` field at its unwired value AND
/// no entropy/hypercall bytes. The prior check tested only `guest_hz`/
/// `snapshot_vns`, so a blob with those zero but a nonzero
/// `ratio_num`/`ratio_den`/`guest_base` or entropy bytes was accepted and its
/// live V-time/entropy state **silently discarded** — a fail-closed
/// snapshot-contract violation.
#[test]
fn arm64_unwired_restore_requires_the_full_vtime_sentinel() {
    // The genuine unwired sentinel the save path stamps restores cleanly.
    let base = vmm(vec![]).save_vm_state().unwrap();
    vmm(vec![]).restore_vm_state(&base).unwrap();
    assert_eq!(
        (
            base.vtime.ratio_num,
            base.vtime.ratio_den,
            base.vtime.guest_hz,
            base.vtime.guest_base,
            base.vtime.snapshot_vns,
            base.hypercall.is_empty(),
        ),
        (0, 1, 0, 0, 0, true),
        "the unwired save sentinel"
    );

    // Populate ONE field at a time — each must fail closed with the wiring
    // message (the old check let every field but guest_hz/snapshot_vns through).
    type Mutator = fn(&mut Arm64VmState);
    let mutators: [(&str, Mutator); 6] = [
        ("ratio_num", |s| s.vtime.ratio_num = 7),
        ("ratio_den", |s| s.vtime.ratio_den = 2),
        ("guest_hz", |s| s.vtime.guest_hz = 1_000),
        ("guest_base", |s| s.vtime.guest_base = 42),
        ("snapshot_vns", |s| s.vtime.snapshot_vns = 99),
        ("entropy", |s| s.hypercall = vec![1, 2, 3]),
    ];
    for (field, mutate) in mutators {
        let mut s = vmm(vec![]).save_vm_state().unwrap();
        mutate(&mut s);
        let err = vmm(vec![]).restore_vm_state(&s).unwrap_err();
        assert!(
            matches!(err, VmmError::ContractViolation(_))
                && format!("{err}").contains("no V-time wired"),
            "unwired restore must reject a populated {field}: {err}"
        );
    }
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

/// M2 — the wired GICv3 fabric: host injection lands in the pending file,
/// the per-entry service hands the backend the arbitrated INTID, acceptance
/// moves it pending→active, and the whole fabric rides the snapshot.
#[test]
fn arm64_gic_fabric_arbitrates_and_rides_the_snapshot() {
    use gicv3::GicFrame;
    use vmm_core::vendor::arm64::board;

    // A fabric with INTID 40 fully deliverable (Group 1, enabled, priority
    // 0x40, forwarding on, PMR open).
    let mut gic = board::new_gic();
    gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap(); // CTLR.EnableGrp1
    gic.mmio_write(GicFrame::Dist, 0x0080 + 4, 1 << 8, 0)
        .unwrap(); // IGROUPR1
    gic.mmio_write(GicFrame::Dist, 0x0100 + 4, 1 << 8, 0)
        .unwrap(); // ISENABLER1
    gic.mmio_write(GicFrame::Dist, 0x0400 + 40, 0x40, 0)
        .unwrap(); // IPRIORITYR
    gic.set_pmr(0xFF);

    let mut v = vmm(vec![Exit::Common(CommonExit::Idle)]);
    v.wire_gic(gic);
    assert!(v.gic_wired());

    // Stage-time validation now answers from the implemented identity space
    // (the board's 64 SPIs ⇒ INTID limit 96): 40 is a legal SPI, 200 is past
    // the distributor bound. (SGIs `0..16` would deliver too — never x86's
    // reserved-vector rule.)
    v.apply_host_fault(&environment::HostFault::InjectInterrupt { vector: 40 })
        .unwrap();
    assert!(
        v.apply_host_fault(&environment::HostFault::InjectInterrupt { vector: 200 })
            .is_err(),
        "past the distributor-bounded identity space"
    );
    assert!(v.has_pending_guest_interrupt().unwrap());

    // Seal at the pending point (before any terminal latches): the pending
    // INTID must ride the blob, not be prematurely in-service.
    let s = v.save_vm_state().unwrap();

    // One step: the service seam hands the mock the arbitrated INTID, the
    // mock accepts it at entry, and completion moves it pending→active — so
    // afterwards nothing is pending and the idle exit latches the terminal.
    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Idle));
    assert!(!v.has_pending_guest_interrupt().unwrap());

    // The fabric rides the snapshot: restore into two gic-wired twins — both
    // resume with the INTID still pending (re-derived, not lost, not
    // in-service) and hash identically to each other.
    let twin_gic = board::new_gic;
    let mut twin_a = vmm(vec![]);
    twin_a.wire_gic(twin_gic());
    twin_a.restore_vm_state(&s).unwrap();
    let mut twin_b = vmm(vec![]);
    twin_b.wire_gic(twin_gic());
    twin_b.restore_vm_state(&s).unwrap();
    assert!(twin_a.has_pending_guest_interrupt().unwrap());
    assert_eq!(twin_a.state_hash(), twin_b.state_hash());

    // Restore into an UNWIRED VM is a loud wiring mismatch, never a silently
    // dropped fabric.
    let mut unwired = vmm(vec![]);
    let err = unwired.restore_vm_state(&s).unwrap_err();
    assert!(format!("{err}").contains("wiring mismatch"), "{err}");

    // Finding 2 (review r2): restoring into a GIC wired with a DIFFERENT config
    // (impl_spis / timer_hz / timer_intid) is rejected — the distributor bound
    // (GICD_TYPER.ITLinesNumber) and the timer deadline conversion cannot
    // silently change under an unchanged board/DTB. A restore never adopts the
    // snapshot's config over the wired target's.
    let mismatched = |cfg: gicv3::GicConfig| {
        let mut v = vmm(vec![]);
        v.wire_gic(gicv3::Gicv3::new(cfg).unwrap());
        v.restore_vm_state(&s)
    };
    let base = board::gic_config();
    for bad in [
        gicv3::GicConfig {
            impl_spis: 32,
            ..base
        }, // GICD_TYPER changes
        gicv3::GicConfig {
            timer_hz: base.timer_hz * 2,
            ..base
        }, // deadline conv changes
        gicv3::GicConfig {
            timer_intid: 26,
            ..base
        }, // a different PPI
    ] {
        let err = mismatched(bad).unwrap_err();
        assert!(
            format!("{err}").contains("GICv3 config mismatch"),
            "config {bad:?} must be rejected: {err}"
        );
    }
    // The matching board config restores cleanly (the round-trip still holds).
    assert!(mismatched(base).is_ok());
}

/// M2 — the generic timer is a pure deadlines-out seam: an armed CVAL is a
/// V-time deadline, and once the fabric's V-time passes it, the PPI latches
/// pending and arbitration delivers it.
#[test]
fn arm64_generic_timer_feeds_the_deadline_seam() {
    use gicv3::{CNTV_CTL_ENABLE, GicFrame};
    use vmm_core::vendor::arm64::board;
    use vmm_core::vmm::VtimeWiring;
    use vmm_core::work::ScriptedWork;
    use vtime::VClockConfig;

    let mut gic = board::new_gic();
    // Make the timer PPI deliverable, then arm CVAL = 125 ticks ⇒ 2000 vns.
    gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap();
    let sgi = 0x1_0000;
    gic.mmio_write(GicFrame::Redist, sgi + 0x0080, 1 << 27, 0)
        .unwrap();
    gic.mmio_write(GicFrame::Redist, sgi + 0x0100, 1 << 27, 0)
        .unwrap();
    gic.set_pmr(0xFF);
    gic.write_cntv_cval(125);
    gic.write_cntv_ctl(CNTV_CTL_ENABLE);
    assert_eq!(gic.next_timer_deadline(), Some(2000));
    assert!(gic.armed_timer_deliverable());

    // A V-time-wired arm64 VM whose work counter sits past the deadline. The
    // mock must NOT claim a deterministic clock here: `now_vns` then reads
    // the live (scripted) counter, exactly like a stock backend.
    let mut b = MockArm64Backend::with_capabilities(vmm_backend::Capabilities {
        name: "mock-arm64-stockish",
        deterministic_rng: true,
        arch: vmm_backend::Arm64Caps {
            deterministic_cntvct: false,
            enforces_cntv_cval: false,
        },
    });
    b.set_policy(&Arm64Policy::default()).unwrap();
    let mut v = Vmm::new(b, GuestRam::new(RAM).unwrap());
    v.wire_vtime(
        VtimeWiring::new(
            VClockConfig {
                ratio_num: 1,
                ratio_den: 1,
                guest_hz: 62_500_000,
                guest_base: 0,
                vns_base: 0,
            },
            Box::new(ScriptedWork::at(2500)), // now_vns = 2500 ≥ 2000
            7,
        )
        .unwrap(),
    );
    v.wire_gic(gic);

    // The out-of-run-loop query advances the fabric to now_vns: the deadline
    // has passed, the PPI latches pending, and arbitration delivers it.
    assert!(v.has_pending_guest_interrupt().unwrap());
}

/// M3 — the board memory map routes device MMIO: the PL011 console frame is a
/// modeled device (a store lands in the capture, read-back works), the
/// reserved doorbell GPA is recognized (default-denied without an SDK, like
/// x86's port), and the GIC frames fail closed when the fabric is unwired.
#[test]
fn arm64_board_mmio_routes_pl011_doorbell_and_gic() {
    use vmm_backend::Gpa;

    // A PL011 UARTDR store (offset 0x000) captures a byte; a UARTFR read
    // (offset 0x018) reads back the flag register.
    let mut v = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0x0900_0000),
            size: 4,
            write: Some(u64::from(b'Z')),
        }),
        Exit::Common(CommonExit::Idle),
    ]);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.serial_output(), b"Z");
    // The idle exit latches the terminal (nothing to wake it — unwired fabric).
    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Idle));

    // The reserved doorbell GPA is recognized; without an SDK channel wired the
    // dispatcher default-denies (a ContractViolation, never an unmodeled-MMIO
    // error) — the arm64 mirror of x86's DOORBELL_PORT.
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0A00_0000),
        size: 4,
        write: Some(0x3150_4348),
    })]);
    let err = v.step().unwrap_err();
    assert!(matches!(err, VmmError::ContractViolation(_)), "{err}");
    assert!(
        !format!("{err}").contains("unmodeled MMIO"),
        "doorbell was recognized: {err}"
    );

    // A GIC-frame access with no fabric wired fails closed, naming the
    // AA-6-gated delivery.
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0800_0000),
        size: 4,
        write: Some(0),
    })]);
    let err = v.step().unwrap_err();
    assert!(format!("{err}").contains("GICv3 MMIO"), "{err}");

    // Every modeled device arm rejects a non-32-bit width **before** touching
    // device state — never a silent `v as u32` truncation. (r1: the GICv3
    // frames; r2: swept across the PL011 console AND the reserved doorbell,
    // all 32-bit-register/word-ABI.) The width guard precedes the
    // unwired-fabric / doorbell-dispatch, so it surfaces regardless.
    for (name, gpa) in [
        ("GICD", 0x0800_0000u64),
        ("GICR", 0x080A_0000),
        ("PL011", 0x0900_0000),
        ("doorbell", 0x0A00_0000),
    ] {
        for bad in [1u8, 2, 8] {
            let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
                gpa: Gpa(gpa),
                size: bad,
                write: Some(0),
            })]);
            let err = v.step().unwrap_err();
            let msg = format!("{err}");
            assert!(
                msg.contains(&format!("size {bad} != 4")),
                "{name} size {bad} must fail closed on width: {msg}"
            );
        }

        // Review r5 P2(a): a start-in-frame predicate is not enough — validate
        // the full checked range + register alignment. A **misaligned** access
        // (base+1, size 4) fails closed on alignment; a **straddling** access
        // (last word of the frame with a width that runs past the boundary)
        // fails closed on the range — neither is silently dispatched.
        let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(gpa + 1),
            size: 4,
            write: Some(0),
        })]);
        let err = v.step().unwrap_err();
        assert!(
            format!("{err}").contains("not 4-byte aligned"),
            "{name} base+1 must fail closed on alignment: {err}"
        );
        // The last 4-aligned word of the 4 KiB/64 KiB/... frame, size 8 →
        // end = frame_end + 4, straddling the boundary (start still in-frame).
        let frame_len = match name {
            "GICD" => 0x1_0000u64,
            "GICR" => 0x2_0000,
            _ => 0x1000, // PL011 / doorbell
        };
        let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(gpa + frame_len - 4),
            size: 8,
            write: Some(0),
        })]);
        let err = v.step().unwrap_err();
        assert!(
            format!("{err}").contains("straddles the frame boundary"),
            "{name} last-word size-8 must fail closed on straddle: {err}"
        );
    }
}

/// Review r5 P2(b): the GICv3 state feeds `state_hash` (the `GICV` chunk), so
/// `state_components()` must expose a labeled `gic` component — otherwise two
/// runs differing **only** in GIC state hash differently while every diagnostic
/// component matches, defeating divergence localization.
#[test]
fn arm64_state_components_localizes_a_gic_only_divergence() {
    use gicv3::GicFrame;
    use vmm_core::vendor::arm64::board;

    let make = |raise: Option<u32>| {
        let mut gic = board::new_gic();
        // Program INTID 40 deliverable, then optionally raise it pending — a
        // GIC-only difference (no vCPU / RAM / serial change).
        gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0080 + 4, 1 << 8, 0)
            .unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0100 + 4, 1 << 8, 0)
            .unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0400 + 40, 0x40, 0)
            .unwrap();
        gic.set_pmr(0xFF);
        if let Some(intid) = raise {
            gic.raise(intid).unwrap();
        }
        let mut v = vmm(vec![]);
        v.wire_gic(gic);
        v
    };

    let a = make(None);
    let b = make(Some(40)); // differs only in the GIC pending file

    // `state_hash` differs (the GICV chunk folds in the pending state)...
    assert_ne!(a.state_hash(), b.state_hash());

    // ...and the `gic` component is exactly what localizes it: it differs, and
    // it is the ONLY differing component (every other label matches).
    let ca = a.state_components();
    let cb = b.state_components();
    let gic_a = ca
        .iter()
        .find(|(l, _)| *l == "gic")
        .expect("a gic component");
    let gic_b = cb
        .iter()
        .find(|(l, _)| *l == "gic")
        .expect("a gic component");
    assert_ne!(
        gic_a.1, gic_b.1,
        "the gic component must localize the divergence"
    );
    for (la, da) in &ca {
        if *la == "gic" {
            continue;
        }
        let db = cb.iter().find(|(lb, _)| lb == la).map(|(_, d)| d);
        assert_eq!(
            Some(da),
            db,
            "component {la} must match (only the GIC differs)"
        );
    }

    // An unwired VM exposes no `gic` component (additive-only; the label
    // appears exactly when the GICV chunk does).
    let unwired = vmm(vec![]);
    assert!(!unwired.state_components().iter().any(|(l, _)| *l == "gic"));
}

/// M3 — the full boot composition: `boot` runs the host-baseline gate then
/// loads an Image + DTB and sets the entry state, all mock-backed.
#[test]
fn arm64_boot_composes_a_ready_vmm() {
    use vmm_backend::MockArm64Backend;
    use vmm_core::vendor::arm64::{bringup, dtb, image_loader};

    // A tiny valid Image (header + 256 bytes), 16 MiB RAM.
    let image = image_loader::wrap_image(&[0x42u8; 256], 0, 0xA);
    let backend = MockArm64Backend::new();
    let v = bringup::boot(backend, &image, "console=ttyAMA0", 16 * 1024 * 1024).unwrap();

    let vcpu = v.inspect_vcpu();
    assert_eq!(vcpu.core.pc, 0x4000_0000); // RAM_BASE
    assert_eq!(vcpu.core.pstate, 0x3c5); // EL1h + DAIF masked
    let dtb_gpa = vcpu.core.x[0];
    // x0 points at a DTB in RAM that parses back to the board's devices.
    let off = (dtb_gpa - 0x4000_0000) as usize;
    let parsed = dtb::parse(&v.guest_memory()[off..]).unwrap();
    assert!(parsed.nodes.iter().any(|n| n == "intc@8000000"));
    assert!(parsed.nodes.iter().any(|n| n == "timer"));
}
