// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::measure;
use vmm_backend::{Arm64AsidBits, Arm64Caps, Backend, Capabilities, MockArm64Backend, MockBackend};

fn arm64_backend() -> MockArm64Backend {
    let width = Arm64AsidBits::Eight;
    let mut backend = MockArm64Backend::with_capabilities(Capabilities {
        name: "mock-arm64",
        arch: Arm64Caps {
            in_kernel_gic: false,
            asid_bits: width,
        },
    });
    backend
        .set_policy(&vmm_core::vendor::arm64::contract::policy(width))
        .unwrap();
    backend
}

pub fn run(check: bool) {
    let mut x86 = vmm_core::vmm::Vmm::new(
        MockBackend::new(),
        vmm_core::vmm::GuestRam::new(4096).unwrap(),
    );
    let mut old_x86 = vmm_core_reference::vmm::Vmm::new(
        MockBackend::new(),
        vmm_core_reference::vmm::GuestRam::new(4096).unwrap(),
    );
    let mut arm64 =
        vmm_core::vmm::Vmm::new(arm64_backend(), vmm_core::vmm::GuestRam::new(4096).unwrap());
    let mut old_arm64 = vmm_core_reference::vmm::Vmm::new(
        arm64_backend(),
        vmm_core_reference::vmm::GuestRam::new(4096).unwrap(),
    );
    x86.wire_snapshot_hashing();
    old_x86.wire_snapshot_hashing();
    arm64.wire_snapshot_hashing();
    old_arm64.wire_snapshot_hashing();
    let x86_hash = old_x86.state_hash().unwrap();
    let arm64_hash = old_arm64.state_hash().unwrap();
    assert_eq!(x86.state_blob().unwrap(), old_x86.state_blob().unwrap());
    assert_eq!(arm64.state_blob().unwrap(), old_arm64.state_blob().unwrap());
    let x86_state = x86.save_vm_state().unwrap();
    let old_x86_state = old_x86.save_vm_state().unwrap();
    let arm64_state = arm64.save_vm_state().unwrap();
    let old_arm64_state = old_arm64.save_vm_state().unwrap();
    assert_eq!(x86_state.encode().unwrap(), old_x86_state.encode().unwrap());
    assert_eq!(
        arm64_state.encode().unwrap(),
        old_arm64_state.encode().unwrap()
    );
    x86.restore_vm_state(&x86_state).unwrap();
    old_x86.restore_vm_state(&old_x86_state).unwrap();
    arm64.restore_vm_state(&arm64_state).unwrap();
    old_arm64.restore_vm_state(&old_arm64_state).unwrap();
    for sample in 0..if check { 1 } else { 9 } {
        for old in if sample % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let name = if old { "original" } else { "current" };
            let iterations = if check { 1 } else { 1000 };
            measure(
                || {
                    if old {
                        old_x86.state_hash().unwrap()
                    } else {
                        x86.state_hash().unwrap()
                    }
                },
                &x86_hash,
                "vmm_x86_hash_4096",
                name,
                sample,
                iterations,
            );
            measure(
                || {
                    if old {
                        old_arm64.state_hash().unwrap()
                    } else {
                        arm64.state_hash().unwrap()
                    }
                },
                &arm64_hash,
                "vmm_arm64_hash_4096",
                name,
                sample,
                iterations,
            );
        }
    }
}
