// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live `KvmBackend` integration tests (gates 6–9).
//!
//! `#[cfg(target_os = "linux")]` + `#[ignore]` so the standard gates (which run
//! `cargo test … --all-features`) **compile but do not run** them — a Cargo
//! feature would be flipped on by `--all-features` and trip the fail-fast on a
//! Mac/CI host. Run explicitly on the determinism box, **CPU-pinned** per
//! `docs/HARDWARE-TESTING.md`; choose an idle core on the qualified host:
//!
//! ```sh
//! ssh <qualified-host> 'taskset -c 1 cargo test -p vmm-backend --test kvm_smoke -- --ignored --test-threads=1'
//! ```
//!
//! **Fail-fast, never skip:** on a host without a usable `/dev/kvm` these panic
//! with what is missing and where to run them, rather than silently passing.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use vmm_backend::{
    Backend, CommonExit, CpuidModel, Exit, ExitCounts, Gpa, KvmBackend, MsrFilter, MsrRange,
    VcpuState, X86Exit, X86Policy,
};

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

/// Fail-fast guard: build a `KvmBackend` or panic with where to run this.
fn new_backend_or_explain() -> KvmBackend {
    if !std::path::Path::new("/dev/kvm").exists() {
        panic!(
            "/dev/kvm missing — these live tests need Linux x86-64 with KVM. \
             Run on the determinism box: ssh <qualified-host> 'taskset -c 1 cargo test -p vmm-backend \
             --test kvm_smoke -- --ignored --test-threads=1'"
        );
    }
    KvmBackend::new().unwrap_or_else(|e| {
        panic!(
            "KvmBackend::new failed ({e}); these need /dev/kvm + VMX on the determinism box \
             (taskset -c 1), or a GitHub x86 runner with KVM access. Not runnable on macOS."
        )
    })
}

/// Minimal frozen CPUID model and a permissive-but-real MSR filter for bring-up.
/// `allow_inkernel` names a couple of harmless MSR ranges KVM keeps servicing;
/// every other MSR (including the gate-8 probe) traps to userspace.
fn configure(backend: &mut KvmBackend) {
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
}

/// Put the vCPU into flat real mode with `rip` at `entry` (linear == GPA, paging
/// off), via the trait's save/restore. Returns nothing; mutates the vCPU.
fn enter_real_mode_at(backend: &mut KvmBackend, entry: u64) {
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.regs.rip = entry;
    st.regs.rflags = 0x2; // reserved bit set, the minimal valid RFLAGS
    backend.restore(&st).expect("restore setup state");
}

