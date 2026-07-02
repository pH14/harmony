// SPDX-License-Identifier: AGPL-3.0-or-later
//! `conductor` — the task-58 close-the-loop demo binary.
//!
//! Runs the explorer's socket [`Machine`] against vmm-core's control-transport
//! server over a socketpair, driving the outer loop N steps: snapshot once,
//! branch across seeds, run, hash, replay-check, and print a run table with the
//! task-58 gate verdicts.
//!
//! Two modes:
//!
//! - **`mock`** (default, portable — macOS + Linux, no `/dev/kvm`): a scripted
//!   `MockBackend` guest. Proves the whole wire path end-to-end and that
//!   `branch(seed) → run → hash` is per-seed reproducible and cross-seed
//!   divergent, with no box.
//! - **`box`** (Linux + patched KVM + the built Postgres image + the det-cfl-v1
//!   host): boots the real Postgres workload via `boot_linux_selected`, seals a
//!   mid-workload snapshot, and runs the same sweep against the real guest —
//!   the milestone gate. Every missing precondition is a loud error, never a
//!   vacuous success.
//!
//! ```sh
//! # portable:
//! cargo run -p conductor -- mock --seeds 8 --runs 2
//! # box (per docs/BOX-PINNING.md; use your assigned core):
//! taskset -c <core> cargo run -p conductor --release -- box --seeds 8 --runs 2
//! ```

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use conductor::record::{
    RecordConfig, RecordReport, render_record_table, run_recording, verify_record,
    verify_store_reload,
};
use conductor::{SweepConfig, render_table, run_session, sweep_client, verify};
use environment::{EnvSpec, FaultPolicy};
use explorer::StreamId;
use runtrace::{RetentionPolicy, TraceStore};

#[derive(Parser)]
#[command(
    name = "conductor",
    about = "task-58 close-the-loop demo: the explorer's socket Machine vs. vmm-core's control server"
)]
struct Cli {
    #[command(subcommand)]
    mode: Mode,
}

#[derive(Subcommand)]
enum Mode {
    /// Portable scripted-MockBackend guest (no /dev/kvm).
    Mock(SweepArgs),
    /// Box-only: the real Postgres workload on patched KVM.
    Box(BoxArgs),
}

#[derive(Parser)]
struct SweepArgs {
    /// Number of entropy seeds to branch (the divergent futures).
    #[arg(long, default_value_t = 8)]
    seeds: usize,
    /// Runs per seed (>= 2 proves bit-identical reproduction).
    #[arg(long, default_value_t = 2)]
    runs: usize,
    /// Record each run's `RunTrace` into this directory (task 65). **Absent** =
    /// the task-58 socket sweep (no recording). **Present** = the in-process
    /// recording session, which drives `ControlServer::handle` directly so it can
    /// drain the guest console between verbs.
    #[arg(long)]
    record: Option<PathBuf>,
    /// Journal retention when `--record` is set: `all` | `interesting` |
    /// `env-only` (default `interesting`). The tiny env sidecar is *always*
    /// written; this gates the full journal bytes.
    #[arg(long, default_value = "interesting")]
    retain: String,
}

#[derive(Parser)]
struct BoxArgs {
    #[command(flatten)]
    sweep: SweepArgs,
    /// V-time (ns) each branch runs past the snapshot before its deadline.
    #[arg(long, default_value_t = 5_000_000_000)]
    deadline_delta: u64,
    /// Kernel bzImage filename under guest/build (or guest/linux).
    #[arg(long, default_value = "bzImage")]
    kernel: String,
    /// Initramfs filename under guest/build (or guest/linux). Defaults to the
    /// bare-Postgres image; point at `initramfs-docker.cpio.gz` to reuse the
    /// runc-Postgres image already staged on the box.
    #[arg(long, default_value = "initramfs-postgres.cpio.gz")]
    initramfs: String,
    /// The serial marker after which the base snapshot is sealed (the
    /// mid-workload, post-readiness point the gate wants). Default: the
    /// postmaster-ready banner.
    #[arg(long, default_value = "database system is ready to accept connections")]
    ready_marker: String,
}

