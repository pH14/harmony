// SPDX-License-Identifier: AGPL-3.0-or-later

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
mod arm64 {
    use vmm_backend::{Arm64Policy, Arm64VcpuState, Backend, CommonExit, Exit, Gpa, HvfBackend};

    const RAM_GPA: u64 = 0x4000_0000;
    const RAM_BYTES: usize = 0x10_0000;
    const MMIO_GPA: u64 = 0x0900_0000;

    const OFF_CODE: usize = 0x0000;
    const OFF_L1: usize = 0x1000;
    const OFF_L2_RAM: usize = 0x2000;
    const OFF_L2_MMIO: usize = 0x6000;
    const OFF_SCRATCH: usize = 0x8000;

    const MMIO_L2_INDEX: usize = 72;
    const ASID: u64 = 1;

    const SCTLR_M: u64 = 1;
    const SCTLR_C: u64 = 1 << 2;
    const SCTLR_I: u64 = 1 << 12;
    const PSTATE_EL1H_MASKED: u64 = 0x3c5;

    const DESC_TABLE: u64 = 0b11;
    const DESC_BLOCK: u64 = 0b01;
    const ATTR_NORMAL: u64 = (1 << 10) | (3 << 8);
    const ATTR_DEVICE: u64 = (1 << 54) | (1 << 53) | (1 << 10) | (1 << 2);

    const MAIR: u64 = 0xff;
    const TCR: u64 =
        25 | (1 << 8) | (1 << 10) | (3 << 12) | (25 << 16) | (1 << 23) | (2 << 30) | (2 << 32);
    const CPACR_FPEN: u64 = 0b11 << 20;

    const ROUNDS: usize = 96;
    const BRANCH_AT: usize = 37;

    fn movz(rd: u32, imm: u32, shift: u32) -> u32 {
        0xd280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
    }
    fn movk(rd: u32, imm: u32, shift: u32) -> u32 {
        0xf280_0000 | ((shift / 16) << 21) | (imm << 5) | rd
    }
    fn add(rd: u32, rn: u32, rm: u32) -> u32 {
        0x8b00_0000 | (rm << 16) | (rn << 5) | rd
    }
    fn eor(rd: u32, rn: u32, rm: u32) -> u32 {
        0xca00_0000 | (rm << 16) | (rn << 5) | rd
    }
    fn mul(rd: u32, rn: u32, rm: u32) -> u32 {
        0x9b00_7c00 | (rm << 16) | (rn << 5) | rd
    }
    fn str_off(rt: u32, rn: u32) -> u32 {
        0xf900_0000 | (rn << 5) | rt
    }
    fn ldr_off(rt: u32, rn: u32) -> u32 {
        0xf940_0000 | (rn << 5) | rt
    }
    fn simd_add_2d(rd: u32, rn: u32, rm: u32) -> u32 {
        0x4ee0_8400 | (rm << 16) | (rn << 5) | rd
    }
    fn umov_low_doubleword(rd: u32, rn: u32) -> u32 {
        0x4e08_3c00 | (rn << 5) | rd
    }
    fn branch_back(words: usize) -> u32 {
        let offset = (-(words as i32)) as u32 & 0x03ff_ffff;
        0x1400_0000 | offset
    }

    fn program() -> Vec<u32> {
        let scratch = (RAM_GPA as usize + OFF_SCRATCH) as u64;
        let mut code = vec![
            movz(1, 0x9e37, 48),
            movk(1, 0x79b9, 32),
            movk(1, 0x7f4a, 16),
            movk(1, 0x7c15, 0),
            movz(2, (scratch >> 16) as u32 & 0xffff, 16),
            movk(2, scratch as u32 & 0xffff, 0),
            movz(9, (MMIO_GPA >> 16) as u32 & 0xffff, 16),
            movz(3, 0x1234, 0),
            movz(4, 0x5678, 0),
            movz(5, 0x9abc, 0),
            movz(6, 0x0001, 0),
            movz(8, 0x0000, 0),
        ];
        let loop_start = code.len();
        code.extend_from_slice(&[
            add(3, 3, 1),
            eor(4, 4, 3),
            mul(5, 5, 1),
            add(6, 6, 4),
            str_off(3, 2),
            ldr_off(7, 2),
            add(8, 8, 7),
            eor(8, 8, 5),
            simd_add_2d(0, 0, 1),
            umov_low_doubleword(10, 0),
            add(8, 8, 10),
            str_off(8, 9),
        ]);
        let body = code.len() - loop_start;
        code.push(branch_back(body));
        code
    }

