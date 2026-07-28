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
//! (bead `hm-xdp`) pins the gate by content hash and FAILS CLOSED on any drift,
//! quoting the expected-vs-found sha256, rather than silently mis-probing.
//!
//! **Approved baselines (`BASELINE`, hm-2nt).** An image is a gate baseline only
//! once it appears in [`BASELINES`] with the characteristics — content pins,
//! readiness marker, chain-window sizing — under which its draw-probe
//! precondition has actually been *measured* green on the box. `BASELINE=pr44`
//! (the default) is the task-78-proven PR-44 pair; `BASELINE=jul9` is the
//! 2026-07-09 rebuild, promoted from "drifted file" to a first-class baseline by
//! hm-2nt. Stage a baseline's build (e.g. from the box's
//! `/root/harmony-pr44/harmony-linux/build`) and verify with `sha256sum
//! harmony-linux/build/{bzImage,initramfs-postgres.cpio.gz}` against its pins, or run a
//! DIFFERENT build deliberately via `INITRAMFS=<name> INITRAMFS_SHA256=<hex>`
//! (+ `BZIMAGE_SHA256=<hex>` / `KERNEL=<name>` for the kernel).
//!
//! [`postgres_baseline_marker_timeline`] is the measuring instrument: it boots a
//! baseline and prints the V-time of every workload marker, which is how a new
//! image's `ready_marker` / window sizing are chosen (and re-checked) rather than
//! guessed.
//!
//! Knobs: `BASELINE` (default `pr44`), `HOPS`, `HOP_DELTA_VNS`, `TAIL_DELTA_VNS`
//! (each defaulting to the selected baseline's proven value), `CHAIN_SEED`,
//! `READY_MARKER` (default: the baseline's), `KERNEL`/`INITRAMFS` (filenames
//! under `harmony-linux/build` or `harmony-linux/linux`) with the
//! `BZIMAGE_SHA256`/`INITRAMFS_SHA256` pins above.
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

/// One **approved gate baseline** (hm-2nt): a pinned (kernel, initramfs) pair
/// *plus* the image characteristics the task-78 draw probe depends on. Both
/// halves are the baseline — a content pin alone says which bytes ran, not
/// whether the chain's windows land where entropy is actually drawn, and it was
/// exactly that second half going unrecorded that left the 2026-07-09 rebuild
/// unusable as a baseline.
struct Baseline {
    /// `BASELINE` selector value.
    id: &'static str,
    /// Kernel filename under `harmony-linux/build` or `harmony-linux/linux`.
    kernel: &'static str,
    /// The kernel's pinned sha256.
    kernel_sha256: &'static str,
    /// Initramfs filename under the same two directories.
    initramfs: &'static str,
    /// The initramfs' pinned sha256.
    initramfs_sha256: &'static str,
    /// Serial marker the chain's base is sealed at — where "the workload has
    /// begun" is for *this* image.
    ready_marker: &'static str,
    /// Chain length whose windows are measured to cover a drawing span from
    /// `ready_marker`.
    hops: u64,
    /// Per-hop window width in V-nanoseconds.
    hop_delta_vns: u64,
    /// Tail (gate-(c) "bug" leg) window width in V-nanoseconds.
    tail_delta_vns: u64,
    /// One line on where this baseline's numbers come from.
    provenance: &'static str,
}

