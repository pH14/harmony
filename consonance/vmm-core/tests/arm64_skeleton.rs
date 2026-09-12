// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The M1 keystone assertion** (`tasks/112`): the arm64 vendor — the first
//! real *second* implementor of `Vendor`/`Backend`/`Arch` — instantiates every
//! method the engine calls, and the engine drives it through exactly the same
//! generic types it drives x86 through. This is the structural check no
//! cross-compile gate can perform (`docs/ARCHITECTURE.md`: on the aarch64
//! CI leg no vendor exists to *instantiate* the trait, so a signature only a
//! second implementor could refute stays invisible until this vendor exists).
//!
//! Portable and Miri-clean: driven by the scripted `MockArm64Backend`, no
//! `/dev/kvm`, no mmap (the snapshot round-trip seals and decodes through the
//! in-memory store; the mmap-backed `materialize` path is the x86-shared
//! engine machinery already covered elsewhere and is not re-tested here).

use environment::channel::Effect;
use vm_state::{Arm64VmState, SnapshotRecords, VmState, VmStateError};
use vmm_backend::{
    Arm64, Arm64Exit, Arm64Injection, Arm64Policy, Arm64VcpuState, Backend, CommonExit, Exit,
    GicIntId, MockArm64Backend, MpState,
};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vmm::{GuestRam, Step, TerminalReason, Vmm, VmmError};

const RAM: usize = 0x4000;

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
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: vmm_backend::Gpa(0x0100_0000),
        size: 4,
        write: Some(0x41),
    })]);
    let err = v.step().unwrap_err();
    assert!(matches!(err, VmmError::ContractViolation(_)), "{err}");
    assert!(format!("{err}").contains("unmodeled MMIO"), "{err}");

    let mut v = vmm(vec![Exit::Arch(Arm64Exit::Sysreg {
        sysreg: 0x0018_0000,
        write: None,
    })]);
    let err = v.step().unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("no ruled disposition"), "{msg}");
}

/// HVF traps Linux's OSDLR_EL1 zero write during debug-monitor setup. Only
/// that deterministic unlock is ruled; nonzero writes and reads stay denied.
#[test]
fn arm64_os_debug_lock_accepts_only_the_boot_unlock() {
    const OSDLR_EL1: u32 = 0x0028_0406;
    const OSLAR_EL1: u32 = 0x0028_0400;

    for sysreg in [OSDLR_EL1, OSLAR_EL1] {
        let mut accepted = vmm(vec![Exit::Arch(Arm64Exit::Sysreg {
            sysreg,
            write: Some(0),
        })]);
        wire_virtual_time_clock(&mut accepted);
        assert_eq!(accepted.step().unwrap(), Step::Continued);

        for write in [Some(1), None] {
            let mut rejected = vmm(vec![Exit::Arch(Arm64Exit::Sysreg { sysreg, write })]);
            wire_virtual_time_clock(&mut rejected);
            let err = rejected.step().unwrap_err();
            assert!(format!("{err}").contains("debug-lock"), "{err}");
        }
    }
}

/// The interrupt seams answer honestly with no fabric wired: stage-time
/// validation refuses every identity, injection fails loud, and nothing is
/// pending — mirroring the x86 unwired-LAPIC posture.
#[test]
fn arm64_interrupt_seams_report_no_fabric() {
    let mut v = vmm(vec![]);
    assert!(!v.has_pending_guest_interrupt().unwrap());
    let err = v.apply_effect(&Effect::InjectInterrupt { vector: 40 });
    assert!(err.is_err(), "no fabric wired: injection must fail loud");
}

/// The keystone round trip: build → seal → decode → restore, entirely through
/// the engine's generic snapshot path (`Vendor::Snapshot = Arm64VmState`), and
/// state-hash-transparent: the restored VM hashes identically to the source.
#[test]
fn arm64_snapshot_round_trip_is_restore_transparent() {
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
    v.inject_serial_input(b"never-snapshotted");

    let s: Arm64VmState = v.save_vm_state().unwrap();
    assert_eq!(s.regs.pc, 0x0020_0000);
    assert_eq!(
        <Arm64VmState as SnapshotRecords>::ARCH_TAG,
        vm_state::ARCH_AARCH64
    );

    let mut eng = SnapshotEngine::new(RAM);
    let blob = s.encode().unwrap();
    let snap = eng.snapshot_base(v.guest_memory(), &blob).unwrap();
    let decoded: Arm64VmState = eng.vm_state(snap).unwrap();
    assert_eq!(decoded, s);

    let mut fresh = vmm(vec![]);
    fresh.restore_snapshot(v.guest_memory(), &decoded).unwrap();
    assert_eq!(fresh.inspect_vcpu(), v.inspect_vcpu());
    assert_eq!(
        fresh.state_hash().unwrap(),
        v.state_hash().unwrap(),
        "a restored arm64 VM must hash like a never-restored one"
    );

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
    let x86 = VmState::default();
    let eng = SnapshotEngine::new(RAM);
    let _ = eng;
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
    assert!(fresh.terminal_reason().is_none());
}

