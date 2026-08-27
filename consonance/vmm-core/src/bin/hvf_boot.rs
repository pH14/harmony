// SPDX-License-Identifier: AGPL-3.0-or-later
//! Event-count-bounded live arm64 Linux boot on Hypervisor.framework.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn placement_event(error: &vmm_core::prescriptive::PlacementViolation) -> Option<u64> {
    use vmm_core::prescriptive::PlacementViolation;

    match error {
        PlacementViolation::BadEventIndex { position, .. } => Some(*position),
        PlacementViolation::VtimeRegressed { event_index, .. }
        | PlacementViolation::WrongDelivery { event_index, .. } => Some(*event_index),
        PlacementViolation::Undelivered { .. } => None,
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn write_normalized_log(
    path: &std::path::Path,
    trace: &vmm_core::prescriptive::LivePrescriptiveTrace,
) -> std::io::Result<()> {
    use std::io::Write;

    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(out, "format consonance.live-prescriptive-log.v1")?;
    writeln!(out, "digest {}", hex(&trace.normalized_digest()))?;
    writeln!(out, "events {}", trace.normalized_log().events.len())?;
    for event in &trace.normalized_log().events {
        writeln!(
            out,
            "event {} class={:?} payload={} vns={} interrupts={:?} state_hash={}",
            event.event_index,
            event.class,
            hex(&event.payload_digest),
            event.vns_after,
            event.interrupts,
            event
                .state_hash
                .as_ref()
                .map_or_else(|| "-".to_string(), |hash| hex(hash)),
        )?;
    }
    writeln!(out, "schedules {}", trace.schedule().len())?;
    for scheduled in trace.schedule() {
        writeln!(
            out,
            "schedule {} deadline_vns={} armed_for_event={} canceled_at_event={:?} interrupt_id={}",
            scheduled.schedule_index,
            scheduled.deadline_vns,
            scheduled.armed_for_event,
            scheduled.canceled_at_event,
            scheduled.interrupt_id,
        )?;
    }
    Ok(())
}

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
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]");
        return std::process::ExitCode::from(2);
    };
    let Some(initramfs_path) = args.next() else {
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]");
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
    let normalized_log_path = args.next();
    if args.next().is_some() {
        eprintln!("usage: hvf_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]");
        return std::process::ExitCode::from(2);
    }
    let component_event = std::env::var("HARMONY_COMPONENT_EVENT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());

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
        if component_event == Some(event) {
            for (label, digest) in vmm.state_components() {
                eprintln!(
                    "HVF_STATE_COMPONENT event={event} label={label} digest={}",
                    hex(&digest)
                );
            }
            eprintln!("HVF_VCPU event={event} state={:?}", vmm.inspect_vcpu());
            eprintln!(
                "HVF_GIC event={event} state={:?}",
                vmm.canonical_arm64_gic_state()
            );
            if let Some(path) = std::env::var_os("HARMONY_DUMP_RAM")
                && let Err(error) = std::fs::write(&path, vmm.guest_memory())
            {
                eprintln!("cannot write diagnostic guest RAM {path:?}: {error}");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
        }
        let serial = vmm.serial_output();
        if serial.len() > emitted {
            if let Err(error) = std::io::stdout().write_all(&serial[emitted..]) {
                eprintln!("serial output failed: {error}");
                return std::process::ExitCode::FAILURE;
            }
            emitted = serial.len();
        }
        if serial.windows(READY.len()).any(|window| window == READY) {
            use vmm_core::prescriptive::{
                LogField, check_delivery_placement, compare_normalized_logs,
            };

            if let Err(error) = vmm.checkpoint_prescriptive_trace() {
                eprintln!("cannot checkpoint production prescriptive trace: {error}");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
            let trace = vmm
                .prescriptive_trace()
                .expect("prescriptive HVF composition wires a production trace");
            if let Some(path) = normalized_log_path.as_deref()
                && let Err(error) = write_normalized_log(std::path::Path::new(path), trace)
            {
                eprintln!("cannot write normalized log {path:?}: {error}");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
            if let Err(error) = check_delivery_placement(trace.schedule(), trace.normalized_log()) {
                eprintln!(
                    "production delivery-placement oracle failed: {error}; final_vns={:?}; \
                     final_schedule={:?}",
                    trace
                        .normalized_log()
                        .events
                        .last()
                        .map(|logged| logged.vns_after),
                    trace.schedule().last(),
                );
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
            let deliveries: usize = trace
                .normalized_log()
                .events
                .iter()
                .map(|logged| logged.interrupts.len())
                .sum();
            if deliveries == 0 {
                eprintln!("production trace contains no clockevent delivery");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }

            // Required negative oracle on the exact production workload: move
            // every delivered tick one exit late. Two identically late logs
            // agree with each other, but both independent oracles must reject
            // them at the first genuine delivery boundary.
            let original = trace.normalized_log();
            let mut late = original.clone();
            for logged in &mut late.events {
                logged.interrupts.clear();
            }
            for (index, logged) in original.events.iter().enumerate() {
                if logged.interrupts.is_empty() {
                    continue;
                }
                let Some(next) = late.events.get_mut(index + 1) else {
                    eprintln!("cannot shift a final-event clockevent delivery one exit late");
                    stop_watchdog(&watchdog_tx, watchdog);
                    return std::process::ExitCode::FAILURE;
                };
                next.interrupts.extend_from_slice(&logged.interrupts);
            }
            let late_peer = late.clone();
            if let Err(error) = compare_normalized_logs(&late, &late_peer) {
                eprintln!("identically late negative logs unexpectedly diverged: {error}");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
            let divergence = match compare_normalized_logs(original, &late) {
                Err(divergence) if divergence.field == LogField::Interrupts => divergence,
                Err(divergence) => {
                    eprintln!("late-log comparator reported wrong field: {divergence}");
                    stop_watchdog(&watchdog_tx, watchdog);
                    return std::process::ExitCode::FAILURE;
                }
                Ok(()) => {
                    eprintln!("normalized comparator accepted a one-exit-late production log");
                    stop_watchdog(&watchdog_tx, watchdog);
                    return std::process::ExitCode::FAILURE;
                }
            };
            let placement = match check_delivery_placement(trace.schedule(), &late) {
                Err(error) => error,
                Ok(()) => {
                    eprintln!("placement checker accepted a one-exit-late production log");
                    stop_watchdog(&watchdog_tx, watchdog);
                    return std::process::ExitCode::FAILURE;
                }
            };
            let Some(late_placement_event) = placement_event(&placement) else {
                eprintln!("late-log placement failure had no exact event: {placement}");
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            };
            if late_placement_event != divergence.event_index {
                eprintln!(
                    "negative oracles disagree: comparator event {}, placement event {}",
                    divergence.event_index, late_placement_event
                );
                stop_watchdog(&watchdog_tx, watchdog);
                return std::process::ExitCode::FAILURE;
            }
            let checkpoints = original
                .events
                .iter()
                .filter(|logged| logged.state_hash.is_some())
                .count();
            println!(
                "HVF_M1_ORACLE events={} raw={} schedules={} deliveries={} checkpoints={} \
                 placement=ok late_comparator_event={} late_placement_event={} log_digest={}",
                original.events.len(),
                trace.raw_log().len(),
                trace.schedule().len(),
                deliveries,
                checkpoints,
                divergence.event_index,
                late_placement_event,
                hex(&trace.normalized_digest()),
            );
            let hash = vmm.state_hash();
            println!("HVF_BOOT_READY event={event} state_hash={}", hex(&hash));
            stop_watchdog(&watchdog_tx, watchdog);
            return std::process::ExitCode::SUCCESS;
        }
        if let Step::Terminal(reason) = step {
            eprintln!("HVF boot stopped before /init marker at event {event}: {reason:?}");
            stop_watchdog(&watchdog_tx, watchdog);
            return std::process::ExitCode::FAILURE;
        }
    }

    if let Some(path) = normalized_log_path.as_deref() {
        if let Err(error) = vmm.checkpoint_prescriptive_trace() {
            eprintln!("cannot checkpoint bounded prescriptive trace: {error}");
            stop_watchdog(&watchdog_tx, watchdog);
            return std::process::ExitCode::FAILURE;
        }
        let trace = vmm
            .prescriptive_trace()
            .expect("prescriptive HVF composition wires a production trace");
        if let Err(error) = write_normalized_log(std::path::Path::new(path), trace) {
            eprintln!("cannot write bounded normalized log {path:?}: {error}");
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
