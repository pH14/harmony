// SPDX-License-Identifier: AGPL-3.0-or-later
//! Print the stock KVM/arm64 guest identity-register surface before policy
//! installation. M5 uses this beside `hvf_probe` to derive the conservative
//! cross-host register intersection from measured values.

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use vmm_backend::{Arm64Kvm, LiveKvm};

    const KVM_REG_ARM64: u64 = 0x6000_0000_0000_0000;
    const KVM_REG_SIZE_U64: u64 = 0x0030_0000_0000_0000;
    const KVM_REG_ARM64_SYSREG: u64 = 0x0013_0000;

    let mut kvm = match LiveKvm::new() {
        Ok(kvm) => kvm,
        Err(error) => {
            eprintln!("cannot create initialized KVM/arm64 vCPU: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    println!("KVM arm64 identity probe v1");
    for (name, encoding, baseline) in [
        ("MIDR_EL1", 0xc000u32, 0x0000_0000_410f_d811),
        ("MPIDR_EL1", 0xc005, 0x0000_0000_8000_0000),
        ("ID_AA64PFR0_EL1", 0xc020, 0x1101_0000_1111_0011),
        ("ID_AA64PFR1_EL1", 0xc021, 0),
        ("ID_AA64ZFR0_EL1", 0xc024, 0),
        ("ID_AA64SMFR0_EL1", 0xc025, 0),
        ("ID_AA64DFR0_EL1", 0xc028, 0x0000_00f0_1030_5006),
        ("ID_AA64DFR1_EL1", 0xc029, 0),
        ("ID_AA64ISAR0_EL1", 0xc030, 0x0221_1001_1021_2120),
        ("ID_AA64ISAR1_EL1", 0xc031, 0x0000_0111_0021_1002),
        ("ID_AA64MMFR0_EL1", 0xc038, 0x0000_0111_0f10_0022),
        ("ID_AA64MMFR1_EL1", 0xc039, 0x0000_0000_1121_2120),
        ("ID_AA64MMFR2_EL1", 0xc03a, 0x1201_0111_0000_1011),
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
    std::process::ExitCode::SUCCESS
}

#[cfg(not(all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("arm64_kvm_id_probe requires a Linux/aarch64 host outside Miri");
    std::process::ExitCode::from(2)
}
