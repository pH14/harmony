// SPDX-License-Identifier: AGPL-3.0-or-later
//! Print the stock KVM/arm64 guest identity-register surface before policy
//! installation. M5 uses this beside `hvf_probe` to derive the conservative
//! cross-host register intersection from measured values.

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::alloc::{Layout, alloc_zeroed, dealloc};
    use std::ptr::NonNull;

    use vmm_backend::{Arm64Kvm, LiveKvm};

    const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
    const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
    const KVM_REG_ARM_CORE: u64 = 0x0010_0000;
    const KVM_REG_ARM64_SYSREG: u64 = 0x0013_0000;
    const KVM_REG_ARM_FW: u64 = 0x0014_0000;
    const PAGE_SIZE: usize = 4096;

    const fn core_reg(index: u64) -> u64 {
        KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_CORE | index
    }

    struct ProbePage {
        ptr: NonNull<u8>,
        layout: Layout,
    }

    impl ProbePage {
        fn new() -> Option<Self> {
            let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).ok()?;
            // SAFETY: the non-zero layout is valid and retained until `Drop`.
            let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(Self { ptr, layout })
        }

        fn put(&mut self, offset: usize, instruction: u32) {
            // SAFETY: both callers use aligned offsets whose four-byte ranges
            // lie within this exclusively owned page before it is registered.
            unsafe {
                self.ptr
                    .as_ptr()
                    .add(offset)
                    .cast::<u32>()
                    .write(instruction)
            };
        }
    }

    impl Drop for ProbePage {
        fn drop(&mut self) {
            // SAFETY: this pointer was allocated with exactly this layout and
            // the KVM object (declared later) has already released its memslot.
            unsafe { dealloc(self.ptr.as_ptr(), self.layout) };
        }
    }

    let mut page = match ProbePage::new() {
        Some(page) => page,
        None => {
            eprintln!("cannot allocate aligned guest probe page");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut kvm = match LiveKvm::new() {
        Ok(kvm) => kvm,
        Err(error) => {
            eprintln!("cannot create initialized KVM/arm64 vCPU: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("KVM arm64 identity probe v1");
    for (name, index) in [
        ("SMCCC_ARCH_WORKAROUND_1", 1u64),
        ("SMCCC_ARCH_WORKAROUND_2", 2),
        ("SMCCC_ARCH_WORKAROUND_3", 3),
    ] {
        let id = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM_FW | index;
        match kvm.get_one_reg(id) {
            Ok(value) => println!("firmware.{name}: {value:#x}"),
            Err(error) => {
                eprintln!("cannot read {name}: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }
    for (name, encoding, baseline) in [
        ("MIDR_EL1", 0xc000u32, 0x0000_0000_410f_d811),
        ("MPIDR_EL1", 0xc005, 0x0000_0000_8000_0000),
        ("ID_AA64PFR0_EL1", 0xc020, 0x1101_0000_1111_0011),
        ("ID_AA64PFR1_EL1", 0xc021, 0),
        ("ID_AA64PFR2_EL1", 0xc022, 0),
        ("ID_AA64ZFR0_EL1", 0xc024, 0),
        ("ID_AA64SMFR0_EL1", 0xc025, 0),
        ("ID_AA64FPFR0_EL1", 0xc027, 0),
        ("ID_AA64DFR0_EL1", 0xc028, 0x0000_00f0_1030_5006),
        ("ID_AA64DFR1_EL1", 0xc029, 0),
        ("ID_AA64DFR2_EL1", 0xc02a, 0),
        ("ID_AA64ISAR0_EL1", 0xc030, 0x0221_1001_1021_2120),
        ("ID_AA64ISAR1_EL1", 0xc031, 0x0000_0111_0021_1002),
        ("ID_AA64ISAR2_EL1", 0xc032, 0),
        ("ID_AA64ISAR3_EL1", 0xc033, 0),
        ("ID_AA64MMFR0_EL1", 0xc038, 0x0000_0111_0f10_0022),
        ("ID_AA64MMFR1_EL1", 0xc039, 0x0000_0000_1121_2120),
        ("ID_AA64MMFR2_EL1", 0xc03a, 0x1201_0111_0000_1011),
        ("ID_AA64MMFR3_EL1", 0xc03b, 0),
        ("ID_AA64MMFR4_EL1", 0xc03c, 0),
        ("CTR_EL0", 0xd801, 0x0000_0000_8444_c004),
    ] {
        let id = KVM_REG_ARM64 | KVM_REG_SIZE_U64 | KVM_REG_ARM64_SYSREG | u64::from(encoding);
        match kvm.get_one_reg(id) {
            Ok(value) => println!("identity.{name}: {value:#018x}"),
            Err(error) => {
                eprintln!("cannot read {name} ({encoding:#06x}): {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
        match kvm.set_one_reg(id, baseline) {
            Ok(()) => match kvm.get_one_reg(id) {
                Ok(value) if value == baseline => {
                    println!("baseline.{name}: accepted {value:#018x}")
                }
                Ok(value) => {
                    eprintln!("baseline {name} read back {value:#018x}, expected {baseline:#018x}");
                    return std::process::ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("cannot read back baseline {name}: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            },
            Err(error) => {
                eprintln!("baseline.{name}: rejected {baseline:#018x}: {error}");
                return std::process::ExitCode::FAILURE;
            }
        }
    }

    // DCZID_EL0 is guest-visible but intentionally absent from KVM's
    // one-register list. Measure what the guest actually reads: MRS X0,
    // DCZID_EL0 followed by an eight-byte store to an unmapped IPA, whose
    // KVM_EXIT_MMIO payload is an independent copy of X0.
    page.put(0, 0xd53b_00e0); // mrs x0, dczid_el0
    page.put(4, 0xf900_0020); // str x0, [x1]
    // SAFETY: the aligned allocation is PAGE_SIZE bytes and outlives `kvm`.
    if let Err(error) =
        unsafe { kvm.set_user_memory_region(0, 0, page.ptr.as_ptr(), PAGE_SIZE as u64) }
    {
        eprintln!("cannot map guest identity probe page: {error}");
        return std::process::ExitCode::FAILURE;
    }
    for (id, value) in [
        (core_reg(2), 0x1_0000), // X1: unmapped MMIO address
        (core_reg(64), 0),       // PC
        (core_reg(66), 0x3c5),   // PSTATE: EL1h, DAIF masked
    ] {
        if let Err(error) = kvm.set_one_reg(id, value) {
            eprintln!("cannot initialize guest identity probe register: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }
    let view = match kvm.run() {
        Ok(view) => view,
        Err(error) => {
            eprintln!("guest DCZID_EL0 probe failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if !view.mmio.is_write || view.mmio.phys_addr != 0x1_0000 || view.mmio.len != 8 {
        eprintln!("guest DCZID_EL0 probe returned unexpected exit: {view:?}");
        return std::process::ExitCode::FAILURE;
    }
    println!(
        "identity-insn.DCZID_EL0: {:#018x}",
        u64::from_le_bytes(view.mmio.data)
    );
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("arm64_kvm_id_probe requires a Linux/aarch64 host outside Miri");
    std::process::ExitCode::from(2)
}
