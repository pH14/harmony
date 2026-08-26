// SPDX-License-Identifier: AGPL-3.0-or-later
//! M0 oracles for assigned-at-exit V-time.  Every comparator used by the
//! positive properties is also driven against a deliberately perturbed twin.

use proptest::prelude::*;
use sha2::{Digest, Sha256};
use vmm_backend::{
    Backend, CommonExit, Exit, Gpa, HypercallFrame, Injection, MockBackend, VcpuState, X86,
    X86Exit, X86Policy,
};
use vmm_core::prescriptive::{
    ClassifiedExit, DeviceClass, LogField, NormalizedEventClass, PlacementViolation,
    PrescriptiveCheckpoint, PrescriptiveError, PrescriptiveRunLoop, PrescriptiveTiming,
    check_delivery_placement, compare_normalized_logs,
};
use vmm_core::vmm::{GuestRam, Vmm, VtimeWiring};
use vtime::VClockConfig;

fn clock_config() -> VClockConfig {
    VClockConfig {
        ratio_num: 1,
        ratio_den: 1,
        guest_hz: 1_000_000_000,
        guest_base: 0,
        vns_base: 0,
    }
}

fn timing() -> PrescriptiveTiming {
    PrescriptiveTiming {
        interrupt_controller_mmio_vns: 5,
        serial_mmio_vns: 5,
        paravirtual_device_mmio_vns: 7,
        trapped_time_read_vns: 2,
    }
}

fn doorbell(duration_vns: u64, marker: u64) -> Exit<X86> {
    Exit::Common(CommonExit::Hypercall(HypercallFrame {
        args: [0x3150_4348, marker, 0, duration_vns],
    }))
}

fn mmio(marker: u64) -> Exit<X86> {
    Exit::Common(CommonExit::Mmio {
        gpa: Gpa(0x0800_0000 + marker),
        size: 4,
        write: Some(marker),
    })
}

fn configured_loop(
    exits: Vec<Exit<X86>>,
    checkpoint_every: u64,
) -> PrescriptiveRunLoop<MockBackend> {
    let mut backend = MockBackend::with_exits(exits);
    backend.set_policy(&X86Policy::default()).unwrap();
    PrescriptiveRunLoop::new(backend, clock_config(), timing(), checkpoint_every).unwrap()
}

fn classify(
    backend: &mut MockBackend,
    exit: &Exit<X86>,
) -> Result<ClassifiedExit, PrescriptiveError> {
    match exit {
        Exit::Common(CommonExit::Hypercall(frame)) => {
            backend.complete_hypercall(0)?;
            let mut payload = Vec::new();
            for arg in frame.args {
                payload.extend_from_slice(&arg.to_le_bytes());
            }
            Ok(ClassifiedExit::doorbell(payload, frame.args[3]))
        }
        Exit::Common(CommonExit::Mmio {
            gpa,
            size,
            write: Some(value),
        }) => {
            let mut payload = gpa.0.to_le_bytes().to_vec();
            payload.push(*size);
            payload.extend_from_slice(&value.to_le_bytes());
            Ok(ClassifiedExit::device_mmio(
                DeviceClass::InterruptController,
                payload,
            ))
        }
        Exit::Arch(X86Exit::Rdtsc) => {
            backend.complete_read(0)?;
            Ok(ClassifiedExit::time_read(b"rdtsc".to_vec()))
        }
        Exit::Common(CommonExit::Idle) => Ok(ClassifiedExit::idle(b"wfi".to_vec())),
        Exit::Common(CommonExit::Shutdown) => Ok(ClassifiedExit::terminal(b"shutdown".to_vec())),
        other => Err(PrescriptiveError::Classification(format!(
            "unmodeled test exit: {other:?}"
        ))),
    }
}

fn checkpoint_hash(backend: &MockBackend, checkpoint: PrescriptiveCheckpoint) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"m0-test-full-state-v1\0");
    hasher.update(checkpoint.vns.to_le_bytes());
    hasher.update(checkpoint.pending_interrupts.to_le_bytes());
    hasher.update(checkpoint.event_index.to_le_bytes());
    hasher.update(format!("{:?}", backend.save().unwrap()).as_bytes());
    hasher.finalize().into()
}

