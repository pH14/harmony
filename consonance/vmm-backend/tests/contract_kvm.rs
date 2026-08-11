// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **hardware** leg of the `Backend` contract tests (`docs/TESTING.md`,
//! rung 2): the *identical* [`vmm_backend::contract`] exam the portable
//! `contract_mock.rs` runs over `MockBackend`, run here over the live
//! `KvmBackend` and `PatchedKvmBackend`.
//!
//! `#[cfg(all(target_os = "linux", target_arch = "x86_64"))]` + `#[ignore]`, so
//! CI **compiles** it on every push and never runs it; the hardware lane
//! (`.github/workflows/box.yml` → `scripts/box-gates.sh`) executes it. On a host
//! without `/dev/kvm` these panic with what is missing and where to run them —
//! never a silent pass.
//!
//! ```sh
//! taskset -c 1 cargo test -p vmm-backend --all-features --test contract_kvm \
//!     -- --ignored --nocapture --test-threads=1
//! ```
//!
//! **Transcribed, pending its first hardware run.** The guest stubs below are
//! hand-assembled real-mode fragments written from the instruction encodings and
//! the pattern in `tests/kvm_smoke.rs`; they have not yet executed on the box.
//! The first hardware run is expected to correct stub details (segment setup,
//! which scenarios a patched backend can actually surface). What is *not*
//! provisional is the exam itself — it is the same code the portable leg
//! already passes.
#![cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    feature = "contract-tests"
))]

use vmm_backend::contract::{BackendFixture, ContractReport, Scenario, run_all};
use vmm_backend::{
    Backend, CpuidModel, Gpa, KvmBackend, MsrFilter, MsrRange, PatchedKvmBackend, X86Policy,
};

/// Guest RAM size. 64 KiB is a whole real-mode segment and covers every guest
/// frame the stubs touch.
const RAM_LEN: usize = 0x1_0000;
/// Where every scenario stub is loaded and where `rip` starts.
const ENTRY: u64 = 0x1000;
/// Where the dirty-page stub is loaded (distinct from `ENTRY`, so arming the
/// dirty log never clobbers a scenario stub).
const DIRTY_ENTRY: u64 = 0x3000;
/// The guest frames the dirty-page stub writes.
const DIRTY_GFNS: [u64; 2] = [2, 5];

/// One identity-mapped guest RAM region, page-aligned (the `map_memory` host
/// alignment invariant), reached by the backend through a raw pointer. Held by
/// the fixture so it outlives every backend the exam hands out. Moving the
/// struct is harmless — the *backing* is a separate `alloc_zeroed` allocation
/// that never moves, which is exactly the pinning `map_memory` requires.
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

/// The hand-assembled real-mode stub for each scenario, or `None` when this
/// backend cannot put the guest in that situation at all.
///
/// `patched` selects the determinism backend's extra reach: stock KVM services
/// `CPUID` and `VMCALL` in-kernel and never traps `RDTSC`/`RDRAND`, so those
/// scenarios are unavailable there by design, not by omission.
///
/// Every stub ends in `hlt; jmp -3`, so each `run` after the armed exit returns
/// `Idle` again — the same repeated-halt shape the mock's scripted `Idle` tail
/// provides.
fn stub(scenario: Scenario, patched: bool) -> Option<Vec<u8>> {
    // hlt ; jmp -3  (back to the hlt)
    const HALT_LOOP: [u8; 3] = [0xF4, 0xEB, 0xFD];
    let head: Vec<u8> = match scenario {
        Scenario::Idle => Vec::new(),
        // mov dx, 0x3f8 ; in al, dx
        Scenario::PortIn => vec![0xBA, 0xF8, 0x03, 0xEC],
        // No MMIO aperture is reachable from real mode with all of guest RAM
        // mapped; the exam's read-style cell uses `PortIn` instead.
        Scenario::MmioLoad => return None,
        // mov ecx, 0x12345678 ; rdmsr   (the index is default-denied by `policy`)
        Scenario::Rdmsr => vec![0x66, 0xB9, 0x78, 0x56, 0x34, 0x12, 0x0F, 0x32],
        // mov ecx, 0x12345678 ; xor eax,eax ; xor edx,edx ; wrmsr
        Scenario::Wrmsr => vec![
            0x66, 0xB9, 0x78, 0x56, 0x34, 0x12, 0x66, 0x31, 0xC0, 0x66, 0x31, 0xD2, 0x0F, 0x30,
        ],
        // Serviced in-kernel from the installed CPUID table on both backends.
        Scenario::Cpuid => return None,
        // Stock KVM services VMCALL in-kernel; the patched backend's hypercall
        // transport needs the doorbell wiring vmm-core composes, which is above
        // this trait.
        Scenario::Hypercall => return None,
        // rdtsc — only the determinism backend traps it.
        Scenario::Rdtsc if patched => vec![0x0F, 0x31],
        Scenario::Rdtsc => return None,
        // rdrand ax — only the determinism backend traps it.
        Scenario::Rdrand if patched => vec![0x0F, 0xC7, 0xF0],
        Scenario::Rdrand => return None,
        // dec cx ; jnz -3 ; jmp -5 — a conditional-branch-retiring loop that
        // never exits on its own, so only a `run_until` deadline stops it.
        Scenario::BusyLoop => return Some(vec![0x49, 0x75, 0xFD, 0xEB, 0xFB]),
    };
    let mut code = head;
    code.extend_from_slice(&HALT_LOOP);
    Some(code)
}