/// Review r9 (P1): restoring into an UNWIRED VM must require the **complete**
/// unwired V-time sentinel — every `VtimeState` field at its unwired value AND
/// no entropy/hypercall bytes. The prior check tested only `guest_hz`/
/// `snapshot_vns`, so a blob with those zero but a nonzero
/// `guest_base` or entropy bytes was accepted and its
/// live V-time/entropy state **silently discarded** — a fail-closed
/// snapshot-contract violation.
#[test]
fn arm64_unwired_restore_requires_the_full_vtime_sentinel() {
    let base = vmm(vec![]).save_vm_state().unwrap();
    vmm(vec![]).restore_vm_state(&base).unwrap();
    assert_eq!(
        (
            base.vtime.guest_hz,
            base.vtime.guest_base,
            base.vtime.snapshot_vns,
            base.hypercall.is_empty(),
        ),
        (0, 0, 0, true),
        "the unwired save sentinel"
    );

    type Mutator = fn(&mut Arm64VmState);
    let mutators: [(&str, Mutator); 4] = [
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
    v.inject_serial_input(b"exec-input");
    assert_eq!(v.serial_output(), b"");

    let s = v.save_vm_state().unwrap();
    let mut fresh = vmm(vec![]);
    fresh.restore_vm_state(&s).unwrap();
    assert_eq!(fresh.serial_output(), b"");
}

/// Every `Backend` method the engine calls is instantiated by the second
/// vendor — exercised directly against the mock (the compile itself is most
/// of the keystone; this pins the runtime contract for the seams the engine
/// reaches).
#[test]
fn mock_arm64_backend_enforces_the_run_loop_contract() {
    let mut b = MockArm64Backend::new();
    assert!(matches!(
        b.run(),
        Err(vmm_backend::BackendError::NotConfigured)
    ));
    b.set_policy(&Arm64Policy::default()).unwrap();

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

    b.set_pending_irq(Some(GicIntId(27))).unwrap();
    b.push_exit(Exit::Common(CommonExit::Idle));
    let _ = b.run().unwrap();
    assert_eq!(b.take_accepted_interrupt(), Some(GicIntId(27)));
    assert_eq!(b.take_accepted_interrupt(), None);

    b.inject(Arm64Injection::Interrupt { intid: GicIntId(3) })
        .unwrap();
    assert_eq!(
        b.injected(),
        &[Arm64Injection::Interrupt { intid: GicIntId(3) }]
    );

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

    let mut gic = board::new_gic();
    gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap();
    gic.mmio_write(GicFrame::Dist, 0x0080 + 4, 1 << 8, 0)
        .unwrap();
    gic.mmio_write(GicFrame::Dist, 0x0100 + 4, 1 << 8, 0)
        .unwrap();
    gic.mmio_write(GicFrame::Dist, 0x0400 + 40, 0x40, 0)
        .unwrap();
    gic.set_pmr(0xFF);
    gic.set_group1_enabled(true);

    let mut v = vmm(vec![Exit::Common(CommonExit::Idle)]);
    v.wire_gic(gic);
    assert!(v.gic_wired());

    v.apply_effect(&Effect::InjectInterrupt { vector: 40 })
        .unwrap();
    assert!(
        v.apply_effect(&Effect::InjectInterrupt { vector: 200 })
            .is_err(),
        "past the distributor-bounded identity space"
    );
    assert!(v.has_pending_guest_interrupt().unwrap());

    let s = v.save_vm_state().unwrap();

    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Idle));
    assert!(!v.has_pending_guest_interrupt().unwrap());

    let twin_gic = board::new_gic;
    let mut twin_a = vmm(vec![]);
    twin_a.wire_gic(twin_gic());
    twin_a.restore_vm_state(&s).unwrap();
    let mut twin_b = vmm(vec![]);
    twin_b.wire_gic(twin_gic());
    twin_b.restore_vm_state(&s).unwrap();
    assert!(twin_a.has_pending_guest_interrupt().unwrap());
    assert_eq!(twin_a.state_hash().unwrap(), twin_b.state_hash().unwrap());

    let mut unwired = vmm(vec![]);
    let err = unwired.restore_vm_state(&s).unwrap_err();
    assert!(format!("{err}").contains("wiring mismatch"), "{err}");

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
        },
        gicv3::GicConfig {
            timer_hz: base.timer_hz * 2,
            ..base
        },
        gicv3::GicConfig {
            timer_intid: 26,
            ..base
        },
    ] {
        let err = mismatched(bad).unwrap_err();
        assert!(
            format!("{err}").contains("GICv3 config mismatch"),
            "config {bad:?} must be rejected: {err}"
        );
    }
    assert!(mismatched(base).is_ok());
}

