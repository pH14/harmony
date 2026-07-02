// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 63 — **the box-only seal-rate measurement** (the Wave-5 go/no-go).
//! `#![cfg(target_os = "linux")]` **and `#[ignore]`**: needs real + LOADED patched KVM,
//! the built Postgres image, and the `det-cfl-v1` host. macOS builds an empty binary;
//! the pure bookkeeping this drives is covered portably by `src/seal_rate/` (gate 1).
//!
//! This harness adversarially measures the one empirical assumption the whole
//! `docs/EXPLORATION.md` archive rests on: that task 41 lets you **seal a snapshot at an
//! arbitrary mid-workload V-time** and branch from it deterministically — not just at the
//! handful of quiescent boundaries task 40 found (0 of 8392 sealable under the old codec).
//! It **diagnoses and measures**; it does not build the archive.
//!
//! ## What it does (task 63 §§1–5)
//!
//! 1. **Sample N ≥ 64 target V-times** across the post-readiness run
//!    ([`SamplingSchedule`]): uniform in retired V-time + a handful inside known-busy
//!    windows (interrupt service) discovered by a profiling pass. At each: `run` to the
//!    target, then `save_vm_state` — record success/failure **and the reason**.
//! 2. **Prove each successful seal is a real branch point**: restore + `reseed_entropy`
//!    **twice with the same seed**, run a fixed V-time horizon past the seal, and
//!    `state_hash` — the two must be **bit-identical** (no `step_error`, no skid). A seal
//!    that will not branch deterministically is **reclassified a failure** (§2), and —
//!    since task 41 already gated mid-workload determinism — the gate also fails **loud**
//!    (a genuine seal regression is a determinism-core bug to escalate, per the spec).
//! 3. **Adversarial pass**: a jittered schedule with a small timing perturbation staged
//!    just before each target (task 59's host-perturb path is not yet landed, so we vary
//!    the target + step the guest a little way in — landing it in a less "convenient",
//!    often non-synchronized interior state) and re-measure the seal rate.
//! 4. **Materialization depth**: seal a shallow parent and a deep child; materialize the
//!    child by `branch(parent) → run the suffix` and confirm it reproduces the child's
//!    live `state_hash` bit-for-bit — recording the suffix-vs-genesis replay-depth ratio.
//! 5. All of it rolls up through the **same** [`vmm_core::seal_rate`] bookkeeping the
//!    portable suite tests, ending in an explicit [`Ruling`] the report quotes.
//!
//! The numbers this prints are transcribed into `consonance/vmm-core/SEAL-RATE-REPORT.md`.
//!
//! ## Box-safety (CRITICAL — see the task spec)
//! Stock KVM = **1396736**; the patched module is larger. ALWAYS leave the box on stock +
//! verified after every run: `pkill -9 -f seal_rate_sweep` FIRST → wait
//! `lsmod | grep '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe
//! kvm_intel` → verify `lsmod | grep '^kvm '` == 1396736 on a FRESH ssh connection. Pin
//! to `taskset -c 2` (the standing frontier-gate core, `docs/BOX-PINNING.md`).
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux postgres-image     # build the image
//! # load patched kvm.ko/kvm-intel.ko, then (core 2, serialize with other frontier gates):
//! taskset -c 2 timeout 7200 cargo test -p vmm-core --test seal_rate_sweep \
//!     -- --ignored --nocapture --test-threads=1
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736.
//! ```
//!
//! Knobs (env): `TARGETS` (N, default 64), `BRANCH_HORIZON_VNS` (V-time run past each
//! seal for the determinism check, default 4_000_000), `ADV_JITTER_VNS` (default 50_000),
//! `ADV_PERTURB_STEPS` (max perturbation steps, default 4096), `WALL_BUDGET_SECS` (per
//! run, default 1800), `SPAN_START`/`SPAN_END` (skip the profiling pass and use this
//! V-time span directly), `BUSY_CENTERS` (comma-separated V-times to place
//! interrupt-service windows at when profiling is skipped), `BOOT_CMDLINE`.
#![cfg(target_os = "linux")]

use std::io::Write;
use std::time::{Duration, Instant};

use snapshot_store::SnapshotId;
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::seal_rate::{
    BusyKind, BusyWindow, CpuSnapshot, FailureReason, MaterializationDepth, Overshoot,
    PredicateQuality, Ruling, RulingInputs, RulingThresholds, SamplingSchedule, SealAttempt,
    SealResult, SealStats, VTime, ppm_percent, rate_ppm, sealable,
};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vmm::{Step, TerminalReason, Vmm, VmmError};

