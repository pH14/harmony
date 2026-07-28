// SPDX-License-Identifier: AGPL-3.0-or-later
//! **The hm-xkh5 draw-probe positive/negative pair (box-only, `#[ignore]`)** —
//! the regression for the tasks/167 probe fix, on the PRODUCTION probe
//! ([`campaign_runner::materialize_client`] → `run_materialize`, over the real
//! socket + codec):
//!
//! - **Negative arm (Postgres, the pr44 pinned pair):** the marker timeline
//!   (`postgres_baseline_marker_timeline`) proves this guest draws NO seeded
//!   entropy anywhere in the Postgres phase (hm-2nt; re-measured in the
//!   tasks/167 lane), so the fixed probe MUST NOT fire on any window. Before
//!   the fix this exact configuration reported `hops [f,f,f,true]` +
//!   `tail_draws = true` — the hm-xkh5 false positive (the trailing-reseed
//!   marker's exact-count arrival clamp freezing the guest at a different
//!   micro-position than the plain leg's natural stop; see the chunk-diff in
//!   `live_draw_probe_diagnosis.rs` and `docs/history/IMPLEMENTATION-task167.md`)
//!   — so this arm is the fail-before direction, red on the unfixed probe.
//! - **Positive arm (the `/dev/harmony` bridge guest):** the first workload
//!   that genuinely draws seeded entropy on demand (`BRIDGE_ENTROPY_RAW` at
//!   ~113.188 M v-ns and `BRIDGE_ENTROPY_LIB` at ~113.205 M v-ns, measured by
//!   the timeline instrument on this image). A chain based at `BRIDGE_LAUNCH`
//!   with windows covering that span MUST probe a draw — proving the settled
//!   comparison did not blind the probe to real draws.
//!
//! Run (per `docs/BOX-PINNING.md`, on the leased core):
//!
//! ```sh
//! taskset -c <core> cargo test --release -p campaign-runner \
//!     --test live_draw_probe_pair -- --ignored --nocapture --test-threads=1
//! ```

#![cfg(all(target_os = "linux", target_arch = "x86_64"))]

use std::io::Write;

use campaign_runner::materialize::{
    MaterializeConfig, render_materialize_table, verify_materialize,
};
use campaign_runner::{materialize_client, run_session};
use environment::{EnvSpec, FaultPolicy};
use vmm_backend::{Backend, X86};
use vmm_core::control::{ControlServer, VmmFactory};
use vmm_core::vendor::x86::bringup::{BackendKind, boot_linux_selected};
use vmm_core::vmm::{Step, Vmm};

/// The boot seed every live gate runs under.
const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
/// The chain seed the production gate branches (its default).
const CHAIN_SEED: u64 = BOOT_SEED ^ 0x9E37_79B9_7F4A_7C15;
/// The determinism command line (identical to the other live gates).
const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                       lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
/// Safety cap on the boot-to-marker drive.
const MAX_BOOT_STEPS: u64 = 50_000_000_000;

/// The pr44 Postgres pair (the gate default; `live_materialization.rs`
/// `BASELINES[pr44]`), pinned by content hash per hm-xdp.
const PG_KERNEL: &str = "bzImage";
const PG_KERNEL_SHA256: &str = "f06a34a79010a8f2cc8226dc629cc8fb049740016f035f53e3f2e53d9a30dd41";
const PG_INITRAMFS: &str = "initramfs-postgres.cpio.gz";
const PG_INITRAMFS_SHA256: &str =
    "3c4a7f2f0db4b59aaf4dee55d43a42c57fc0d10ac25441de88128c61be0778c2";
const PG_READY: &str = "database system is ready to accept connections";

/// The bridge pair (tasks/157): the manifest-pinned char-device kernel and the
/// bridge-probe initramfs, both by content hash.
const BR_KERNEL: &str = "bzImage-bridge";
const BR_KERNEL_SHA256: &str = "91b092c56b18df883d3289bafa536e12ab5227dc94235500f6f634c9e2d89c7b";
const BR_INITRAMFS: &str = "initramfs-bridge.cpio.gz";
const BR_INITRAMFS_SHA256: &str =
    "37feabf039210ef5f804b517f9bb6616e307e3e888468d527ea7de4c00dbae59";
/// Base the bridge chain where the probe script starts: the draws land ~100 k
/// v-ns later, inside the deep window / tail of the layout below.
const BR_READY: &str = "BRIDGE_LAUNCH";

fn repo_root() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn require_artifact(name: &str) -> Vec<u8> {
    for p in [
        repo_root().join("harmony-linux/build").join(name),
        repo_root().join("harmony-linux/linux").join(name),
    ] {
        if let Ok(bytes) = std::fs::read(&p) {
            return bytes;
        }
    }
    panic!("guest artifact `{name}` not found in harmony-linux/build or harmony-linux/linux");
}

fn verify_pin(name: &str, bytes: &[u8], expected_sha256: &str) {
    use sha2::{Digest, Sha256};
    let observed = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(
        observed, expected_sha256,
        "guest artifact `{name}` does not match its pinned content hash (hm-xdp)"
    );
}

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

