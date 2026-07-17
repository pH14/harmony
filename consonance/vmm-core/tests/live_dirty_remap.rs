// SPDX-License-Identifier: AGPL-3.0-or-later
//! **Task-95 M2 box gates (a0)/(a)/(b) + the (d) numbers** — `#![cfg(target_os =
//! "linux")]` **and `#[ignore]`**: needs real + LOADED patched KVM, the
//! det-cfl-v1 host, and the built Postgres image. Gate (c) — nothing regresses —
//! is the existing `seal_rate_sweep.rs` and campaign-runner `live_materialization.rs`
//! suites, run unchanged alongside this file.
//!
//! - **(a0) tracking is inert** — same seed, dirty logging enabled (the new
//!   default) vs a `flags: 0` backend (`set_dirty_log_enabled(false)`), no seal
//!   taken → bit-identical `state_hash` at the same V-time stop. Write-protect
//!   faults are host-side and must not perturb the guest-observable execution or
//!   the Moment count.
//! - **(a) capture A/B** — two same-seed runs to the same seal schedule: the
//!   tracked run's second seal captures via harvested `snapshot_derive`
//!   (`chain_len == 2`, asserted — a silent fallback would make this gate
//!   vacuous), the `flags: 0` run's via `snapshot_base` (`chain_len == 1`);
//!   `replay`ing each and hashing must agree bit-for-bit — the derived image +
//!   `vm_state` are byte-identical to the full scan's (to hash strength, over
//!   the whole restored state).
//! - **(b) restore A/B** — branch the same (snapshot, env) under
//!   `RestoreMode::Memcpy` and `RestoreMode::Remap` (the mapping **is** the
//!   memslot backing — `ram_backing_is_snapshot()` asserted) and run the same
//!   suffix → identical stop + `state_hash`.
//! - **(d)** — every `[GATE-D]` line is a number for `docs/history/IMPLEMENTATION-task95.md`:
//!   base-seal vs derive-seal wall time, and memcpy vs remap branch wall time.
//!
//! Run (per `docs/BOX-PINNING.md` — lease a core via `box-window.sh`; serialize
//! with other frontier gates):
//!
//! ```sh
//! taskset -c 2 timeout 7200 cargo test -p vmm-core --release --test live_dirty_remap \
//!     -- --ignored --nocapture --test-threads=1 2>&1 | tee /tmp/live_dirty_remap.log
//! ```
//!
//! **Images are pinned by content hash** (hm-xdp): the harness refuses to run
//! on a bzImage/initramfs whose sha256 differs from the pinned task-78-proven
//! pair — stage that build (e.g. from the box's `/root/harmony-pr44/guest/build`)
//! or deliberately override with `INITRAMFS=<name> INITRAMFS_SHA256=<hex>`
//! (+ `BZIMAGE_SHA256=<hex>`). Verify before staging:
//! `sha256sum guest/build/{bzImage,initramfs-postgres.cpio.gz}` against the
//! `PINNED_*` constants below.
//!
//! Knobs: `DR_RUN_VNS` (V-time the guest runs before the first seal, default
//! 20 000 000), `DR_DELTA_VNS` (V-time between the two seals / past the branch,
//! default 5 000 000), `DR_SNAP_STEP` (retry step past a non-sealable boundary,
//! default 1 000 000), `BOOT_CMDLINE`, `INITRAMFS` (+ the hash pins above).
//!
//! **Box-safety (CRITICAL).** Stock KVM = 1396736; ALWAYS leave the box on
//! stock + verified after the run: `pkill -9 -f live_dirty_remap` FIRST
//! (separate ssh call) → wait `lsmod | grep '^kvm_intel'` users=0 → `rmmod
//! kvm_intel kvm; modprobe kvm; modprobe kvm_intel` → verify size 1396736 on a
//! FRESH connection.

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::time::Duration;

use control_proto::{
    HashScope, Moment, Reply, Reproducer, Request, SnapId, StopConditions, StopMask, StopReason,
};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, RemapVmmFactory, RestoreMode, VmmFactory, server_caps};
use vmm_core::vendor::x86::bringup::{boot_linux_patched_with_dirty_log, compose_restore_target};
use vmm_core::vendor::x86::contract_vclock_config;
use vmm_core::vmm::VtimeWiring;

type DynVmm = vmm_core::vmm::Vmm<Box<dyn Backend<A = X86>>>;

