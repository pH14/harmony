// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **non-quiescent snapshot** gates (task 41 — `#[cfg(target_os = "linux")]`
//! **and `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules,
//! CPU-pinned per `docs/BOX-PINNING.md`, reverted to stock after). The substrate
//! unlock: make a **running, interrupt-driven** guest (Postgres + the LAPIC timer)
//! snapshottable **mid-execution**, where task 40 measured **0 of 8392** snapshottable
//! because task 39's codec dropped the in-flight CPU event/interrupt state.
//!
//! Task 41 captures the full `kvm_vcpu_events` (the in-flight injection KVM has not yet
//! delivered) in the snapshot blob and re-establishes it on restore, so any V-time
//! point — including one with an interrupt **in flight** — round-trips bit-identically.
//!
//! **Gate 1 — a non-quiescent point is snapshottable** ([`gate1_nonquiescent_point_is_snapshottable`]).
//! Scan the post-readiness Postgres workload and tally, **on the same run**: the
//! V-time-synchronized boundaries with an interrupt **in flight** (which task 39
//! fail-closed-rejected — the *before* count) versus those that are now snapshottable
//! (the *after* count). The before/after split (`0 → N`) is quoted. Then a snapshot is
//! taken at one such non-quiescent point, restored into a fresh VM, and resumed to a
//! clean terminal — proving `save_vm_state` succeeds **and** `restore_vm_state` resumes.
//!
//! **Gate 2 — round-trip mid-execution, the milestone**
//! ([`gate2_mid_postgres_roundtrip_is_deterministic`]). Snapshot a **running Postgres
//! mid-workload** (at a non-quiescent, in-flight point), restore into a fresh VM, and
//! resume → the resumed run reaches the **same terminal `state_hash`** as the
//! un-snapshotted reference run. Deterministic-twice (the reference is verified
//! bit-identical across two boots first). Restore is exact at a non-quiescent point.
//!
//! ## Gate honesty (why `#[ignore]`)
//!
//! Needs real + patched KVM (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), `perf_event`, the
//! built Postgres image (`guest/build/bzImage` + `guest/build/initramfs-postgres.cpio.gz`,
//! via `guest/linux/build-postgres-image.sh`), and the `det-cfl-v1` host — none in the
//! default `cargo nextest` lane — so it is `#[ignore]`d (like `live_postgres.rs` /
//! `live_branching_demo.rs`); default CI shows it not-run, never a vacuous green. Every
//! missing precondition is a **loud panic**, never an early-return `Ok`. macOS builds an
//! empty test binary; the non-quiescent capture *logic* is covered portably by the
//! `src/snapshot.rs` unit tests (full-events device-blob round-trip), the `src/vmm.rs`
//! unit tests (in-flight save/restore/re-save + LAPIC-seam re-derivation), and
//! `tests/snapshot_branch.rs` (full-engine non-quiescent round-trip).
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux postgres-image       # build the image
//! # load patched kvm.ko/kvm-intel.ko, then (core 4 per the box briefing):
//! taskset -c 4 timeout 3600 cargo test -p vmm-core --test live_nonquiescent_snapshot \
//!     -- --ignored --nocapture --test-threads=1
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736
//! # (coordinate first: check lsmod, do NOT revert while another patched run is live).
//! ```
//!
//! Knobs (env): `WORKLOAD_MARKER` (the serial substring after which the scan/seal looks
//! for a mid-workload non-quiescent point, default the postmaster-ready banner),
//! `BOOT_CMDLINE` (overrides the kernel command line, as in `live_postgres.rs`).
#![cfg(target_os = "linux")]

use std::io::Write;
use std::time::{Duration, Instant};

