// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(feature = "mock")]

use vmm_backend::{Arm64, Arm64Policy, Arm64VcpuState, Backend, CommonExit, Exit, Gpa};

const RAM: u64 = 0x4000_0000;
const OUTPUT: u64 = 0x0900_0000;

#[repr(C, align(16384))]
struct GuestMemory([u8; 16384]);

struct Guest<B: Backend<A = Arm64>> {
    backend: B,
    _memory: Box<GuestMemory>,
}

impl<B: Backend<A = Arm64>> Guest<B> {
    fn new(mut backend: B, program: &[u32]) -> Self {
        let mut memory = Box::new(GuestMemory([0; 16384]));
        for (slot, word) in memory.0.chunks_exact_mut(4).zip(program) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
        backend.set_policy(&Arm64Policy::default()).unwrap();
        // SAFETY: the boxed, page-aligned allocation stays at the same address and
        // outlives the backend, which is the first field dropped. The host only
        // accesses its bytes while the backend is stopped.
        unsafe { backend.map_memory(Gpa(RAM), &mut memory.0) }.unwrap();
        backend.invalidate_instruction_cache(memory.0.as_ptr() as usize, memory.0.len());
        let mut initial = backend.save().unwrap();
        initial.core.pc = RAM;
        initial.core.pstate = 0x3c5;
        initial.sysregs.sctlr_el1 &= !1;
        backend.restore(&initial).unwrap();
        Self {
            backend,
            _memory: memory,
        }
    }

    fn next_store(&mut self) -> u64 {
        match self.backend.run().unwrap() {
            Exit::Common(CommonExit::Mmio {
                gpa: Gpa(OUTPUT),
                write: Some(value),
                ..
            }) => value,
            other => panic!("expected an observation, got {other:?}"),
        }
    }

    fn save(&mut self) -> Arm64VcpuState {
        self.backend.save().unwrap()
    }

    fn restore(&mut self, state: &Arm64VcpuState) {
        self.backend.restore(state).unwrap();
    }
}

