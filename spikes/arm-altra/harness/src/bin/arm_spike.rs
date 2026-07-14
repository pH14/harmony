//! `arm-spike` — the AA-0 capability probe and the run orchestrator.
//!
//! Three subcommands:
//!
//! - `plan` — emit a deterministic run plan as stable JSON, so a run-set's sample
//!   list can be reviewed and diffed before a single measurement is spent. Pure
//!   logic; runs anywhere.
//! - `probe` — issue AA-0's capability probes against the running kernel. Linux
//!   only, and **untested on silicon**.
//! - `run` — the measurement loop: create the VM, publish the params page, run each
//!   planned sample to its window marks, sample `BR_RETIRED`, and write the run-set
//!   (`run-set.json` + `records.jsonl`) the floor checker adjudicates. Linux only,
//!   and **untested on silicon**.
//!
//! # What `run` refuses to invent
//!
//! Everything it can derive from the machine, it derives: the perf configuration is
//! a projection of the `perf_event_attr` the counter was actually opened with, the
//! pinning block says the core it actually pinned to, the mechanism block carries
//! the capability it actually probed, and the records' sha256 is of the bytes it
//! actually wrote. Everything it *cannot* know — the environment (MIDR, firmware,
//! SoC), the measured weights, the measured skid margin — must be supplied, and is
//! otherwise left `null` rather than guessed. A run-set with `weights: null` is a
//! legitimate artifact: it is one the floor checker will refuse to grade counts on,
//! which is the correct outcome before AA-1 has produced them.

use std::path::PathBuf;
use std::process::ExitCode;

use arm_harness::evidence::{Environment, Stage};
use arm_harness::plan::{PlanSpec, plan};
use arm_harness::sys::{self, Capability};
use clap::{Parser, Subcommand, ValueEnum};
use oracle_model::{ALL_PAYLOADS, Scale, Weights};

#[derive(Parser)]
#[command(
    name = "arm-spike",
    about = "ARM vendor spike harness — AA-0 probes, run planning, and the KVM_RUN measurement loop \
             (untested on silicon)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Which overflow mechanism a run arms. Named on the command line, never inferred:
/// "unsupported is a result", and a run that silently downgraded the patched exit to
/// the stock kick would be exactly the PR-98 failure.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum MechanismArg {
    /// AA-1(c): a host-side signal kicks the vCPU out of `KVM_RUN`.
    Stock,
    /// AA-3: the patched in-kernel `KVM_EXIT_PREEMPT`. Refuses to open on a kernel
    /// that does not advertise the capability.
    Patched,
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
    /// Run the planned samples and write a run-set (Linux/box only).
    Run(Box<RunOpts>),
}

/// `arm-spike run`'s options. Boxed in the [`Command`] enum: it is far larger than
/// the other variants, and clap parses it straight into this shape.
#[derive(clap::Args, Debug)]
struct RunOpts {
    /// The payload ELF to run.
    #[arg(long)]
    payload: PathBuf,
    /// Its expected sha256. Verified against the bytes actually loaded, immediately
    /// before the vCPU runs — §Evidence integrity #3: recording a hash without
    /// verifying it is the anti-pattern that rule exists to kill. Omitted, the
    /// run-set records `verified_before_boot: false` and the floor checker refuses it.
    #[arg(long)]
    payload_sha256: Option<String>,
    /// sha256 of the running host kernel image — the mechanism block's kernel
    /// identity. arm64 KVM is built in (`CONFIG_KVM=y`), so the kernel *is* the module
    /// identity and swapping it is a reboot; the harness cannot read it from the
    /// running system, so the operator pins it from the booted artifact.
    #[arg(long)]
    host_kernel_sha256: String,
    /// The core to hard-pin the vCPU thread to. Pinning is a correctness condition on
    /// this lineage (rr #3607), not hygiene.
    #[arg(long)]
    core: u32,
    /// The stage this run-set belongs to.
    #[arg(long)]
    stage: StageArg,
    /// Which overflow mechanism to arm.
    #[arg(long)]
    mechanism: MechanismArg,
    /// AA-0's environment block, as JSON (MIDR, SoC, firmware, host kernel, KVM mode).
    /// Required: the harness cannot read the machine's identity out of thin air, and
    /// inventing it would be fabricated evidence.
    #[arg(long)]
    environment: PathBuf,
    /// The measured weights pack, as JSON (AA-1's deliverable). Absent, the run-set
    /// carries `weights: null` and the checker refuses to grade counts — which is
    /// correct before AA-1 has measured them.
    #[arg(long)]
    weights: Option<PathBuf>,
    /// The measured skid margin (AA-1's deliverable). Absent for the same reason.
    #[arg(long)]
    skid_margin: Option<u64>,
    /// The experimental condition (`pinned-solo`, `co-tenant-load`, …).
    #[arg(long, default_value = "pinned-solo")]
    condition: String,
    /// Identifier for this run-set. Golden evidence is immutable; a rerun makes a new
    /// run-set rather than overwriting one.
    #[arg(long)]
    run_set_id: String,
    /// Where to write `run-set.json` and `records.jsonl`.
    #[arg(long)]
    out: PathBuf,
    /// Master seed for the plan.
    #[arg(long, default_value_t = 0x5EED_5EED_5EED_5EED)]
    seed: u64,
    /// Repetitions of the whole matrix.
    #[arg(long, default_value_t = 1)]
    reps: u64,
    /// Draw seeded-random target deltas over 1..=100000 (AA-3).
    #[arg(long)]
    with_targets: bool,
}