/// Map one guest image, install the frozen policy, and place the vCPU at its
/// real-mode entry point. The backing allocation remains owned by the caller
/// until the backend is dropped, preserving the raw mapping's lifetime.
fn setup_real_mode(backend: &mut KvmBackend, mem: &mut GuestMem, code: &[u8]) {
    // SAFETY: mem is page-aligned, remains allocated until after backend is
    // dropped, and no host slice aliases it while the guest runs.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    configure(backend);
    backend.write_guest(Gpa(0x1000), code).expect("load stub");
    enter_real_mode_at(backend, 0x1000);
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn serviced_pio_is_exactly_snapshottable_without_guest_execution() {
    // Each mode reaches the same I/O boundary. INC BX immediately after I/O
    // witnesses any guest execution during completion retirement or capture.
    for read in [false, true] {
        let code = [
            0xBA,
            0xF8,
            0x03,
            0xB0,
            0x42,
            if read { 0xEC } else { 0xEE },
            0x43,
            0xF4,
        ];
        let mut snapshot = None;
        let mut endpoints = Vec::new();
        for mode in 0..3 {
            // RAM outlives the backend mapping, including backend destruction.
            let mut mem = GuestMem::new(0x10000);
            let mut backend = new_backend_or_explain();
            setup_real_mode(&mut backend, &mut mem, &code);
            let mut initial = backend.save().expect("initialize instruction witness");
            initial.regs.rbx = 0;
            backend.restore(&initial).expect("reset BX witness");
            if mode == 2 {
                backend
                    .restore(snapshot.as_ref().expect("retired snapshot"))
                    .expect("cold restore");
            } else {
                match backend.run().expect("run to I/O") {
                    Exit::Arch(X86Exit::Io {
                        port: 0x3F8,
                        size: 1,
                        write,
                    }) => {
                        assert_eq!(write, if read { None } else { Some(0x42) });
                    }
                    other => panic!("expected PIO, got {other:?}"),
                }
                if read {
                    backend.complete_read(0x77).expect("stage input");
                }
                if mode == 1 {
                    let before = backend.save().expect("pre-retirement state");
                    assert_eq!(before.regs.rbx, 0, "eager completion executes no INC");
                    assert_eq!(
                        before.regs.rip, 0x1006,
                        "serviced PIO has already completed"
                    );
                    assert_eq!(before.regs.rax & 0xff, if read { 0x77 } else { 0x42 });
                    let counts = backend.exit_counts();
                    backend.retire_pending_completion().expect("retire PIO");
                    let sealed = backend.save().expect("capture completed PIO");
                    assert_eq!(sealed.regs.rip, 0x1006, "stopped before INC BX");
                    assert_eq!(
                        sealed.regs.rbx, before.regs.rbx,
                        "retirement executes no INC"
                    );
                    assert_eq!(sealed.regs.rax & 0xff, if read { 0x77 } else { 0x42 });
                    assert_eq!(
                        backend.exit_counts(),
                        counts,
                        "retirement produces no guest exit"
                    );
                    backend
                        .retire_pending_completion()
                        .expect("repeat retirement is inert");
                    assert_eq!(
                        backend.save().unwrap(),
                        sealed,
                        "capture/retirement is a fixpoint"
                    );
                    assert_eq!(
                        before, sealed,
                        "explicit retirement cannot change a stopped boundary"
                    );
                    snapshot = Some(before);
                }
            }
            assert_eq!(
                backend.run().expect("continue to HLT"),
                Exit::Common(CommonExit::Idle)
            );
            let endpoint = backend.save().expect("endpoint");
            assert_eq!(
                endpoint.regs.rbx, 1,
                "INC executes exactly once on continuation"
            );
            endpoints.push(endpoint);
        }
        assert_eq!(
            endpoints[0], endpoints[1],
            "uninterrupted equals retire-save-continue"
        );
        assert_eq!(
            endpoints[0], endpoints[2],
            "uninterrupted equals retire-save-close-restore-continue"
        );
    }
}

const MSR_INDEX: u32 = 0x10;
const MSR_ENTRY: u64 = 0x1000;
const GP_VECTOR: u64 = 13;
const GP_HANDLER: u64 = 0x2000;
const READ_EAX: u32 = 0x5566_7788;
const READ_EDX: u32 = 0x1122_3344;
const READ_VALUE: u64 = (READ_EDX as u64) << 32 | READ_EAX as u64;
const WRITE_EAX: u32 = 0xDDEE_FF00;
const WRITE_EDX: u32 = 0xAABB_CCDD;
const WRITE_VALUE: u64 = (WRITE_EDX as u64) << 32 | WRITE_EAX as u64;

#[derive(Clone, Copy, Debug)]
enum MsrOperation {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug)]
enum MsrResponse {
    Success,
    Fault,
}

