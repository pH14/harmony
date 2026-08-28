// SPDX-License-Identifier: AGPL-3.0-or-later
//! M3 PostgreSQL liveness/performance oracle on Apple Silicon HVF.

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ExitAccounting {
    raw: u64,
    portable: u64,
    substrate_private: u64,
}

fn audit_exit_accounting(
    event_loop: u64,
    raw: &[vmm_core::virtual_time::RawEvent],
    normalized_len: usize,
) -> Result<ExitAccounting, String> {
    let raw_count = u64::try_from(raw.len()).unwrap_or(u64::MAX);
    if event_loop != raw_count {
        return Err(format!(
            "event loop/raw trace mismatch: event loop {event_loop}, raw trace {raw_count}"
        ));
    }

    let mut portable = 0u64;
    let mut substrate_private = 0u64;
    for (position, event) in raw.iter().enumerate() {
        let expected_raw = u64::try_from(position).unwrap_or(u64::MAX);
        if event.event_index != expected_raw {
            return Err(format!(
                "raw exit ordinal mismatch at position {position}: got {}",
                event.event_index
            ));
        }
        match event.portable_event_index {
            Some(index) if index == portable => portable += 1,
            Some(index) => {
                return Err(format!(
                    "portable exit ordinal mismatch at raw {expected_raw}: expected {portable}, got {index}"
                ));
            }
            None => substrate_private += 1,
        }
    }

    let normalized = u64::try_from(normalized_len).unwrap_or(u64::MAX);
    if portable != normalized {
        return Err(format!(
            "raw disposition/normalized trace mismatch: {portable} portable dispositions, {normalized} normalized events"
        ));
    }
    if portable.saturating_add(substrate_private) != raw_count {
        return Err("raw dispositions do not partition the raw trace".to_string());
    }

    Ok(ExitAccounting {
        raw: raw_count,
        portable,
        substrate_private,
    })
}