#[test]
fn aligned_guest_mapping_survives_capture_and_restore() {
    let backend = vmm_backend::MockArm64Backend::with_exits(vec![Exit::Common(CommonExit::Mmio {
        gpa: Gpa(OUTPUT),
        size: 8,
        write: Some(7),
    })]);
    let mut guest = Guest::new(backend, &[0x1400_0000]);
    let state = guest.save();
    assert_eq!(state.core.pc, RAM);
    assert_eq!(guest._memory.0.as_ptr() as usize % 16384, 0);
    assert_eq!(&guest._memory.0[..4], &0x1400_0000u32.to_le_bytes());
    guest.restore(&state);
    assert_eq!(guest.next_store(), 7);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn live_backend() -> impl Backend<A = Arm64> {
    vmm_backend::HvfBackend::new().unwrap()
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn live_backend() -> impl Backend<A = Arm64> {
    vmm_backend::Arm64KvmBackend::new(vmm_backend::LiveKvm::new().unwrap())
}

#[test]
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "macos", target_os = "linux"),
    not(miri)
))]
#[ignore = "live ARM hypervisor; registered in HOST_TESTS"]
fn guest_system_registers_survive_reused_and_fresh_restore() {
    let mut failures = Vec::new();
    for (name, msr, mrs) in [
        ("CONTEXTIDR_EL1", 0xd518_d020, 0xd538_d021),
        ("AMAIR_EL1", 0xd518_a300, 0xd538_a301),
        ("PAR_EL1", 0xd518_7400, 0xd538_7401),
        ("CSSELR_EL1", 0xd51a_0000, 0xd53a_0001),
        ("AFSR0_EL1", 0xd518_5100, 0xd538_5101),
        ("AFSR1_EL1", 0xd518_5120, 0xd538_5121),
    ] {
        let movz = |r: u32, imm: u32| 0xd280_0000 | (imm << 5) | r;
        let program = [
            0xd2a1_2009,
            movz(0, 0x1234),
            msr,
            0xd503_3fdf,
            0xf900_0120,
            mrs,
            0xf900_0121,
            movz(0, 0x5678),
            msr,
            0xd503_3fdf,
            mrs,
            0xf900_0121,
            mrs,
            0xf900_0121,
            0x1400_0000,
        ];
        let mut reference = Guest::new(live_backend(), &program);
        assert_eq!(reference.next_store(), 0x1234);
        let uninterrupted = reference.next_store();
        drop(reference);
        let mut guest = Guest::new(live_backend(), &program);
        assert_eq!(guest.next_store(), 0x1234);
        let cut = guest.save();
        let original = guest.next_store();
        let immediate_detour = guest.next_store();
        let detour = guest.next_store();
        let observable = original != detour;
        if matches!(name, "CONTEXTIDR_EL1" | "PAR_EL1" | "CSSELR_EL1") {
            assert!(
                observable,
                "{name} detour must change the guest's next read"
            );
        }
        guest.restore(&cut);
        let reused = guest.next_store();
        drop(guest);
        let mut fresh = Guest::new(live_backend(), &program);
        fresh.restore(&cut);
        let cold = fresh.next_store();
        let sensitivity = if observable {
            "observable"
        } else {
            "not observable on this host"
        };
        eprintln!(
            "{name}: {sensitivity}; uninterrupted={uninterrupted:#x} captured={original:#x} immediate-detour={immediate_detour:#x} detour-after-exit={detour:#x} reused={reused:#x} fresh={cold:#x}"
        );
        if uninterrupted != original || original != reused || original != cold {
            failures.push((name, uninterrupted, original, reused, cold));
        }
    }
    assert!(
        failures.is_empty(),
        "system register continuations differ: {failures:?}"
    );
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_mdccint_survives_fresh_restore() {
    let program = [
        0xd2a8_0000,
        0xd2a1_2001,
        0xd510_0200,
        0xd280_0020,
        0xf900_0020,
        0xd530_0200,
        0xf900_0020,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &program);
    assert_eq!(source.next_store(), 1);
    let saved = source.save();
    assert_eq!(saved.debug.mdccint_el1, 1 << 30);
    assert_eq!(source.next_store(), 1 << 30);
    drop(source);

    let mut restored = Guest::new(live_backend(), &program);
    restored.restore(&saved);
    assert_eq!(restored.next_store(), 1 << 30);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_gic_cpu_interface_survives_fresh_restore() {
    for (name, msr, mrs, value) in [
        ("BPR0", 0xd518_c860, 0xd538_c860, 7),
        ("BPR1", 0xd518_cc60, 0xd538_cc60, 7),
        ("AP0R0", 0xd518_c880, 0xd538_c880, 1),
        ("AP1R0", 0xd518_c900, 0xd538_c900, 1),
        ("CTLR", 0xd518_cc80, 0xd538_cc80, 2),
        ("IGRPEN0", 0xd518_ccc0, 0xd538_ccc0, 1),
    ] {
        let program = [
            0xd2a1_2004,
            0xd280_0000 | (value << 5),
            msr,
            0xd503_3fdf,
            mrs,
            0xf900_0080,
            mrs,
            0xf900_0080,
            0x1400_0000,
        ];
        let mut source = Guest::new(live_backend(), &program);
        let first = source.next_store();
        let cut = source.save();
        let uninterrupted = source.next_store();
        drop(source);
        let mut target = Guest::new(live_backend(), &program);
        target.restore(&cut);
        let resumed = target.next_store();
        assert_eq!(first, uninterrupted, "{name} uninterrupted");
        assert_eq!(first, resumed, "{name} fresh restore");
    }

    let cbpr_program = [
        0xd2a1_2004,
        0xd280_0020,
        0xd518_cc80,
        0xd503_3fdf,
        0xd538_cc80,
        0xf900_0080,
        0x1400_0000,
    ];
    let mut guest = Guest::new(live_backend(), &cbpr_program);
    assert_ne!(guest.next_store() & 1, 0);
    assert!(matches!(
        guest.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { .. })
    ));
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_gic_spi_trigger_survives_fresh_restore() {
    let movz = |r: u32, imm: u32, shift: u32| 0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    let movk = |r: u32, imm: u32, shift: u32| 0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    let program = [
        movz(1, 0x0800, 16),
        movk(1, 0x0c08, 0),
        movz(4, 0x0900, 16),
        0xb940_0020,
        0xf900_0080,
        movz(0, 2, 16),
        0xb900_0020,
        0xb940_0020,
        0xf900_0080,
        0xb940_0020,
        0xf900_0080,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &program);
    let initial = source.next_store();
    assert_eq!(initial, 0);
    let first = source.next_store();
    let cut = source.save();
    assert_eq!(cut.gic.as_ref().unwrap().config[2] as u64, first);
    let uninterrupted = source.next_store();
    drop(source);
    let mut target = Guest::new(live_backend(), &program);
    let fresh = target.save().gic.unwrap().config[2];
    assert_eq!(fresh, 0);
    target.restore(&cut);
    assert_eq!(target.save().gic.as_ref().unwrap().config[2] as u64, first);
    let resumed = target.next_store();
    eprintln!(
        "GICD_ICFGR2 initial={initial:#x} fresh={fresh:#x} programmed={first:#x} uninterrupted={uninterrupted:#x} restored={resumed:#x}"
    );
    assert_eq!(first, 0x20000);
    assert_eq!(uninterrupted, first);
    assert_eq!(resumed, first);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_gicr_bases_survive_reused_and_fresh_restore() {
    let movz = |r: u32, imm: u32, shift: u32| 0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    let movk = |r: u32, imm: u32, shift: u32| 0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    for (name, offset) in [("PROPBASER", 0x70), ("PENDBASER", 0x78)] {
        let program = [
            movz(1, 0x080a, 16),
            movk(1, offset, 0),
            movz(4, 0x0900, 16),
            movz(0, 0x1234, 16),
            movk(0, 0x11, 32),
            0xf900_0020,
            0xf940_0020,
            0xf900_0080,
            0xf940_0020,
            0xf900_0080,
            movz(0, 0x5678, 16),
            movk(0, 0x22, 32),
            0xf900_0020,
            0xf940_0020,
            0xf900_0080,
            0x1400_0000,
        ];
        let mut source = Guest::new(live_backend(), &program);
        let first = source.next_store();
        let cut = source.save();
        let saved_base = if name == "PROPBASER" {
            cut.gic.as_ref().unwrap().gicr_propbaser
        } else {
            cut.gic.as_ref().unwrap().gicr_pendbaser
        };
        assert_eq!(saved_base, first);
        let uninterrupted = source.next_store();
        let detour = source.next_store();
        assert_ne!(detour, first);
        source.restore(&cut);
        let mut reused_state = source.save();
        reused_state.vtimer.counter = cut.vtimer.counter;
        assert_eq!(reused_state, cut);
        let reused = source.next_store();

        let mut fresh = Guest::new(live_backend(), &program);
        fresh.restore(&cut);
        let mut fresh_state = fresh.save();
        fresh_state.vtimer.counter = cut.vtimer.counter;
        assert_eq!(fresh_state, cut);
        let cold = fresh.next_store();
        eprintln!(
            "GICR_{name} saved={saved_base:#x} uninterrupted={uninterrupted:#x} detour={detour:#x} reused={reused:#x} fresh={cold:#x}"
        );
        assert_eq!(uninterrupted, first);
        assert_eq!(reused, first);
        assert_eq!(cold, first);
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_nonzero_spi_route_rejects_capture_and_zero_route_restores() {
    let movz = |r: u32, imm: u32, shift: u32| 0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    let movk = |r: u32, imm: u32, shift: u32| 0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | r;
    let program = |route| {
        [
            movz(1, 0x0800, 16),
            movk(1, 0x6100, 0),
            movz(4, 0x0900, 16),
            movz(0, route, 0),
            0xf900_0020,
            0xf940_0020,
            0xf900_0080,
            0xf940_0020,
            0xf900_0080,
            0x1400_0000,
        ]
    };
    let mut source = Guest::new(live_backend(), &program(0));
    assert_eq!(source.next_store(), 0);
    let cut = source.save();
    assert_eq!(source.next_store(), 0);
    drop(source);

    let mut reused = Guest::new(live_backend(), &program(1));
    let guest_route = reused.next_store();
    assert_eq!(guest_route, 1);
    assert!(matches!(
        reused.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { .. })
    ));
    reused.restore(&cut);
    let restored = reused.next_store();
    assert_eq!(restored, 0);

    let mut fresh = Guest::new(live_backend(), &program(1));
    fresh.restore(&cut);
    let cold = fresh.next_store();
    assert_eq!(cold, 0);
    eprintln!(
        "GICD_IROUTER32 guest={guest_route:#x} capture=rejected reused={restored:#x} fresh={cold:#x}"
    );
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_physical_timer_state_refuses_capture_and_legacy_restore() {
    let enabled = [
        0xd280_0020,
        0xd51b_e200,
        0xd51b_e220,
        0xd503_3fdf,
        0xd53b_e220,
        0xd2a1_2001,
        0xf900_0020,
        0xd53b_e220,
        0xf900_0020,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &enabled);
    assert_eq!(source.next_store(), 5);
    assert!(matches!(
        source.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { .. })
    ));
    assert_eq!(source.next_store(), 5);

    let disabled_with_compare = [
        0xd280_0020,
        0xd51b_e200,
        0xd53b_e240,
        0xd2a1_2001,
        0xf900_0020,
        0x1400_0000,
    ];
    let mut disabled = Guest::new(live_backend(), &disabled_with_compare);
    assert_ne!(disabled.next_store(), 0);
    assert!(matches!(
        disabled.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { .. })
    ));

    let pristine = [0xd53b_e220, 0xd2a1_2001, 0xf900_0020, 0x1400_0000];
    let mut target = Guest::new(live_backend(), &pristine);
    let pristine_cut = target.save();
    assert_eq!(target.next_store(), 0);
    let valid = target.save();
    let mut legacy = valid;
    legacy.gic.as_mut().unwrap().version = 5;
    assert!(matches!(
        target.backend.restore(&legacy),
        Err(vmm_backend::BackendError::InvalidState)
    ));
    let after = target.save();
    assert_eq!(after.core, valid.core);
    assert_eq!(after.gic, valid.gic);
    for intid in [20, 30] {
        assert!(matches!(
            target
                .backend
                .set_pending_irq(Some(vmm_backend::GicIntId(intid))),
            Err(vmm_backend::BackendError::InvalidState)
        ));
        assert_eq!(target.save().gic, valid.gic);
    }

    source.restore(&pristine_cut);
    for (slot, word) in source._memory.0.chunks_exact_mut(4).zip(pristine) {
        slot.copy_from_slice(&word.to_le_bytes());
    }
    source
        .backend
        .invalidate_instruction_cache(source._memory.0.as_ptr() as usize, source._memory.0.len());
    assert_eq!(source.next_store(), 0);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_timer_owned_ppi_line_refuses_terminal_cut_capture_and_restore() {
    let program = [
        0xd280_0020,
        0xd51b_e200,
        0xd51b_e220,
        0xd503_3fdf,
        0xd53b_e220,
        0xd2a1_2001,
        0xb900_0020,
        0xd280_0000,
        0xd51b_e220,
        0xd51b_e240,
        0xd503_3fdf,
        0xd280_0100,
        0xf2b0_8000,
        0xd400_0002,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &program);
    assert_eq!(source.next_store(), 5);
    assert_eq!(source.backend.run().unwrap(), CommonExit::Shutdown.into());
    assert!(matches!(
        source.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { what })
            if what == "save vGIC with timer-owned PPI delivery state"
    ));

    let mut target = Guest::new(live_backend(), &[0x1400_0000]);
    let valid = target.save();
    for intid in [20, 30] {
        let bit = 1u32 << intid;
        for field in [0, 1, 2] {
            let mut malformed = valid;
            let gic = malformed.gic.as_mut().unwrap();
            match field {
                0 => gic.pending[0] = bit,
                1 => gic.active[0] = bit,
                _ => gic.line_level[0] = bit,
            }
            assert!(matches!(
                target.backend.restore(&malformed),
                Err(vmm_backend::BackendError::InvalidState)
            ));
            assert_eq!(target.save().gic, valid.gic);
        }
    }
    target
        .backend
        .set_pending_irq(Some(vmm_backend::GicIntId(29)))
        .unwrap();
    assert_eq!(target.save().gic.unwrap().line_level[0], 1 << 29);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_disr_state_refuses_capture_and_legacy_restore() {
    let program = [
        0xd282_4680,
        0xd518_c120,
        0xd503_3fdf,
        0xd538_c120,
        0xd2a1_2001,
        0xf900_0020,
        0xd538_c120,
        0xf900_0020,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &program);
    let mut clean = source.save();
    clean.core.pc = RAM + 24;
    clean.core.x[1] = OUTPUT;
    assert_eq!(source.next_store(), 0x1234);
    assert!(matches!(
        source.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { what })
            if what == "save ARM KVM with noncanonical DISR_EL1"
    ));

    let mut legacy = clean;
    legacy.gic.as_mut().unwrap().version = 6;
    assert!(matches!(
        source.backend.restore(&legacy),
        Err(vmm_backend::BackendError::InvalidState)
    ));
    assert_eq!(source.next_store(), 0x1234);

    source.restore(&clean);
    assert_eq!(source.next_store(), 0);
    let mut cold = Guest::new(live_backend(), &program);
    cold.restore(&clean);
    assert_eq!(cold.next_store(), 0);
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn guest_os_lock_refuses_capture_and_clean_restore_unlocks() {
    let program = [
        0xd280_0020,
        0xd510_1080,
        0xd503_3fdf,
        0xd530_1180,
        0xd2a1_2001,
        0xf900_0020,
        0xd530_1180,
        0xf900_0020,
        0x17ff_fffe,
    ];
    let mut source = Guest::new(live_backend(), &program);
    let mut clean = source.save();
    clean.core.pc = RAM + 24;
    clean.core.x[1] = OUTPUT;
    let locked = source.next_store();
    assert_eq!(locked & 2, 2, "guest OSLAR write must set OSLSR.OSLK");
    assert!(matches!(
        source.backend.save(),
        Err(vmm_backend::BackendError::Unsupported {
            what: "save ARM KVM with debug OS lock set"
        })
    ));
    assert_eq!(source.next_store(), locked, "capture refusal is neutral");
    let mut legacy = clean;
    legacy.gic.as_mut().unwrap().version = 8;
    assert!(matches!(
        source.backend.restore(&legacy),
        Err(vmm_backend::BackendError::InvalidState)
    ));
    assert_eq!(
        source.next_store(),
        locked,
        "old restore refusal is neutral"
    );
    source.restore(&clean);
    let unlocked = source.next_store();
    assert_eq!(unlocked & 2, 0, "reused restore must clear OSLSR.OSLK");
    let continued = source.save();
    assert_eq!(source.next_store(), unlocked, "unlocked capture is neutral");
    source.restore(&continued);
    assert_eq!(source.next_store(), unlocked);
    drop(source);

    let mut fresh = Guest::new(live_backend(), &program);
    fresh.restore(&clean);
    assert_eq!(fresh.next_store(), unlocked, "fresh restore");
    drop(fresh);

    let mut policy = Guest::new(live_backend(), &program);
    assert_eq!(policy.next_store(), locked);
    policy.backend.set_policy(&Arm64Policy::default()).unwrap();
    assert_eq!(
        policy.next_store(),
        unlocked,
        "policy must clear OSLSR.OSLK"
    );
    println!(
        "ARM_OS_LOCK locked={locked:#x} capture=refused capture_continuation=pass unlocked={unlocked:#x} unlocked_capture=pass reused=pass fresh=pass policy=pass legacy=refused"
    );
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
#[ignore = "live HVF; registered in HOST_TESTS"]
fn guest_actlr_state_refuses_capture_and_clean_restore_resets_it() {
    let program = [
        0xd280_0040,
        0xd518_1020,
        0xd503_3fdf,
        0xd538_1020,
        0xd2a1_2001,
        0xf900_0020,
        0xd538_1020,
        0xf900_0020,
        0x1400_0000,
    ];
    let mut source = Guest::new(live_backend(), &program);
    let mut clean = source.save();
    clean.core.pc = RAM + 24;
    clean.core.x[1] = OUTPUT;
    assert_eq!(source.next_store(), 2);
    assert!(matches!(
        source.backend.save(),
        Err(vmm_backend::BackendError::Unsupported { what })
            if what == "save HVF with noncanonical ACTLR_EL1"
    ));
    assert_eq!(source.next_store(), 2);
    source.restore(&clean);
    assert_eq!(source.next_store(), 0);
    drop(source);

    let mut cold = Guest::new(live_backend(), &program);
    cold.restore(&clean);
    assert_eq!(cold.next_store(), 0);
    drop(cold);

    let mut independent = Guest::new(live_backend(), &program);
    assert_eq!(independent.next_store(), 2);
    independent.restore(&clean);
    assert_eq!(independent.next_store(), 0);
    drop(independent);

    let mut policy = Guest::new(live_backend(), &program);
    assert_eq!(policy.next_store(), 2);
    policy.backend.set_policy(&Arm64Policy::default()).unwrap();
    assert_eq!(policy.next_store(), 0);
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
#[ignore = "live HVF; registered in HOST_TESTS"]
fn guest_pauth_keys_refuse_capture_and_clean_restore_resets_them() {
    for (name, sysreg) in [
        ("APIAKeyLo", 0xc108u32),
        ("APIAKeyHi", 0xc109),
        ("APIBKeyLo", 0xc10a),
        ("APIBKeyHi", 0xc10b),
        ("APDAKeyLo", 0xc110),
        ("APDAKeyHi", 0xc111),
        ("APDBKeyLo", 0xc112),
        ("APDBKeyHi", 0xc113),
        ("APGAKeyLo", 0xc118),
        ("APGAKeyHi", 0xc119),
    ] {
        let program = [
            0xd282_4680,
            0xd500_0000 | sysreg << 5,
            0xd503_3fdf,
            0xd520_0000 | sysreg << 5,
            0xd2a1_2001,
            0xf900_0020,
            0xd520_0000 | sysreg << 5,
            0xf900_0020,
            0x1400_0000,
        ];
        let mut source = Guest::new(live_backend(), &program);
        let mut clean = source.save();
        clean.core.pc = RAM + 24;
        clean.core.x[1] = OUTPUT;
        assert_eq!(source.next_store(), 0x1234, "{name} first read");
        assert!(
            matches!(
                source.backend.save(),
                Err(vmm_backend::BackendError::Unsupported { what })
                    if what == "save HVF with noncanonical pointer-authentication key"
            ),
            "{name} capture"
        );
        assert_eq!(source.next_store(), 0x1234, "{name} uninterrupted read");
        source.restore(&clean);
        assert_eq!(source.next_store(), 0, "{name} reused restore");
        drop(source);

        let mut cold = Guest::new(live_backend(), &program);
        cold.restore(&clean);
        assert_eq!(cold.next_store(), 0, "{name} fresh restore");
        drop(cold);

        let mut policy = Guest::new(live_backend(), &program);
        assert_eq!(policy.next_store(), 0x1234, "{name} policy setup");
        policy.backend.set_policy(&Arm64Policy::default()).unwrap();
        assert_eq!(policy.next_store(), 0, "{name} policy reset");
    }
}

#[test]
#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
#[ignore = "live ARM KVM; registered in HOST_TESTS"]
fn kvm_irq_mask_read_matches_full_capture() {
    let mut guest = Guest::new(live_backend(), &[0x1400_0000]);
    let mut state = guest.save();
    assert!(state.core.pstate & (1 << 7) != 0);
    assert_eq!(guest.backend.read_irq_mask().unwrap(), Some(true));
    state.core.pstate &= !(1 << 7);
    guest.restore(&state);
    assert_eq!(guest.backend.read_irq_mask().unwrap(), Some(false));
    assert_eq!(guest.save().core.pstate & (1 << 7), 0);
}

#[test]
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "macos", target_os = "linux"),
    not(miri)
))]
#[ignore = "live ARM hypervisor; registered in HOST_TESTS"]
fn smccc_workaround_features_match_backend_levels() {
    let mut guest = Guest::new(live_backend(), &[0xd400_0002, 0xf900_0080, 0x1400_0000]);
    let levels = guest.backend.capabilities().arch.smccc_workaround_levels;
    for (index, feature) in [0x8000_8000, 0x8000_7fff, 0x8000_3fff]
        .into_iter()
        .enumerate()
    {
        let mut state = guest.save();
        state.core.pc = RAM;
        state.core.x[0] = 0x8000_0001;
        state.core.x[1] = feature;
        state.core.x[4] = OUTPUT;
        guest.restore(&state);
        let observed = guest.next_store();
        let expected = match (index, levels[index]) {
            (0 | 2, 0) => u64::MAX,
            (0 | 2, 1) => 0,
            (0 | 2, 2) => 1,
            (1, 0) => u64::MAX,
            (1, 3) => u64::MAX - 1,
            _ => panic!(
                "unknown workaround feature {feature:#x}, level {}, HVC answer {observed:#x}",
                levels[index]
            ),
        };
        assert_eq!(
            observed, expected,
            "feature {feature:#x}, level {}",
            levels[index]
        );
    }
}

#[test]
#[cfg(all(
    target_arch = "aarch64",
    any(target_os = "macos", target_os = "linux"),
    not(miri)
))]
#[ignore = "live ARM hypervisor; registered in HOST_TESTS"]
fn psci_profile_matches_guest_hvc() {
    use vmm_backend::Arm64PsciProfile;

    let mut guest = Guest::new(live_backend(), &[0xd400_0002, 0xf900_0080, 0x1400_0000]);
    let profile = guest.backend.capabilities().arch.psci_profile;
    for (name, function, argument, hvf, kvm) in [
        ("version", 0x8400_0000, 0, 0x1_0000, 0x1_0000),
        (
            "features CPU_SUSPEND32",
            0x8400_000a,
            0x8400_0001,
            u64::MAX,
            0,
        ),
        (
            "features CPU_SUSPEND64",
            0x8400_000a,
            0xc400_0001,
            u64::MAX,
            0,
        ),
        ("features CPU_OFF", 0x8400_000a, 0x8400_0002, u64::MAX, 0),
        ("features CPU_ON64", 0x8400_000a, 0xc400_0003, u64::MAX, 0),
        (
            "features AFFINITY_INFO64",
            0x8400_000a,
            0xc400_0004,
            u64::MAX,
            0,
        ),
        ("CPU_ON self", 0xc400_0003, 0, u64::MAX - 3, u64::MAX - 3),
        ("CPU_ON absent", 0xc400_0003, 1, u64::MAX - 3, u64::MAX - 1),
        ("AFFINITY_INFO self", 0xc400_0004, 0, u64::MAX - 6, 0),
        (
            "AFFINITY_INFO absent",
            0xc400_0004,
            1,
            u64::MAX - 6,
            u64::MAX - 1,
        ),
    ] {
        let mut state = guest.save();
        state.core.pc = RAM;
        state.core.x[0] = function;
        state.core.x[1] = argument;
        state.core.x[2] = 0;
        state.core.x[3] = 0;
        state.core.x[4] = OUTPUT;
        guest.restore(&state);
        let expected = match profile {
            Arm64PsciProfile::HvfSingleVcpu => hvf,
            Arm64PsciProfile::KvmPsci10 => kvm,
        };
        assert_eq!(guest.next_store(), expected, "{name}, profile {profile:?}");
    }
}
