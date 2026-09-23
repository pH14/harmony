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
    const OFF_L3: usize = 0x3000;
    const OFF_PAGE_A: usize = 0x4000;
    const OFF_PAGE_B: usize = 0x5000;
    const OFF_L2_MMIO: usize = 0x6000;

    const PROBE_L2_INDEX: usize = 1;
    const MMIO_L2_INDEX: usize = 72;

    const VALUE_A: u64 = 0xaaaa;
    const VALUE_B: u64 = 0xbbbb;
    const ASID: u64 = 1;

    const SCTLR_M: u64 = 1;
    const SCTLR_C: u64 = 1 << 2;
    const SCTLR_I: u64 = 1 << 12;
    const PSTATE_EL1H_MASKED: u64 = 0x3c5;

    const DESC_TABLE: u64 = 0b11;
    const DESC_BLOCK: u64 = 0b01;
    const DESC_PAGE: u64 = 0b11;
    const ATTR_NORMAL: u64 = (1 << 10) | (3 << 8);
    const ATTR_DEVICE: u64 = (1 << 54) | (1 << 53) | (1 << 10) | (1 << 2);

    const MAIR: u64 = 0xff;
    const TCR: u64 =
        25 | (1 << 8) | (1 << 10) | (3 << 12) | (25 << 16) | (1 << 23) | (2 << 30) | (2 << 32);

    const WARM_ROUNDS: usize = 64;

    const READ_PROBE: [u32; 5] = [
        0xd2a0_0000 | (0x4020 << 5) | 1,
        0xd2a0_0000 | (0x0900 << 5) | 2,
        0xf940_0020,
        0xf900_0040,
        0x1400_0000,
    ];

    const FLUSH_THEN_READ_PROBE: [u32; 8] = [
        0xd2a0_0000 | (0x4020 << 5) | 1,
        0xd2a0_0000 | (0x0900 << 5) | 2,
        0xd508_831f,
        0xd503_3b9f,
        0xd503_3fdf,
        0xf940_0020,
        0xf900_0040,
        0x1400_0000,
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
        write_u64(
            ram,
            OFF_L2_RAM + PROBE_L2_INDEX * 8,
            (RAM_GPA + OFF_L3 as u64) | DESC_TABLE,
        );
        write_u64(ram, OFF_PAGE_A, VALUE_A);
        write_u64(ram, OFF_PAGE_B, VALUE_B);
    }

    pub struct Probe {
        backend: HvfBackend,
        ram: &'static mut [u8],
        ram_ptr: *mut u8,
        state: Arm64VcpuState,
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
            write_words(ram, OFF_CODE, &READ_PROBE);

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

            let mut state = backend.save().map_err(|error| format!("save: {error:?}"))?;
            state.core.pc = RAM_GPA + OFF_CODE as u64;
            state.core.pstate = PSTATE_EL1H_MASKED;
            state.sysregs.sctlr_el1 |= SCTLR_M | SCTLR_C | SCTLR_I;
            state.sysregs.ttbr0_el1 = (ASID << 48) | (RAM_GPA + OFF_L1 as u64);
            state.sysregs.tcr_el1 = TCR;
            state.sysregs.mair_el1 = MAIR;

            // SAFETY: the allocation outlives the probe and this slice is the
            // only reference to it.
            let ram = unsafe { std::slice::from_raw_parts_mut(ram_ptr, RAM_BYTES) };
            Ok(Self {
                backend,
                ram,
                ram_ptr,
                state,
            })
        }

        fn point_probe_page(&mut self, page_offset: usize) {
            write_u64(
                self.ram,
                OFF_L3,
                (RAM_GPA + page_offset as u64) | ATTR_NORMAL | DESC_PAGE,
            );
        }

        fn load_program(&mut self, words: &[u32]) {
            write_words(self.ram, OFF_CODE, words);
            self.backend
                .invalidate_instruction_cache(self.ram_ptr as usize, RAM_BYTES);
        }

        fn run_once(&mut self) -> Result<u64, String> {
            self.backend
                .restore(&self.state)
                .map_err(|error| format!("restore: {error:?}"))?;
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

        fn warm_on_page_a(&mut self) -> Result<(), String> {
            self.point_probe_page(OFF_PAGE_A);
            self.load_program(&FLUSH_THEN_READ_PROBE);
            match self.run_once()? {
                VALUE_A => {}
                value => return Err(format!("clearing read {value:#x}")),
            }
            self.load_program(&READ_PROBE);
            for round in 0..WARM_ROUNDS {
                match self.run_once()? {
                    VALUE_A => {}
                    value => return Err(format!("warm round {round} read {value:#x}")),
                }
            }
            Ok(())
        }

        pub fn after_restore(&mut self) -> Result<u64, String> {
            self.warm_on_page_a()?;
            self.point_probe_page(OFF_PAGE_B);
            self.run_once()
        }

        pub fn after_guest_maintenance(&mut self) -> Result<u64, String> {
            self.warm_on_page_a()?;
            self.point_probe_page(OFF_PAGE_B);
            self.load_program(&FLUSH_THEN_READ_PROBE);
            self.run_once()
        }
    }

    pub fn run() -> Result<bool, String> {
        let mut probe = Probe::new()?;
        let restored = probe.after_restore()?;
        let maintained = probe.after_guest_maintenance()?;
        println!("HVF_TLB restored={restored:#x} expected={VALUE_B:#x}");
        println!("HVF_TLB maintained={maintained:#x} expected={VALUE_B:#x}");
        if maintained != VALUE_B {
            println!("HVF_TLB_UNOBSERVABLE the guest cannot see the replaced mapping at all");
            return Ok(false);
        }
        if restored != VALUE_B {
            println!("HVF_TLB_STALE guest translations survive a snapshot restore");
            return Ok(false);
        }
        println!("HVF_TLB_CLEAN a snapshot restore drops guest translations");
        Ok(true)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani)))]
fn main() -> std::process::ExitCode {
    match arm64::run() {
        Ok(true) => std::process::ExitCode::SUCCESS,
        Ok(false) => std::process::ExitCode::FAILURE,
        Err(detail) => {
            eprintln!("HVF_TLB_FAIL {detail}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri), not(kani))))]
fn main() {
    eprintln!("hvf_tlb_probe requires macOS on aarch64");
}
