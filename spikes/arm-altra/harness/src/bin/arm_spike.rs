// SPDX-License-Identifier: AGPL-3.0-or-later
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
    /// Directory of built payload ELFs, one per class, named by
    /// [`oracle_model::Payload::name`] (the same layout `arm-scan windows` reads).
    /// Every planned sample boots the ELF that matches ITS payload — booting one ELF
    /// under every class's label would produce mislabeled evidence.
    #[arg(long)]
    payload_dir: PathBuf,
    /// The running host kernel image (e.g. `/boot/Image`). It is read and hashed, and
    /// that hash becomes both the mechanism block's `host_kernel_sha256` and a
    /// verified image pin — so the kernel the run attests is content-verified, never
    /// an operator-typed string the checker only checks is nonempty.
    #[arg(long)]
    host_kernel_image: PathBuf,
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
    /// The measurement scales to sweep, e.g. `--scale 1e6 --scale 1e7 --scale 1e8`
    /// for the AA-1 differential. Defaults to `smoke` alone — enough to shake the
    /// pipeline out, but NOT the AA-1 sweep, which the differencing argument needs
    /// the larger scales for.
    #[arg(long = "scale", value_name = "SCALE")]
    scales: Vec<ScaleArg>,
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
    /// The experimental condition (`pinned-solo`, `co-tenant-load`, …). Threaded into
    /// every planned sample AND the manifest, so the two cannot disagree about which
    /// experiment ran.
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

/// A measurement scale, on the command line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
enum ScaleArg {
    /// The TCG / smoke scale.
    Smoke,
    /// ~1e6 trips.
    #[value(name = "1e6")]
    S1e6,
    /// ~1e7 trips.
    #[value(name = "1e7")]
    S1e7,
    /// ~1e8 trips.
    #[value(name = "1e8")]
    S1e8,
}