fn deliver(
    backend: &mut MockBackend,
    delivery: vmm_core::prescriptive::InterruptDelivery,
) -> Result<(), PrescriptiveError> {
    let vector = u8::try_from(delivery.interrupt_id).map_err(|_| {
        PrescriptiveError::Classification(format!(
            "test interrupt {} does not fit x86 vector width",
            delivery.interrupt_id
        ))
    })?;
    backend.inject(Injection::Interrupt { vector })?;
    Ok(())
}

fn drive_once(loop_: &mut PrescriptiveRunLoop<MockBackend>) -> NormalizedEventClass {
    loop_
        .run_backend_once(classify, deliver, checkpoint_hash)
        .unwrap()
        .class
}

fn run_to_terminal(loop_: &mut PrescriptiveRunLoop<MockBackend>) {
    loop {
        if drive_once(loop_) == NormalizedEventClass::Terminal {
            break;
        }
    }
}

fn script_for_deltas(deltas: &[u64]) -> Vec<Exit<X86>> {
    let mut exits: Vec<_> = deltas
        .iter()
        .enumerate()
        .map(|(index, delta)| doorbell(*delta, u64::try_from(index).unwrap()))
        .collect();
    exits.push(Exit::Common(CommonExit::Shutdown));
    exits
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn assigned_clock_is_monotonic_and_matches_saturating_sum(
        deltas in prop::collection::vec(any::<u64>(), 1..64)
    ) {
        let mut loop_ = configured_loop(script_for_deltas(&deltas), 7);
        run_to_terminal(&mut loop_);

        let mut expected = 0u64;
        for (index, event) in loop_.normalized_log().events.iter().enumerate() {
            if index < deltas.len() {
                expected = expected.saturating_add(deltas[index]);
            }
            prop_assert_eq!(event.vns_after, expected);
        }
        for pair in loop_.normalized_log().events.windows(2) {
            prop_assert!(pair[0].vns_after <= pair[1].vns_after);
        }
    }

    #[test]
    fn generated_schedules_deliver_at_the_first_eligible_exit(
        deltas in prop::collection::vec(1u64..100, 1..48),
        deadline_seeds in prop::collection::vec(any::<u16>(), 1..48),
    ) {
        let total = deltas.iter().copied().fold(0u64, u64::saturating_add);
        let mut loop_ = configured_loop(script_for_deltas(&deltas), 5);
        for (index, seed) in deadline_seeds.iter().enumerate() {
            let deadline = u64::from(*seed) % total.saturating_add(1);
            loop_.schedule_interrupt(deadline, u32::try_from(index).unwrap()).unwrap();
        }
        run_to_terminal(&mut loop_);
        prop_assert_eq!(
            check_delivery_placement(loop_.schedule(), loop_.normalized_log()),
            Ok(())
        );
    }
}