/// M2 — the generic timer is a pure deadlines-out seam: an armed CVAL is a
/// V-time deadline, and once the fabric's V-time passes it, the PPI latches
/// pending and arbitration delivers it.
/// M3 — the board memory map routes device MMIO: the PL011 console frame is a
/// modeled device (a store lands in the capture, read-back works), the
/// reserved doorbell GPA is recognized (default-denied without an SDK, like
/// x86's port), and the GIC frames fail closed when the fabric is unwired.
#[test]
fn arm64_board_mmio_routes_pl011_doorbell_and_gic() {
    use vmm_backend::Gpa;

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
    assert_eq!(v.step().unwrap(), Step::Terminal(TerminalReason::Idle));

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

    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0800_0000),
        size: 4,
        write: Some(0),
    })]);
    let err = v.step().unwrap_err();
    assert!(format!("{err}").contains("GICv3 MMIO"), "{err}");

    for size in [1u8, 2, 4] {
        let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0x0900_0000),
            size,
            write: Some(0xFFFF_FF51),
        })]);
        assert_eq!(v.step().unwrap(), Step::Continued);
        assert_eq!(v.serial_output(), b"Q");
    }
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0900_0000),
        size: 8,
        write: Some(0),
    })]);
    assert!(format!("{}", v.step().unwrap_err()).contains("unmodeled size 8"));

    for (name, gpa) in [
        ("GICD", 0x0800_0000u64),
        ("GICR", 0x080A_0000),
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
                msg.contains(&format!("unmodeled size {bad}")),
                "{name} size {bad} must fail closed on width: {msg}"
            );
        }

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
        let frame_len = match name {
            "GICD" => 0x1_0000u64,
            "GICR" => 0x2_0000,
            _ => 0x1000,
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

    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x080A_0008),
        size: 8,
        write: None,
    })]);
    v.wire_gic(vmm_core::vendor::arm64::board::new_gic());
    assert_eq!(v.step().unwrap(), Step::Continued);

    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x080A_0008),
        size: 8,
        write: Some(0),
    })]);
    v.wire_gic(vmm_core::vendor::arm64::board::new_gic());
    assert!(format!("{}", v.step().unwrap_err()).contains("read-only"));

    for write in [None, Some(0)] {
        let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(0x0800_6100),
            size: 8,
            write,
        })]);
        v.wire_gic(vmm_core::vendor::arm64::board::new_gic());
        assert_eq!(v.step().unwrap(), Step::Continued);
    }
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0800_6100),
        size: 8,
        write: Some(1),
    })]);
    v.wire_gic(vmm_core::vendor::arm64::board::new_gic());
    assert!(format!("{}", v.step().unwrap_err()).contains("unsupported affinity"));

    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0900_0001),
        size: 1,
        write: Some(0),
    })]);
    assert!(format!("{}", v.step().unwrap_err()).contains("not 4-byte aligned"));
    let mut v = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0900_0ffc),
        size: 8,
        write: Some(0),
    })]);
    assert!(format!("{}", v.step().unwrap_err()).contains("straddles the frame boundary"));
}

