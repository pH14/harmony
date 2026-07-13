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

use vmm_backend::{Backend, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange, Moment};

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
    match backend.run_until(Moment(deadline)).expect("run_until") {
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
        match backend.run_until(Moment(d)).expect("run_until") {
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
    match backend.run_until(Moment(1_000_000)).expect("run_until") {
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

#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn save_restore_roundtrip_re_zeroes_the_run_until_counter() {
    // P1 round-10: vmm-core's V-time-only restore re-arms the backend's run_until PMU
    // baseline (B) by round-tripping the vCPU through `save()` + `restore()` (the FROZEN
    // trait — no new method): `restore` re-arms the first-entry reset as a side effect,
    // leaving the vCPU byte-identical. The NEXT `run_until` then re-baselines B, so a
    // fresh small deadline lands EXACTLY, not against a stale B. (This is the backend-side
    // mechanism `restore_vtime` uses for B; the counter-A re-arm is portable-tested in
    // vmm-core's `restore_vtime_rearms_counter_a_first_entry_baseline`.)
    let (mut b, _m) = boot_with(SPIN_CODE);
    match b.run_until(Moment(30_000)).expect("advance B to 30000") {
        Exit::Deadline { reached } => assert_eq!(reached.0, 30_000),
        other => panic!("expected Deadline at 30000, got {other:?}"),
    }
    // The save+restore round-trip restore_vtime performs (re-arms B; vCPU unchanged).
    let snap = b.save().expect("save");
    b.restore(&snap)
        .expect("restore re-arms the run_until baseline");
    // A fresh small deadline now lands at EXACTLY 5_000 (B re-baselined). WITHOUT the
    // re-arm, B would still read ~30_000 and `run_until(5_000)` would fail closed
    // (deadline < current — round-8) instead of preempting at 5_000.
    match b
        .run_until(Moment(5_000))
        .expect("run_until after the save+restore re-arm lands exactly")
    {
        Exit::Deadline { reached } => assert_eq!(
            reached.0, 5_000,
            "after the save+restore re-arm a fresh deadline lands EXACTLY (B re-baselined) — got {}",
            reached.0
        ),
        other => panic!("expected Deadline at 5000, got {other:?}"),
    }
    eprintln!("[p1-r10] save+restore re-armed B; fresh run_until(5000) landed exactly");
}

#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn restore_re_arms_pmu_reset_excluding_foreign_branches() {
    // P1(b): after a restore, the backend PMU counter's reset must fire at the NEXT
    // entry (not at restore time), so a coexisting VM running on the same pinned
    // thread in between does not contaminate the restored VM's run_until counter.
    //
    // B1: a busy-spin VM, run a bit, snapshot, restore.
    let (mut b1, _m1) = boot_with(SPIN_CODE);
    match b1.run_until(Moment(20_000)).expect("b1 run_until") {
        Exit::Deadline { reached } => assert_eq!(reached.0, 20_000),
        other => panic!("expected Deadline at 20000, got {other:?}"),
    }
    let snap = b1.save().expect("save b1");
    b1.restore(&snap).expect("restore b1"); // P1(b): re-arms the first-entry reset

    // A DIFFERENT VM runs on the SAME (test) thread, retiring ~100k guest branches —
    // which land in B1's shared, exclude_host PMU counter too.
    {
        let (mut b2, _m2) = boot_with(SPIN_CODE);
        match b2.run_until(Moment(100_000)).expect("b2 run_until") {
            Exit::Deadline { reached } => assert_eq!(reached.0, 100_000),
            other => panic!("expected Deadline at 100000, got {other:?}"),
        }
    }

    // Re-enter the restored B1: its PMU reset fires at THIS entry, excluding B2's
    // foreign branches, so run_until(50_000) lands at EXACTLY 50_000 — not 50_000 +
    // B2's ~100_000. Without the P1(b) re-arm, B1's counter would already read
    // ~100_000 here and run_until(50_000) would report a past-deadline (≈100_000).
    match b1
        .run_until(Moment(50_000))
        .expect("b1 run_until after foreign VM")
    {
        Exit::Deadline { reached } => assert_eq!(
            reached.0, 50_000,
            "restored VM's run_until must count only ITS branches (foreign-VM \
             contamination excluded by the first-entry reset re-arm) — got {}",
            reached.0
        ),
        other => panic!("expected Deadline at 50000, got {other:?}"),
    }
    eprintln!(
        "[p1b] restored VM run_until landed at exactly 50000 after a foreign VM ran on the \
         same thread — no foreign-branch contamination"
    );
}

/// `in al, 0xF8` (a READ-style port exit → completed via `complete_read`) followed by
/// the busy-spin. After the IN traps, completing it stages AL into the run page; the
/// NEXT entry commits it (RIP past the IN), then the spin retires counted branches.
const IN_THEN_SPIN_CODE: &[u8] = &[0xE4, 0xF8, 0x31, 0xC0, 0x85, 0xC0, 0x74, 0xFC];

/// P1 round-8 — the complete `run_until` contract, case `deadline == current`: deliver
/// `Exit::Deadline` with ZERO guest steps toward the deadline (never overstep). On a
/// fresh VM `run_until(Moment(0))` lands at EXACTLY 0 (round-7's single-step would have
/// landed at 0 or 1 — an overstep); a subsequent `run_until` off that sane baseline
/// lands exactly. Bit-identical across two VMs.
#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn run_until_at_current_deadline_takes_zero_steps() {
    let land = || -> (u64, u64) {
        let (mut b, _m) = boot_with(SPIN_CODE);
        let z = match b
            .run_until(Moment(0))
            .expect("run_until(0) at current deadline")
        {
            Exit::Deadline { reached } => reached.0,
            other => panic!("expected Deadline from run_until(0), got {other:?}"),
        };
        let d = match b
            .run_until(Moment(50_000))
            .expect("run_until after the zero-step")
        {
            Exit::Deadline { reached } => reached.0,
            other => panic!("expected Deadline at 50000, got {other:?}"),
        };
        (z, d)
    };
    let (z1, d1) = land();
    let (z2, d2) = land();
    assert_eq!(
        z1, 0,
        "deadline==current took ZERO guest steps (no overstep) — got {z1}"
    );
    assert_eq!(z1, z2, "the zero-step landing is deterministic");
    assert_eq!(d1, d2, "the subsequent deadline landing is deterministic");
    assert_eq!(
        d1, 50_000,
        "the run_until after the zero-step lands at EXACTLY 50000"
    );
    eprintln!("[p1-r8] run_until(Moment(0)) took zero steps (reached {z1}); next landed at {d1}");
}