use snapshot_store::SnapshotId;
use vmm_backend::Backend;
use vmm_core::bringup::{BackendKind, boot_linux_selected};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 2 GiB of guest RAM — identical to `live_postgres.rs` / `live_branching_demo.rs`.
const GUEST_RAM_LEN: usize = 2 << 30;
/// The base seed — same value `live_postgres.rs` pins (so the reference run is the
/// canonical deterministic Postgres boot).
const BASE_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line (identical to `live_postgres.rs` / `live_branching_demo.rs`).
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Step budget per run — a high cap so a stuck guest cannot run forever (real bound is
/// the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 50_000_000_000;
/// Per-run wall-clock budget.
const WALL_BUDGET: Duration = Duration::from_secs(1200);

/// postgres announces this once the cluster is accepting connections.
const PG_READY: &[u8] = b"database system is ready to accept connections";
/// The final workload row (iteration 20): a pure function of the loop index, so it
/// proves the *query results* reached the serial in every continuation.
const FINAL_ROW: &[u8] = b"row|20|407|20|3010";
/// `pg-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";
/// How many extra steps to scan past the first non-quiescent seal point when tallying
/// the before/after counts (bounded so the gate-1 scan stays quick once it has its
/// evidence).
const SCAN_WINDOW: u64 = 50_000;

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact (`guest/build/<name>` then `guest/linux/<name>`).
/// Panics loudly (with the build command) if absent.
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
        "/dev/kvm absent — run this `#[ignore]`d box gate on `ssh <det-box>` with the LOADED \
         patched KVM modules, CPU-pinned per docs/BOX-PINNING.md (taskset -c 4)."
    );
}

/// Require the §1.1 `det-cfl-v1` host baseline, else **panic** with the report.
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
        "host CPU is not the det-cfl-v1 baseline — the frozen contract cannot run here. Run on the \
         determinism box (i9-9900K) per docs/BOX-PINNING.md."
    );
}

fn cmdline() -> String {
    std::env::var("BOOT_CMDLINE").unwrap_or_else(|_| DEFAULT_CMDLINE.to_string())
}

/// The serial substring after which the scan/seal looks for a mid-workload
/// non-quiescent point (default: the postmaster-ready banner — well into the run, where
/// task 40 measured 0/8392 snapshottable).
fn workload_marker() -> Vec<u8> {
    std::env::var("WORKLOAD_MARKER")
        .map(String::into_bytes)
        .unwrap_or_else(|_| PG_READY.to_vec())
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

type DynVmm = Vmm<Box<dyn Backend>>;

/// Boot the Postgres image on the patched backend at `seed`. Panics loudly if the box
/// is not ready (no early-return that nextest would count as a vacuous pass).
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

/// What a bounded run observed.
struct RunOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    final_row: bool,
    guest_ready: bool,
    step_error: Option<String>,
}

impl RunOutcome {
    /// Ran the workload's final row and reached `GUEST_READY` through a clean terminal
    /// with no contract violation.
    fn internally_consistent(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none() && self.final_row && self.guest_ready
    }
}

