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
    Backend, CommonExit, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange, X86Exit, X86Policy,
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

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn completion_retirement_preserves_pio_future_without_guest_execution() {
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
            // SAFETY: mem is page-aligned, remains allocated until after backend
            // is dropped, and no host slice aliases it while the guest runs.
            unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
            configure(&mut backend);
            backend.write_guest(Gpa(0x1000), &code).expect("load stub");
            enter_real_mode_at(&mut backend, 0x1000);
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
                    snapshot = Some(sealed);
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