/// P1 round-11 — the first-entry-reset INVARIANT: a zero-step `run_until` (the
/// `AtOrPastDeadline` branch, no `KVM_RUN`) must NOT consume the pending first-entry
/// reset; it stays armed until a REAL entry. Otherwise a coexisting VM on the shared
/// pinned thread between the zero-step and this VM's first real entry contaminates this
/// VM's baseline (the same contamination `restore_re_arms_pmu_reset_excluding_foreign_branches`
/// guards for restore — here for the zero-step path). This is the round-10 regression the
/// invariant closes.
#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn zero_step_run_until_keeps_first_entry_reset_pending() {
    // B1 fresh: a zero-step run_until(0) — AtOrPastDeadline, no KVM_RUN. Per the
    // invariant this must leave the first-entry reset PENDING (not consume it).
    let (mut b1, _m1) = boot_with(SPIN_CODE);
    match b1.run_until(Moment(0)).expect("b1 zero-step run_until(0)") {
        Exit::Deadline { reached } => assert_eq!(reached.0, 0, "zero-step lands at 0"),
        other => panic!("expected Deadline at 0, got {other:?}"),
    }

    // A DIFFERENT VM runs on the SAME thread, retiring ~100k guest branches into the
    // shared exclude_host PMU counter.
    {
        let (mut b2, _m2) = boot_with(SPIN_CODE);
        match b2.run_until(Moment(100_000)).expect("b2 run_until") {
            Exit::Deadline { reached } => assert_eq!(reached.0, 100_000),
            other => panic!("expected Deadline at 100000, got {other:?}"),
        }
    }

    // B1's FIRST REAL entry now fires the still-pending reset, excluding B2's foreign
    // branches, so run_until(50_000) lands at EXACTLY 50_000. With the round-10 bug (the
    // zero-step consumed the reset), B1's counter would already read ~100_000 here and
    // run_until(50_000) would fail closed as a past deadline.
    match b1
        .run_until(Moment(50_000))
        .expect("b1 first real entry after the zero-step + foreign VM")
    {
        Exit::Deadline { reached } => assert_eq!(
            reached.0, 50_000,
            "the zero-step left the first-entry reset pending, so B1's first real entry \
             excludes the foreign VM's branches — got {}",
            reached.0
        ),
        other => panic!("expected Deadline at 50000, got {other:?}"),
    }
    eprintln!(
        "[p1-r11] zero-step run_until(0) kept the first-entry reset pending; B1's first real \
         entry landed at exactly 50000 despite a foreign VM (no contamination)"
    );
}

