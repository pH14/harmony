// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]

use vmm_backend::{Backend, Gpa, HvfBackend};
use vmm_core::vendor::arm64::{board, contract};
use vmm_core::vmm::{GuestRam, Step, Vmm};

const RAM_BYTES: usize = 0x1_0000;
const PSTATE_EL1H_MASKED: u64 = 0x3c5;

fn movz(rd: u32, imm: u32, shift: u32) -> u32 {
    0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
}

fn guest() -> Vmm<HvfBackend> {
    let mut ram = GuestRam::new(RAM_BYTES).unwrap();
    let program = [
        movz(0, 0x4000, 16),
        movz(1, (board::PL011.0 >> 16) as u32, 16),
        0xd510_0200,
        movz(0, u32::from(b'A'), 0),
        0xb900_0020,
        0xd530_0200,
        0xd35e_fc00,
        0x1100_c000,
        0xb900_0020,
        0x1400_0000,
    ];
    for (index, word) in program.into_iter().enumerate() {
        ram.as_mut_bytes()[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    let mut backend = HvfBackend::new().unwrap();
    backend
        .set_policy(&contract::policy(backend.capabilities().arch.asid_bits))
        .unwrap();
    // SAFETY: the RAM backing stays pinned in the VMM until its backend is
    // dropped, and only the stopped guest or this owner accesses it at a time.
    unsafe { backend.map_memory(Gpa(board::RAM_BASE), ram.as_mut_bytes()) }.unwrap();
    backend.invalidate_instruction_cache(ram.as_bytes().as_ptr() as usize, ram.len());
    let mut entry = backend.save().unwrap();
    entry.core.pc = board::RAM_BASE;
    entry.core.pstate = PSTATE_EL1H_MASKED;
    backend.restore(&entry).unwrap();
    Vmm::new(backend, ram)
}

#[test]
#[ignore = "live Apple HVF guest; run with --ignored"]
fn guest_mdccint_el1_survives_a_cold_public_snapshot_restore() {
    let (memory, snapshot) = {
        let mut source = guest();
        assert_eq!(source.step().unwrap(), Step::Continued);
        assert_eq!(source.serial_output(), b"A");
        let snapshot = source.save_vm_state().unwrap();
        let memory = source.guest_memory().to_vec();
        assert_eq!(source.step().unwrap(), Step::Continued);
        assert_eq!(
            source.serial_output(),
            b"A1",
            "the guest must read back its own MDCCINT_EL1 write"
        );
        (memory, snapshot)
    };

    let mut restored = guest();
    restored.restore_snapshot(&memory, &snapshot).unwrap();
    assert_eq!(restored.serial_output(), b"A");
    assert_eq!(restored.step().unwrap(), Step::Continued);
    assert_eq!(
        restored.serial_output(),
        b"A1",
        "cold restore lost guest MDCCINT_EL1 state"
    );
}