/// 2 GiB of guest RAM — identical to `live_postgres.rs` / `live_nonquiescent_snapshot.rs`.
const GUEST_RAM_LEN: usize = 2 << 30;
/// The base seed the live run uses (same value `live_postgres.rs` pins).
const BASE_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// A distinct branch seed for the §2 same-seed determinism check (the two branches use
/// *this* value; being the same for both is the point — same seed ⇒ identical run).
const BRANCH_SEED: u64 = BASE_SEED ^ 0x9E37_79B9_7F4A_7C15;
/// The determinism command line (identical to `live_nonquiescent_snapshot.rs`).
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Step budget per run — a high cap; the real bound is the wall budget + external timeout.
const MAX_STEPS: u64 = 50_000_000_000;

/// postgres announces this once the cluster is accepting connections (post-readiness = the
/// span we sample). The terminal itself is detected via `Step::Terminal`, not a serial marker.
const PG_READY: &[u8] = b"database system is ready to accept connections";

type DynVmm = Vmm<Box<dyn Backend>>;

// ---------------------------------------------------------------------------
// Preconditions + boot (loud panics; never a vacuous early-return Ok).
// ---------------------------------------------------------------------------

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("guest/build").join(name),
        repo_root().join("guest/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in guest/build or guest/linux — build it first on the \
         box: `make -C guest fetch && make -C guest/linux postgres-image`."
    );
}

fn require_kvm() {
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run this `#[ignore]`d box gate on the determinism box with the LOADED \
         patched KVM modules, CPU-pinned per docs/BOX-PINNING.md (taskset -c 2)."
    );
}

fn require_host_baseline() {
    let report = vmm_core::hostassert::report();
    let mut all = true;
    eprintln!("[host-assert] CPU-MSR-CONTRACT §1.1 baseline:");
    for o in &report {
        eprintln!(
            "[host-assert]   {}  {}: expected {}, observed {}",
            if o.pass { "PASS" } else { "FAIL" },
            o.key,
            o.expected,
            o.actual
        );
        all &= o.pass;
    }
    assert!(
        all,
        "host CPU is not the det-cfl-v1 baseline — run on the determinism box (i9-9900K) per \
         docs/BOX-PINNING.md."
    );
}

fn cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn wall_budget() -> Duration {
    Duration::from_secs(env_u64("WALL_BUDGET_SECS", 1800))
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn boot_pg(kernel: &[u8], initramfs: &[u8], seed: u64) -> DynVmm {
    boot_linux_selected(
        BackendKind::Patched,
        kernel,
        initramfs,
        GUEST_RAM_LEN,
        &cmdline(),
        seed,
    )
    .expect(
        "boot_linux_selected (patched) — needs the LOADED patched KVM modules + perf + det-cfl-v1 \
         host",
    )
}

/// A splitmix64 mixer for deterministic, RNG-free perturbation sizing (mirrors the one in
/// `src/seal_rate.rs`; kept local so the test needs no extra public surface).
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

// ---------------------------------------------------------------------------
// Run-control primitives.
// ---------------------------------------------------------------------------

/// What advancing the guest observed.
struct Advance {
    /// The effective V-time reached (`0` if V-time never advanced / unwired).
    landed_vtime: VTime,
    /// The terminal reason, if the guest ended.
    terminal: Option<TerminalReason>,
    /// A step error, if the run faulted.
    step_error: Option<String>,
    /// Steps taken in this call — kept for diagnostics/streamed logging (not always read).
    #[allow(dead_code)]
    steps: u64,
}

/// Stream any new serial bytes to stderr (so a hang shows the last line reached).
fn drain_serial(vmm: &DynVmm, printed: &mut usize) {
    let serial = vmm.serial();
    if serial.len() > *printed {
        let stderr = std::io::stderr();
        let mut h = stderr.lock();
        let _ = h.write_all(&serial[*printed..]);
        let _ = h.flush();
        *printed = serial.len();
    }
}

/// Step `vmm` forward until its effective V-time reaches `target`, it terminates, or a
/// budget is hit. Lands at the first V-time-synchronized boundary at/after `target` (the
/// only points where `effective_vns` advances), which is exactly where `save_vm_state`
/// can succeed. `printed` tracks streamed serial across calls.
fn run_to_vtime(vmm: &mut DynVmm, target: VTime, printed: &mut usize, start: Instant) -> Advance {
    let mut steps = 0u64;
    loop {
        if vmm.effective_vns().unwrap_or(0) >= target {
            return Advance {
                landed_vtime: vmm.effective_vns().unwrap_or(0),
                terminal: None,
                step_error: None,
                steps,
            };
        }
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                return Advance {
                    landed_vtime: vmm.effective_vns().unwrap_or(0),
                    terminal: Some(r),
                    step_error: None,
                    steps,
                };
            }
            Err(e) => {
                return Advance {
                    landed_vtime: vmm.effective_vns().unwrap_or(0),
                    terminal: None,
                    step_error: Some(format_err(&e)),
                    steps,
                };
            }
        }
        steps += 1;
        drain_serial(vmm, printed);
        if steps.is_multiple_of(8192) && (start.elapsed() > wall_budget() || steps > MAX_STEPS) {
            return Advance {
                landed_vtime: vmm.effective_vns().unwrap_or(0),
                terminal: None,
                step_error: Some(format!("wall/step budget hit after {steps} steps")),
                steps,
            };
        }
    }
}

