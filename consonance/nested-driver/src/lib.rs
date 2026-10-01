// SPDX-License-Identifier: AGPL-3.0-or-later

use vmm_backend::{Backend, Gpa, X86, X86Policy};
use vmm_core::{
    vendor::x86::contract,
    vmm::{GuestRam, Step, Vmm, VmmError},
};

#[cfg(all(
    feature = "host-search",
    target_os = "linux",
    target_arch = "x86_64",
    not(miri)
))]
pub mod host;
pub mod operations;

pub const RAM_LEN: usize = 0x10000;
pub const STEPS: u64 = 12;
pub const PROGRAM: &[u8] = &[
    0x43, 0x01, 0x1e, 0x00, 0x30, 0x31, 0x1e, 0x00, 0x40, 0x03, 0x36, 0x00, 0x50, 0x89, 0x36, 0x00,
    0x50, 0x33, 0x3e, 0x00, 0x60, 0x01, 0xdf, 0x01, 0x3e, 0x00, 0x60, 0xba, 0xf8, 0x03, 0xb0, 0x5a,
    0xee, 0xeb, 0xdd,
];
const PAGES: [usize; 4] = [0x3000, 0x4000, 0x5000, 0x6000];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Oracle {
    pub registers: [u16; 3],
    pub memory: [u16; 4],
}

impl Default for Oracle {
    fn default() -> Self {
        Self {
            registers: [0x1234, 0x2345, 0x3456],
            memory: [0x1111, 0x2222, 0x3333, 0x4444],
        }
    }
}

impl Oracle {
    pub fn advance(&mut self) {
        self.registers[0] = self.registers[0].wrapping_add(1);
        self.memory[0] = self.memory[0].wrapping_add(self.registers[0]);
        self.memory[1] ^= self.registers[0];
        self.registers[1] = self.registers[1].wrapping_add(self.memory[2]);
        self.memory[2] = self.registers[1];
        self.registers[2] = (self.registers[2] ^ self.memory[3]).wrapping_add(self.registers[0]);
        self.memory[3] = self.memory[3].wrapping_add(self.registers[2]);
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.registers
            .iter()
            .chain(self.memory.iter())
            .flat_map(|word| word.to_le_bytes())
            .collect()
    }
}

pub fn compose<B: Backend<A = X86>>(backend: B) -> Result<Vmm<B>, VmmError> {
    compose_program(backend, PROGRAM)
}

fn compose_program<B: Backend<A = X86>>(
    mut backend: B,
    program: &[u8],
) -> Result<Vmm<B>, VmmError> {
    backend.set_policy(&X86Policy {
        cpuid: contract::cpuid_model(),
        msr_filter: contract::msr_filter_allow(),
    })?;
    let mut ram = GuestRam::new(RAM_LEN)?;
    ram.as_mut_bytes()[0x1000..0x1000 + program.len()].copy_from_slice(program);
    let oracle = Oracle::default();
    for (page, word) in PAGES.into_iter().zip(oracle.memory) {
        ram.as_mut_bytes()[page..page + 2].copy_from_slice(&word.to_le_bytes());
    }
    // SAFETY: Vmm owns the page-aligned fixed-address RAM until after its backend
    // is dropped. Its exclusive run loop prevents RAM access during guest entry.
    unsafe { backend.map_memory(Gpa(0), ram.as_mut_bytes())? };
    let mut state = backend.save()?;
    state.sregs.cs.base = 0;
    state.sregs.cs.selector = 0;
    state.regs.rip = 0x1000;
    state.regs.rsp = 0x7000;
    state.regs.rflags = 2;
    state.regs.rbx = u64::from(oracle.registers[0]);
    state.regs.rsi = u64::from(oracle.registers[1]);
    state.regs.rdi = u64::from(oracle.registers[2]);
    backend.restore(&state)?;
    Ok(Vmm::new(backend, ram))
}

pub fn observe<B: Backend<A = X86>>(vmm: &mut Vmm<B>) -> Result<Oracle, VmmError> {
    let state = vmm.vcpu_record()?;
    let memory = PAGES
        .map(|page| u16::from_le_bytes(vmm.guest_memory()[page..page + 2].try_into().unwrap()));
    Ok(Oracle {
        registers: [
            state.regs.rbx as u16,
            state.regs.rsi as u16,
            state.regs.rdi as u16,
        ],
        memory,
    })
}

pub fn step<B: Backend<A = X86>>(vmm: &mut Vmm<B>) -> Result<Oracle, VmmError> {
    let before = vmm.serial().len();
    for _ in 0..64 {
        let result = vmm.step()?;
        if vmm.serial().len() == before + 1 && vmm.serial()[before] == 0x5a {
            vmm.prepare_snapshot()?;
            return observe(vmm);
        }
        if result != Step::Continued {
            return Err(VmmError::ContractViolation(format!(
                "L2 stopped: {result:?}"
            )));
        }
    }
    Err(VmmError::ContractViolation(
        "L2 missed its port boundary".into(),
    ))
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vmm_backend::MockBackend;

    #[test]
    fn composition_owns_the_loaded_mapping() {
        let vmm = compose(MockBackend::new()).unwrap();
        assert_eq!(&vmm.guest_memory()[0x1000..0x1000 + PROGRAM.len()], PROGRAM);
        for (page, word) in PAGES.into_iter().zip(Oracle::default().memory) {
            assert_eq!(&vmm.guest_memory()[page..page + 2], &word.to_le_bytes());
        }
    }

    #[test]
    fn one_step_matches_independent_hand_values_and_reads_previous_state() {
        let mut oracle = Oracle::default();
        oracle.advance();
        assert_eq!(oracle.registers, [0x1235, 0x5678, 0x8247]);
        assert_eq!(oracle.memory, [0x2346, 0x3017, 0x5678, 0xc68b]);
        let mut changed = Oracle::default();
        changed.memory[3] ^= 1;
        changed.advance();
        assert_ne!(oracle.bytes(), changed.bytes());
    }
}