/// Drive `vmm` to a terminal state (or the step / wall budget), streaming new serial
/// bytes to stderr. Returns the outcome.
fn drive_to_terminal(vmm: &mut DynVmm) -> RunOutcome {
    #[allow(clippy::disallowed_methods)] // test-only wall-clock watchdog; never reaches state/hash
    let start = Instant::now();
    let mut printed = vmm.serial().len();
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    let stderr = std::io::stderr();
    while steps < MAX_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                reason = Some(r);
                break;
            }
            Err(e) => {
                step_error = Some(format!("{e}"));
                eprintln!("\n[nq] step error after {steps} steps: {e}");
                break;
            }
        }
        steps += 1;
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
        }
        if steps.is_multiple_of(8192) && start.elapsed() > WALL_BUDGET {
            eprintln!("\n[nq] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let serial = vmm.serial();
    RunOutcome {
        reason,
        steps,
        final_row: find(serial, FINAL_ROW),
        guest_ready: find(serial, GUEST_READY),
        step_error,
    }
}

/// The reference: a fresh patched Postgres VM run straight to a clean terminal. Returns
/// its terminal `state_hash` and the run outcome (asserted internally consistent).
fn run_reference(kernel: &[u8], initramfs: &[u8], label: &str) -> ([u8; 32], RunOutcome) {
    let mut vmm = boot_pg(kernel, initramfs, BASE_SEED);
    let outcome = drive_to_terminal(&mut vmm);
    eprintln!(
        "[nq] reference {label}: terminal={:?} steps={} final_row={} GUEST_READY={}",
        outcome.reason, outcome.steps, outcome.final_row, outcome.guest_ready
    );
    assert!(
        outcome.internally_consistent(),
        "reference {label} must run the workload to its final row and reach GUEST_READY cleanly \
         (final_row={}, guest_ready={}, terminal={:?}, err={:?})",
        outcome.final_row,
        outcome.guest_ready,
        outcome.reason,
        outcome.step_error,
    );
    (vmm.state_hash(), outcome)
}

/// Where a mid-workload non-quiescent snapshot was sealed.
struct Sealed {
    vm_state: vm_state::VmState,
    step: u64,
}

/// Drive `live` from boot until the **first V-time-synchronized, non-quiescent
/// (interrupt-in-flight) boundary at/after `marker`** appears, and seal it. When
/// `post_seal_scan > 0`, keep scanning that many more steps **after** the seal to tally
/// the before/after split **on the same run** (gate 1's evidence: how many synchronized
/// boundaries carry an in-flight injection task 39 rejected vs. are now snapshottable);
/// pass `0` to seal-and-return immediately (gates 2/3, which only need the seal point).
/// Panics loudly if the guest terminates before any non-quiescent snapshottable point.
fn seal_first_nonquiescent(live: &mut DynVmm, marker: &[u8], post_seal_scan: u64) -> Sealed {
    let stderr = std::io::stderr();
    let mut printed = live.serial().len();
    let mut steps = 0u64;
    let mut armed = marker.is_empty();
    // Tally over the armed (at/after-marker) region.
    let mut inflight_snapshottable = 0u64; // was-rejected-before, now-OK (the 0→N flip)
    let mut quiescent_snapshottable = 0u64; // OK and quiescent
    let mut rejections: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut sealed: Option<Sealed> = None;
    let mut scanned_after_seal = 0u64;

    loop {
        if !armed {
            armed = find(live.serial(), marker);
        }
        if armed {
            let inflight = live.has_inflight_event_injection();
            match live.save_vm_state() {
                Ok(vm_state) => {
                    if inflight {
                        inflight_snapshottable += 1;
                        if sealed.is_none() {
                            eprintln!(
                                "[nq] sealed a NON-QUIESCENT snapshot S at step {steps} \
                                 (interrupt in flight — task 39 would have rejected this point)"
                            );
                            sealed = Some(Sealed {
                                vm_state,
                                step: steps,
                            });
                        }
                    } else {
                        quiescent_snapshottable += 1;
                    }
                }
                Err(e) => {
                    // Classify the rejection (non-synchronized / RNG mid-exit / other).
                    let msg = format!("{e}");
                    let key = msg.split(':').nth(1).unwrap_or(&msg).trim().to_string();
                    *rejections.entry(key).or_insert(0) += 1;
                }
            }
            if sealed.is_some() {
                if scanned_after_seal >= post_seal_scan {
                    break;
                }
                scanned_after_seal += 1;
            }
        }
        match live.step() {
            Ok(Step::Continued) => {
                let serial = live.serial();
                if serial.len() > printed {
                    let mut h = stderr.lock();
                    let _ = h.write_all(&serial[printed..]);
                    let _ = h.flush();
                    printed = serial.len();
                }
                steps += 1;
            }
            Ok(Step::Terminal(r)) => {
                if sealed.is_some() {
                    break;
                }
                panic!(
                    "the guest reached a terminal ({r:?}) at step {steps} before any \
                     non-quiescent snapshottable point at/after the marker (inflight_snapshottable=\
                     {inflight_snapshottable}, quiescent_snapshottable={quiescent_snapshottable}, \
                     rejection tally: {rejections:#?})"
                );
            }
            Err(e) => panic!("step while hunting a non-quiescent snapshot boundary failed: {e}"),
        }
    }

    eprintln!(
        "\n[nq] ============ BEFORE / AFTER (same Postgres run, at/after marker) ============\n\
         [nq]  BEFORE (task 39, quiescent-only): in-flight points were REJECTED — \
         {inflight_snapshottable} of the synchronized boundaries scanned carried an interrupt in \
         flight, all 0 snapshottable.\n\
         [nq]  AFTER  (task 41): those same {inflight_snapshottable} in-flight points are now \
         SNAPSHOTTABLE (+ {quiescent_snapshottable} quiescent). still-rejected (not a quiescence \
         issue): {rejections:#?}"
    );
    let sealed =
        sealed.expect("a non-quiescent snapshottable point was found (loop only exits with one)");
    assert!(
        inflight_snapshottable > 0,
        "gate 1: at least one synchronized boundary in the workload must carry an interrupt in \
         flight and now be snapshottable (the 0→N flip task 40 documented as missing)"
    );
    sealed
}

/// One fork's terminal observation: the full-state digest, the per-component digest
/// breakdown (for divergence localization), and the run outcome.
struct ForkResult {
    hash: [u8; 32],
    components: Vec<(&'static str, [u8; 32])>,
    outcome: RunOutcome,
}

/// Restore base snapshot `snap` into a **fresh** patched VM, optionally reseed the
/// entropy stream (`Some(seed)` = a branch; `None` = the base continuation replayed
/// verbatim), resume to terminal, and return its [`ForkResult`]. The prior live VM
/// must already be dropped (single open work counter at a time).
fn run_fork(
    engine: &SnapshotEngine,
    snap: SnapshotId,
    kernel: &[u8],
    initramfs: &[u8],
    reseed: Option<u64>,
) -> ForkResult {
    let mut vmm = boot_pg(kernel, initramfs, reseed.unwrap_or(BASE_SEED));
    let mapping = engine.materialize(snap).expect("materialize the base");
    let vm_state = engine.vm_state(snap).expect("decode the sealed vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore the mid-workload snapshot into the fresh VM");
    if let Some(seed) = reseed {
        vmm.reseed_entropy(seed)
            .expect("reseed the entropy stream for the branch");
    }
    let outcome = drive_to_terminal(&mut vmm);
    ForkResult {
        hash: vmm.state_hash(),
        components: vmm.state_components(),
        outcome,
    }
}

/// Restore + resume verbatim (the gate-1/2 path): just the terminal hash + outcome.
fn restore_and_resume(
    engine: &SnapshotEngine,
    snap: SnapshotId,
    kernel: &[u8],
    initramfs: &[u8],
) -> ([u8; 32], RunOutcome) {
    let fork = run_fork(engine, snap, kernel, initramfs, None);
    (fork.hash, fork.outcome)
}

/// A distinct, non-base entropy seed for branch `k` (same scheme as `live_branching_demo.rs`).
fn branch_seed(k: usize) -> u64 {
    BASE_SEED ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[test]
#[ignore = "box-only non-quiescent snapshot gate (LOADED patched KVM + built Postgres image + \
            det-cfl-v1 host); run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn gate1_nonquiescent_point_is_snapshottable() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");
    let marker = workload_marker();
    eprintln!(
        "[nq] gate 1: cmdline: {}\n[nq] WORKLOAD_MARKER={:?}",
        cmdline(),
        String::from_utf8_lossy(&marker)
    );

    // Boot the live guest, scan the post-readiness workload, tally the before/after
    // split, and seal the first non-quiescent (interrupt-in-flight) point.
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        // Gate 1 wants the before/after tally → scan a window past the seal.
        let sealed = seal_first_nonquiescent(&mut live, &marker, SCAN_WINDOW);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(live.guest_memory(), &blob)
            .expect("snapshot the live guest image + vm_state at the non-quiescent point");
        eprintln!(
            "[nq] gate 1: sealed non-quiescent S at step {} ({} guest pages, {} owned, vm_state {} \
             bytes).",
            sealed.step,
            engine.mem_pages(),
            engine.stats(snap).expect("base stats").owned_pages,
            blob.len(),
        );
        snap // drop the live VM (and its perf counter) here
    };

    // save_vm_state succeeded at a non-quiescent point (above); now prove
    // restore_vm_state resumes it to a clean, internally-consistent terminal.
    let (_hash, outcome) = restore_and_resume(&engine, snap, &kernel, &initramfs);
    eprintln!(
        "[nq] gate 1: restored continuation: terminal={:?} steps={} final_row={} GUEST_READY={}",
        outcome.reason, outcome.steps, outcome.final_row, outcome.guest_ready
    );
    assert!(
        outcome.internally_consistent(),
        "gate 1: the restored continuation from a non-quiescent point must run the workload to its \
         final row and reach GUEST_READY cleanly (final_row={}, guest_ready={}, terminal={:?}, \
         err={:?})",
        outcome.final_row,
        outcome.guest_ready,
        outcome.reason,
        outcome.step_error,
    );
    eprintln!(
        "[nq] gate 1 ✓ a non-quiescent point (interrupt in flight) is snapshottable AND restores — \
         the 0→N flip task 40 measured as missing."
    );
}