    fn write_words(ram: &mut [u8], offset: usize, words: &[u32]) {
        for (slot, word) in words.iter().enumerate() {
            let at = offset + slot * 4;
            ram[at..at + 4].copy_from_slice(&word.to_le_bytes());
        }
    }

    fn write_u64(ram: &mut [u8], offset: usize, value: u64) {
        ram[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    fn build_tables(ram: &mut [u8]) {
        write_u64(ram, OFF_L1, (RAM_GPA + OFF_L2_MMIO as u64) | DESC_TABLE);
        write_u64(ram, OFF_L1 + 8, (RAM_GPA + OFF_L2_RAM as u64) | DESC_TABLE);
        write_u64(
            ram,
            OFF_L2_MMIO + MMIO_L2_INDEX * 8,
            MMIO_GPA | ATTR_DEVICE | DESC_BLOCK,
        );
        write_u64(ram, OFF_L2_RAM, RAM_GPA | ATTR_NORMAL | DESC_BLOCK);
    }

    pub struct Probe {
        backend: HvfBackend,
        start: Arm64VcpuState,
    }

    impl Probe {
        pub fn new() -> Result<Self, String> {
            let layout = std::alloc::Layout::from_size_align(RAM_BYTES, 16 * 1024)
                .map_err(|error| format!("layout: {error:?}"))?;
            // SAFETY: the layout has a non-zero size, so this is a valid
            // allocation request; the pointer is checked for null before use.
            let ram_ptr = unsafe { std::alloc::alloc_zeroed(layout) };
            if ram_ptr.is_null() {
                return Err("alloc returned null".to_string());
            }
            // SAFETY: the allocation is RAM_BYTES of readable, writable memory
            // that lives for the rest of the program and nothing else refers
            // to it.
            let ram = unsafe { std::slice::from_raw_parts_mut(ram_ptr, RAM_BYTES) };
            build_tables(ram);
            write_words(ram, OFF_CODE, &program());

            let mut backend = HvfBackend::new().map_err(|error| format!("create: {error:?}"))?;
            backend
                .set_policy(&Arm64Policy::default())
                .map_err(|error| format!("policy: {error:?}"))?;
            // SAFETY: the allocation stays live and unmoved for the rest of the
            // program, nothing else aliases it while the guest runs, and its
            // layout gives it the required 16 KiB alignment.
            unsafe { backend.map_memory(Gpa(RAM_GPA), ram) }
                .map_err(|error| format!("map: {error:?}"))?;
            backend.invalidate_instruction_cache(ram_ptr as usize, RAM_BYTES);

            let mut start = backend.save().map_err(|error| format!("save: {error:?}"))?;
            start.core.pc = RAM_GPA + OFF_CODE as u64;
            start.core.pstate = PSTATE_EL1H_MASKED;
            start.sysregs.sctlr_el1 |= SCTLR_M | SCTLR_C | SCTLR_I;
            start.sysregs.cpacr_el1 |= CPACR_FPEN;
            start.sysregs.ttbr0_el1 = (ASID << 48) | (RAM_GPA + OFF_L1 as u64);
            start.sysregs.tcr_el1 = TCR;
            start.sysregs.mair_el1 = MAIR;
            for (lane, slot) in start.simd_fp.q.iter_mut().enumerate() {
                slot[0] = lane as u8;
                slot[8] = (lane as u8) ^ 0x5a;
            }
            Ok(Self { backend, start })
        }

        fn step(&mut self) -> Result<u64, String> {
            match self.backend.run() {
                Ok(Exit::Common(CommonExit::Mmio {
                    gpa,
                    write: Some(value),
                    ..
                })) if gpa == Gpa(MMIO_GPA) => Ok(value),
                Ok(other) => Err(format!("unexpected exit {other:?}")),
                Err(error) => Err(format!("run: {error:?}")),
            }
        }

        pub fn straight(&mut self, rounds: usize) -> Result<Vec<u64>, String> {
            let start = self.start;
            self.backend
                .restore(&start)
                .map_err(|error| format!("restore: {error:?}"))?;
            (0..rounds).map(|_| self.step()).collect()
        }

        pub fn round_tripped(&mut self, rounds: usize) -> Result<Vec<u64>, String> {
            let start = self.start;
            self.backend
                .restore(&start)
                .map_err(|error| format!("restore: {error:?}"))?;
            let mut out = Vec::with_capacity(rounds);
            for _ in 0..rounds {
                out.push(self.step()?);
                let here = self
                    .backend
                    .save()
                    .map_err(|error| format!("save: {error:?}"))?;
                self.backend
                    .restore(&here)
                    .map_err(|error| format!("restore: {error:?}"))?;
            }
            Ok(out)
        }

        pub fn rebranched(&mut self, rounds: usize, at: usize) -> Result<Vec<u64>, String> {
            let start = self.start;
            self.backend
                .restore(&start)
                .map_err(|error| format!("restore: {error:?}"))?;
            for _ in 0..at {
                self.step()?;
            }
            let fork = self
                .backend
                .save()
                .map_err(|error| format!("save: {error:?}"))?;
            let first: Vec<u64> = (at..rounds)
                .map(|_| self.step())
                .collect::<Result<_, _>>()?;
            self.backend
                .restore(&fork)
                .map_err(|error| format!("restore: {error:?}"))?;
            let second: Vec<u64> = (at..rounds)
                .map(|_| self.step())
                .collect::<Result<_, _>>()?;
            if first != second {
                let index = first
                    .iter()
                    .zip(&second)
                    .position(|(a, b)| a != b)
                    .unwrap_or(0);
                return Err(format!(
                    "rebranch diverged at tail index {index}: {:#x} vs {:#x}",
                    first[index], second[index]
                ));
            }
            Ok(first)
        }
    }

    pub fn run() -> Result<bool, String> {
        let mut probe = Probe::new()?;
        let straight = probe.straight(ROUNDS)?;
        let tripped = probe.round_tripped(ROUNDS)?;
        let tail = probe.rebranched(ROUNDS, BRANCH_AT)?;
        println!(
            "HVF_RT straight={:#x} round_tripped={:#x} rebranch_tail={:#x}",
            straight[ROUNDS - 1],
            tripped[ROUNDS - 1],
            tail[tail.len() - 1]
        );
        if straight != tripped {
            let index = straight
                .iter()
                .zip(&tripped)
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            println!(
                "HVF_RT_LOSSY a save/restore between steps changed the guest at step {index}: \
                 {:#x} vs {:#x}",
                straight[index], tripped[index]
            );
            return Ok(false);
        }
        if straight[BRANCH_AT..] != tail[..] {
            println!("HVF_RT_LOSSY a rebranched tail differs from the straight run");
            return Ok(false);
        }
        println!("HVF_RT_CLEAN save and restore preserve what the guest computes");
        Ok(true)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
fn main() -> std::process::ExitCode {
    match arm64::run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(detail) => {
            eprintln!("HVF_RT_FAIL {detail}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani))))]
fn main() {
    eprintln!("hvf_roundtrip_probe requires macOS on aarch64");
}
