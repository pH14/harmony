// SPDX-License-Identifier: AGPL-3.0-or-later
//! X1 live oracles: the prescriptive run loop drives the stock-KVM x86 backend
//! through a real-mode guest whose `OUT` stream carries the prescribed
//! durations, with timer delivery injected at exits through the guest IVT.
//!
//! `#[ignore]` so portable suites compile but do not run these; run explicitly
//! on a Linux x86-64 host with `/dev/kvm` (the `x86-vtime` workflow's
//! GitHub-hosted runners, or any KVM machine):
//!
//! ```sh
//! cargo test -p vmm-core --test x86_kvm_prescriptive -- --ignored --test-threads=1
//! ```
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use sha2::{Digest, Sha256};
use vmm_backend::Gpa;
use vmm_backend::{
    Backend, CpuidModel, Exit, Injection, KvmBackend, MsrFilter, MsrRange, X86, X86Exit, X86Policy,
};
use vmm_core::prescriptive::{
    ClassifiedExit, LogField, NormalizedLog, PlacementViolation, PrescriptiveCheckpoint,
    PrescriptiveError, PrescriptiveRunLoop, PrescriptiveTiming, ScheduledInterrupt,
    check_delivery_placement, compare_normalized_logs,
};
use vtime::VClockConfig;

/// Guest physical layout: IVT entry for `TIMER_VECTOR` at `4 * vector`, the
/// main program, the interrupt handler, and one counter byte the handler
/// increments — the in-guest witness that delivery really landed.
const CODE_GPA: u64 = 0x1000;
const HANDLER_GPA: u64 = 0x2000;
const COUNTER_GPA: usize = 0x3000;
const STACK_TOP: u64 = 0x7000;
const RAM_LEN: usize = 0x10000;
const TIMER_VECTOR: u32 = 0x20;

/// Doorbell port: each `OUT` value is the prescribed duration for that exit.
const DOORBELL_PORT: u8 = 0x10;
/// Terminal port: one `OUT` ends the run.
const POWEROFF_PORT: u8 = 0xF4;

/// The prescribed durations, one per doorbell round. Cumulative V-time
/// 3, 7, 12, 18, 25; the deadlines at 5 and 15 become due after events 1 and
/// 3, so both interrupts land while the guest still has rounds left to run
/// the handler in.
const DURATIONS: [u8; 5] = [3, 4, 5, 6, 7];
const DEADLINES: [u64; 2] = [5, 15];

/// One identity-mapped guest RAM region, page-aligned (the `map_memory` host
/// alignment invariant), reached by the backend through a raw pointer.
struct GuestMem {
    ptr: *mut u8,
    layout: std::alloc::Layout,
    len: usize,
}

