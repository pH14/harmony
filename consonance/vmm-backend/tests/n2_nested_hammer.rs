// SPDX-License-Identifier: AGPL-3.0-or-later
//! SPIKE(nested-x86): N-2 deadline hammer — **spike-branch-only apparatus**, not
//! production surface (see `docs/NESTED-X86.md` §N-2 and the re-certification
//! program, bead hm-b5b).
//!
//! Drives the **production patched** [`PatchedKvmBackend`] `run_until` path
//! (patch-0004 `KVM_ARM_PREEMPT_EXIT` overflow arming + patch-0005
//! `KVM_ARM_MTF_STEP` exact landing — the constructor fails loudly if the
//! patched modules are not loaded, so the hammer can never silently fall back
//! to the stock SIGIO path; that fallback is exactly what invalidated the
//! original N-2 evidence, PR #98 review finding 1) over a long sequence of
//! seeded-random work targets on ONE busy-spin VM, and requires, per deadline:
//!
//! 1. **exact landing** — `CommonExit::Deadline { reached } == target`, never
//!    overshoot, never a different exit;
//! 2. **independent guest work oracle** — the guest loop increments a counter
//!    in guest RAM once per retired conditional branch, so after a landing the
//!    memory word must equal `target mod 2^32`. This breaks the circularity of
//!    checking the PMU against itself: a systematically wrong work clock would
//!    land "exactly" on its own axis but disagree with the memory-visible
//!    progress (review finding: no independent oracle);
//! 3. **per-record overflow-multiplicity accounting** — the perf ring records
//!    are counted (not inferred): an armed deadline must show its overflow PMI
//!    as `PERF_RECORD_SAMPLE` records within the arithmetic bound below, and
//!    `LOST`/`THROTTLE` records must never appear (review finding: overflow
//!    delivery was inferred from landings only).
//!
//! Every attempted deadline is accounted in the machine-readable JSON
//! progress/summary lines this test emits on stdout (`N2JSON ...`).
//!
//! ## The record-count bound
//!
//! The planner arms the overflow only when `delta > SKID_MARGIN` (256), at
//! `target − SKID_MARGIN`, with period `p = delta − SKID_MARGIN`. The kernel
//! auto-re-arms a sampling event at the same period, so while the (≤
//! `SKID_MARGIN`-branch) skid window elapses, up to `SKID_MARGIN / p`
//! *additional* legitimate overflows can fire for small `p` (the skid-bracket
//! delta class). The bound is therefore `1 ..= 1 + SKID_MARGIN / p` samples for
//! an armed deadline and exactly `0` for an unarmed one; anything outside it is
//! a recorded violation. `LOST > 0` (ring overrun — a PMI whose record is gone)
//! and `THROTTLE > 0` (kernel suppressed PMIs — nested N-0 saw the kernel lower
//! `perf_event_max_sample_rate`) each break "observed exactly once" and are
//! violations regardless of the landing.
//!
//! Env parameters (all optional):
//!   N2_DEADLINES  total armed deadlines this invocation (default 10_000)
//!   N2_SEED       xorshift seed for the delta stream (default 0x5EED_2026)
//!   N2_PROGRESS   progress line every this many deadlines (default 10_000)
//!
//! Delta classes are interleaved deterministically from the seed: small
//! (1..=64, the MTF stepping edge), skid-adjacent (128..=512, brackets
//! `SKID_MARGIN = 256`), and large (4k..=100k, the pure overflow path).
//!
//! Run inside the L1 appliance (or on bare metal for the N-3 control):
//!   N2_DEADLINES=250000 ./n2_nested_hammer --ignored --nocapture --test-threads=1
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use vmm_backend::{
    Backend, CommonExit, CpuidModel, Exit, Gpa, Moment, MsrFilter, MsrRange, PatchedKvmBackend,
    PmuOverflowStats, X86Policy,
};

