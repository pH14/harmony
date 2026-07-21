// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Task-68 box gates (a)/(b)/(c)** — `#![cfg(all(target_os = "linux", target_arch = "x86_64"))]` **and
//! `#[ignore]`**: needs real + LOADED patched KVM, the det-cfl-v1 host, and
//! the built Postgres image. Runs the same chain protocol the portable
//! loopback proves (`campaign_runner::materialize::run_materialize`, over the
//! task-58 socket against the real guest), then checks the gates as a pure
//! function of the report:
//!
//! - **(a) measured depth** — the deep exemplar materializes parent-rooted
//!   (only its own suffix), and its depth ratio against a full from-scratch
//!   re-execution beats the task-63 §4 baseline (1.5463 % = 15 463 ppm;
//!   `SEAL-RATE-REPORT.md` §6) — both numbers are printed.
//! - **(b) eviction round-trip** — evict the retained ancestor,
//!   re-materialize (deeper, compose-folded replay) → bit-identical
//!   `state_hash`; then evict everything → the from-genesis worst case, still
//!   bit-identical.
//! - **(c) composed reproducer** — a run below the ≥ 2-deep chain replays
//!   from the base via its compose-folded `bug_env` with identical stop +
//!   `state_hash` (the `docs/history/IMPLEMENTATION-task-93.md` end-to-end gate, on
//!   the production codec and real `recorded_env`).
//!
//! Run (per `docs/BOX-PINNING.md` — use the standing frontier-gate core;
//! serialize with any other frontier gate):
//!
//! ```sh
//! taskset -c 2 timeout 7200 cargo test -p campaign-runner --test live_materialization \
//!     -- --ignored --nocapture --test-threads=1 2>&1 | tee /tmp/live_materialization.log
//! ```
//!
//! **Images are pinned by content hash** (hm-xdp / hm-2nt): the harness refuses
//! to run on a bzImage/initramfs whose sha256 differs from the pinned
//! task-78-proven pair. The box's canonical `initramfs-postgres.cpio.gz` was
//! silently rebuilt 2026-07-09 (t81 checkout, md5 `9860a065…`) — a mutation
//! under main's gates that no path caught, exactly the silent-drift hazard this
//! pin closes: a mismatched image is now a loud, expected-vs-found refusal, not
//! a quiet mis-probe. (The drift was not itself what broke the task-78
//! `REQUIRE_DRAWS` precondition — the pinned PR-44 image fails it identically at
//! the old default `HOPS=3`, hops all false / tail draws; that was a stale
//! default, corrected to `HOPS=4` below. Every substantive assertion — depth,
//! round-trip, reproducer — passes on the pinned image either way.) The ruling
//! (bead `hm-xdp`; the new-image path is `hm-2nt`) pins the gate to the PR-44
//! pair by content hash and FAILS CLOSED on any future drift, quoting the
//! expected-vs-found sha256, rather than silently mis-probing. Stage the pinned
//! build (e.g. from the box's `/root/harmony-pr44/harmony-linux/build`) and verify with
//! `sha256sum harmony-linux/build/{bzImage,initramfs-postgres.cpio.gz}` against the
//! `PINNED_*` constants below, or run a DIFFERENT build deliberately via
//! `INITRAMFS=<name> INITRAMFS_SHA256=<hex>` (+ `BZIMAGE_SHA256=<hex>` /
//! `KERNEL=<name>` for the kernel).
//!
//! Knobs: `HOPS` (default 4 — the PR-44-proven count; `HOPS=3` measures no hop
//! draw on the pinned image, see the `cfg_hops` note), `HOP_DELTA_VNS` (default 2 000 000),
//! `TAIL_DELTA_VNS` (default 1 000 000), `CHAIN_SEED`, `READY_MARKER`,
//! `KERNEL`/`INITRAMFS` (filenames under `harmony-linux/build` or `harmony-linux/linux`) with
//! the `BZIMAGE_SHA256`/`INITRAMFS_SHA256` pins above.
//!
//! **Box-safety (CRITICAL).** Stock KVM = 1396736; ALWAYS leave the box on
//! stock + verified after the run: `pkill -9 -f live_materialization` FIRST
//! (separate ssh call; expect exit 255 on drop) → wait `lsmod | grep
//! '^kvm_intel'` users=0 → `rmmod kvm_intel kvm; modprobe kvm; modprobe
//! kvm_intel` → verify size 1396736 on a FRESH connection.
//!
//! **Task 78 (draw-carrying fold, FRONTIER).** The env format now stores every
//! hop's **reseed marker** and the server re-executes each collapsed hop's
//! reseed at its recorded Moment, so the round-trip / reproducer hashes are
//! bit-identical **even when entropy is drawn inside a collapsed interval**
//! (the task-68 documented limit, retired; positive twin pinned portably in
//! `tests/materialize_loopback.rs::sequential_entropy_fold_is_bit_identical_reseed_markers_flip_the_task68_pin`).
//! This gate therefore also requires the tail window to actually DRAW
//! (`MaterializeReport::tail_draws`, a measured two-seed divergence probe —
//! never an assumption): drive the guest into an entropy-drawing span (the
//! Postgres workload's `gen_random_uuid()` loop rides `pg_strong_random` →
//! RDRAND, so a `READY_MARKER` inside the workload loop works; a raw-RDRAND
//! payload or the task-73 SDK entropy service also qualifies), or set
//! `REQUIRE_DRAWS=0` to accept a draw-free window (the pre-task-78 shape,
//! e.g. for an A/B against the old baseline). If a gate (b)/(c) hash mismatch
//! appears WITH draws, that is a task-78 defect (marker lost / mis-spliced /
//! mis-anchored) — a real finding on this task's machinery.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;

