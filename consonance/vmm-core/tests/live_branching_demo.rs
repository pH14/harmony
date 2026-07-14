// SPDX-License-Identifier: AGPL-3.0-or-later
//! Box-only **single-node branching demo** (`#[cfg(target_os = "linux")]` **and
//! `#[ignore]`**, on `ssh <det-box>` with the LOADED patched KVM modules, CPU-pinned
//! per `docs/BOX-PINNING.md`). Task 40 — the dissonance "join point": where the
//! live snapshot/branch substrate (task 39) meets the bare-Postgres workload (task
//! 37) to show *the multiverse from one snapshot*.
//!
//! This is the **hand-driven demo of the mechanism** (not the automated explorer,
//! task 12). It:
//!
//! 1. Boots the task-37 Postgres workload image on the patched backend and seals a
//!    **quiescent snapshot point** (INTEGRATION.md §4) of the **live** machine (guest
//!    memory → `snapshot-store`, the non-memory machine → the `vm_state` codec) into a
//!    base snapshot `S`, then **drops the live VM** (freeing its `perf_event` work
//!    counter — exactly one counter is ever open at a time, per `live_postgres.rs`).
//!
//!    **Where `S` lands (a substrate constraint, measured — see `IMPLEMENTATION.md`).**
//!    Task 39's snapshot codec captures the *quiescent* machine: `vm-state`'s
//!    `VcpuEvents` deliberately omits the in-flight-interrupt-injection fields (its
//!    doc names the HLT point as the snapshot target). A **cooperative, never-halting**
//!    Postgres guest holds a LAPIC-timer interrupt in flight at essentially *every*
//!    V-time-sync boundary once the timer is calibrated, so `save_vm_state` is
//!    rejectable there — measured: **0 of 8392** post-readiness boundaries were
//!    snapshummable (5280 non-synchronized, 3112 in-flight injection). The reliably
//!    clean boundaries are the interrupt-free V-time reads of **early boot**. A second
//!    constraint converges on the same place: the entropy fork only surfaces into
//!    *guest-observable* state (RAM/serial) if it happens **before** the kernel seeds
//!    its CRNG (which it does before the first console byte) — seal later and the
//!    branches are byte-identical guests differing only in host-side entropy
//!    bookkeeping ("N identical reruns," not a multiverse). So `S` is sealed at the
//!    **boot-entry quiescent point**, and the forks each run the *whole*
//!    boot→Postgres→workload forward from `S` — a real fork into many futures, rooted
//!    at boot entry rather than mid-workload (a task-39 substrate limit, not a choice).
//!
//!    **Superseded by task 41 (the non-quiescent-snapshot unlock).** The "0 of 8392"
//!    measurement and the boot-entry-only seal below reflect task 39's *quiescent-only*
//!    codec. Task 41 captures the in-flight `kvm_vcpu_events` task 39 dropped, so those
//!    in-flight points are now snapshottable and `S` can be sealed **mid-workload** —
//!    see `tests/live_nonquiescent_snapshot.rs` (the mid-Postgres round-trip + the
//!    mid-Postgres branching matrix). This demo's boot-entry seal is left intact as the
//!    task-40 baseline; the mid-workload capability lives in the task-41 gate.
//!
//! 2. Forks `S` into a **base continuation** (replay `S` verbatim — restore, no
//!    reseed) and `K` **branches**, each `branch(S, seed') = restore(S) +
//!    reseed_entropy(seed')` (task 39 Phase 4). Every fork materializes its own
//!    private CoW view over the **one shared read-only base** (gate-3 sharing: the
//!    store keeps the base's pages once store-wide, never `K ×`).
//!
//! 3. Proves the two properties that make `S` a *multiverse*, not a copy:
//!    - **Reproducibility (the headline, gate 1):** each fork, replayed from its
//!      `(S, seed)` pair `N` times, is **bit-identical** every time
//!      (`state_hash`). The matrix (per-fork digest, equal across its `N` replays)
//!      is printed. This is the Antithesis property — any future it finds it can
//!      replay exactly.
//!    - **Divergence (gate 2):** at least one branch's terminal `state_hash`
//!      **differs** from the base continuation — quoted with a **per-component
//!      breakdown** (`Vmm::state_components`) so *which* state diverged (guest RAM,
//!      serial, devices, or the determinism/entropy bookkeeping) is shown, not
//!      asserted.
//!
//! ## Bug class — honest expectations
//!
//! On RAM-backed storage the realistic divergence class is **concurrency /
//! scheduling** (timer-driven preemption interleavings, recovery-path ordering),
//! **not durability / crash-consistency**: there is no durable-vs-volatile split to
//! fault against (the "fsync lied → recover wrong" class rides the deferred host-side
//! RAM-disk model, **D1**). The crash-timing fault the spec also lists ("kill
//! `postgres` at V-time T, restart so WAL recovery runs") needs either a cooperating
//! guest or the host-side fault seam (`dissonance/environment` — a *separate*, live
//! frontier), so it is **out of scope here** and called out in `IMPLEMENTATION.md`.
//! This demo drives the **entropy-fork** knob the public branch API exposes
//! (`reseed_entropy`); what that surfaces into guest-observable state on this
//! substrate is **measured and reported**, never asserted beyond what the substrate
//! produces.
//!
//! ## Gate honesty (why `#[ignore]`)
//!
//! Needs real + patched KVM, the built Postgres image (`guest/build/bzImage` +
//! `guest/build/initramfs-postgres.cpio.gz`, via
//! `guest/linux/build-postgres-image.sh`), and the `det-cfl-v1` host — none in the
//! default `cargo nextest` lane — so it is `#[ignore]`d (like `live_postgres.rs` /
//! `live_snapshot_branch.rs`); default CI shows it not-run, never a vacuous green.
//! Every missing precondition is a **loud panic**, never an early-return `Ok`. macOS
//! builds an empty test binary; the snapshot/branch + reseed wiring is covered
//! portably by `tests/snapshot_branch.rs` and the `src/snapshot.rs` unit tests.
//!
//! ```sh
//! make -C guest fetch && make -C guest/linux postgres-image       # build the image
//! # load patched kvm.ko/kvm-intel.ko, then (core 4 per the box briefing):
//! taskset -c 4 timeout 3600 cargo test -p vmm-core --test live_branching_demo \
//!     -- --ignored --nocapture --test-threads=1
//! # always revert to stock KVM afterwards and verify `lsmod | grep '^kvm '` == 1396736
//! # (coordinate first: check lsmod, do NOT revert while another patched run is live).
//! ```
//!
//! Knobs (env): `BRANCHES` (K, default 4), `REPLAYS` (N per fork, default 3),
//! `SNAPSHOT_MARKER` (the serial substring that marks the quiescent snapshot point,
//! default the postmaster-ready banner), `BOOT_CMDLINE` (overrides the kernel
//! command line, as in `live_postgres.rs`).
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;
use std::time::{Duration, Instant};