/// P1 round-12 — case `deadline < current`: an OVERDUE timer. Round-8 wrongly failed this
/// closed (aborting the VM); a past deadline is a legitimate LATE timer (the deadline,
/// derived from a stale `last_intercept_work`, is already behind the live count — Postgres
/// /Linux re-arm LAPIC one-shots constantly), so it must fire IMMEDIATELY: an
/// `Exit::Deadline` delivered now, at the current count, NOT an error/abort.
#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn run_until_past_deadline_fires_immediately() {
    let (mut b, _m) = boot_with(SPIN_CODE);
    match b.run_until(Moment(10_000)).expect("advance to 10000") {
        Exit::Deadline { reached } => assert_eq!(reached.0, 10_000),
        other => panic!("expected Deadline at 10000, got {other:?}"),
    }
    // The current work is now 10_000; a deadline of 5_000 is OVERDUE (in the past). It
    // must fire the timer NOW — an immediate Exit::Deadline at the current count — never
    // an error (which would abort a Postgres/Linux guest re-arming one-shots).
    match b
        .run_until(Moment(5_000))
        .expect("an overdue deadline must fire immediately, NOT error/abort")
    {
        Exit::Deadline { reached } => {
            assert_eq!(
                reached.0, 10_000,
                "the overdue timer fires NOW at the current count (10000), not at the past \
                 deadline (5000) — got {}",
                reached.0
            );
            eprintln!("[p1-r12] overdue run_until(5000) fired immediately at 10000 (no abort)");
        }
        other => panic!("expected an immediate Deadline, got {other:?}"),
    }
}

/// P1 round-8 — case `deadline == current` WITH an owed completion: the prior step's
/// read-style exit was completed (staged in the run page). `run_until(current)` takes
/// ZERO guest steps (no overstep) AND does not lose the staged completion: the NEXT
/// (future-deadline) `run_until` commits it (the guest progresses past the IN, no
/// re-trap) and lands exactly. Proves round-7's preservation + round-8's no-overstep.
#[test]
#[ignore = "live KVM + perf; run on the box with --ignored"]
fn run_until_at_current_deadline_preserves_owed_completion() {
    let (mut b, _m) = boot_with(IN_THEN_SPIN_CODE);
    // First entry: the IN (read-style) traps before any counted branch (work 0).
    match b.run().expect("run to the IN") {
        Exit::Io {
            port, write: None, ..
        } => assert_eq!(port, EXIT_EARLY_PORT, "the IN reads port 0xF8"),
        other => panic!("expected a read-style Io exit from the IN, got {other:?}"),
    }
    b.complete_read(0x42)
        .expect("complete the IN (stages AL=0x42)"); // owed completion
    // deadline == current (0): ZERO steps, no overstep, owed completion preserved.
    match b
        .run_until(Moment(0))
        .expect("run_until(0) with an owed completion")
    {
        Exit::Deadline { reached } => assert_eq!(
            reached.0, 0,
            "deadline==current with an owed completion still takes zero steps — got {}",
            reached.0
        ),
        other => panic!("expected Deadline at 0, got {other:?}"),
    }
    // The NEXT entry commits the owed IN completion (RIP past it) + spins to 5_000. If the
    // completion had been lost/overstepped, the IN would re-trap (an Io exit) instead.
    match b
        .run_until(Moment(5_000))
        .expect("run_until after committing the owed read")
    {
        Exit::Deadline { reached } => assert_eq!(
            reached.0, 5_000,
            "the owed read was committed (guest progressed past the IN) and landed exactly"
        ),
        other => panic!("expected Deadline at 5000 (owed read committed), got {other:?}"),
    }
    eprintln!(
        "[p1-r8] deadline==current preserved the owed completion; next run_until committed it"
    );
}
