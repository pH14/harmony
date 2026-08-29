// SPDX-License-Identifier: AGPL-3.0-or-later
//! Live N6 x86 table-generated JIT sweep and its traps-off kernel negative.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;
use vmm_core::vmm::Step;

const GUEST_RAM_LEN: usize = 256 << 20;
const SEED: u64 = 0x004E_365E_EDC0_FFEE;
const MAX_STEPS: u64 = 50_000_000;
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable no_timer_check \
    lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 nox2apic \
    hpet=disable harmony_pvclock";
const DONE: &[u8] = b"N6_GUEST_OK arch=x86_64";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn artifact(name: &str) -> Vec<u8> {
    std::fs::read(root().join("consonance/harmony-linux/build").join(name))
        .unwrap_or_else(|error| panic!("N6 artifact {name} is required: {error}"))
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn boot_to_report(kernel: &[u8], initramfs: &[u8]) -> Vec<u8> {
    let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, GUEST_RAM_LEN, CMDLINE, SEED)
        .expect("boot N6 Linux guest");
    for _step in 0..MAX_STEPS {
        match vmm.step().expect("N6 guest step") {
            Step::Continued => {}
            other => panic!("N6 guest terminated before its report: {other:?}"),
        }
        if contains(vmm.serial(), DONE) {
            return vmm.serial().to_vec();
        }
    }
    panic!(
        "N6 guest exceeded its step budget; serial={}",
        String::from_utf8_lossy(vmm.serial())
    );
}

fn write_report(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = directory.join(name);
    let mut output = std::fs::File::create(&path).expect("create N6 report");
    output.write_all(bytes).expect("write N6 report");
    path
}

fn verify(first: &Path, second: &Path) -> std::process::Output {
    Command::new("python3")
        .current_dir(root())
        .arg("consonance/harmony-linux/scripts/n6-instruction-sweep.py")
        .arg("verify")
        .arg("--arch")
        .arg("x86_64")
        .arg("--run")
        .arg(first)
        .arg("--run")
        .arg(second)
        .output()
        .expect("run N6 verifier")
}

#[test]
#[ignore = "requires Linux/x86_64 KVM plus the N6 locked guest artifacts"]
fn traps_off_fails_before_two_traps_on_runs_are_credited() {
    assert!(Path::new("/dev/kvm").exists(), "/dev/kvm is required");
    let report_root = std::env::var_os("N6_REPORT_DIR").map_or_else(
        || std::env::temp_dir().join(format!("harmony-n6-x86-{}", std::process::id())),
        PathBuf::from,
    );
    std::fs::create_dir_all(&report_root).expect("create N6 report directory");

    let negative = boot_to_report(
        &artifact("bzImage-n6-traps-off"),
        &artifact("initramfs-n6-traps-off.cpio.gz"),
    );
    let negative_path = write_report(&report_root, "traps-off.log", &negative);
    let negative_verdict = verify(&negative_path, &negative_path);
    assert!(
        !negative_verdict.status.success(),
        "traps-off planted negative passed: {}",
        String::from_utf8_lossy(&negative_verdict.stdout)
    );
    assert!(
        String::from_utf8_lossy(&negative_verdict.stderr).contains("escaped the guest trap policy"),
        "negative failed for the wrong reason: {}",
        String::from_utf8_lossy(&negative_verdict.stderr)
    );

    let kernel = artifact("bzImage");
    let initramfs = artifact("initramfs-n6.cpio.gz");
    let first = boot_to_report(&kernel, &initramfs);
    let second = boot_to_report(&kernel, &initramfs);
    let first_path = write_report(&report_root, "traps-on-1.log", &first);
    let second_path = write_report(&report_root, "traps-on-2.log", &second);
    let positive = verify(&first_path, &second_path);
    assert!(
        positive.status.success(),
        "traps-on sweep failed: {}",
        String::from_utf8_lossy(&positive.stderr)
    );
    let stdout = String::from_utf8_lossy(&positive.stdout);
    assert!(stdout.contains("table_rows=9 exercised_rows=9 operations=166 runs=2"));
    eprintln!("{}", stdout.trim());
    eprintln!("N6_X86_REPORT_DIR={}", report_root.display());
}