/// Take up to `n` raw steps (the adversarial timing perturbation) — stops early on a
/// terminal or error. Lands the guest wherever those steps end, which may be a
/// non-synchronized interior exit.
fn perturb(vmm: &mut DynVmm, n: u64, printed: &mut usize) -> Advance {
    let mut steps = 0u64;
    while steps < n {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                return Advance {
                    landed_vtime: vmm.effective_vns().unwrap_or(0),
                    terminal: Some(r),
                    step_error: None,
                    steps,
                };
            }
            Err(e) => {
                return Advance {
                    landed_vtime: vmm.effective_vns().unwrap_or(0),
                    terminal: None,
                    step_error: Some(format_err(&e)),
                    steps,
                };
            }
        }
        steps += 1;
        drain_serial(vmm, printed);
    }
    Advance {
        landed_vtime: vmm.effective_vns().unwrap_or(0),
        terminal: None,
        step_error: None,
        steps,
    }
}

/// Flatten a `VmmError` and its source chain into one string (for classification/report).
fn format_err(e: &VmmError) -> String {
    let mut msg = format!("{e}");
    let mut src = std::error::Error::source(e);
    while let Some(s) = src {
        msg.push_str(&format!(" | {s}"));
        src = s.source();
    }
    msg
}

/// Read the seal-relevant features at the current landing **and** attempt the seal,
/// classifying the fail-closed reason from `save_vm_state`. Returns the observed
/// [`CpuSnapshot`], the seal reason (`None` == sealed), and — when it sealed — the encoded
/// `vm_state` blob to store.
fn probe_seal(vmm: &mut DynVmm) -> (CpuSnapshot, Option<FailureReason>, Option<Vec<u8>>) {
    // `has_pending_guest_interrupt` re-arbitrates (peek, no IRR→ISR move) so it does not
    // perturb the snapshot; the injection reads are best-effort and read-only.
    let pending = vmm.has_pending_guest_interrupt().unwrap_or(false);
    let inflight = vmm.has_inflight_event_injection();
    let active = vmm.has_active_event_injection();

    match vmm.save_vm_state() {
        Ok(vm_state) => {
            let blob = vm_state.encode().expect("a sealed vm_state always encodes");
            (
                CpuSnapshot {
                    synchronized: true,
                    rng_mid_exit: false,
                    unrepresentable: false,
                    inflight_injection: inflight,
                    active_injection: active,
                    pending_guest_interrupt: pending,
                },
                None,
                Some(blob),
            )
        }
        Err(e) => {
            let msg = format_err(&e);
            // Classify against the exact fail-closed messages of `Vmm::save_vm_state`.
            let (synchronized, rng_mid, unrep, reason) = if msg.contains("non-synchronized") {
                (false, false, false, FailureReason::NonSynchronized)
            } else if msg.contains("RNG mid-exit") {
                (true, true, false, FailureReason::RngMidExit)
            } else {
                // representability fail-closed (kvm_sregs2 flags/pdptrs, debugregs.flags,
                // triple_fault/payload) — essentially never for the 64-bit guest.
                (true, false, true, FailureReason::Unrepresentable)
            };
            (
                CpuSnapshot {
                    synchronized,
                    rng_mid_exit: rng_mid,
                    unrepresentable: unrep,
                    inflight_injection: inflight,
                    active_injection: active,
                    pending_guest_interrupt: pending,
                },
                Some(reason),
                None,
            )
        }
    }
}

/// One branch's terminal observation for the §2 determinism check.
struct BranchOutcome {
    hash: [u8; 32],
    step_error: Option<String>,
    /// Whether a skid / DIAG-SKID49 marker appeared in the error (a determinism violation).
    skid: bool,
}

/// Restore `snap` into a fresh VM, `reseed_entropy(seed)` (a branch), run `horizon_vns`
/// of V-time past the seal, and return the terminal `state_hash`. The VM + its CoW mapping
/// (and single perf counter) are dropped on return.
fn branch_and_run(
    engine: &SnapshotEngine,
    snap: SnapshotId,
    kernel: &[u8],
    initramfs: &[u8],
    seed: u64,
    horizon_vns: VTime,
    start: Instant,
) -> BranchOutcome {
    let mut vmm = boot_pg(kernel, initramfs, seed);
    let mapping = engine
        .materialize(snap)
        .expect("materialize the sealed base");
    let vm_state = engine.vm_state(snap).expect("decode the sealed vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore the sealed snapshot into the fresh VM");
    vmm.reseed_entropy(seed)
        .expect("reseed the entropy stream for the branch");
    let seal_vtime = vmm.effective_vns().unwrap_or(0);
    let mut printed = vmm.serial().len();
    let adv = run_to_vtime(&mut vmm, seal_vtime + horizon_vns, &mut printed, start);
    let skid = adv
        .step_error
        .as_deref()
        .is_some_and(|e| e.contains("skid") || e.contains("DIAG-SKID49"));
    BranchOutcome {
        hash: vmm.state_hash(),
        step_error: adv.step_error,
        skid,
    }
}

