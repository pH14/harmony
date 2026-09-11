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

const MMIO_GPA: u64 = 0xFEE0_0000;
const MMIO_SOURCE_GPA: u64 = 0x3000;
const SCALAR_READ_VALUE: u32 = 0xA1B2_C3D4;
const SCALAR_STORE_VALUE: u32 = 0xD4C3_B2A1;
const ADD_READ_VALUE: u32 = 0x1020_3040;
const ADD_IMMEDIATE: u8 = 5;
const MOVDQU_READ_WORDS: [u64; 2] = [0x8877_6655_4433_2211, 0x1100_FFEE_DDCC_BBAA];
const MOVDQU_STORE_WORDS: [u64; 2] = [0x0123_4567_89AB_CDEF, 0xFEDC_BA98_7654_3210];

#[derive(Clone, Copy, Debug)]
enum MmioOperation {
    ScalarLoad,
    ScalarStore,
    AddDword,
    MovdquLoad,
    MovdquStore,
}

impl MmioOperation {
    fn name(self) -> &'static str {
        match self {
            Self::ScalarLoad => "scalar-load",
            Self::ScalarStore => "scalar-store",
            Self::AddDword => "add-dword",
            Self::MovdquLoad => "movdqu-load",
            Self::MovdquStore => "movdqu-store",
        }
    }
}

/// Build a flat-real-mode instruction stream using 32-bit absolute addressing.
/// The returned offset is the RIP immediately after the MMIO instruction; the
/// two-byte `INC BX; HLT` witness follows it.
fn mmio_guest_code(operation: MmioOperation) -> (Vec<u8>, u64) {
    let mmio_addr = (MMIO_GPA as u32).to_le_bytes();
    let source_addr = (MMIO_SOURCE_GPA as u32).to_le_bytes();
    let mut code = Vec::new();
    match operation {
        // 67 66 A1 moffs32: MOV EAX, [0xFEE00000].
        MmioOperation::ScalarLoad => {
            code.extend_from_slice(&[0x67, 0x66, 0xA1]);
            code.extend_from_slice(&mmio_addr);
        }
        // MOV EAX, imm32; 67 66 A3 moffs32: MOV [0xFEE00000], EAX.
        MmioOperation::ScalarStore => {
            code.extend_from_slice(&[0x66, 0xB8]);
            code.extend_from_slice(&SCALAR_STORE_VALUE.to_le_bytes());
            code.extend_from_slice(&[0x67, 0x66, 0xA3]);
            code.extend_from_slice(&mmio_addr);
        }
        // 67 66 83 /0: ADD dword ptr [0xFEE00000], imm8. KVM must expose the
        // instruction as a load followed by a store of the updated dword.
        MmioOperation::AddDword => {
            code.extend_from_slice(&[0x67, 0x66, 0x83, 0x05]);
            code.extend_from_slice(&mmio_addr);
            code.push(ADD_IMMEDIATE);
        }
        // 67 F3 0F 6F /r: MOVDQU XMM0, [0xFEE00000]. KVM's eight-byte MMIO
        // payload is expected to expose this as two fragments.
        MmioOperation::MovdquLoad => {
            code.extend_from_slice(&[0x67, 0xF3, 0x0F, 0x6F, 0x05]);
            code.extend_from_slice(&mmio_addr);
        }
        // Load XMM0 from RAM first, then 67 F3 0F 7F /r: MOVDQU
        // [0xFEE00000], XMM0. The source at 0x3000 is also a RAM endpoint
        // witness for cold restore.
        MmioOperation::MovdquStore => {
            code.extend_from_slice(&[0x67, 0xF3, 0x0F, 0x6F, 0x05]);
            code.extend_from_slice(&source_addr);
            code.extend_from_slice(&[0x67, 0xF3, 0x0F, 0x7F, 0x05]);
            code.extend_from_slice(&mmio_addr);
        }
    }
    let mmio_end = code.len() as u64;
    code.extend_from_slice(&[0x43, 0xF4]); // INC BX; HLT
    (code, mmio_end)
}