/// Distinct, non-boot branch seeds (a multiplicative hash folded into a base) —
/// the same shape `live_branching_demo.rs` uses.
fn seeds(n: usize) -> Vec<u64> {
    (0..n)
        .map(|k| 0x0028_C0FF_EE5E_EDC0u64 ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1))
        .collect()
}

/// Reject too few seeds up front, before the sweep runs, so a valid-looking
/// run cannot print `GATES PASS` below the required bar. `min` is the mode's
/// floor: the mock demo uses `2` (the divergence gate's minimum — fewer can
/// never diverge), the **box milestone uses `8`** (the task-58 gate is
/// `N >= 8`, so a `--seeds 2..7` box run that happened to produce ≥ 2 distinct
/// hashes must NOT be able to report a milestone PASS). Returns `true` if the
/// count meets `min`.
fn seeds_ok(n: usize, min: usize) -> bool {
    if n < min {
        eprintln!(
            "[conductor] --seeds must be >= {min} here: got {n}. The divergence gate needs at \
             least 2 distinct futures, and the box milestone gate is N >= 8."
        );
        return false;
    }
    true
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode {
        Mode::Mock(args) => run_mock(args),
        Mode::Box(args) => run_box(args),
    }
}

/// Parse the `--retain` flag, reporting an unknown value loudly.
fn parse_retain(s: &str) -> Option<RetentionPolicy> {
    match RetentionPolicy::parse(s) {
        Some(p) => Some(p),
        None => {
            eprintln!("[conductor] unknown --retain {s:?}: use all | interesting | env-only");
            None
        }
    }
}

