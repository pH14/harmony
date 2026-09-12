// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stock-KVM Go runtime acceptance for #276. The same uninstrumented workload
//! runs with default Go settings on both production kernel profiles. Repeated
//! boots must agree on the execution trace and full-state checkpoints, including
//! a checkpoint at the workload's success marker. The corresponding traps-off
//! kernel is a planted control: it must expose differing host-counter values in
//! the full guest state, preventing a production pass from masking disabled
//! counter confinement.
//!
//! Build with `nix run .#guest-images -- --output OUT`, stage OUT/x86_64 under
//! consonance/harmony-linux/build, then run this ignored test on Linux/x86 KVM.
//! An external timeout is required: a stuck KVM_RUN cannot poll a Rust watchdog.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use vmm_core::vendor::x86::bringup::boot_linux_stock_virtual_time;
use vmm_core::virtual_time::{NormalizedLog, check_delivery_placement, compare_normalized_logs};
use vmm_core::vmm::Step;

const RAM: usize = 256 << 20;
const SEED: u64 = 0x0047_4f27_65ee_dc01;
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t tsc=reliable \
    no_timer_check lpj=4000000 random.trust_cpu=off nokaslr nosmp maxcpus=1 \
    nox2apic hpet=disable harmony_pvclock";
const SUCCESS: &[u8] = b"GO_RUNTIME_OK count=12 checksum=2881f549424eb0d1";
const MAX_STEPS: u64 = 50_000_000;

fn artifact(name: &str) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../consonance/harmony-linux/build")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("build guest artifact {}: {e}", path.display()))
}

fn contains(bytes: &[u8], marker: &[u8]) -> bool {
    bytes.windows(marker.len()).any(|window| window == marker)
}

fn final_state_hash(log: &NormalizedLog, label: &str) -> [u8; 32] {
    log.events
        .iter()
        .rev()
        .find_map(|event| event.state_hash)
        .unwrap_or_else(|| panic!("{label}: no full-state checkpoint"))
}

fn boot(kernel: &[u8], initramfs: &[u8], label: &str) -> (NormalizedLog, [u8; 32]) {
    let mut vmm = boot_linux_stock_virtual_time(kernel, initramfs, RAM, CMDLINE, SEED)
        .expect("stock-KVM virtual-time boot");
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0;
    let mut success = false;
    let mut stderr = std::io::stderr();
    for steps in 0..MAX_STEPS {
        let step = vmm.step();
        if vmm.serial().len() > printed {
            stderr.write_all(&vmm.serial()[printed..]).unwrap();
            printed = vmm.serial().len();
            assert!(
                !contains(vmm.serial(), b"GO_RUNTIME_FAIL") && !contains(vmm.serial(), b"panic:"),
                "{label}: Go reported failure"
            );
            success = contains(vmm.serial(), SUCCESS);
        }
        assert!(
            matches!(step, Ok(Step::Continued)),
            "{label}: guest stopped before Go completed: {step:?}"
        );
        if success {
            break;
        }
        if steps.is_multiple_of(4096) {
            assert!(
                start.elapsed() < Duration::from_secs(300),
                "{label}: timeout"
            );
        }
    }
    assert!(
        success,
        "{label}: Go did not complete within the step budget"
    );
    assert!(
        contains(
            vmm.serial(),
            b"harmony_pvclock: exit-count clock page registered"
        ),
        "{label}: virtual clock was not registered"
    );
    vmm.checkpoint_virtual_time_trace()
        .expect("checkpoint full guest state at the Go completion marker");
    let trace = vmm.virtual_time_trace().unwrap();
    check_delivery_placement(trace.schedule(), trace.normalized_log())
        .expect("virtual timer interrupt placement");
    let log = trace.normalized_log();
    assert!(log.events.last().unwrap().state_hash.is_some());
    eprintln!(
        "GO_RUNTIME_TRACE profile={label} events={} digest={:02x?}",
        log.events.len(),
        trace.normalized_digest()
    );
    (log.clone(), trace.normalized_digest())
}

#[test]
#[ignore = "requires stock x86 KVM and Nix-built Go/kernel guest artifacts"]
fn uninstrumented_go_repeats_on_production_kernels_and_rejects_traps_off() {
    let initramfs = artifact("initramfs-go-runtime.cpio.gz");
    for profile in ["bzImage", "bzImage-faultlab"] {
        let kernel = artifact(profile);
        let (reference, digest) = boot(&kernel, &initramfs, profile);
        for repetition in 1..3 {
            let (observed, observed_digest) = boot(&kernel, &initramfs, profile);
            compare_normalized_logs(&reference, &observed).unwrap_or_else(|difference| {
                panic!("{profile} repetition {repetition} diverged: {difference:?}")
            });
            assert_eq!(digest, observed_digest, "{profile}: digest coverage");
        }
    }

    let traps_off = artifact("bzImage-n6-traps-off");
    let (first, first_digest) = boot(&traps_off, &initramfs, "bzImage-n6-traps-off-1");
    let (second, second_digest) = boot(&traps_off, &initramfs, "bzImage-n6-traps-off-2");
    let first_hash = final_state_hash(&first, "bzImage-n6-traps-off-1");
    let second_hash = final_state_hash(&second, "bzImage-n6-traps-off-2");
    assert_ne!(
        first_hash, second_hash,
        "traps-off control leaked no host-counter value into full guest state"
    );
    assert!(
        compare_normalized_logs(&first, &second).is_err(),
        "traps-off control unexpectedly repeated its normalized trace"
    );
    assert_ne!(
        first_digest, second_digest,
        "traps-off control unexpectedly repeated its digest"
    );
    eprintln!("GO_RUNTIME_TRAPS_OFF_REJECTED first={first_hash:02x?} second={second_hash:02x?}");
}
