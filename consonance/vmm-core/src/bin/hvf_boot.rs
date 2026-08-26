// SPDX-License-Identifier: AGPL-3.0-or-later
//! Event-count-bounded live arm64 Linux boot on Hypervisor.framework.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::io::Write;

    use vmm_core::vendor::arm64::bringup;
    use vmm_core::vmm::Step;

    const READY: &[u8] = b"HARMONY_AA5_READY\n";
    const DEFAULT_RAM: usize = 128 * 1024 * 1024;
    const DEFAULT_MAX_EVENTS: u64 = 1_000_000;

    let mut args = std::env::args_os().skip(1);
    let Some(image_path) = args.next() else {
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events]");
        return std::process::ExitCode::from(2);
    };
    let Some(initramfs_path) = args.next() else {
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events]");
        return std::process::ExitCode::from(2);
    };
    let max_events = match args.next() {
        Some(value) => match value.to_string_lossy().parse::<u64>() {
            Ok(value) if value > 0 => value,
            _ => {
                eprintln!("max-events must be a positive integer");
                return std::process::ExitCode::from(2);
            }
        },
        None => DEFAULT_MAX_EVENTS,
    };
    if args.next().is_some() {
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events]");
        return std::process::ExitCode::from(2);
    }

    let image = match std::fs::read(&image_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {:?}: {error}", image_path);
            return std::process::ExitCode::FAILURE;
        }
    };
    let initramfs = match std::fs::read(&initramfs_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {:?}: {error}", initramfs_path);
            return std::process::ExitCode::FAILURE;
        }
    };
    let bootargs = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";
    let mut vmm = match bringup::boot_hvf(&image, &initramfs, bootargs, DEFAULT_RAM) {
        Ok(vmm) => vmm,
        Err(error) => {
            eprintln!("HVF composition failed: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut emitted = 0;
    for event in 0..max_events {
        let step = match vmm.step() {
            Ok(step) => step,
            Err(error) => {
                eprintln!("HVF boot failed at event {event}: {error}");
                eprintln!("vcpu: {:?}", vmm.inspect_vcpu());
                return std::process::ExitCode::FAILURE;
            }
        };
        let serial = vmm.serial_output();
        if serial.len() > emitted {
            if let Err(error) = std::io::stdout().write_all(&serial[emitted..]) {
                eprintln!("serial output failed: {error}");
                return std::process::ExitCode::FAILURE;
            }
            emitted = serial.len();
        }
        if serial.windows(READY.len()).any(|window| window == READY) {
            let hash = vmm.state_hash();
            let hex: String = hash.iter().map(|byte| format!("{byte:02x}")).collect();
            println!("HVF_BOOT_READY event={event} state_hash={hex}");
            return std::process::ExitCode::SUCCESS;
        }
        if let Step::Terminal(reason) = step {
            eprintln!("HVF boot stopped before /init marker at event {event}: {reason:?}");
            return std::process::ExitCode::FAILURE;
        }
    }

    eprintln!("HVF boot watchdog reached {max_events} events before /init marker");
    std::process::ExitCode::FAILURE
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_boot requires an Apple Silicon macOS host outside Miri");
    std::process::ExitCode::from(2)
}