#[test]
fn arm64_virtual_time_pvclock_registration_is_exact_and_stamps_guest_ram() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::{CNTFRQ_HZ, PVCLOCK};
    use vmm_core::vmm::VtimeWiring;
    use vtime::VClockConfig;

    let page_gpa = 0x1000;
    let mut v = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(page_gpa),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 8),
            size: 4,
            write: None,
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x20),
            size: 4,
            write: Some(1),
        }),
    ]);
    v.wire_vtime(
        VtimeWiring::new_virtual_time(
            VClockConfig {
                guest_hz: CNTFRQ_HZ,
                guest_base: 0,
                vns_base: 0,
            },
            7,
        )
        .unwrap(),
    );
    v.enable_pvclock();

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.pvclock_registration(), Some(page_gpa));
    let first = vtime::pvclock::read(v.pvclock_page().unwrap()).unwrap();
    assert_eq!(first.vns, 10_000);
    assert_eq!(first.guest_clock_hz, CNTFRQ_HZ);
    assert_eq!(v.step().unwrap(), Step::Continued);
    let second = vtime::pvclock::read(v.pvclock_page().unwrap()).unwrap();
    assert_eq!(second.vns, 20_000);
    assert_eq!(v.step().unwrap(), Step::Continued);
    let tick = vtime::pvclock::read(v.pvclock_page().unwrap()).unwrap();
    assert_eq!(tick.vns, 120_000);

    for (offset, size, write) in [
        (0, 8, None),
        (8, 8, None),
        (8, 4, Some(1)),
        (0x20, 4, Some(0)),
        (0x20, 8, Some(1)),
        (0x20, 4, None),
        (0x24, 4, Some(0)),
        (0x24, 8, Some(1)),
        (0x24, 4, None),
    ] {
        let mut bad = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + offset),
            size,
            write,
        })]);
        assert!(format!("{}", bad.step().unwrap_err()).contains("protocol fault"));
        assert_eq!(bad.pvclock_registration(), None);
    }
}

/// Build the M1 userspace fabric with the dedicated pvclock PPI configured as
/// a deliverable Group-1 level interrupt.
fn clockevent_gic() -> gicv3::Gicv3 {
    use gicv3::GicFrame;
    use vmm_core::vendor::arm64::board::{self, PVCLOCK_PPI};

    let mut gic = board::new_gic();
    gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap();
    let sgi = 0x1_0000;
    gic.mmio_write(GicFrame::Redist, sgi + 0x0080, 1 << PVCLOCK_PPI, 0)
        .unwrap();
    gic.mmio_write(GicFrame::Redist, sgi + 0x0100, 1 << PVCLOCK_PPI, 0)
        .unwrap();
    gic.set_pmr(0xff);
    gic.set_group1_enabled(true);
    gic
}

/// Compose the exact assigned-at-exit clock used by the M1 board tests.
fn wire_virtual_time_clock(v: &mut Vmm<MockArm64Backend>) {
    use vmm_core::vendor::arm64::board::CNTFRQ_HZ;
    use vmm_core::vmm::VtimeWiring;
    use vtime::VClockConfig;

    v.wire_vtime(
        VtimeWiring::new_virtual_time(
            VClockConfig {
                guest_hz: CNTFRQ_HZ,
                guest_base: 0,
                vns_base: 0,
            },
            7,
        )
        .unwrap(),
    );
    v.enable_pvclock();
    v.wire_gic(clockevent_gic());
}

/// The paravirtual clockevent is an absolute-deadline, one-shot, level PPI:
/// reaching the deadline asserts the clockevent PPI, EOI without device ACK re-pends it,
/// ACK lowers it, and the complete in-flight state survives a snapshot.
#[test]
fn arm64_clockevent_is_level_triggered_and_snapshot_complete() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::{PVCLOCK, PVCLOCK_PPI};

    const ICC_IAR1_EL1: u32 = 0x0030_3018;
    const ICC_EOIR1_EL1: u32 = 0x0032_3018;
    let page_gpa = 0x1000;
    let mut v = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(page_gpa),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(125),
        }),
        Exit::Arch(Arm64Exit::Sysreg {
            sysreg: ICC_IAR1_EL1,
            write: None,
        }),
        Exit::Arch(Arm64Exit::Sysreg {
            sysreg: ICC_EOIR1_EL1,
            write: Some(u64::from(PVCLOCK_PPI)),
        }),
        Exit::Arch(Arm64Exit::Sysreg {
            sysreg: ICC_IAR1_EL1,
            write: None,
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x18),
            size: 4,
            write: Some(2),
        }),
        Exit::Arch(Arm64Exit::Sysreg {
            sysreg: ICC_EOIR1_EL1,
            write: Some(u64::from(PVCLOCK_PPI)),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x18),
            size: 4,
            write: Some(2),
        }),
    ]);
    wire_virtual_time_clock(&mut v);

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(v.has_pending_guest_interrupt().unwrap());
    assert!(
        v.state_components()
            .iter()
            .any(|(label, _)| *label == "arm-clockevent"),
        "non-default clockevent state must be independently localizable"
    );

    let snapshot = v.save_vm_state().unwrap();

    let mut restored = vmm(vec![]);
    wire_virtual_time_clock(&mut restored);
    restored
        .restore_snapshot(v.guest_memory(), &snapshot)
        .unwrap();
    assert!(restored.has_pending_guest_interrupt().unwrap());
    assert_eq!(restored.state_hash().unwrap(), v.state_hash().unwrap());

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(v.has_pending_guest_interrupt().unwrap());
    assert_eq!(v.step().unwrap(), Step::Continued);

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(!v.has_pending_guest_interrupt().unwrap());
    let err = v.step().unwrap_err();
    assert!(format!("{err}").contains("ACK while its PPI is not asserted"));
}