fn expected_mmio_accesses(operation: MmioOperation) -> Vec<CommonExit> {
    let read = |gpa, size| CommonExit::Mmio {
        gpa: Gpa(gpa),
        size,
        write: None,
    };
    let write = |gpa, size, value| CommonExit::Mmio {
        gpa: Gpa(gpa),
        size,
        write: Some(value),
    };
    match operation {
        MmioOperation::ScalarLoad => vec![read(MMIO_GPA, 4)],
        MmioOperation::ScalarStore => vec![write(MMIO_GPA, 4, SCALAR_STORE_VALUE as u64)],
        MmioOperation::AddDword => vec![
            read(MMIO_GPA, 4),
            write(
                MMIO_GPA,
                4,
                u64::from(ADD_READ_VALUE.wrapping_add(u32::from(ADD_IMMEDIATE))),
            ),
        ],
        MmioOperation::MovdquLoad => vec![read(MMIO_GPA, 8), read(MMIO_GPA + 8, 8)],
        MmioOperation::MovdquStore => vec![
            write(MMIO_GPA, 8, MOVDQU_STORE_WORDS[0]),
            write(MMIO_GPA + 8, 8, MOVDQU_STORE_WORDS[1]),
        ],
    }
}

fn setup_mmio_guest(backend: &mut KvmBackend, mem: &mut GuestMem, operation: MmioOperation) -> u64 {
    let (code, mmio_end) = mmio_guest_code(operation);
    setup_real_mode(backend, mem, &code);

    // Keep an ordinary RAM source/witness mapped beside the code. The MOVDQU
    // store reads this source, and every mode compares the complete RAM image.
    let source: Vec<u8> = MOVDQU_STORE_WORDS
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    backend
        .write_guest(Gpa(MMIO_SOURCE_GPA), &source)
        .expect("load MMIO RAM source");

    // CR4.OSFXSR is required for MOVDQU in this real-mode guest. It is harmless
    // for the scalar and integer operations and keeps all five cases on the
    // same setup path.
    let mut initial = backend.save().expect("save MMIO setup state");
    initial.regs.rbx = 0;
    // The address-size prefix selects a 32-bit offset but does not enlarge
    // real mode's hidden segment limit. Model a flat unreal-mode data segment
    // so the access reaches the MMIO page rather than faulting on DS.limit.
    initial.sregs.ds.base = 0;
    initial.sregs.ds.limit = u32::MAX;
    // A 4 GiB effective limit requires page granularity in the hidden descriptor.
    initial.sregs.ds.g = 1;
    initial.sregs.cr4 |= 1 << 9; // OSFXSR
    backend.restore(&initial).expect("restore MMIO setup state");
    mmio_end
}