/// mov byte [0x2000], 1 ; mov byte [0x5000], 1 ; hlt — dirties gfns 2 and 5.
const DIRTY_STUB: &[u8] = &[
    0xC6, 0x06, 0x00, 0x20, 0x01, 0xC6, 0x06, 0x00, 0x50, 0x01, 0xF4,
];

/// Minimal frozen CPUID model and a real, default-deny MSR filter: the SYSENTER
/// MSRs stay in-kernel, every other index (including the scenario stubs' probe)
/// traps to userspace.
fn policy() -> X86Policy {
    X86Policy {
        cpuid: CpuidModel::default(),
        msr_filter: MsrFilter {
            allow_inkernel: vec![MsrRange {
                base: 0x174,
                count: 3,
            }],
        },
    }
}

/// Fail-fast guard: a missing host baseline panics with where to run this,
/// rather than reporting a green that means nothing.
fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm missing — the contract exam's hardware leg needs bare-metal x86-64 with \
         VMX. Run it on the determinism box: taskset -c 1 cargo test -p vmm-backend \
         --all-features --test contract_kvm -- --ignored --test-threads=1"
    );
}

/// A backend this fixture can build. The two live backends differ only in their
/// constructor and their reach, so the fixture is generic over the pair.
trait LiveBackend: Backend<A = vmm_backend::X86> + Sized {
    fn open() -> Self;
    fn enable_dirty_log(&mut self);
    fn load(&mut self, gpa: Gpa, bytes: &[u8]);
    fn map(&mut self, gpa: Gpa, host: &mut [u8]);
}

impl LiveBackend for KvmBackend {
    fn open() -> Self {
        KvmBackend::new().unwrap_or_else(|e| {
            panic!("KvmBackend::new failed ({e}); needs /dev/kvm + VMX on the determinism box")
        })
    }
    fn enable_dirty_log(&mut self) {
        self.set_dirty_log_enabled(true);
    }
    fn load(&mut self, gpa: Gpa, bytes: &[u8]) {
        self.write_guest(gpa, bytes).expect("write_guest");
    }
    fn map(&mut self, gpa: Gpa, host: &mut [u8]) {
        // SAFETY: `host` is the fixture's boxed, page-aligned `GuestMem`, which
        // outlives every backend the exam holds and is not aliased while the
        // guest runs.
        unsafe { self.map_memory(gpa, host) }.expect("map_memory");
    }
}

impl LiveBackend for PatchedKvmBackend {
    fn open() -> Self {
        PatchedKvmBackend::new().unwrap_or_else(|e| {
            panic!(
                "PatchedKvmBackend::new failed ({e}); the patched KVM modules \
                 (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS) must be loaded"
            )
        })
    }
    fn enable_dirty_log(&mut self) {
        self.set_dirty_log_enabled(true);
    }
    fn load(&mut self, gpa: Gpa, bytes: &[u8]) {
        self.write_guest(gpa, bytes).expect("write_guest");
    }
    fn map(&mut self, gpa: Gpa, host: &mut [u8]) {
        // SAFETY: as above.
        unsafe { self.map_memory(gpa, host) }.expect("map_memory");
    }
}

/// Put the vCPU into flat real mode with `rip` at `entry` (linear == GPA, paging
/// off), through the trait's own save/restore.
fn enter_real_mode_at<B: LiveBackend>(backend: &mut B, entry: u64) {
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.sregs.ds.base = 0;
    st.sregs.ds.selector = 0;
    st.regs.rip = entry;
    st.regs.rflags = 0x2; // reserved bit set, the minimal valid RFLAGS
    backend.restore(&st).expect("restore setup state");
}

/// The live fixture. Owns the guest memory of every backend it has handed out,
/// so a `GuestMem` can never be freed while a backend still maps it.
struct KvmFixture<B: LiveBackend> {
    name: &'static str,
    patched: bool,
    mems: Vec<GuestMem>,
    _marker: std::marker::PhantomData<B>,
}