use campaign_runner::materialize::{
    MaterializeConfig, TASK63_BASELINE_PPM, render_materialize_table, verify_materialize,
};
use campaign_runner::run_session;
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, VmmFactory};
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, Vmm};

/// 2 GiB guest RAM (matches `live_branching_demo.rs` / the campaign-runner box mode).
const GUEST_RAM_LEN: usize = 2 << 30;
/// The boot seed the live VM runs under (matches the branching demo).
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The determinism command line (identical to the branching demo).
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                       lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// A safety cap on the boot-to-marker drive (the external `timeout` is the
/// real bound; this stops a wedged guest from looping forever).
const MAX_BOOT_STEPS: u64 = 50_000_000_000;

/// **Pin-by-content-hash discipline (foreman ruling, beads `hm-xdp` / `hm-2nt`).**
/// This gate references the guest images by CONTENT HASH, never a mutable
/// canonical path: the 2026-07-09 rebuild of the box's canonical
/// `initramfs-postgres.cpio.gz` silently changed what default-knob gate runs
/// were testing — a mutation under main's gates that no path caught. These pins are
/// the task-78-proven pair (the `/root/harmony-pr44` build, Jul 2; initramfs
/// md5 `46b1461962b5b0f8aea98654f52a9ce5` for cross-reference) — the same pins
/// `vmm-core`'s task-95 `live_dirty_remap` gate enforces, so the two gates
/// cannot drift apart on which image "the Postgres guest" means. Running a
/// *different* build deliberately requires supplying its hash:
/// `INITRAMFS=<name> INITRAMFS_SHA256=<hex>` (and `BZIMAGE_SHA256=<hex>` /
/// `KERNEL=<name>` if the kernel changes too) — the check never silently
/// accepts a drifted file.
const PINNED_BZIMAGE_SHA256: &str =
    "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41";
const PINNED_INITRAMFS_SHA256: &str =
    "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2";

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .map(|v| v.parse().unwrap_or_else(|_| panic!("{key} is a u64")))
        .unwrap_or(default)
}

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn artifact(name: &str) -> Option<Vec<u8>> {
    for p in [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return Some(bytes);
        }
    }
    None
}

/// Load a guest artifact or fail loudly — a missing image is never a vacuous
/// (silently skipped) gate.
fn require_artifact(name: &str) -> Vec<u8> {
    artifact(name).unwrap_or_else(|| {
        panic!(
            "guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux — build it on the \
             box (`make -C harmony-linux fetch && make -C harmony-linux/linux postgres-image`) or point \
             KERNEL/INITRAMFS at staged files"
        )
    })
}

/// Verify a loaded guest artifact against its pinned content hash (hm-xdp): a
/// mismatch is a loud refusal with both hashes quoted, never a silent run on a
/// drifted build.
fn verify_pin(name: &str, bytes: &[u8], expected_sha256: &str) {
    use sha2::{Digest, Sha256};
    let observed = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(
        observed, expected_sha256,
        "guest artifact `{name}` does not match its pinned content hash (hm-xdp: this gate \
         references images BY HASH, never a mutable path — the canonical box image drifted on \
         2026-07-09 and broke the draw-probe precondition on main). Stage the pinned PR-44 \
         build, or run a different build DELIBERATELY via INITRAMFS=<name> INITRAMFS_SHA256=<hex> \
         / BZIMAGE_SHA256=<hex> / KERNEL=<name>."
    );
}