/// **Pin-by-content-hash discipline (foreman ruling, beads `hm-xdp` / `hm-2nt`).**
/// This gate references the guest images by CONTENT HASH, never a mutable
/// canonical path: the 2026-07-09 rebuild of the box's canonical
/// `initramfs-postgres.cpio.gz` silently changed what default-knob gate runs
/// were testing — a mutation under main's gates that no path caught. Running a
/// build that is in no baseline requires supplying its hash explicitly
/// (`INITRAMFS=<name> INITRAMFS_SHA256=<hex>`, and `BZIMAGE_SHA256=<hex>` /
/// `KERNEL=<name>` if the kernel changes too) — the check never silently
/// accepts a drifted file.
///
/// `pr44` is the default and is byte-identical to the pins `vmm-core`'s task-95
/// `live_dirty_remap` gate enforces, so the two gates cannot drift apart on
/// which image "the Postgres guest" means.
const BASELINES: &[Baseline] = &[
    Baseline {
        id: "pr44",
        kernel: "bzImage",
        kernel_sha256: "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41",
        initramfs: "initramfs-postgres.cpio.gz",
        initramfs_sha256: "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2",
        // Postgres' own readiness log line. The first uuid draw lands ~6 M v-ns
        // later, so the chain needs a 4th hop to cover it — see `hops` below.
        ready_marker: "database system is ready to accept connections",
        // HOPS=4, not 3: on this image the uuid workload's first entropy draw
        // lands just beyond three 2 M-v-ns hop windows, so `HOPS=3` measures no
        // hop draw (only the tail draws) and `REQUIRE_DRAWS` fails on the
        // correct image. The 4th hop extends the chain LENGTH (not the window
        // WIDTH) so a compose-collapsed hop window covers that drawing span.
        hops: 4,
        hop_delta_vns: 2_000_000,
        tail_delta_vns: 1_000_000,
        provenance: "task-78 box gate (PR 44 build, Jul 2; initramfs md5 46b1461962b5b0f8aea98654f52a9ce5)",
    },
    Baseline {
        id: "jul9",
        // Same kernel bytes as pr44 — the 2026-07-09 rebuild moved the
        // initramfs only (verified by sha256 on the box's t81 checkout).
        kernel: "bzImage",
        kernel_sha256: "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41",
        // Deliberately NOT the canonical `initramfs-postgres.cpio.gz` name:
        // baselines coexist in `harmony-linux/build` under distinct filenames so
        // selecting one is `BASELINE=jul9`, never swapping files under a shared
        // name (which is how the 2026-07-09 drift happened in the first place).
        // Stage it as:
        //   cp <t81-checkout>/build/initramfs-postgres.cpio.gz \
        //      harmony-linux/build/initramfs-postgres-jul9.cpio.gz
        initramfs: "initramfs-postgres-jul9.cpio.gz",
        initramfs_sha256: "82395d189e3b2e0605b583cabe1035381921cedf0b6044c1ecb25ecb56a2880b",
        // Identical to pr44's, and that is the hm-2nt finding, not laziness:
        // `postgres_baseline_marker_timeline` measured both images and they run
        // the same program to within ~35 µs of V-time at every marker, with the
        // same (empty) draw map after early userspace. The bead's premise — that
        // THIS image's first entropy draw lands past the hop windows, so its
        // marker must move into the uuid loop — does not survive measurement:
        // neither image draws anywhere in the Postgres phase, so no marker
        // placement inside that phase changes the draw map. What broke on
        // 2026-07-09 was the then-default HOPS=3 (already corrected to 4 for
        // pr44), not the rebuild. See docs/history/IMPLEMENTATION-task157.md §3.
        ready_marker: "database system is ready to accept connections",
        hops: 4,
        hop_delta_vns: 2_000_000,
        tail_delta_vns: 1_000_000,
        provenance: "hm-2nt (2026-07-09 rebuild, initramfs md5 9860a065abc69d7e9c7144d7c2c37e2b)",
    },
];

/// The baseline named by `BASELINE` (default `pr44`) — an unknown id is a loud
/// refusal listing what IS approved, never a silent fallback to the default.
fn selected_baseline() -> &'static Baseline {
    let id = std::env::var("BASELINE").unwrap_or_else(|_| "pr44".into());
    BASELINES.iter().find(|b| b.id == id).unwrap_or_else(|| {
        let ids: Vec<&str> = BASELINES.iter().map(|b| b.id).collect();
        panic!("BASELINE={id} is not an approved gate baseline (approved: {ids:?})")
    })
}

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

/// The selected baseline's (kernel, initramfs) pair, each verified against its
/// content hash before a byte of it reaches the guest — the drift gate (hm-xdp)
/// that makes `REQUIRE_DRAWS` meaningful again.
fn guest_images(b: &Baseline) -> (Vec<u8>, Vec<u8>) {
    let kernel = resolve_pinned("KERNEL", b.kernel, "BZIMAGE_SHA256", b.kernel_sha256);
    let initramfs = resolve_pinned(
        "INITRAMFS",
        b.initramfs,
        "INITRAMFS_SHA256",
        b.initramfs_sha256,
    );
    (kernel, initramfs)
}