use snapshot_store::SnapshotId;
use vmm_backend::{Backend, X86};
use vmm_core::snapshot::SnapshotEngine;
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, TerminalReason, Vmm};

/// 2 GiB of guest RAM — identical to `live_postgres.rs` (the Postgres rootfs + the
/// RAM-backed ext4 PGDATA + shared memory + per-backend processes). The
/// `SnapshotEngine` is sized to match so a full image round-trips.
const GUEST_RAM_LEN: usize = 2 << 30;
/// The base seed — the seed the live (un-branched) run uses, and the seed the base
/// continuation replays verbatim. Same value `live_postgres.rs` pins.
const BASE_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line (identical to `live_postgres.rs`; see that file for
/// the rationale of each token, notably the dropped `random.trust_cpu=off` and the
/// `reboot=t,force`).
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Step budget per run — a high cap so a stuck guest cannot run forever (real bound
/// is the wall budget + the external `timeout`).
const MAX_STEPS: u64 = 50_000_000_000;
/// Per-run wall-clock budget. A *branch/replay* resumes a snapshot taken after the
/// kernel + Postgres are already up, so it runs only the remaining workload +
/// shutdown — far lighter than the full boot the initial snapshot pass pays — but the
/// budget is kept generous (the initial boot-to-snapshot pass is the heavy one).
const WALL_BUDGET: Duration = Duration::from_secs(1200);

/// postgres announces this once the cluster is accepting connections — used to report
/// whether `S` was sealed before or after Postgres came up (the snapshot itself is
/// sealed at the first clean boundary, see `seal_first_clean`).
const PG_READY: &[u8] = b"database system is ready to accept connections";
/// The deterministic prefix of the final workload row (iteration 20): `row`, loop
/// index 20, running `count(*)` = 20, running `sum(i)` = 210 (task 42's workload v2 —
/// the streamed line is `row|20|20|210|<uuid>|<t>`, the uuid + t seed-derived). This
/// count/sum prefix is a pure function of the loop index, so matching it proves the
/// *query results* reached the serial in every continuation. (The demo only needs a
/// workload-completed marker; the per-row UUID/timestamp shape is gated in
/// `live_postgres.rs`.)
const FINAL_ROW: &[u8] = b"row|20|20|210|";
/// `pg-init.sh` prints this after a clean shutdown.
const GUEST_READY: &[u8] = b"GUEST_READY";

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// Read a built guest artifact (`guest/build/<name>` then `guest/linux/<name>`).
/// Panics loudly (with the build command) if absent — this `#[ignore]`d gate runs
/// only on the box, where the image is built first.
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

/// Require the §1.1 `det-cfl-v1` host baseline, else **panic** with the report (the
/// boot would refuse such a host anyway).
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

/// The serial substring after which we are willing to seal the base snapshot `S` —
/// the snapshot is taken at the **first clean boundary at-or-after** it appears.
/// Default empty: seal the very first clean boundary (early boot — the reliably
/// snapshummable phase of an interrupt-driven Linux guest; see `seal_first_clean`).
/// Set e.g. `SNAPSHOT_MARKER="database system is ready"` to try sealing nearer the
/// workload (succeeds only if a clean boundary exists after it).
fn snapshot_marker() -> Vec<u8> {
    std::env::var("SNAPSHOT_MARKER")
        .map(String::into_bytes)
        .unwrap_or_default()
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn find(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// A distinct, non-base entropy seed for branch `k` (a multiplicative hash of
/// `k + 1` XOR-folded into the base — distinct from `BASE_SEED` and from each other).
fn branch_seed(k: usize) -> u64 {
    BASE_SEED ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1)
}

type DynVmm = Vmm<Box<dyn Backend<A = X86>>>;

/// What a bounded run observed (mirrors `live_postgres.rs`).
struct RunOutcome {
    reason: Option<TerminalReason>,
    steps: u64,
    final_row: bool,
    guest_ready: bool,
    step_error: Option<String>,
}

impl RunOutcome {
    /// A continuation is *internally consistent* iff it ran the workload's final row
    /// and reached `GUEST_READY` through a clean terminal with no contract violation —
    /// so a branch cannot pass by stranding identically.
    fn internally_consistent(&self) -> bool {
        self.reason.is_some() && self.step_error.is_none() && self.final_row && self.guest_ready
    }
}

/// Drive `vmm` to a terminal state (or the step / wall budget), optionally stopping
/// **early** the first time `stop_marker` appears on the serial (used to reach the
/// snapshot point in the live boot pass). Streams new serial bytes to stderr as they
/// are captured so a hang shows the last line reached. Returns the outcome and
/// whether `stop_marker` was hit (always `false` when `stop_marker` is `None`).
fn drive(vmm: &mut DynVmm, stop_marker: Option<&[u8]>) -> (RunOutcome, bool) {
    // not order-observable: a test-only wall-clock watchdog (belt-and-braces with
    // the external `timeout`); it bounds this `#[ignore]`d box gate and never reaches
    // guest state, the serial capture, or any hash.
    #[allow(clippy::disallowed_methods)]
    let start = Instant::now();
    let mut printed = 0usize;
    let mut steps = 0u64;
    let mut reason = None;
    let mut step_error = None;
    let mut hit_marker = false;
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
                let mut msg = format!("{e}");
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    msg.push_str(&format!(" | {s}"));
                    src = s.source();
                }
                eprintln!("\n[demo] step error after {steps} steps: {msg}");
                step_error = Some(msg);
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
        if let Some(m) = stop_marker
            && find(serial, m)
        {
            hit_marker = true;
            break;
        }
        if steps.is_multiple_of(8192) && start.elapsed() > WALL_BUDGET {
            eprintln!("\n[demo] wall-clock budget exceeded after {steps} steps");
            break;
        }
    }
    let serial = vmm.serial();
    (
        RunOutcome {
            reason,
            steps,
            final_row: find(serial, FINAL_ROW),
            guest_ready: find(serial, GUEST_READY),
            step_error,
        },
        hit_marker,
    )
}

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

/// What sealing the live VM produced: the canonical `vm_state` blob, the step it was
/// taken at, and the serial captured up to that point (so the demo can report *where*
/// in the guest's life `S` was taken).
struct LiveSnapshot {
    vm_state: vm_state::VmState,
    step: u64,
    serial: Vec<u8>,
}

/// Drive `live` from its current point and seal the **first** clean, representable,
/// V-time-synchronized boundary at-or-after `earliest` appears on the serial (empty =
/// the very first clean boundary). `save_vm_state` is retried at each boundary,
/// stepping once between attempts.
///
/// **Why the boot-entry quiescent point (two constraints converge there).**
/// 1. *Substrate.* Task 39's snapshot codec captures only the **quiescent** machine —
///    no in-flight interrupt injection (`vm-state`'s `VcpuEvents` omits the injection
///    fields; its doc names the HLT point as the target), and
///    `snapshot::unrepresentable_state` fails closed on any pending injection. A
///    *cooperative, never-halting* Postgres guest holds a LAPIC-timer interrupt in
///    flight at essentially every V-time-sync boundary once the timer is calibrated
///    (measured: 0 of 8392 post-readiness boundaries snapshummable), so the reliably
///    clean boundaries are early boot's interrupt-free V-time reads.
/// 2. *Meaningful divergence.* The entropy fork only surfaces into **guest-observable**
///    state (RAM/serial) if it happens **before** the kernel seeds its CRNG (which it
///    does before the first console byte). Seal later and the branches are byte-for-
///    byte identical guests differing only in the host-side entropy bookkeeping —
///    "N identical reruns," not a multiverse.
///
/// Both point to the same place, so by default `S` is sealed at the first clean
/// boundary (boot entry). The forks then each run the *whole* boot→Postgres→workload
/// forward from `S`. See `IMPLEMENTATION.md` for the full reasoning + the limitation.
///
/// Arming: with no `earliest` marker (default), seal at the first clean boundary
/// (boot entry); with an explicit marker, seal at the first clean boundary once it
/// appears on the serial (a later, post-CRNG-seed root — demonstrates the trade-off).
///
/// Streams serial. Panics with a rejection tally if the guest terminates before a
/// clean boundary is found (a loud failure, never a silent skip).
fn seal_first_clean(live: &mut DynVmm, earliest: &[u8]) -> LiveSnapshot {
    let stderr = std::io::stderr();
    let mut printed = live.serial().len();
    let mut reasons: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut attempts = 0u64;
    // Default (empty marker): seal at the **boot-entry quiescent point** — the first
    // clean boundary, before the guest seeds its kernel CRNG. This is deliberate: the
    // entropy fork only surfaces into *guest-observable* state (RAM/serial) if it
    // happens **before** the CRNG is seeded (which the kernel does before the first
    // console byte), so a later seal yields only bookkeeping-level divergence —
    // "N identical reruns," not a multiverse. With an explicit marker, seal at the
    // first clean boundary once it appears (a *later*, post-CRNG-seed root — useful to
    // demonstrate that very trade-off; see IMPLEMENTATION.md).
    let mut armed = earliest.is_empty();
    let mut steps = 0u64;
    loop {
        if !armed {
            armed = find(live.serial(), earliest);
        }
        if armed {
            attempts += 1;
            match live.save_vm_state() {
                Ok(vm_state) => {
                    eprintln!(
                        "[demo] sealed a clean snapshot S at step {steps} (after {attempts} save \
                         attempts at/after the earliest marker)"
                    );
                    return LiveSnapshot {
                        vm_state,
                        step: steps,
                        serial: live.serial().to_vec(),
                    };
                }
                Err(e) => {
                    *reasons.entry(format!("{e}")).or_insert(0) += 1;
                }
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
            Ok(Step::Terminal(r)) => panic!(
                "the guest reached a terminal ({r:?}) at step {steps} before any clean snapshot \
                 boundary (armed={armed}, {attempts} save attempts). save_vm_state rejection tally: \
                 {reasons:#?}"
            ),
            // This non-SDK workload never emits an SDK stop (task 73) — a stop
            // before the boundary is a gate failure, same as the terminal arm.
            Ok(Step::SdkStop) => panic!(
                "unexpected SDK stop at step {steps} while hunting a clean snapshot boundary \
                 (this workload declares no SDK points)"
            ),
            Err(e) => panic!("step while hunting a clean snapshot boundary failed: {e}"),
        }
    }
}

/// One fork's terminal observation: the full-state digest, the per-component digest
/// breakdown (for divergence localization), and the run outcome.
struct ForkResult {
    hash: [u8; 32],
    components: Vec<(&'static str, [u8; 32])>,
    outcome: RunOutcome,
}

/// Restore base snapshot `snap` into a **fresh** patched VM (single open work
/// counter at a time — the prior VM must already be dropped), optionally reseed the
/// entropy stream (`Some(seed)` = a branch; `None` = the base continuation replayed
/// verbatim), run it forward to a terminal, and return its [`ForkResult`]. The VM and
/// the materialized CoW mapping are dropped on return.
fn run_fork(
    engine: &SnapshotEngine,
    snap: SnapshotId,
    kernel: &[u8],
    initramfs: &[u8],
    reseed: Option<u64>,
) -> ForkResult {
    // The fresh VM only needs to be composed like the snapshot source (vtime +
    // xAPIC + legacy wired, same contract) — its boot-loaded image is immediately
    // overwritten by the restore. `boot_linux_selected` produces exactly that. The
    // boot seed is irrelevant (restore overwrites the entropy stream; a branch then
    // reseeds it explicitly).
    let mut vmm = boot_pg(kernel, initramfs, reseed.unwrap_or(BASE_SEED));
    let mapping = engine
        .materialize(snap)
        .expect("materialize the shared base");
    let vm_state = engine.vm_state(snap).expect("decode the sealed vm_state");
    vmm.restore_snapshot(mapping.as_slice(), &vm_state)
        .expect("restore base snapshot into the fresh VM");
    if let Some(seed) = reseed {
        vmm.reseed_entropy(seed)
            .expect("reseed the entropy stream for the branch");
    }
    let (outcome, _) = drive(&mut vmm, None);
    ForkResult {
        hash: vmm.state_hash(),
        components: vmm.state_components(),
        outcome,
    }
}

/// Assert every replay of one fork produced the same `state_hash` (reproducibility),
/// returning the canonical digest. Panics with the offending pair if any replay
/// diverged.
fn fold_identical(label: &str, hashes: &[[u8; 32]]) -> [u8; 32] {
    let first = hashes[0];
    for (i, h) in hashes.iter().enumerate().skip(1) {
        assert_eq!(
            *h,
            first,
            "{label}: replay {i} diverged from replay 0 — NOT reproducible.\n  replay 0: {}\n  \
             replay {i}: {}",
            hex(&first),
            hex(h)
        );
    }
    first
}

/// The components (in `Vmm::state_components` order) by which two states differ.
fn diff_components<'a>(a: &[(&'a str, [u8; 32])], b: &[(&'a str, [u8; 32])]) -> Vec<&'a str> {
    a.iter()
        .zip(b.iter())
        .filter(|((_, ha), (_, hb))| ha != hb)
        .map(|((label, _), _)| *label)
        .collect()
}

/// A component name denotes *guest-observable* state (vs. the host-side
/// determinism/entropy bookkeeping in the `vtim:` namespace). Divergence reaching one
/// of these is the strong "it explored a different guest future" signal.
fn is_guest_observable(component: &str) -> bool {
    component.starts_with("RAM:")
        || matches!(
            component,
            "regs"
                | "segments"
                | "desc-tables"
                | "control-regs"
                | "pdptrs"
                | "xcr0"
                | "debugregs"
                | "events"
                | "mp_state"
                | "msrs"
                | "xsave-legacy"
                | "xsave-header"
                | "xsave-extended"
                | "serial"
                | "dev"
        )
}

/// DIAGNOSTIC (not a gate): scan the *whole* live Postgres run and report at which
/// steps `save_vm_state` would succeed (a clean, representable, V-time-synchronized
/// snapshot point), with the serial context at each, plus a rejection tally. Used to
/// learn where — if anywhere — an interrupt-driven Linux guest is snapshummable.
#[test]
#[ignore = "diagnostic, box-only: scans for snapshummable points across a live Postgres run"]
fn scan_snapshot_points() {
    require_kvm();
    require_host_baseline();
    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");
    let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);

    #[allow(clippy::disallowed_methods)] // test-only watchdog; never reaches state/hash
    let start = Instant::now();
    let mut reasons: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    let mut ok_points: Vec<(u64, usize, String)> = Vec::new();
    let mut steps = 0u64;
    loop {
        match live.save_vm_state() {
            Ok(_) => {
                let serial = live.serial();
                let tail: String =
                    String::from_utf8_lossy(&serial[serial.len().saturating_sub(48)..])
                        .replace('\n', "⏎");
                if ok_points.len() < 64 {
                    ok_points.push((steps, serial.len(), tail));
                }
                *reasons.entry("OK".to_string()).or_insert(0) += 1;
            }
            Err(e) => {
                let msg = format!("{e}");
                let key = msg.split(':').nth(1).unwrap_or(&msg).trim().to_string();
                *reasons.entry(key).or_insert(0) += 1;
            }
        }
        match live.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                eprintln!("[scan] terminal {r:?} after {steps} steps");
                break;
            }
            // A cooperating-SDK stop (task 73) ends the scan, like a terminal.
            Ok(Step::SdkStop) => {
                eprintln!("[scan] SDK stop after {steps} steps");
                break;
            }
            Err(e) => {
                eprintln!("[scan] step error after {steps} steps: {e}");
                break;
            }
        }
        steps += 1;
        if steps.is_multiple_of(8192) && start.elapsed() > WALL_BUDGET {
            eprintln!("[scan] wall budget hit after {steps} steps");
            break;
        }
    }
    eprintln!("[scan] save_vm_state result tally over {steps} steps: {reasons:#?}");
    eprintln!(
        "[scan] first {} OK snapshot points (step, serial_len, ctx):",
        ok_points.len()
    );
    for (s, len, tail) in &ok_points {
        eprintln!("[scan]   step {s:>8}  serial_len={len:>7}  …{tail}");
    }
    assert!(
        !ok_points.is_empty(),
        "no snapshummable point anywhere in the live Postgres run — the snapshot substrate cannot \
         capture this interrupt-driven Linux guest at any V-time-sync boundary (tally: {reasons:#?})"
    );
}