#[test]
#[ignore = "box-only non-quiescent snapshot gate (LOADED patched KVM + built Postgres image + \
            det-cfl-v1 host); run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn gate2_mid_postgres_roundtrip_is_deterministic() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");
    let marker = workload_marker();
    eprintln!("[nq] gate 2 (milestone): cmdline: {}", cmdline());

    // --- Deterministic-twice: the un-snapshotted reference is bit-identical twice. ---
    let (ref_hash_1, _) = run_reference(&kernel, &initramfs, "run 1");
    let (ref_hash_2, _) = run_reference(&kernel, &initramfs, "run 2");
    assert_eq!(
        ref_hash_1,
        ref_hash_2,
        "the un-snapshotted Postgres run must be deterministic-twice (terminal state_hash). \
         run1={} run2={}",
        hex(&ref_hash_1),
        hex(&ref_hash_2)
    );
    eprintln!(
        "[nq] reference deterministic-twice ✓ terminal state_hash = {}",
        hex(&ref_hash_1)
    );

    // --- Snapshot a RUNNING Postgres mid-workload at a non-quiescent point. ---
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let sealed = seal_first_nonquiescent(&mut live, &marker, 0);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(live.guest_memory(), &blob)
            .expect("snapshot the running Postgres mid-workload");
        eprintln!(
            "[nq] gate 2: sealed mid-workload S at step {}.",
            sealed.step
        );
        snap // drop the live VM before the restore VM boots
    };

    // --- Restore into a FRESH VM and resume → same terminal state_hash as the
    //     un-snapshotted reference. Restore is exact at a non-quiescent point. ---
    let (restored_hash, outcome) = restore_and_resume(&engine, snap, &kernel, &initramfs);
    eprintln!(
        "[nq] gate 2: restored continuation: terminal={:?} steps={} final_row={} GUEST_READY={}",
        outcome.reason, outcome.steps, outcome.final_row, outcome.guest_ready
    );
    assert!(
        outcome.internally_consistent(),
        "gate 2: the restored continuation must run the workload to its final row + GUEST_READY \
         cleanly (final_row={}, guest_ready={}, terminal={:?}, err={:?})",
        outcome.final_row,
        outcome.guest_ready,
        outcome.reason,
        outcome.step_error,
    );
    eprintln!(
        "[nq] gate 2: reference  terminal state_hash = {}\n[nq] gate 2: restored   terminal \
         state_hash = {}",
        hex(&ref_hash_1),
        hex(&restored_hash)
    );
    assert_eq!(
        restored_hash, ref_hash_1,
        "gate 2 (the milestone): a Postgres run snapshotted mid-workload at a non-quiescent point, \
         restored into a fresh VM, and resumed must reach the SAME terminal state_hash as the \
         un-snapshotted reference — restore is exact at a non-quiescent point. Same state ⇒ same \
         future."
    );
    eprintln!(
        "[nq] gate 2 ✓ mid-Postgres snapshot → restore → resume is bit-identical to the \
         un-snapshotted run. The dissonance unlock: fork a system while it is doing work."
    );
}