const GUEST_RAM_LEN: usize = 2 << 30;
const BASE_SEED: u64 = 0x0095_D127_5EED_C0DE;
const BRANCH_SEED: u64 = 0x0095_2EBA_5EED_0001;
const DEFAULT_CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable \
     no_timer_check lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";

/// **Pin-by-content-hash discipline (foreman ruling, bead hm-xdp).** Box gates
/// reference guest images by CONTENT HASH, never a mutable canonical path: the
/// 2026-07-09 rebuild of the box's canonical `initramfs-postgres.cpio.gz`
/// silently changed what default-knob gate runs were testing (and broke the
/// task-78 draw-probe precondition on main). These pins are the
/// task-78-proven pair (the `/root/harmony-pr44` build, Jul 2;
/// initramfs md5 `46b1461962b5b0f8aea98654f52a9ce5` for cross-reference).
/// Running a *different* build deliberately requires supplying its hash:
/// `INITRAMFS=<name> INITRAMFS_SHA256=<hex>` (and `BZIMAGE_SHA256=<hex>` if
/// the kernel changes too) — the check never silently accepts a drifted file.
const PINNED_BZIMAGE_SHA256: &str =
    "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41";
const PINNED_INITRAMFS_SHA256: &str =
    "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2";

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
        "/dev/kvm absent — run this `#[ignore]`d box gate on the determinism box with the \
         LOADED patched KVM modules, CPU-pinned per docs/BOX-PINNING.md."
    );
}

