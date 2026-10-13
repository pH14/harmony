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
        let detour = guest.next_store();
        guest.restore(&cut);
        let reused = guest.next_store();
        drop(guest);
        let mut fresh = Guest::new(live_backend(), &program);
        fresh.restore(&cut);
        let cold = fresh.next_store();
        eprintln!(
            "{name}: uninterrupted={uninterrupted:#x} captured={original:#x} detour={detour:#x} reused={reused:#x} fresh={cold:#x}"
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