fn msr_guest_code(operation: MsrOperation) -> Vec<u8> {
    let mut code = Vec::with_capacity(match operation {
        MsrOperation::Read => 10,
        MsrOperation::Write => 22,
    });
    // Real mode defaults to 16-bit operands, so each 32-bit immediate move
    // needs the 0x66 operand-size override before its opcode.
    // `mov ecx, MSR_INDEX` makes the userspace filter decision observable.
    code.push(0x66);
    code.push(0xB9);
    code.extend_from_slice(&MSR_INDEX.to_le_bytes());
    match operation {
        MsrOperation::Read => code.extend_from_slice(&[0x0F, 0x32]), // RDMSR
        MsrOperation::Write => {
            // `mov eax, WRITE_EAX; mov edx, WRITE_EDX; wrmsr`.
            code.push(0x66);
            code.push(0xB8);
            code.extend_from_slice(&WRITE_EAX.to_le_bytes());
            code.push(0x66);
            code.push(0xBA);
            code.extend_from_slice(&WRITE_EDX.to_le_bytes());
            code.extend_from_slice(&[0x0F, 0x30]); // WRMSR
        }
    }
    // The witness immediately after the MSR instruction must not run during
    // completion retirement. A fault instead vectors to the handler below.
    code.extend_from_slice(&[0x43, 0xF4]); // INC BX; HLT
    code
}

fn msr_rip(operation: MsrOperation) -> u64 {
    MSR_ENTRY
        + match operation {
            MsrOperation::Read => 6,
            MsrOperation::Write => 18,
        }
}

fn setup_msr_guest(backend: &mut KvmBackend, mem: &mut GuestMem, operation: MsrOperation) {
    let code = msr_guest_code(operation);
    setup_real_mode(backend, mem, &code);

    // #GP vector 13 points to a handler that increments SI then halts. The
    // fault arm must reach this handler only after the completion boundary is
    // captured and resumed.
    backend
        .write_guest(Gpa(4 * GP_VECTOR), &[0x00, 0x20, 0x00, 0x00])
        .expect("load #GP IVT");
    backend
        .write_guest(Gpa(GP_HANDLER), &[0x46, 0xF4])
        .expect("load #GP handler");

    let mut initial = backend.save().expect("initialize instruction witnesses");
    initial.regs.rbx = 0;
    initial.regs.rsi = 0;
    backend.restore(&initial).expect("reset BX/SI witnesses");
}

fn run_to_msr(backend: &mut KvmBackend, operation: MsrOperation) -> (VcpuState, ExitCounts) {
    match (operation, backend.run().expect("run to MSR")) {
        (MsrOperation::Read, Exit::Arch(X86Exit::Rdmsr { index })) => {
            assert_eq!(
                index, MSR_INDEX,
                "RDMSR index must reach the userspace filter"
            );
        }
        (MsrOperation::Write, Exit::Arch(X86Exit::Wrmsr { index, value })) => {
            assert_eq!(
                index, MSR_INDEX,
                "WRMSR index must reach the userspace filter"
            );
            assert_eq!(value, WRITE_VALUE, "WRMSR must expose split EDX:EAX value");
        }
        (operation, other) => panic!("expected {operation:?} exit, got {other:?}"),
    }

    let before = backend.save().expect("save MSR boundary before completion");
    assert_eq!(before.regs.rip, msr_rip(operation));
    assert_eq!(before.regs.rcx, MSR_INDEX as u64);
    assert_eq!(before.regs.rbx, 0, "MSR exit must precede INC BX");
    assert_eq!(before.regs.rsi, 0, "MSR exit must precede #GP handler");
    let counts = backend.exit_counts();
    assert_eq!(counts.total(), 1, "the MSR exit is the first guest exit");
    match operation {
        MsrOperation::Read => assert_eq!(counts.rdmsr, 1),
        MsrOperation::Write => assert_eq!(counts.wrmsr, 1),
    }
    (before, counts)
}

fn complete_msr(backend: &mut KvmBackend, operation: MsrOperation, response: MsrResponse) {
    match (operation, response) {
        (MsrOperation::Read, MsrResponse::Success) => backend
            .complete_read(READ_VALUE)
            .expect("complete RDMSR with host value"),
        (MsrOperation::Write, MsrResponse::Success) => {
            backend.complete_ok().expect("complete WRMSR successfully")
        }
        (_, MsrResponse::Fault) => backend.complete_fault().expect("complete MSR with #GP"),
    }
}