fn service_mmio(backend: &mut KvmBackend, operation: MmioOperation) -> Vec<CommonExit> {
    let expected = expected_mmio_accesses(operation);
    let mut observed = Vec::with_capacity(expected.len());
    let mut next =
        Some(backend.run().unwrap_or_else(|e| {
            panic!("{}: run to first MMIO exit failed: {e}", operation.name())
        }));

    while let Some(exit) = next {
        let access = match exit {
            Exit::Common(access @ CommonExit::Mmio { .. }) => access,
            other => panic!(
                "{}: expected MMIO continuation, got {other:?}",
                operation.name()
            ),
        };
        let index = observed.len();
        assert_eq!(access, expected[index], "{}: MMIO access", operation.name());
        observed.push(access);

        // A surfaced MMIO callback is still a transaction: `run` must not
        // resume the guest before the load is completed or the write is
        // retired. This also covers writes returned as queued continuations by
        // `finish_exit`, not only the first exit from KVM.
        let counts_before = backend.exit_counts();
        assert!(matches!(
            backend.run(),
            Err(vmm_backend::BackendError::PendingCompletion)
        ));
        assert_eq!(
            backend.exit_counts(),
            counts_before,
            "a rejected resume cannot add an MMIO exit"
        );

        if let CommonExit::Mmio { write: None, .. } = access {
            let value = match operation {
                MmioOperation::ScalarLoad => u64::from(SCALAR_READ_VALUE),
                MmioOperation::AddDword => u64::from(ADD_READ_VALUE),
                MmioOperation::MovdquLoad => MOVDQU_READ_WORDS[index],
                MmioOperation::ScalarStore | MmioOperation::MovdquStore => {
                    panic!("{}: unexpected read fragment", operation.name())
                }
            };
            backend
                .complete_read(value)
                .unwrap_or_else(|e| panic!("{}: complete MMIO read failed: {e}", operation.name()));
        }

        // This is the only operation between device accesses. It retires the
        // current callback with immediate-exit and may return the next fragment
        // of this same guest instruction. It never runs the successor.
        next = backend
            .finish_exit()
            .unwrap_or_else(|e| panic!("{}: finish MMIO exit failed: {e}", operation.name()));
    }

    assert_eq!(
        observed,
        expected,
        "{}: each MMIO access exactly once",
        operation.name()
    );
    observed
}

fn assert_mmio_completion_boundary(
    backend: &mut KvmBackend,
    operation: MmioOperation,
    mmio_end: u64,
    access_count: usize,
) -> VcpuState {
    let state = backend
        .save()
        .unwrap_or_else(|e| panic!("{}: save completion boundary failed: {e}", operation.name()));
    assert_eq!(
        state.regs.rip,
        0x1000 + mmio_end,
        "{}: RIP",
        operation.name()
    );
    assert_eq!(
        state.regs.rbx,
        0,
        "{}: INC BX ran too early",
        operation.name()
    );
    assert_eq!(
        backend.exit_counts().mmio,
        access_count as u64,
        "{}: MMIO exit count",
        operation.name()
    );
    match operation {
        MmioOperation::ScalarLoad => {
            assert_eq!(state.regs.rax, u64::from(SCALAR_READ_VALUE));
        }
        MmioOperation::ScalarStore => {
            assert_eq!(state.regs.rax, u64::from(SCALAR_STORE_VALUE));
        }
        MmioOperation::AddDword | MmioOperation::MovdquStore => {}
        MmioOperation::MovdquLoad => {
            let mut expected = [0u8; 16];
            expected[..8].copy_from_slice(&MOVDQU_READ_WORDS[0].to_le_bytes());
            expected[8..].copy_from_slice(&MOVDQU_READ_WORDS[1].to_le_bytes());
            assert!(
                state.xsave.len() >= 176,
                "{}: XSAVE too short",
                operation.name()
            );
            assert_eq!(
                &state.xsave[160..176],
                expected,
                "{}: XMM0",
                operation.name()
            );
        }
    }
    state
}