#[test]
#[ignore = "box-only branching demo (LOADED patched KVM + built Postgres image + det-cfl-v1 host); \
            run on `ssh <det-box>` with `-- --ignored --nocapture`"]
fn branching_demo_reproducibility_and_divergence() {
    require_kvm();
    require_host_baseline();

    let k = env_usize("BRANCHES", 4);
    let n = env_usize("REPLAYS", 3);
    assert!(k >= 1 && n >= 1, "need BRANCHES>=1 and REPLAYS>=1");
    let marker = snapshot_marker();
    eprintln!(
        "[demo] cmdline: {}\n[demo] BRANCHES(K)={k} REPLAYS(N)={n} SNAPSHOT_MARKER={:?}",
        cmdline(),
        String::from_utf8_lossy(&marker),
    );

    let kernel = require_artifact("bzImage");
    let initramfs = require_artifact("initramfs-postgres.cpio.gz");

    // --- 1. Boot the live guest and seal the base snapshot S at the first clean
    //        boundary at-or-after the marker (default: the first clean boundary).
    eprintln!("\n[demo] === booting the live guest and sealing the base snapshot S ===");
    let mut engine = SnapshotEngine::new(GUEST_RAM_LEN);
    let snap = {
        let mut live = boot_pg(&kernel, &initramfs, BASE_SEED);
        let sealed = seal_first_clean(&mut live, &marker);
        let blob = sealed.vm_state.encode().expect("vm_state encodes");
        let snap = engine
            .snapshot_base(live.guest_memory(), &blob)
            .expect("snapshot the live guest image + vm_state");
        let s = engine.stats(snap).expect("base stats");
        let ctx: String =
            String::from_utf8_lossy(&sealed.serial[sealed.serial.len().saturating_sub(96)..])
                .replace('\n', " ⏎ ");
        let phase = if find(&sealed.serial, PG_READY) {
            "AFTER Postgres announced ready"
        } else {
            "BEFORE Postgres announced ready (early boot)"
        };
        eprintln!(
            "[demo] base snapshot S sealed at step {} ({phase}): {} guest pages, {} owned \
             (non-zero) pages, vm_state {} bytes.\n[demo]   serial-so-far ({} bytes), tail: …{ctx}",
            sealed.step,
            engine.mem_pages(),
            s.owned_pages,
            blob.len(),
            sealed.serial.len(),
        );
        // Drop the live VM (and its perf counter) before any fork boots.
        snap
    };
    let base_unique_pages = engine.store_stats().stored_unique_pages;

    // --- 2/3a. Base continuation (verbatim replay), N times → reproducibility.
    eprintln!("\n[demo] === base continuation: replay S verbatim, {n}× ===");
    let mut base_hashes = Vec::with_capacity(n);
    let mut base_components = None;
    for r in 0..n {
        let fork = run_fork(&engine, snap, &kernel, &initramfs, None);
        let out = &fork.outcome;
        eprintln!(
            "[demo] base replay {r}: state_hash={} final_row={} GUEST_READY={} terminal={:?} \
             steps={}",
            hex(&fork.hash),
            out.final_row,
            out.guest_ready,
            out.reason,
            out.steps,
        );
        assert!(
            out.internally_consistent(),
            "base replay {r} must run the workload to its final row and reach GUEST_READY cleanly \
             (got final_row={}, guest_ready={}, terminal={:?}, err={:?})",
            out.final_row,
            out.guest_ready,
            out.reason,
            out.step_error,
        );
        base_hashes.push(fork.hash);
        base_components = Some(fork.components);
    }
    let base_digest = fold_identical("base continuation", &base_hashes);
    let base_components = base_components.expect("n>=1");

    // --- 2/3b. K branches, each branch(S, seed') replayed N times.
    eprintln!("\n[demo] === {k} entropy-fork branches, each replayed {n}× ===");
    struct Branch {
        seed: u64,
        digest: [u8; 32],
        components: Vec<(&'static str, [u8; 32])>,
    }
    let mut branches = Vec::with_capacity(k);
    for b in 0..k {
        let seed = branch_seed(b);
        let mut hashes = Vec::with_capacity(n);
        let mut comps = None;
        for r in 0..n {
            let fork = run_fork(&engine, snap, &kernel, &initramfs, Some(seed));
            let out = &fork.outcome;
            eprintln!(
                "[demo] branch {b} (seed={seed:#018x}) replay {r}: state_hash={} final_row={} \
                 GUEST_READY={} terminal={:?} steps={}",
                hex(&fork.hash),
                out.final_row,
                out.guest_ready,
                out.reason,
                out.steps,
            );
            assert!(
                out.internally_consistent(),
                "branch {b} replay {r} must be internally consistent (workload final row + \
                 GUEST_READY, clean terminal): final_row={} guest_ready={} terminal={:?} err={:?}",
                out.final_row,
                out.guest_ready,
                out.reason,
                out.step_error,
            );
            hashes.push(fork.hash);
            comps = Some(fork.components);
        }
        let digest = fold_identical(&format!("branch {b}"), &hashes);
        branches.push(Branch {
            seed,
            digest,
            components: comps.expect("n>=1"),
        });
    }

    // --- The reproducibility matrix (GATE 1, the headline) ---------------------
    eprintln!("\n[demo] ======================= REPRODUCIBILITY MATRIX =======================");
    eprintln!(
        "[demo]  fork                       seed                 replays   digest (all equal)"
    );
    eprintln!(
        "[demo]  base (verbatim replay)     {:#018x}   {n:>3}/{n:<3}   {}",
        BASE_SEED,
        hex(&base_digest)
    );
    for (b, br) in branches.iter().enumerate() {
        eprintln!(
            "[demo]  branch {b:<2}                  {:#018x}   {n:>3}/{n:<3}   {}",
            br.seed,
            hex(&br.digest)
        );
    }
    eprintln!(
        "[demo] gate 1 ✓ every fork is bit-identical across its {n} replays from (S, seed) — \
         reproducible."
    );
    eprintln!(
        "[demo] base shared once store-wide: {} unique pages after {k} branches (× materialized \
         views) — one read-only base, not {k}× copies.",
        base_unique_pages,
    );
    assert_eq!(
        engine.store_stats().stored_unique_pages,
        base_unique_pages,
        "the forks must share one read-only base: materializing K views adds no unique pages."
    );

    // --- Divergence (GATE 2) ---------------------------------------------------
    eprintln!("\n[demo] ============================ DIVERGENCE ============================");
    let mut any_diverged = false;
    let mut any_guest_observable = false;
    for (b, br) in branches.iter().enumerate() {
        if br.digest == base_digest {
            eprintln!(
                "[demo]  branch {b}: digest EQUALS the base continuation — this branch did not \
                 diverge."
            );
            continue;
        }
        any_diverged = true;
        let diff = diff_components(&base_components, &br.components);
        let guest: Vec<&str> = diff
            .iter()
            .copied()
            .filter(|&c| is_guest_observable(c))
            .collect();
        if !guest.is_empty() {
            any_guest_observable = true;
        }
        eprintln!(
            "[demo]  branch {b} DIVERGES from base:\n           base   = {}\n           branch = \
             {}\n           differing components = {:?}\n           guest-observable among them = \
             {:?}",
            hex(&base_digest),
            hex(&br.digest),
            diff,
            guest,
        );
    }
    assert!(
        any_diverged,
        "gate 2: at least one branch's terminal state_hash must differ from the base continuation \
         — none did. The (S, seed') fork is not reaching the terminal state."
    );
    if any_guest_observable {
        eprintln!(
            "[demo] gate 2 ✓ divergence reaches GUEST-OBSERVABLE state (RAM/regs/serial/devices) — \
             a branch explored a genuinely different, internally-consistent future. Multiverse, \
             not a copy."
        );
    } else {
        eprintln!(
            "[demo] gate 2: terminal state_hash differs, but only in the host-side \
             determinism/entropy bookkeeping (`vtim:` components), not guest-observable state — the \
             branches are byte-identical guests (\"N identical reruns\"). This happens when `S` is \
             sealed AFTER the kernel CRNG is seeded; see IMPLEMENTATION.md."
        );
    }
    // For the default (boot-entry) seal, gate 2 requires *meaningful* divergence —
    // reaching guest-observable state, not just the entropy counter. (With an explicit
    // SNAPSHOT_MARKER the run intentionally seals later to demonstrate the trade-off,
    // so the meaningful-divergence bar is relaxed there — the divergence is still
    // reported.)
    if marker.is_empty() {
        assert!(
            any_guest_observable,
            "gate 2 (meaningful divergence): at the boot-entry seal, a branch's divergence must \
             reach GUEST-OBSERVABLE state (RAM/regs/serial/devices), not only the entropy \
             bookkeeping — else it is N identical reruns, not a multiverse."
        );
    }
}