/// Replay `snap` **verbatim** (restore, no reseed) into a fresh VM and run to `to_vtime`,
/// returning the reached `state_hash`. Used for the §4 materialization-depth reproduction
/// (parent-rooted suffix replay).
fn replay_to(
    engine: &SnapshotEngine,
    snap: SnapshotId,
    kernel: &[u8],
    initramfs: &[u8],
    to_vtime: VTime,
    start: Instant,
) -> [u8; 32] {
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mapping = engine.materialize(snap).expect("materialize");
    let vm_state = engine.vm_state(snap).expect("decode vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore");
    let mut printed = vmm.serial().len();
    let _ = run_to_vtime(&mut vmm, to_vtime, &mut printed, start);
    vmm.state_hash()
}

/// Boot a fresh VM at `BASE_SEED` and run to `to_vtime` from genesis, returning the reached
/// `state_hash` — the from-genesis leg of §4.
fn boot_and_run_to(kernel: &[u8], initramfs: &[u8], to_vtime: VTime, start: Instant) -> [u8; 32] {
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mut printed = 0usize;
    let _ = run_to_vtime(&mut vmm, to_vtime, &mut printed, start);
    vmm.state_hash()
}

// ---------------------------------------------------------------------------
// Profiling: find the post-readiness span + a handful of busy windows.
// ---------------------------------------------------------------------------

/// The V-time span to sample + the busy windows to aim a handful of targets at.
struct Profile {
    span_start: VTime,
    span_end: VTime,
    busy: Vec<BusyWindow>,
}

/// Run one live guest to a clean terminal, recording the V-time at `PG_READY` (span start),
/// the terminal V-time (span end), and up to three interrupt-service busy windows (V-times
/// where a genuine active event injection is in flight). Deterministic (same seed ⇒ same
/// timeline as the measurement passes).
fn profile(kernel: &[u8], initramfs: &[u8], start: Instant) -> Profile {
    // Allow skipping the (expensive) profiling run by pinning the span + busy centers.
    if let (Ok(s), Ok(e)) = (std::env::var("SPAN_START"), std::env::var("SPAN_END")) {
        let span_start: VTime = s.parse().expect("SPAN_START is a u64");
        let span_end: VTime = e.parse().expect("SPAN_END is a u64");
        assert!(span_start < span_end, "SPAN_START must be < SPAN_END");
        let busy: Vec<BusyWindow> = std::env::var("BUSY_CENTERS")
            .ok()
            .map(|v| {
                v.split(',')
                    .filter_map(|t| t.trim().parse::<VTime>().ok())
                    .map(|c| BusyWindow {
                        start: c.saturating_sub(500),
                        end: c + 500,
                        kind: BusyKind::InterruptService,
                    })
                    .collect()
            })
            .unwrap_or_default();
        eprintln!(
            "[profile] SPAN pinned via env: [{span_start}, {span_end}), {} busy window(s)",
            busy.len()
        );
        return Profile {
            span_start,
            span_end,
            busy,
        };
    }

    eprintln!("[profile] === booting live guest to profile the post-readiness span ===");
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mut printed = 0usize;
    let mut span_start = 0u64;
    let mut ready = false;
    let mut busy_centers: Vec<VTime> = Vec::new();
    let mut steps = 0u64;
    let mut terminal_vtime;
    loop {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                terminal_vtime = vmm.effective_vns().unwrap_or(0);
                eprintln!(
                    "[profile] terminal {r:?} at V-time {terminal_vtime} after {steps} steps"
                );
                break;
            }
            Err(e) => {
                terminal_vtime = vmm.effective_vns().unwrap_or(0);
                eprintln!(
                    "[profile] step error after {steps} steps: {}",
                    format_err(&e)
                );
                break;
            }
        }
        steps += 1;
        drain_serial(&vmm, &mut printed);
        if !ready && find(vmm.serial(), PG_READY) {
            ready = true;
            span_start = vmm.effective_vns().unwrap_or(0);
            eprintln!("[profile] PG_READY at V-time {span_start}");
        }
        // Sample a few busy windows: genuine active event injection in the post-readiness
        // phase (interrupt service). Only at synchronized boundaries (effective_vns valid).
        if ready
            && busy_centers.len() < 3
            && vmm.has_active_event_injection()
            && let Some(v) = vmm.effective_vns()
            && busy_centers.last().is_none_or(|&last| v > last + 1_000_000)
        {
            busy_centers.push(v);
        }
        if steps.is_multiple_of(8192) && start.elapsed() > wall_budget() {
            terminal_vtime = vmm.effective_vns().unwrap_or(0);
            eprintln!("[profile] wall budget hit after {steps} steps at V-time {terminal_vtime}");
            break;
        }
    }
    assert!(
        ready,
        "guest never reached PG_READY — cannot define a post-readiness span"
    );
    assert!(
        terminal_vtime > span_start,
        "terminal V-time {terminal_vtime} not after PG_READY {span_start}"
    );
    let busy: Vec<BusyWindow> = busy_centers
        .iter()
        .map(|&c| BusyWindow {
            start: c.saturating_sub(500),
            end: c + 500,
            kind: BusyKind::InterruptService,
        })
        .collect();
    eprintln!(
        "[profile] post-readiness span [{span_start}, {terminal_vtime}), {} busy window(s) at {:?}",
        busy.len(),
        busy_centers
    );
    Profile {
        span_start,
        span_end: terminal_vtime,
        busy,
    }
}

