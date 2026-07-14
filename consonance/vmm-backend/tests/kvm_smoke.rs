// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live `KvmBackend` integration tests (gates 6–9).
//!
//! `#[cfg(target_os = "linux")]` + `#[ignore]` so the standard gates (which run
//! `cargo test … --all-features`) **compile but do not run** them — a Cargo
//! feature would be flipped on by `--all-features` and trip the fail-fast on a
//! Mac/CI host. Run explicitly on the determinism box, **CPU-pinned** per
//! `docs/BOX-PINNING.md` (core 1 is spare; 2/4 are measurement, 5–7 the CI
//! runner, 0 the OS):
//!
//! ```sh
//! ssh <det-box> 'taskset -c 1 cargo test -p vmm-backend --test kvm_smoke -- --ignored --test-threads=1'
//! ```
//!
//! **Fail-fast, never skip:** on a host without `/dev/kvm`/VMX/Intel these panic
//! with what is missing and where to run them, rather than silently passing.
#![cfg(target_os = "linux")]

use vmm_backend::{Backend, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange};

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
            "/dev/kvm missing — these live tests need bare-metal Intel x86-64 with VMX. \
             Run on the determinism box: ssh <det-box> 'taskset -c 1 cargo test -p vmm-backend \
             --test kvm_smoke -- --ignored --test-threads=1'"
        );
    }
    KvmBackend::new().unwrap_or_else(|e| {
        panic!(
            "KvmBackend::new failed ({e}); these need /dev/kvm + VMX on the determinism box \
             (taskset -c 1). Not runnable on macOS or under nested virt."
        )
    })
}

/// Minimal frozen CPUID model and a permissive-but-real MSR filter for bring-up.
/// `allow_inkernel` names a couple of harmless MSR ranges KVM keeps servicing;
/// every other MSR (including the gate-8 probe) traps to userspace.
fn configure(backend: &mut KvmBackend) {
    backend
        .set_cpuid(&CpuidModel::default())
        .expect("set_cpuid");
    backend
        .set_msr_filter(&MsrFilter {
            // SYSENTER MSRs (0x174..0x177) — present, harmless, in-kernel.
            allow_inkernel: vec![MsrRange {
                base: 0x174,
                count: 3,
            }],
        })
        .expect("set_msr_filter");
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
fn bringup_smoke_out_then_hlt() {
    // mov dx, 0x3f8 ; mov al, 0x42 ; out dx, al ; hlt
    let code: &[u8] = &[0xBA, 0xF8, 0x03, 0xB0, 0x42, 0xEE, 0xF4];

    let mut backend = new_backend_or_explain();
    let mut mem = GuestMem::new(0x10000);
    // SAFETY: `mem` outlives `backend` (dropped after it), is page-aligned, and
    // is not aliased while the guest runs.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    configure(&mut backend);
    backend.write_guest(Gpa(0x1000), code).expect("load stub");
    enter_real_mode_at(&mut backend, 0x1000);

    match backend.run().expect("run to OUT") {
        Exit::Io {
            port: 0x3F8,
            size: 1,
            write: Some(v),
        } => assert_eq!(v, 0x42),
        other => panic!("expected OUT to 0x3f8, got {other:?}"),
    }
    assert_eq!(backend.run().expect("run to HLT"), Exit::Idle);

    let counts = backend.exit_counts();
    assert_eq!(counts.io, 1, "exactly one IO exit");
    assert_eq!(counts.idle, 1, "exactly one HLT exit");
    assert_eq!(counts.total(), 2);
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
    //   out 0x10, al          (e6 10)       ; -> Exit::Io (the silent-value path)
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
        Exit::Rdmsr { index: 0x1234_5678 } => {}
        other => panic!("expected RDMSR exit for the denied index, got {other:?}"),
    }
    // Deny it (#GP). The fault vectors through IVT[13] to the HLT handler, so the
    // next exit is HLT — proving the guest took the fault. A silent in-kernel
    // value instead would have advanced past RDMSR into the `out 0x10` and
    // surfaced Exit::Io, which would fail this assertion loudly.
    backend.complete_fault().expect("complete_fault");
    match backend.run().expect("run after #GP") {
        Exit::Idle => {}
        Exit::Io { port, .. } => {
            panic!("RDMSR was silently allowed (reached out 0x{port:x}) — filter not loud")
        }
        other => panic!("expected HLT from the #GP handler, got {other:?}"),
    }
    assert_eq!(backend.exit_counts().rdmsr, 1);
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored"]
fn capabilities_are_honest() {
    let backend = new_backend_or_explain();
    let caps = backend.capabilities();
    assert_eq!(caps.name, "kvm-stock");
    assert!(!caps.deterministic_tsc, "stock KVM cannot trap RDTSC");
    assert!(
        !caps.deterministic_rng,
        "stock KVM cannot trap RDRAND/RDSEED"
    );
    assert!(!caps.enforces_tsc_deadline_msr, "stock KVM swallows 0x6E0");
}
