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
//! resume **twice** → deterministic-twice, and each restored continuation is
//! **bit-identical** to the **un-snapshotted (live) continuation from the same seal** —
//! its **full `state_hash`** plus serial + `observable_digest`. (The two
//! architecturally-don't-care fields KVM perturbs across a snapshot/restore — the
//! `type` of *unusable* data segments and the inert `kvm_vcpu_events` modifier residuals
//! — are canonicalized in `encode_segment`/`encode_events`, so the FULL hash matches; see
//! IMPLEMENTATION.md.) Both run the remaining workload to `GUEST_READY` + the guest's
//! clean power-off `Hlt`. Restore is exact at a non-quiescent point — *same state ⇒ same
//! future, while the system is doing work.*
//!
//! ## Gate honesty (why `#[ignore]`)
//!
//! Needs real + patched KVM (`KVM_CAP_X86_DETERMINISTIC_INTERCEPTS`), `perf_event`, the
//! built Postgres image (`harmony-linux/build/bzImage` + `harmony-linux/build/initramfs-postgres.cpio.gz`,
//! via `harmony-linux/linux/build-postgres-image.sh`), and the `det-cfl-v1` host — none in the
//! default `cargo nextest` lane — so it is `#[ignore]`d (like `live_postgres.rs` /
//! `live_branching_demo.rs`); default CI shows it not-run, never a vacuous green. Every
//! missing precondition is a **loud panic**, never an early-return `Ok`. macOS builds an
//! empty test binary; the non-quiescent capture *logic* is covered portably by the
//! `src/snapshot.rs` unit tests (full-events device-blob round-trip), the `src/vmm.rs`
//! unit tests (in-flight save/restore/re-save + LAPIC-seam re-derivation), and
//! `tests/snapshot_branch.rs` (full-engine non-quiescent round-trip).
//!
//! ```sh
//! make -C harmony-linux fetch && make -C harmony-linux/linux postgres-image       # build the image
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
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::time::{Duration, Instant};

use snapshot_store::SnapshotId;
use vmm_backend::{Backend, X86};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
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

/// Read a built guest artifact (`harmony-linux/build/<name>` then `harmony-linux/linux/<name>`).
/// Panics loudly (with the build command) if absent.
fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!(
        "guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux — build it first on the \
         box: `make -C harmony-linux fetch && make -C harmony-linux/linux postgres-image`."
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
    let report = vmm_core::vendor::x86::hostassert::report();
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

type DynVmm = Vmm<Box<dyn Backend<A = X86>>>;

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

/// What a bounded run observed. A continuation from a mid-workload seal runs the remaining
/// workload to its final row + `GUEST_READY` and the guest's clean power-off `Hlt`
/// (box-confirmed). The gates assert restore *fidelity* (the resumed run is bit-identical
/// to the un-snapshotted continuation) **and** that each continuation is internally
/// consistent — so the milestone cannot pass by comparing a shared *failed* prefix.
struct RunOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    final_row: bool,
    guest_ready: bool,
    step_error: Option<String>,
}