// ---------------------------------------------------------------------------
// The measurement.
// ---------------------------------------------------------------------------

/// A successful nominal seal we retained for the §2 determinism check + §4 depth.
struct Sealed {
    attempt_idx: usize,
    snap: SnapshotId,
    seal_vtime: VTime,
    live_hash: [u8; 32],
}

#[test]
#[ignore = "box-only seal-rate measurement (LOADED patched KVM + Postgres image + det-cfl-v1 host); \
            run pinned to core 2 with `-- --ignored --nocapture`, then revert KVM to stock 1396736"]
fn seal_rate_sweep() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");

    let n = env_usize("TARGETS", 64);
    assert!(n >= 64, "task 63 requires N >= 64 target Moments (got {n})");
    let horizon = env_u64("BRANCH_HORIZON_VNS", 4_000_000);
    let jitter = env_u64("ADV_JITTER_VNS", 50_000);
    let perturb_max = env_u64("ADV_PERTURB_STEPS", 4096);
    eprintln!(
        "[sweep] TARGETS={n} BRANCH_HORIZON_VNS={horizon} ADV_JITTER_VNS={jitter} \
         ADV_PERTURB_STEPS={perturb_max} cmdline={:?}",
        cmdline()
    );

    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with the external
    // `timeout`) that bounds this `#[ignore]`d box gate; it never reaches guest state, the serial
    // capture, or any hash — mirrors `live_branching_demo.rs`.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();

    // --- Profiling: span + busy windows ------------------------------------
    let prof = profile(&kernel, &initramfs, start);
    let schedule = SamplingSchedule::build(prof.span_start, prof.span_end, n, &prof.busy)
        .expect("build the sampling schedule over the post-readiness span");
    eprintln!(
        "[sweep] schedule: {} targets ({} uniform + {} busy) across [{}, {})",
        schedule.len(),
        schedule.uniform_count(),
        schedule.busy_count(),
        prof.span_start,
        prof.span_end
    );

    // --- 1. Nominal pass: one live guest, seal-tested at each target --------
    eprintln!(
        "\n[sweep] === nominal pass: run→seal at each of {} targets ===",
        schedule.len()
    );
    let mut nominal: Vec<SealAttempt> = Vec::with_capacity(schedule.len());
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let mut sealed: Vec<Sealed> = Vec::new();
    {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let mut printed = 0usize;
        let mut prev_snap: Option<SnapshotId> = None;
        for (i, &target) in schedule.targets().iter().enumerate() {
            let adv = run_to_vtime(&mut live, target.vtime, &mut printed, start);
            if let Some(r) = adv.terminal {
                panic!(
                    "guest reached a terminal ({r:?}) at V-time {} before target {} (idx {i}) — the \
                     span/timeline is inconsistent with profiling",
                    adv.landed_vtime, target.vtime
                );
            }
            if let Some(e) = adv.step_error {
                panic!(
                    "run_to_vtime failed reaching target {} (idx {i}): {e}",
                    target.vtime
                );
            }
            let landed = live.effective_vns().unwrap_or(0);
            let (snapshot, reason, blob) = probe_seal(&mut live);
            let result = match (reason, blob) {
                (None, Some(blob)) => {
                    // Store the memory image alongside the vm_state (base for the first,
                    // dirty-deduped derive for the rest — bounds store memory).
                    let snap = match prev_snap {
                        None => engine
                            .snapshot_base(live.guest_memory(), &blob)
                            .expect("snapshot base"),
                        Some(parent) => engine
                            .snapshot_derive(parent, live.guest_memory(), None, &blob)
                            .expect("snapshot derive"),
                    };
                    prev_snap = Some(snap);
                    sealed.push(Sealed {
                        attempt_idx: i,
                        snap,
                        seal_vtime: landed,
                        live_hash: live.state_hash(),
                    });
                    SealResult::Sealed
                }
                (Some(reason), _) => SealResult::Failed(reason),
                (None, None) => unreachable!("sealed Ok always yields a blob"),
            };
            nominal.push(SealAttempt {
                target,
                landed_vtime: landed,
                snapshot,
                result,
            });
        }
        eprintln!(
            "[sweep] nominal: {} sealed / {} targets ({} retained for determinism check)",
            nominal.iter().filter(|a| a.result.is_sealed()).count(),
            nominal.len(),
            sealed.len()
        );
        // Drop the live guest (frees its perf counter) before any fork boots.
    }

    // --- 2. Prove each successful seal is a real branch point --------------
    eprintln!("\n[sweep] === branch-determinism check: 2 same-seed branches per sealed point ===");
    let mut nondeterministic: Vec<(VTime, String)> = Vec::new();
    for s in &sealed {
        let b1 = branch_and_run(
            &engine,
            s.snap,
            &kernel,
            &initramfs,
            BRANCH_SEED,
            horizon,
            start,
        );
        let b2 = branch_and_run(
            &engine,
            s.snap,
            &kernel,
            &initramfs,
            BRANCH_SEED,
            horizon,
            start,
        );
        let ok = b1.step_error.is_none()
            && b2.step_error.is_none()
            && !b1.skid
            && !b2.skid
            && b1.hash == b2.hash;
        if !ok {
            let why = format!(
                "hash1={} hash2={} err1={:?} err2={:?} skid1={} skid2={}",
                hex(&b1.hash),
                hex(&b2.hash),
                b1.step_error,
                b2.step_error,
                b1.skid,
                b2.skid
            );
            eprintln!(
                "[sweep] !! seal at V-time {} did NOT branch deterministically: {why}",
                s.seal_vtime
            );
            nondeterministic.push((s.seal_vtime, why));
            // §2: reclassify the seal as a failure.
            nominal[s.attempt_idx].result =
                SealResult::Failed(FailureReason::BranchNondeterministic);
        }
    }
    let determinism_verified = !sealed.is_empty() && nondeterministic.is_empty();
    eprintln!(
        "[sweep] branch-determinism: {}/{} sealed points bit-identical across 2 same-seed branches",
        sealed.len() - nondeterministic.len(),
        sealed.len()
    );

    // --- 3. Adversarial pass (§3) + interior grid-probe (supports §5) -------
    //
    // Two distinct questions, one guest run per jittered target:
    //   (a) ADVERSARIAL (§3): run to the jittered target's boundary and seal. Jitter lands
    //       the guest at *different, often busier* synchronized boundaries (more likely to
    //       carry in-flight injection). This tests whether task 41's non-quiescent capture
    //       is robust — does sealing hold when perturbed into a less "convenient" state, or
    //       only at incidentally-quiescent points? A drop here is the real §3 finding.
    //   (b) INTERIOR probe: from that boundary, step a deterministic little way in (a timing
    //       perturbation) and seal at the *interior*. Interior points are usually
    //       non-synchronized (exact V-time is known only at intercepts), so these mostly
    //       fail — that is the fundamental addressability limit, and it supplies the
    //       negatives that make `sealable()`'s precision/recall (§5) non-trivial and defines
    //       why the archive keys exemplars to boundaries, not arbitrary interior Moments.
    eprintln!("\n[sweep] === adversarial (§3) + interior grid-probe (§5 negatives) ===");
    let adv_schedule = schedule.jittered(jitter);
    let mut adversarial: Vec<SealAttempt> = Vec::with_capacity(adv_schedule.len());
    let mut interior: Vec<SealAttempt> = Vec::with_capacity(adv_schedule.len());
    {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let mut printed = 0usize;
        for &target in adv_schedule.targets() {
            let adv = run_to_vtime(&mut live, target.vtime, &mut printed, start);
            if adv.terminal.is_some() || adv.step_error.is_some() {
                // Near the tail a jittered target may sit at/after terminal — stop probing.
                break;
            }
            // (a) Adversarial: seal at the (busier) jittered boundary.
            let landed = live.effective_vns().unwrap_or(0);
            let (snapshot, reason, _blob) = probe_seal(&mut live);
            adversarial.push(SealAttempt {
                target,
                landed_vtime: landed,
                snapshot,
                result: match reason {
                    None => SealResult::Sealed,
                    Some(r) => SealResult::Failed(r),
                },
            });
            // (b) Interior probe: perturb a deterministic little way past the boundary and
            //     seal at the interior (the guest is now off any V-time intercept).
            let extra = 1 + splitmix64(target.vtime) % perturb_max.max(1);
            let padv = perturb(&mut live, extra, &mut printed);
            if padv.terminal.is_some() {
                break;
            }
            let ilanded = live.effective_vns().unwrap_or(0);
            let (isnap, ireason, _b) = probe_seal(&mut live);
            interior.push(SealAttempt {
                target,
                landed_vtime: ilanded,
                snapshot: isnap,
                result: match ireason {
                    None => SealResult::Sealed,
                    Some(r) => SealResult::Failed(r),
                },
            });
        }
        // Drop the adversarial guest.
    }
    eprintln!(
        "[sweep] adversarial (boundary): {} sealed / {} probed | interior: {} sealed / {} probed",
        adversarial.iter().filter(|a| a.result.is_sealed()).count(),
        adversarial.len(),
        interior.iter().filter(|a| a.result.is_sealed()).count(),
        interior.len(),
    );

    // --- 4. Materialization depth (parent-rooted premise) ------------------
    let depth = materialization_depth(&engine, &sealed, &kernel, &initramfs, start);

    // --- 5. Roll up + emit the report block --------------------------------
    let nominal_stats = SealStats::of(&nominal);
    let adversarial_stats = SealStats::of(&adversarial);
    let interior_stats = SealStats::of(&interior);
    let overshoot = Overshoot::of(&nominal);
    // Predicate quality over all three passes: nominal + adversarial supply the boundary
    // positives, the interior probe supplies the non-synchronized negatives — so `sealable`
    // is measured against both classes it must discriminate (§5).
    let mut all_attempts = nominal.clone();
    all_attempts.extend_from_slice(&adversarial);
    all_attempts.extend_from_slice(&interior);
    let predicate = PredicateQuality::measure(&all_attempts, sealable);

    let inputs = RulingInputs {
        nominal: nominal_stats.clone(),
        adversarial: adversarial_stats.clone(),
        determinism_verified,
        overshoot,
    };
    let ruling = vmm_core::seal_rate::rule(&inputs, RulingThresholds::default());

    emit_report(
        &schedule,
        &nominal_stats,
        &adversarial_stats,
        &interior_stats,
        overshoot,
        &predicate,
        depth,
        &nondeterministic,
        ruling,
    );

    // A NO-GO / grid-restricted verdict is a *valid finding*, not a test failure — do not
    // assert on the ruling. Assert only the structural + regression invariants:
    assert_eq!(nominal.len(), n, "must have measured all N targets");
    assert!(n >= 64, "N >= 64");
    assert!(
        !sealed.is_empty(),
        "no target sealed at all — task 41's non-quiescent capture regressed (escalate)"
    );
    // A genuine seal that will not branch deterministically is a determinism-core
    // regression to escalate (task 63 non-goal: do not patch it here).
    assert!(
        nondeterministic.is_empty(),
        "SEAL REGRESSION — {} sealed point(s) did not branch deterministically; escalate to the \
         foreman (task 41/63 determinism-core bug, not this harness's to patch): {:?}",
        nondeterministic.len(),
        nondeterministic
    );
}