/// Drive the live guest until `marker` appears on the serial (mirrors the gate).
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
                    "guest terminal ({r:?}) at step {steps} before readiness"
                ));
            }
            Ok(Step::SdkStop) => {
                return Err(format!("guest SDK stop at step {steps} before readiness"));
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

/// Boot `kernel`/`initramfs`, drive to `marker`, wrap in a `ControlServer`, and
/// run the production chain protocol under `cfg`. Returns the report.
fn chain_report(
    kernel: Vec<u8>,
    initramfs: Vec<u8>,
    ram_len: usize,
    marker: &str,
    cfg: MaterializeConfig,
) -> campaign_runner::materialize::MaterializeReport {
    let mut live = boot_linux_selected(
        BackendKind::Patched,
        &kernel,
        &initramfs,
        ram_len,
        CMDLINE,
        BOOT_SEED,
    )
    .expect("boot_linux_selected (patched)");
    eprintln!("[PAIR] booting to {marker:?} …");
    let steps = drive_to_marker(&mut live, marker.as_bytes()).expect("reach readiness");
    eprintln!("\n[PAIR] readiness at step {steps}; running the chain protocol.");
    let factory: VmmFactory<Box<dyn Backend<A = X86>>> = Box::new(move || {
        boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            ram_len,
            CMDLINE,
            BOOT_SEED,
        )
    });
    let mut server = ControlServer::new(live, factory);
    let initial = EnvSpec::Seeded {
        seed: BOOT_SEED,
        policy: FaultPolicy::none(),
    };
    let (served, report) = run_session(&mut server, move |stream| {
        materialize_client(stream, initial, cfg)
    });
    served.expect("server session");
    report.expect("the chain protocol (a MachineError here is a live finding)")
}

/// **Negative arm.** The pr44 Postgres guest draws nothing in the Postgres
/// phase (measured — the marker timeline), so the fixed probe must be quiet on
/// every window of the production gate's own default layout. Red before the
/// tasks/167 fix: this exact layout reported `hops [f,f,f,true]` + tail.
#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built Postgres image"]
fn fixed_probe_is_quiet_on_the_draw_free_postgres_guest() {
    require_box_host();
    let kernel = require_artifact(PG_KERNEL);
    verify_pin(PG_KERNEL, &kernel, PG_KERNEL_SHA256);
    let initramfs = require_artifact(PG_INITRAMFS);
    verify_pin(PG_INITRAMFS, &initramfs, PG_INITRAMFS_SHA256);
    // The production gate's pr44 defaults, byte-for-byte.
    let cfg = MaterializeConfig {
        seed: CHAIN_SEED,
        hops: 4,
        hop_delta: 2_000_000,
        tail_delta: 1_000_000,
        snapshot_retry_step: 1_000_000,
        snapshot_max_attempts: 100_000,
    };
    let report = chain_report(kernel, initramfs, 2 << 30, PG_READY, cfg);
    print!("{}", render_materialize_table(&report));
    // The chain's substantive gates must still hold (the fix touched only the
    // probe's comparison point, never the fold or the reproducer).
    let failures = verify_materialize(&report, None);
    assert!(failures.is_empty(), "chain gates failed: {failures:?}");
    assert!(
        report.hop_draws.iter().all(|d| !d) && !report.tail_draws,
        "the draw-free Postgres guest must probe quiet on every window (the marker timeline \
         shows the seeded stream never moves in the Postgres phase — hm-2nt / tasks/167); a \
         firing probe here is the hm-xkh5 arrival-clamp false positive: hops {:?}, tail {}",
        report.hop_draws,
        report.tail_draws
    );
}

/// **Positive arm.** The bridge guest draws seeded entropy on demand
/// (`fuzz_get_random` over `/dev/harmony`); a chain based at `BRIDGE_LAUNCH`
/// whose span covers the measured draw burst must probe a draw somewhere —
/// the settled comparison must not blind the probe to genuine draws.
#[test]
#[ignore = "box-only: needs loaded patched KVM + det-cfl-v1 host + the built bridge image"]
fn fixed_probe_fires_on_the_genuinely_drawing_bridge_guest() {
    require_box_host();
    let kernel = require_artifact(BR_KERNEL);
    verify_pin(BR_KERNEL, &kernel, BR_KERNEL_SHA256);
    let initramfs = require_artifact(BR_INITRAMFS);
    verify_pin(BR_INITRAMFS, &initramfs, BR_INITRAMFS_SHA256);
    // Measured layout (the timeline instrument on this image, tasks/167 lane):
    // BRIDGE_LAUNCH at ~113.085 M v-ns; draws at ~113.188 M and ~113.205 M;
    // the guest halts ~113.28 M + serial tail. Three 32 k-v-ns hops from the
    // launch marker put the draw burst in the deep window / tail; the small
    // retry step keeps seal nudges from blowing past the halt.
    let cfg = MaterializeConfig {
        seed: CHAIN_SEED,
        hops: 3,
        hop_delta: 32_000,
        tail_delta: 40_000,
        snapshot_retry_step: 5_000,
        snapshot_max_attempts: 1_000,
    };
    let report = chain_report(kernel, initramfs, 512 << 20, BR_READY, cfg);
    print!("{}", render_materialize_table(&report));
    // The fold gates across genuinely-drawing collapsed windows — exactly the
    // task-78 property, exercised for real. A failure here is a live splice
    // finding, not a probe artifact.
    let failures = verify_materialize(&report, None);
    assert!(failures.is_empty(), "chain gates failed: {failures:?}");
    assert!(
        report.hop_draws == [false, false, true] && report.tail_draws,
        "the bridge guest draws on demand inside this chain's span (measured: raw at \
         ~113.188 M, lib at ~113.205 M v-ns): the fixed probe must fire on exactly the deep \
         hop AND the tail — the full measured pattern (lane-record table), not a \
         species-partial any(hop)||tail that a regression could pass while the production \
         precondition any(hop)&&tail never fires: hops {:?}, tail {}",
        report.hop_draws,
        report.tail_draws
    );
}