/// Boot the selected baseline on the patched backend at [`BOOT_SEED`] — the one
/// composition both the gate and the timeline probe use, so a timeline measured
/// here describes the VM the gate actually runs.
fn boot_baseline(kernel: &[u8], initramfs: &[u8]) -> Vmm<Box<dyn Backend<A = X86>>> {
    boot_linux_selected(
        BackendKind::Patched,
        kernel,
        initramfs,
        GUEST_RAM_LEN,
        CMDLINE,
        BOOT_SEED,
    )
    .expect("boot_linux_selected (patched)")
}

/// Refuse to run anywhere but the box, loudly (a missing precondition is never a
/// vacuous skip).
fn require_box_host() {
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
    require_box_host();
    // Pinned by content hash (hm-xdp): a drifted image is a loud refusal here,
    // not a silent mis-probe of the draw windows.
    let baseline = selected_baseline();
    let (kernel, initramfs) = guest_images(baseline);
    let marker = std::env::var("READY_MARKER").unwrap_or_else(|_| baseline.ready_marker.into());
    eprintln!(
        "[live_materialization] baseline {} ({}) — marker {:?}",
        baseline.id, baseline.provenance, marker
    );

    // Boot the live guest to the readiness marker (the one workload-aware
    // step — the chain seals land mid-workload, post-readiness).
    let mut live = boot_baseline(&kernel, &initramfs);
    eprintln!("[live_materialization] booting to the readiness marker {marker:?} …");
    let steps = drive_to_marker(&mut live, marker.as_bytes()).expect("reach readiness");
    eprintln!("\n[live_materialization] readiness at step {steps}; starting the chain protocol.");

    // The factory returns the Result rather than going through `boot_baseline`:
    // a re-boot failure mid-session is the server's error to report, not a panic.
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

    // Window sizing comes from the selected baseline (hm-2nt), because it is a
    // property of the IMAGE, not of the protocol: `pr44` bases at Postgres'
    // readiness line, ~6 M v-ns before the uuid workload's first entropy draw,
    // so it needs HOPS=4 for a compose-collapsed hop window to cover that
    // drawing span (`HOPS=3` measures no hop draw and `REQUIRE_DRAWS` fails on
    // the correct image). `jul9` bases at the workload marker itself, so hop 0
    // already covers draws. Overriding raises the chain LENGTH, not the window
    // WIDTH: the draw stays a measured two-seed probe and drift still fails
    // closed by hash.
    let cfg_hops = env_u64("HOPS", baseline.hops) as usize;
    let cfg = MaterializeConfig {
        // The same non-boot chain seed shape the task-58 box sweep branches.
        seed: env_u64("CHAIN_SEED", 0x0028_C0FF_EE5E_EDC0 ^ 0x9E37_79B9_7F4A_7C15),
        hops: cfg_hops,
        hop_delta: env_u64("HOP_DELTA_VNS", baseline.hop_delta_vns),
        tail_delta: env_u64("TAIL_DELTA_VNS", baseline.tail_delta_vns),
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
             reproducer fold replays across its trailing reseed point. Run \
             `postgres_baseline_marker_timeline` on this image FIRST: it prints where \
             the seeded stream actually moves, and on both Postgres baselines the \
             answer is `nowhere after early userspace` (hm-2nt) — so moving \
             READY_MARKER around inside the workload will NOT help, and the knob that \
             matters is HOPS (3 fails, 4 passes). Otherwise use an entropy-drawing \
             payload, or set REQUIRE_DRAWS=0 to accept the draw-free (pre-task-78) \
             shape",
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

/// The workload markers a Postgres baseline is characterized by, in the order
/// `pg-init.sh` + the task-42 workload emit them. `row|i|count|sum|` prefixes are
/// the workload's deterministic anchor (the uuid/timestamp tail is seed-derived,
/// so only the prefix is a stable marker).
const POSTGRES_MARKERS: &[&str] = &[
    "PG37: starting postgres",
    "database system is ready to accept connections",
    "PG37: workload begin",
    "row|1|1|1|",
    "row|2|2|3|",
    "row|5|5|15|",
    "row|10|10|55|",
    "row|20|20|210|",
    "PG37: workload end",
    "GUEST_READY",
];

/// Wire the doorbell channels **exactly as `ControlServer::new` does**, on a VM
/// the timeline probe drives itself.
///
/// This is not optional dressing: `Vmm::doorbell_service_offered` gates the
/// **Entropy** service on `sdk.is_some() || net.is_some()`, so a bare VM answers
/// a guest's entropy request with `UnknownService` while the gate's
/// server-wrapped VM services it from the seeded stream. Measure the bare VM and
/// you measure a *different guest*: one whose entropy consumers are being
/// refused. Every draw-related number this probe prints would then under-report
/// — which is exactly the trap this function exists to avoid.
fn wire_doorbell_channels(vmm: &mut Vmm<Box<dyn Backend<A = X86>>>) {
    let recorded = EnvSpec::Seeded {
        seed: vmm.entropy_state().unwrap_or(BOOT_SEED),
        policy: FaultPolicy::none(),
    };
    vmm.enable_sdk(recorded.materialize(), recorded.policy());
    vmm.enable_net();
}

/// The seeded-entropy stream position at this instant, as a short hex tag.
///
/// `Vmm::state_components()`'s `vtim:entropy` component is `SeededEntropy::
/// save_state()` digested — the PRNG's position. It changes **exactly** when the
/// guest consumes seeded entropy, which is the same condition the task-78
/// `hop_draws`/`tail_draws` probes measure by re-running a window with a
/// trailing reseed marker. Reading it directly costs one state digest instead of
/// two guest re-executions per window, so a whole image can be mapped in one
/// boot rather than one chain run per candidate window layout.
fn entropy_tag(vmm: &Vmm<Box<dyn Backend<A = X86>>>) -> String {
    vmm.state_components()
        .into_iter()
        .find(|(k, _)| *k == "vtim:entropy")
        .map(|(_, d)| d[..8].iter().map(|b| format!("{b:02x}")).collect())
        .unwrap_or_else(|| "unwired".to_string())
}

/// **hm-2nt's measuring instrument.** Boot a baseline once and print, for each
/// workload marker, its V-time *and* the seeded-entropy stream position — so a
/// new image's `ready_marker` and chain-window sizing are chosen from
/// measurement instead of guesswork. The gap this bead closed was that nobody
/// knew *where* the 2026-07-09 rebuild's first entropy draw landed, only that
/// the default windows missed it; the `draws` column answers that directly.
///
/// This is a characterization probe, not a gate: it asserts only that every
/// marker appears in order before the guest terminates (an image whose workload
/// never runs is a broken image, and that IS worth failing on). Its output is
/// what the `BASELINES` table's numbers are derived from.
///
/// ```sh
/// BASELINE=jul9 taskset -c 2 cargo test -p campaign-runner --test live_materialization \
///     -- --ignored --nocapture --test-threads=1 postgres_baseline_marker_timeline
/// ```
///
/// `MARKERS` (a `;`-separated list) overrides the marker set for a non-Postgres
/// image.
#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built Postgres image"]
fn postgres_baseline_marker_timeline() {
    require_box_host();
    let baseline = selected_baseline();
    let (kernel, initramfs) = guest_images(baseline);
    let markers: Vec<String> = match std::env::var("MARKERS") {
        Ok(s) => s
            .split(';')
            .filter(|m| !m.is_empty())
            .map(String::from)
            .collect(),
        Err(_) => POSTGRES_MARKERS.iter().map(|m| (*m).to_string()).collect(),
    };
    assert!(!markers.is_empty(), "MARKERS is empty — nothing to measure");

    eprintln!(
        "[timeline] baseline {} ({}) — {} markers",
        baseline.id,
        baseline.provenance,
        markers.len()
    );
    let mut vmm = boot_baseline(&kernel, &initramfs);
    wire_doorbell_channels(&mut vmm);

    // (marker, step, effective V-time ns, entropy-stream tag). Both are read at
    // the step the marker became visible on the serial: exact to a step
    // boundary, which is the same granularity the chain's `run_to` deadlines
    // land on.
    let mut hits: Vec<(String, u64, u64, String)> = Vec::with_capacity(markers.len());
    let mut next = 0usize;
    // Absolute index into the serial buffer that the *current* marker search
    // starts from — advanced past each hit so markers are matched strictly in
    // order, and so several markers landing in one chunk are all seen.
    let mut scan_from = 0usize;
    let mut printed = vmm.serial().len();
    let mut steps = 0u64;
    let mut terminal: Option<String> = None;
    let stderr = std::io::stderr();
    while steps < MAX_BOOT_STEPS && next < markers.len() {
        match vmm.step() {
            Ok(Step::Continued) => {}
            Ok(Step::Terminal(r)) => {
                terminal = Some(format!("{r:?}"));
                break;
            }
            Ok(Step::SdkStop) => {
                terminal = Some("SdkStop".into());
                break;
            }
            Err(e) => panic!("step error at {steps}: {e}"),
        }
        steps += 1;
        let serial_len = vmm.serial().len();
        if serial_len <= printed {
            continue;
        }
        {
            let mut h = stderr.lock();
            let _ = h.write_all(&vmm.serial()[printed..]);
            let _ = h.flush();
        }
        printed = serial_len;
        // Drain every marker that is now visible (a chunk can carry more than one).
        while next < markers.len() {
            let needle = markers[next].as_bytes();
            let Some(off) = vmm.serial()[scan_from..]
                .windows(needle.len())
                .position(|w| w == needle)
            else {
                break;
            };
            let vns = vmm
                .effective_vns()
                .expect("V-time is wired by boot_linux_selected");
            let tag = entropy_tag(&vmm);
            hits.push((markers[next].clone(), steps, vns, tag));
            scan_from += off + needle.len();
            next += 1;
        }
    }

    println!(
        "\n[TIMELINE] baseline {} — {}",
        baseline.id, baseline.provenance
    );
    println!(
        "[TIMELINE] kernel  {} sha256 {}",
        baseline.kernel, baseline.kernel_sha256
    );
    println!(
        "[TIMELINE] initramfs {} sha256 {}",
        baseline.initramfs, baseline.initramfs_sha256
    );
    println!(
        "[TIMELINE] {:<48} {:>12} {:>14} {:>12} {:>18} {:>6}",
        "marker", "step", "vtime_ns", "delta_ns", "entropy_state", "drew?"
    );
    let mut prev: Option<(u64, &str)> = None;
    for (m, s, v, tag) in &hits {
        let (delta, drew) = match prev {
            // "drew" = the seeded stream moved since the previous marker, i.e.
            // the guest consumed entropy somewhere in that interval. This is the
            // per-interval draw map the chain's window sizing needs.
            Some((pv, ptag)) => (v.saturating_sub(pv), if ptag == tag { "no" } else { "YES" }),
            None => (0, "-"),
        };
        println!("[TIMELINE] {m:<48} {s:>12} {v:>14} {delta:>12} {tag:>18} {drew:>6}");
        prev = Some((*v, tag));
    }
    if let Some(t) = &terminal {
        println!("[TIMELINE] terminal after {steps} steps: {t}");
    }
    // The verdict this probe exists for: the gate's chain runs
    // `hops * hop_delta + tail_delta` V-ns forward from the base marker, and
    // `REQUIRE_DRAWS` needs BOTH a hop window and the tail to land where the
    // guest actually draws. So compare the chain's span against the last marker
    // whose interval drew — not merely against the end of the workload, which is
    // what makes a chain "fit" on paper while its tail seals in a draw-free
    // shutdown.
    if let Some(base) = hits.iter().find(|(m, _, _, _)| *m == baseline.ready_marker) {
        let chain = baseline.hops * baseline.hop_delta_vns + baseline.tail_delta_vns;
        let last_draw = hits
            .windows(2)
            .filter(|w| w[0].3 != w[1].3)
            .map(|w| (w[1].0.clone(), w[1].2))
            .next_back();
        println!(
            "[TIMELINE] base {:?} at {} ns; the chain spans {} ns ({} hops x {} + {} tail) \
             ending at {} ns",
            baseline.ready_marker,
            base.2,
            chain,
            baseline.hops,
            baseline.hop_delta_vns,
            baseline.tail_delta_vns,
            base.2 + chain
        );
        match last_draw {
            Some((m, at)) => println!(
                "[TIMELINE] last drawing interval ends at {at} ns ({m:?}) => the chain's tail {}",
                if base.2 + chain <= at {
                    "lands INSIDE the drawing span"
                } else {
                    "OVERRUNS it (REQUIRE_DRAWS will fail on the tail)"
                }
            ),
            None => println!(
                "[TIMELINE] NO interval drew entropy — this image cannot satisfy REQUIRE_DRAWS \
                 at any window layout over these markers"
            ),
        }
    }

    assert_eq!(
        next,
        markers.len(),
        "only {}/{} markers appeared before the guest {} — this image's workload did not run to \
         completion (hits: {:?})",
        next,
        markers.len(),
        terminal.as_deref().unwrap_or("ran out of steps"),
        hits.iter()
            .map(|(m, _, _, _)| m.as_str())
            .collect::<Vec<_>>()
    );
}