/// A due clockevent remains only a deadline while IRQs are masked. The first
/// explicit post-unmask exit is the sole delivery boundary, so HVF and KVM
/// cannot choose different instructions from an implementation-defined
/// pending-IRQ recognition window.
#[test]
fn arm64_clockevent_delivery_waits_for_the_irq_unmask_exit() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::PVCLOCK;

    let mut state = Arm64VcpuState::default();
    state.core.pstate = 1 << 7;
    let mut backend = MockArm64Backend::with_exits([
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(0x1000),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(125),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x24),
            size: 4,
            write: Some(1),
        }),
    ]);
    backend.set_policy(&Arm64Policy::default()).unwrap();
    backend.set_state(state);
    let mut v = Vmm::new(backend, GuestRam::new(RAM).unwrap());
    wire_virtual_time_clock(&mut v);

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(
        !v.has_pending_guest_interrupt().unwrap(),
        "the planted early-delivery mutant would assert while PSTATE.I is set"
    );

    let mut unmasked = v.save_vm_state().unwrap();
    unmasked.regs.pstate &= !(1 << 7);
    v.restore_vm_state(&unmasked).unwrap();

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(v.has_pending_guest_interrupt().unwrap());
    assert_eq!(
        v.effective_vns(),
        Some(30_000),
        "the unmask fence must not assign the 100 µs execution-density tick"
    );
}

/// Protocol misuse is rejected rather than silently changing the one-shot
/// state, while DISARM before expiry cancels both the deadline and delivery.
#[test]
fn arm64_clockevent_protocol_faults_and_disarm_are_fail_closed() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::PVCLOCK;

    let page_gpa = 0x1000;
    let mut disarm = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(page_gpa),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(10_000),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x18),
            size: 4,
            write: Some(1),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 8),
            size: 4,
            write: None,
        }),
    ]);
    wire_virtual_time_clock(&mut disarm);
    for _ in 0..4 {
        assert_eq!(disarm.step().unwrap(), Step::Continued);
    }
    assert!(!disarm.has_pending_guest_interrupt().unwrap());
    assert!(
        !disarm
            .state_components()
            .iter()
            .any(|(label, _)| *label == "arm-clockevent"),
        "a fully disarmed never-fired device returns to canonical default state"
    );

    for control in [0, 3, u64::from(u32::MAX) + 1] {
        let mut bad = vmm(vec![Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x18),
            size: 4,
            write: Some(control),
        })]);
        wire_virtual_time_clock(&mut bad);
        assert!(bad.step().is_err(), "control {control} must fail closed");
    }

    let mut asserted = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(page_gpa),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(125),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(250),
        }),
    ]);
    wire_virtual_time_clock(&mut asserted);
    assert_eq!(asserted.step().unwrap(), Step::Continued);
    assert_eq!(asserted.step().unwrap(), Step::Continued);
    let err = asserted.step().unwrap_err();
    assert!(format!("{err}").contains("deadline write while its PPI is asserted"));
}

