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

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use conductor::{SweepConfig, render_table, run_session, sweep_client, verify};
use environment::{EnvSpec, FaultPolicy};

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
}

#[derive(Parser)]
struct BoxArgs {
    #[command(flatten)]
    sweep: SweepArgs,
    /// V-time (ns) each branch runs past the snapshot before its deadline.
    #[arg(long, default_value_t = 5_000_000_000)]
    deadline_delta: u64,
}

/// Distinct, non-boot branch seeds (a multiplicative hash folded into a base) —
/// the same shape `live_branching_demo.rs` uses.
fn seeds(n: usize) -> Vec<u64> {
    (0..n)
        .map(|k| 0x0028_C0FF_EE5E_EDC0u64 ^ 0x9E37_79B9_7F4A_7C15u64.wrapping_mul(k as u64 + 1))
        .collect()
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.mode {
        Mode::Mock(args) => run_mock(args),
        Mode::Box(args) => run_box(args),
    }
}

/// The portable demo: scripted mock guest, sweep, table, verdicts.
fn run_mock(args: SweepArgs) -> ExitCode {
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
    boxrun::run(args.sweep, args.deadline_delta)
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
#[cfg(target_os = "linux")]
mod boxrun {
    use std::process::ExitCode;

    use conductor::{SweepConfig, run_session, sweep_client};
    use environment::{EnvSpec, FaultPolicy};
    use vmm_backend::Backend;
    use vmm_core::bringup::{BackendKind, boot_linux_selected};
    use vmm_core::control::{ControlServer, VmmFactory};

    use super::{SweepArgs, finish, seeds};

    /// 2 GiB guest RAM (matches `live_postgres.rs` / `live_branching_demo.rs`).
    const GUEST_RAM_LEN: usize = 2 << 30;
    /// The boot seed the live VM runs under (matches the branching demo).
    const BOOT_SEED: u64 = 0x0028_C0FF_EE5E_EDC0;
    /// The determinism command line (identical to the branching demo).
    const CMDLINE: &str = "console=ttyS0 panic=-1 reboot=t,force tsc=reliable no_timer_check \
                           lpj=4000000 nokaslr nosmp maxcpus=1 nox2apic hpet=disable";

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

    pub fn run(args: SweepArgs, deadline_delta: u64) -> ExitCode {
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
        let (Some(kernel), Some(initramfs)) =
            (artifact("bzImage"), artifact("initramfs-postgres.cpio.gz"))
        else {
            eprintln!(
                "[conductor] guest image missing — build it first: `make -C guest fetch && \
                 make -C guest/linux postgres-image`."
            );
            return ExitCode::FAILURE;
        };

        // Boot the live Postgres guest.
        let live = match boot_linux_selected(
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
        // The fork factory: fresh, equivalently-composed patched VMs whose
        // boot-loaded image the restore immediately overwrites.
        let factory: VmmFactory<Box<dyn Backend>> = {
            let kernel = kernel.clone();
            let initramfs = initramfs.clone();
            Box::new(move || {
                boot_linux_selected(
                    BackendKind::Patched,
                    &kernel,
                    &initramfs,
                    GUEST_RAM_LEN,
                    CMDLINE,
                    BOOT_SEED,
                )
            })
        };
        let mut server = ControlServer::new(live, factory);

        let cfg = SweepConfig {
            seeds: seeds(args.seeds),
            runs_per_seed: args.runs.max(2),
            deadline_delta: Some(deadline_delta),
            // Postgres is interrupt-driven; the snapshot search may need many
            // steps to find a sealable boundary. Generous retry budget.
            snapshot_retry_step: 1_000_000,
            snapshot_max_attempts: 100_000,
        };
        let initial = EnvSpec::Seeded {
            seed: BOOT_SEED,
            policy: FaultPolicy::none(),
        };
        println!(
            "[conductor] box mode: booting Postgres, then {} seeds x {} runs; each branch runs \
             {} ns of V-time past the snapshot.\n",
            cfg.seeds.len(),
            cfg.runs_per_seed,
            deadline_delta
        );
        let (served, client) = run_session(&mut server, move |stream| {
            sweep_client(stream, initial, cfg)
        });
        finish("box", served, client)
    }
}