fn require_host_baseline() {
    let report = vmm_core::vendor::x86::hostassert::report();
    let mut all = true;
    for o in &report {
        if !o.pass {
            eprintln!(
                "[host-assert] FAIL {}: expected {}, observed {}",
                o.key, o.expected, o.actual
            );
        }
        all &= o.pass;
    }
    assert!(
        all,
        "host CPU is not the det-cfl-v1 baseline — run on the determinism box."
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

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify a loaded guest artifact against its pinned content hash (hm-xdp):
/// a mismatch is a loud refusal with both hashes quoted, never a silent run
/// on a drifted build.
fn verify_pin(name: &str, bytes: &[u8], expected_sha256: &str) {
    use sha2::{Digest, Sha256};
    let observed = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(
        observed, expected_sha256,
        "guest artifact `{name}` does not match its pinned content hash (hm-xdp: gates \
         reference images BY HASH, never a mutable path — the canonical box image has \
         drifted before). Stage the pinned build, or run a different build DELIBERATELY \
         via INITRAMFS=<name> INITRAMFS_SHA256=<hex> / BZIMAGE_SHA256=<hex>."
    );
}

fn guest_images() -> (Vec<u8>, Vec<u8>) {
    let kernel = require_artifact("bzImage");
    let kernel_pin =
        std::env::var("BZIMAGE_SHA256").unwrap_or_else(|_| PINNED_BZIMAGE_SHA256.to_string());
    verify_pin("bzImage", &kernel, &kernel_pin);

    let (name, pin) = match (
        std::env::var("INITRAMFS").ok(),
        std::env::var("INITRAMFS_SHA256").ok(),
    ) {
        (None, None) => (
            "initramfs-postgres.cpio.gz".to_string(),
            PINNED_INITRAMFS_SHA256.to_string(),
        ),
        (Some(n), Some(h)) => (n, h),
        (None, Some(h)) => ("initramfs-postgres.cpio.gz".to_string(), h),
        (Some(n), None) => panic!(
            "INITRAMFS={n} without INITRAMFS_SHA256 — overriding the image requires \
             supplying its content hash (hm-xdp: never trust a mutable path)"
        ),
    };
    let initramfs = require_artifact(&name);
    verify_pin(&name, &initramfs, &pin);
    (kernel, initramfs)
}

/// Boot the Postgres guest on patched KVM with dirty logging at its **default
/// (enabled)** — the production composition.
fn boot_pg(kernel: &[u8], initramfs: &[u8], seed: u64) -> DynVmm {
    boot_linux_patched_with_dirty_log(kernel, initramfs, GUEST_RAM_LEN, &cmdline(), seed, true)
        .expect("patched Linux boot — needs the LOADED patched KVM + perf + det-cfl-v1 host")
}

/// The **same shared composition** with dirty logging disabled (`flags: 0`) —
/// the A/B arm of gates (a0) and (a). One body, one knob: the two arms cannot
/// drift apart in wiring.
fn boot_pg_no_dirty_log(kernel: &[u8], initramfs: &[u8], seed: u64) -> DynVmm {
    boot_linux_patched_with_dirty_log(kernel, initramfs, GUEST_RAM_LEN, &cmdline(), seed, false)
        .expect("patched Linux boot (flags: 0 arm)")
}

fn call<B: Backend<A = X86>>(
    s: &mut ControlServer<B>,
    req: &Request,
) -> Result<Reply, control_proto::ControlError> {
    s.handle(req)
        .expect("session-fatal ServeError from the control server")
}

fn expect_ok<B: Backend<A = X86>>(s: &mut ControlServer<B>, req: &Request) -> Reply {
    match call(s, req) {
        Ok(reply) => reply,
        Err(e) => panic!("verb {req:?} answered a ControlError: {e:?}"),
    }
}

fn run_until<B: Backend<A = X86>>(s: &mut ControlServer<B>, deadline: u64) -> StopReason {
    match expect_ok(
        s,
        &Request::Run {
            until: StopConditions {
                deadline: Some(Moment(deadline)),
                on: StopMask::NONE,
            },
            resolve: None,
        },
    ) {
        Reply::Stop(stop) => stop,
        other => panic!("run answered {other:?}"),
    }
}

fn hash_whole<B: Backend<A = X86>>(s: &mut ControlServer<B>) -> [u8; 32] {
    match expect_ok(
        s,
        &Request::Hash {
            scope: HashScope::Whole,
        },
    ) {
        Reply::Hash(h) => h,
        other => panic!("hash answered {other:?}"),
    }
}

fn seeded_env(seed: u64) -> Reproducer {
    Reproducer {
        blob_version: EnvSpec::BLOB_VERSION,
        bytes: EnvSpec::Seeded {
            seed,
            policy: FaultPolicy::none(),
        }
        .encode(),
    }
}

/// Seal at the first sealable boundary at-or-after the current point (the
/// task-58 NotQuiescent retry), returning `(handle, boundary vtime, seal wall
/// time)` — the wall time covers only the successful `Snapshot` verb, so the
/// (d) numbers compare seal costs, not the retry walk.
fn seal_with_retry<B: Backend<A = X86>>(
    s: &mut ControlServer<B>,
    start_vt: u64,
    step: u64,
) -> (SnapId, u64, Duration) {
    let mut vt = start_vt;
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        assert!(
            attempts < 100_000,
            "no snapshottable boundary within budget"
        );
        #[allow(clippy::disallowed_methods)] // test-only timing; never reaches state/hash
        let t0 = std::time::Instant::now();
        match call(s, &Request::Snapshot) {
            Ok(Reply::Snapshot { id, .. }) => return (id, vt, t0.elapsed()),
            Ok(other) => panic!("snapshot answered {other:?}"),
            Err(control_proto::ControlError::NotQuiescent) => {
                match run_until(s, vt.saturating_add(step)) {
                    StopReason::Deadline { vtime } => vt = vtime.0,
                    other => panic!("guest ended before a sealable boundary: {other:?}"),
                }
            }
            Err(e) => panic!("snapshot answered a ControlError: {e:?}"),
        }
    }
}

/// One (a)-gate arm: boot with `tracked` dirty logging, run to the shared seal
/// schedule (first seal at the first sealable boundary ≥ `run_vns`, second at
/// the first ≥ first + `delta`), then `replay` the second seal and hash.
/// Returns (chain lengths, boundary vtimes, seal durations, post-replay hash).
#[allow(clippy::type_complexity)]
fn capture_arm(
    kernel: &[u8],
    initramfs: &[u8],
    tracked: bool,
) -> ((u32, u32), (u64, u64), (Duration, Duration), [u8; 32]) {
    let live = if tracked {
        boot_pg(kernel, initramfs, BASE_SEED)
    } else {
        boot_pg_no_dirty_log(kernel, initramfs, BASE_SEED)
    };
    let (fk, fi) = (kernel.to_vec(), initramfs.to_vec());
    let tracked_factory = tracked;
    let factory: VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move || {
        Ok(if tracked_factory {
            boot_pg(&fk, &fi, BASE_SEED)
        } else {
            boot_pg_no_dirty_log(&fk, &fi, BASE_SEED)
        })
    });
    let mut s = ControlServer::new(live, factory);
    assert_eq!(
        expect_ok(&mut s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );

    let run_vns = env_u64("DR_RUN_VNS", 20_000_000);
    let delta = env_u64("DR_DELTA_VNS", 5_000_000);
    let step = env_u64("DR_SNAP_STEP", 1_000_000);

    let vt0 = match run_until(&mut s, run_vns) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("run to the first seal point ended early: {other:?}"),
    };
    let (first, vt1, d1) = seal_with_retry(&mut s, vt0, step);
    let vt1b = match run_until(&mut s, vt1.saturating_add(delta)) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("run between seals ended early: {other:?}"),
    };
    let (second, vt2, d2) = seal_with_retry(&mut s, vt1b, step);

    let chains = (
        s.snapshot_chain_len(first).expect("first handle live"),
        s.snapshot_chain_len(second).expect("second handle live"),
    );

    // Replay the second seal and hash the restored whole state: two arms whose
    // second seals stored identical bytes (image + vm_state + devices) hash
    // identically here; a derive under-capture would diverge this hash.
    assert_eq!(expect_ok(&mut s, &Request::Replay(second)), Reply::Unit);
    let h = hash_whole(&mut s);
    ((chains), (vt1, vt2), (d1, d2), h)
}

