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

    const BURN_RESTORES: usize = 400;

    const READ_COUNTER: [u32; 4] = [
        0xd2a0_0000 | (0x0900 << 5) | 1,
        0xd53b_e040,
        0xf900_0020,
        0x1400_0000 | (0x03ff_ffff & (-3i32 as u32)),
    ];

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
            write_words(ram, OFF_CODE, &READ_COUNTER);

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
            start.sysregs.ttbr0_el1 = (ASID << 48) | (RAM_GPA + OFF_L1 as u64);
            start.sysregs.tcr_el1 = TCR;
            start.sysregs.mair_el1 = MAIR;
            Ok(Self { backend, start })
        }

        fn restore_start(&mut self) -> Result<(), String> {
            let start = self.start;
            self.backend
                .restore(&start)
                .map_err(|error| format!("restore: {error:?}"))
        }

        fn read_counter(&mut self) -> Result<u64, String> {
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

        pub fn advance_across(&mut self, burn: usize) -> Result<u64, String> {
            self.restore_start()?;
            let first = self.read_counter()?;
            for _ in 0..burn {
                self.restore_start()?;
            }
            self.restore_start()?;
            let second = self.read_counter()?;
            Ok(second.wrapping_sub(first))
        }
    }

    pub fn run() -> Result<bool, String> {
        let mut probe = Probe::new()?;
        let idle = probe.advance_across(0)?;
        let burned = probe.advance_across(BURN_RESTORES)?;
        let scaled = probe.advance_across(BURN_RESTORES * 8)?;
        println!(
            "HVF_CNT advance_0={idle} advance_{BURN_RESTORES}={burned} advance_{}={scaled}",
            BURN_RESTORES * 8
        );
        if burned > idle.saturating_mul(4) && scaled > burned.saturating_mul(4) {
            println!(
                "HVF_CNT_HOST_TIME the guest counter advances with host work between restores \
                 of one snapshot, so a restore does not rewind it"
            );
            return Ok(false);
        }
        println!("HVF_CNT_REWOUND a restore puts the guest counter back");
        Ok(true)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
fn main() -> std::process::ExitCode {
    match arm64::run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(detail) => {
            eprintln!("HVF_CNT_FAIL {detail}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani))))]
fn main() {
    eprintln!("hvf_counter_probe requires macOS on aarch64");
}