impl RunOutcome {
    /// A continuation **cleanly completed the workload**: no step error, it reached a real
    /// terminal (`reason.is_some()` — `drive_to_terminal` leaves `reason == None` on a
    /// step-error or wall-budget break, which `step_error`/this `is_some` reject), and the
    /// guest emitted both the workload's **final row** and **`GUEST_READY`** (the
    /// clean-shutdown marker). This is what makes the milestone a *proof* of a clean
    /// continuation rather than a match over a shared failed prefix.
    fn internally_consistent(&self) -> bool {
        self.step_error.is_none() && self.reason.is_some() && self.final_row && self.guest_ready
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
            // A cooperating-SDK stop (task 73) is a terminal here — mirror
            // vmm.rs's own run loop, which maps it to `TerminalReason::SdkStop`.
            Ok(Step::SdkStop) => {
                reason = Some(TerminalReason::SdkStop);
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

/// Where a mid-workload non-quiescent snapshot was sealed.
struct Sealed {
    vm_state: vm_state::VmState,
    step: u64,
    /// The guest-memory image **at the seal**. Captured here (not via
    /// `live.guest_memory()` after the call returns) because with `post_seal_scan > 0`
    /// the live VM steps past the seal during the tally — pairing post-scan memory with
    /// at-seal `vm_state` would seal an inconsistent (corrupt) snapshot.
    memory: Vec<u8>,
}

/// Drive `live` from boot until the **first V-time-synchronized boundary at/after
/// `marker` whose `kvm_vcpu_events` carries in-flight state the OLD task-39 predicate
/// fail-closed-rejected** ([`Vmm::has_inflight_event_injection`]) appears, and seal it.
/// That is exactly the task-41 unlock: a `kvm_vcpu_events` state task 39 could not
/// represent, which task 41 captures, canonicalizes, and restores exactly. (For this live
/// workload those points are inert KVM modifier residuals — a genuine in-flight
/// `kvm_vcpu_events` *injection* does not reliably land at a synchronized boundary, so its
/// capture→exact-restore is proven by the constructed
/// `task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact` test, not the live
/// run.) A GENUINE in-flight *event* — a vector pending in the LAPIC IRR
/// ([`Vmm::has_pending_guest_interrupt`]) or a `kvm_vcpu_events` active-injection bit
/// ([`Vmm::has_active_event_injection`]) — is tracked only as BONUS evidence: the LAPIC IRR
/// was already serialized by task 39, so an IRR-only point is not a task-39 win. When
/// `post_seal_scan > 0`, keep scanning that many more steps **after** the seal to tally the
/// before/after split **on the same run** (gate 1's evidence); pass `0` to seal-and-return
/// immediately (gates 2/3, which only need the seal point). Panics loudly if the guest
/// terminates before any task-39-rejected snapshottable point.
fn seal_first_nonquiescent(live: &mut DynVmm, marker: &[u8], post_seal_scan: u64) -> Sealed {
    let stderr = std::io::stderr();
    let mut printed = live.serial().len();
    let mut steps = 0u64;
    let mut armed = marker.is_empty();
    // Tally over the armed (at/after-marker) region.
    let mut task39_rejected = 0u64; // the OLD task-39 predicate fired — the 0→N flip (the SEAL target)
    let mut genuine_inflight = 0u64; // bonus: a genuine in-flight EVENT (LAPIC IRR pending / kve injection)
    let mut quiescent_snapshottable = 0u64; // not task-39-rejected
    let mut rejections: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut sealed: Option<Sealed> = None;
    let mut scanned_after_seal = 0u64;

    loop {
        if !armed {
            armed = find(live.serial(), marker);
        }
        if armed {
            // SEAL on the OLD task-39 rejection predicate (`has_inflight_event_injection`) —
            // a `kvm_vcpu_events` state task 39 could NOT represent and fail-closed-refused.
            // THAT is the task-41 unlock (capture + canonicalize + exactly restore the full
            // kvm_vcpu_events). For this live workload those points are inert modifier
            // residuals; a genuine in-flight kvm_vcpu_events *injection* does not reliably
            // land at a synchronized boundary, so its capture→exact-restore is proven by the
            // constructed `task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact`
            // test instead (see IMPLEMENTATION.md "what gate 2 proves / does not prove").
            // `genuine` (a vector pending in the LAPIC IRR, or a kve active-injection bit) is
            // tracked only as BONUS evidence: the LAPIC IRR was already serialized by task
            // 39, so an IRR-only point is NOT a task-39 win.
            let inflight = live.has_inflight_event_injection();
            let active_kve = live.has_active_event_injection();
            let pending_irq = live.has_pending_guest_interrupt().unwrap_or(false);
            let genuine = active_kve || pending_irq;
            match live.save_vm_state() {
                Ok(vm_state) => {
                    if genuine {
                        genuine_inflight += 1;
                    }
                    if inflight {
                        task39_rejected += 1;
                        if sealed.is_none() {
                            let what = if active_kve {
                                "a GENUINE kvm_vcpu_events injection in flight"
                            } else if pending_irq {
                                "a residual kvm_vcpu_events state + a genuine pending LAPIC interrupt"
                            } else {
                                "an inert kvm_vcpu_events residual (genuine-injection capture is \
                                 proven by the constructed test)"
                            };
                            eprintln!(
                                "[nq] sealed a TASK-39-REJECTED non-quiescent snapshot S at step \
                                 {steps} — {what}; task 39 fail-closed-refused this exact \
                                 kvm_vcpu_events state, task 41 captures + restores it"
                            );
                            sealed = Some(Sealed {
                                vm_state,
                                step: steps,
                                memory: live.guest_memory().to_vec(),
                            });
                        }
                    } else if !genuine {
                        quiescent_snapshottable += 1;
                    }
                    // (genuine && !inflight: a LAPIC-IRR-only point — counted in
                    //  genuine_inflight; task 39 already serialized the LAPIC, so it is
                    //  neither a task-39 win nor quiescent.)
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
                     TASK-39-REJECTED snapshottable point at/after the marker \
                     (task39_rejected={task39_rejected}, genuine_inflight={genuine_inflight}, \
                     quiescent_snapshottable={quiescent_snapshottable}, rejection tally: \
                     {rejections:#?})"
                );
            }
            // This non-SDK workload never emits an SDK stop (task 73) — a terminal
            // before the boundary is a gate failure, same as the arm above.
            Ok(Step::SdkStop) => {
                panic!(
                    "unexpected SDK stop at step {steps} while hunting a non-quiescent \
                     snapshot boundary (this workload declares no SDK points)"
                );
            }
            Err(e) => panic!("step while hunting a non-quiescent snapshot boundary failed: {e}"),
        }
    }

    eprintln!(
        "\n[nq] ============ BEFORE / AFTER (same Postgres run, at/after marker) ============\n\
         [nq]  BEFORE (task 39, quiescent-only): every point whose `kvm_vcpu_events` carried in-flight \
         state was fail-closed-REJECTED — {task39_rejected} synchronized boundaries (the OLD task-39 \
         predicate); all 0 snapshottable.\n\
         [nq]  AFTER  (task 41): all {task39_rejected} are now SNAPSHOTTABLE — task 41 captures the \
         full kvm_vcpu_events, canonicalizes it, and restores it exactly (the 0→N flip). Of these, \
         {genuine_inflight} synchronized boundaries also carried a GENUINE in-flight EVENT (a vector \
         pending in the LAPIC IRR / a kvm_vcpu_events injection); the genuine-injection capture→exact-\
         restore is proven by the constructed test. {quiescent_snapshottable} quiescent. still-rejected \
         (not a quiescence issue): {rejections:#?}"
    );
    let sealed =
        sealed.expect("a non-quiescent snapshottable point was found (loop only exits with one)");
    assert!(
        task39_rejected > 0,
        "gate 1: at least one synchronized boundary must carry `kvm_vcpu_events` in-flight state the \
         OLD task-39 predicate fail-closed-rejected, and now be snapshottable — the seal proves task \
         41 captures + restores a state task 39 could not represent (the 0→N flip task 40 documented \
         as missing)"
    );
    // PR #12 round 4/5 — gate rigor. Require that the live workload genuinely produced **≥ 1
    // in-flight EVENT** (a vector pending in the LAPIC IRR / a `kvm_vcpu_events` injection) —
    // not merely inert residuals — so a gate cannot "pass" on residual canonicalization alone
    // without the live run ever reaching a real in-flight point. The V-time **deterministic**
    // Postgres workload reaches the same genuine point(s) every run (non-flaky). When this
    // caller **scans to the workload terminal** (gate 1, `post_seal_scan > 0` runs the scan
    // window past the end), it asserts that here over its own scan. Gates 2/3 seal-and-return
    // at the first task-39-rejected point (`post_seal_scan == 0`) so they cannot assert it
    // here — they enforce the SAME `genuine_inflight >= 1` via the standalone
    // [`assert_run_reaches_genuine_inflight`] full-run presence check (round 5: codex/GPT-5.5
    // sharpened that gates 2/3, the headline gates, must also prove the run is not
    // residual-only). The genuine-injection *capture* proof is the constructed
    // `task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact`.
    if post_seal_scan > 0 {
        assert!(
            genuine_inflight >= 1,
            "gate 1: the scanned run must encounter ≥1 GENUINE in-flight event (LAPIC IRR pending / \
             kve injection), proving the live workload reaches real in-flight points — not only \
             residuals (genuine_inflight was {genuine_inflight})"
        );
    }
    sealed
}

/// Boot a fresh live Postgres run and assert the workload reaches **≥ 1 genuine in-flight
/// event at a snapshottable boundary** over its full span (`marker` → terminal): a vector
/// pending in the LAPIC IRR ([`Vmm::has_pending_guest_interrupt`]) or a `kvm_vcpu_events`
/// active injection ([`Vmm::has_active_event_injection`]) — **not** merely an inert residual.
///
/// The headline gates (2 and 3) **seal** the snapshot at the first task-39-rejected point
/// (an inert residual for this workload — the genuine point does not reliably land at a
/// synchronized *save* boundary, which is why the constructed
/// `task39_rejected_in_flight_kvm_events_restore_is_state_hash_exact` test exists), so the
/// seal alone proves only residual canonicalization. They additionally call this **full-run
/// presence check** so the headline result also proves the live run is **not residual-only**
/// — the same `genuine_inflight >= 1` property gate 1 asserts over its own scan, but over a
/// fresh boot-to-terminal scan that does not perturb the gate's own seal/continuation. The
/// seal POINT is unchanged. Cheap: the `save_vm_state` confirmation runs only at the rare
/// steps where a genuine event is present (short-circuit), so the scan is ~a plain run to
/// terminal. Non-flaky: the V-time-deterministic workload reaches the same point(s) every run.
fn assert_run_reaches_genuine_inflight(kernel: &[u8], initramfs: &[u8], marker: &[u8]) {
    let mut live = boot_pg(kernel, initramfs, BASE_SEED);
    let mut armed = marker.is_empty();
    let mut genuine = 0u64;
    let mut steps = 0u64;
    loop {
        if !armed {
            armed = find(live.serial(), marker);
        }
        // A genuine in-flight event AND snapshottable (save succeeds) — the same condition
        // gate 1 tallies as `genuine_inflight`. `save_vm_state` is only reached when a genuine
        // event is present (short-circuit), so the full scan stays cheap.
        if armed
            && (live.has_active_event_injection()
                || live.has_pending_guest_interrupt().unwrap_or(false))
            && live.save_vm_state().is_ok()
        {
            genuine += 1;
        }
        match live.step() {
            Ok(Step::Continued) => steps += 1,
            Ok(Step::Terminal(_)) => break,
            // A cooperating-SDK stop (task 73) ends the drive here, like a terminal.
            Ok(Step::SdkStop) => break,
            Err(e) => panic!("step while checking genuine in-flight presence failed: {e}"),
        }
    }
    assert!(
        genuine >= 1,
        "the live workload must reach ≥1 GENUINE in-flight point at a snapshottable boundary over the \
         full run (genuine={genuine}, steps={steps}) — the headline gate is not residual-only"
    );
    eprintln!(
        "[nq] genuine-presence check ✓ — the run reached {genuine} genuine in-flight point(s) over \
         {steps} steps (gates 2/3 are not residual-only)"
    );
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
    // split, and seal the first non-quiescent (interrupt-in-flight) point. `sealed`
    // captures the guest memory AT the seal (not after the tally scan), so the snapshot
    // pairs at-seal memory with at-seal vm_state.
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        // Gate 1 wants the before/after tally → scan a window past the seal.
        let sealed = seal_first_nonquiescent(&mut live, &marker, SCAN_WINDOW);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(&sealed.memory, &blob)
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

    // save_vm_state SUCCEEDED at a non-quiescent point (above — the 0→N flip); now prove
    // restore_vm_state produces a **runnable** VM that resumes without error and runs the
    // remaining workload to a clean terminal (box-confirmed: the restored continuation
    // reaches the workload's final row + GUEST_READY, then the guest's power-off Hlt).
    // Gate 2 proves it is bit-identical to the un-snapshotted continuation.
    let mut b = boot_pg(&kernel, &initramfs, BASE_SEED);
    b.restore_snapshot(
        engine.materialize(snap).expect("materialize").as_slice(),
        &engine.vm_state(snap).expect("decode"),
    )
    .expect("restore_vm_state must accept the non-quiescent snapshot");
    let outcome = drive_to_terminal(&mut b);
    eprintln!(
        "[nq] gate 1: restored continuation: terminal={:?} steps={} final_row={} GUEST_READY={} \
         step_error={:?}",
        outcome.reason, outcome.steps, outcome.final_row, outcome.guest_ready, outcome.step_error
    );
    assert!(
        outcome.internally_consistent() && outcome.steps > 0,
        "gate 1: the restored VM from a non-quiescent point must resume and cleanly complete the \
         workload — final_row + GUEST_READY, a real terminal, no step error — terminal={:?} \
         steps={} final_row={} guest_ready={} err={:?}",
        outcome.reason,
        outcome.steps,
        outcome.final_row,
        outcome.guest_ready,
        outcome.step_error,
    );
    eprintln!(
        "[nq] gate 1 ✓ a TASK-39-REJECTED point (kvm_vcpu_events in-flight state task 39 could not \
         represent) is snapshottable AND restores into a runnable VM — the 0→N flip task 40 measured \
         as missing. (Genuine in-flight-injection capture→exact-restore: the constructed test.)"
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

    // Gate rigor (PR #12 round 5): the seal below lands at the first task-39-rejected point
    // (an inert residual for this workload), so prove SEPARATELY — over a fresh full
    // boot-to-terminal scan — that the live run genuinely reaches ≥1 in-flight point, so this
    // headline gate is not merely demonstrating residual canonicalization.
    assert_run_reaches_genuine_inflight(&kernel, &initramfs, &marker);

    // --- Snapshot a RUNNING Postgres mid-workload at a non-quiescent point, AND keep
    //     stepping the LIVE (un-snapshotted) VM forward — that live continuation is the
    //     "un-snapshotted run" the restored continuation must match (the spec's gate 2:
    //     "the resumed run reaches the same terminal state_hash as the un-snapshotted
    //     run"). The reference is the live continuation from the SAME seal (not a separate
    //     from-boot run) so the V-time origin matches exactly — both share vns_base and the
    //     same retired-work timeline — making this a clean bit-for-bit restore-transparency
    //     check. Both run the remaining workload to GUEST_READY + the guest's power-off Hlt
    //     terminal (box-confirmed); the restored continuation is bit-identical to the live. ---
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let (snap, live_hash, live_serial, live_obs, live_reason) = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let sealed = seal_first_nonquiescent(&mut live, &marker, 0);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(&sealed.memory, &blob)
            .expect("snapshot the running Postgres mid-workload");
        eprintln!(
            "[nq] gate 2: sealed mid-workload S at step {}.",
            sealed.step
        );
        // Step the un-snapshotted live continuation to its terminal. It MUST cleanly
        // complete the workload (final row + GUEST_READY + a real terminal, no step error)
        // — otherwise the milestone could "pass" by comparing a shared FAILED prefix of
        // two runs that both broke the same way (a budget-expiry / step-error leaves
        // `reason == None`).
        let outcome = drive_to_terminal(&mut live);
        eprintln!(
            "[nq] gate 2: live (un-snapshotted) continuation: terminal={:?} steps={} final_row={} \
             GUEST_READY={} step_error={:?}",
            outcome.reason,
            outcome.steps,
            outcome.final_row,
            outcome.guest_ready,
            outcome.step_error
        );
        assert!(
            outcome.internally_consistent(),
            "gate 2: the un-snapshotted (live) continuation must cleanly complete the workload \
             (final_row + GUEST_READY, real terminal, no step error) — else the milestone proves \
             nothing. terminal={:?} final_row={} guest_ready={} err={:?}",
            outcome.reason,
            outcome.final_row,
            outcome.guest_ready,
            outcome.step_error,
        );
        (
            snap,
            live.state_hash(),
            live.serial().to_vec(),
            live.observable_digest(),
            outcome.reason,
        )
    };

    // --- Restore S into a fresh VM and resume to terminal, TWICE → deterministic-twice
    //     across restores, AND each must match the live (un-snapshotted) continuation. ---
    let run_restored = |label: &str| {
        let mut b = boot_pg(&kernel, &initramfs, BASE_SEED);
        b.restore_snapshot(
            engine.materialize(snap).expect("materialize").as_slice(),
            &engine.vm_state(snap).expect("decode"),
        )
        .expect("restore the mid-workload snapshot");
        let outcome = drive_to_terminal(&mut b);
        eprintln!(
            "[nq] gate 2: restored continuation ({label}): terminal={:?} steps={} final_row={} \
             GUEST_READY={} step_error={:?}",
            outcome.reason,
            outcome.steps,
            outcome.final_row,
            outcome.guest_ready,
            outcome.step_error
        );
        // Each restored continuation must ALSO cleanly complete the workload — not just
        // match the live one (which the full-hash + serial asserts below cover).
        assert!(
            outcome.internally_consistent(),
            "gate 2: the restored continuation ({label}) must cleanly complete the workload \
             (final_row + GUEST_READY, real terminal, no step error). terminal={:?} final_row={} \
             guest_ready={} err={:?}",
            outcome.reason,
            outcome.final_row,
            outcome.guest_ready,
            outcome.step_error,
        );
        (
            b.state_hash(),
            b.serial().to_vec(),
            b.observable_digest(),
            outcome.reason,
        )
    };
    let (h1, s1, o1, r1) = run_restored("replay 1");
    let (h2, s2, o2, r2) = run_restored("replay 2");

    // Deterministic-twice: two restores of S reach a bit-identical terminal (FULL hash).
    assert_eq!(
        (h1, o1, r1),
        (h2, o2, r2),
        "deterministic-twice: two restores of the mid-Postgres snapshot must reach a bit-identical \
         terminal (full state_hash + observable_digest + reason). h1={} h2={}",
        hex(&h1),
        hex(&h2)
    );
    assert_eq!(
        s1, s2,
        "deterministic-twice: restored serial must be bit-identical"
    );

    // --- Restore is exact at a non-quiescent point: the restored continuation is
    //     bit-identical to the un-snapshotted (live) continuation from the same seal,
    //     down to the **full state_hash** (after canonicalizing the two
    //     architecturally-don't-care fields — unusable-segment `type` + inert
    //     `kvm_vcpu_events` residuals — in `encode_segment`/`encode_events`; see
    //     IMPLEMENTATION.md). Guest-observable output (serial + report stream) too. ---
    assert_eq!(
        r1, live_reason,
        "restored continuation must reach the same terminal reason as the un-snapshotted run"
    );
    assert_eq!(
        s1, live_serial,
        "gate 2: the restored continuation's serial must be bit-identical to the un-snapshotted \
         (live) continuation from the seal — same guest-observable output"
    );
    assert_eq!(
        o1, live_obs,
        "gate 2: the restored continuation's observable_digest (O2: serial + report stream) must be \
         bit-identical to the un-snapshotted continuation"
    );
    eprintln!(
        "[nq] gate 2: live     terminal={live_reason:?} state_hash={}\n[nq] gate 2: restored \
         terminal={r1:?} state_hash={}",
        hex(&live_hash),
        hex(&h1)
    );
    assert_eq!(
        h1,
        live_hash,
        "gate 2 (THE MILESTONE): a Postgres run snapshotted mid-workload at a non-quiescent point, \
         restored into a fresh VM, and resumed reaches the SAME FULL terminal `state_hash` as the \
         un-snapshotted continuation from that point — restore is exact at a non-quiescent point. \
         Same state ⇒ same future, while the system is doing work. live={} restored={}",
        hex(&live_hash),
        hex(&h1)
    );
    eprintln!(
        "[nq] gate 2 ✓ mid-Postgres snapshot → restore → resume is bit-identical to the \
         un-snapshotted continuation on the FULL state_hash (+ serial + observable_digest), \
         deterministic-twice. The dissonance unlock: fork a system while it is doing work."
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
    // task 40 documented as missing — it could only seal at boot entry because every
    // mid-workload V-time-sync boundary was non-quiescent (task 39 fail-closed-rejected
    // it). Task 41 makes the mid-Postgres seal possible.
    let marker = PG_READY.to_vec();
    let k = env_usize("BRANCHES", 3);
    let n = env_usize("REPLAYS", 2);
    assert!(k >= 1 && n >= 1, "need BRANCHES>=1 and REPLAYS>=1");
    eprintln!(
        "[nq] gate 3: re-run task 40's matrix sealed MID-POSTGRES. BRANCHES(K)={k} REPLAYS(N)={n}"
    );

    // Gate rigor (PR #12 round 5): the seal below lands at the first task-39-rejected point
    // (an inert residual for this workload), so prove SEPARATELY that the live run genuinely
    // reaches ≥1 in-flight point — this headline (branching) gate is not residual-only.
    assert_run_reaches_genuine_inflight(&kernel, &initramfs, &marker);

    // --- Seal S at a mid-Postgres non-quiescent point. ---
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let sealed = seal_first_nonquiescent(&mut live, &marker, 0);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(&sealed.memory, &blob)
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
            "base replay {r} (the verbatim continuation) must cleanly complete the workload \
             (final_row + GUEST_READY, real terminal, no step error): terminal={:?} final_row={} \
             guest_ready={} err={:?}",
            fork.outcome.reason,
            fork.outcome.final_row,
            fork.outcome.guest_ready,
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
            // A branch must reach a **clean shutdown** (`GUEST_READY`, a real terminal, no
            // step error) — proving it ran a clean continuation, not a shared failed prefix.
            // We do NOT pin the workload's final row here: a seed fork is *allowed* to
            // diverge into a different guest-observable future (the point of branching), so
            // long as it cleanly shuts down.
            assert!(
                fork.outcome.step_error.is_none()
                    && fork.outcome.reason.is_some()
                    && fork.outcome.guest_ready,
                "branch {b} replay {r} must reach a clean shutdown (GUEST_READY, real terminal, no \
                 step error): terminal={:?} guest_ready={} err={:?}",
                fork.outcome.reason,
                fork.outcome.guest_ready,
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
         (each (S, seed') fork must reach a distinguishable future — the reseeded entropy stream \
         alone makes the terminal state_hash distinct)"
    );
    assert_eq!(
        engine.store_stats().stored_unique_pages,
        base_unique_pages,
        "gate 3: the forks must share one read-only base (materializing K views adds no unique \
         pages)"
    );
    eprintln!(
        "[nq] gate 3 ✓ task 40's matrix, sealed MID-POSTGRES: every fork reproducible across {n} \
         replays, ≥1 divergent (distinct terminal state_hash per seed), one shared base — the \
         mid-workload fork task 40 documented as missing. (Each fork runs the remaining workload to \
         its clean terminal; the per-branch line above reports whether the entropy fork surfaces \
         into the guest-observable workload or only the host-side entropy bookkeeping.)"
    );
}