/// Print a recording run table and set the exit code from the task-65 gates: the
/// pure report checks plus the post-campaign store-reload check (kept out of the
/// recording loop, which stays write-only to the store).
fn finish_recording(
    mode: &str,
    report: &RecordReport,
    min_distinct: usize,
    store: &TraceStore,
    dir: &Path,
) -> ExitCode {
    print!("{}", render_record_table(report));
    let mut failures = verify_record(report, min_distinct);
    failures.extend(verify_store_reload(store, report));
    if failures.is_empty() {
        println!(
            "\n[conductor] {mode} RECORDING GATES PASS: per-seed byte-identical RunTraces, \
             >= {min_distinct} distinct TraceIds, non-empty monotone records, lossless reload. \
             Traces under {}",
            dir.display()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] {mode} RECORDING GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// The portable recording demo: the mock guest's console is scraped into a
/// `RunTrace` per run and persisted, then the task-65 gates are checked.
fn run_mock_recording(args: &SweepArgs, dir: PathBuf, retain: RetentionPolicy) -> ExitCode {
    let store = match TraceStore::open(&dir) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] cannot open trace store {}: {e}", dir.display());
            return ExitCode::FAILURE;
        }
    };
    let mut server = match conductor::mock::server(conductor::mock::recording_fork_script()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] failed to compose the mock recording server: {e}");
            return ExitCode::FAILURE;
        }
    };
    let cfg = RecordConfig {
        sweep: SweepConfig {
            seeds: seeds(args.seeds),
            runs_per_seed: args.runs.max(2),
            deadline_delta: None, // run each fork to its clean Hlt terminal
            ..SweepConfig::default()
        },
        retain,
        stream: StreamId(0),
    };
    println!(
        "[conductor] mock recording: {} seeds x {} runs, retain={}, into {}\n",
        cfg.sweep.seeds.len(),
        cfg.sweep.runs_per_seed,
        retain.as_str(),
        dir.display()
    );
    match run_recording(&mut server, &store, &cfg) {
        Ok(report) => finish_recording("mock", &report, 2, &store, &dir),
        Err(e) => {
            eprintln!("[conductor] mock recording failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The portable demo: scripted mock guest, sweep, table, verdicts.
fn run_mock(args: SweepArgs) -> ExitCode {
    if !seeds_ok(args.seeds, 2) {
        return ExitCode::FAILURE;
    }
    if let Some(dir) = args.record.clone() {
        let Some(retain) = parse_retain(&args.retain) else {
            return ExitCode::FAILURE;
        };
        return run_mock_recording(&args, dir, retain);
    }
    let cfg = SweepConfig {
        seeds: seeds(args.seeds),
        runs_per_seed: args.runs.max(2),
        deadline_delta: None, // run each fork to its clean Hlt terminal
        ..SweepConfig::default()
    };
    let mut server = match conductor::mock::server(conductor::mock::default_fork_script()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[conductor] failed to compose the mock server: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The mock live VM boots under BOOT_SEED with the never-fault policy.
    let initial = EnvSpec::Seeded {
        seed: conductor::mock::BOOT_SEED,
        policy: FaultPolicy::none(),
    };
    println!(
        "[conductor] mock mode: {} seeds x {} runs over a scripted MockBackend guest\n",
        cfg.seeds.len(),
        cfg.runs_per_seed
    );
    let (served, client) = run_session(&mut server, move |stream| {
        sweep_client(stream, initial, cfg)
    });
    finish("mock", served, client)
}

/// The box demo. Compiled everywhere (so `--help` and the crate build are
/// portable), but refuses to run off Linux + patched KVM — loudly, never a
/// vacuous pass.
#[cfg(target_os = "linux")]
fn run_box(args: BoxArgs) -> ExitCode {
    boxrun::run(args)
}

#[cfg(not(target_os = "linux"))]
fn run_box(_args: BoxArgs) -> ExitCode {
    eprintln!(
        "[conductor] box mode needs Linux + patched KVM + the built Postgres image + the \
         det-cfl-v1 host (see docs/BOX-PINNING.md). This is not a Linux host."
    );
    ExitCode::FAILURE
}

/// Print the outcome of a sweep session and set the exit code from the gates.
fn finish(
    mode: &str,
    served: Result<(), vmm_core::control::ServeError>,
    client: Result<conductor::SweepReport, explorer::MachineError>,
) -> ExitCode {
    let report = match client {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[conductor] {mode}: the sweep failed (transport/backend): {e}");
            // A client-side failure usually means the server tore down too.
            if let Err(se) = served {
                eprintln!("[conductor] {mode}: server session ended with: {se}");
            }
            return ExitCode::FAILURE;
        }
    };
    if let Err(se) = served {
        eprintln!("[conductor] {mode}: server session ended with a fatal error: {se}");
        return ExitCode::FAILURE;
    }
    print!("{}", render_table(&report));
    let failures = verify(&report, 2);
    if failures.is_empty() {
        println!(
            "\n[conductor] {mode} GATES PASS: per-seed reproducible, >= 2 distinct futures, \
             replay == capture."
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("\n[conductor] {mode} GATES FAILED:");
        for f in &failures {
            eprintln!("  - {f}");
        }
        ExitCode::FAILURE
    }
}

/// The box composition root. Linux-only (`boot_linux_selected` + `perf_event`).
///
/// The one piece of **workload-aware policy** in the whole path lives here (the
/// server and adapter stay workload-blind): the live guest is driven to a
/// readiness marker on its serial *before* the sweep seals the base, so the
/// snapshot lands **mid-workload, post-readiness** (the gate's point) rather
/// than at boot entry. Choosing *where* to snapshot is a property of the guest;
/// the snapshot *mechanism* (the verb) is not.
#[cfg(target_os = "linux")]
mod boxrun {
    use std::io::Write;
    use std::process::ExitCode;

    use conductor::record::{RecordConfig, run_recording};
    use conductor::{SweepConfig, run_session, sweep_client};
    use environment::{EnvSpec, FaultPolicy};
    use explorer::StreamId;
    use runtrace::TraceStore;
    use vmm_backend::Backend;
    use vmm_core::bringup::{BackendKind, boot_linux_selected};
    use vmm_core::control::{ControlServer, VmmFactory};
    use vmm_core::vmm::{Step, Vmm};

    use super::{BoxArgs, finish, finish_recording, parse_retain, seeds};

    /// 2 GiB guest RAM (matches `live_postgres.rs` / `live_branching_demo.rs`).
    const GUEST_RAM_LEN: usize = 2 << 30;
    /// The boot seed the live VM runs under (matches the branching demo).
    const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
    /// The determinism command line (identical to the branching demo).
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                           lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";
    /// A safety cap on the boot-to-marker drive (the external `timeout` is the
    /// real bound; this stops a wedged guest from looping forever).
    const MAX_BOOT_STEPS: u64 = 50_000_000_000;

    fn repo_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
    }

    fn artifact(name: &str) -> Option<Vec<u8>> {
        for p in [
            repo_root().join("guest/build").join(name),
            repo_root().join("guest/linux").join(name),
        ] {
            if let Ok(bytes) = std::fs::read(&p) {
                return Some(bytes);
            }
        }
        None
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Drive the live guest until `marker` appears on the serial, streaming new
    /// serial bytes to stderr so a hang shows the last line reached. Returns the
    /// number of steps taken, or an error string if the guest terminated first.
    ///
    /// The marker is scanned **only over newly-emitted bytes** (with a
    /// `marker.len()-1` overlap so a match straddling the boundary is not
    /// missed), gated on the serial actually growing. Rescanning the whole
    /// ever-growing buffer every step would be `O(steps × serial_len)` — on a
    /// real Postgres boot (millions of steps, a large console) that alone can
    /// make the drive look hung.
    fn drive_to_marker(vmm: &mut Vmm<Box<dyn Backend>>, marker: &[u8]) -> Result<u64, String> {
        let stderr = std::io::stderr();
        let mut printed = vmm.serial().len();
        // Where the next marker scan starts: keep a marker-1 overlap behind
        // `printed` so a match split across two batches of new bytes is seen.
        let overlap = marker.len().saturating_sub(1);
        let mut scan_from = printed.saturating_sub(overlap);
        let mut steps = 0u64;
        while steps < MAX_BOOT_STEPS {
            match vmm.step() {
                Ok(Step::Continued) => {}
                Ok(Step::Terminal(r)) => {
                    return Err(format!(
                        "guest reached a terminal ({r:?}) at step {steps} before the readiness \
                         marker appeared"
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
                // Only scan the fresh tail (plus the overlap) — not the whole buffer.
                if contains(&serial[scan_from..], marker) {
                    return Ok(steps);
                }
                scan_from = serial.len().saturating_sub(overlap);
            }
        }
        Err(format!("marker not seen within {MAX_BOOT_STEPS} steps"))
    }

    pub fn run(args: BoxArgs) -> ExitCode {
        // The box milestone gate is N >= 8 — enforce it so a smaller box run
        // can never print a milestone PASS below the bar.
        if !super::seeds_ok(args.sweep.seeds, 8) {
            return ExitCode::FAILURE;
        }
        if !std::path::Path::new("/dev/kvm").exists() {
            eprintln!(
                "[conductor] /dev/kvm absent — run on the determinism box with the LOADED patched \
                 KVM modules, CPU-pinned per docs/BOX-PINNING.md."
            );
            return ExitCode::FAILURE;
        }
        // The frozen contract cannot run off the det-cfl-v1 baseline.
        let report = vmm_core::hostassert::report();
        if let Some(bad) = report.iter().find(|o| !o.pass) {
            eprintln!(
                "[conductor] host is not the det-cfl-v1 baseline (first failing assertion: {} \
                 expected {}, observed {}). Run on the box.",
                bad.key, bad.expected, bad.actual
            );
            return ExitCode::FAILURE;
        }
        let (Some(kernel), Some(initramfs)) = (artifact(&args.kernel), artifact(&args.initramfs))
        else {
            eprintln!(
                "[conductor] guest image missing ({} / {}) — build it first: `make -C guest fetch \
                 && make -C guest/linux postgres-image`, or pass --initramfs for an image already \
                 on the box (e.g. initramfs-docker.cpio.gz).",
                args.kernel, args.initramfs
            );
            return ExitCode::FAILURE;
        };

        // Boot the live guest and drive it to the readiness marker, so the base
        // snapshot the sweep seals is mid-workload (the gate's point). This is
        // the workload-aware step; everything after it is workload-blind.
        let mut live = match boot_linux_selected(
            BackendKind::Patched,
            &kernel,
            &initramfs,
            GUEST_RAM_LEN,
            CMDLINE,
            BOOT_SEED,
        ) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[conductor] boot_linux_selected (patched) failed: {e}");
                return ExitCode::FAILURE;
            }
        };
        println!(
            "[conductor] box mode: booting the guest to the readiness marker {:?} …",
            args.ready_marker
        );
        match drive_to_marker(&mut live, args.ready_marker.as_bytes()) {
            Ok(steps) => println!(
                "\n[conductor] readiness marker reached at step {steps}; the base snapshot will be \
                 sealed at the next snapshottable boundary at/after this point.\n"
            ),
            Err(e) => {
                eprintln!("\n[conductor] failed to reach the readiness marker: {e}");
                return ExitCode::FAILURE;
            }
        }

        // The fork factory: fresh, equivalently-composed patched VMs whose
        // boot-loaded image the restore immediately overwrites. `live` is
        // already booted (it owns its guest RAM), so **move** the sole
        // kernel/initramfs copies into the closure rather than cloning them —
        // an initramfs is tens/hundreds of MB, and cloning would keep two
        // copies resident for the whole run.
        let factory: VmmFactory<Box<dyn Backend>> = Box::new(move || {
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

        // Postgres is interrupt-driven; the snapshot search may need many steps
        // to find a sealable boundary at/after readiness. Generous retry budget
        // (task 41 made mid-workload points snapshottable).
        let (snapshot_retry_step, snapshot_max_attempts) = (1_000_000u64, 100_000usize);

        // The task-65 box gate: record each run's RunTrace and check byte-
        // stability. The readiness banner is already confirmed present above (the
        // boot drive only returns Ok once the marker is seen), so the recorded
        // per-run console is the post-snapshot workload.
        if let Some(dir) = args.sweep.record.clone() {
            let Some(retain) = parse_retain(&args.sweep.retain) else {
                return ExitCode::FAILURE;
            };
            let store = match TraceStore::open(&dir) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("[conductor] cannot open trace store {}: {e}", dir.display());
                    return ExitCode::FAILURE;
                }
            };
            let cfg = RecordConfig {
                sweep: SweepConfig {
                    seeds: seeds(args.sweep.seeds),
                    runs_per_seed: args.sweep.runs.max(2),
                    deadline_delta: Some(args.deadline_delta),
                    snapshot_retry_step,
                    snapshot_max_attempts,
                },
                retain,
                stream: StreamId(0),
            };
            println!(
                "[conductor] box recording: {} seeds x {} runs, retain={}, into {}\n",
                cfg.sweep.seeds.len(),
                cfg.sweep.runs_per_seed,
                retain.as_str(),
                dir.display()
            );
            return match run_recording(&mut server, &store, &cfg) {
                Ok(report) => finish_recording("box", &report, 2, &store, &dir),
                Err(e) => {
                    eprintln!("[conductor] box recording failed: {e}");
                    ExitCode::FAILURE
                }
            };
        }

        let cfg = SweepConfig {
            seeds: seeds(args.sweep.seeds),
            runs_per_seed: args.sweep.runs.max(2),
            deadline_delta: Some(args.deadline_delta),
            snapshot_retry_step,
            snapshot_max_attempts,
        };
        let initial = EnvSpec::Seeded {
            seed: BOOT_SEED,
            policy: FaultPolicy::none(),
        };
        println!(
            "[conductor] box mode: {} seeds x {} runs; each branch runs {} ns of V-time past the \
             snapshot.\n",
            cfg.seeds.len(),
            cfg.runs_per_seed,
            args.deadline_delta
        );
        let (served, client) = run_session(&mut server, move |stream| {
            sweep_client(stream, initial, cfg)
        });
        finish("box", served, client)
    }
}