#[test]
fn dedicated_mask_wfi_simultaneous_and_reassertion_workload() {
    let exits = vec![
        mmio(1),                        // vns 5: masked-at-deadline
        Exit::Arch(X86Exit::Rdtsc),     // vns 7: simultaneous pair
        Exit::Arch(X86Exit::Rdtsc),     // vns 9: first assertion while masked
        mmio(2),                        // vns 14: unmask + reassertion
        Exit::Common(CommonExit::Idle), // vns 14: WFI with already-due deadline
        Exit::Common(CommonExit::Shutdown),
    ];
    let mut loop_ = configured_loop(exits, 2);
    let masked = loop_.schedule_interrupt(5, 40).unwrap();
    let simultaneous_a = loop_.schedule_interrupt(7, 41).unwrap();
    let simultaneous_b = loop_.schedule_interrupt(7, 42).unwrap();
    let first_assertion = loop_.schedule_interrupt(9, 50).unwrap();
    let reassertion = loop_.schedule_interrupt(14, 50).unwrap();

    for _ in 0..4 {
        drive_once(&mut loop_);
    }
    // Schedule at the current boundary, then WFI.  With no intervening exit,
    // WFI is the first exit whose post-advance V-time is at the deadline.
    let wfi_due = loop_.schedule_interrupt(14, 60).unwrap();
    run_to_terminal(&mut loop_);

    let events = &loop_.normalized_log().events;
    assert_eq!(events[0].interrupts, vec![masked.into()]);
    assert_eq!(
        events[1].interrupts,
        vec![simultaneous_a.into(), simultaneous_b.into()],
        "equal deadlines must retain FIFO schedule order"
    );
    assert_eq!(events[2].interrupts, vec![first_assertion.into()]);
    assert_eq!(events[3].interrupts, vec![reassertion.into()]);
    assert_eq!(events[4].interrupts, vec![wfi_due.into()]);
    assert_eq!(
        loop_.backend().injected().len(),
        6,
        "every logical delivery must be raised through the backend/fabric seam"
    );
    assert_eq!(
        check_delivery_placement(loop_.schedule(), loop_.normalized_log()),
        Ok(())
    );
}

#[test]
fn identical_scripts_have_identical_complete_normalized_logs() {
    let script = script_for_deltas(&[3, 0, 7, 11]);
    let mut left = configured_loop(script.clone(), 2);
    let mut right = configured_loop(script, 2);
    left.schedule_interrupt(3, 1).unwrap();
    right.schedule_interrupt(3, 1).unwrap();
    left.schedule_interrupt(21, 2).unwrap();
    right.schedule_interrupt(21, 2).unwrap();
    run_to_terminal(&mut left);
    run_to_terminal(&mut right);

    assert_eq!(
        compare_normalized_logs(left.normalized_log(), right.normalized_log()),
        Ok(())
    );
    assert_eq!(
        check_delivery_placement(left.schedule(), left.normalized_log()),
        Ok(())
    );
    assert_eq!(
        check_delivery_placement(right.schedule(), right.normalized_log()),
        Ok(())
    );
}

#[test]
fn comparator_rejects_one_vns_increment_at_the_exact_event() {
    let mut run = configured_loop(script_for_deltas(&[3, 4, 5]), 2);
    run_to_terminal(&mut run);
    let good = run.normalized_log().clone();
    let mut perturbed = good.clone();
    perturbed.events[1].vns_after += 1;

    let error = compare_normalized_logs(&good, &perturbed).unwrap_err();
    assert_eq!(error.event_index, 1);
    assert_eq!(error.field, LogField::VnsAfter);
}

#[test]
fn comparator_rejects_a_one_exit_late_interrupt_at_the_exact_event() {
    let mut run = configured_loop(script_for_deltas(&[5, 1]), 1);
    run.schedule_interrupt(5, 9).unwrap();
    run_to_terminal(&mut run);
    let good = run.normalized_log().clone();
    let mut late = good.clone();
    let delivery = late.events[0].interrupts.remove(0);
    late.events[1].interrupts.push(delivery);

    let error = compare_normalized_logs(&good, &late).unwrap_err();
    assert_eq!(error.event_index, 0);
    assert_eq!(error.field, LogField::Interrupts);
}

#[test]
fn comparator_rejects_one_flipped_guest_state_byte_at_the_exact_checkpoint() {
    let script = script_for_deltas(&[1, 1, 1]);
    let mut good_run = configured_loop(script.clone(), 2);
    let mut perturbed_run = configured_loop(script, 2);
    drive_once(&mut good_run);
    drive_once(&mut perturbed_run);
    drive_once(&mut good_run);
    perturbed_run
        .run_backend_once(
            |backend, exit| {
                let classified = classify(backend, exit)?;
                let mut state = VcpuState::default();
                state.regs.rax = 0x80;
                backend.set_state(state);
                Ok(classified)
            },
            deliver,
            checkpoint_hash,
        )
        .unwrap();

    let error = compare_normalized_logs(good_run.normalized_log(), perturbed_run.normalized_log())
        .unwrap_err();
    assert_eq!(error.event_index, 1);
    assert_eq!(error.field, LogField::StateHash);
}