fn assert_msr_completion_boundary(
    backend: &KvmBackend,
    operation: MsrOperation,
    response: MsrResponse,
    counts: ExitCounts,
) -> VcpuState {
    let state = backend.save().expect("save completed MSR boundary");
    match response {
        MsrResponse::Success => {
            assert_eq!(state.regs.rip, msr_rip(operation) + 2);
            assert_eq!(
                state.regs.rbx, 0,
                "success completion must stop before INC BX"
            );
            assert_eq!(
                state.regs.rsi, 0,
                "success completion must skip #GP handler"
            );
            match operation {
                MsrOperation::Read => {
                    assert_eq!(state.regs.rax, READ_EAX as u64);
                    assert_eq!(state.regs.rdx, READ_EDX as u64);
                }
                MsrOperation::Write => {
                    assert_eq!(state.regs.rax, WRITE_EAX as u64);
                    assert_eq!(state.regs.rdx, WRITE_EDX as u64);
                }
            }
        }
        MsrResponse::Fault => {
            assert_eq!(state.regs.rip, msr_rip(operation));
            assert_eq!(
                state.regs.rbx, 0,
                "fault completion must stop before INC BX"
            );
            assert_eq!(
                state.regs.rsi, 0,
                "fault completion must stop before #GP handler"
            );
            assert_eq!(state.events.exception_pending, 1, "#GP must remain pending");
            assert_eq!(state.events.exception_nr, GP_VECTOR as u8);
        }
    }
    assert_eq!(
        backend.exit_counts(),
        counts,
        "completion/capture must not add a guest exit"
    );
    state
}

fn capture_msr_boundary(
    backend: &mut KvmBackend,
    operation: MsrOperation,
    response: MsrResponse,
    counts: ExitCounts,
) -> VcpuState {
    let captured = assert_msr_completion_boundary(backend, operation, response, counts);
    backend
        .retire_pending_completion()
        .expect("retire completed MSR boundary");
    assert_eq!(
        backend.exit_counts(),
        counts,
        "retirement adds no guest exit"
    );
    assert_eq!(
        backend.save().expect("save after MSR retirement"),
        captured,
        "MSR retirement must preserve the captured boundary"
    );
    backend
        .retire_pending_completion()
        .expect("repeat MSR retirement is inert");
    assert_eq!(
        backend.exit_counts(),
        counts,
        "repeat retirement adds no exit"
    );
    assert_eq!(
        backend.save().expect("save after repeated MSR retirement"),
        captured,
        "repeated retirement must be a fixpoint"
    );
    captured
}