/// Mirror of `vmm_backend::run_until::SKID_MARGIN` (crate-private): the planner
/// arms the overflow at `target − SKID_MARGIN` iff `delta > SKID_MARGIN`. If the
/// production margin ever changes, the record-count bound here goes wrong LOUDLY
/// (armed/unarmed misclassification ⇒ record violations on every affected
/// deadline), so drift cannot pass silently.
const SKID_MARGIN: u64 = 256;

/// Page-aligned guest RAM (mirrors `live_preemption.rs`).
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
        // SAFETY: same layout as alloc.
        unsafe { std::alloc::dealloc(self.ptr, self.layout) }
    }
}

/// The GuestMem alloc/slice/drop path is plain `std::alloc` unsafe with no
/// KVM dependency, so it IS Miri-reachable — this (non-ignored) test runs it
/// under `cargo +nightly miri test -p vmm-backend --test n2_nested_hammer`
/// (review finding: the hammer's new unsafe had no Miri-exercisable path).
/// The remaining unsafe in this file is the `map_memory` call, which is the
/// crate's existing box-only FFI seam.
#[test]
fn guest_mem_alloc_slice_drop_is_miri_clean() {
    let mut mem = GuestMem::new(8192);
    let s = mem.as_mut_slice();
    assert_eq!(s.len(), 8192);
    assert!(s.iter().all(|&b| b == 0), "alloc_zeroed really zeroes");
    s[0] = 0xAA;
    s[8191] = 0x55;
    assert_eq!(
        (mem.as_mut_slice()[0], mem.as_mut_slice()[8191]),
        (0xAA, 0x55)
    );
    drop(mem);
}

/// Busy-spin with a **memory-visible work oracle**: each iteration increments a
/// dword counter in guest RAM and retires exactly ONE conditional branch
/// (16-bit real mode; `inc` uses an operand-size prefix for a 32-bit counter):
///
/// ```text
/// 1000:  66 FF 06 00 20   inc dword ptr [0x2000]   ; memory-visible progress
/// 1005:  31 C0            xor ax, ax               ; ZF := 1 (branch always taken)
/// 1007:  74 F7            jz  0x1000               ; the ONE counted branch
/// ```
///
/// `run_until(target)` lands exactly after the `target`-th `jz` retires, at
/// which point the `inc` of every completed iteration — and no more — has
/// executed, so `[0x2000] == target mod 2^32` independent of the PMU.
const SPIN_CODE: &[u8] = &[0x66, 0xFF, 0x06, 0x00, 0x20, 0x31, 0xC0, 0x74, 0xF7];
const ENTRY: u64 = 0x1000;
/// The guest-RAM dword the oracle loop increments once per counted branch.
const COUNTER_GPA: u64 = 0x2000;
const RAM: usize = 0x10000;

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// xorshift64* — deterministic delta stream, no external dep.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Deterministic interleave of the three delta classes.
fn delta(rng: &mut Rng, i: u64) -> u64 {
    match i % 3 {
        0 => 1 + rng.next() % 64,         // MTF stepping edge
        1 => 128 + rng.next() % 385,      // skid-margin bracket (128..=512)
        _ => 4_096 + rng.next() % 95_905, // pure overflow path (4k..=100k)
    }
}

/// The allowed `PERF_RECORD_SAMPLE` count for one deadline of `delta` work
/// (see the module doc): `0..=0` unarmed, `1..=1 + SKID_MARGIN/period` armed.
fn allowed_samples(delta: u64) -> std::ops::RangeInclusive<u64> {
    if delta > SKID_MARGIN {
        let period = delta - SKID_MARGIN;
        1..=(1 + SKID_MARGIN / period)
    } else {
        0..=0
    }
}