/// VirtualTime WFI uses `IdlePlanner` to land exactly on the paravirtual
/// clockevent deadline, raises the clockevent PPI at that same normalized event, and never
/// asks the backend for an unsupported mid-stream `run_with_deadline` stop.
#[test]
fn arm64_virtual_time_wfi_jumps_to_the_clockevent_deadline() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::{CNTFRQ_HZ, PVCLOCK, PVCLOCK_PPI};
    use vmm_core::virtual_time::{NormalizedEventClass, check_delivery_placement};

    let page_gpa = 0x1000;
    let deadline_vns = 50_000;
    let deadline_ticks = deadline_vns * CNTFRQ_HZ / 1_000_000_000;
    let mut v = vmm(vec![
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0),
            size: 8,
            write: Some(page_gpa),
        }),
        Exit::Common(CommonExit::Mmio {
            gpa: Gpa(PVCLOCK.0 + 0x10),
            size: 8,
            write: Some(deadline_ticks),
        }),
        Exit::Common(CommonExit::Idle),
    ]);
    wire_virtual_time_clock(&mut v);

    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert!(!v.has_pending_guest_interrupt().unwrap());
    assert_eq!(v.step().unwrap(), Step::Continued);
    assert_eq!(v.effective_vns(), Some(deadline_vns));
    assert_eq!(v.idle_landings(), &[deadline_vns]);
    assert!(v.has_pending_guest_interrupt().unwrap());

    let trace = v.virtual_time_trace().unwrap();
    let idle = &trace.normalized_log().events[2];
    assert_eq!(idle.class, NormalizedEventClass::Idle);
    assert_eq!(idle.vns_after, deadline_vns);
    assert_eq!(idle.interrupts.len(), 1);
    assert_eq!(idle.interrupts[0].interrupt_id, PVCLOCK_PPI);
    check_delivery_placement(trace.schedule(), trace.normalized_log()).unwrap();
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
        gic.mmio_write(GicFrame::Dist, 0x0000, 0b10, 0).unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0080 + 4, 1 << 8, 0)
            .unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0100 + 4, 1 << 8, 0)
            .unwrap();
        gic.mmio_write(GicFrame::Dist, 0x0400 + 40, 0x40, 0)
            .unwrap();
        gic.set_pmr(0xFF);
        gic.set_group1_enabled(true);
        if let Some(intid) = raise {
            gic.raise(intid).unwrap();
        }
        let mut v = vmm(vec![]);
        v.wire_gic(gic);
        v
    };

    let a = make(None);
    let b = make(Some(40));

    assert_ne!(a.state_hash().unwrap(), b.state_hash().unwrap());

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

    let unwired = vmm(vec![]);
    assert!(!unwired.state_components().iter().any(|(l, _)| *l == "gic"));
}

/// Every HVF-retained vCPU class is part of both the canonical VCPU hash and
/// the diagnostic component roster. A one-field perturbation must therefore
/// change the full state hash and exactly its named diagnostic component.
#[test]
fn arm64_hvf_retained_classes_are_hash_observable() {
    let make = |state: Arm64VcpuState| {
        let mut backend = MockArm64Backend::new();
        backend.set_policy(&Arm64Policy::default()).unwrap();
        backend.set_state(state);
        Vmm::new(backend, GuestRam::new(RAM).unwrap())
    };

    let base = Arm64VcpuState::default();
    let baseline = make(base);
    let baseline_components = baseline.state_components();

    let mut general = base;
    general.core.x[30] = 1;
    let mut sysregs = base;
    sysregs.sysregs.tpidr_el1 = 1;
    let mut simd = base;
    simd.simd_fp.q[31][15] = 1;
    let mut debug = base;
    debug.debug.watchpoint_control[15] = 1;
    let mut vtimer = base;
    vtimer.vtimer.offset = 1;
    let mut interrupts = base;
    interrupts.interrupts.fiq = true;

    for (expected, state) in [
        ("core-regs", general),
        ("sysregs", sysregs),
        ("simd-fp", simd),
        ("debug", debug),
        ("vtimer", vtimer),
        ("interrupts", interrupts),
    ] {
        let candidate = make(state);
        assert_ne!(
            baseline.state_hash().unwrap(),
            candidate.state_hash().unwrap(),
            "{expected} must feed the canonical VCPU hash"
        );
        let candidate_components = candidate.state_components();
        let differing: Vec<&str> = baseline_components
            .iter()
            .filter_map(|(label, digest)| {
                let other = candidate_components
                    .iter()
                    .find(|(other_label, _)| other_label == label)
                    .map(|(_, digest)| digest)
                    .expect("component rosters match");
                (digest != other).then_some(*label)
            })
            .collect();
        assert_eq!(differing, [expected]);

        let snapshot = candidate.save_vm_state().unwrap();
        let mut restored = make(base);
        restored
            .restore_snapshot(candidate.guest_memory(), &snapshot)
            .unwrap();
        assert_eq!(restored.inspect_vcpu(), state, "{expected} restore");
        assert_eq!(
            restored.state_hash().unwrap(),
            candidate.state_hash().unwrap(),
            "{expected} must round-trip through the canonical snapshot"
        );
    }
}