fn continue_msr_guest(
    backend: &mut KvmBackend,
    response: MsrResponse,
    counts_before_msr: ExitCounts,
) -> VcpuState {
    assert_eq!(
        backend.run().expect("continue MSR guest to HLT"),
        Exit::Common(CommonExit::Idle)
    );
    let endpoint = backend.save().expect("save MSR endpoint");
    match response {
        MsrResponse::Success => {
            assert_eq!(endpoint.regs.rbx, 1, "successful MSR resumes at INC BX");
            assert_eq!(endpoint.regs.rsi, 0, "successful MSR skips #GP handler");
        }
        MsrResponse::Fault => {
            assert_eq!(endpoint.regs.rbx, 0, "faulting MSR must skip INC BX");
            assert_eq!(endpoint.regs.rsi, 1, "faulting MSR must reach #GP handler");
        }
    }
    let counts = backend.exit_counts();
    assert_eq!(counts.total(), counts_before_msr.total() + 1);
    assert_eq!(
        counts.idle, 1,
        "only HLT is observable after the MSR completion"
    );
    endpoint
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn serviced_exception_payload_round_trips_and_empty_restore_clears_it() {
    // Exercise the real GET/SET event ABI independently of the MSR #GP path,
    // which has no payload. Neither loading nor clearing a pending page fault
    // may enter guest code or apply its payload to CR2 ahead of delivery.
    let mut mem = GuestMem::new(0x10000);
    let mut backend = new_backend_or_explain();
    setup_real_mode(&mut backend, &mut mem, &[0x43, 0xF4]); // INC BX; HLT
    let initial = backend.save().expect("initial state");
    let counts = backend.exit_counts();
    let mut pending = initial.clone();
    pending.events.exception_pending = 1;
    pending.events.exception_nr = 14;
    pending.events.exception_has_error_code = 1;
    pending.events.exception_error_code = 4;
    pending.events.exception_has_payload = 1;
    pending.events.exception_payload = 0x1234_5000;
    backend
        .restore(&pending)
        .expect("install pending page fault");
    assert_eq!(backend.save().expect("capture pending payload"), pending);
    assert_eq!(backend.exit_counts(), counts);

    // A fresh backend must preserve the same pending payload, while restoring
    // an empty record over it must replace (and clear) the displaced exception.
    let mut cold_mem = GuestMem::new(0x10000);
    let mut cold = new_backend_or_explain();
    setup_real_mode(&mut cold, &mut cold_mem, &[0x43, 0xF4]);
    let cold_counts = cold.exit_counts();
    cold.restore(&pending)
        .expect("cold restore pending payload");
    assert_eq!(cold.save().expect("capture cold payload"), pending);
    cold.restore(&initial).expect("clear pending payload");
    assert_eq!(cold.save().expect("capture cleared exception"), initial);
    assert_eq!(cold.exit_counts(), cold_counts);
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn serviced_msr_is_exactly_snapshottable_without_guest_execution() {
    // Each operation and response reaches one userspace MSR boundary. The
    // three continuation modes are uninterrupted, save-and-continue, and cold
    // restore; the same matrix covers RDMSR/WRMSR success and #GP responses.
    for operation in [MsrOperation::Read, MsrOperation::Write] {
        for response in [MsrResponse::Success, MsrResponse::Fault] {
            let mut snapshot = None;
            let mut endpoints = Vec::new();
            for mode in 0..3 {
                // RAM outlives the backend mapping, including backend
                // destruction between save-and-continue and cold restore.
                let mut mem = GuestMem::new(0x10000);
                let mut backend = new_backend_or_explain();
                setup_msr_guest(&mut backend, &mut mem, operation);
                if mode == 2 {
                    let captured = snapshot.as_ref().expect("captured MSR snapshot");
                    backend
                        .restore(captured)
                        .expect("cold restore MSR boundary");
                    assert_eq!(
                        backend.save().expect("save cold-restored MSR boundary"),
                        *captured,
                        "cold restore must reproduce the exact MSR VcpuState"
                    );
                } else {
                    let (_, counts) = run_to_msr(&mut backend, operation);
                    complete_msr(&mut backend, operation, response);
                    if mode == 1 {
                        snapshot = Some(capture_msr_boundary(
                            &mut backend,
                            operation,
                            response,
                            counts,
                        ));
                    } else {
                        assert_msr_completion_boundary(&backend, operation, response, counts);
                    }
                }
                let counts_before_hlt = backend.exit_counts();
                let endpoint = continue_msr_guest(&mut backend, response, counts_before_hlt);
                // Exception delivery also writes the guest stack. Compare RAM
                // after the vCPU is stopped so a restored fault cannot silently
                // produce a different frame while matching its final registers.
                endpoints.push((endpoint, mem.as_mut_slice().to_vec()));
            }
            assert_eq!(
                endpoints[0], endpoints[1],
                "{operation:?}/{response:?}: uninterrupted equals save-and-continue"
            );
            assert_eq!(
                endpoints[0], endpoints[2],
                "{operation:?}/{response:?}: uninterrupted equals cold restore"
            );
        }
    }
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored"]
fn save_restore_round_trips_on_real_kvm() {
    let mut backend = new_backend_or_explain();
    let mut mem = GuestMem::new(0x10000);
    // SAFETY: as above.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    configure(&mut backend);

    // Set GPRs via restore, save, then prove restore→save is a fixpoint.
    let mut st = backend.save().expect("save");
    st.regs.rax = 0xDEAD_BEEF_CAFE_F00D;
    st.regs.rbx = 0x0123_4567_89AB_CDEF;
    st.regs.rip = 0x1000;
    backend.restore(&st).expect("restore");

    let a = backend.save().expect("save a");
    assert_eq!(a.regs.rax, 0xDEAD_BEEF_CAFE_F00D);
    assert_eq!(a.regs.rbx, 0x0123_4567_89AB_CDEF);

    // The full allow-stateful MSR set was captured (get_msrs got == requested):
    // the 3 SYSENTER MSRs from `configure`, none silently dropped.
    assert_eq!(a.msrs.len(), 3, "all allow-stateful MSRs captured");
    // The XSAVE image is the host-sized XSAVE2 buffer (>= the 4 KiB legacy size),
    // not a fixed 4 KiB truncation.
    assert!(a.xsave.len() >= 4096, "host-sized XSAVE2 image");

    backend.restore(&a).expect("restore a");
    let b = backend.save().expect("save b");
    // The fixpoint now spans SREGS2 (incl. flags/PDPTRs) and the full XSAVE2 image.
    assert_eq!(a, b, "restore→save must be a fixpoint on real KVM");
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored"]
fn msr_filter_is_loud() {
    // Real-mode stub at 0x1000:
    //   mov ecx, 0x12345678   (66 b9 ..)   ; denied MSR index
    //   rdmsr                 (0f 32)
    //   mov al, 0x99          (b0 99)       ; only reached if rdmsr *silently allowed*
    //   out 0x10, al          (e6 10)       ; -> X86Exit::Io (the silent-value path)
    //   hlt                   (f4)
    let code: &[u8] = &[
        0x66, 0xB9, 0x78, 0x56, 0x34, 0x12, 0x0F, 0x32, 0xB0, 0x99, 0xE6, 0x10, 0xF4,
    ];
    // Real-mode IVT entry for #GP (vector 13) at physical 13*4 = 0x34: offset
    // 0x2000, segment 0x0000.
    let gp_ivt: &[u8] = &[0x00, 0x20, 0x00, 0x00];
    // The #GP handler at 0x2000: a single HLT — reached only if the fault is
    // actually delivered.
    let gp_handler: &[u8] = &[0xF4];

    let mut backend = new_backend_or_explain();
    let mut mem = GuestMem::new(0x10000);
    // SAFETY: as above.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    configure(&mut backend);
    backend
        .write_guest(Gpa(0x34), gp_ivt)
        .expect("load #GP IVT");
    backend.write_guest(Gpa(0x1000), code).expect("load stub");
    backend
        .write_guest(Gpa(0x2000), gp_handler)
        .expect("load handler");
    enter_real_mode_at(&mut backend, 0x1000);

    // The denied RDMSR surfaces loudly to userspace, not a silent in-kernel value.
    match backend.run().expect("run to RDMSR") {
        Exit::Arch(X86Exit::Rdmsr { index: 0x1234_5678 }) => {}
        other => panic!("expected RDMSR exit for the denied index, got {other:?}"),
    }
    // Deny it (#GP). The fault vectors through IVT[13] to the HLT handler, so the
    // next exit is HLT — proving the guest took the fault. A silent in-kernel
    // value instead would have advanced past RDMSR into the `out 0x10` and
    // surfaced X86Exit::Io, which would fail this assertion loudly.
    backend.complete_fault().expect("complete_fault");
    match backend.run().expect("run after #GP") {
        Exit::Common(CommonExit::Idle) => {}
        Exit::Arch(X86Exit::Io { port, .. }) => {
            panic!("RDMSR was silently allowed (reached out 0x{port:x}) — filter not loud")
        }
        other => panic!("expected HLT from the #GP handler, got {other:?}"),
    }
    assert_eq!(backend.exit_counts().rdmsr, 1);
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored"]
fn capabilities_report_the_stock_backend_name() {
    let backend = new_backend_or_explain();
    let caps = backend.capabilities();
    assert_eq!(caps.name, "kvm-stock");
}