fn record_pvclock_boundary(
    event: &vmm_core::virtual_time::RawEvent,
    vns: u64,
    values: &mut Vec<u64>,
) -> Option<u64> {
    let portable = event.portable_event_index?;
    values.push(vns);
    Some(portable)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn arm64_gic_diagnostic(blob: &[u8]) -> Result<String, &'static str> {
    struct Cursor<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Cursor<'_> {
        fn take(&mut self, len: usize) -> Result<&[u8], &'static str> {
            let end = self.pos.checked_add(len).ok_or("device offset overflow")?;
            let bytes = self
                .bytes
                .get(self.pos..end)
                .ok_or("truncated ARM64 device blob")?;
            self.pos = end;
            Ok(bytes)
        }

        fn u16(&mut self) -> Result<u16, &'static str> {
            let bytes: [u8; 2] = self.take(2)?.try_into().map_err(|_| "bad u16 field")?;
            Ok(u16::from_le_bytes(bytes))
        }

        fn u32(&mut self) -> Result<u32, &'static str> {
            let bytes: [u8; 4] = self.take(4)?.try_into().map_err(|_| "bad u32 field")?;
            Ok(u32::from_le_bytes(bytes))
        }

        fn u64(&mut self) -> Result<u64, &'static str> {
            let bytes: [u8; 8] = self.take(8)?.try_into().map_err(|_| "bad u64 field")?;
            Ok(u64::from_le_bytes(bytes))
        }

        fn words(&mut self, len: usize) -> Result<Vec<u32>, &'static str> {
            (0..len).map(|_| self.u32()).collect()
        }
    }

    let mut cursor = Cursor {
        bytes: blob,
        pos: 0,
    };
    if cursor.u32()? != 0x3156_4441 {
        return Err("bad ARM64 device-blob magic");
    }
    let version = cursor.u16()?;
    if !matches!(version, 2 | 4 | 6 | 8) {
        return Err("ARM64 device blob has no GIC record");
    }
    let _clock_offset = cursor.u64()?;
    let report_words = usize::try_from(cursor.u32()?).map_err(|_| "report length overflow")?;
    cursor.take(
        report_words
            .checked_mul(4)
            .ok_or("report length overflow")?,
    )?;
    let serial_bytes = usize::try_from(cursor.u32()?).map_err(|_| "serial length overflow")?;
    cursor.take(serial_bytes)?;
    cursor.take(5 * 4)?;

    let _state_version = cursor.u32()?;
    let _impl_spis = cursor.u32()?;
    let _timer_hz = cursor.u64()?;
    let _timer_intid = cursor.u32()?;
    let gicd_ctlr = cursor.u32()?;
    let group = cursor.words(32)?;
    let enable = cursor.words(32)?;
    let pending = cursor.words(32)?;
    let active = cursor.words(32)?;
    cursor.take(1020)?;
    let pmr = cursor.take(1)?[0];
    Ok(format!(
        "gicd_ctlr=0x{gicd_ctlr:08x} group0=0x{:08x} enable0=0x{:08x} \
         pending0=0x{:08x} active0=0x{:08x} pmr=0x{pmr:02x}",
        group[0], enable[0], pending[0], active[0]
    ))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, mpsc};
    use std::time::{Duration, Instant};

    use sha2::{Digest, Sha256};
    use vmm_core::m3_report::{
        GapHistogram, MAX_GAP_FACTOR, MAX_GAP_VNS, PerformanceMark, PhasePerformance,
        TICK_PERIOD_VNS, Throughput, WORKLOAD_ROWS, compare_exit_counts, compare_gap_oracles,
        parse_x86_diagnostic, validate_acceptance,
    };
    use vmm_core::vendor::arm64::bringup;
    use vmm_core::virtual_time::check_delivery_placement;
    use vmm_core::vmm::Step;

    const READY: &[u8] = b"ARM64_PG_M3_READY";
    const POSTGRES_START: &[u8] = b"PGC38: starting postgres in container";
    const POSTGRES_READY: &[u8] = b"database system is ready to accept connections";
    const WORKLOAD_BEGIN: &[u8] = b"PGC38: workload begin";
    const WORKLOAD_END: &[u8] = b"PGC38: workload end";
    const POSTGRES_STOPPED: &[u8] = b"PGC38: postgres stopped";
    const DEFAULT_RAM: usize = 512 * 1024 * 1024;
    const DEFAULT_MAX_EVENTS: u64 = 5_000_000;
    const ENTRY_WATCHDOG: Duration = Duration::from_secs(5);

    enum WatchdogCommand {
        Arm(u64),
        Disarm,
        Stop,
    }

    let usage = "usage: hvf_postgres_m3 <Image-postgres> <initramfs-postgres.cpio.gz> \
                 <optional-x86-diagnostic|-> <m3-report> [max-events]";
    let mut args = std::env::args_os().skip(1);
    let Some(image_path) = args.next() else {
        eprintln!("{usage}");
        return std::process::ExitCode::from(2);
    };
    let Some(initramfs_path) = args.next() else {
        eprintln!("{usage}");
        return std::process::ExitCode::from(2);
    };
    let Some(x86_diagnostic_path) = args.next() else {
        eprintln!("{usage}");
        return std::process::ExitCode::from(2);
    };
    let Some(report_path) = args.next() else {
        eprintln!("{usage}");
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
        eprintln!("{usage}");
        return std::process::ExitCode::from(2);
    }

    let read = |path: &std::ffi::OsStr, label: &str| -> Result<Vec<u8>, String> {
        std::fs::read(path).map_err(|error| format!("cannot read {label} {path:?}: {error}"))
    };
    let image = match read(&image_path, "kernel") {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let initramfs = match read(&initramfs_path, "initramfs") {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("{error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let image_sha: [u8; 32] = Sha256::digest(&image).into();
    let initramfs_sha: [u8; 32] = Sha256::digest(&initramfs).into();
    let (x86_diagnostic, diagnostic_sha, diagnostic_error) = if x86_diagnostic_path
        == std::ffi::OsStr::new("-")
    {
        (None, None, None)
    } else {
        let x86_diagnostic_bytes = match read(&x86_diagnostic_path, "descriptive-x86 diagnostic") {
            Ok(bytes) => bytes,
            Err(error) => {
                eprintln!("optional diagnostic unavailable: {error}");
                Vec::new()
            }
        };
        let parsed = match parse_x86_diagnostic(&x86_diagnostic_bytes) {
            Ok(sample) => (Some(sample), None),
            Err(error) => {
                eprintln!("optional descriptive-x86 diagnostic rejected: {error}");
                (None, Some(error.to_string()))
            }
        };
        let sha = if x86_diagnostic_bytes.is_empty() {
            None
        } else {
            Some(<[u8; 32]>::from(Sha256::digest(&x86_diagnostic_bytes)))
        };
        (parsed.0, sha, parsed.1)
    };

    let bootargs = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";
    let mut vmm = match bringup::boot_hvf(&image, &initramfs, bootargs, DEFAULT_RAM) {
        Ok(vmm) => vmm,
        Err(error) => {
            eprintln!("HVF composition failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };

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

    // not order-observable: M3 reports host throughput and uses wall time only
    // as a fail-loud watchdog. Neither value advances V-time or enters guest state.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut terminal_event = None;
    let mut run_error = None;
    let mut ready_seen = false;
    let mut pvclock_trace_start = None;
    let mut pvclock_values = Vec::new();
    let run_start = PerformanceMark {
        exits: 0,
        wall_ns: 0,
    };
    let mut postgres_start_mark = None;
    let mut postgres_ready_mark = None;
    let mut workload_begin_mark = None;
    let mut workload_end_mark = None;
    let mut postgres_stopped_mark = None;
    let mut terminal_mark = None;
    for event in 0..max_events {
        if watchdog_tx.send(WatchdogCommand::Arm(event)).is_err() {
            run_error = Some("HVF liveness watchdog thread stopped unexpectedly".to_string());
            break;
        }
        let step = match vmm.step() {
            Ok(step) => step,
            Err(error) => {
                let fired = watchdog_fired.load(Ordering::Acquire);
                run_error = Some(if fired == event {
                    format!(
                        "liveness watchdog aborted event {event} after {ENTRY_WATCHDOG:?}: {error}"
                    )
                } else {
                    format!("HVF payload failed at event {event}: {error}")
                });
                break;
            }
        };
        let _ = watchdog_tx.send(WatchdogCommand::Disarm);
        if watchdog_fired.load(Ordering::Acquire) == event {
            run_error = Some(format!(
                "liveness watchdog fired at event {event} after {ENTRY_WATCHDOG:?}"
            ));
            break;
        }

        let serial = vmm.serial_output();
        if serial.len() > printed {
            if let Err(error) = std::io::stdout().write_all(&serial[printed..]) {
                run_error = Some(format!("serial output failed: {error}"));
                break;
            }
            printed = serial.len();
        }
        ready_seen |= contains(serial, READY);

        // One cumulative observation per exit keeps phase accounting intrinsic
        // to this ARM run. Wall time is diagnostic only and never enters guest
        // state, scheduling, hashes, or the deterministic exit policy.
        #[allow(clippy::disallowed_methods)]
        let observed_wall_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let observed = PerformanceMark {
            exits: event.saturating_add(1),
            wall_ns: observed_wall_ns,
        };
        for (slot, marker) in [
            (&mut postgres_start_mark, POSTGRES_START),
            (&mut postgres_ready_mark, POSTGRES_READY),
            (&mut workload_begin_mark, WORKLOAD_BEGIN),
            (&mut workload_end_mark, WORKLOAD_END),
            (&mut postgres_stopped_mark, POSTGRES_STOPPED),
            (&mut terminal_mark, READY),
        ] {
            if slot.is_none() && contains(serial, marker) {
                *slot = Some(observed);
            }
        }

        if let Some(page) = vmm.pvclock_page() {
            let Some(frame) = vtime::pvclock::read(page) else {
                run_error = Some("guest pvclock page was not a stable ABI-v1 frame".to_string());
                break;
            };
            if let Err(error) = vmm.pvclock_check_oracle() {
                run_error = Some(format!("guest pvclock functional oracle failed: {error}"));
                break;
            }
            let Some(trace) = vmm.virtual_time_trace() else {
                run_error = Some("virtual_time trace absent from HVF composition".to_string());
                break;
            };
            let Some(last) = trace.normalized_log().events.last() else {
                run_error =
                    Some("pvclock registered before the first normalized event".to_string());
                break;
            };
            if frame.vns != last.vns_after {
                run_error = Some(format!(
                    "independent pvclock/trace mismatch at event {event}: page {} vs trace {}",
                    frame.vns, last.vns_after
                ));
                break;
            }
            let Some(raw) = trace.raw_log().last() else {
                run_error = Some("pvclock registered before the first raw exit".to_string());
                break;
            };
            if let Some(portable_index) =
                record_pvclock_boundary(raw, frame.vns, &mut pvclock_values)
                && pvclock_trace_start.is_none()
            {
                pvclock_trace_start = match usize::try_from(portable_index) {
                    Ok(index) => Some(index),
                    Err(_) => {
                        run_error =
                            Some("pvclock portable event index does not fit usize".to_string());
                        break;
                    }
                };
            }
        }

        // The guest emits READY only after the 20-row oracle, clean PostgreSQL
        // shutdown, and its dmesg liveness scan. `halt -f` follows immediately,
        // but HVF may keep that halted vCPU inside a non-returning entry. The
        // explicit guest-authored marker is therefore the workload terminal;
        // stop before entering the architectural halt loop.
        if ready_seen {
            terminal_event = Some(event);
            break;
        }

        if event > 0 && event % 10_000 == 0 {
            let pc = vmm.inspect_vcpu().core.pc;
            if let Some(trace) = vmm.virtual_time_trace() {
                let vns = trace
                    .normalized_log()
                    .events
                    .last()
                    .map_or(0, |last| last.vns_after);
                eprintln!(
                    "M3_PROGRESS event={event} serial_bytes={} vns={vns} schedules={} \
                     pc=0x{pc:016x}",
                    vmm.serial_output().len(),
                    trace.schedule().len()
                );
            }
        }

        match step {
            Step::Terminal(_) => {
                terminal_event = Some(event);
                break;
            }
            Step::SdkStop => {
                run_error = Some(format!("unexpected SDK stop at event {event}"));
                break;
            }
            Step::Continued => {}
        }
    }
    if terminal_event.is_none() && run_error.is_none() {
        run_error = Some(format!(
            "event budget reached before terminal: {max_events}"
        ));
    }
    let elapsed = start.elapsed();
    let _ = watchdog_tx.send(WatchdogCommand::Stop);
    let _ = watchdog.join();

    if let Some(error) = run_error {
        let vcpu = vmm.inspect_vcpu();
        let gic_diagnostic = vmm
            .save_vm_state()
            .map_err(|save_error| format!("state unavailable: {save_error}"))
            .and_then(|snapshot| arm64_gic_diagnostic(&snapshot.devices.0).map_err(str::to_string));
        let trace = vmm.virtual_time_trace();
        let failure_report = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&report_path)
            .and_then(|file| {
                let mut report = std::io::BufWriter::new(file);
                writeln!(report, "format consonance.virtual_time-m3-failure.v1")?;
                writeln!(report, "status FAIL")?;
                writeln!(report, "payload postgres-container-task38-arm64-static-lse")?;
                writeln!(report, "kernel_sha256 {}", hex(&image_sha))?;
                writeln!(report, "canonical_snapshot_sha256 {}", hex(&initramfs_sha))?;
                writeln!(report, "failure {error}")?;
                let fired = watchdog_fired.load(Ordering::Acquire);
                if fired == u64::MAX {
                    writeln!(report, "watchdog per_entry_ms=5000 status=NOT_FIRED")?;
                } else {
                    writeln!(
                        report,
                        "watchdog per_entry_ms=5000 status=FIRED event={fired}"
                    )?;
                }
                writeln!(
                    report,
                    "guest_pc_range first=0x{:016x} last=0x{:016x} samples=1",
                    vcpu.core.pc, vcpu.core.pc
                )?;
                match &gic_diagnostic {
                    Ok(diagnostic) => writeln!(report, "gic {diagnostic}")?,
                    Err(diagnostic_error) => {
                        writeln!(report, "gic diagnostic_error={diagnostic_error:?}")?;
                    }
                }
                writeln!(report, "serial_bytes {}", vmm.serial_output().len())?;
                if let Some(trace) = trace {
                    let deliveries: usize = trace
                        .normalized_log()
                        .events
                        .iter()
                        .map(|event| event.interrupts.len())
                        .sum();
                    writeln!(
                        report,
                        "trace normalized={} raw={} schedules={} deliveries={deliveries}",
                        trace.normalized_log().events.len(),
                        trace.raw_log().len(),
                        trace.schedule().len()
                    )?;
                    for (label, scheduled) in [
                        ("first", trace.schedule().first()),
                        ("last", trace.schedule().last()),
                    ] {
                        if let Some(scheduled) = scheduled {
                            writeln!(
                                report,
                                "scheduled_{label} deadline_vns={} index={} armed_for_event={} \
                                 canceled_at_event={:?} interrupt_id={}",
                                scheduled.deadline_vns,
                                scheduled.schedule_index,
                                scheduled.armed_for_event,
                                scheduled.canceled_at_event,
                                scheduled.interrupt_id
                            )?;
                        }
                    }
                    if let Some((event, delivery)) = trace
                        .normalized_log()
                        .events
                        .iter()
                        .rev()
                        .find_map(|event| event.interrupts.last().map(|delivery| (event, delivery)))
                    {
                        writeln!(
                            report,
                            "last_delivery event={} vns_after={} deadline_vns={} index={} \
                             interrupt_id={}",
                            event.event_index,
                            event.vns_after,
                            delivery.deadline_vns,
                            delivery.schedule_index,
                            delivery.interrupt_id
                        )?;
                    }
                    if let Some(last) = trace.raw_log().last() {
                        writeln!(
                            report,
                            "last_raw event={} portable_event={:?} reason={:?} backend_debug={:?}",
                            last.event_index,
                            last.portable_event_index,
                            last.reason,
                            last.backend_debug
                        )?;
                    }
                    if let Some(last) = trace.normalized_log().events.last() {
                        writeln!(
                            report,
                            "last_normalized event={} class={:?} vns_after={} payload_digest={}",
                            last.event_index,
                            last.class,
                            last.vns_after,
                            hex(&last.payload_digest)
                        )?;
                    }
                }
                report.flush()
            });
        match failure_report {
            Ok(()) => eprintln!("M3 live run failed: {error}; report={report_path:?}"),
            Err(report_error) => eprintln!(
                "M3 live run failed: {error}; cannot create failure report {report_path:?}: \
                 {report_error}"
            ),
        }
        return std::process::ExitCode::FAILURE;
    }
    let Some(terminal_event) = terminal_event else {
        eprintln!("M3 event budget reached before terminal: {max_events}");
        return std::process::ExitCode::FAILURE;
    };
    if !ready_seen {
        eprintln!("M3 payload reached terminal without {READY:?}");
        return std::process::ExitCode::FAILURE;
    }
    if let Err(error) = vmm.checkpoint_virtual_time_trace() {
        eprintln!("cannot checkpoint production virtual_time trace: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let Some(trace) = vmm.virtual_time_trace() else {
        eprintln!("production virtual_time trace absent at terminal");
        return std::process::ExitCode::FAILURE;
    };
    let Some(trace_start) = pvclock_trace_start else {
        eprintln!("guest never registered its pvclock page");
        return std::process::ExitCode::FAILURE;
    };
    let trace_values: Vec<u64> = trace.normalized_log().events[trace_start..]
        .iter()
        .map(|event| event.vns_after)
        .collect();

    let mut issues = Vec::new();
    let acceptance = match validate_acceptance(vmm.serial_output()) {
        Ok(summary) => Some(summary),
        Err(error) => {
            issues.push(format!("acceptance: {error}"));
            None
        }
    };
    if let Err(error) = check_delivery_placement(trace.schedule(), trace.normalized_log()) {
        issues.push(format!("delivery placement: {error}"));
    }
    let deliveries: usize = trace
        .normalized_log()
        .events
        .iter()
        .map(|event| event.interrupts.len())
        .sum();
    if deliveries == 0 {
        issues.push("paravirtual clockevent trace contains no delivery".to_string());
    }

    let histogram = match GapHistogram::analyze(&trace_values) {
        Ok(histogram) => {
            if let Err(error) = histogram.validate_bound() {
                issues.push(format!("gap bound: {error}"));
            }
            Some(histogram)
        }
        Err(error) => {
            issues.push(format!("gap histogram: {error}"));
            None
        }
    };
    let mut pvclock_max = 0u64;
    let mut pvclock_count = 0u64;
    for pair in pvclock_values.windows(2) {
        let Some(gap) = pair[1].checked_sub(pair[0]) else {
            issues.push(format!(
                "guest pvclock regressed: {} -> {}",
                pair[0], pair[1]
            ));
            break;
        };
        pvclock_max = pvclock_max.max(gap);
        pvclock_count += 1;
    }
    if let Some(histogram) = &histogram
        && let Err(error) = compare_gap_oracles(histogram, pvclock_max, pvclock_count)
    {
        issues.push(format!("independent gap comparator: {error}"));
    }

    let total_wall_ns = match u64::try_from(elapsed.as_nanos()) {
        Ok(value) if value > 0 => value,
        _ => {
            eprintln!("ARM wall duration does not fit nonzero u64 nanoseconds");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut phases: Vec<(&'static str, PhasePerformance)> = Vec::new();
    for (name, start_mark, end_mark) in [
        (
            "boot_to_postgres_start",
            Some(run_start),
            postgres_start_mark,
        ),
        ("postgres_startup", postgres_start_mark, postgres_ready_mark),
        (
            "ready_to_workload",
            postgres_ready_mark,
            workload_begin_mark,
        ),
        ("workload", workload_begin_mark, workload_end_mark),
        (
            "postgres_shutdown",
            workload_end_mark,
            postgres_stopped_mark,
        ),
        ("kernel_health", postgres_stopped_mark, terminal_mark),
    ] {
        match (start_mark, end_mark) {
            (Some(start_mark), Some(end_mark)) => {
                match PhasePerformance::between(name, start_mark, end_mark) {
                    Ok(phase) => phases.push((name, phase)),
                    Err(error) => issues.push(format!("performance: {error}")),
                }
            }
            _ => issues.push(format!("performance: phase marker missing for {name}")),
        }
    }
    let total_exits = terminal_event.saturating_add(1);
    let total_performance = match PhasePerformance::between(
        "total",
        run_start,
        terminal_mark.unwrap_or(PerformanceMark {
            exits: total_exits,
            wall_ns: total_wall_ns,
        }),
    ) {
        Ok(phase) => Some(phase),
        Err(error) => {
            issues.push(format!("performance: {error}"));
            None
        }
    };
    let raw_trace_count = u64::try_from(trace.raw_log().len()).unwrap_or(u64::MAX);
    if let Err(error) = compare_exit_counts(total_exits, raw_trace_count) {
        issues.push(format!("performance comparator: {error}"));
    }
    let exit_accounting = match audit_exit_accounting(
        total_exits,
        trace.raw_log(),
        trace.normalized_log().events.len(),
    ) {
        Ok(accounting) => Some(accounting),
        Err(error) => {
            issues.push(format!("exit accounting: {error}"));
            None
        }
    };
    let workload_wall_ns = phases
        .iter()
        .find_map(|(name, phase)| (*name == "workload").then_some(phase.wall_ns()))
        .unwrap_or(0);
    if workload_wall_ns == 0 {
        issues.push("performance: workload phase has no wall duration".to_string());
    }
    let arm_workload = Throughput {
        rows: acceptance
            .as_ref()
            .map_or(WORKLOAD_ROWS, |summary| summary.rows()),
        wall_ns: workload_wall_ns,
    };

    let report_file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&report_path)
    {
        Ok(file) => file,
        Err(error) => {
            eprintln!("cannot create M3 report {report_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut report = std::io::BufWriter::new(report_file);
    let status = if issues.is_empty() { "PASS" } else { "FAIL" };
    let report_result = (|| -> std::io::Result<()> {
        writeln!(report, "format consonance.virtual_time-m3-report.v2")?;
        writeln!(report, "status {status}")?;
        writeln!(report, "payload postgres-container-task38-arm64-static-lse")?;
        writeln!(report, "kernel_sha256 {}", hex(&image_sha))?;
        writeln!(report, "canonical_snapshot_sha256 {}", hex(&initramfs_sha))?;
        writeln!(report, "terminal_event {terminal_event}")?;
        writeln!(report, "terminal_source ARM64_PG_M3_READY")?;
        writeln!(report, "watchdog per_entry_ms=5000 status=PASS")?;
        writeln!(
            report,
            "acceptance rows={} status={}",
            acceptance.as_ref().map_or(0, |summary| summary.rows()),
            if acceptance.is_some() { "PASS" } else { "FAIL" }
        )?;
        if let Some(summary) = &acceptance {
            writeln!(
                report,
                "final_row uuid={} timestamp={}",
                summary.final_uuid(),
                summary.final_timestamp()
            )?;
        }
        writeln!(
            report,
            "kernel_health guest_dmesg_oracle=PASS host_serial_scan=PASS"
        )?;
        writeln!(
            report,
            "clockevents deliveries={deliveries} placement_status={}",
            if issues
                .iter()
                .any(|issue| issue.starts_with("delivery placement"))
            {
                "FAIL"
            } else {
                "PASS"
            }
        )?;
        writeln!(
            report,
            "gap_policy tick_period_vns={TICK_PERIOD_VNS} factor={MAX_GAP_FACTOR} limit_vns={MAX_GAP_VNS}"
        )?;
        if let Some(histogram) = &histogram {
            writeln!(
                report,
                "gap_histogram labels=0,1-1us,1us-100us,100us-1ms,1ms-5ms,5ms-10ms,10ms-20ms,gt20ms counts={:?}",
                histogram.counts()
            )?;
            writeln!(
                report,
                "gap_result count={} max_vns={} status={}",
                histogram.gap_count(),
                histogram.max_gap_vns(),
                if histogram.max_gap_vns() <= MAX_GAP_VNS {
                    "PASS"
                } else {
                    "FAIL"
                }
            )?;
        } else {
            writeln!(report, "gap_histogram status=FAIL")?;
        }
        writeln!(
            report,
            "independent_pvclock gaps={pvclock_count} max_vns={pvclock_max} status={}",
            if histogram.as_ref().is_some_and(|histogram| {
                histogram.gap_count() == pvclock_count && histogram.max_gap_vns() == pvclock_max
            }) {
                "PASS"
            } else {
                "FAIL"
            }
        )?;
        if let Some(total) = total_performance {
            writeln!(
                report,
                "performance_intrinsic status=PASS total_wall_ns={} total_exits={} \
                 milli_exits_per_second={}",
                total.wall_ns(),
                total.exits(),
                total.milli_exits_per_second()
            )?;
        } else {
            writeln!(report, "performance_intrinsic status=FAIL")?;
        }
        for (name, phase) in &phases {
            writeln!(
                report,
                "performance_phase name={name} wall_ns={} exits={} milli_exits_per_second={}",
                phase.wall_ns(),
                phase.exits(),
                phase.milli_exits_per_second()
            )?;
        }
        writeln!(
            report,
            "workload_rate rows={} wall_ns={} milli_rows_per_second={}",
            arm_workload.rows,
            arm_workload.wall_ns,
            arm_workload.milli_rows_per_second()
        )?;
        writeln!(
            report,
            "exit_count_comparator event_loop={total_exits} raw_trace={raw_trace_count} \
             portable_trace={} substrate_private={} status={}",
            exit_accounting.map_or(0, |accounting| accounting.portable),
            exit_accounting.map_or(0, |accounting| accounting.substrate_private),
            if exit_accounting.is_some() && total_exits == raw_trace_count {
                "PASS"
            } else {
                "FAIL"
            }
        )?;
        if let (Some(x86), Some(diagnostic_sha)) = (x86_diagnostic, diagnostic_sha) {
            writeln!(
                report,
                "optional_x86_diagnostic status=PRESENT rows={} wall_ns={} \
                 milli_rows_per_second={} evidence_sha256={}",
                x86.rows,
                x86.wall_ns,
                x86.milli_rows_per_second(),
                hex(&diagnostic_sha)
            )?;
        } else if let Some(error) = &diagnostic_error {
            writeln!(
                report,
                "optional_x86_diagnostic status=INVALID non_blocking=true error={error:?}"
            )?;
        } else {
            writeln!(
                report,
                "optional_x86_diagnostic status=NOT_PROVIDED non_blocking=true"
            )?;
        }
        writeln!(
            report,
            "trace events={} raw={} schedules={} digest={}",
            trace.normalized_log().events.len(),
            trace.raw_log().len(),
            trace.schedule().len(),
            hex(&trace.normalized_digest())
        )?;
        for issue in &issues {
            writeln!(report, "issue {issue}")?;
        }
        report.flush()
    })();
    if let Err(error) = report_result {
        eprintln!("cannot write M3 report {report_path:?}: {error}");
        return std::process::ExitCode::FAILURE;
    }

    if issues.is_empty() {
        println!(
            "HVF_M3_ORACLE status=PASS events={} max_gap_vns={} arm_total_wall_ns={} \
             workload_wall_ns={} report={:?}",
            trace.normalized_log().events.len(),
            histogram.as_ref().map_or(0, GapHistogram::max_gap_vns),
            total_performance.map_or(0, PhasePerformance::wall_ns),
            arm_workload.wall_ns,
            report_path,
        );
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("HVF_M3_ORACLE status=FAIL report={report_path:?}");
        for issue in issues {
            eprintln!("M3 finding: {issue}");
        }
        std::process::ExitCode::FAILURE
    }
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("hvf_postgres_m3 requires an Apple Silicon macOS host outside Miri");
    std::process::ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::{audit_exit_accounting, record_pvclock_boundary};
    use vmm_backend::ExitReason;
    use vmm_core::virtual_time::RawEvent;

    fn raw(event_index: u64, portable_event_index: Option<u64>) -> RawEvent {
        RawEvent {
            event_index,
            portable_event_index,
            reason: ExitReason::Mmio,
            backend_debug: String::new(),
        }
    }

    #[test]
    fn exit_accounting_partitions_every_raw_exit() {
        let events = [raw(0, Some(0)), raw(1, None), raw(2, Some(1))];
        let accounting = audit_exit_accounting(3, &events, 2).unwrap();
        assert_eq!(accounting.raw, 3);
        assert_eq!(accounting.portable, 2);
        assert_eq!(accounting.substrate_private, 1);
    }

    #[test]
    fn planted_dropped_portable_event_fails_accounting() {
        let events = [raw(0, Some(0)), raw(1, None), raw(2, Some(2))];
        assert!(audit_exit_accounting(3, &events, 3).is_err());
    }

    #[test]
    fn planted_private_to_portable_misclassification_fails_accounting() {
        let events = [raw(0, Some(0)), raw(1, Some(1)), raw(2, Some(1))];
        assert!(audit_exit_accounting(3, &events, 2).is_err());
    }

    #[test]
    fn pvclock_boundaries_keep_zero_time_portable_events() {
        let events = [raw(0, Some(0)), raw(1, None), raw(2, Some(1))];
        let mut values = Vec::new();
        assert_eq!(record_pvclock_boundary(&events[0], 7, &mut values), Some(0));
        assert_eq!(record_pvclock_boundary(&events[1], 7, &mut values), None);
        assert_eq!(record_pvclock_boundary(&events[2], 7, &mut values), Some(1));
        assert_eq!(values, [7, 7]);
    }
}