/// Resolve one pinned image: default to the PR-44 pin, or accept a deliberate
/// override that MUST carry its own content hash (overriding the name without a
/// hash is a loud panic — never trust a mutable path). Mirrors the task-95
/// `live_dirty_remap` discipline exactly.
fn resolve_pinned(
    name_var: &str,
    default_name: &str,
    hash_var: &str,
    default_hash: &str,
) -> Vec<u8> {
    let (name, pin) = match (std::env::var(name_var).ok(), std::env::var(hash_var).ok()) {
        (None, None) => (default_name.to_string(), default_hash.to_string()),
        (Some(n), Some(h)) => (n, h),
        (None, Some(h)) => (default_name.to_string(), h),
        (Some(n), None) => panic!(
            "{name_var}={n} without {hash_var} — overriding the image requires supplying its \
             content hash (hm-xdp: never trust a mutable path)"
        ),
    };
    let bytes = require_artifact(&name);
    verify_pin(&name, &bytes, &pin);
    bytes
}

/// The pinned (kernel, initramfs) pair, each verified against its content hash
/// before a byte of it reaches the guest — the drift gate (hm-xdp) that makes
/// `REQUIRE_DRAWS` meaningful again.
fn guest_images() -> (Vec<u8>, Vec<u8>) {
    let kernel = resolve_pinned("KERNEL", "bzImage", "BZIMAGE_SHA256", PINNED_BZIMAGE_SHA256);
    let initramfs = resolve_pinned(
        "INITRAMFS",
        "initramfs-postgres.cpio.gz",
        "INITRAMFS_SHA256",
        PINNED_INITRAMFS_SHA256,
    );
    (kernel, initramfs)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Drive the live guest until `marker` appears on the serial, streaming new
/// serial bytes to stderr (mirrors the campaign-runner box mode's drive; scans only
/// the fresh tail with a marker-1 overlap).
fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend<A = X86>>>, marker: &[u8]) -> Result<u64, String> {
    let stderr = std::io::stderr();
    let mut printed = vmm.serial().len();
    let overlap = marker.len().saturating_sub(1);
    let mut scan_from = printed.saturating_sub(overlap);
    let mut steps = 0u64;
    while steps < MAX_BOOT_STEPS {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                return Err(format!(
                    "guest reached a terminal ({r:?}) at step {steps} before the readiness marker"
                ));
            }
            // A cooperating-SDK stop (task 73) — an assertion violation — is a
            // premature stop here, just like a terminal.
            Ok(Step::SdkStop) => {
                return Err(format!(
                    "guest hit an SDK stop (assertion) at step {steps} before the readiness marker"
                ));
            }
            Err(e) => return Err(format!("step error at {steps}: {e}")),
        }
        steps += 1;
        let serial = vmm.serial();
        if serial.len() > printed {
            let mut h = stderr.lock();
            let _ = h.write_all(&serial[printed..]);
            let _ = h.flush();
            printed = serial.len();
            if contains(&serial[scan_from..], marker) {
                return Ok(steps);
            }
            scan_from = serial.len().saturating_sub(overlap);
        }
    }
    Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
}

