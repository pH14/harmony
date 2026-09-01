// SPDX-License-Identifier: AGPL-3.0-or-later
//! Event-count-bounded live arm64 Linux boot on KVM.

#[cfg(any(test, all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn contains_complete_ready_line(serial: &[u8], ready: &[u8]) -> bool {
    serial.split_inclusive(|byte| *byte == b'\n').any(|line| {
        line.ends_with(b"\n") && line.windows(ready.len()).any(|window| window == ready)
    })
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn placement_event(error: &vmm_core::virtual_time::PlacementViolation) -> Option<u64> {
    use vmm_core::virtual_time::PlacementViolation;

    match error {
        PlacementViolation::BadEventIndex { position, .. } => Some(*position),
        PlacementViolation::VtimeRegressed { event_index, .. }
        | PlacementViolation::WrongDelivery { event_index, .. } => Some(*event_index),
        PlacementViolation::Undelivered { .. } => None,
    }
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn main() -> std::process::ExitCode {
    use std::io::Write;

    use vmm_core::vendor::arm64::bringup;
    use vmm_core::vmm::Step;

    const DEFAULT_READY: &[u8] = b"HARMONY_AA5_READY";
    const DEFAULT_RAM: usize = 128 * 1024 * 1024;
    const DEFAULT_MAX_EVENTS: u64 = 1_000_000;

    let mut args = std::env::args_os().skip(1);
    let Some(image_path) = args.next() else {
        eprintln!(
            "usage: kvm_arm64_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]"
        );
        return std::process::ExitCode::from(2);
    };
    let Some(initramfs_path) = args.next() else {
        eprintln!(
            "usage: kvm_arm64_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]"
        );
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
        eprintln!(
            "usage: kvm_arm64_boot <Image> <initramfs.cpio.gz> [max-events] [normalized-log]"
        );
        return std::process::ExitCode::from(2);
    }
    let ready = std::env::var("HARMONY_READY_MARKER")
        .map_or_else(|_| DEFAULT_READY.to_vec(), String::into_bytes);
    if ready.is_empty() {
        eprintln!("HARMONY_READY_MARKER must not be empty");
        return std::process::ExitCode::from(2);
    }
    let component_event = std::env::var("HARMONY_COMPONENT_EVENT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok());
    let mut portable_component_events = std::env::var("HARMONY_PORTABLE_COMPONENT_EVENTS")
        .ok()
        .into_iter()
        .flat_map(|value| {
            value
                .split(',')
                .filter_map(|item| item.parse::<u64>().ok())
                .collect::<Vec<_>>()
        })
        .collect::<std::collections::BTreeSet<_>>();
    if let Some(event) = std::env::var("HARMONY_PORTABLE_COMPONENT_EVENT")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
    {
        portable_component_events.insert(event);
    }

    let image = match std::fs::read(&image_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {image_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let initramfs = match std::fs::read(&initramfs_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("cannot read {initramfs_path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let bootargs = "console=ttyAMA0 earlycon=pl011,0x09000000 rdinit=/init nohlt";
    let mut vmm = match bringup::boot_selected(&image, &initramfs, bootargs, DEFAULT_RAM) {
        Ok(vmm) => vmm,
        Err(error) => {
            eprintln!("KVM composition failed: {error:?}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let mut emitted = 0;
    for event in 0..max_events {
        let step = match vmm.step() {
            Ok(step) => step,
            Err(error) => {
                eprintln!("KVM boot failed at event {event}: {error}");
                eprintln!("pvclock registration: {:?}", vmm.pvclock_registration());
                eprintln!("vcpu: {:?}", vmm.inspect_vcpu());
                return std::process::ExitCode::FAILURE;
            }
        };
        let current_portable_event = vmm
            .virtual_time_trace()
            .and_then(|trace| trace.normalized_log().events.last())
            .map(|logged| logged.event_index);
        let portable_component_match = current_portable_event
            .is_some_and(|portable_event| portable_component_events.remove(&portable_event));
        if component_event == Some(event) || portable_component_match {
            eprintln!(
                "KVM_STATE_BOUNDARY raw_event={event} portable_event={current_portable_event:?}"
            );
            for (label, digest) in vmm.state_components() {
                eprintln!(
                    "KVM_STATE_COMPONENT event={event} label={label} digest={}",
                    hex(&digest)
                );
            }
            eprintln!("KVM_VCPU event={event} state={:?}", vmm.inspect_vcpu());
            eprintln!(
                "KVM_GIC event={event} state={:?}",
                vmm.canonical_arm64_gic_state()
            );
            if let Some(path) = std::env::var_os("HARMONY_DUMP_RAM")
                && let Err(error) = std::fs::write(&path, vmm.guest_memory())
            {
                eprintln!("cannot write diagnostic guest RAM {path:?}: {error}");
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
        if contains_complete_ready_line(serial, &ready) {
            use vmm_core::vendor::arm64::compare_gic_architecture;
            use vmm_core::virtual_time::{
                LogField, check_delivery_placement, compare_normalized_logs,
            };

            // M4's live save/restore oracle. Capture the typed architectural
            // GIC independently of the hash/codec, restore the exact VM-state
            // and RAM bytes, then require all three views to agree.
            let pre_restore_hash = vmm.state_hash();
            let pre_restore_components = vmm.state_components();
            let pre_restore_vcpu = vmm.inspect_vcpu();
            let pre_restore_gic = match vmm.canonical_arm64_gic_state() {
                Ok(Some(gic)) => gic,
                Ok(None) => {
                    eprintln!("KVM M4 oracle found no in-kernel vGIC state");
                    return std::process::ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("cannot read pre-restore architectural GIC state: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let vm_state = match vmm.save_vm_state() {
                Ok(state) => state,
                Err(error) => {
                    eprintln!("cannot seal M4 live save/restore checkpoint: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let ram = vmm.guest_memory().to_vec();
            if let Err(error) = vmm.restore_guest_memory(&ram) {
                eprintln!("cannot restore M4 checkpoint RAM: {error}");
                return std::process::ExitCode::FAILURE;
            }
            if let Err(error) = vmm.restore_vm_state(&vm_state) {
                eprintln!("cannot restore M4 checkpoint state: {error}");
                return std::process::ExitCode::FAILURE;
            }
            let post_restore_hash = vmm.state_hash();
            if post_restore_hash != pre_restore_hash {
                let post_restore_vcpu = vmm.inspect_vcpu();
                eprintln!(
                    "M4 save/restore state_hash mismatch: before={} after={}",
                    hex(&pre_restore_hash),
                    hex(&post_restore_hash)
                );
                let post_restore_components = vmm.state_components();
                for ((pre_label, pre_digest), (post_label, post_digest)) in
                    pre_restore_components.iter().zip(&post_restore_components)
                {
                    if pre_label != post_label || pre_digest != post_digest {
                        eprintln!(
                            "M4 component mismatch: before_label={pre_label} \
                             after_label={post_label} before={} after={}",
                            hex(pre_digest),
                            hex(post_digest),
                        );
                    }
                }
                if pre_restore_components.len() != post_restore_components.len() {
                    eprintln!(
                        "M4 component-count mismatch: before={} after={}",
                        pre_restore_components.len(),
                        post_restore_components.len(),
                    );
                }
                if pre_restore_vcpu.vtimer != post_restore_vcpu.vtimer {
                    eprintln!(
                        "M4 vtimer mismatch: before={:?} after={:?}",
                        pre_restore_vcpu.vtimer, post_restore_vcpu.vtimer,
                    );
                }
                return std::process::ExitCode::FAILURE;
            }
            let post_restore_gic = match vmm.canonical_arm64_gic_state() {
                Ok(Some(gic)) => gic,
                Ok(None) => {
                    eprintln!("restored KVM VM lost its in-kernel vGIC state");
                    return std::process::ExitCode::FAILURE;
                }
                Err(error) => {
                    eprintln!("cannot read post-restore architectural GIC state: {error}");
                    return std::process::ExitCode::FAILURE;
                }
            };
            if let Err(difference) = compare_gic_architecture(&pre_restore_gic, &post_restore_gic) {
                eprintln!("independent GIC comparator rejected restore: {difference:?}");
                return std::process::ExitCode::FAILURE;
            }
            let mut planted = post_restore_gic.clone();
            let Ok(planted_index) = usize::try_from(planted.timer_intid) else {
                eprintln!("canonical timer INTID does not fit this host's usize");
                return std::process::ExitCode::FAILURE;
            };
            planted.priority[planted_index] ^= 1;
            let planted_difference = match compare_gic_architecture(&pre_restore_gic, &planted) {
                Err(difference)
                    if difference.field == "priority"
                        && difference.index == Some(planted_index) =>
                {
                    difference
                }
                Err(difference) => {
                    eprintln!("planted GIC negative localized incorrectly: {difference:?}");
                    return std::process::ExitCode::FAILURE;
                }
                Ok(()) => {
                    eprintln!("independent GIC comparator accepted planted priority corruption");
                    return std::process::ExitCode::FAILURE;
                }
            };
            println!(
                "KVM_VGIC_ROUNDTRIP state_hash={} architectural=ok planted_field={} \
                 planted_index={}",
                hex(&post_restore_hash),
                planted_difference.field,
                planted_difference.index.unwrap_or_default(),
            );

            if let Err(error) = vmm.checkpoint_virtual_time_trace() {
                eprintln!("cannot checkpoint production virtual_time trace: {error}");
                return std::process::ExitCode::FAILURE;
            }
            let trace = vmm
                .virtual_time_trace()
                .expect("virtual_time KVM composition wires a production trace");
            if let Some(path) = normalized_log_path.as_deref()
                && let Err(error) = write_normalized_log(std::path::Path::new(path), trace)
            {
                eprintln!("cannot write normalized log {path:?}: {error}");
                return std::process::ExitCode::FAILURE;
            }
            if let Err(error) = check_delivery_placement(trace.schedule(), trace.normalized_log()) {
                eprintln!("production delivery-placement oracle failed: {error}");
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
                return std::process::ExitCode::FAILURE;
            }

            // Required planted negative on the exact production workload:
            // move every delivered tick one exit late. Identically late twins
            // still compare equal, while the independent schedule oracle must
            // reject that shared error at the same genuine boundary.
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
                    return std::process::ExitCode::FAILURE;
                };
                next.interrupts.extend_from_slice(&logged.interrupts);
            }
            let late_peer = late.clone();
            if let Err(error) = compare_normalized_logs(&late, &late_peer) {
                eprintln!("identically late negative logs unexpectedly diverged: {error}");
                return std::process::ExitCode::FAILURE;
            }
            let divergence = match compare_normalized_logs(original, &late) {
                Err(divergence) if divergence.field == LogField::Interrupts => divergence,
                Err(divergence) => {
                    eprintln!("late-log comparator reported wrong field: {divergence}");
                    return std::process::ExitCode::FAILURE;
                }
                Ok(()) => {
                    eprintln!("normalized comparator accepted a one-exit-late production log");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let placement = match check_delivery_placement(trace.schedule(), &late) {
                Err(error) => error,
                Ok(()) => {
                    eprintln!("placement checker accepted a one-exit-late production log");
                    return std::process::ExitCode::FAILURE;
                }
            };
            let Some(late_placement_event) = placement_event(&placement) else {
                eprintln!("late-log placement failure had no exact event: {placement}");
                return std::process::ExitCode::FAILURE;
            };
            if late_placement_event != divergence.event_index {
                eprintln!(
                    "negative oracles disagree: comparator event {}, placement event {}",
                    divergence.event_index, late_placement_event
                );
                return std::process::ExitCode::FAILURE;
            }
            let checkpoints = original
                .events
                .iter()
                .filter(|logged| logged.state_hash.is_some())
                .count();
            println!(
                "KVM_M1_ORACLE events={} raw={} schedules={} deliveries={} checkpoints={} \
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
            println!(
                "KVM_ARM64_BOOT_READY event={event} state_hash={}",
                hex(&vmm.state_hash()),
            );
            for (label, digest) in vmm.state_components() {
                println!("KVM_STATE_COMPONENT label={label} digest={}", hex(&digest));
            }
            if let Some(path) = std::env::var_os("HARMONY_DUMP_RAM")
                && let Err(error) = std::fs::write(&path, vmm.guest_memory())
            {
                eprintln!("cannot write diagnostic guest RAM {path:?}: {error}");
                return std::process::ExitCode::FAILURE;
            }
            return std::process::ExitCode::SUCCESS;
        }
        if let Step::Terminal(reason) = step {
            eprintln!("KVM boot stopped before /init marker at event {event}: {reason:?}");
            return std::process::ExitCode::FAILURE;
        }
    }

    if let Some(path) = normalized_log_path.as_deref() {
        if let Err(error) = vmm.checkpoint_virtual_time_trace() {
            eprintln!("cannot checkpoint bounded virtual_time trace: {error}");
            return std::process::ExitCode::FAILURE;
        }
        let trace = vmm
            .virtual_time_trace()
            .expect("virtual_time KVM composition wires a production trace");
        if let Err(error) = write_normalized_log(std::path::Path::new(path), trace) {
            eprintln!("cannot write bounded normalized log {path:?}: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }
    eprintln!("KVM boot reached {max_events} events before /init marker");
    std::process::ExitCode::FAILURE
}

#[cfg(all(target_os = "linux", target_arch = "aarch64", not(miri)))]
fn write_normalized_log(
    path: &std::path::Path,
    trace: &vmm_core::virtual_time::LiveVirtualTimeTrace,
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

#[cfg(not(all(target_os = "linux", target_arch = "aarch64", not(miri))))]
fn main() -> std::process::ExitCode {
    eprintln!("kvm_arm64_boot requires a Linux/aarch64 host outside Miri");
    std::process::ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::contains_complete_ready_line;

    #[test]
    fn ready_marker_requires_its_complete_serial_line() {
        let ready = b"N6_GUEST_OK arch=arm64";

        assert!(!contains_complete_ready_line(
            b"boot\nN6_GUEST_OK arch=arm64 rows=9/9",
            ready,
        ));
        assert!(contains_complete_ready_line(
            b"boot\nN6_GUEST_OK arch=arm64 rows=9/9\n",
            ready,
        ));
    }
}
