//! `arm-spike` — the run orchestrator and, on the box, the capability probe.
//!
//! Off the box (the Mac, or any non-Linux host) it does the two things that do not
//! need hardware: emit a **plan** as stable JSON (so a run-set's sample list can be
//! reviewed and diffed before a single measurement is spent), and print what it
//! *would* probe. On the box its `probe` subcommand issues the real perf/KVM
//! syscalls of stage AA-0 — but that path is Linux-only and **has never run**,
//! because the Altra is not yet in hand.
//!
//! The actual `KVM_RUN` loop (arm the counter, run to a window mark, sample
//! `BR_RETIRED`, write a [`arm_harness::evidence::RunRecord`]) is deliberately not
//! wired to hardware here: it is AA-1's to drive, on the box, against the real
//! kernel. What this binary delivers pre-silicon is the scaffolding around it —
//! the plan, the evidence shapes, and the probe entry point — so arrival day wires
//! the loop, not the tooling.

use std::process::ExitCode;

use arm_harness::plan::{PlanSpec, plan};
use arm_harness::sys::{self, Capability};
use clap::{Parser, Subcommand};
use oracle_model::{ALL_PAYLOADS, ALL_SCALES, Payload, Scale};

#[derive(Parser)]
#[command(
    name = "arm-spike",
    about = "ARM vendor spike harness — orchestration + AA-0 probes (untested on silicon)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Emit a deterministic run plan as stable JSON.
    Plan {
        /// Master seed.
        #[arg(long, default_value_t = 0x5EED_5EED_5EED_5EED)]
        seed: u64,
        /// Repetitions of the whole matrix.
        #[arg(long, default_value_t = 1)]
        reps: u64,
        /// Draw seeded-random target deltas over 1..=100000 (AA-3), instead of a
        /// pure counting plan (AA-1).
        #[arg(long)]
        with_targets: bool,
    },
    /// Probe the AA-0 capabilities on the running host (Linux/box only).
    Probe,
}

fn emit_plan(seed: u64, reps: u64, with_targets: bool) -> Result<(), String> {
    let spec = PlanSpec {
        payloads: ALL_PAYLOADS
            .iter()
            .copied()
            .filter(|p| p.has_window())
            .collect(),
        scales: vec![Scale::Smoke],
        conditions: vec!["pinned-solo".into()],
        reps,
        seed,
        target_delta_range: with_targets.then_some((1, 100_000)),
    };
    let samples = plan(&spec);
    let json =
        serde_json::to_string_pretty(&samples).map_err(|e| format!("serialize plan: {e}"))?;
    println!("{json}");
    Ok(())
}

/// A capability probe, formatted so a "cannot probe" is visibly distinct from a
/// "no". A stage disposition may never rest on a probe that could not run.
fn report(cap: Capability, label: &str) -> bool {
    match sys::probe(cap) {
        Ok(true) => {
            println!("ok       {label}: present");
            true
        }
        Ok(false) => {
            println!("absent   {label}: not present");
            false
        }
        Err(e) => {
            eprintln!("unprobed {label}: {e}");
            false
        }
    }
}

fn probe() -> Result<(), String> {
    // On non-Linux hosts every probe returns "unprobed"; that is the honest
    // answer, and the command exits nonzero rather than pretending.
    let dev_kvm = report(Capability::DevKvm, "/dev/kvm");
    let br = report(
        Capability::PerfBrRetired,
        "perf BR_RETIRED (raw 0x21, pinned)",
    );
    let _debug = report(Capability::GuestDebug, "KVM_CAP_SET_GUEST_DEBUG");
    let _det = report(
        Capability::DeterministicIntercepts,
        "KVM_CAP_ARM_DETERMINISTIC_INTERCEPTS (0004-analogue)",
    );

    if dev_kvm && br {
        Ok(())
    } else {
        Err("a load-bearing AA-0 capability is absent or could not be probed on this host".into())
    }
}

fn run() -> Result<(), String> {
    let _ = Payload::Ident;
    let _ = ALL_SCALES;
    match Cli::parse().command {
        Command::Plan {
            seed,
            reps,
            with_targets,
        } => emit_plan(seed, reps, with_targets),
        Command::Probe => probe(),
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("FAIL: {e}");
            ExitCode::FAILURE
        }
    }
}