/// The stage a run-set claims, on the command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum StageArg {
    /// Day-one bring-up + capability truth table.
    Aa0,
    /// The work clock.
    Aa1,
    /// Single-step exactness.
    Aa2,
    /// Deterministic force-exit + exact landing.
    Aa3,
    /// The LL/SC vs LSE ruling.
    Aa4,
    /// The paravirt work-derived clock.
    Aa5,
    /// Contract enforcement + the mini determinism gate.
    Aa6,
}

impl From<StageArg> for Stage {
    fn from(s: StageArg) -> Stage {
        match s {
            StageArg::Aa0 => Stage::Aa0,
            StageArg::Aa1 => Stage::Aa1,
            StageArg::Aa2 => Stage::Aa2,
            StageArg::Aa3 => Stage::Aa3,
            StageArg::Aa4 => Stage::Aa4,
            StageArg::Aa5 => Stage::Aa5,
            StageArg::Aa6 => Stage::Aa6,
        }
    }
}

fn plan_spec(seed: u64, reps: u64, with_targets: bool) -> PlanSpec {
    PlanSpec {
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
    }
}

fn emit_plan(seed: u64, reps: u64, with_targets: bool) -> Result<(), String> {
    let samples = plan(&plan_spec(seed, reps, with_targets));
    let json =
        serde_json::to_string_pretty(&samples).map_err(|e| format!("serialize plan: {e}"))?;
    println!("{json}");
    Ok(())
}

/// What a capability probe returned. The three-way split is the whole point: a stage
/// disposition may never rest on a probe that **could not run**, so "unprobed" is
/// not allowed to collapse into "absent".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Present,
    Absent,
    Unprobed,
}

fn report(cap: Capability) -> Row {
    match sys::probe(cap) {
        Ok(true) => {
            println!("ok       {}: present", cap.name());
            Row::Present
        }
        Ok(false) => {
            println!("absent   {}: not present", cap.name());
            Row::Absent
        }
        Err(e) => {
            eprintln!("unprobed {}: {e}", cap.name());
            Row::Unprobed
        }
    }
}

/// AA-0's capability rows, and the exit status that must follow from them.
///
/// The rule the RC enforces — and it is the RC, not the printout, that scripts
/// consume:
///
/// - **any row unprobed ⇒ nonzero.** An unprobed mandatory row is a stage that
///   cannot be dispositioned, not a stage that passed.
/// - **an expect-present row absent ⇒ nonzero** (`/dev/kvm`, raw 0x21 pinned,
///   `KVM_CAP_SET_GUEST_DEBUG` — AA-2's load-bearing capability).
/// - **the determinism cap absent ⇒ OK.** It is the one expect-*absent* row: a stock
///   kernel does not have it, and that is a finding, not a failure. Only the patched
///   kernel advertises it, which is what makes it AA-3's mechanism attestation.
fn probe() -> Result<(), String> {
    let mut unprobed: Vec<&str> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();

    for cap in [
        Capability::DevKvm,
        Capability::PerfBrRetired,
        Capability::GuestDebug,
        Capability::DeterministicIntercepts,
    ] {
        match report(cap) {
            Row::Present => {}
            Row::Absent if cap.expect_present() => missing.push(cap.name()),
            Row::Absent => {}
            Row::Unprobed => unprobed.push(cap.name()),
        }
    }

    let mut problems = Vec::new();
    if !unprobed.is_empty() {
        problems.push(format!(
            "{} mandatory row(s) could not be probed ({}): a disposition may not rest on a probe \
             that did not run",
            unprobed.len(),
            unprobed.join(", ")
        ));
    }
    if !missing.is_empty() {
        problems.push(format!(
            "{} expect-present capability/ies absent ({})",
            missing.len(),
            missing.join(", ")
        ));
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems.join("; "))
    }
}