#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built Postgres image"]
fn task68_box_gates_measured_depth_eviction_roundtrip_composed_reproducer() {
    // Preconditions — every missing one is a loud failure, never vacuous.
    assert!(
        std::path::Path::new("/dev/kvm").exists(),
        "/dev/kvm absent — run on the determinism box with the LOADED patched KVM modules"
    );
    let report = vmm_core::vendor::x86::hostassert::report();
    if let Some(bad) = report.iter().find(|o| !o.pass) {
        panic!(
            "host is not the det-cfl-v1 baseline (first failing assertion: {} expected {}, \
             observed {})",
            bad.key, bad.expected, bad.actual
        );
    }
    // Pinned by content hash (hm-xdp): a drifted image is a loud refusal here,
    // not a silent mis-probe of the draw windows.
    let (kernel, initramfs) = guest_images();
    let marker = std::env::var("READY_MARKER")
        .unwrap_or_else(|_| "database system is ready to accept connections".into());

    // Boot the live guest to the readiness marker (the one workload-aware
    // step — the chain seals land mid-workload, post-readiness).
    let mut live = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        GUEST_RAM_LEN,
        CMDLINE,
        BOOT_SEED,
    )
    .expect("boot_linux_selected (patched)");
    eprintln!("[live_materialization] booting to the readiness marker {marker:?} …");
    let steps = drive_to_marker(&mut live, marker.as_bytes()).expect("reach readiness");
    eprintln!("\n[live_materialization] readiness at step {steps}; starting the chain protocol.");

    let factory: VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            CMDLINE,
            BOOT_SEED,
        )
    });
    let mut server = ControlServer::new(live, factory);

    // Default HOPS=4, not 3 (hm-xdp): on the pinned PR-44 image the Postgres
    // uuid workload's first entropy draw lands ~6 M v-ns past the base, just
    // beyond three 2 M-v-ns hop windows — so `HOPS=3` measures no hop draw (only
    // the tail draws) and the task-78 `REQUIRE_DRAWS` precondition fails even on
    // the correct image. HOPS=4 extends the chain one hop so a compose-collapsed
    // hop window covers that drawing span (the same span the tail already proves
    // draws) — the count the task-78 box gate was PROVEN green with. This raises
    // the chain LENGTH, not the window WIDTH (`HOP_DELTA_VNS`): the draw is still
    // a measured two-seed probe, and image drift still fails closed by hash.
    let cfg_hops = env_u64("HOPS", 4) as usize;
    let cfg = MaterializeConfig {
        // The same non-boot chain seed shape the task-58 box sweep branches.
        seed: env_u64("CHAIN_SEED", 0x0028_C0FF_EE5E_EDC0 ^ 0x9E37_79B9_7F4A_7C15),
        hops: cfg_hops,
        hop_delta: env_u64("HOP_DELTA_VNS", 2_000_000),
        tail_delta: env_u64("TAIL_DELTA_VNS", 1_000_000),
        // Postgres is interrupt-driven; generous retry past non-sealable
        // boundaries (mirrors the campaign-runner box mode).
        snapshot_retry_step: 1_000_000,
        snapshot_max_attempts: 100_000,
    };
    let initial = EnvSpec::Seeded {
        seed: BOOT_SEED,
        policy: FaultPolicy::none(),
    };
    let (served, report) = run_session(&mut server, move |stream| {
        campaign_runner::materialize_client(stream, initial, cfg)
    });
    served.expect("server session");
    let report = report.expect("the chain protocol (a MachineError here is a live finding)");

    println!("\n[REPORT] task-68 live_materialization (box)");
    print!("{}", render_materialize_table(&report));

    // Task-78 assertions: the reproducer is reseed-aware (one marker per
    // branch leg: the chain's hops plus the tail leg), and — unless explicitly
    // waived — the tail window actually drew entropy, so the bit-identity
    // gates exercised the reseed-marker machinery, not a draw-free span.
    let decoded = explorer::AdapterEnv::decode(&report.bug_env).expect("adapter blob");
    assert_eq!(
        decoded.spec.reseeds().len(),
        cfg_hops + 1,
        "bug_env must carry every collapsed leg's reseed marker (hops + tail)"
    );
    if env_u64("REQUIRE_DRAWS", 1) == 1 {
        assert!(
            report.hop_draws.iter().any(|d| *d) && report.tail_draws,
            "the task-78 gate needs BOTH a draw inside a compose-collapsed hop window AND a \
             drawing tail window (probes: hops {:?}, tail {}) — the tail is what gate (c)'s \
             reproducer fold replays across its trailing reseed point. Raise \
             HOPS/HOP_DELTA_VNS so a hop window covers a drawing span, move READY_MARKER \
             into the Postgres workload loop, use an entropy-drawing payload, or set \
             REQUIRE_DRAWS=0 to accept the draw-free (pre-task-78) shape",
            report.hop_draws,
            report.tail_draws
        );
    }

    let failures = verify_materialize(&report, Some(TASK63_BASELINE_PPM));
    if failures.is_empty() {
        println!(
            "[REPORT] GATES PASS: (a) parent-rooted depth beats the task-63 baseline; (b) \
             eviction round-trip bit-identical (folded + from-genesis worst case); (c) composed \
             reproducer replays with identical stop + state_hash."
        );
    } else {
        println!("[REPORT] GATES FAILED:");
        for f in &failures {
            println!("[REPORT]   - {f}");
        }
    }
    assert!(
        failures.is_empty(),
        "task-68 box gates failed (see [REPORT])"
    );
}