impl GuestMem {
    fn new(len: usize) -> Self {
        assert_eq!(len % 4096, 0, "guest RAM must be page-sized");
        let layout = std::alloc::Layout::from_size_align(len, 4096).expect("layout");
        // SAFETY: non-zero size, power-of-two align.
        let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "guest RAM alloc failed");
        Self { ptr, layout, len }
    }
    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: `ptr`/`len` came from `alloc_zeroed`; exclusive borrow.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for GuestMem {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` from `alloc_zeroed`; freed once.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

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
        architectural_control_vns: 3,
    }
}

/// `sti`, then one doorbell `OUT` per duration, then the terminal `OUT`.
fn guest_program() -> Vec<u8> {
    let mut code = vec![0xFB];
    for duration in DURATIONS {
        code.extend_from_slice(&[0xB0, duration, 0xE6, DOORBELL_PORT]);
    }
    code.extend_from_slice(&[0xB0, 0x00, 0xE6, POWEROFF_PORT, 0xF4]);
    code
}

/// `inc byte [COUNTER_GPA]` then `iret`.
const HANDLER: [u8; 5] = [0xFE, 0x06, 0x00, 0x30, 0xCF];

fn new_backend_or_explain() -> KvmBackend {
    if !std::path::Path::new("/dev/kvm").exists() {
        panic!(
            "/dev/kvm missing — run on a Linux x86-64 KVM host \
             (the x86-vtime workflow runner grants access first)"
        );
    }
    KvmBackend::new().unwrap_or_else(|e| panic!("KvmBackend::new failed ({e})"))
}

fn classify(
    _backend: &mut KvmBackend,
    exit: &Exit<X86>,
) -> Result<ClassifiedExit, PrescriptiveError> {
    match exit {
        Exit::Arch(X86Exit::Io {
            port,
            size: 1,
            write: Some(value),
        }) if *port == u16::from(DOORBELL_PORT) => Ok(ClassifiedExit::doorbell(
            value.to_le_bytes().to_vec(),
            u64::from(*value),
        )),
        Exit::Arch(X86Exit::Io {
            port,
            size: 1,
            write: Some(_),
        }) if *port == u16::from(POWEROFF_PORT) => {
            Ok(ClassifiedExit::terminal(b"poweroff".to_vec()))
        }
        other => Err(PrescriptiveError::Classification(format!(
            "unmodeled X1 exit: {other:?}"
        ))),
    }
}

fn deliver(
    backend: &mut KvmBackend,
    delivery: vmm_core::prescriptive::InterruptDelivery,
) -> Result<(), PrescriptiveError> {
    let vector = u8::try_from(delivery.interrupt_id).map_err(|_| {
        PrescriptiveError::Classification(format!(
            "interrupt {} does not fit x86 vector width",
            delivery.interrupt_id
        ))
    })?;
    backend.inject(Injection::Interrupt { vector })?;
    Ok(())
}

fn checkpoint_hash(backend: &KvmBackend, checkpoint: PrescriptiveCheckpoint) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"x1-live-state-v1\0");
    hasher.update(checkpoint.vns.to_le_bytes());
    hasher.update(checkpoint.pending_interrupts.to_le_bytes());
    hasher.update(checkpoint.event_index.to_le_bytes());
    hasher.update(format!("{:?}", backend.save().unwrap()).as_bytes());
    hasher.finalize().into()
}

/// SHA-256 over every comparator-visible field of the normalized log, in a
/// fixed little-endian encoding, for cross-job digest comparison in CI logs.
fn log_digest(log: &NormalizedLog) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"x1-normalized-log-v1\0");
    h.update(u64::try_from(log.events.len()).unwrap().to_le_bytes());
    for event in &log.events {
        h.update(event.event_index.to_le_bytes());
        h.update(format!("{:?}", event.class).as_bytes());
        h.update(event.payload_digest);
        h.update(event.vns_after.to_le_bytes());
        h.update(u64::try_from(event.interrupts.len()).unwrap().to_le_bytes());
        for delivery in &event.interrupts {
            h.update(delivery.deadline_vns.to_le_bytes());
            h.update(delivery.schedule_index.to_le_bytes());
            h.update(delivery.interrupt_id.to_le_bytes());
        }
        h.update([u8::from(event.state_hash.is_some())]);
        if let Some(state_hash) = event.state_hash {
            h.update(state_hash);
        }
    }
    h.finalize().into()
}

struct RunResult {
    digest: [u8; 32],
    guest_deliveries: u8,
    log: NormalizedLog,
    schedule: Vec<ScheduledInterrupt>,
}

fn one_run() -> RunResult {
    let mut mem = GuestMem::new(RAM_LEN);
    let mut backend = new_backend_or_explain();
    // SAFETY: `mem` is declared before the loop that owns `backend`, so it
    // outlives every guest entry, is page-aligned, and is not touched from
    // this thread while the guest runs.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    backend
        .set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter {
                // SYSENTER MSRs (0x174..0x177) — present, harmless, in-kernel.
                allow_inkernel: vec![MsrRange {
                    base: 0x174,
                    count: 3,
                }],
            },
        })
        .expect("set_policy");

    // Real-mode IVT entry: handler offset then segment 0.
    let handler_offset = u16::try_from(HANDLER_GPA).unwrap();
    let mut ivt_entry = handler_offset.to_le_bytes().to_vec();
    ivt_entry.extend_from_slice(&[0x00, 0x00]);
    backend
        .write_guest(Gpa(u64::from(TIMER_VECTOR) * 4), &ivt_entry)
        .expect("load IVT entry");
    backend
        .write_guest(Gpa(CODE_GPA), &guest_program())
        .expect("load program");
    backend
        .write_guest(Gpa(HANDLER_GPA), &HANDLER)
        .expect("load handler");

    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.regs.rip = CODE_GPA;
    st.regs.rsp = STACK_TOP;
    st.regs.rflags = 0x2;
    backend.restore(&st).expect("restore setup state");

    let mut run_loop =
        PrescriptiveRunLoop::new(backend, clock_config(), timing(), 2).expect("run loop");
    for deadline in DEADLINES {
        run_loop
            .schedule_interrupt(deadline, TIMER_VECTOR)
            .expect("schedule");
    }
    loop {
        let event = run_loop
            .run_backend_once(classify, deliver, checkpoint_hash)
            .expect("run_backend_once");
        if event.class == vmm_core::prescriptive::NormalizedEventClass::Terminal {
            break;
        }
    }
    let digest = log_digest(run_loop.normalized_log());
    let log = run_loop.normalized_log().clone();
    let schedule = run_loop.schedule().to_vec();
    drop(run_loop);
    let guest_deliveries = mem.as_mut_slice()[COUNTER_GPA];
    RunResult {
        digest,
        guest_deliveries,
        log,
        schedule,
    }
}

#[test]
#[ignore = "live KVM; run with --ignored on a /dev/kvm host"]
fn x1_ten_same_seed_runs_produce_one_normalized_log() {
    let first = one_run();
    check_delivery_placement(&first.schedule, &first.log).expect("placement");
    assert_eq!(
        first.guest_deliveries,
        u8::try_from(DEADLINES.len()).unwrap(),
        "every scheduled interrupt must land in-guest through the IVT handler"
    );
    // Exact placement: deadline 5 becomes due after event 1 (vns 7), deadline
    // 15 after event 3 (vns 18).
    assert_eq!(first.log.events[1].interrupts.len(), 1);
    assert_eq!(first.log.events[3].interrupts.len(), 1);

    for _ in 1..10 {
        let run = one_run();
        compare_normalized_logs(&first.log, &run.log).expect("same-seed logs must be identical");
        assert_eq!(run.digest, first.digest);
        assert_eq!(run.guest_deliveries, first.guest_deliveries);
        check_delivery_placement(&run.schedule, &run.log).expect("placement");
    }
    let mut hex = String::new();
    for byte in first.digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    println!("X1_DIGEST={hex}");
    println!("X1_GUEST_DELIVERIES={}", first.guest_deliveries);
    println!("X1_EVENTS={}", first.log.events.len());
}

#[test]
#[ignore = "live KVM; run with --ignored on a /dev/kvm host"]
fn x1_comparator_catches_one_exit_late_delivery_on_this_workload() {
    let run = one_run();
    let mut late = run.log.clone();
    let delivery = late.events[1].interrupts.remove(0);
    late.events[2].interrupts.push(delivery);

    let error = compare_normalized_logs(&run.log, &late).unwrap_err();
    assert_eq!(error.event_index, 1);
    assert_eq!(error.field, LogField::Interrupts);
}

#[test]
#[ignore = "live KVM; run with --ignored on a /dev/kvm host"]
fn x1_placement_checker_catches_consistently_late_twins() {
    let run = one_run();
    let mut late = run.log.clone();
    let first = late.events[1].interrupts.remove(0);
    let second = late.events[3].interrupts.remove(0);
    late.events[2].interrupts.push(first);
    late.events[4].interrupts.push(second);
    let twin = late.clone();

    assert_eq!(compare_normalized_logs(&late, &twin), Ok(()));
    let error = check_delivery_placement(&run.schedule, &late).unwrap_err();
    assert!(matches!(
        error,
        PlacementViolation::WrongDelivery { event_index: 1, .. }
    ));
}