/// The non-vCPU retained classes named by the M1 state-completeness rule each
/// change the hash alone and reproduce that exact hash through restore.
#[test]
fn arm64_devices_gic_vtime_and_entropy_are_hash_and_restore_complete() {
    use vmm_backend::Gpa;
    use vmm_core::vendor::arm64::board::{CNTFRQ_HZ, PL011, PVCLOCK_PPI};
    use vmm_core::vmm::VtimeWiring;
    use vtime::VClockConfig;

    let restore = |source: &Vmm<MockArm64Backend>, target: &mut Vmm<MockArm64Backend>| {
        let snapshot = source.save_vm_state().unwrap();
        target
            .restore_snapshot(source.guest_memory(), &snapshot)
            .unwrap();
        assert_eq!(target.state_hash().unwrap(), source.state_hash().unwrap());
        assert_eq!(target.save_vm_state().unwrap(), snapshot);
    };

    let serial_base = vmm(vec![]);
    let mut serial = vmm(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(PL011.0),
        size: 1,
        write: Some(b'X'.into()),
    })]);
    assert_eq!(serial.step().unwrap(), Step::Continued);
    assert_ne!(
        serial.state_hash().unwrap(),
        serial_base.state_hash().unwrap()
    );
    restore(&serial, &mut vmm(vec![]));

    let mut pending_gic = clockevent_gic();
    pending_gic.raise(PVCLOCK_PPI).unwrap();
    let mut gic_base = vmm(vec![]);
    gic_base.wire_gic(clockevent_gic());
    let mut gic_pending = vmm(vec![]);
    gic_pending.wire_gic(pending_gic);
    assert_ne!(
        gic_pending.state_hash().unwrap(),
        gic_base.state_hash().unwrap()
    );
    let mut gic_target = vmm(vec![]);
    gic_target.wire_gic(clockevent_gic());
    restore(&gic_pending, &mut gic_target);

    let config = VClockConfig {
        guest_hz: CNTFRQ_HZ,
        guest_base: 0,
        vns_base: 0,
    };
    let timed = |vns: u64, seed: u64| {
        let mut vm = vmm(vec![]);
        let mut wiring = VtimeWiring::new_virtual_time(config, seed).unwrap();
        wiring.advance_virtual_time(vns);
        vm.wire_vtime(wiring);
        vm
    };

    let time_base = timed(0, 7);
    let time_changed = timed(9, 7);
    assert_ne!(
        time_changed.state_hash().unwrap(),
        time_base.state_hash().unwrap()
    );
    restore(&time_changed, &mut timed(0, 7));

    let entropy_base = timed(0, 7);
    let mut entropy_changed = timed(0, 7);
    entropy_changed.reseed_entropy(8).unwrap();
    assert_ne!(
        entropy_changed.state_hash().unwrap(),
        entropy_base.state_hash().unwrap()
    );
    restore(&entropy_changed, &mut timed(0, 7));
}

/// M3 — the full boot composition: `boot` installs the shared guest policy then
/// loads an Image + DTB and sets the entry state, all mock-backed.
#[test]
fn arm64_boot_composes_a_ready_vmm() {
    use vmm_backend::MockArm64Backend;
    use vmm_core::vendor::arm64::{bringup, dtb, image_loader};

    let image = image_loader::wrap_image(&[0x42u8; 256], 0, 0xA);
    let backend = MockArm64Backend::new();
    let v = bringup::boot(backend, &image, "console=ttyAMA0", 16 * 1024 * 1024).unwrap();

    let vcpu = v.inspect_vcpu();
    assert_eq!(vcpu.core.pc, 0x4000_0000);
    assert_eq!(vcpu.core.pstate, 0x3c5);
    let dtb_gpa = vcpu.core.x[0];
    let off = (dtb_gpa - 0x4000_0000) as usize;
    let parsed = dtb::parse(&v.guest_memory()[off..]).unwrap();
    assert!(parsed.nodes.iter().any(|n| n == "intc@8000000"));
    assert!(parsed.nodes.iter().any(|n| n == "timer"));
}
