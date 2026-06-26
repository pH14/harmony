// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only live gate 1 for task 47 (`#[cfg(target_os = "linux")]` + `#[ignore]`):
//! the **live `vtime::CpuBackend`** (real `perf_event` overflow + KVM single-step)
//! satisfies the same precise-injection contract as `vtime::sim` —
//! `Backend::run_until(deadline)` lands at **exactly** `deadline` retired
//! conditional branches and returns `Exit::Deadline`, while a genuine guest exit
//! before the deadline returns *that* exit, short of the deadline.
//!
//! This is the direct proof of the primitive and needs **only stock KVM** (the PMU
//! overflow + `KVM_GUESTDBG_SINGLESTEP` are stock features; the determinism
//! intercepts are not exercised here), so it runs against the loaded stock module.
//! It needs bare-metal Intel + `perf_event` (no nested virt), CPU-pinned per
//! `docs/BOX-PINNING.md` — **core 2** while PR #12 owns core 4 — and bounded by an
//! on-box `timeout` (a broken overflow-signal kick would hang the free-run):
//!
//! ```sh
//! ssh hetzner 'cd <worktree> && taskset -c 2 timeout 120 \
//!   cargo test -p vmm-backend --test live_preemption -- --ignored --nocapture --test-threads=1'
//! ```
//!
//! Fail-fast, never skip: a host without `/dev/kvm`/`perf_event` panics with what
//! is missing. macOS builds an empty test binary (the contract is property-tested
//! against `SimCpu` in `src/run_until.rs`).
#![cfg(target_os = "linux")]

use vmm_backend::{Backend, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange, Vtime};

/// Page-aligned guest RAM (the `map_memory` host-alignment invariant), reached by
/// the backend through a raw pointer.
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
        // SAFETY: `ptr`/`len` from `alloc_zeroed`; exclusive borrow.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for GuestMem {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`layout` from `alloc_zeroed`; freed once.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) };
    }
}

fn new_backend_or_explain() -> KvmBackend {
    if !std::path::Path::new("/dev/kvm").exists() {
        panic!(
            "/dev/kvm missing — run on the determinism box (bare-metal Intel + perf_event), \
             CPU-pinned core 2: ssh hetzner 'taskset -c 2 timeout 120 cargo test -p vmm-backend \
             --test live_preemption -- --ignored --nocapture --test-threads=1'"
        );
    }
    KvmBackend::new()
        .unwrap_or_else(|e| panic!("KvmBackend::new failed ({e}); needs /dev/kvm + VMX"))
}

fn configure(backend: &mut KvmBackend) {
    backend
        .set_cpuid(&CpuidModel::default())
        .expect("set_cpuid");
    backend
        .set_msr_filter(&MsrFilter {
            allow_inkernel: vec![MsrRange {
                base: 0x174,
                count: 3,
            }],
        })
        .expect("set_msr_filter");
}

/// Flat real mode with `cs.base = 0`, `rip = entry`, paging off (Multiboot-style
/// bring-up entry; linear == GPA).
fn enter_real_mode_at(backend: &mut KvmBackend, entry: u64) {
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.regs.rip = entry;
    st.regs.rflags = 0x2; // reserved bit set, minimal valid RFLAGS
    backend.restore(&st).expect("restore setup state");
}

/// An **infinite busy-spin** that takes no natural VM-exit but retires exactly one
/// **conditional** branch per iteration (the V-time work event — an unconditional
/// `jmp` would NOT count and work would never advance). 16-bit real mode:
///
/// ```text
///   xor ax, ax        ; 31 C0
///  .loop:
///   test ax, ax       ; 85 C0   (ZF=1; ax never changes → always taken)
///   jz .loop          ; 74 FC   (-4 → back to test)  ← the one counted branch
/// ```
const SPIN_CODE: &[u8] = &[0x31, 0xC0, 0x85, 0xC0, 0x74, 0xFC];

/// A short loop of exactly 3 conditional branches, then a natural VM-exit (`OUT`):
///
/// ```text
///   mov cx, 3         ; B9 03 00
///  .loop:
///   dec cx            ; 49
///   jnz .loop         ; 75 FD   (3 conditional branches: taken,taken,not-taken)
///   mov al, 0x42      ; B0 42
///   out 0xF8, al      ; E6 F8   (OUT imm8: 8-bit port 0xF8) ← Exit::Io at work == 3
///   hlt               ; F4
/// ```
const EXIT_EARLY_CODE: &[u8] = &[
    0xB9, 0x03, 0x00, 0x49, 0x75, 0xFD, 0xB0, 0x42, 0xE6, 0xF8, 0xF4,
];
/// The port `EXIT_EARLY_CODE`'s `OUT imm8` (`E6 F8`) writes to (8-bit immediate).
const EXIT_EARLY_PORT: u16 = 0xF8;

