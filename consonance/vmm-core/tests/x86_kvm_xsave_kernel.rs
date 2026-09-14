// SPDX-License-Identifier: AGPL-3.0-or-later
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;
use vmm_core::vmm::Step;

#[test]
#[ignore = "requires KVM and G1_KERNEL / G1_INITRAMFS guest artifacts"]
fn g1_kernel_xsave_functional() {
    let read = |name| std::fs::read(std::env::var(name).expect(name)).expect(name);
    let kernel = read("G1_KERNEL");
    let initramfs = read("G1_INITRAMFS");
    let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1";
    let mut vmm = boot_linux_stock_virtual_time(&kernel, &initramfs, 256 << 20, cmdline, 42)
        .expect("boot diagnostic Linux");
    let mut printed = 0;
    for _ in 0..50_000_000u64 {
        let step = vmm.step().expect("guest step");
        let serial = vmm.serial();
        std::io::stderr().write_all(&serial[printed..]).unwrap();
        printed = serial.len();
        if serial
            .windows(b"G1_GUEST_OK".len())
            .any(|w| w == b"G1_GUEST_OK")
        {
            return;
        }
        if !matches!(step, Step::Continued) {
            panic!("guest terminated without G1_GUEST_OK: {step:?}");
        }
    }
    panic!("guest exceeded step budget");
}

#[test]
#[ignore = "requires KVM and G1_KERNEL / G1_INITRAMFS; retains complete endpoint artifacts"]
fn g1_kernel_xsave_paired_endpoint() {
    let read = |name| std::fs::read(std::env::var(name).expect(name)).expect(name);
    let kernel = read("G1_KERNEL");
    let initramfs = read("G1_INITRAMFS");
    let root = std::path::PathBuf::from(std::env::var("G1_REPORT_DIR").expect("G1_REPORT_DIR"));
    let cmdline = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic hpet=disable harmony_pvclock noxsaveopt noxsaves LD_BIND_NOW=1";
    let mut captures = Vec::new();
    #[allow(unused_mut)]
    let mut variants = vec![
        ("reference".to_string(), None),
        ("paired".to_string(), None),
    ];
    #[cfg(feature = "xsave-diagnostics")]
    if let Ok(addresses) = std::env::var("G1_DEBUG_RIPS") {
        for address in addresses.split(',') {
            let rip = u64::from_str_radix(address.trim_start_matches("0x"), 16).expect("hex RIP");
            variants.push((format!("debug-{rip:x}"), Some(rip)));
        }
    }
    for (label, breakpoint) in variants {
        let mut vmm = boot_linux_stock_virtual_time(&kernel, &initramfs, 256 << 20, cmdline, 42)
            .expect("boot paired diagnostic Linux");
        #[cfg(feature = "xsave-diagnostics")]
        if let Some(rip) = breakpoint {
            vmm.diagnostic_breakpoint(rip).expect("arm breakpoint");
        }
        #[cfg(not(feature = "xsave-diagnostics"))]
        let _: Option<u64> = breakpoint;
        let mut endpoint = None;
        for steps in 1..=50_000_000u64 {
            match vmm.step().expect("paired guest step") {
                Step::Continued => {}
                Step::Terminal(reason) => {
                    endpoint = Some((steps, reason));
                    break;
                }
                Step::SdkStop => panic!("unexpected SDK stop"),
            }
        }
        let (steps, reason) = endpoint.expect("guest exceeded endpoint step budget");
        #[cfg(feature = "xsave-diagnostics")]
        assert_eq!(
            vmm.diagnostic_debug_hits(),
            breakpoint.into_iter().collect::<Vec<_>>(),
            "missing or repeated breakpoint hit"
        );
        let directory = root.join(&label);
        std::fs::create_dir_all(&directory).unwrap();
        let memory = vmm.guest_memory().to_vec();
        let state = vmm.state_blob().expect("capture modeled state");
        let vm_state = vmm
            .save_vm_state()
            .expect("capture VM state")
            .encode()
            .unwrap();
        let components = vmm.state_components();
        std::fs::write(directory.join("memory.bin"), &memory).unwrap();
        std::fs::write(directory.join("state-blob.bin"), &state).unwrap();
        std::fs::write(directory.join("vm-state.bin"), &vm_state).unwrap();
        std::fs::write(directory.join("serial.bin"), vmm.serial()).unwrap();
        std::fs::write(directory.join("summary.txt"), format!("steps={steps}\nreason={reason:?}\nstate_hash={:02x?}\ncomponents={components:02x?}\n", vmm.state_hash().unwrap())).unwrap();
        assert!(
            vmm.serial()
                .windows(b"G1_GUEST_OK".len())
                .any(|w| w == b"G1_GUEST_OK"),
            "functional fixture did not finish; see retained serial"
        );
        captures.push((memory, state, vm_state, steps, reason, label));
    }
    let a = &captures[0];
    for b in &captures[1..] {
        let mut differences = String::new();
        for (name, left, right) in [
            ("memory", &a.0, &b.0),
            ("state-blob", &a.1, &b.1),
            ("vm-state", &a.2, &b.2),
        ] {
            let count = left.iter().zip(right).filter(|(x, y)| x != y).count()
                + left.len().abs_diff(right.len());
            let first: Vec<_> = left
                .iter()
                .zip(right)
                .enumerate()
                .filter(|(_, (x, y))| x != y)
                .take(64)
                .map(|(offset, (x, y))| (offset, *x, *y))
                .collect();
            differences.push_str(&format!(
                "{name}: lengths={}/{} differing_bytes={count} first={first:x?}\n",
                left.len(),
                right.len()
            ));
        }
        std::fs::write(root.join(format!("differences-{}.txt", b.5)), &differences).unwrap();
        eprintln!("{differences}");
        assert_eq!(a.3, b.3, "endpoint step counts differ");
        assert_eq!(a.4, b.4, "terminal reasons differ");
        assert!(
            a.0 == b.0 && a.1 == b.1 && a.2 == b.2,
            "unmasked endpoint mismatch; see G1_REPORT_DIR"
        );
    }
}