impl From<ScaleArg> for Scale {
    fn from(s: ScaleArg) -> Scale {
        match s {
            ScaleArg::Smoke => Scale::Smoke,
            ScaleArg::S1e6 => Scale::S1e6,
            ScaleArg::S1e7 => Scale::S1e7,
            ScaleArg::S1e8 => Scale::S1e8,
        }
    }
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

fn plan_spec(
    seed: u64,
    reps: u64,
    with_targets: bool,
    scales: Vec<Scale>,
    condition: &str,
) -> PlanSpec {
    PlanSpec {
        payloads: ALL_PAYLOADS
            .iter()
            .copied()
            .filter(|p| p.has_window())
            .collect(),
        scales,
        conditions: vec![condition.to_string()],
        reps,
        seed,
        target_delta_range: with_targets.then_some((1, 100_000)),
    }
}

fn emit_plan(seed: u64, reps: u64, with_targets: bool) -> Result<(), String> {
    let samples = plan(&plan_spec(
        seed,
        reps,
        with_targets,
        vec![Scale::Smoke],
        "pinned-solo",
    ));
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
    payload_dir: PathBuf,
    host_kernel_image: PathBuf,
    core: u32,
    stage: Stage,
    mechanism: MechanismArg,
    scales: Vec<Scale>,
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

/// A payload ELF loaded from the payload directory, with its verified content pin.
#[cfg(target_os = "linux")]
struct LoadedPayload {
    image: arm_harness::elf::Elf,
    sha256: String,
}

/// Read, hash, and parse the ELF for `payload` out of `dir`, keyed by the payload's
/// canonical name. Loading one ELF for every class would run one payload under seven
/// wrong labels — mislabeled evidence — so each class is loaded from its own file.
#[cfg(target_os = "linux")]
fn load_payload(
    dir: &std::path::Path,
    payload: oracle_model::Payload,
) -> Result<LoadedPayload, String> {
    use arm_harness::elf::Elf;
    use arm_harness::evidence::hex_lower;
    use sha2::{Digest, Sha256};

    let path = dir.join(payload.name());
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha256 = hex_lower(&h.finalize());
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(LoadedPayload { image, sha256 })
}

#[cfg(target_os = "linux")]
fn execute(args: RunArgs) -> Result<(), String> {
    use arm_harness::evidence::{
        ExitReason, ImagePin, Mechanism, Pinning, RunSetContext, assemble_run_set, hex_lower,
    };
    use arm_harness::run::{SampleSpec, run_sample};
    use arm_harness::sys::{self, Machine, ParamsPage, PerfCounter, perf_config, pin_to_core};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    // Pin first, and pin the thread that will call KVM_RUN: the perf context follows
    // the thread, and on this lineage an unpinned sample is not a slower sample, it
    // is an untrusted one (rr #3607).
    pin_to_core(args.core).map_err(|e| format!("pin to core {}: {e}", args.core))?;

    // Content-verify the running host kernel by hashing the image itself, rather than
    // trusting an operator-typed string. That hash is the mechanism block's kernel
    // identity AND a verified image pin, so §Evidence integrity #3's rule — which
    // explicitly includes host kernels — actually covers it.
    let host_kernel_bytes = std::fs::read(&args.host_kernel_image)
        .map_err(|e| format!("read host kernel {}: {e}", args.host_kernel_image.display()))?;
    let host_kernel_sha256 = {
        let mut h = Sha256::new();
        h.update(&host_kernel_bytes);
        hex_lower(&h.finalize())
    };

    // Load every payload class the plan will run, up front — one ELF per class, from
    // its own file, each with its verified content pin.
    let mut payloads: BTreeMap<String, LoadedPayload> = BTreeMap::new();
    for p in ALL_PAYLOADS.iter().copied().filter(|p| p.has_window()) {
        payloads.insert(p.name().to_string(), load_payload(&args.payload_dir, p)?);
    }

    let scales = if args.scales.is_empty() {
        vec![Scale::Smoke]
    } else {
        args.scales.iter().copied().map(Scale::from).collect()
    };
    let samples = plan(&plan_spec(
        args.seed,
        args.reps,
        args.with_targets,
        scales,
        &args.condition,
    ));
    let attempted = samples.len() as u64;

    let mechanism_kind = match args.mechanism {
        MechanismArg::Stock => sys::Mechanism::SignalKick,
        MechanismArg::Patched => sys::Mechanism::Preempt,
    };

    // Run the samples, gathering records. A sample that cannot be MEASURED stops the
    // run — but the evidence gathered so far is still written, so the gap is visible
    // to the totality checker (attempted counts the full plan). A reliability failure
    // must not disappear when the operator reruns; it must be on the record.
    let mut records = Vec::new();
    let mut armed_attr = None;
    let mut patch_marker = false;
    let mut failure: Option<String> = None;

    for (i, s) in samples.iter().enumerate() {
        let loaded = match payloads.get(s.payload.name()) {
            Some(l) => l,
            None => {
                failure = Some(format!(
                    "sample {i}: no ELF for payload {}",
                    s.payload.name()
                ));
                break;
            }
        };
        // A fresh VM per sample: the guest starts from the same state every time,
        // which is what makes two same-seed samples comparable at all.
        let params = ParamsPage {
            scale_index: s.scale.index(),
            seed: s.seed,
        };
        let result = (|| {
            let mut machine = Machine::new(&loaded.image, &params)
                .map_err(|e| format!("create the machine: {e}"))?;
            // The patch marker, probed on the VM actually running the sample — the
            // positive proof of §Evidence integrity #4, not a build-time assumption.
            patch_marker = machine
                .patch_marker_observed()
                .map_err(|e| format!("probe the patch marker: {e}"))?;
            // Opening the counter for the patched mechanism on a kernel that lacks the
            // capability FAILS here. There is no code path from the patched request to
            // the stock kick.
            let mut counter = PerfCounter::open(&machine, mechanism_kind, s.target_delta)
                .map_err(|e| format!("open the work counter: {e}"))?;
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
            run_sample(&mut machine, &mut counter, &spec).map_err(|e| e.to_string())
        })();
        match result {
            Ok(record) => records.push(record),
            Err(e) => {
                failure = Some(format!("sample {i} ({}): {e}", s.payload.name()));
                break;
            }
        }
    }

    // Assemble and WRITE the evidence regardless of whether a sample failed. The perf
    // block needs an armed attr; if not one sample got that far, there is nothing to
    // write and the failure is the whole story.
    if let Some(attr) = armed_attr {
        let mut images = vec![ImagePin {
            path: args.host_kernel_image.display().to_string(),
            sha256: host_kernel_sha256.clone(),
            md5: String::new(),
            verified_before_boot: true,
        }];
        for (name, l) in &payloads {
            images.push(ImagePin {
                path: args.payload_dir.join(name).display().to_string(),
                sha256: l.sha256.clone(),
                md5: String::new(),
                verified_before_boot: true,
            });
        }
        let context = RunSetContext {
            stage: args.stage,
            run_set_id: args.run_set_id,
            environment: args.environment,
            mechanism: Mechanism {
                kvm_patched: patch_marker,
                host_kernel_sha256,
                expected_exit_reason: match args.mechanism {
                    MechanismArg::Stock => ExitReason::SignalKick,
                    MechanismArg::Patched => ExitReason::Preempt,
                },
                patch_marker_observed: patch_marker,
            },
            images,
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
        let (manifest, records_jsonl) = assemble_run_set(context, &records)
            .map_err(|e| format!("assemble the run-set: {e}"))?;
        std::fs::create_dir_all(&args.out)
            .map_err(|e| format!("create {}: {e}", args.out.display()))?;
        std::fs::write(args.out.join("records.jsonl"), &records_jsonl)
            .map_err(|e| format!("write records.jsonl: {e}"))?;
        std::fs::write(args.out.join("run-set.json"), &manifest)
            .map_err(|e| format!("write run-set.json: {e}"))?;
        println!(
            "wrote {} of {attempted} attempted records to {}",
            records.len(),
            args.out.display()
        );
        println!(
            "NOTE: this harness's verdict is not a disposition. Run `floor-check {}` with the \
             stage's floors; the checker's output is the evidence.",
            args.out.display()
        );
    }

    // A failed sample is reported AFTER the partial evidence is on disk, so the gap is
    // both persisted and surfaced.
    match failure {
        None => Ok(()),
        Some(e) => Err(format!(
            "{e} — {} record(s) of {attempted} were written; the gap is in the evidence",
            records.len()
        )),
    }
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
                payload_dir: opts.payload_dir,
                host_kernel_image: opts.host_kernel_image,
                core: opts.core,
                stage: opts.stage.into(),
                mechanism: opts.mechanism,
                scales: opts.scales.iter().copied().map(Scale::from).collect(),
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