/// Gate (a0): dirty logging enabled vs `flags: 0`, same seed, **no seal taken**
/// → bit-identical `state_hash` at the same V-time stop.
#[test]
#[ignore = "box-only task-95 gate (LOADED patched KVM + built Postgres image + det-cfl-v1 host); \
            run per docs/BOX-PINNING.md"]
fn a0_dirty_logging_is_guest_inert() {
    require_kvm();
    require_host_baseline();
    let (kernel, initramfs) = guest_images();
    let deadline = env_u64("DR_RUN_VNS", 20_000_000);

    // Sequential arms: the box allows one open perf work counter at a time.
    let run_arm = |vmm: DynVmm| -> (u64, [u8; 32]) {
        let factory: VmmFactory<Box<dyn Backend<A = X86>>> =
            Box::new(|| panic!("a0 never restores; the factory must not be called"));
        let mut s = ControlServer::new(vmm, factory);
        assert_eq!(
            expect_ok(&mut s, &Request::Hello(server_caps())),
            Reply::Hello(server_caps())
        );
        let vt = match run_until(&mut s, deadline) {
            StopReason::Deadline { vtime } => vtime.0,
            other => panic!("a0 run ended before the deadline: {other:?}"),
        };
        (vt, hash_whole(&mut s))
    };

    let (vt_on, h_on) = run_arm(boot_pg(&kernel, &initramfs, BASE_SEED));
    let (vt_off, h_off) = run_arm(boot_pg_no_dirty_log(&kernel, &initramfs, BASE_SEED));

    eprintln!(
        "[a0] logging-on:  vt={vt_on} hash={}\n[a0] logging-off: vt={vt_off} hash={}",
        hex(&h_on),
        hex(&h_off)
    );
    assert_eq!(vt_on, vt_off, "(a0) the Moment count must not move");
    assert_eq!(h_on, h_off, "(a0) dirty logging must be guest-inert");
}

/// Gate (a): harvested-derive capture vs full-scan capture of the same state —
/// byte-identical (via replay + whole-state hash), with the derive path
/// **proven engaged** (chain_len 2) and the (d) seal-cost numbers printed.
#[test]
#[ignore = "box-only task-95 gate (LOADED patched KVM + built Postgres image + det-cfl-v1 host); \
            run per docs/BOX-PINNING.md"]
fn a_harvested_derive_matches_full_scan_capture() {
    require_kvm();
    require_host_baseline();
    let (kernel, initramfs) = guest_images();

    let (chains_t, vts_t, durs_t, h_t) = capture_arm(&kernel, &initramfs, true);
    let (chains_f, vts_f, durs_f, h_f) = capture_arm(&kernel, &initramfs, false);

    eprintln!(
        "[a] tracked:  chains={chains_t:?} seal-vts={vts_t:?} hash={}",
        hex(&h_t)
    );
    eprintln!(
        "[a] flags:0   chains={chains_f:?} seal-vts={vts_f:?} hash={}",
        hex(&h_f)
    );
    eprintln!(
        "[GATE-D] base_seal_ms={} derive_seal_ms={} full_scan_second_seal_ms={}",
        durs_t.0.as_millis(),
        durs_t.1.as_millis(),
        durs_f.1.as_millis()
    );

    // The two same-seed runs must have sealed at the same boundaries — the
    // precondition for "the same state" (a0 already proved the knob is inert).
    assert_eq!(vts_t, vts_f, "(a) the two arms sealed at different Moments");
    assert_eq!(chains_t.0, 1, "(a) first tracked seal is the base");
    assert_eq!(
        chains_t.1, 2,
        "(a) the tracked second seal must DERIVE — a silent fallback makes this gate vacuous"
    );
    assert_eq!(chains_f, (1, 1), "(a) the flags:0 arm must full-scan");
    assert_eq!(
        h_t, h_f,
        "(a) derived capture and full-scan capture must restore to identical state"
    );
}