#[test]
#[ignore = "box-only non-quiescent branching gate (LOADED patched KVM + built Postgres image + \
            det-cfl-v1 host); run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn gate3_branching_from_a_mid_postgres_snapshot() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");
    // Force the seal to be MID-POSTGRES (not boot-entry): this is exactly the matrix
    // task 40 documented as missing — it could only seal at boot entry because the
    // workload's V-time-sync boundaries all held an interrupt in flight (0/8392). Task
    // 41 makes the mid-Postgres seal possible.
    let marker = PG_READY.to_vec();
    let k = env_usize("BRANCHES", 3);
    let n = env_usize("REPLAYS", 2);
    assert!(k >= 1 && n >= 1, "need BRANCHES>=1 and REPLAYS>=1");
    eprintln!(
        "[nq] gate 3: re-run task 40's matrix sealed MID-POSTGRES. BRANCHES(K)={k} REPLAYS(N)={n}"
    );

    // --- Seal S at a mid-Postgres non-quiescent point. ---
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let sealed = seal_first_nonquiescent(&mut live, &marker, 0);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(live.guest_memory(), &blob)
            .expect("snapshot the running Postgres mid-workload");
        eprintln!(
            "[nq] gate 3: sealed mid-Postgres S at step {} ({} owned pages).",
            sealed.step,
            engine.stats(snap).expect("stats").owned_pages
        );
        snap
    };
    let base_unique_pages = engine.store_stats().stored_unique_pages;

    // --- Base continuation (verbatim replay), N times → reproducibility. ---
    let mut base_digest: Option<[u8; 32]> = None;
    let mut base_components = None;
    for r in 0..n {
        let fork = run_fork(&engine, snap, &kernel, &initramfs, None);
        assert!(
            fork.outcome.internally_consistent(),
            "base replay {r} must be internally consistent (final_row={}, guest_ready={}, \
             terminal={:?}, err={:?})",
            fork.outcome.final_row,
            fork.outcome.guest_ready,
            fork.outcome.reason,
            fork.outcome.step_error,
        );
        match base_digest {
            None => {
                base_digest = Some(fork.hash);
                base_components = Some(fork.components);
            }
            Some(first) => assert_eq!(
                fork.hash,
                first,
                "base replay {r} diverged from replay 0 — NOT reproducible ({} vs {})",
                hex(&fork.hash),
                hex(&first)
            ),
        }
    }
    let base_digest = base_digest.expect("n>=1");
    let base_components = base_components.expect("n>=1");
    eprintln!(
        "[nq] gate 3: base continuation reproducible across {n} replays ✓ digest={}",
        hex(&base_digest)
    );

    // --- K entropy-fork branches, each replayed N times → reproducible per fork. ---
    let mut any_diverged = false;
    for b in 0..k {
        let seed = branch_seed(b);
        let mut digest: Option<[u8; 32]> = None;
        let mut comps = None;
        for r in 0..n {
            let fork = run_fork(&engine, snap, &kernel, &initramfs, Some(seed));
            assert!(
                fork.outcome.internally_consistent(),
                "branch {b} replay {r} must be internally consistent (final_row={}, \
                 guest_ready={}, terminal={:?}, err={:?})",
                fork.outcome.final_row,
                fork.outcome.guest_ready,
                fork.outcome.reason,
                fork.outcome.step_error,
            );
            match digest {
                None => {
                    digest = Some(fork.hash);
                    comps = Some(fork.components);
                }
                Some(first) => assert_eq!(
                    fork.hash,
                    first,
                    "branch {b} replay {r} diverged — NOT reproducible ({} vs {})",
                    hex(&fork.hash),
                    hex(&first)
                ),
            }
        }
        let digest = digest.expect("n>=1");
        let comps = comps.expect("n>=1");
        let diff: Vec<&str> = base_components
            .iter()
            .zip(comps.iter())
            .filter(|((_, a), (_, c))| a != c)
            .map(|((label, _), _)| *label)
            .collect();
        if digest != base_digest {
            any_diverged = true;
        }
        eprintln!(
            "[nq] gate 3: branch {b} (seed={seed:#018x}) reproducible ✓ digest={} diverged={} \
             differing components={diff:?}",
            hex(&digest),
            digest != base_digest,
        );
    }

    // --- Gate-1 (reproducible) + gate-2 (≥1 divergent) of the matrix, plus sharing. ---
    assert!(
        any_diverged,
        "gate 3: at least one branch's terminal state_hash must differ from the base continuation \
         (each (S, seed') fork must reach a distinguishable future)"
    );
    assert_eq!(
        engine.store_stats().stored_unique_pages,
        base_unique_pages,
        "gate 3: the forks must share one read-only base (materializing K views adds no unique \
         pages)"
    );
    eprintln!(
        "[nq] gate 3 ✓ task 40's matrix, sealed MID-POSTGRES: every fork reproducible across {n} \
         replays, ≥1 divergent, one shared base. The capability task 40 documented as missing."
    );
}
