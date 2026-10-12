// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use vmm_core::vendor::x86::bringup::boot_linux_nested_host_virtual_time;
use vmm_core::vmm::Step;

#[test]
#[ignore = "requires nested VMX or SVM and NESTED_HOST_KERNEL / NESTED_HOST_INITRAMFS"]
fn l1_creates_kvm_vm() {
    let read = |name| std::fs::read(std::env::var(name).expect(name)).expect(name);
    let kernel = read("NESTED_HOST_KERNEL");
    let initramfs = read("NESTED_HOST_INITRAMFS");
    let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable noxsaveopt noxsaves LD_BIND_NOW=1";
    let mut vmm = boot_linux_nested_host_virtual_time(&kernel, &initramfs, 256 << 20, cmdline, 42)
        .expect("boot nested-host Linux");
    let mut printed = 0;
    for _ in 0..5_000_000u64 {
        let step = vmm.step().expect("nested-host guest step");
        let serial = vmm.serial();
        std::io::stderr().write_all(&serial[printed..]).unwrap();
        printed = serial.len();
        if serial
            .windows(b"NESTED_KVM_OK".len())
            .any(|w| w == b"NESTED_KVM_OK")
        {
            return;
        }
        assert!(
            matches!(step, Step::Continued),
            "guest terminated: {step:?}"
        );
    }
    panic!("nested-host guest exceeded step budget");
}