#[test]
fn placement_checker_rejects_consistently_late_twins_that_comparator_accepts() {
    let mut run = configured_loop(script_for_deltas(&[5, 1, 1]), 1);
    run.schedule_interrupt(5, 9).unwrap();
    run.schedule_interrupt(6, 10).unwrap();
    run_to_terminal(&mut run);
    let mut late_a = run.normalized_log().clone();
    let first = late_a.events[0].interrupts.remove(0);
    let second = late_a.events[1].interrupts.remove(0);
    late_a.events[1].interrupts.push(first);
    late_a.events[2].interrupts.push(second);
    let late_b = late_a.clone();

    assert_eq!(compare_normalized_logs(&late_a, &late_b), Ok(()));
    let error = check_delivery_placement(run.schedule(), &late_a).unwrap_err();
    assert!(matches!(
        error,
        PlacementViolation::WrongDelivery { event_index: 0, .. }
    ));
}

#[test]
fn placement_checker_rejects_duplicate_and_undelivered_deadlines() {
    let mut run = configured_loop(script_for_deltas(&[5]), 1);
    run.schedule_interrupt(5, 9).unwrap();
    run_to_terminal(&mut run);

    let mut duplicate = run.normalized_log().clone();
    duplicate.events[1].interrupts = duplicate.events[0].interrupts.clone();
    assert!(matches!(
        check_delivery_placement(run.schedule(), &duplicate),
        Err(PlacementViolation::WrongDelivery { event_index: 1, .. })
    ));

    let mut missing = run.normalized_log().clone();
    missing.events[0].interrupts.clear();
    assert!(matches!(
        check_delivery_placement(run.schedule(), &missing),
        Err(PlacementViolation::WrongDelivery { event_index: 0, .. })
    ));
}

fn has_chunk(blob: &[u8], wanted: &[u8; 4]) -> bool {
    let mut at = 0usize;
    while let Some(header_end) = at.checked_add(12) {
        let Some(header) = blob.get(at..header_end) else {
            return false;
        };
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&header[4..12]);
        let Ok(len) = usize::try_from(u64::from_le_bytes(len_bytes)) else {
            return false;
        };
        if &header[..4] == wanted {
            return true;
        }
        let Some(next) = header_end.checked_add(len) else {
            return false;
        };
        if next > blob.len() {
            return false;
        }
        at = next;
    }
    false
}

fn state_vmm(vns: u64, seed: u64) -> Vmm<MockBackend> {
    let mut backend = MockBackend::new();
    backend.set_policy(&X86Policy::default()).unwrap();
    let mut wiring = VtimeWiring::new_prescriptive(clock_config(), seed).unwrap();
    wiring.advance_prescriptive(vns);
    assert_eq!(wiring.prescriptive_vns(), vns);
    let mut vmm = Vmm::new(backend, GuestRam::new(4096).unwrap());
    vmm.wire_vtime(wiring);
    vmm
}

#[test]
fn state_blob_carries_assigned_vtime_and_entropy_at_work_zero() {
    let baseline = state_vmm(17, 0xCAFE);
    let same = state_vmm(17, 0xCAFE);
    let changed_vtime = state_vmm(18, 0xCAFE);
    let changed_entropy = state_vmm(17, 0xBABE);

    assert!(has_chunk(&baseline.state_blob(), b"VTIM"));
    assert_eq!(baseline.state_hash(), same.state_hash());
    assert_ne!(baseline.state_hash(), changed_vtime.state_hash());
    assert_ne!(baseline.state_hash(), changed_entropy.state_hash());

    let labels: Vec<_> = baseline
        .state_components()
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert!(labels.contains(&"vtim:eff-vns"));
    assert!(labels.contains(&"vtim:entropy"));
}