const ENTRY: u64 = 0x1000;
const RAM: usize = 0x10000;

/// Fresh backend running `code` from real-mode `ENTRY`.
fn boot_with(code: &[u8]) -> (KvmBackend, GuestMem) {
    let mut backend = new_backend_or_explain();
    let mut mem = GuestMem::new(RAM);
    // SAFETY: `mem` outlives `backend`, is page-aligned, not aliased during run.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    configure(&mut backend);
    backend.write_guest(Gpa(ENTRY), code).expect("load code");
    enter_real_mode_at(&mut backend, ENTRY);
    (backend, mem)
}

/// Drive `run_until(deadline)` on a fresh busy-spin VM; assert it lands at EXACTLY
/// `deadline` and return the reached work for the determinism check.
fn spin_until(deadline: u64) -> u64 {
    let (mut backend, _mem) = boot_with(SPIN_CODE);
    match backend.run_until(Vtime(deadline)).expect("run_until") {
        Exit::Deadline { reached } => {
            assert_eq!(
                reached.0, deadline,
                "run_until must land at EXACTLY the deadline (got {}, want {deadline}) — a \
                 value off by the skid is a determinism bug, not a tolerance to widen",
                reached.0
            );
            reached.0
        }
        other => panic!("expected Exit::Deadline at {deadline}, got {other:?}"),
    }
}

#[test]
#[ignore = "live KVM + perf; run on the box (stock KVM ok) with --ignored (see header)"]
fn run_until_lands_exactly_at_the_deadline() {
    for &deadline in &[10_000u64, 50_000, 250_000] {
        let reached = spin_until(deadline);
        eprintln!(
            "[gate1] busy-spin: armed at {}−128, single-stepped to exact, landed at {reached} \
             (== deadline {deadline})",
            deadline
        );
    }
}

#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn run_until_is_deterministic_twice() {
    // Two independent VMs, same deadline → identical reached work (a pure function
    // of the deadline / instruction stream, not of where the PMU skid fell).
    const D: u64 = 100_000;
    let a = spin_until(D);
    let b = spin_until(D);
    assert_eq!(
        a, b,
        "two same-deadline runs must reach the identical work count"
    );
    eprintln!("[gate1] deterministic-twice: both runs landed at {a} branches");
}

#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn run_until_advances_monotonically_within_a_run() {
    // Successive run_until calls on the SAME VM advance the cumulative counter to
    // each exact deadline (the spin never exits, so each lands on Deadline).
    let (mut backend, _mem) = boot_with(SPIN_CODE);
    for &d in &[20_000u64, 60_000, 130_000] {
        match backend.run_until(Vtime(d)).expect("run_until") {
            Exit::Deadline { reached } => assert_eq!(reached.0, d, "exact landing at {d}"),
            other => panic!("expected Deadline at {d}, got {other:?}"),
        }
    }
    eprintln!("[gate1] monotone: 20k → 60k → 130k all landed exactly");
}

#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn guest_exit_before_deadline_returns_that_exit() {
    // The guest takes a natural VM-exit (OUT) after 3 branches; a far deadline must
    // therefore yield THAT exit, short of the deadline — never Exit::Deadline.
    let (mut backend, _mem) = boot_with(EXIT_EARLY_CODE);
    match backend.run_until(Vtime(1_000_000)).expect("run_until") {
        Exit::Io {
            port: EXIT_EARLY_PORT,
            size: 1,
            write: Some(v),
        } => {
            assert_eq!(v, 0x42, "the OUT value the guest wrote");
            eprintln!("[gate1] guest exit (OUT 0x42) returned short of the 1e6 deadline ✓");
        }
        Exit::Deadline { reached } => panic!(
            "the guest OUT-exits at work 3, but run_until ran past it to a Deadline at {} — \
             a natural exit before the deadline must be returned, never skipped",
            reached.0
        ),
        other => panic!("expected Exit::Io (the guest's OUT), got {other:?}"),
    }
}