#[test]
#[ignore = "SPIKE(nested-x86) live hammer; run via the N-2 appliance harness"]
fn n2_deadline_hammer() {
    let total = env_u64("N2_DEADLINES", 10_000);
    let seed = env_u64("N2_SEED", 0x5EED_2026);
    let progress_every = env_u64("N2_PROGRESS", 10_000);
    // Vacuity guard (PR #98 round-4 P2): a zero-deadline invocation would pass
    // the final assertion with all counters at zero — a green gate with no
    // evidence. Refuse it before touching the backend.
    assert!(
        total > 0,
        "N2_DEADLINES must be > 0 — a zero-deadline run is vacuously green, never evidence"
    );
    let mut rng = Rng(seed | 1);

    // Declared BEFORE `backend` so it drops AFTER it: the KVM memslot installed
    // by `map_memory` must never outlive the memory it references (the
    // `map_memory` SAFETY contract; PR #98 review finding 2 — the original
    // order was inverted).
    let mut mem = GuestMem::new(RAM);
    // The PATCHED backend, or fail loudly: `PatchedKvmBackend::new` errors with
    // `Capability` when `KVM_CAP_X86_DETERMINISTIC_INTERCEPTS` is absent, so a
    // stock-KVM environment can never silently produce "exact" evidence on the
    // wrong mechanism again (PR #98 review finding 1).
    let mut backend = PatchedKvmBackend::new().unwrap_or_else(|e| {
        panic!(
            "PatchedKvmBackend::new failed ({e}); the N-2 hammer REQUIRES the patched \
             kvm/kvm-intel modules (patches 0004/0005) — it must never fall back to stock"
        )
    });
    // SAFETY: `mem` outlives `backend` (declared before it, so dropped after
    // it), page-aligned, not aliased during run.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
    backend
        .set_policy(&X86Policy {
            cpuid: CpuidModel::default(),
            msr_filter: MsrFilter {
                allow_inkernel: vec![MsrRange {
                    base: 0x174,
                    count: 3,
                }],
            },
        })
        .expect("set_policy");
    backend.write_guest(Gpa(ENTRY), SPIN_CODE).expect("load");
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.regs.rip = ENTRY;
    st.regs.rflags = 0x2;
    backend.restore(&st).expect("restore setup state");

    let read_counter = |backend: &PatchedKvmBackend| -> u32 {
        let mut buf = [0u8; 4];
        backend
            .read_guest(Gpa(COUNTER_GPA), &mut buf)
            .expect("read oracle counter");
        u32::from_le_bytes(buf)
    };
    let read_stats = |backend: &PatchedKvmBackend| -> PmuOverflowStats {
        backend
            .pmu_overflow_stats()
            .expect("PMU branch counter must be open for the hammer")
    };

    let mut target = 0u64;
    // `deadlines` counts EVERY driven deadline. It is NOT an armed-PMI count:
    // a `d <= SKID_MARGIN` deadline is MTF-stepped with NO overflow armed. The
    // two classes are counted separately (`armed_pmi` / `mtf_only`) — the PR
    // #98 floor-accounting finding was exactly this conflation (a summary
    // field named `armed` that included unarmed deadlines, read back by the
    // floor checker). The authoritative armed-PMI count for floor purposes is
    // `records.samples` (counted from perf records), never a summary field.
    let mut deadlines = 0u64;
    let mut armed_pmi = 0u64;
    let mut mtf_only = 0u64;
    let mut exact = 0u64;
    let mut oracle_ok = 0u64;
    let mut mismatches: Vec<String> = Vec::new();
    let mut record_violations: Vec<String> = Vec::new();
    let mut stats_before = read_stats(&backend);

    println!(
        "N2JSON {{\"event\":\"start\",\"total\":{total},\"seed\":{seed},\"backend\":\"PatchedKvmBackend\",\"pid\":{}}}",
        std::process::id()
    );

    for i in 0..total {
        let d = delta(&mut rng, i);
        target += d;
        deadlines += 1;
        if d > SKID_MARGIN {
            armed_pmi += 1;
        } else {
            mtf_only += 1;
        }
        match backend.run_until(Moment(target)) {
            Ok(Exit::Common(CommonExit::Deadline { reached })) if reached.0 == target => {
                exact += 1;
                // Independent guest oracle: memory-visible progress must agree
                // with the PMU axis the landing was steered by.
                let counter = read_counter(&backend);
                let expect = (target & 0xFFFF_FFFF) as u32;
                if counter == expect {
                    oracle_ok += 1;
                } else {
                    mismatches.push(format!(
                        "i={i} target={target} ORACLE counter={counter} expect={expect}"
                    ));
                }
            }
            Ok(Exit::Common(CommonExit::Deadline { reached })) => {
                mismatches.push(format!(
                    "i={i} target={target} reached={} (delta {})",
                    reached.0,
                    reached.0 as i64 - target as i64
                ));
            }
            Ok(other) => mismatches.push(format!("i={i} target={target} exit={other:?}")),
            Err(e) => mismatches.push(format!("i={i} target={target} err={e}")),
        }
        // Per-record overflow-multiplicity accounting (counted, not inferred).
        let stats_after = read_stats(&backend);
        let samples = stats_after.samples - stats_before.samples;
        let lost = stats_after.lost - stats_before.lost;
        let throttle = stats_after.throttle - stats_before.throttle;
        let other = stats_after.other - stats_before.other;
        let allowed = allowed_samples(d);
        if lost != 0 || throttle != 0 || other != 0 || !allowed.contains(&samples) {
            record_violations.push(format!(
                "i={i} delta={d} samples={samples} allowed={}..={} lost={lost} throttle={throttle} other={other}",
                allowed.start(),
                allowed.end()
            ));
        }
        stats_before = stats_after;

        if mismatches.len() + record_violations.len() > 16 {
            break; // enough to diagnose; do not spin forever on a broken substrate
        }
        if progress_every != 0 && (i + 1) % progress_every == 0 {
            println!(
                "N2JSON {{\"event\":\"progress\",\"deadlines\":{deadlines},\"armed_pmi\":{armed_pmi},\"mtf_only\":{mtf_only},\"exact\":{exact},\"oracle_ok\":{oracle_ok},\"mismatches\":{},\"record_violations\":{},\"records\":{{\"samples\":{},\"lost\":{},\"throttle\":{},\"other\":{}}}}}",
                mismatches.len(),
                record_violations.len(),
                stats_after.samples,
                stats_after.lost,
                stats_after.throttle,
                stats_after.other
            );
        }
    }

    let totals = read_stats(&backend);
    println!(
        "N2JSON {{\"event\":\"summary\",\"deadlines\":{deadlines},\"armed_pmi\":{armed_pmi},\"mtf_only\":{mtf_only},\"exact\":{exact},\"oracle_ok\":{oracle_ok},\"mismatches\":{},\"record_violations\":{},\"final_work\":{target},\"records\":{{\"samples\":{},\"lost\":{},\"throttle\":{},\"other\":{}}}}}",
        mismatches.len(),
        record_violations.len(),
        totals.samples,
        totals.lost,
        totals.throttle,
        totals.other
    );
    for m in &mismatches {
        println!("N2JSON {{\"event\":\"mismatch\",\"detail\":\"{m}\"}}");
    }
    for v in &record_violations {
        println!("N2JSON {{\"event\":\"record_violation\",\"detail\":\"{v}\"}}");
    }
    assert!(
        mismatches.is_empty()
            && record_violations.is_empty()
            && exact == deadlines
            && oracle_ok == exact,
        "N-2 hammer: {} mismatches + {} record violations over {} deadlines \
         ({} armed-PMI + {} MTF-only; exact={}, oracle_ok={}) — one unexplained \
         mismatch is blocking (docs/NESTED-X86.md §kill conditions)",
        mismatches.len(),
        record_violations.len(),
        deadlines,
        armed_pmi,
        mtf_only,
        exact,
        oracle_ok
    );
}