/// §4: take the **deepest** sealed point as the child and its **nearest shallower** sealed
/// ancestor as the parent (exactly the "nearest retained ancestor" an `Archive` would root a
/// virtual exemplar at). Confirm the child materializes bit-identically by branching from the
/// parent and replaying only the suffix, cross-check from genesis, and return the
/// suffix-vs-genesis depth ratio. Rooting at the *nearest* ancestor (not the shallowest) is
/// the point — it makes the suffix one inter-sample gap, demonstrating cost = suffix ≪ prefix.
/// `None` if fewer than two distinct-depth seals exist.
fn materialization_depth(
    engine: &SnapshotEngine,
    sealed: &[Sealed],
    kernel: &[u8],
    initramfs: &[u8],
    start: Instant,
) -> Option<MaterializationDepth> {
    if sealed.len() < 2 {
        eprintln!("[sweep] §4 materialization depth: <2 seals, skipped");
        return None;
    }
    let child = sealed.iter().max_by_key(|s| s.seal_vtime).unwrap();
    // The nearest sealed ancestor strictly shallower than the child.
    let parent = sealed
        .iter()
        .filter(|s| s.seal_vtime < child.seal_vtime)
        .max_by_key(|s| s.seal_vtime);
    let Some(parent) = parent else {
        eprintln!("[sweep] §4 materialization depth: no distinct-depth pair, skipped");
        return None;
    };
    if parent.seal_vtime == 0 {
        eprintln!("[sweep] §4 materialization depth: parent at genesis, skipped");
        return None;
    }
    eprintln!(
        "\n[sweep] === §4 materialization depth: parent V-time {} → child V-time {} ===",
        parent.seal_vtime, child.seal_vtime
    );
    // Parent-rooted: branch(parent) verbatim → run the suffix to the child's V-time.
    let from_parent_hash = replay_to(
        engine,
        parent.snap,
        kernel,
        initramfs,
        child.seal_vtime,
        start,
    );
    // From-genesis cross-check (pure determinism — should also reproduce the child).
    let from_genesis_hash = boot_and_run_to(kernel, initramfs, child.seal_vtime, start);

    let parent_ok = from_parent_hash == child.live_hash;
    let genesis_ok = from_genesis_hash == child.live_hash;
    eprintln!(
        "[sweep] §4 child live_hash={}\n[sweep] §4 parent-rooted reproduces={} (hash={})\n[sweep] \
         §4 genesis-rooted reproduces={} (hash={})",
        hex(&child.live_hash),
        parent_ok,
        hex(&from_parent_hash),
        genesis_ok,
        hex(&from_genesis_hash),
    );
    assert!(
        parent_ok,
        "§4 parent-rooted materialization did NOT reproduce the child's live state_hash — the \
         parent-rooted lazy-materialization premise fails (escalate)"
    );
    MaterializationDepth::new(0, parent.seal_vtime, child.seal_vtime).ok()
}