impl<B: LiveBackend> KvmFixture<B> {
    fn new(name: &'static str, patched: bool) -> Self {
        KvmFixture {
            name,
            patched,
            mems: Vec::new(),
            _marker: std::marker::PhantomData,
        }
    }

    /// A fresh, mapped, stub-loaded, **unconfigured** backend positioned at
    /// `entry`.
    fn boot(&mut self, code: &[u8], entry: u64) -> B {
        let mut backend = B::open();
        // Armed before `map_memory`: the flag is a property of the memslot, so
        // it has to be set before the slot is registered.
        backend.enable_dirty_log();
        self.mems.push(GuestMem::new(RAM_LEN));
        let mem = self.mems.last_mut().expect("just pushed");
        backend.map(Gpa(0), mem.as_mut_slice());
        backend.load(Gpa(entry), code);
        enter_real_mode_at(&mut backend, entry);
        backend
    }
}

impl<B: LiveBackend> BackendFixture for KvmFixture<B> {
    type B = B;

    fn name(&self) -> &'static str {
        self.name
    }

    fn spawn(&mut self, scenario: Scenario) -> Option<B> {
        let code = stub(scenario, self.patched)?;
        Some(self.boot(&code, ENTRY))
    }

    fn policy(&self) -> X86Policy {
        policy()
    }

    fn implements_run_until(&self) -> bool {
        // Bring-up stock `KvmBackend` answers `Unsupported`; the determinism
        // backend implements the overflow-arm + exact-landing path.
        self.patched
    }

    fn dirty_pages(&mut self, backend: &mut B) -> Option<Vec<u64>> {
        backend.load(Gpa(DIRTY_ENTRY), DIRTY_STUB);
        enter_real_mode_at(backend, DIRTY_ENTRY);
        backend.run().expect("run the dirty-page stub to its halt");
        Some(DIRTY_GFNS.to_vec())
    }
}

/// The exams that must run on **any** live backend. `run_until`, the trapped
/// clock/RNG reads, and the CPUID/hypercall cells are backend-dependent and are
/// asserted per backend below.
const REQUIRED_EVERYWHERE: &[&str] = &[
    "ordering/not_configured",
    "ordering/completion_grid",
    "exactness/dirty_log",
    "fixpoint/save_restore_save",
    "interrupts/one_overwritable_slot",
];

#[track_caller]
fn assert_ran(report: &ContractReport, exams: &[&'static str]) {
    for exam in exams {
        assert!(
            report.did_run(exam),
            "{exam} did not run against {}: {report:?}",
            report.backend
        );
    }
}

#[test]
#[ignore = "live KVM; run on the determinism box with --ignored (see file header)"]
fn stock_kvm_backend_passes_the_contract_exam() {
    require_kvm();
    let mut fx: KvmFixture<KvmBackend> = KvmFixture::new("kvm-stock", false);
    let report = run_all(&mut fx);
    println!("[CONTRACT] {report:#?}");

    assert_ran(&report, REQUIRED_EVERYWHERE);
    // Stock KVM declines `run_until` — and the exam checks that it declines
    // *loudly*, with the documented `Unsupported`, rather than misbehaving.
    assert_ran(&report, &["exactness/run_until_declines_loudly"]);
    // Its determinism capabilities are all false, so the capability-keyed
    // exactness exams are declined by design and recorded as such.
    assert!(
        !report.did_run("exactness/deterministic_tsc_traps"),
        "stock KVM cannot trap RDTSC; claiming that exam ran would be a false green"
    );
    assert!(
        !report.declined.is_empty(),
        "stock KVM's declines are part of its contract; an empty decline list means the exam \
         stopped recording them"
    );
}

#[test]
#[ignore = "live patched KVM; run on the determinism box with --ignored (see file header)"]
fn patched_kvm_backend_passes_the_contract_exam() {
    require_kvm();
    let mut fx: KvmFixture<PatchedKvmBackend> = KvmFixture::new("kvm-patched", true);
    let report = run_all(&mut fx);
    println!("[CONTRACT] {report:#?}");

    assert_ran(&report, REQUIRED_EVERYWHERE);
    // The determinism backend implements the deadline path and advertises the
    // trapped clock and RNG, so those exams must actually run.
    assert_ran(
        &report,
        &[
            "exactness/deadline_is_late_only",
            "exactness/guest_exit_preempts_the_deadline",
            "exactness/deadline_monotonicity",
            "exactness/deadline_is_repeatable",
            "exactness/deterministic_tsc_traps",
            "exactness/deterministic_rng_traps",
        ],
    );
}
