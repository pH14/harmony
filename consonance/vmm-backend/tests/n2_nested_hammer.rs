// SPDX-License-Identifier: AGPL-3.0-or-later
//! SPIKE(nested-x86): N-2 deadline hammer — **spike-branch-only apparatus**, not
//! production surface (see `docs/NESTED-X86.md` §N-2).
//!
//! Drives the **production** `Backend::run_until` path (patch-0004
//! `KVM_ARM_PREEMPT_EXIT` overflow arming + patch-0005 `KVM_ARM_MTF_STEP` exact
//! landing when the patched modules are loaded) over a long sequence of
//! seeded-random work targets on ONE busy-spin VM, and requires **every** landing
//! to be exact: `Exit::Deadline { reached } == target`, never overshoot, never a
//! different exit. Every attempted deadline is accounted in the machine-readable
//! JSON progress/summary lines this test emits on stdout (`N2JSON ...`).
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
#![cfg(target_os = "linux")]

use vmm_backend::{Backend, CpuidModel, Exit, Gpa, KvmBackend, MsrFilter, MsrRange, Vtime};

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

/// Infinite busy-spin retiring exactly one conditional branch per iteration
/// (identical to `live_preemption.rs`'s `SPIN_CODE`).
const SPIN_CODE: &[u8] = &[0x31, 0xC0, 0x85, 0xC0, 0x74, 0xFC];
const ENTRY: u64 = 0x1000;
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
        0 => 1 + rng.next() % 64,          // MTF stepping edge
        1 => 128 + rng.next() % 385,       // skid-margin bracket (128..=512)
        _ => 4_096 + rng.next() % 95_905,  // pure overflow path (4k..=100k)
    }
}

#[test]
#[ignore = "SPIKE(nested-x86) live hammer; run via the N-2 appliance harness"]
fn n2_deadline_hammer() {
    let total = env_u64("N2_DEADLINES", 10_000);
    let seed = env_u64("N2_SEED", 0x5EED_2026);
    let progress_every = env_u64("N2_PROGRESS", 10_000);
    let mut rng = Rng(seed | 1);

    let mut backend = KvmBackend::new()
        .unwrap_or_else(|e| panic!("KvmBackend::new failed ({e}); needs /dev/kvm + VMX"));
    let mut mem = GuestMem::new(RAM);
    // SAFETY: `mem` outlives `backend`, page-aligned, not aliased during run.
    unsafe { backend.map_memory(Gpa(0), mem.as_mut_slice()) }.expect("map_memory");
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
    backend.write_guest(Gpa(ENTRY), SPIN_CODE).expect("load");
    let mut st = backend.save().expect("save for setup");
    st.sregs.cs.base = 0;
    st.sregs.cs.selector = 0;
    st.regs.rip = ENTRY;
    st.regs.rflags = 0x2;
    backend.restore(&st).expect("restore setup state");

    let mut target = 0u64;
    let mut armed = 0u64;
    let mut exact = 0u64;
    let mut mismatches: Vec<String> = Vec::new();

    println!(
        "N2JSON {{\"event\":\"start\",\"total\":{total},\"seed\":{seed},\"pid\":{}}}",
        std::process::id()
    );

    for i in 0..total {
        target += delta(&mut rng, i);
        armed += 1;
        match backend.run_until(Vtime(target)) {
            Ok(Exit::Deadline { reached }) if reached.0 == target => exact += 1,
            Ok(Exit::Deadline { reached }) => {
                mismatches.push(format!(
                    "i={i} target={target} reached={} (delta {})",
                    reached.0,
                    reached.0 as i64 - target as i64
                ));
            }
            Ok(other) => mismatches.push(format!("i={i} target={target} exit={other:?}")),
            Err(e) => mismatches.push(format!("i={i} target={target} err={e}")),
        }
        if mismatches.len() > 16 {
            break; // enough to diagnose; do not spin forever on a broken substrate
        }
        if progress_every != 0 && (i + 1) % progress_every == 0 {
            println!(
                "N2JSON {{\"event\":\"progress\",\"armed\":{armed},\"exact\":{exact},\"mismatches\":{}}}",
                mismatches.len()
            );
        }
    }

    println!(
        "N2JSON {{\"event\":\"summary\",\"armed\":{armed},\"exact\":{exact},\"mismatches\":{},\"final_work\":{target}}}",
        mismatches.len()
    );
    for m in &mismatches {
        println!("N2JSON {{\"event\":\"mismatch\",\"detail\":\"{m}\"}}");
    }
    assert!(
        mismatches.is_empty() && exact == armed,
        "N-2 hammer: {} mismatches over {} armed deadlines (exact={}) — one unexplained \
         mismatch is blocking (docs/NESTED-X86.md §kill conditions)",
        mismatches.len(),
        armed,
        exact
    );
}
