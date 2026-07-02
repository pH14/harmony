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
//! 4. **Materialization depth**: seal a shallow parent and a deep child; materialize the child
//!    by `branch(parent) → run the suffix` and confirm it reproduces the **genesis-rooted**
//!    materialization bit-for-bit (ancestor-independence) — recording the suffix-vs-genesis
//!    replay-depth ratio. Then the **schedule-faithful replay** (distinguishing experiment,
//!    "§4b"): the clean replays agree with each other but diverge from the probe-laden live run,
//!    so restore the parent and re-run the live legs *with the same `run(deadline)`+`probe_seal`
//!    schedule* — match ⇒ the schedule is a deterministic part of the trajectory (substrate
//!    sound); mismatch ⇒ escalate.
//! 5. All of it rolls up through the **same** [`vmm_core::seal_rate`] bookkeeping the
//!    portable suite tests; the final GO/NO-GO ruling is the integrator's.
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
//! `ADV_PERTURB_STEPS` (max perturbation steps, default 32), `DET_SUBSET` (how many spread
//! seals get the full 2 GiB snapshot + §2/§4 branch-verify, default 24), `WALL_BUDGET_SECS`
//! (per **guest**, default 1800), `SPAN_START`/`SPAN_END` (skip the profiling pass and use
//! this V-time span directly), `BUSY_CENTERS` (comma-separated V-times to place
//! interrupt-service windows at when profiling is skipped), `UNPROBED_TAIL_ALLOWANCE` (max §3
//! jittered targets allowed to be dropped past a terminal break, default 4 — a mid-span step
//! error always fails), `BOOT_CMDLINE`.
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

