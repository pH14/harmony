// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use vmm_backend::{Backend, BackendError, CommonExit, Exit, Gpa, KvmBackend, X86Policy};
use vmm_core::vendor::x86::contract;
use vmm_core::vmm::GuestRam;

fn backend(ram: &mut GuestRam) -> KvmBackend {
    let mut backend = KvmBackend::new().expect("KVM backend");
    backend
        .set_policy(&X86Policy {
            cpuid: contract::cpuid_model(),
            msr_filter: contract::msr_filter_allow(),
        })
        .expect("set production x86 policy");
    // SAFETY: GuestRam is page-aligned, stays fixed, and outlives the backend.
    unsafe { backend.map_memory(Gpa(0), ram.as_mut_bytes()) }.expect("map guest RAM");
    backend
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored"]
fn pcide_restore_requires_ia32e_mode_before_entry() {
    let mut real_ram = GuestRam::new(0x10000).expect("real-mode RAM");
    let mut real = backend(&mut real_ram);
    real.write_guest(Gpa(0x1000), &[0xf4]).expect("write HLT");
    let mut setup = real.save().expect("save real-mode setup");
    setup.sregs.cs.base = 0;
    setup.sregs.cs.selector = 0;
    setup.regs.rip = 0x1000;
    setup.regs.rflags = 2;
    real.restore(&setup).expect("restore real-mode setup");
    let baseline = real.save().expect("save real-mode baseline");

    let mut invalid = baseline.clone();
    invalid.sregs.cr4 |= 1 << 17;
    invalid.regs.rax ^= 1;
    assert!(matches!(
        real.validate_restore_state(&invalid),
        Err(BackendError::InvalidState)
    ));
    assert!(matches!(
        real.restore(&invalid),
        Err(BackendError::InvalidState)
    ));
    assert_eq!(real.save().expect("save after rejection"), baseline);
    assert_eq!(
        real.run().expect("run real-mode HLT"),
        Exit::Common(CommonExit::Idle)
    );
    drop(real);

    let mut long_ram = GuestRam::new(0x10000).expect("long-mode RAM");
    let mut long = backend(&mut long_ram);
    long.write_guest(Gpa(0x1000), &[0xf4])
        .expect("write long-mode HLT");
    for (address, entry) in [(0x4000, 0x5003u64), (0x5000, 0x6003), (0x6000, 0x83)] {
        long.write_guest(Gpa(address), &entry.to_le_bytes())
            .expect("write page table");
    }
    let mut valid = long.save().expect("save long-mode setup");
    valid.regs.rip = 0x1000;
    valid.regs.rflags = 2;
    valid.sregs.cs.base = 0;
    valid.sregs.cs.selector = 8;
    valid.sregs.cs.l = 1;
    valid.sregs.cs.db = 0;
    valid.sregs.cs.type_ = 11;
    valid.sregs.cs.s = 1;
    valid.sregs.cs.present = 1;
    valid.sregs.cs.limit = u32::MAX;
    valid.sregs.cs.g = 1;
    valid.sregs.cr0 |= (1 << 31) | 1;
    valid.sregs.cr4 |= (1 << 17) | (1 << 5);
    valid.sregs.cr3 = 0x4000;
    valid.sregs.efer |= (1 << 8) | (1 << 10);
    valid.msrs.insert(0xc000_0080, valid.sregs.efer);
    long.validate_restore_state(&valid)
        .expect("validate long-mode PCIDE");
    long.restore(&valid).expect("restore long-mode PCIDE");
    let immediate = long.save().expect("save long-mode PCIDE");
    assert_eq!(immediate.sregs.cr0, valid.sregs.cr0);
    assert_eq!(immediate.sregs.cr4, valid.sregs.cr4);
    assert_eq!(immediate.sregs.efer, valid.sregs.efer);
    assert_eq!(
        long.run().expect("run long-mode HLT"),
        Exit::Common(CommonExit::Idle)
    );
    let after = long.save().expect("save after long-mode entry");
    assert_eq!(after.sregs.cr0, immediate.sregs.cr0);
    assert_eq!(after.sregs.cr4, immediate.sregs.cr4);
    assert_eq!(after.sregs.efer, immediate.sregs.efer);
}