/// Read a JSON file into a deserializable shape.
fn read_json<T: serde::de::DeserializeOwned>(path: &PathBuf) -> Result<T, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("parse {}: {e}", path.display()))
}

#[cfg(not(target_os = "linux"))]
#[allow(clippy::too_many_arguments)]
fn execute(_args: RunArgs) -> Result<(), String> {
    Err(
        "`arm-spike run` issues KVM ioctls and needs /dev/kvm: it is Linux-only, and this host is \
         not Linux. (The logic it drives is tested here natively; the syscalls run on the Altra \
         box.)"
            .into(),
    )
}

/// The `run` subcommand's arguments, gathered so the Linux and non-Linux entry
/// points share one signature. Off Linux the fields are unread by construction —
/// there is no `/dev/kvm` to hand them to.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct RunArgs {
    payload: PathBuf,
    payload_sha256: Option<String>,
    host_kernel_sha256: String,
    core: u32,
    stage: Stage,
    mechanism: MechanismArg,
    environment: Environment,
    weights: Option<Weights>,
    skid_margin: Option<u64>,
    condition: String,
    run_set_id: String,
    out: PathBuf,
    seed: u64,
    reps: u64,
    with_targets: bool,
}

#[cfg(target_os = "linux")]
fn execute(args: RunArgs) -> Result<(), String> {
    use arm_harness::elf::Elf;
    use arm_harness::evidence::{
        ExitReason, ImagePin, Mechanism, Pinning, RunSetContext, assemble_run_set, hex_lower,
    };
    use arm_harness::run::{SampleSpec, run_sample};
    use arm_harness::sys::{self, Machine, ParamsPage, PerfCounter, perf_config, pin_to_core};
    use sha2::{Digest, Sha256};

    // Pin first, and pin the thread that will call KVM_RUN: the perf context follows
    // the thread, and on this lineage an unpinned sample is not a slower sample, it
    // is an untrusted one (rr #3607).
    pin_to_core(args.core).map_err(|e| format!("pin to core {}: {e}", args.core))?;

    // Content-verify the image against its pin, on the bytes we are about to load —
    // not on a path we hope still holds them.
    let bytes = std::fs::read(&args.payload)
        .map_err(|e| format!("read {}: {e}", args.payload.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let digest = hex_lower(&h.finalize());
    let verified = match &args.payload_sha256 {
        Some(want) => {
            let want = want.trim_start_matches("sha256:").to_ascii_lowercase();
            if want != digest {
                return Err(format!(
                    "image pin mismatch: {} hashes to {digest}, but --payload-sha256 says {want}. \
                     Refusing to boot an artifact that is not the one pinned",
                    args.payload.display()
                ));
            }
            true
        }
        None => false,
    };

    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", args.payload.display()))?;
    let samples = plan(&plan_spec(args.seed, args.reps, args.with_targets));
    let attempted = samples.len() as u64;

    let mechanism_kind = match args.mechanism {
        MechanismArg::Stock => sys::Mechanism::SignalKick,
        MechanismArg::Patched => sys::Mechanism::Preempt,
    };

    let mut records = Vec::new();
    let mut armed_attr = None;
    let mut patch_marker = false;

    for (i, s) in samples.iter().enumerate() {
        // A fresh VM per sample: the guest starts from the same state every time,
        // which is what makes two same-seed samples comparable at all.
        let params = ParamsPage {
            scale_index: s.scale.index(),
            seed: s.seed,
        };
        let mut machine = Machine::new(&image, &params)
            .map_err(|e| format!("sample {i}: create the machine: {e}"))?;
        // The patch marker, probed on the VM actually running the sample — the
        // positive proof of §Evidence integrity #4, not a build-time assumption.
        patch_marker = machine
            .patch_marker_observed()
            .map_err(|e| format!("sample {i}: probe the patch marker: {e}"))?;

        // Opening the counter for the patched mechanism on a kernel that lacks the
        // capability FAILS here. There is no code path from the patched request to
        // the stock kick, which is what makes the fallback structurally unable to
        // masquerade as the mechanism under test.
        let mut counter = PerfCounter::open(&machine, mechanism_kind, s.target_delta)
            .map_err(|e| format!("sample {i}: open the work counter: {e}"))?;
        armed_attr = Some(*counter.attr());

        let spec = SampleSpec {
            sample_id: s.sample_id,
            payload: s.payload,
            scale: s.scale,
            seed: s.seed,
            trips: oracle_model::trips(s.payload, s.scale),
            condition: s.condition.clone(),
            target_delta: s.target_delta,
        };
        // A sample that cannot be MEASURED does not quietly vanish from the evidence:
        // the run fails here, and whatever records exist are short of `attempted` —
        // which the totality check catches. A missing sample is a failure to account,
        // not a pass.
        let record = run_sample(&mut machine, &mut counter, &spec)
            .map_err(|e| format!("sample {i} ({}): {e}", s.payload.name()))?;
        records.push(record);
    }

    let attr = armed_attr.ok_or("the plan produced no samples: nothing was measured")?;
    let context = RunSetContext {
        stage: args.stage,
        run_set_id: args.run_set_id,
        environment: args.environment,
        mechanism: Mechanism {
            kvm_patched: patch_marker,
            host_kernel_sha256: args.host_kernel_sha256,
            expected_exit_reason: match args.mechanism {
                MechanismArg::Stock => ExitReason::SignalKick,
                MechanismArg::Patched => ExitReason::Preempt,
            },
            patch_marker_observed: patch_marker,
        },
        images: vec![ImagePin {
            path: args.payload.display().to_string(),
            sha256: digest,
            // The md5 cross-reference is the operator's (no md5 crate is on the
            // dependency whitelist); sha256 is the identity this harness verifies.
            md5: String::new(),
            verified_before_boot: verified,
        }],
        perf: perf_config(&attr),
        pinning: Pinning {
            pinned: true,
            core: Some(args.core),
            governor: std::fs::read_to_string(
                "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
            )
            .unwrap_or_default()
            .trim()
            .to_string(),
            migration_probe: false,
        },
        condition: args.condition,
        weights: args.weights,
        skid_margin: args.skid_margin,
        attempted,
    };

    let (manifest, records_jsonl) =
        assemble_run_set(context, &records).map_err(|e| format!("assemble the run-set: {e}"))?;

    std::fs::create_dir_all(&args.out)
        .map_err(|e| format!("create {}: {e}", args.out.display()))?;
    std::fs::write(args.out.join("records.jsonl"), &records_jsonl)
        .map_err(|e| format!("write records.jsonl: {e}"))?;
    std::fs::write(args.out.join("run-set.json"), &manifest)
        .map_err(|e| format!("write run-set.json: {e}"))?;

    println!(
        "wrote {} records to {} ({} attempted)",
        records.len(),
        args.out.display(),
        attempted
    );
    println!(
        "NOTE: this harness's verdict is not a disposition. Run `floor-check {}` with the \
         stage's floors; the checker's output is the evidence.",
        args.out.display()
    );
    Ok(())
}

fn run() -> Result<(), String> {
    match Cli::parse().command {
        Command::Plan {
            seed,
            reps,
            with_targets,
        } => emit_plan(seed, reps, with_targets),
        Command::Probe => probe(),
        Command::Run(opts) => {
            let environment: Environment = read_json(&opts.environment)?;
            let weights: Option<Weights> = match &opts.weights {
                Some(p) => Some(read_json(p)?),
                None => None,
            };
            execute(RunArgs {
                payload: opts.payload,
                payload_sha256: opts.payload_sha256,
                host_kernel_sha256: opts.host_kernel_sha256,
                core: opts.core,
                stage: opts.stage.into(),
                mechanism: opts.mechanism,
                environment,
                weights,
                skid_margin: opts.skid_margin,
                condition: opts.condition,
                run_set_id: opts.run_set_id,
                out: opts.out,
                seed: opts.seed,
                reps: opts.reps,
                with_targets: opts.with_targets,
            })
        }
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