/// A fresh **per-guest** watchdog clock. `run_to_vtime` bounds each guest run by
/// `WALL_BUDGET_SECS` measured from this — so a legitimately long multi-phase sweep never
/// trips the budget just because the *cumulative* time crossed it (a global start would).
/// not order-observable: a test-only wall-clock watchdog, belt-and-braces with the external
/// `timeout`; it never reaches guest state, the serial capture, or any hash.
#[allow(clippy::disallowed_methods)]
fn watchdog_start() -> Instant {
    Instant::now()
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
    /// The V-time actually reached — `< seal_vtime + horizon_vns` iff the branch hit terminal
    /// (the workload ended) before the requested horizon. A same-terminal-hash pair whose
    /// `reached_vtime` fell short was verified over a **truncated** horizon (weaker evidence),
    /// so §2 records it rather than silently counting it as full-horizon.
    reached_vtime: VTime,
    /// Hit terminal before the requested horizon.
    early_terminal: bool,
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
    let adv = run_to_vtime(
        &mut vmm,
        seal_vtime + horizon_vns,
        &mut printed,
        watchdog_start(),
    );
    let skid = adv
        .step_error
        .as_deref()
        .is_some_and(|e| e.contains("skid") || e.contains("DIAG-SKID49"));
    let reached_vtime = adv.landed_vtime;
    BranchOutcome {
        hash: vmm.state_hash(),
        step_error: adv.step_error,
        skid,
        reached_vtime,
        early_terminal: adv.terminal.is_some(),
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
) -> [u8; 32] {
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mapping = engine.materialize(snap).expect("materialize");
    let vm_state = engine.vm_state(snap).expect("decode vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore");
    let mut printed = vmm.serial().len();
    let _ = run_to_vtime(&mut vmm, to_vtime, &mut printed, watchdog_start());
    vmm.state_hash()
}

/// Boot a fresh VM at `BASE_SEED` and run to `to_vtime` from genesis, returning the reached
/// `state_hash` — the from-genesis leg of §4.
fn boot_and_run_to(kernel: &[u8], initramfs: &[u8], to_vtime: VTime) -> [u8; 32] {
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mut printed = 0usize;
    let _ = run_to_vtime(&mut vmm, to_vtime, &mut printed, watchdog_start());
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
fn profile(kernel: &[u8], initramfs: &[u8]) -> Profile {
    let start = watchdog_start();
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
    let terminal_vtime;
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
                // A step error mid-profiling truncates the span silently — every later target
                // would be measured against a bogus [ready, error) window. Fail loudly instead
                // (analogous to the §3 honest-denominator gate); a clean `Terminal` is the only
                // valid span end.
                panic!(
                    "[profile] step error after {steps} steps at V-time {} — the post-readiness \
                     span would be truncated; refusing to profile a bogus span: {}",
                    vmm.effective_vns().unwrap_or(0),
                    format_err(&e)
                );
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
            // A wall-budget timeout during profiling means we never reached a clean terminal —
            // recording the timeout point as `span_end` would present a truncated `[ready,
            // timeout)` prefix as a clean span, and every later measurement would run over it.
            // Hard-fail (the profile is INCOMPLETE); a clean `Step::Terminal` is the only valid
            // span end. Raise `WALL_BUDGET_SECS`, or pin `SPAN_START`/`SPAN_END` from a good run.
            panic!(
                "[profile] WALL_BUDGET_SECS hit after {steps} steps at V-time {} (ready={ready}) \
                 BEFORE a clean terminal — the post-readiness span would be truncated; refusing to \
                 measure over an INCOMPLETE profile",
                vmm.effective_vns().unwrap_or(0)
            );
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

/// A successful nominal seal we retained for the §2 determinism check + §4/§4b.
struct Sealed {
    /// Index of this seal's target in the schedule (its position in the live leg sequence).
    attempt_idx: usize,
    snap: SnapshotId,
    seal_vtime: VTime,
    /// `state_hash` at the landing **before** `probe_seal` (a clean replay reproduces this).
    live_hash_clean: [u8; 32],
    /// `state_hash` at the landing **after** `probe_seal` (the probe-laden live trajectory —
    /// what §4b's schedule-faithful replay must reproduce).
    live_hash_probed: [u8; 32],
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
    // A SMALL perturbation: a handful of raw steps lands the guest a little way past the
    // boundary (usually a non-synchronized interior point). Large values advance V-time by
    // millions and exhaust the span after a few targets — the first run's §3 bug.
    let perturb_max = env_u64("ADV_PERTURB_STEPS", 32);
    // §1 (the seal rate) probes `save_vm_state` at ALL N targets (cheap). §2/§4 need the full
    // 2 GiB memory snapshot, which is expensive (~seconds each) and would make branching all N
    // impractical — so a spread subset of `DET_SUBSET` successful seals is fully snapshotted and
    // branch-verified (see IMPLEMENTATION.md; the ruling needs a determinism-clean spread, not
    // all N). Independent `snapshot_base`s (not a derive chain) keep materialize O(1)-layer.
    let n_det = env_usize("DET_SUBSET", 24).max(2);
    eprintln!(
        "[sweep] TARGETS={n} DET_SUBSET={n_det} BRANCH_HORIZON_VNS={horizon} \
         ADV_JITTER_VNS={jitter} ADV_PERTURB_STEPS={perturb_max} cmdline={:?}",
        cmdline()
    );

    // --- Profiling: span + busy windows ------------------------------------
    let prof = profile(&kernel, &initramfs);
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
    let last_idx = schedule.len().saturating_sub(1);
    // Snapshot a successful seal every `snap_stride` targets (+ always the deepest, for §4).
    // Ceil the stride so `DET_SUBSET` snapshots ≈ the requested count, not more (floor
    // `64/24 = 2` would snapshot ~33; ceil `= 3` snapshots ~22). `DET_SUBSET >= TARGETS`
    // yields stride 1 (snapshot every sealed target).
    let snap_stride = schedule.len().div_ceil(n_det).max(1);
    {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let mut printed = 0usize;
        let start = watchdog_start();
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
            let take_snapshot = i % snap_stride == 0 || i == last_idx;
            // `state_hash` hashes the full 2 GiB guest image — expensive. Only compute it for the
            // snapshotted subset that actually needs it (§2/§4/§4b), not every target (computing
            // it for all N was the second run's timeout). Capture the CLEAN hash BEFORE probe_seal:
            // `has_pending_guest_interrupt` re-arbitrates the LAPIC (a benign mutation that washes
            // out over a run but perturbs the instantaneous state_hash), so capturing after it would
            // make §4's live reference disagree with a clean replay.
            let live_hash_clean = if take_snapshot {
                live.state_hash()
            } else {
                [0u8; 32]
            };
            let (snapshot, reason, blob) = probe_seal(&mut live);
            let result = match reason {
                None => {
                    // §1 rate: this target sealed. For the spread subset (+ the deepest), also take
                    // the full 2 GiB snapshot for the §2 determinism check + §4/§4b. Independent
                    // `snapshot_base`s so materialize is O(1)-layer; the store dedups store-wide.
                    if take_snapshot {
                        // The probe-laden hash: `state_hash` AFTER probe_seal — §4b replays with the
                        // same probe schedule to reproduce it.
                        let live_hash_probed = live.state_hash();
                        let blob = blob.expect("a sealed point always yields an encoded vm_state");
                        let snap = engine
                            .snapshot_base(live.guest_memory(), &blob)
                            .expect("snapshot the sealed guest image");
                        sealed.push(Sealed {
                            attempt_idx: i,
                            snap,
                            seal_vtime: landed,
                            live_hash_clean,
                            live_hash_probed,
                        });
                    }
                    SealResult::Sealed
                }
                Some(reason) => SealResult::Failed(reason),
            };
            nominal.push(SealAttempt {
                target,
                landed_vtime: landed,
                snapshot,
                result,
            });
        }
        eprintln!(
            "[sweep] nominal: {} sealed / {} targets ({} full snapshots retained for §2/§4)",
            nominal.iter().filter(|a| a.result.is_sealed()).count(),
            nominal.len(),
            sealed.len()
        );
        // Drop the live guest (frees its perf counter) before any fork boots.
    }

    // --- 2. Prove each successful seal is a real branch point --------------
    eprintln!("\n[sweep] === branch-determinism check: 2 same-seed branches per sealed point ===");
    let mut nondeterministic: Vec<(VTime, String)> = Vec::new();
    // Per-pair ACTUAL verified horizon (min of the two branches' reached V-time, minus the seal)
    // + whether either branch hit terminal early. A same-hash pair verified over a truncated
    // horizon is weaker evidence — recorded, never silently counted as full-horizon.
    let mut verified_horizons: Vec<(VTime, VTime, bool)> = Vec::new();
    for (vi, s) in sealed.iter().enumerate() {
        eprintln!(
            "[sweep] branch-verify {}/{} at V-time {} …",
            vi + 1,
            sealed.len(),
            s.seal_vtime
        );
        let b1 = branch_and_run(&engine, s.snap, &kernel, &initramfs, BRANCH_SEED, horizon);
        let b2 = branch_and_run(&engine, s.snap, &kernel, &initramfs, BRANCH_SEED, horizon);
        let verified_horizon = b1
            .reached_vtime
            .min(b2.reached_vtime)
            .saturating_sub(s.seal_vtime);
        let early_terminal = b1.early_terminal || b2.early_terminal;
        verified_horizons.push((s.seal_vtime, verified_horizon, early_terminal));
        if early_terminal {
            eprintln!(
                "[sweep]   (seal at {} verified over a TRUNCATED horizon {verified_horizon} < \
                 requested {horizon} — hit terminal; same-hash still holds but is weaker evidence)",
                s.seal_vtime
            );
        }
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
    // Honest subset counts (not a global bool): how many of the branch-verified spread subset
    // were bit-identical, out of how many were checked. `rule()` requires all-checked-passed
    // AND at least one checked.
    let det_sealed_total = sealed.len();
    let det_verified = det_sealed_total - nondeterministic.len();
    let det_early_terminal = verified_horizons.iter().filter(|(_, _, e)| *e).count();
    let det_full_horizon = verified_horizons.len() - det_early_terminal;
    let det_min_horizon = verified_horizons
        .iter()
        .map(|(_, h, _)| *h)
        .min()
        .unwrap_or(0);
    eprintln!(
        "[sweep] branch-determinism: {det_verified}/{det_sealed_total} branch-verified points \
         bit-identical (a spread subset of {} sealed; the §2 DET_SUBSET deviation) — {det_full_horizon} \
         verified to the full horizon {horizon}, {det_early_terminal} truncated at terminal \
         (min verified horizon {det_min_horizon})",
        nominal.iter().filter(|a| a.result.is_sealed()).count(),
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
    // How many jittered targets the guest had **already overshot** when the loop reached them —
    // a prior boundary landing (overshoot can be millions of ns; p90 ≈ 4.76 M ≫ target spacing)
    // or the interior perturbation advanced `effective_vns` past the target. Without this guard,
    // `run_to_vtime` would return immediately at that prior point and its seal would silently
    // contaminate this target's §3 sample. We count + skip them instead; §3/§5 rates are over the
    // non-overshot samples only.
    let mut skipped_overshot = 0usize;
    // Why the loop stopped early (if it did): a legitimate terminal near the tail vs a mid-span
    // step error. `unprobed` (targets neither probed nor skipped) is gated on this below — a rate
    // over a silently-shrunken denominator must not satisfy a threshold the full population wouldn't.
    let mut adv_terminal_break = false;
    let mut adv_step_error: Option<String> = None;
    {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let mut printed = 0usize;
        let start = watchdog_start();
        for &target in adv_schedule.targets() {
            // Overshoot guard (the loop head): the guest only runs forward, so once it is at/past
            // a target there is no way to re-address it without contaminating the sample.
            if live.effective_vns().unwrap_or(0) >= target.vtime {
                skipped_overshot += 1;
                continue;
            }
            let adv = run_to_vtime(&mut live, target.vtime, &mut printed, start);
            if let Some(e) = adv.step_error {
                adv_step_error = Some(e);
                break;
            }
            if adv.terminal.is_some() {
                // Near the tail a jittered target may sit at/after terminal — stop probing.
                adv_terminal_break = true;
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
            if let Some(e) = padv.step_error {
                adv_step_error = Some(e);
                break;
            }
            if padv.terminal.is_some() {
                adv_terminal_break = true;
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
    // Honest denominators: every jittered target is probed, skipped-overshot, or unprobed (dropped
    // when the loop broke). A rate over `adversarial.len()` alone hides the unprobed tail.
    let adv_unprobed = adv_schedule
        .len()
        .saturating_sub(adversarial.len())
        .saturating_sub(skipped_overshot);
    eprintln!(
        "[sweep] adversarial (boundary): {} sealed / {} probed | interior: {} sealed / {} probed \
         | {skipped_overshot} skipped-overshot | {adv_unprobed} unprobed (of {}) | break={}",
        adversarial.iter().filter(|a| a.result.is_sealed()).count(),
        adversarial.len(),
        interior.iter().filter(|a| a.result.is_sealed()).count(),
        interior.len(),
        adv_schedule.len(),
        if adv_step_error.is_some() {
            "step-error"
        } else if adv_terminal_break {
            "terminal"
        } else {
            "none"
        },
    );

    // --- 4. Materialization depth (parent-rooted premise) ------------------
    let depth = materialization_depth(&engine, &sealed, &kernel, &initramfs);
    // --- 4b. Schedule-faithful replay (the foreman's distinguishing experiment) --------
    let schedule_faithful =
        materialization_schedule_faithful(&engine, &sealed, &schedule, &kernel, &initramfs);

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
        det_verified,
        det_sealed_total,
        adversarial_scheduled: adv_schedule.len(),
        overshoot,
    };
    let ruling = vmm_core::seal_rate::rule(&inputs, RulingThresholds::default());
    eprintln!(
        "[sweep] determinism evidence: {}",
        inputs.determinism_summary()
    );

    emit_report(
        &schedule,
        &nominal_stats,
        &adversarial_stats,
        &interior_stats,
        skipped_overshot,
        adv_unprobed,
        overshoot,
        &predicate,
        depth,
        schedule_faithful,
        &nondeterministic,
        &inputs.determinism_summary(),
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
    // §3 honest-denominator gate: a mid-span step error (not a legitimate terminal near the tail)
    // truncates the adversarial population — never accept a rate over a silently-shrunken
    // denominator. A terminal break is legitimate only if the unprobed tail is small.
    assert!(
        adv_step_error.is_none(),
        "§3 adversarial pass hit a mid-span step error (span truncated, NOT a legitimate tail): \
         {adv_step_error:?}"
    );
    let tail_allowance = env_usize("UNPROBED_TAIL_ALLOWANCE", 4);
    assert!(
        adv_unprobed <= tail_allowance,
        "§3 dropped {adv_unprobed} unprobed jittered target(s) (> tail allowance {tail_allowance}) \
         — the adversarial denominator is silently shrunk beyond the legitimate past-terminal tail \
         (terminal_break={adv_terminal_break})"
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
    // The Phase-C premise: materializing the child by branch(parent) → run the suffix must
    // reproduce the SAME state as materializing it from genesis (replay the whole prefix). This
    // is the load-bearing assertion — a materialized virtual exemplar must be independent of
    // which retained ancestor it was rooted at.
    let from_parent_hash = replay_to(engine, parent.snap, kernel, initramfs, child.seal_vtime);
    let from_genesis_hash = boot_and_run_to(kernel, initramfs, child.seal_vtime);

    let premise_ok = from_parent_hash == from_genesis_hash;
    // `live_hash_clean` is captured at the seal instant BEFORE probe_seal; it agrees with the
    // clean replays (informational — not the assertion; §4b tests the probe-laden trajectory).
    let live_agrees = from_genesis_hash == child.live_hash_clean;
    eprintln!(
        "[sweep] §4 parent-rooted (suffix) = {}\n[sweep] §4 genesis-rooted (prefix) = {}\n[sweep] \
         §4 parent==genesis (the premise): {premise_ok}   (live_hash agrees: {live_agrees})",
        hex(&from_parent_hash),
        hex(&from_genesis_hash),
    );
    assert!(
        premise_ok,
        "§4 parent-rooted materialization ({}) did NOT match genesis-rooted ({}) — the \
         parent-rooted lazy-materialization premise fails; materialization is not \
         ancestor-independent (escalate — a determinism-core bug)",
        hex(&from_parent_hash),
        hex(&from_genesis_hash),
    );
    MaterializationDepth::new(0, parent.seal_vtime, child.seal_vtime).ok()
}

/// §4b (the foreman's distinguishing experiment): the clean replays agree with each other
/// (§4) but both diverge from the probe-laden live run. Is that divergence a *deterministic*
/// function of the deadline/probe schedule (reproducible), or non-reproducible perturbation?
///
/// Restore the parent snapshot, then re-run the EXACT sequence of live legs between parent and
/// child — `run_to_vtime(target_j)` + `probe_seal` at each intermediate target, exactly as the
/// nominal pass did (the same `run(deadline)` arm-points + `save_vm_state`/`has_*` probes) — and
/// compare the final probe-laden `state_hash` to the child's `live_hash_probed`.
///
/// - **Match** ⇒ the deadline/probe schedule is a deterministic part of the trajectory; the live
///   run's divergence from a clean replay is fully reproduced by replaying the schedule → the
///   substrate is sound (materialize by replaying the exact schedule, or probe-free).
/// - **Mismatch** ⇒ the probes inject non-reproducible perturbation → escalate.
///
/// Reports the verdict; does **not** assert (the GO/NO-GO ruling is the integrator's). `None`
/// if there is no distinct-depth snapshotted pair to test.
fn materialization_schedule_faithful(
    engine: &SnapshotEngine,
    sealed: &[Sealed],
    schedule: &SamplingSchedule,
    kernel: &[u8],
    initramfs: &[u8],
) -> Option<bool> {
    if sealed.len() < 2 {
        eprintln!("[sweep] §4b schedule-faithful replay: <2 seals, skipped");
        return None;
    }
    let child = sealed.iter().max_by_key(|s| s.seal_vtime).unwrap();
    let parent = sealed
        .iter()
        .filter(|s| s.seal_vtime < child.seal_vtime)
        .max_by_key(|s| s.seal_vtime)?;
    let (p_idx, c_idx) = (parent.attempt_idx, child.attempt_idx);
    if p_idx >= c_idx {
        eprintln!("[sweep] §4b: parent index >= child index, skipped");
        return None;
    }
    eprintln!(
        "\n[sweep] === §4b schedule-faithful replay: restore parent (idx {p_idx}, V-time {}) → \
         re-run legs {}..={c_idx} with the live probe schedule → child (V-time {}) ===",
        parent.seal_vtime,
        p_idx + 1,
        child.seal_vtime
    );
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let mapping = engine.materialize(parent.snap).expect("materialize parent");
    let vm_state = engine
        .vm_state(parent.snap)
        .expect("decode parent vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore parent snapshot");
    let start = watchdog_start();
    let mut printed = vmm.serial().len();
    // Replay each intermediate live leg exactly: run to the target, then probe_seal.
    for j in (p_idx + 1)..=c_idx {
        let target = schedule.targets()[j].vtime;
        let _ = run_to_vtime(&mut vmm, target, &mut printed, start);
        let _ = probe_seal(&mut vmm);
    }
    let replayed = vmm.state_hash();
    let matches = replayed == child.live_hash_probed;
    eprintln!(
        "[sweep] §4b child live_hash_probed  = {}\n[sweep] §4b schedule-faithful replay = {}\n\
         [sweep] §4b MATCH (the probe/deadline schedule is deterministic): {matches}",
        hex(&child.live_hash_probed),
        hex(&replayed),
    );
    Some(matches)
}

/// Print the full measurement in the shape `SEAL-RATE-REPORT.md` transcribes.
#[allow(clippy::too_many_arguments)]
fn emit_report(
    schedule: &SamplingSchedule,
    nominal: &SealStats,
    adversarial: &SealStats,
    interior: &SealStats,
    skipped_overshot: usize,
    unprobed: usize,
    overshoot: Option<Overshoot>,
    predicate: &PredicateQuality,
    depth: Option<MaterializationDepth>,
    schedule_faithful: Option<bool>,
    nondeterministic: &[(VTime, String)],
    det_summary: &str,
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
        "[REPORT] ADVERSARIAL seal rate: {}/{} = {} (denominators: {} probed / {skipped_overshot} \
         skipped-overshot / {unprobed} unprobed — no silent contamination or shrunk denominator)",
        adversarial.sealed,
        adversarial.n,
        ppm_percent(adversarial.success_rate_ppm),
        adversarial.n,
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
        "[REPORT] branch-determinism: {det_summary} bit-identical; {} nondeterministic (must be 0)",
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
            "[REPORT] §4 materialization depth (parent-rooted==genesis-rooted, verified): \
             from_parent={} from_genesis={} ratio={} savings={}",
            d.from_parent,
            d.from_genesis,
            ppm_percent(d.ratio_ppm()),
            ppm_percent(d.savings_ppm())
        );
    }
    match schedule_faithful {
        Some(true) => eprintln!(
            "[REPORT] §4b schedule-faithful replay: MATCH — the probe/deadline schedule is a \
             DETERMINISTIC part of the trajectory; the live run reproduces exactly when replayed \
             with the same schedule (substrate sound; materialize probe-free or replay the schedule)"
        ),
        Some(false) => eprintln!(
            "[REPORT] §4b schedule-faithful replay: MISMATCH — replaying the same probe schedule \
             did NOT reproduce the live trajectory (non-reproducible perturbation → ESCALATE)"
        ),
        None => {
            eprintln!("[REPORT] §4b schedule-faithful replay: skipped (no distinct-depth pair)")
        }
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
    // A mechanical, threshold-derived summary of the DATA — NOT the go/no-go ruling, which the
    // foreman has escalated to the integrator (task 63 gate 3). Presented for reference only.
    eprintln!(
        "[REPORT] MECHANICAL SUMMARY (thresholds; the final ruling is the integrator's): {}",
        ruling.label()
    );
    eprintln!("[REPORT] ============================================================");
}