fn endpoint(
    backend: &KvmBackend,
    mem: &mut GuestMem,
    operation: MmioOperation,
) -> (VcpuState, Vec<u8>) {
    let state = backend
        .save()
        .unwrap_or_else(|e| panic!("{}: save endpoint failed: {e}", operation.name()));
    assert_eq!(
        state.regs.rbx,
        1,
        "{}: INC BX did not run once",
        operation.name()
    );
    (state, mem.as_mut_slice().to_vec())
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn serviced_mmio_is_exactly_snapshottable_across_scalar_rmw_and_movdqu() {
    for operation in [
        MmioOperation::ScalarLoad,
        MmioOperation::ScalarStore,
        MmioOperation::AddDword,
        MmioOperation::MovdquLoad,
        MmioOperation::MovdquStore,
    ] {
        let expected = expected_mmio_accesses(operation);
        let mut snapshot = None;
        let mut snapshot_ram: Option<Vec<u8>> = None;
        let mut endpoints = Vec::new();

        for mode in 0..3 {
            let mut mem = GuestMem::new(0x10000);
            let mut backend = new_backend_or_explain();
            let mmio_end = setup_mmio_guest(&mut backend, &mut mem, operation);

            if mode == 2 {
                let state = snapshot.as_ref().expect("MMIO boundary snapshot");
                let ram = snapshot_ram.as_ref().expect("MMIO boundary RAM snapshot");
                backend
                    .write_guest(Gpa(0), ram)
                    .expect("restore MMIO RAM snapshot");
                backend.restore(state).expect("cold restore MMIO boundary");
                assert_eq!(
                    backend.save().expect("save cold MMIO boundary"),
                    *state,
                    "{}: cold restore changes the completion boundary",
                    operation.name()
                );
            } else {
                let observed = service_mmio(&mut backend, operation);
                assert_eq!(
                    observed,
                    expected,
                    "{}: observed sequence",
                    operation.name()
                );
                let boundary = assert_mmio_completion_boundary(
                    &mut backend,
                    operation,
                    mmio_end,
                    expected.len(),
                );
                let counts = backend.exit_counts();

                // Repeated finish/capture at the stopped boundary is a fixpoint:
                // it cannot enter the guest or add an exit count.
                assert_eq!(backend.finish_exit().expect("repeat MMIO finish"), None);
                assert_eq!(backend.exit_counts(), counts);
                assert_eq!(backend.save().expect("repeat MMIO capture"), boundary);
                assert_eq!(backend.exit_counts(), counts);

                if mode == 1 {
                    snapshot = Some(boundary);
                    snapshot_ram = Some(mem.as_mut_slice().to_vec());
                }
            }

            // The only ordinary run is the successor instruction after the full
            // MMIO instruction has been completed and captured.
            assert_eq!(
                backend.run().expect("continue MMIO guest to HLT"),
                Exit::Common(CommonExit::Idle),
                "{}: successor run",
                operation.name()
            );
            endpoints.push(endpoint(&backend, &mut mem, operation));
        }

        assert_eq!(
            endpoints[0],
            endpoints[1],
            "{}: uninterrupted vs save/continue",
            operation.name()
        );
        assert_eq!(
            endpoints[0],
            endpoints[2],
            "{}: uninterrupted vs cold restore",
            operation.name()
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

const RF_CODE_GPA: u64 = 0x1000;
const RF_HANDLER_GPA: u64 = 0x2000;
const RF_STACK_TOP: u64 = 0x8000;
const RF_UART_PORT: u16 = 0x3F8;
const RF_WARM_MARKER: u8 = 0x42;
const RF_DEBUG_MARKER: u8 = 0x99;
const RF_RFLAGS: u64 = 0x1_0002;
const RF_DR7: u64 = 0x401;

/// `INC BX; OUT 0x42; HLT` in flat 16-bit real mode. The execution breakpoint
/// is at the first instruction, so RF determines whether this path reaches the
/// UART marker or vectors to the handler below.
const RF_PROGRAM: [u8; 8] = [0x43, 0xBA, 0xF8, 0x03, 0xB0, RF_WARM_MARKER, 0xEE, 0xF4];

/// The #DB handler emits a distinct marker and halts. It has no return path,
/// keeping the diagnostic bounded even when the restored snapshot loses RF.
const RF_HANDLER: [u8; 7] = [0xBA, 0xF8, 0x03, 0xB0, RF_DEBUG_MARKER, 0xEE, 0xF4];

fn setup_resume_flag_guest(backend: &mut KvmBackend, mem: &mut GuestMem, rflags: u64) {
    setup_real_mode(backend, mem, &RF_PROGRAM);
    // Vector 1 points to the flat real-mode handler at 0x2000.
    backend
        .write_guest(Gpa(4), &[0x00, 0x20, 0x00, 0x00])
        .expect("load #DB IVT entry");
    backend
        .write_guest(Gpa(RF_HANDLER_GPA), &RF_HANDLER)
        .expect("load #DB handler");

    // Start from KVM's valid template and change only the guest-visible state
    // needed by this witness. In particular, the stack receives the #DB frame
    // and the debug register points at the first instruction.
    let mut state = backend.save().expect("save RF setup state");
    state.regs.rip = RF_CODE_GPA;
    state.regs.rbx = 0;
    state.regs.rsp = RF_STACK_TOP;
    state.regs.rflags = rflags;
    state.sregs.ss.base = 0;
    state.sregs.ss.limit = 0xFFFF;
    state.sregs.ss.selector = 0;
    state.sregs.ss.type_ = 3;
    state.sregs.ss.present = 1;
    state.sregs.ss.dpl = 0;
    state.sregs.ss.db = 0;
    state.sregs.ss.s = 1;
    state.sregs.ss.g = 0;
    state.sregs.ss.l = 0;
    state.sregs.ss.unusable = 0;
    state.sregs.idt.base = 0;
    state.sregs.idt.limit = 0x03FF;
    state.debugregs.db[0] = RF_CODE_GPA;
    state.debugregs.dr7 = RF_DR7;
    backend
        .restore(&state)
        .expect("restore RF breakpoint setup state");
}

struct ResumeFlagEndpoint {
    markers: Vec<u8>,
    state: VcpuState,
    ram: Vec<u8>,
}

/// Run only the short main/handler program, accepting one UART write and one
/// halt. The fixed exit budget bounds repeated surfaced exits; the workflow's
/// outer timeout bounds a guest entry that does not return.
fn run_resume_flag_guest(backend: &mut KvmBackend, mem: &mut GuestMem) -> ResumeFlagEndpoint {
    let mut markers = Vec::new();
    let mut halted = false;
    for _ in 0..4 {
        match backend.run().expect("run RF guest") {
            Exit::Arch(X86Exit::Io {
                port: RF_UART_PORT,
                size: 1,
                write: Some(value),
            }) => markers.push(value as u8),
            Exit::Common(CommonExit::Idle) => {
                halted = true;
                break;
            }
            other => panic!("RF guest produced unexpected exit: {other:?}"),
        }
    }
    assert!(halted, "RF guest did not reach HLT within the bounded run");
    ResumeFlagEndpoint {
        markers,
        state: backend.save().expect("save RF endpoint"),
        ram: mem.as_mut_slice().to_vec(),
    }
}

fn log_resume_flag_endpoint(label: &str, endpoint: &ResumeFlagEndpoint) {
    println!(
        "resume-flag {label}: markers={:?} rflags={:#x} rip={:#x} rbx={} dr0={:#x} dr7={:#x}",
        endpoint.markers,
        endpoint.state.regs.rflags,
        endpoint.state.regs.rip,
        endpoint.state.regs.rbx,
        endpoint.state.debugregs.db[0],
        endpoint.state.debugregs.dr7,
    );
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn resume_flag_preserves_instruction_breakpoint_continuation() {
    // Original: after the state restore, run immediately. There is deliberately
    // no save/read between restore and the first KVM entry.
    let original = {
        let mut mem = GuestMem::new(0x10000);
        let mut backend = new_backend_or_explain();
        setup_resume_flag_guest(&mut backend, &mut mem, RF_RFLAGS);
        let endpoint = run_resume_flag_guest(&mut backend, &mut mem);
        log_resume_flag_endpoint("original", &endpoint);
        endpoint
    };

    // Save-and-continue: take two entry captures before running, then continue
    // the same live vCPU. These captures must be observationally inert, while
    // the RF assertion itself is delayed until every endpoint is collected.
    let (saved_state, saved_ram, saved_endpoint) = {
        let mut mem = GuestMem::new(0x10000);
        let mut backend = new_backend_or_explain();
        setup_resume_flag_guest(&mut backend, &mut mem, RF_RFLAGS);
        let before_ram = mem.as_mut_slice().to_vec();
        let before_counts = backend.exit_counts();
        let saved_state = backend.save().expect("save RF entry state");
        let repeated_state = backend.save().expect("repeat RF entry state");
        println!(
            "resume-flag saved-entry: rflags={:#x} rip={:#x} rbx={} dr0={:#x} dr7={:#x}",
            saved_state.regs.rflags,
            saved_state.regs.rip,
            saved_state.regs.rbx,
            saved_state.debugregs.db[0],
            saved_state.debugregs.dr7,
        );

        assert_eq!(saved_state.debugregs.db[0], RF_CODE_GPA, "saved DR0");
        assert_eq!(
            saved_state.debugregs.dr7 & 0xf0003,
            1,
            "DR0 must be enabled as a one-byte execution breakpoint"
        );
        assert_eq!(saved_state, repeated_state, "repeated RF saves differ");
        assert_eq!(
            backend.exit_counts(),
            before_counts,
            "save changed exit counts"
        );
        assert_eq!(mem.as_mut_slice(), before_ram, "save changed guest RAM");
        let saved_ram = before_ram;
        let saved_endpoint = run_resume_flag_guest(&mut backend, &mut mem);
        log_resume_flag_endpoint("save-and-continue", &saved_endpoint);
        (saved_state, saved_ram, saved_endpoint)
    };

    // An independent RF-cleared control must actually trigger the breakpoint.
    // This prevents an inert debug-register setup from making every arm pass.
    let rf_clear_control = {
        let mut mem = GuestMem::new(0x10000);
        let mut backend = new_backend_or_explain();
        setup_resume_flag_guest(&mut backend, &mut mem, RF_RFLAGS & !(1 << 16));
        let endpoint = run_resume_flag_guest(&mut backend, &mut mem);
        log_resume_flag_endpoint("rf-cleared-control", &endpoint);
        endpoint
    };

    // Cold restore: the source backend and its mapped memory have been dropped
    // by the preceding scope before this fresh backend is created.
    let cold = {
        let mut mem = GuestMem::new(0x10000);
        let mut backend = new_backend_or_explain();
        setup_resume_flag_guest(&mut backend, &mut mem, RF_RFLAGS);
        backend
            .write_guest(Gpa(0), &saved_ram)
            .expect("restore RF guest RAM");
        backend
            .restore(&saved_state)
            .expect("cold restore RF entry state");
        let endpoint = run_resume_flag_guest(&mut backend, &mut mem);
        log_resume_flag_endpoint("cold", &endpoint);
        endpoint
    };

    assert_eq!(
        rf_clear_control.markers,
        vec![RF_DEBUG_MARKER],
        "RF-cleared control must take the armed instruction breakpoint"
    );
    assert_eq!(
        rf_clear_control.state.regs.rbx, 0,
        "the breakpoint must fault before INC BX"
    );
    assert_eq!(
        rf_clear_control.state.regs.rip,
        RF_HANDLER_GPA + RF_HANDLER.len() as u64
    );

    for (label, endpoint) in [
        ("original", &original),
        ("save-and-continue", &saved_endpoint),
        ("cold", &cold),
    ] {
        assert_eq!(endpoint.markers, vec![RF_WARM_MARKER], "{label} marker");
        assert_eq!(endpoint.state.regs.rbx, 1, "{label} INC BX");
    }
    assert_ne!(cold.markers, vec![RF_DEBUG_MARKER], "cold took #DB handler");

    // This is intentionally after endpoint collection: the current canonical
    // save path clears RF, and the failed assertion should retain the causal
    // marker/RIP/RBX observations above for the owning architecture fix.
    assert_ne!(
        saved_state.regs.rflags & (1 << 16),
        0,
        "saved RF must be retained"
    );

    assert!(
        original.state == saved_endpoint.state,
        "original and save-and-continue endpoint CPU state differ"
    );
    assert!(
        original.state == cold.state,
        "original and cold endpoint CPU state differ"
    );
    assert!(
        original.ram == saved_endpoint.ram,
        "original and save-and-continue endpoint RAM differ"
    );
    assert!(
        original.ram == cold.ram,
        "original and cold endpoint RAM differ"
    );
}

const SHADOW_DIRTY_FIRST_GPA: u64 = 0x2000;
const SHADOW_DIRTY_SECOND_GPA: u64 = 0x3000;
const SHADOW_DIRTY_FIRST_VALUE: u8 = 0xA1;
const SHADOW_DIRTY_SECOND_VALUE: u8 = 0xB2;
const SHADOW_DIRTY_FIRST_MARKER: u8 = 0xA5;
const SHADOW_DIRTY_SECOND_MARKER: u8 = 0x5A;

/// Write two distinct RAM pages across two ordinary guest entries, with a
/// serviced UART exit between each write and before HLT. The dirty log is
/// drained only before the first entry and after the second UART stop: the
/// second drain therefore tests that entry-shadow invalidation did not erase
/// the first entry's history.
const SHADOW_DIRTY_PROGRAM: [u8; 23] = [
    // mov byte [0x2000], 0xa1
    0xC6,
    0x06,
    0x00,
    0x20,
    SHADOW_DIRTY_FIRST_VALUE,
    // mov dx, 0x3f8; mov al, 0xa5; out dx, al
    0xBA,
    0xF8,
    0x03,
    0xB0,
    SHADOW_DIRTY_FIRST_MARKER,
    0xEE,
    // mov byte [0x3000], 0xb2
    0xC6,
    0x06,
    0x00,
    0x30,
    SHADOW_DIRTY_SECOND_VALUE,
    // mov dx, 0x3f8; mov al, 0x5a; out dx, al
    0xBA,
    0xF8,
    0x03,
    0xB0,
    SHADOW_DIRTY_SECOND_MARKER,
    0xEE,
    0xF4, // HLT
];

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn shadow_invalidation_preserves_dirty_log_history() {
    let mut mem = GuestMem::new(0x10000);
    let mut backend = new_backend_or_explain();
    setup_real_mode(&mut backend, &mut mem, &SHADOW_DIRTY_PROGRAM);

    // Registration and setup may leave implementation-defined dirty bits. Two
    // drains establish an empty baseline before any guest instruction runs;
    // host writes through `write_guest` are not supposed to enter this log.
    let _ = backend
        .drain_dirty_pages()
        .expect("clear initial dirty log");
    assert!(
        backend
            .drain_dirty_pages()
            .expect("verify initial dirty log cleared")
            .is_empty(),
        "dirty log was not empty after its initial drain"
    );

    let mut markers = Vec::new();
    let mut uart_stop = |backend: &mut KvmBackend, marker: u8, rip: u64| {
        match backend.run().expect("run to dirty-log UART stop") {
            Exit::Arch(X86Exit::Io {
                port: 0x3F8,
                size: 1,
                write: Some(value),
            }) => assert_eq!(value, u32::from(marker), "unexpected UART marker"),
            other => panic!("expected UART stop, got {other:?}"),
        }
        markers.push(marker);

        // A write-style PIO callback must be fully retired before this stopped
        // boundary is inspected. `finish_exit` performs no ordinary guest run;
        // a non-empty result would be an unexpected same-instruction access.
        assert!(
            backend
                .finish_exit()
                .expect("retire dirty-log UART callback")
                .is_none(),
            "UART retirement produced an unexpected continuation"
        );
        let state = backend.save().expect("save dirty-log UART boundary");
        assert_eq!(state.regs.rip, rip, "UART boundary RIP");
        state
    };

    // The first ordinary entry writes only the first page, then stops at the
    // first UART callback. The second page must still contain its zero value.
    let first_stop = uart_stop(&mut backend, SHADOW_DIRTY_FIRST_MARKER, 0x100B);
    let mut first_value = [0u8; 1];
    let mut second_value = [0u8; 1];
    backend
        .read_guest(Gpa(SHADOW_DIRTY_FIRST_GPA), &mut first_value)
        .expect("read first guest write");
    backend
        .read_guest(Gpa(SHADOW_DIRTY_SECOND_GPA), &mut second_value)
        .expect("read second page before its write");
    assert_eq!(first_value[0], SHADOW_DIRTY_FIRST_VALUE);
    assert_eq!(second_value[0], 0, "second page changed before its entry");
    assert_eq!(backend.exit_counts().io, 1, "first UART stop count");
    assert_eq!(backend.exit_counts().idle, 0, "first stop ran past UART");
    assert_eq!(first_stop.regs.rip, 0x100B);

    // This is a separate ordinary entry. It writes the second page and stops
    // again at UART, before the HLT instruction is entered.
    let second_stop = uart_stop(&mut backend, SHADOW_DIRTY_SECOND_MARKER, 0x1016);
    backend
        .read_guest(Gpa(SHADOW_DIRTY_FIRST_GPA), &mut first_value)
        .expect("read first guest write after second entry");
    backend
        .read_guest(Gpa(SHADOW_DIRTY_SECOND_GPA), &mut second_value)
        .expect("read second guest write");
    assert_eq!(first_value[0], SHADOW_DIRTY_FIRST_VALUE);
    assert_eq!(second_value[0], SHADOW_DIRTY_SECOND_VALUE);
    assert_eq!(backend.exit_counts().io, 2, "two serviced UART stops");
    assert_eq!(backend.exit_counts().idle, 0, "second stop ran past UART");
    assert_eq!(second_stop.regs.rip, 0x1016);

    // Do not drain between the writes. Both pages must be represented after
    // the second ordinary-entry invalidation, including the page from the
    // first entry. Over-reporting is allowed by the Backend contract, so this
    // checks the required members without depending on unrelated KVM bits.
    let dirty = backend
        .drain_dirty_pages()
        .expect("drain dirty pages after both guest writes");
    assert!(
        dirty.contains(&(SHADOW_DIRTY_FIRST_GPA / 4096)),
        "first guest write missing from dirty log: {dirty:?}"
    );
    assert!(
        dirty.contains(&(SHADOW_DIRTY_SECOND_GPA / 4096)),
        "second guest write missing from dirty log: {dirty:?}"
    );
    assert!(
        backend
            .drain_dirty_pages()
            .expect("drain dirty log after reset")
            .is_empty(),
        "dirty log retained pages after its post-write drain"
    );

    // One final ordinary entry reaches HLT without another guest write.
    // Invalidation may conservatively report pages dirty (Backend explicitly
    // permits supersets). Verify actual RAM remains unchanged, and retain the
    // reported set rather than mistaking this cost hint for a state mutation.
    let mut ram_before_hlt = vec![0u8; 0x10000];
    backend
        .read_guest(Gpa(0), &mut ram_before_hlt)
        .expect("capture RAM before write-free HLT");
    assert_eq!(
        backend.run().expect("run from second UART stop to HLT"),
        Exit::Common(CommonExit::Idle)
    );
    assert_eq!(backend.exit_counts().io, 2, "HLT added a UART exit");
    assert_eq!(backend.exit_counts().idle, 1, "HLT exit missing");
    let mut ram_after_hlt = vec![0u8; ram_before_hlt.len()];
    backend
        .read_guest(Gpa(0), &mut ram_after_hlt)
        .expect("capture RAM after write-free HLT");
    assert_eq!(ram_before_hlt, ram_after_hlt, "write-free HLT changed RAM");
    let after_hlt_dirty = backend
        .drain_dirty_pages()
        .expect("drain dirty log after write-free HLT entry");
    println!("SHADOW_WRITE_FREE_HLT_DIRTY_GFNS={after_hlt_dirty:?}");
    assert_eq!(
        markers,
        vec![SHADOW_DIRTY_FIRST_MARKER, SHADOW_DIRTY_SECOND_MARKER]
    );
}