/// Print the full measurement in the shape `SEAL-RATE-REPORT.md` transcribes.
#[allow(clippy::too_many_arguments)]
fn emit_report(
    schedule: &SamplingSchedule,
    nominal: &SealStats,
    adversarial: &SealStats,
    interior: &SealStats,
    overshoot: Option<Overshoot>,
    predicate: &PredicateQuality,
    depth: Option<MaterializationDepth>,
    nondeterministic: &[(VTime, String)],
    ruling: Ruling,
) {
    eprintln!("\n[REPORT] ======================= SEAL-RATE MEASUREMENT =======================");
    eprintln!(
        "[REPORT] schedule: {} targets ({} uniform + {} busy)",
        schedule.len(),
        schedule.uniform_count(),
        schedule.busy_count()
    );
    eprintln!(
        "[REPORT] NOMINAL   seal rate: {}/{} = {}",
        nominal.sealed,
        nominal.n,
        ppm_percent(nominal.success_rate_ppm)
    );
    for (reason, count) in &nominal.by_reason {
        if *count > 0 {
            eprintln!("[REPORT]     nominal fail [{reason}]: {count}");
        }
    }
    eprintln!(
        "[REPORT] ADVERSARIAL seal rate: {}/{} = {}",
        adversarial.sealed,
        adversarial.n,
        ppm_percent(adversarial.success_rate_ppm)
    );
    for (reason, count) in &adversarial.by_reason {
        if *count > 0 {
            eprintln!("[REPORT]     adversarial fail [{reason}]: {count}");
        }
    }
    eprintln!(
        "[REPORT] INTERIOR (grid-probe) seal rate: {}/{} = {} (non-boundary points; low is \
         expected — defines the addressable grid, not a NO-GO)",
        interior.sealed,
        interior.n,
        ppm_percent(interior.success_rate_ppm)
    );
    for (reason, count) in &interior.by_reason {
        if *count > 0 {
            eprintln!("[REPORT]     interior fail [{reason}]: {count}");
        }
    }
    eprintln!(
        "[REPORT] branch-determinism: {} nondeterministic sealed point(s) (must be 0)",
        nondeterministic.len()
    );
    if let Some(o) = overshoot {
        eprintln!(
            "[REPORT] addressability (overshoot V-time ns): min={} p50={} p90={} max={} mean={} \
             exact_hits={}/{}",
            o.min, o.p50, o.p90, o.max, o.mean, o.exact_hits, o.n
        );
    }
    if let Some(d) = depth {
        eprintln!(
            "[REPORT] materialization depth: from_parent={} from_genesis={} ratio={} savings={}",
            d.from_parent,
            d.from_genesis,
            ppm_percent(d.ratio_ppm()),
            ppm_percent(d.savings_ppm())
        );
    }
    eprintln!(
        "[REPORT] sealable() predicate: TP={} FP={} TN={} FN={} precision={} recall={}",
        predicate.true_pos,
        predicate.false_pos,
        predicate.true_neg,
        predicate.false_neg,
        ppm_percent(predicate.precision_ppm),
        ppm_percent(predicate.recall_ppm),
    );
    eprintln!(
        "[REPORT] (rate_ppm cross-check: nominal={} adversarial={})",
        rate_ppm(nominal.sealed, nominal.n),
        rate_ppm(adversarial.sealed, adversarial.n),
    );
    eprintln!("[REPORT] ------------------------------------------------------------");
    eprintln!("[REPORT] RULING: {}", ruling.label());
    eprintln!("[REPORT] ============================================================");
}
