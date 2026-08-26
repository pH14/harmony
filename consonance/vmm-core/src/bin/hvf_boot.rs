// SPDX-License-Identifier: AGPL-3.0-or-later
//! Event-count-bounded live arm64 Linux boot on Hypervisor.framework.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::Duration;

    use vmm_core::vendor::arm64::bringup;
    use vmm_core::vmm::Step;

    // Kernel-console newline translation inserts `\r` before `\n`. Match the
    // complete semantic marker rather than one transport's line ending.
    const READY: &[u8] = b"HARMONY_AA5_READY";
    const DEFAULT_RAM: usize = 128 * 1024 * 1024;
    const DEFAULT_MAX_EVENTS: u64 = 1_000_000;
    const ENTRY_WATCHDOG: Duration = Duration::from_secs(5);

    enum WatchdogCommand {
        Arm(u64),
        Disarm,
        Stop,
    }

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
            eprintln!("HVF composition failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // Host wall time is a kill condition only. It never advances V-time or
    // enters guest state: expiry asks HVF to return from the current entry and
    // the run fails loudly with the event index.
    let exit_handle = vmm.hvf_exit_handle();
    let (watchdog_tx, watchdog_rx) = mpsc::channel();
    let watchdog_fired = Arc::new(AtomicU64::new(u64::MAX));
    let watchdog_result = Arc::clone(&watchdog_fired);
    let watchdog = std::thread::spawn(move || {
        let mut armed = None;
        loop {
            let command = match armed {
                None => watchdog_rx
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                Some(_) => watchdog_rx.recv_timeout(ENTRY_WATCHDOG),
            };
            match command {
                Ok(WatchdogCommand::Arm(event)) => armed = Some(event),
                Ok(WatchdogCommand::Disarm) => armed = None,
                Ok(WatchdogCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let event = armed.take().expect("timeout implies an armed watchdog");
                    watchdog_result.store(event, Ordering::Release);
                    let _ = exit_handle.request_exit();
                }
            }
        }
    });

    let stop_watchdog = |tx: &mpsc::Sender<WatchdogCommand>,
                         thread: std::thread::JoinHandle<()>| {
        let _ = tx.send(WatchdogCommand::Stop);
        let _ = thread.join();
    };

    let mut emitted = 0;
    for event in 0..max_events {
        if watchdog_tx.send(WatchdogCommand::Arm(event)).is_err() {
            eprintln!("HVF liveness watchdog thread stopped unexpectedly");
            return std::process::ExitCode::FAILURE;
        }
        let step = match vmm.step() {
            Ok(step) => step,
            Err(error) => {
                let fired = watchdog_fired.load(Ordering::Acquire);
                if fired == event {
                    eprintln!(
                        "HVF boot liveness watchdog aborted event {event} after \
                         {ENTRY_WATCHDOG:?}: {error}"
                    );
                } else {
                    eprintln!("HVF boot failed at event {event}: {error}");
                }
                eprintln!("pvclock registration: {:?}", vmm.pvclock_registration());
                if let Some(page) = vmm.pvclock_page() {
                    eprintln!(
                        "pvclock frame: {:?}; prefix={:02x?}",
                        vtime::pvclock::read(page),
                        &page[..40]
                    );
                }
                eprintln!("vcpu: {:?}", vmm.inspect_vcpu());
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
        };
        let _ = watchdog_tx.send(WatchdogCommand::Disarm);
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
            stop_watchdog(&watchdog_tx, watchdog);
            return std::process::ExitCode::SUCCESS;
        }
        if let Step::Terminal(reason) = step {
            eprintln!("HVF boot stopped before /init marker at event {event}: {reason:?}");
            stop_watchdog(&watchdog_tx, watchdog);
            return std::process::ExitCode::FAILURE;
        }
    }

    eprintln!("HVF boot watchdog reached {max_events} events before /init marker");
    stop_watchdog(&watchdog_tx, watchdog);
    std::process::ExitCode::FAILURE
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_boot requires an Apple Silicon macOS host outside Miri");
    std::process::ExitCode::from(2)
}