/// Gate (b): the same (snapshot, env, suffix) under `Memcpy` and `Remap` →
/// identical stop + `state_hash`, with the remap arm proven mapping-backed,
/// plus the (d) restore-cost numbers.
#[test]
#[ignore = "box-only task-95 gate (LOADED patched KVM + built Postgres image + det-cfl-v1 host); \
            run per docs/BOX-PINNING.md"]
fn b_remap_and_memcpy_restores_agree() {
    require_kvm();
    require_host_baseline();
    let (kernel, initramfs) = guest_images();

    let live = boot_pg(&kernel, &initramfs, BASE_SEED);
    let (fk, fi) = (kernel.clone(), initramfs.clone());
    let factory: VmmFactory<Box<dyn Backend<A = X86>>> =
        Box::new(move || Ok(boot_pg(&fk, &fi, BASE_SEED)));
    let mut s = ControlServer::new(live, factory);
    // The remap factory mirrors the composition minus the boot-image load: the
    // mapping is the RAM; `restore_vm_state` supplies the register file.
    let remap: RemapVmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move |mapping| {
        let backend: Box<dyn Backend<A = X86>> = Box::new(vmm_backend::PatchedKvmBackend::new()?);
        let mut v = compose_restore_target(backend, mapping, true)?;
        let work = Box::new(vmm_core::vendor::x86::work_perf::PerfWorkCounter::open()?);
        v.wire_vtime(VtimeWiring::new(contract_vclock_config(), work, BASE_SEED)?);
        Ok(v)
    });
    s.set_remap_factory(remap);
    assert_eq!(
        expect_ok(&mut s, &Request::Hello(server_caps())),
        Reply::Hello(server_caps())
    );

    let run_vns = env_u64("DR_RUN_VNS", 20_000_000);
    let delta = env_u64("DR_DELTA_VNS", 5_000_000);
    let step = env_u64("DR_SNAP_STEP", 1_000_000);
    let vt0 = match run_until(&mut s, run_vns) {
        StopReason::Deadline { vtime } => vtime.0,
        other => panic!("run to the seal point ended early: {other:?}"),
    };
    let (snap, vt1, _) = seal_with_retry(&mut s, vt0, step);
    eprintln!("[b] sealed at V-time {vt1}");

    let arm = |s: &mut ControlServer<Box<dyn Backend<A = X86>>>,
               mode: RestoreMode|
     -> (bool, StopReason, [u8; 32], Duration) {
        s.set_restore_mode(mode);
        #[allow(clippy::disallowed_methods)] // test-only timing; never reaches state/hash
        let t0 = std::time::Instant::now();
        assert_eq!(
            expect_ok(
                s,
                &Request::Branch {
                    snap,
                    env: seeded_env(BRANCH_SEED)
                }
            ),
            Reply::Unit
        );
        let branch_wall = t0.elapsed();
        let mapping_backed = s
            .vmm()
            .expect("live after branch")
            .ram_backing_is_snapshot();
        let stop = run_until(s, vt1.saturating_add(delta));
        (mapping_backed, stop, hash_whole(s), branch_wall)
    };

    let (mb_memcpy, stop_memcpy, h_memcpy, wall_memcpy) = arm(&mut s, RestoreMode::Memcpy);
    let (mb_remap, stop_remap, h_remap, wall_remap) = arm(&mut s, RestoreMode::Remap);

    eprintln!(
        "[b] memcpy: stop={stop_memcpy:?} hash={}\n[b] remap:  stop={stop_remap:?} hash={}",
        hex(&h_memcpy),
        hex(&h_remap)
    );
    eprintln!(
        "[GATE-D] branch_memcpy_ms={} branch_remap_ms={}",
        wall_memcpy.as_millis(),
        wall_remap.as_millis()
    );

    assert!(!mb_memcpy, "(b) the memcpy arm must be owned-RAM backed");
    assert!(
        mb_remap,
        "(b) the remap arm's guest RAM must BE the materialized mapping"
    );
    assert_eq!(stop_memcpy, stop_remap, "(b) the two arms stopped apart");
    assert_eq!(h_memcpy, h_remap, "(b) memcpy and remap restores diverged");
}
