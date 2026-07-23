// SPDX-License-Identifier: AGPL-3.0-or-later
//! `arm-spike` — the AA-0 capability probe and the run orchestrator.
//!
//! Six subcommands:
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
//! - `linux-boot` — a bounded, explicitly non-certifying AA-5(c) bring-up path for
//!   a hash-pinned arm64 Image + initramfs. It refreshes the pvclock page at exact
//!   retired-branch targets, delivers the dedicated PPI 20 work clockevent, and stops
//!   only after a fixed console marker plus a complete assert/ACK/rearm cycle.
//! - `aa4-guard-reject` — a bounded, hash-pinned planted-exclusive proof. It succeeds
//!   only after the stage-2 guard scans a frozen page, rejects an exclusive-bearing
//!   generation, and leaves the vCPU PC in that unexecuted page.
//! - `aa4-guard-write` — a hash-pinned self-modification and anti-replay proof. It
//!   requires the original page at the synchronous pre-store write exit, rejection
//!   of the superseded approval token, and the exact replacement page at a fresh scan.
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
#[cfg(target_os = "linux")]
use arm_harness::sys::{self, Capability};
#[cfg(target_os = "linux")]
use arm_harness::truth_table::{self, Found, Identity, RowInput, Topology};
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
        /// Distinct target/seed CASES per matrix cell — the dimension that gives the armed
        /// floor a distribution of distinct seeded-random targets. Each case is repeated
        /// `--reps` times for replay identity.
        #[arg(long, default_value_t = 1)]
        cases: u64,
        /// Repetitions of EACH case (same seed + target), for replay identity / the rep
        /// floor. Distinct targets come from `--cases`, not from reps.
        #[arg(long, default_value_t = 1)]
        reps: u64,
        /// Draw seeded-random target deltas over 1..=100000 (AA-3), instead of a
        /// pure counting plan (AA-1).
        #[arg(long)]
        with_targets: bool,
    },
    /// Probe AA-0 and write the complete `truth-table.json` artifact (Linux/box only).
    Probe {
        /// The box config JSON: SoC, firmware, core-assignment topology, and the pinned
        /// expected values for the graded rows (PMUVer, KVM mode).
        #[arg(long)]
        box_config: PathBuf,
        /// Optional JSON map of `{ "<row-id>": "<recorded ruling>" }` — the operator's
        /// explicit dispositions for expected DEVIATIONS (including a favourable one, e.g. ECV
        /// unexpectedly present). A ruled deviation is acceptable; an unruled one gates the RC.
        #[arg(long)]
        rulings: Option<PathBuf>,
        /// Where to write `truth-table.json` (e.g. `results/aa-0/<capture>/truth-table.json`).
        #[arg(long)]
        out: PathBuf,
    },
    /// Run the planned samples and write a run-set (Linux/box only).
    Run(Box<RunOpts>),
    /// Boot hash-pinned arm64 Linux to a fixed console marker (Linux/box only).
    LinuxBoot(Box<LinuxBootOpts>),
    /// Prove a hash-pinned planted-exclusive page is rejected before execution.
    Aa4GuardReject(Box<Aa4GuardRejectOpts>),
    /// Prove write-revokes-execute before modification and forces an exact rescan.
    Aa4GuardWrite(Box<Aa4GuardWriteOpts>),
    /// AA-4 concurrency: prove a memslot update's mmu-notifier invalidation forces the guard
    /// to re-scan an already-approved page (Linux/box only).
    Aa4GuardNotifier(Box<Aa4GuardNotifierOpts>),
    /// AA-4 concurrency: prove moving slot 0 to a distinct byte-identical backing forces a
    /// re-scan (approval keyed to the mapping, not content) (Linux/box only).
    Aa4GuardBacking(Box<Aa4GuardNotifierOpts>),
    /// AA-4 concurrency: prove a second vCPU's write to a page a first vCPU has frozen for a
    /// scan is BLOCKED behind that scan (Linux/box only).
    Aa4GuardRace(Box<Aa4GuardNotifierOpts>),
    /// AA-6(a): install a below-host synthetic ID-register model and prove the guest sees the
    /// frozen values (Linux/box only).
    IdFreeze {
        /// Where to write the enforcement truth-table JSON.
        #[arg(long)]
        out: PathBuf,
    },
    /// AA-6(b): prove the in-kernel vGIC's injection state round-trips through save/restore
    /// bit-identically (Linux/box only).
    VgicRoundtrip {
        /// Where to write the round-trip result JSON.
        #[arg(long)]
        out: PathBuf,
    },
}

const DEFAULT_LINUX_MAX_EXITS: u64 = 5_000_000;
const DEFAULT_LINUX_MAX_CONSOLE_BYTES: usize = 16 << 20;
const DEFAULT_LINUX_BOOTARGS: &str = "console=ttyAMA0 earlycon=pl011,mmio32,0x09000000 \
    keep_bootcon rdinit=/init nokaslr maxcpus=1 random.trust_cpu=off harmony_pvclock nohlt";

/// `arm-spike linux-boot`'s non-certifying substrate options.
#[derive(clap::Args, Debug)]
struct LinuxBootOpts {
    /// Flat arm64 Linux `Image`.
    #[arg(long)]
    image: PathBuf,
    /// Trusted sha256 pin for `--image` (optional `sha256:` prefix).
    #[arg(long)]
    image_sha256: String,
    /// Gzip-compressed or raw cpio initramfs consumed by the kernel.
    #[arg(long)]
    initramfs: PathBuf,
    /// Trusted sha256 pin for `--initramfs` (optional `sha256:` prefix).
    #[arg(long)]
    initramfs_sha256: String,
    /// The isolated host core on which to run the vCPU thread.
    #[arg(long)]
    core: u32,
    /// Kernel command line placed in the generated DTB.
    #[arg(long, default_value = DEFAULT_LINUX_BOOTARGS)]
    bootargs: String,
    /// Fresh path for the captured binary console transcript.
    #[arg(long)]
    console_out: PathBuf,
    /// Maximum KVM exits before the boot is refused.
    #[arg(long, default_value_t = DEFAULT_LINUX_MAX_EXITS)]
    max_exits: u64,
    /// Maximum captured console bytes before the boot is refused.
    #[arg(long, default_value_t = DEFAULT_LINUX_MAX_CONSOLE_BYTES)]
    max_console_bytes: usize,
    /// Measured N1 PMU skid margin for exact work-clock refresh landings.
    #[arg(long)]
    skid_margin: u64,
    /// Maximum retired branches between publications (1..=100,000,000 for the owned guest).
    #[arg(
        long,
        default_value_t = arm_harness::linux_console::DEFAULT_REFRESH_DELTA_WORK
    )]
    refresh_delta_work: u64,
    /// Per-KVM_RUN watchdog seconds; 0 disables it.
    #[arg(long, default_value_t = arm_harness::run::DEFAULT_WATCHDOG_SECS)]
    watchdog_secs: u64,
    /// Enable the AA-4 default-XN stage-2 execute guard before vCPU creation.
    /// Requires the patched capability; clean pages are scanned/approved transparently.
    #[arg(long)]
    stage2_exec_guard: bool,
    /// AA-6: the unwired PPI INTID to inject at a seeded Moment during the boot (never the
    /// clockevent's PPI 20). Requires `--inject-at-work`. Absent = the AA-5(c) negative control
    /// (byte-identical boot). The injection sets a deterministic vGIC pending bit carried in the
    /// register+vGIC digest.
    #[arg(long = "inject-ppi")]
    inject_ppi: Option<u32>,
    /// AA-6: the seeded-random work Moment (the first exact refresh landing at or after this
    /// asserts the injected PPI). Requires `--inject-ppi`.
    #[arg(long = "inject-at-work")]
    inject_at_work: Option<u64>,
    /// AA-6: emit a `LinuxGuest` armed+delivered `RunRecord` (JSON) for the injected boot to this
    /// path, for the mini-gate matrix. The state digest is the register+vGIC digest (the AA-5(c)
    /// identity carrier).
    #[arg(long = "aa6-record")]
    aa6_record: Option<PathBuf>,
    /// AA-6: the seed the emitted `LinuxGuest` record carries (its replay-group key, with
    /// `--inject-at-work`). Keep it and `--inject-at-work` constant across the ≥1000 reps so they
    /// form one same-seed group.
    #[arg(long, default_value_t = 0x5EED_5EED_5EED_5EED)]
    seed: u64,
    /// AA-6: the experimental condition the emitted `LinuxGuest` record carries.
    #[arg(long, default_value = "pinned-solo")]
    condition: String,
}

/// `arm-spike aa4-guard-reject`'s planted runtime proof inputs.
#[derive(clap::Args, Debug)]
struct Aa4GuardRejectOpts {
    /// Bare arm64 ELF containing a planted LDXR/STXR-family instruction.
    #[arg(long)]
    image: PathBuf,
    /// Trusted sha256 pin for `--image` (optional `sha256:` prefix).
    #[arg(long)]
    image_sha256: String,
    /// Isolated host core on which to run the vCPU thread.
    #[arg(long)]
    core: u32,
    /// Per-KVM_RUN watchdog seconds; 0 disables it.
    #[arg(long, default_value_t = arm_harness::run::DEFAULT_WATCHDOG_SECS)]
    watchdog_secs: u64,
}

/// `arm-spike aa4-guard-write`'s planted self-modification proof inputs.
#[derive(clap::Args, Debug)]
struct Aa4GuardWriteOpts {
    /// Bare `aa4-self-modify` ELF built from this tree.
    #[arg(long)]
    image: PathBuf,
    /// Trusted sha256 pin for `--image` (optional `sha256:` prefix).
    #[arg(long)]
    image_sha256: String,
    /// Isolated host core on which to run the vCPU thread.
    #[arg(long)]
    core: u32,
    /// Maximum caller-visible KVM exits before the proof is refused.
    #[arg(long, default_value_t = 1_000_000)]
    max_exits: u64,
    /// Per-KVM_RUN watchdog seconds; 0 disables it.
    #[arg(long, default_value_t = arm_harness::run::DEFAULT_WATCHDOG_SECS)]
    watchdog_secs: u64,
}

/// `arm-spike aa4-guard-notifier`'s inputs.
#[derive(clap::Args, Debug)]
struct Aa4GuardNotifierOpts {
    /// Bare `aa4-reexec` ELF built from this tree.
    #[arg(long)]
    image: PathBuf,
    /// Trusted sha256 pin for `--image` (optional `sha256:` prefix).
    #[arg(long)]
    image_sha256: String,
    /// Isolated host core on which to run the vCPU thread.
    #[arg(long)]
    core: u32,
    /// Maximum caller-visible KVM exits before the proof is refused.
    #[arg(long, default_value_t = 1_000_000)]
    max_exits: u64,
    /// Per-KVM_RUN watchdog seconds; 0 disables it.
    #[arg(long, default_value_t = arm_harness::run::DEFAULT_WATCHDOG_SECS)]
    watchdog_secs: u64,
}

/// `arm-spike run`'s options. Boxed in the [`Command`] enum: it is far larger than
/// the other variants, and clap parses it straight into this shape.
const DEFAULT_AA2_MAX_STEPS: u64 = 12_000;

#[derive(clap::Args, Debug)]
struct RunOpts {
    /// Directory of built payload ELFs, one per class, named by
    /// [`oracle_model::Payload::name`] (the same layout `arm-scan windows` reads).
    /// Every planned sample boots the ELF that matches ITS payload — booting one ELF
    /// under every class's label would produce mislabeled evidence.
    #[arg(long)]
    payload_dir: PathBuf,
    /// A JSON object of **trusted expected sha256 pins**, `{ "<payload>": "<sha256>" }`,
    /// one per payload class. Each loaded ELF is hashed and compared against its pin;
    /// a mismatch (a swapped or rebuilt artifact) is a hard error, and only a match
    /// attests `verified_before_boot`. Without this, the harness would hash whatever
    /// bytes are present and hand a changed artifact a fresh accepted identity —
    /// exactly what §Evidence integrity #3 forbids.
    #[arg(long)]
    payload_pins: PathBuf,
    /// The running host kernel image (e.g. `/boot/Image`).
    #[arg(long)]
    host_kernel_image: PathBuf,
    /// The **trusted expected sha256** of the running host kernel, from the box's boot
    /// record. The `--host-kernel-image` bytes are hashed and compared against it; a
    /// mismatch means the image is not the one the operator vouched is running, and is
    /// a hard error. (Binding the file to the *live* kernel is a box-day step; the
    /// operator asserts identity by pinning, and the harness verifies the bytes match.)
    #[arg(long)]
    host_kernel_sha256: String,
    /// The **GNU build-id of the running kernel** the operator built and booted. The harness
    /// reads the LIVE kernel's build-id from `/sys/kernel/notes` and requires a match before
    /// attesting `verified_before_boot` — a file hash alone proves only that a FILE matches a
    /// pin, not that it is the image actually running (a stale or newly-installed
    /// `/boot/Image` hashes fine while another kernel is booted). The build-id is the boot
    /// measurement that identifies the running image.
    #[arg(long)]
    host_kernel_build_id: String,
    /// The core to hard-pin the vCPU thread to. Pinning is a correctness condition on
    /// this lineage (rr #3607), not hygiene.
    #[arg(long)]
    core: u32,
    /// Run AA-1's **bounded migration probe**: deliberately ROTATE the vCPU thread across
    /// the allowed cpuset (one core per sample) so it genuinely moves under armed overflow,
    /// to observe the rr #3607 failure mode (lost PMI → hang vs delayed) on this exact
    /// kernel/silicon. Merely leaving the thread unpinned is not enough — on a quiet host
    /// the scheduler may leave it on one CPU for the whole run, exercising no migration.
    /// Needs ≥2 allowed cores (a single-core lease is refused). The evidence records
    /// `pinned: false, migration_probe: true` — the honest posture (not pinned to one core)
    /// — so the checker accepts the missing single-core pin only for this sanctioned probe,
    /// never as a normal run. Re-pin permanently after (`docs/ARM-ALTRA.md` §AA-1).
    #[arg(long)]
    migration_probe: bool,
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
    /// Distinct target/seed CASES per matrix cell — the distribution of seeded-random
    /// targets the armed floor is over. Each case is repeated `--reps` times.
    #[arg(long, default_value_t = 1)]
    cases: u64,
    /// Repetitions of EACH case (same seed + target), for replay identity / the rep floor.
    #[arg(long, default_value_t = 1)]
    reps: u64,
    /// Draw seeded-random target deltas over 1..=100000 (AA-3).
    #[arg(long)]
    with_targets: bool,
    /// AA-6: inject the given PPI INTID at each exact landed `Moment` (the seeded-random target).
    /// Absent = the NEGATIVE CONTROL: the default AA-3/AA-5 deterministic path is byte-identical
    /// (no `KVM_IRQ_LINE` is issued, the state digest is the pre-hook value). The spike injects
    /// the harness's dedicated unowned line, PPI **20** (never the KVM-owned architected-timer
    /// PPI 27). Only meaningful with a patched-mechanism armed run (`--with-targets --skid-margin`).
    #[arg(long = "inject-ppi")]
    inject_ppi: Option<u32>,
    /// Payload class(es) to EXCLUDE from the plan (by name, repeatable). At AA-3 the exact
    /// landing passes `--exclude-payload wfi-idle`: its WFI is resumed by a real-time timer that
    /// shifts under the slow single-step, so under the exact landing it loses PMIs and exits
    /// without closing its window — an AA-5 (paravirt-clock) breakage, not a force-exit failure.
    /// The run grades on the seven deterministic-count payloads rather than fabricating wfi-idle
    /// evidence it cannot produce.
    #[arg(long = "exclude-payload")]
    exclude_payloads: Vec<String>,
    /// AA-2: arm KVM single-step and emit one record per stepped instruction, instead of
    /// measuring a counting window. Implied by `--stage aa2`. A stepped run is smoke-scale by
    /// construction — a 1e6 window is millions of steps — so keep the scales small.
    #[arg(long)]
    single_step: bool,
    /// AA-2: cap a single-step run at N steps (default 12000; explicit 0 is unbounded).
    /// A bounded run stops at N steps OR the sentinel, whichever comes first — hitting N first
    /// is normal, not a failure. The finite default fail-closes the `llsc-atomics` livelock: each
    /// step clears the exclusive monitor, so `STXR` never succeeds and the retry loops forever,
    /// never reaching MARK_END or the sentinel. Every stepped record is registers-only except
    /// the last, which carries the full-payload digest, so replay-identity still catches memory
    /// divergence across the stepped window. Only meaningful with `--single-step`/`--stage aa2`.
    #[arg(long, default_value_t = DEFAULT_AA2_MAX_STEPS)]
    max_steps: u64,
    /// Per-`KVM_RUN` watchdog budget in seconds; 0 disables. A wedged guest past this
    /// deadline is recorded as a failed attempt rather than hanging the run.
    #[arg(long, default_value_t = arm_harness::run::DEFAULT_WATCHDOG_SECS)]
    watchdog_secs: u64,
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

// An internal plan-builder: each argument is one plan dimension threaded straight from the CLI
// (seed, cases, reps, with-targets, scales, condition, target lower bound, payload exclusions).
// Bundling them into a struct would only rename the same fields; the flat list is the plan shape.
#[allow(clippy::too_many_arguments)]
fn plan_spec(
    seed: u64,
    cases: u64,
    reps: u64,
    with_targets: bool,
    scales: Vec<Scale>,
    condition: &str,
    // The LOWER bound of the seeded-random target draw. 1 for AA-1/AA-3-proxy; for the AA-3
    // EXACT landing it is `skid_margin + LANDING_HEADROOM + 1`, because a target closer to
    // `MARK_BEGIN` than that combined margin cannot be armed strictly below and single-stepped up
    // to its canonical landing PC — those deltas are simply not drawn, rather than drawn and then
    // failed.
    target_lo: u64,
    // Payload names to EXCLUDE from the plan. Empty for AA-1/AA-2. At AA-3 the exact landing
    // excludes `wfi-idle`: its WFI is resumed by a real-time timer whose firing shifts under the
    // slow single-step, so under the exact landing it loses PMIs and exits without closing its
    // window — an AA-5 (paravirt-clock) breakage, not a force-exit failure (foreman ruling: the
    // ≥10⁶ run grades on the seven deterministic-count payloads). Excluding it is honest: the run
    // does not fabricate wfi-idle exact-landing evidence it cannot produce.
    exclude_payloads: &[String],
) -> PlanSpec {
    PlanSpec {
        payloads: ALL_PAYLOADS
            .iter()
            .copied()
            .filter(|p| p.has_window())
            .filter(|p| !exclude_payloads.iter().any(|x| x == p.name()))
            .collect(),
        scales,
        conditions: vec![condition.to_string()],
        cases,
        reps,
        seed,
        target_delta_range: with_targets.then_some((target_lo, 100_000)),
    }
}

fn emit_plan(seed: u64, cases: u64, reps: u64, with_targets: bool) -> Result<(), String> {
    let samples = plan(&plan_spec(
        seed,
        cases,
        reps,
        with_targets,
        vec![Scale::Smoke],
        "pinned-solo",
        1,
        &[],
    ))
    .map_err(|e| e.to_string())?;
    let json =
        serde_json::to_string_pretty(&samples).map_err(|e| format!("serialize plan: {e}"))?;
    println!("{json}");
    Ok(())
}

/// The box-specific configuration the harness cannot read from a register: the SoC name,
/// firmware versions, the standing core-assignment topology, and the operator's pinned
/// expectations for the two GRADED rows (PMUVer, KVM mode). Supplied as JSON, like the run
/// environment — the harness measures everything it can and the operator vouches for the rest.
#[cfg(target_os = "linux")]
#[derive(serde::Deserialize)]
struct BoxConfig {
    /// The SoC part string (not in any register).
    soc: String,
    /// Firmware versions, key/value.
    firmware: std::collections::BTreeMap<String, String>,
    /// The standing core-assignment table.
    topology: Topology,
    /// The pinned expected `ID_AA64DFR0_EL1.PMUVer` (e.g. `"0x6"`) — a graded row.
    pmuver_expected: String,
    /// The pinned expected KVM mode (`"vhe"`/`"nvhe"`) — a graded row.
    kvm_mode_expected: String,
}

/// Probe one capability into `(found, raw)`. `Unprobed` is never collapsed into `Absent`: a
/// row that could not be read is a deviation the truth table records, not a clean "no".
#[cfg(target_os = "linux")]
fn probe_cap(cap: Capability) -> (Found, String) {
    match sys::probe(cap) {
        Ok(true) => (
            Found::Present,
            format!("{}: probe returned present", cap.name()),
        ),
        Ok(false) => (
            Found::Absent,
            format!("{}: probe returned absent", cap.name()),
        ),
        Err(e) => (
            Found::Unprobed,
            format!("{}: probe could not run: {e}", cap.name()),
        ),
    }
}

/// Emit AA-0's complete, machine-readable, reboot-diffable `truth-table.json` (finding r16):
/// all thirteen mandatory rows (host caps + ID-register facts), the machine identity, and the
/// standing core-assignment topology. The RC is nonzero if ANY row is a deviation (found !=
/// expected) — including a favourable one — because AA-0's acceptance is that every row is
/// confirmed or has an explicit recorded ruling, which a machine cannot invent.
#[cfg(target_os = "linux")]
fn probe(box_config: PathBuf, rulings: Option<PathBuf>, out: PathBuf) -> Result<(), String> {
    let cfg: BoxConfig = read_json(&box_config)?;
    let regs = sys::read_host_id_registers().map_err(|e| format!("read host ID registers: {e}"))?;
    let host_kernel = sys::running_kernel_release().map_err(|e| format!("read uname: {e}"))?;
    // The MACHINE's online CPU count — NOT available_parallelism(), which under taskset / a
    // systemd CPU set / a leased partition reports the calling process's allowance, not the
    // Altra's topology.
    let core_count =
        sys::online_cpu_count().map_err(|e| format!("read the online-CPU set: {e}"))?;
    // The EFFECTIVE KVM mode KVM selected at boot, not the architectural VHE feature bit
    // (VH stays nonzero on an nvhe-booted host).
    let kvm_mode = sys::kvm_mode()
        .map_err(|e| format!("read the KVM mode: {e}"))?
        .unwrap_or_else(|| "unknown".to_string());

    let f4 = |v: u64, shift: u32| (v >> shift) & 0xf;
    let present = |b: bool| if b { Found::Present } else { Found::Absent };
    let mut rows: Vec<RowInput> = Vec::new();

    // The host capability rows (the KVM/perf probes).
    let cap_rows = [
        (
            Capability::DevKvm,
            "dev-kvm",
            "kvm",
            "/dev/kvm present and openable?",
        ),
        (
            Capability::PerfBrRetired,
            "perf-raw-0x21-pinned",
            "perf",
            "raw event 0x21 opens pinned + non-multiplexed and counts?",
        ),
        (
            Capability::GuestDebug,
            "kvm-cap-set-guest-debug",
            "kvm",
            "KVM_CAP_SET_GUEST_DEBUG (single-step) advertised?",
        ),
        (
            Capability::Pmceid,
            "br-retired-pmceid1",
            "perf",
            "BR_RETIRED (0x21) is PMCEID1-implemented (events/br_retired = event=0x21)?",
        ),
        (
            Capability::HostOverflowDelivers,
            "host-overflow-delivers",
            "perf",
            "a host BR_RETIRED overflow actually delivers a sample?",
        ),
        (
            Capability::Vgicv3Creatable,
            "vgicv3-creatable",
            "kvm",
            "an in-kernel GICv3 is creatable?",
        ),
        (
            Capability::WritableIdRegisters,
            "writable-id-registers",
            "kvm",
            "a below-host ID-register feature installs and reads back?",
        ),
    ];
    for (cap, id, kind, q) in cap_rows {
        let (found, raw) = probe_cap(cap);
        rows.push(RowInput::cap(id, kind, q, Found::Present, found, raw));
    }
    // The patch marker: expect ABSENT on a stock kernel (present attests the patched one).
    let (di_found, di_raw) = probe_cap(Capability::DeterministicIntercepts);
    rows.push(RowInput::cap(
        "kvm-cap-arm-deterministic-intercepts",
        "kvm",
        "the 0004-analogue determinism cap advertised? Expect absent on a stock kernel.",
        Found::Absent,
        di_found,
        di_raw,
    ));
    // AA-4's stock-KVM feasibility marker. Absence is the honest result until the
    // dedicated per-GFN execute-guard patch exists; a generic MEMORY_FAULT cap is not it.
    let (wx_found, wx_raw) = probe_cap(Capability::Stage2ExecGuard);
    rows.push(RowInput::cap(
        "kvm-cap-arm-stage2-exec-guard",
        "kvm",
        "the AA-4 per-GFN stage-2 execute guard advertised? Expect absent on stock KVM.",
        Found::Absent,
        wx_found,
        wx_raw,
    ));

    // The ID-register rows (read from a disposable VM's vCPU).
    let ecv = f4(regs.id_aa64mmfr0, 60);
    rows.push(RowInput::cap(
        "ecv",
        "id-register",
        "ID_AA64MMFR0_EL1.ECV — FEAT_ECV present? Expect absent (the paravirt-clock premise).",
        Found::Absent,
        present(ecv != 0),
        format!("ID_AA64MMFR0_EL1.ECV = {ecv:#x}"),
    ));
    let lse = f4(regs.id_aa64isar0, 20);
    rows.push(RowInput::cap(
        "lse",
        "id-register",
        "ID_AA64ISAR0_EL1.Atomic — FEAT_LSE present? Expect present (AA-4's premise).",
        Found::Present,
        present(lse != 0),
        format!("ID_AA64ISAR0_EL1.Atomic = {lse:#x}"),
    ));
    let sve = f4(regs.id_aa64pfr0, 32);
    rows.push(RowInput::cap(
        "sve",
        "id-register",
        "ID_AA64PFR0_EL1.SVE — FEAT_SVE present? Expect absent on N1.",
        Found::Absent,
        present(sve != 0),
        format!("ID_AA64PFR0_EL1.SVE = {sve:#x}"),
    ));
    // NV is ID_AA64MMFR2_EL1[27:24]; [35:32] is the AT field. Reading AT would report nested
    // virtualization present on an N1 that has AT but not NV, mislabelling this mandatory row.
    let nv = f4(regs.id_aa64mmfr2, 24);
    rows.push(RowInput::cap(
        "nested-virt",
        "id-register",
        "ID_AA64MMFR2_EL1.NV — FEAT_NV present? Expect absent.",
        Found::Absent,
        present(nv != 0),
        format!("ID_AA64MMFR2_EL1.NV = {nv:#x}"),
    ));
    // Graded rows: PMUVer and the KVM mode (VHE vs nVHE), against the operator's pinned values.
    let pmuver = f4(regs.id_aa64dfr0, 8);
    rows.push(RowInput {
        id: "pmuver",
        kind: "pmu",
        question: "ID_AA64DFR0_EL1.PMUVer — the PMU version behind the BR_RETIRED work-clock bet.",
        expected: cfg.pmuver_expected.trim().to_string(),
        found: format!("{pmuver:#x}"),
        raw: format!(
            "ID_AA64DFR0_EL1.PMUVer = {pmuver:#x} (read from a {} vCPU)",
            if regs.pmu_v3_enabled {
                "KVM_ARM_VCPU_PMU_V3-enabled"
            } else {
                "featureless — host refused the vPMU init, so PMUVer is KVM's mask, not \
                 the host PMU version"
            }
        ),
    });
    let vh = f4(regs.id_aa64mmfr1, 8);
    rows.push(RowInput {
        id: "kvm-mode",
        kind: "kvm",
        question: "The EFFECTIVE KVM mode (kvm_arm.mode), not the architectural VHE bit.",
        expected: cfg.kvm_mode_expected.trim().to_string(),
        found: kvm_mode.clone(),
        raw: format!(
            "/sys/module/kvm_arm/parameters/mode = {kvm_mode} (ID_AA64MMFR1_EL1.VH = {vh:#x})"
        ),
    });

    let identity = Identity {
        midr: regs.midr,
        implementer: ((regs.midr >> 24) & 0xff) as u8,
        part_num: ((regs.midr >> 4) & 0xfff) as u16,
        variant: ((regs.midr >> 20) & 0xf) as u8,
        revision: (regs.midr & 0xf) as u8,
        soc: cfg.soc,
        core_count,
        host_kernel,
        firmware: cfg.firmware,
    };

    // The operator's recorded dispositions for expected deviations (row-id → ruling).
    let rulings: std::collections::BTreeMap<String, String> = match &rulings {
        Some(path) => read_json(path)?,
        None => std::collections::BTreeMap::new(),
    };

    let table = truth_table::assemble(identity, cfg.topology, rows, &rulings);

    // Validate the COMPLETE emitted table against the canonical schema BEFORE writing it or
    // reporting success. `assemble` is pure logic over probed values, so schema-invalid
    // operator metadata (an empty `soc` or `topology.governor`, a short row set) would
    // otherwise serialize fine and pass the gate, which only inspects unresolved rows — a
    // schema-violating AA-0 artifact under a green gate. A malformed table is not evidence, so
    // it is not written at all.
    let violations = table.schema_violations();
    if !violations.is_empty() {
        return Err(format!(
            "the assembled truth table violates schemas/truth-table.schema.json and was NOT \
             written: {}",
            violations.join("; ")
        ));
    }

    let json =
        serde_json::to_string_pretty(&table).map_err(|e| format!("serialize truth table: {e}"))?;
    // Exclusive-create: a reused `--out` path must NOT be silently truncated. The post-reboot
    // capture is diffed byte-for-byte against the first, so an existing golden is immutable
    // evidence — reject rather than overwrite (as the run-set writer does).
    {
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&out)
            .map_err(|e| {
                format!(
                    "create {}: {e} — refusing to overwrite an existing AA-0 capture (the \
                     post-reboot comparison is byte-for-byte; write to a fresh --out)",
                    out.display()
                )
            })?;
        f.write_all(format!("{json}\n").as_bytes())
            .map_err(|e| format!("write {}: {e}", out.display()))?;
    }

    for r in &table.rows {
        let tag = match (&r.disposition, r.confirmed) {
            (_, true) => "ok      ",
            (Some(d), false) if d != truth_table::UNRULED_DEVIATION => "RULED   ",
            _ => "UNRULED ",
        };
        println!("{tag} {}: expected {}, found {}", r.id, r.expected, r.found);
    }
    // Acceptance gates only UNRESOLVED deviations: a ruled one (even favourable) is
    // acceptable. The artifact is always written — a deviation is a finding to record and
    // diff, not to hide.
    let unresolved = table.unresolved();
    if unresolved.is_empty() {
        println!(
            "truth-table.json written to {}: {} rows, every deviation confirmed or ruled",
            out.display(),
            table.rows.len()
        );
        Ok(())
    } else {
        Err(format!(
            "{} row(s) deviate with NO recorded ruling: {} — supply dispositions via --rulings \
             (truth-table.json written to {})",
            unresolved.len(),
            unresolved.join(", "),
            out.display()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn probe(_box_config: PathBuf, _rulings: Option<PathBuf>, _out: PathBuf) -> Result<(), String> {
    Err(
        "`arm-spike probe` issues KVM/perf syscalls and reads /sys to build truth-table.json: \
         it is Linux-only, and this host is not Linux."
            .into(),
    )
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
    payload_pins: std::collections::BTreeMap<String, String>,
    host_kernel_image: PathBuf,
    host_kernel_sha256: String,
    host_kernel_build_id: String,
    core: u32,
    migration_probe: bool,
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
    cases: u64,
    reps: u64,
    with_targets: bool,
    exclude_payloads: Vec<String>,
    single_step: bool,
    max_steps: u64,
    watchdog_secs: u64,
    /// AA-6 injection config (`--inject-ppi`), or `None` for the byte-identical negative control.
    inject: Option<arm_harness::run::InjectionConfig>,
}

/// A payload ELF loaded from the payload directory, hashed and verified against its
/// trusted pin.
#[cfg(target_os = "linux")]
struct LoadedPayload {
    image: arm_harness::elf::Elf,
    sha256: String,
}

/// Normalise a recorded/expected sha256: drop an optional `sha256:` prefix, lowercase.
#[cfg(target_os = "linux")]
fn normalise_sha256(h: &str) -> String {
    h.trim()
        .strip_prefix("sha256:")
        .unwrap_or(h.trim())
        .to_ascii_lowercase()
}

/// Read and verify one boot artifact against an operator-supplied content pin.
#[cfg(target_os = "linux")]
fn verified_artifact(
    path: &std::path::Path,
    expected: &str,
    label: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, String), String> {
    use std::io::Read as _;

    use arm_harness::evidence::hex_lower;
    use sha2::{Digest, Sha256};

    let file = std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let read_limit = max_bytes
        .checked_add(1)
        .ok_or_else(|| format!("{label} byte limit overflows"))?;
    let mut bytes = Vec::new();
    file.take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("read {}: {e}", path.display()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(format!(
            "{label} {} exceeds the fixed-layout {max_bytes}-byte input limit",
            path.display()
        ));
    }
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    let actual = hex_lower(&hasher.finalize());
    let expected = normalise_sha256(expected);
    if expected.len() != 64 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "trusted {label} sha256 must be exactly 64 hexadecimal digits (optional sha256: \
             prefix); got `{expected}`"
        ));
    }
    if actual != expected {
        return Err(format!(
            "{label} {} hashes to {actual}, but its trusted pin is {expected}: refusing to boot \
             an unpinned artifact",
            path.display()
        ));
    }
    Ok((bytes, actual))
}

#[cfg(target_os = "linux")]
fn linux_boot(opts: LinuxBootOpts) -> Result<(), String> {
    use std::io::Write as _;

    use arm_harness::evidence::hex_lower;
    use arm_harness::linux_console::{
        LinuxConsoleConfig, LinuxWorkClockConfig, run_until_ready_work_clock,
    };
    use arm_harness::run::{StepVcpu as _, Vcpu as _};
    use arm_harness::sys::{Machine, Mechanism, PerfCounter, pin_to_core};
    use sha2::{Digest, Sha256};

    // Verify both byte strings immediately before constructing the VM. The hashes
    // are operator-vouched identities, not freshly-minted identities for whatever
    // happened to be at each path.
    let (image, image_sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "Linux Image",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let (initramfs, initramfs_sha256) = verified_artifact(
        &opts.initramfs,
        &opts.initramfs_sha256,
        "Linux initramfs",
        arm_harness::linux_boot::MAX_INITRAMFS_BYTES,
    )?;

    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;
    let mut machine = if opts.stage2_exec_guard {
        Machine::new_linux_guarded(&image, &initramfs, &opts.bootargs)
    } else {
        Machine::new_linux(&image, &initramfs, &opts.bootargs)
    }
    .map_err(|e| format!("construct Linux KVM machine: {e}"))?;
    machine.set_watchdog_secs(opts.watchdog_secs);
    let guest_clock_hz = machine
        .linux_cntfrq_hz()
        .map_err(|e| format!("read guest CNTFRQ_EL0: {e}"))?;
    let mut counter =
        PerfCounter::open(&machine, Mechanism::Preempt, Some(opts.refresh_delta_work))
            .map_err(|e| format!("open patched BR_RETIRED work counter: {e}"))?;
    let result = run_until_ready_work_clock(
        &mut machine,
        &mut counter,
        &LinuxConsoleConfig {
            ready_marker: arm_harness::linux_console::LINUX_READY_MARKER.to_vec(),
            max_exits: opts.max_exits,
            max_console_bytes: opts.max_console_bytes,
        },
        LinuxWorkClockConfig {
            refresh_delta_work: opts.refresh_delta_work,
            skid_margin: opts.skid_margin,
            guest_clock_hz,
        },
        // AA-6 injection into the boot: `Some` when the operator supplies `--inject-ppi` +
        // `--inject-at-work` (the AA-6 LinuxGuest gate); `None` is the AA-5(c) negative control,
        // byte-identical.
        opts.inject_ppi
            .zip(opts.inject_at_work)
            .map(
                |(intid, target_work)| arm_harness::linux_console::LinuxInjection {
                    intid,
                    target_work,
                },
            ),
    )
    .map_err(|e| format!("boot Linux: {e}"))?;
    let guard = machine.exec_guard_stats();
    if opts.stage2_exec_guard
        && !matches!(guard, Some(stats) if stats.exits > 0 && stats.scans > 0 && stats.approvals > 0)
    {
        return Err(format!(
            "stage-2 execute guard was requested but no non-vacuous execute/scan/approval \
             sequence was observed: {guard:?}"
        ));
    }
    let guard = guard.unwrap_or_default();

    // The run ends at the canonical landing of the first exact publication after the
    // marker latched, so this digest is landing-anchored state identity — the value the
    // AA-5(c) same-seed gate compares across runs (console alone cannot prove state).
    let state_digest = machine
        .state_digest()
        .map_err(|e| format!("digest final machine state: {e}"))?;
    // A registers-only (+ vGIC) digest of the SAME landed state, emitted alongside the
    // full state digest (hm-of6t F12). The full state digest carries the kernel-CRNG
    // entropy residual and diverges same-seed; the registers-only digest isolates
    // architectural register identity, which holds bit-identical on the pinned nokaslr
    // image — so the register-identity claim rests on the pinned build itself, not a
    // separate diag build. `digest_regs_only` can never collide with the full digest.
    let regs_digest = machine
        .regs_digest()
        .map_err(|e| format!("digest final register state: {e}"))?;
    // Core registers only (excludes the vGIC injection state): isolates architectural
    // register identity so the same-seed gate can attribute a regs_digest divergence to
    // the host-IRQ-timing vGIC vs the core registers (hm-of6t F12).
    let core_regs_digest = machine
        .core_regs_digest()
        .map_err(|e| format!("digest final core register state: {e}"))?;
    // AA-6 masked-register digest (hm-3bwm) at the success landing: the full register file
    // MINUS exactly {x29, SP} (host-time counters already excluded). The named condition on
    // the AA-6 LinuxGuest PROVISIONAL→full-GO upgrade — bit-identical across ≥1000 same-seed
    // injected reps proves change #4's console+vGIC narrowing masks exactly-and-only the
    // disclosed stack-ASLR residual, not an injection-path register divergence.
    let masked_regs_digest = machine
        .masked_regs_digest()
        .map_err(|e| format!("digest final masked register state: {e}"))?;
    // The masked digest's exclusion set, enumerated (not implied) so the evidence output
    // states EXACTLY what is dropped: the two masked general registers by full KVM id, and
    // the pre-existing host-time counter exclusion by name. Widening either is a P0 STOP.
    let masked_excluded_gprs = format!(
        "x29:{:#018x},SP:{:#018x}",
        arm_harness::sys::kvm::REG_CORE_X29,
        arm_harness::sys::kvm::REG_CORE_SP,
    );
    let masked_excluded_host_time = "CNTPCT_EL0,CNTPCTSS_EL0,CNTVCTSS_EL0,KVM_REG_ARM_TIMER_CNT";
    // hm-fiqo: the injection-Moment masked-register witness, emitted (no longer discarded);
    // `none` on the negative-control OFF path where no injection fired.
    let injected_landed_digest = result.injected_landed_digest.as_deref().unwrap_or("none");
    // The AA-6 injection attestation the boot loop ACTUALLY executed (bead hm-oh3v): the loop
    // injects iff BOTH `--inject-ppi` and `--inject-at-work` were supplied (`Option::zip`), so
    // `injection_enabled` reflects the config that ran, and the two parameters are enumerated.
    // Emitted in the same summary line the masked-digest lane parses, so that lane's checker and
    // the floor checker's `aa6-matrix` read one stamped injection posture, never two that can
    // disagree. `injected_landed_digest` (above) is the independent per-rep FIRED witness.
    let injection_enabled = if opts.inject_ppi.is_some() && opts.inject_at_work.is_some() {
        "ON"
    } else {
        "OFF"
    };
    let inject_ppi = opts
        .inject_ppi
        .map_or_else(|| "none".to_string(), |p| p.to_string());
    let inject_at_work = opts
        .inject_at_work
        .map_or_else(|| "none".to_string(), |w| w.to_string());

    // AA-6 LinuxGuest record emission (the mini-gate matrix's 9th class). A `LinuxGuest` armed+
    // delivered RunRecord whose overflow landed at the injected Moment and whose `state_digest`
    // is the register+vGIC digest (the AA-5(c) identity carrier — full-RAM has the CRNG residual).
    // The window fields are 0: `LinuxGuest` has no counting window (the oracle expects 0), so
    // count-exactness reads 0 == 0 while the injected work Moment lives in the overflow target.
    if let Some(record_path) = &opts.aa6_record {
        let injected_at = result.injected_at_work.ok_or_else(|| {
            "aa6-record was requested but no injection fired during the boot — supply \
             --inject-ppi and a small enough --inject-at-work that it lands before the boot's \
             success gate"
                .to_string()
        })?;
        // The LinuxGuest AA-6 determinism carrier for the RECORD: console + vGIC injection
        // state (change #4). The full register digest diverges same-seed on the userspace
        // stack-placement ASLR (the AA-5(c) kernel-CRNG residual, hm-of6t F12) — orthogonal to
        // injection — so the record's compared digest is the AA-5(c)-proven-deterministic
        // console plus the vGIC state that carries the injected pending interrupt. The
        // masked-register digest and the injection-Moment witness (`injected_landed_digest`,
        // hm-fiqo) — the hm-3bwm named-condition evidence — are emitted in the summary line
        // below, the lane that proves this narrowing masks exactly-and-only {x29, SP}.
        let aa6_digest = machine
            .console_vgic_digest(&result.boot.console)
            .map_err(|e| format!("digest the AA-6 LinuxGuest console+vGIC state: {e}"))?;
        let record = arm_harness::evidence::RunRecord {
            sample_id: 0,
            payload: oracle_model::Payload::LinuxGuest,
            scale: Scale::Smoke,
            seed: opts.seed,
            trips: oracle_model::trips(oracle_model::Payload::LinuxGuest, Scale::Smoke),
            condition: opts.condition.clone(),
            work_begin: 0,
            work_end: 0,
            measured_taken: 0,
            reported_taken: 0,
            exit_reason: arm_harness::evidence::ExitReason::Preempt,
            overflow: Some(arm_harness::evidence::OverflowRecord {
                armed: true,
                deliveries: 1,
                advisory_exits: 0,
                target: injected_at,
                landed: injected_at,
                skid: 0,
                landed_digest: aa6_digest.clone(),
            }),
            // The injection fired at this Moment — `injected_at` above is `Some`, or this
            // record would not have been emitted (the `ok_or_else` refusal). The per-record
            // witness the AA-6 matrix checker cross-checks against the stamped attestation.
            injected: Some(true),
            step: None,
            // The AA-6 LinuxGuest determinism carrier: console + vGIC injection state (the
            // AA-5(c)-deterministic console + the injected pending interrupt), NOT the full
            // register digest — that carries the disclosed userspace stack-ASLR residual.
            state_digest: aa6_digest,
            params_mode: "managed".into(),
            clockpage_mode: Some("work-derived".into()),
            payload_status: 0,
        };
        let line = serde_json::to_string(&record)
            .map_err(|e| format!("serialize the AA-6 LinuxGuest record: {e}"))?;
        use std::io::Write as _;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(record_path)
            .map_err(|e| format!("open {}: {e}", record_path.display()))?;
        writeln!(f, "{line}").map_err(|e| format!("append {}: {e}", record_path.display()))?;
    }

    // Diagnostic (env-gated, off by default): when the same-seed gate flags a RAM-only
    // divergence, `AA5_DUMP_RAM=<path>` writes the final guest RAM so two runs' dumps can
    // be byte-diffed and mapped through System.map to the diverging kernel object.
    if let Some(path) = std::env::var_os("AA5_DUMP_RAM") {
        std::fs::write(&path, machine.guest_ram_bytes())
            .map_err(|e| format!("write {}: {e}", std::path::Path::new(&path).display()))?;
    }
    // Diagnostic (env-gated): `AA5_DUMP_REGS=<path>` writes the per-register dump so two
    // same-seed runs can be diffed to attribute a regs_digest divergence to the exact
    // register(s) — hm-of6t F12 register-residual attribution.
    if let Some(path) = std::env::var_os("AA5_DUMP_REGS") {
        let dump = machine
            .regs_dump_text()
            .map_err(|e| format!("dump registers: {e}"))?;
        std::fs::write(&path, dump)
            .map_err(|e| format!("write {}: {e}", std::path::Path::new(&path).display()))?;
    }

    let mut transcript = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&opts.console_out)
        .map_err(|e| {
            format!(
                "create {}: {e} — refusing to overwrite a prior boot transcript",
                opts.console_out.display()
            )
        })?;
    transcript
        .write_all(&result.boot.console)
        .map_err(|e| format!("write {}: {e}", opts.console_out.display()))?;
    transcript
        .sync_all()
        .map_err(|e| format!("sync {}: {e}", opts.console_out.display()))?;

    let mut hasher = Sha256::new();
    hasher.update(&result.boot.console);
    let console_sha256 = hex_lower(&hasher.finalize());
    println!(
        "NON-CERTIFYING Linux boot reached its fixed console marker: exits={} console_bytes={} \
         console_sha256={} image_sha256={} initramfs_sha256={} pvclock_publications={} \
         pvclock_max_gap_work={} pvclock_last_work={} pvclock_gpa={:#x} guest_clock_hz={} \
         clockevent_assertions={} clockevent_acks={} clockevent_max_lateness_ticks={} \
         exec_guard_enabled={} exec_guard_exits={} exec_guard_scans={} exec_guard_approvals={} \
         exec_guard_rejections={} exec_guard_write_revocations={} exec_guard_blocked_writes={} \
         state_digest={} regs_digest={} core_regs_digest={} masked_regs_digest={} \
         injected_landed_digest={} injection_enabled={} inject_ppi={} inject_at_work={} \
         masked_excluded_gprs={} masked_excluded_host_time={} \
         transcript={}",
        result.boot.exits,
        result.boot.console.len(),
        console_sha256,
        image_sha256,
        initramfs_sha256,
        result.publications,
        result.max_refresh_gap_work,
        result.last_refresh_work,
        result.registration_gpa,
        guest_clock_hz,
        result.clockevent_assertions,
        result.clockevent_acknowledgements,
        result.clockevent_max_lateness_ticks,
        opts.stage2_exec_guard,
        guard.exits,
        guard.scans,
        guard.approvals,
        guard.rejections,
        guard.write_revocations,
        guard.blocked_writes,
        state_digest,
        regs_digest,
        core_regs_digest,
        masked_regs_digest,
        injected_landed_digest,
        injection_enabled,
        inject_ppi,
        inject_at_work,
        masked_excluded_gprs,
        masked_excluded_host_time,
        opts.console_out.display()
    );
    println!(
        "AA-5 remains open: the exact-work page and dedicated PPI 20 clockevent substrate are \
         built, but this command does not claim same-seed live N1 identity, a native pinned-N1 \
         artifact build, or AA-4's planted rejection/write-before-modification proof"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn linux_boot(_opts: LinuxBootOpts) -> Result<(), String> {
    Err(
        "`arm-spike linux-boot` issues KVM ioctls and needs /dev/kvm: it is Linux-only; \
         artifact parsing and the console loop are tested portably on this host."
            .into(),
    )
}

/// Run a planted-exclusive ELF only far enough for the guard to reject its frozen page.
///
/// This is deliberately a rejection proof, not a generic payload run: success requires an
/// exit-43 scan generation, at least one decoded exclusive, a successful REJECT response, and
/// the vCPU PC still pointing into the rejected page. A normal exit or a counter-only rejection
/// cannot satisfy it.
#[cfg(target_os = "linux")]
fn aa4_guard_reject(opts: Aa4GuardRejectOpts) -> Result<(), String> {
    use arm_harness::elf::Elf;
    use arm_harness::run::{RunError, StepVcpu, Vcpu};
    use arm_harness::sys::{Machine, ParamsPage, pin_to_core};

    let (bytes, sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "AA-4 planted ELF",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", opts.image.display()))?;
    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;

    let params = ParamsPage {
        scale_index: 0,
        seed: 0xAA04_AA04_AA04_AA04,
    };
    let mut machine = Machine::new_guarded(&image, &params)
        .map_err(|e| format!("construct guarded KVM machine: {e}"))?;
    machine.set_watchdog_secs(opts.watchdog_secs);
    let pc_before = StepVcpu::pc(&mut machine).map_err(|e| format!("read initial PC: {e}"))?;

    let (gpa, generation, hazards, exclusive_hazards, live_counter_hazards, summary) =
        match Vcpu::run(&mut machine) {
            Err(RunError::ExecGuardRejected {
                gpa,
                generation,
                hazards,
                exclusive_hazards,
                live_counter_hazards,
                summary,
            }) => (
                gpa,
                generation,
                hazards,
                exclusive_hazards,
                live_counter_hazards,
                summary,
            ),
            Err(other) => {
                return Err(format!(
                    "guard run failed before planted rejection: {other}"
                ));
            }
            Ok(exit) => {
                return Err(format!(
                    "guarded planted ELF produced caller-visible {exit:?} instead of rejection"
                ));
            }
        };
    let pc_after = StepVcpu::pc(&mut machine).map_err(|e| format!("read rejected PC: {e}"))?;
    let stats = machine
        .exec_guard_stats()
        .ok_or_else(|| "guarded constructor returned no execute-guard statistics".to_string())?;

    if exclusive_hazards == 0
        || hazards < exclusive_hazards
        || generation == 0
        // F3-REJECT-PC: the guest must not have advanced AT ALL — a page rejected before its
        // first execute leaves PC exactly at entry, not merely somewhere in the rejected page.
        || pc_after != pc_before
        || gpa != pc_after & !0xfff
        || stats.rejections != 1
        || stats.scans != stats.approvals.saturating_add(stats.rejections)
        || stats.exits != stats.scans
        || stats.write_revocations != 0
        || stats.blocked_writes != 0
    {
        return Err(format!(
            "planted rejection was internally inconsistent: gpa={gpa:#x} generation={generation} \
             hazards={hazards} exclusives={exclusive_hazards} live_counters={live_counter_hazards} \
             pc_before={pc_before:#x} pc_after={pc_after:#x} stats={stats:?}"
        ));
    }

    println!(
        "AA4_GUARD_REJECT PASS image_sha256={sha256} gpa={gpa:#x} generation={generation} \
         hazards={hazards} exclusives={exclusive_hazards} live_counters={live_counter_hazards} \
         pc_before={pc_before:#x} pc_after={pc_after:#x} exits={} scans={} approvals={} \
         rejections={} summary={summary}",
        stats.exits, stats.scans, stats.approvals, stats.rejections
    );
    println!(
        "AA-4 remains open: this proves pre-execute planted rejection; write-before-modification, \
         stale-generation, notifier replacement, and two-vCPU scan/write races remain live gates"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn aa4_guard_reject(_opts: Aa4GuardRejectOpts) -> Result<(), String> {
    Err(
        "`arm-spike aa4-guard-reject` issues KVM ioctls and needs /dev/kvm: it is Linux-only"
            .into(),
    )
}

/// Run the dedicated self-modifying ELF through one audited target page.
///
/// Success binds three synchronous observations to the hash-pinned artifact: the initial
/// execute scan sees the original `mov x0, #1` page; the write exit still sees that exact
/// page before the store retries; and a fresh scan generation sees the exact page with only
/// that instruction changed to `mov x0, #2` before it executes.
#[cfg(target_os = "linux")]
fn aa4_guard_write(opts: Aa4GuardWriteOpts) -> Result<(), String> {
    use arm_harness::console::{Console, Event};
    use arm_harness::elf::Elf;
    use arm_harness::evidence::hex_lower;
    use arm_harness::run::{PL011_DR, PL011_FR, PL011_FR_READY, Vcpu, VcpuExit};
    use arm_harness::sys::{Machine, ParamsPage, pin_to_core};
    use sha2::{Digest, Sha256};

    const PAGE_SIZE: u64 = 4096;
    const ORIGINAL_MOV_X0_1: u32 = 0xD280_0020;
    const MODIFIED_MOV_X0_2: u32 = 0xD280_0040;
    const RET: u32 = 0xD65F_03C0;

    if opts.max_exits == 0 {
        return Err("--max-exits must be nonzero".into());
    }
    let (bytes, image_sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "AA-4 self-modifying ELF",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", opts.image.display()))?;
    let target = image
        .symbol("aa4_self_modify_target")
        .map_err(|e| format!("resolve aa4_self_modify_target: {e}"))?;
    if target & (PAGE_SIZE - 1) != 0 {
        return Err(format!(
            "aa4_self_modify_target {target:#x} is not page-aligned"
        ));
    }
    let target_end = target
        .checked_add(PAGE_SIZE)
        .ok_or_else(|| "aa4_self_modify_target page end overflowed".to_string())?;
    let original_page = image
        .bytes(target, target_end)
        .map_err(|e| format!("read planted target page: {e}"))?
        .to_vec();
    let original_prefix = [ORIGINAL_MOV_X0_1.to_le_bytes(), RET.to_le_bytes()].concat();
    if original_page.get(..original_prefix.len()) != Some(original_prefix.as_slice()) {
        return Err(format!(
            "planted target at {target:#x} is not `mov x0, #1; ret`"
        ));
    }
    let expected_initial_sha256: [u8; 32] = Sha256::digest(&original_page).into();
    let mut modified_page = original_page;
    modified_page[..4].copy_from_slice(&MODIFIED_MOV_X0_2.to_le_bytes());
    let expected_modified_sha256: [u8; 32] = Sha256::digest(&modified_page).into();

    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;
    let params = ParamsPage {
        scale_index: 0,
        seed: 0xAA04_AA04_5752_4954,
    };
    let mut machine = Machine::new_guarded(&image, &params)
        .map_err(|e| format!("construct guarded KVM machine: {e}"))?;
    machine
        .audit_exec_guard_page(target)
        .map_err(|e| format!("configure target-page audit: {e}"))?;
    machine
        .probe_stale_exec_guard_generation(target)
        .map_err(|e| format!("configure stale-generation probe: {e}"))?;
    machine.set_watchdog_secs(opts.watchdog_secs);

    let mut console = Console::new();
    let mut saw_ok = false;
    let mut saw_pass = false;
    let mut completed = None;
    for exits in 1..=opts.max_exits {
        let exit =
            Vcpu::run(&mut machine).map_err(|e| format!("run self-modifying payload: {e}"))?;
        // F3-GUARD-BUDGET: the guard's scan/approve/reject exits are serviced inside
        // `Vcpu::run` and never reach this caller-visible loop, so counting only
        // caller-visible exits let an adversarial guest mint unbounded guard work under a
        // small `--max-exits`. Charge cumulative guard exits to the same budget.
        let guard_exits = machine.exec_guard_stats().map_or(0, |s| s.exits);
        if exits.saturating_add(guard_exits) > opts.max_exits {
            return Err(format!(
                "guard exits ({guard_exits}) plus caller-visible exits ({exits}) exceeded \
                 --max-exits {} — refusing unbounded guard work",
                opts.max_exits
            ));
        }
        match exit {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                if !(oracle_model::UART_BASE..oracle_model::UART_BASE + PAGE_SIZE).contains(&addr) {
                    return Err(format!(
                        "self-modifying payload touched unexpected MMIO {addr:#x}"
                    ));
                }
                if !is_write {
                    if addr != PL011_FR || data.is_empty() || data.len() > 8 {
                        return Err(format!(
                            "unexpected PL011 read at {addr:#x} with width {}",
                            data.len()
                        ));
                    }
                    let ready = PL011_FR_READY.to_le_bytes();
                    machine
                        .complete_mmio_read(&ready[..data.len().min(ready.len())])
                        .map_err(|e| format!("complete PL011 read: {e}"))?;
                    continue;
                }
                if addr != PL011_DR {
                    continue;
                }
                let byte = *data
                    .first()
                    .ok_or_else(|| "zero-width PL011 data write".to_string())?;
                match console.push(byte) {
                    Some(Event::Line(line)) if line == "OK write-rescan-complete" => saw_ok = true,
                    Some(Event::Line(line)) if line == "PAYLOAD aa4-self-modify PASS" => {
                        saw_pass = true;
                    }
                    Some(Event::Line(_)) | None => {}
                    Some(Event::Exit(status)) => {
                        completed = Some((status, exits));
                        break;
                    }
                    Some(Event::MarkBegin | Event::MarkEnd) => {
                        return Err("self-modifying proof emitted an oracle window marker".into());
                    }
                }
            }
            VcpuExit::MalformedMmio { addr, width } => {
                return Err(format!("malformed MMIO at {addr:#x} with width {width}"));
            }
            other => {
                return Err(format!(
                    "self-modifying proof produced unexpected caller-visible exit {other:?}"
                ));
            }
        }
    }

    let (status, caller_exits) = completed.ok_or_else(|| {
        format!(
            "self-modifying proof exceeded --max-exits {} before PAYLOAD EXIT",
            opts.max_exits
        )
    })?;
    if status != 0 || !saw_ok || !saw_pass {
        return Err(format!(
            "self-modifying payload did not attest success: status={status} saw_ok={saw_ok} \
             saw_pass={saw_pass}"
        ));
    }

    let audit = machine
        .exec_guard_page_audit()
        .ok_or_else(|| "guarded machine returned no target-page audit".to_string())?;
    let stats = machine
        .exec_guard_stats()
        .ok_or_else(|| "guarded machine returned no execute-guard statistics".to_string())?;
    if !arm_harness::sys::exec_guard_write_proof_holds(
        target,
        expected_initial_sha256,
        expected_modified_sha256,
        audit,
        stats,
    ) {
        return Err(format!(
            "write/rescan audit was internally inconsistent: target={target:#x} audit={audit:?} \
             stats={stats:?} expected_initial={} expected_modified={}",
            hex_lower(&expected_initial_sha256),
            hex_lower(&expected_modified_sha256)
        ));
    }
    if !arm_harness::sys::exec_guard_stale_generation_proof_holds(audit) {
        return Err(format!(
            "stale-generation audit was internally inconsistent: target={target:#x} \
             audit={audit:?}"
        ));
    }

    println!(
        "AA4_GUARD_WRITE PASS image_sha256={image_sha256} target={target:#x} \
         first_generation={} write_generation={} rescan_generation={} caller_exits={caller_exits} \
         guard_exits={} guard_scans={} guard_approvals={} guard_write_revocations={} \
         stale_generation={} stale_errno={} initial_sha256={} modified_sha256={}",
        audit.first_exec_generation,
        audit.write_generation,
        audit.post_write_exec_generation,
        stats.exits,
        stats.scans,
        stats.approvals,
        stats.write_revocations,
        audit.stale_reply_generation,
        audit.stale_reply_errno,
        hex_lower(&expected_initial_sha256),
        hex_lower(&expected_modified_sha256)
    );
    println!(
        "AA-4 remains open: this proves write-before-modification, exact-page rescan, and \
         stale-generation rejection; planted rejection, notifier replacement, and two-vCPU \
         scan/write races remain live gates"
    );
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn aa4_guard_write(_opts: Aa4GuardWriteOpts) -> Result<(), String> {
    Err("`arm-spike aa4-guard-write` issues KVM ioctls and needs /dev/kvm: it is Linux-only".into())
}

/// The memslot operation the `aa4-reexec` harness interposes at the first-execute marker.
#[cfg(target_os = "linux")]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ReexecSlotOp {
    /// No memslot change — the second execute must reuse the approval (negative control).
    None,
    /// Re-add the SAME backing (notifier-replacement).
    Notifier,
    /// Move to a DISTINCT byte-identical backing (backing-replacement).
    Backing,
}

/// Run the `aa4-reexec` payload to completion under the guard, applying `slot_op` at the
/// first-execute marker. Returns `(target_scans, first_scan_gen, last_scan_gen)`.
#[cfg(target_os = "linux")]
fn aa4_run_reexec(
    image: &arm_harness::elf::Elf,
    target: u64,
    watchdog_secs: u64,
    max_exits: u64,
    slot_op: ReexecSlotOp,
) -> Result<(u64, u64, u64), String> {
    use arm_harness::console::{Console, Event};
    use arm_harness::run::{PL011_DR, PL011_FR, PL011_FR_READY, Vcpu, VcpuExit};
    use arm_harness::sys::{Machine, ParamsPage};

    let params = ParamsPage {
        scale_index: 0,
        seed: 0xAA04_4E4F_5449_4659,
    };
    let mut machine = Machine::new_guarded(image, &params)
        .map_err(|e| format!("construct guarded machine: {e}"))?;
    machine
        .audit_exec_guard_page(target)
        .map_err(|e| format!("configure target-page audit: {e}"))?;
    machine.set_watchdog_secs(watchdog_secs);

    let mut console = Console::new();
    let mut replaced = false;
    let mut saw_first = false;
    let mut saw_second = false;
    let mut passed = false;
    for exits in 1..=max_exits {
        let exit = Vcpu::run(&mut machine).map_err(|e| format!("run aa4-reexec: {e}"))?;
        let guard_exits = machine.exec_guard_stats().map_or(0, |s| s.exits);
        if exits.saturating_add(guard_exits) > max_exits {
            return Err(format!(
                "guard exits ({guard_exits}) + caller exits exceeded max"
            ));
        }
        match exit {
            VcpuExit::Mmio {
                addr,
                data,
                is_write,
            } => {
                if !is_write {
                    if addr == PL011_FR {
                        let ready = PL011_FR_READY.to_le_bytes();
                        machine
                            .complete_mmio_read(&ready[..data.len().min(ready.len())])
                            .map_err(|e| format!("complete PL011 read: {e}"))?;
                    }
                    continue;
                }
                if addr != PL011_DR {
                    continue;
                }
                let byte = *data
                    .first()
                    .ok_or_else(|| "zero-width PL011 write".to_string())?;
                match console.push(byte) {
                    Some(Event::Line(line)) if line == "OK reexec-first" => {
                        saw_first = true;
                        // The target's first execute has scanned+approved it. Interpose the
                        // memslot update here, before the second execute.
                        if !replaced {
                            match slot_op {
                                ReexecSlotOp::Notifier => machine
                                    .notifier_replace_slot0()
                                    .map_err(|e| format!("notifier-replace slot 0: {e}"))?,
                                ReexecSlotOp::Backing => machine
                                    .backing_replace_slot0()
                                    .map_err(|e| format!("backing-replace slot 0: {e}"))?,
                                ReexecSlotOp::None => {}
                            }
                            replaced = true;
                        }
                    }
                    Some(Event::Line(line)) if line == "OK reexec-second" => saw_second = true,
                    Some(Event::Line(_)) | None => {}
                    Some(Event::Exit(status)) => {
                        if status != 0 {
                            return Err(format!("aa4-reexec exited nonzero: {status}"));
                        }
                        passed = true;
                        break;
                    }
                    Some(Event::MarkBegin | Event::MarkEnd) => {
                        return Err("aa4-reexec emitted an oracle window marker".into());
                    }
                }
            }
            VcpuExit::MalformedMmio { addr, width } => {
                return Err(format!("malformed MMIO at {addr:#x} width {width}"));
            }
            other => return Err(format!("aa4-reexec unexpected exit {other:?}")),
        }
    }
    if !(saw_first && saw_second && passed) {
        return Err(format!(
            "aa4-reexec did not complete: first={saw_first} second={saw_second} pass={passed}"
        ));
    }
    // Target-page-specific scan count (NOT total scans: a memslot replace re-scans every
    // executable page, so only the audited target isolates the notifier's effect on it).
    let audit = machine
        .exec_guard_page_audit()
        .ok_or_else(|| "no execute-guard page audit".to_string())?;
    Ok((
        audit.exec_scans,
        audit.first_exec_generation,
        machine.exec_guard_last_scan_generation(),
    ))
}

/// AA-4 concurrency: a memslot update's mmu-notifier invalidation must force the guard to
/// re-scan an already-approved page. Self-verifying via a negative control: the same payload
/// run WITHOUT the memslot update reuses the approval (one scan), so a second scan in the
/// notifier run is attributable only to the invalidation.
#[cfg(target_os = "linux")]
fn aa4_guard_notifier(opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    use arm_harness::elf::Elf;
    use arm_harness::sys::pin_to_core;

    let (bytes, image_sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "AA-4 reexec ELF",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", opts.image.display()))?;
    let target = image
        .symbol("aa4_reexec_target")
        .map_err(|e| format!("resolve aa4_reexec_target: {e}"))?;
    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;

    // Negative control first: no memslot update — the second execute must reuse the approval.
    let (ctrl_scans, ctrl_first_gen, ctrl_last_gen) = aa4_run_reexec(
        &image,
        target,
        opts.watchdog_secs,
        opts.max_exits,
        ReexecSlotOp::None,
    )?;
    // Notifier run: replace the memslot at the marker — the second execute must re-scan.
    let (notif_scans, notif_first_gen, notif_last_gen) = aa4_run_reexec(
        &image,
        target,
        opts.watchdog_secs,
        opts.max_exits,
        ReexecSlotOp::Notifier,
    )?;

    let control_reused = ctrl_scans == 1;
    let notifier_rescanned = notif_scans == 2 && notif_last_gen > notif_first_gen;
    println!(
        "AA4_GUARD_NOTIFIER image_sha256={image_sha256} \
         control_scans={ctrl_scans} control_first_gen={ctrl_first_gen} control_last_gen={ctrl_last_gen} \
         notifier_scans={notif_scans} notifier_first_gen={notif_first_gen} notifier_last_gen={notif_last_gen} \
         control_reused_approval={control_reused} notifier_forced_rescan={notifier_rescanned}"
    );
    if control_reused && notifier_rescanned {
        println!(
            "AA4_GUARD_NOTIFIER PASS: a memslot update forced a fresh scan (gen {notif_first_gen} -> \
             {notif_last_gen}) where the unchanged page otherwise reused its approval"
        );
        Ok(())
    } else {
        Err(format!(
            "notifier-replacement NOT proven: control_reused={control_reused} (want 1 scan) \
             notifier_rescanned={notifier_rescanned} (want 2 scans, newer gen)"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn aa4_guard_notifier(_opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    Err(
        "`arm-spike aa4-guard-notifier` issues KVM ioctls and needs /dev/kvm: it is Linux-only"
            .into(),
    )
}

/// AA-4 concurrency: moving RAM slot 0 to a DISTINCT byte-identical backing must force the
/// guard to re-scan an already-approved page — the approval is keyed to the mapping, not to a
/// content hash. Self-verifying via the same negative control as the notifier gate.
#[cfg(target_os = "linux")]
fn aa4_guard_backing(opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    use arm_harness::elf::Elf;
    use arm_harness::sys::pin_to_core;

    let (bytes, image_sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "AA-4 reexec ELF",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", opts.image.display()))?;
    let target = image
        .symbol("aa4_reexec_target")
        .map_err(|e| format!("resolve aa4_reexec_target: {e}"))?;
    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;

    let (ctrl_scans, ctrl_first_gen, ctrl_last_gen) = aa4_run_reexec(
        &image,
        target,
        opts.watchdog_secs,
        opts.max_exits,
        ReexecSlotOp::None,
    )?;
    let (move_scans, move_first_gen, move_last_gen) = aa4_run_reexec(
        &image,
        target,
        opts.watchdog_secs,
        opts.max_exits,
        ReexecSlotOp::Backing,
    )?;

    let control_reused = ctrl_scans == 1;
    let backing_rescanned = move_scans == 2 && move_last_gen > move_first_gen;
    println!(
        "AA4_GUARD_BACKING image_sha256={image_sha256} \
         control_scans={ctrl_scans} control_first_gen={ctrl_first_gen} control_last_gen={ctrl_last_gen} \
         backing_scans={move_scans} backing_first_gen={move_first_gen} backing_last_gen={move_last_gen} \
         control_reused_approval={control_reused} backing_forced_rescan={backing_rescanned}"
    );
    if control_reused && backing_rescanned {
        println!(
            "AA4_GUARD_BACKING PASS: moving slot 0 to a distinct byte-identical backing forced a \
             fresh scan (gen {move_first_gen} -> {move_last_gen}); content unchanged, approval not \
             reused across the move"
        );
        Ok(())
    } else {
        Err(format!(
            "backing-replacement NOT proven: control_reused={control_reused} (want 1 scan) \
             backing_rescanned={backing_rescanned} (want 2 scans, newer gen)"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn aa4_guard_backing(_opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    Err(
        "`arm-spike aa4-guard-backing` issues KVM ioctls and needs /dev/kvm: it is Linux-only"
            .into(),
    )
}

/// AA-4 concurrency: a second vCPU's store to a page a first vCPU has frozen for a scan must be
/// BLOCKED behind that pending scan. Self-verifying negative control: once the first vCPU's
/// scan is APPROVED (page no longer frozen), the same store instead revokes execute (WRITE),
/// not blocked.
#[cfg(target_os = "linux")]
fn aa4_guard_race(opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    use arm_harness::elf::Elf;
    use arm_harness::sys::{Machine, ParamsPage, RaceExit, pin_to_core};

    let (bytes, image_sha256) = verified_artifact(
        &opts.image,
        &opts.image_sha256,
        "AA-4 reexec ELF",
        arm_harness::linux_boot::MAX_IMAGE_BYTES,
    )?;
    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", opts.image.display()))?;
    let target = image
        .symbol("aa4_reexec_target")
        .map_err(|e| format!("resolve aa4_reexec_target: {e}"))?;
    let writer = image
        .symbol("aa4_reexec_writer")
        .map_err(|e| format!("resolve aa4_reexec_writer: {e}"))?;
    let writer_page = writer & !0xfff;
    pin_to_core(opts.core).map_err(|e| format!("pin to core {}: {e}", opts.core))?;

    // One scenario: freeze the target on vCPU 0, optionally approve it, then have the writer
    // vCPU store to it. Returns the writer store's exit.
    let run_scenario = |approve_target_first: bool| -> Result<RaceExit, String> {
        let params = ParamsPage {
            scale_index: 0,
            seed: 0xAA04_5241_4345_0000,
        };
        let mut m = Machine::new_race_guarded(&image, &params)
            .map_err(|e| format!("construct race (no-vGIC) guarded machine: {e}"))?;
        m.set_watchdog_secs(opts.watchdog_secs);
        m.race_arm_vcpu0(target)
            .map_err(|e| format!("arm vCPU 0 at the target: {e}"))?;
        m.race_create_writer(writer, target, 0xd280_0040)
            .map_err(|e| {
                format!("create the writer vCPU (KVM may refuse a 2nd vCPU on the guarded VM): {e}")
            })?;

        // vCPU 0 fault-freezes the target page for a scan.
        let target_gen = match m.race_run_vcpu0().map_err(|e| format!("run vCPU 0: {e}"))? {
            RaceExit::GuardExec { gpa, generation } if gpa == target => generation,
            other => return Err(format!("vCPU 0 did not fault-freeze the target: {other:?}")),
        };
        if approve_target_first {
            m.race_scan_and_approve(target, target_gen)
                .map_err(|e| format!("approve the target scan: {e}"))?;
        }
        // The writer vCPU first faults on its own code page; approve it.
        match m
            .race_run_writer()
            .map_err(|e| format!("run writer (code fault): {e}"))?
        {
            RaceExit::GuardExec { gpa, generation } if gpa == writer_page => m
                .race_scan_and_approve(writer_page, generation)
                .map_err(|e| format!("approve the writer code page: {e}"))?,
            other => return Err(format!("writer did not fault its own code page: {other:?}")),
        }
        // The writer's store to the target: BLOCKED while frozen, WRITE-revoke once approved.
        m.race_run_writer()
            .map_err(|e| format!("run writer (store): {e}"))
    };

    let race = run_scenario(false)?;
    let control = run_scenario(true)?;

    let blocked_when_frozen = matches!(race, RaceExit::GuardBlocked { gpa, .. } if gpa == target);
    let write_when_approved = matches!(control, RaceExit::GuardWrite { gpa, .. } if gpa == target);
    println!(
        "AA4_GUARD_RACE image_sha256={image_sha256} target={target:#x} writer={writer:#x} \
         race={race:?} control={control:?} blocked_when_frozen={blocked_when_frozen} \
         write_when_approved={write_when_approved}"
    );
    if blocked_when_frozen && write_when_approved {
        println!(
            "AA4_GUARD_RACE PASS: a writer vCPU's store to a page frozen for another vCPU's scan \
             was BLOCKED; once that scan was approved the same store revoked execute (WRITE), not \
             blocked"
        );
        Ok(())
    } else {
        Err(format!(
            "two-vCPU race NOT proven: blocked_when_frozen={blocked_when_frozen} (want a BLOCKED \
             store while frozen) write_when_approved={write_when_approved} (want a WRITE store once \
             approved)"
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn aa4_guard_race(_opts: Aa4GuardNotifierOpts) -> Result<(), String> {
    Err("`arm-spike aa4-guard-race` issues KVM ioctls and needs /dev/kvm: it is Linux-only".into())
}

/// AA-6(a): install a below-host synthetic ID-register model on a disposable vCPU and confirm
/// every frozen value survives read-back (the guest-visible value). Writes an enforcement
/// truth-table and fails closed unless every reducible register froze and the PMU is denied.
#[cfg(target_os = "linux")]
fn id_freeze(out: PathBuf) -> Result<(), String> {
    use std::fmt::Write as _;

    let proof = sys::id_freeze_proof().map_err(|e| format!("run the ID-freeze proof: {e}"))?;
    if proof.rows.is_empty() {
        return Err("no ID_AA64* register was probed".to_string());
    }
    // F9 tri-state: the freeze is fully enforced iff NO register is `reducible-but-clamped`
    // (an un-freezable, guest-visible feature). `no-reducible-field` registers do not gate
    // (nothing to freeze); `frozen-below-host` are the installed freezes.
    let clamped: Vec<&str> = proof
        .rows
        .iter()
        .filter(|r| r.status == sys::IdFreezeStatus::ReducibleButClamped)
        .map(|r| r.name)
        .collect();
    // A defensive re-check of the enforced rows: below host, read-back holds.
    let frozen_ok = proof.rows.iter().all(|r| {
        if r.status != sys::IdFreezeStatus::FrozenBelowHost {
            return true;
        }
        let f = (r.frozen_value >> r.field_shift) & 0xF;
        let h = (r.host_value >> r.field_shift) & 0xF;
        r.enforced && r.read_back == r.frozen_value && f < h
    });
    let frozen_count = proof
        .rows
        .iter()
        .filter(|r| r.status == sys::IdFreezeStatus::FrozenBelowHost)
        .count();
    let all_enforced = clamped.is_empty() && frozen_ok && frozen_count > 0;

    // Machine-readable enforcement truth-table (stable JSON, sorted-order fields). Every row
    // carries its F9 tri-state `status`, so an un-freezable register is recorded, not hidden.
    let mut json = String::new();
    json.push_str("{\n  \"check\": \"aa6-id-register-freeze\",\n");
    let _ = writeln!(
        json,
        "  \"pmu_denied_without_feature\": {},",
        proof.pmu_denied_without_feature
    );
    let _ = writeln!(json, "  \"host_pmuver\": {},", proof.host_pmuver);
    let _ = writeln!(json, "  \"frozen_below_host\": {frozen_count},");
    let _ = writeln!(json, "  \"reducible_but_clamped\": {},", clamped.len());
    let _ = writeln!(json, "  \"all_enforced\": {all_enforced},");
    json.push_str("  \"rows\": [\n");
    for (i, r) in proof.rows.iter().enumerate() {
        let comma = if i + 1 < proof.rows.len() { "," } else { "" };
        let _ = writeln!(
            json,
            "    {{\"name\": \"{}\", \"field_shift\": {}, \"host_value\": \"{:#018x}\", \
             \"frozen_value\": \"{:#018x}\", \"read_back\": \"{:#018x}\", \"status\": \"{}\", \
             \"enforced\": {}}}{}",
            r.name,
            r.field_shift,
            r.host_value,
            r.frozen_value,
            r.read_back,
            r.status.as_str(),
            r.enforced,
            comma
        );
    }
    json.push_str("  ]\n}\n");
    std::fs::write(&out, &json).map_err(|e| format!("write {}: {e}", out.display()))?;

    for r in &proof.rows {
        println!(
            "ID_FREEZE {} field_shift={} host={:#018x} frozen={:#018x} read_back={:#018x} \
             status={} enforced={}",
            r.name,
            r.field_shift,
            r.host_value,
            r.frozen_value,
            r.read_back,
            r.status.as_str(),
            r.enforced
        );
    }
    println!(
        "ID_FREEZE pmu_denied_without_feature={} host_pmuver={} rows={} frozen_below_host={} \
         reducible_but_clamped={} all_enforced={} out={}",
        proof.pmu_denied_without_feature,
        proof.host_pmuver,
        proof.rows.len(),
        frozen_count,
        clamped.len(),
        all_enforced,
        out.display()
    );
    if all_enforced && proof.pmu_denied_without_feature {
        Ok(())
    } else {
        Err(format!(
            "ID-register freeze NOT fully enforced: all_enforced={all_enforced} \
             reducible_but_clamped={:?} pmu_denied={} — a guest-visible feature could not be \
             frozen (record its enforcement disposition, e.g. HCR_EL2.TID3 trap-emulation) or \
             the PMU is not denied",
            clamped, proof.pmu_denied_without_feature
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn id_freeze(_out: PathBuf) -> Result<(), String> {
    Err("`arm-spike id-freeze` issues KVM ioctls and needs /dev/kvm: it is Linux-only".into())
}

/// AA-6(b): prove the in-kernel vGIC's injection state round-trips through save/restore
/// bit-identically, with a fresh-vGIC negative control.
#[cfg(target_os = "linux")]
fn vgic_roundtrip(out: PathBuf) -> Result<(), String> {
    use std::fmt::Write as _;

    let rt = sys::vgic_roundtrip_proof().map_err(|e| format!("run the vGIC round-trip: {e}"))?;

    let mut json = String::new();
    json.push_str("{\n  \"check\": \"aa6-vgic-save-restore-roundtrip\",\n");
    let _ = writeln!(json, "  \"injected_intid\": {},", rt.injected_intid);
    let _ = writeln!(json, "  \"injected_spi\": {},", rt.injected_spi);
    let _ = writeln!(
        json,
        "  \"groups_covered\": [{}],",
        rt.groups_covered
            .iter()
            .map(|g| format!("\"{g}\""))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        json,
        "  \"negative_control_differs\": {},",
        rt.negative_control_differs
    );
    let _ = writeln!(
        json,
        "  \"roundtrip_identical\": {},",
        rt.roundtrip_identical
    );
    json.push_str("  \"registers\": [\n");
    for (i, label) in rt.labels.iter().enumerate() {
        let comma = if i + 1 < rt.labels.len() { "," } else { "" };
        let _ = writeln!(
            json,
            "    {{\"reg\": \"{}\", \"group\": \"{}\", \"saved\": \"{:#018x}\", \
             \"fresh_before\": \"{:#018x}\", \"fresh_after\": \"{:#018x}\"}}{}",
            label, rt.groups[i], rt.saved[i], rt.fresh_before[i], rt.fresh_after[i], comma
        );
    }
    json.push_str("  ]\n}\n");
    std::fs::write(&out, &json).map_err(|e| format!("write {}: {e}", out.display()))?;

    println!(
        "VGIC_ROUNDTRIP injected_intid={} injected_spi={} groups_covered={:?} \
         negative_control_differs={} roundtrip_identical={} registers={} out={}",
        rt.injected_intid,
        rt.injected_spi,
        rt.groups_covered,
        rt.negative_control_differs,
        rt.roundtrip_identical,
        rt.labels.len(),
        out.display()
    );
    if rt.roundtrip_identical && rt.negative_control_differs {
        Ok(())
    } else {
        Err(format!(
            "vGIC round-trip NOT proven: roundtrip_identical={} negative_control_differs={} — a \
             non-identical restore or a vacuous match (fresh already equalled the save)",
            rt.roundtrip_identical, rt.negative_control_differs
        ))
    }
}

#[cfg(not(target_os = "linux"))]
fn vgic_roundtrip(_out: PathBuf) -> Result<(), String> {
    Err("`arm-spike vgic-roundtrip` issues KVM ioctls and needs /dev/kvm: it is Linux-only".into())
}

/// Read, hash, and parse the ELF for `payload` out of `dir`, and **verify it against
/// its trusted expected pin** before returning it.
///
/// Loading one ELF for every class would run one payload under seven wrong labels;
/// hashing whatever bytes are present without comparing to a trusted pin would give a
/// swapped or rebuilt artifact a fresh accepted identity (§Evidence integrity #3). So
/// each class is loaded from its own file AND its hash is compared to the pin the
/// operator supplied: a mismatch, or a missing pin, is a hard error — nothing is
/// attested that was not verified against an identity the operator vouched for.
#[cfg(target_os = "linux")]
fn load_payload(
    dir: &std::path::Path,
    payload: oracle_model::Payload,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<LoadedPayload, String> {
    use arm_harness::elf::Elf;
    use arm_harness::evidence::hex_lower;
    use sha2::{Digest, Sha256};

    let name = payload.name();
    let path = dir.join(name);
    let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let sha256 = hex_lower(&h.finalize());

    let expected = pins.get(name).ok_or_else(|| {
        format!(
            "no trusted sha256 pin for payload `{name}` in --payload-pins: cannot attest an \
             artifact whose identity was not vouched for"
        )
    })?;
    let expected = normalise_sha256(expected);
    if expected != sha256 {
        return Err(format!(
            "payload `{name}` at {} hashes to {sha256}, but its trusted pin is {expected}: \
             refusing to boot an artifact that is not the one pinned",
            path.display()
        ));
    }

    let image = Elf::parse(bytes).map_err(|e| format!("parse {}: {e}", path.display()))?;
    Ok(LoadedPayload { image, sha256 })
}

/// The uniform overflow period across the armed records, or `None` if they vary (or
/// nothing armed). Each armed record's period is `target - work_begin`.
#[cfg(target_os = "linux")]
fn uniform_period(records: &[arm_harness::evidence::RunRecord]) -> Option<u64> {
    let mut period: Option<u64> = None;
    for r in records {
        let Some(o) = r.overflow.as_ref().filter(|o| o.armed) else {
            continue;
        };
        let p = o.target.checked_sub(r.work_begin)?;
        match period {
            None => period = Some(p),
            Some(prev) if prev == p => {}
            Some(_) => return None, // periods vary → not uniform
        }
    }
    period
}

/// Stamp the AA-6 injection attestation from the injection config the run loop **actually
/// executed** (bead hm-oh3v), never echoed from CLI intent.
///
/// - Injection configured → `on(intid, None)`: the bare-payload `run` lane injects at every
///   exact landing, so there is no single `inject_at_work` index (the LinuxGuest lane stamps
///   its single-Moment attestation in [`linux_boot`]).
/// - AA-6 with injection OFF → `off()`: the honest actual config, so the matrix checker fails
///   it with "stamp says OFF" rather than a missing stamp — closing the config-slip hole where
///   an accidentally-bare AA-6 matrix read PASS.
/// - Any other stage that did not inject → no stamp: injection is not a concept there.
///
/// Portable and pure so it is unit-tested off-box; the `run` loop that calls it is Linux-only
/// (so off Linux it is unread by construction — there is no `/dev/kvm` to run against).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn injection_attestation(
    stage: arm_harness::evidence::Stage,
    inject: Option<&arm_harness::run::InjectionConfig>,
) -> Option<arm_harness::evidence::InjectionAttestation> {
    use arm_harness::evidence::{InjectionAttestation, Stage};
    match inject {
        Some(cfg) => Some(InjectionAttestation::on(cfg.intid, None)),
        None if stage == Stage::Aa6 => Some(InjectionAttestation::off()),
        None => None,
    }
}

#[cfg(target_os = "linux")]
fn execute(args: RunArgs) -> Result<(), String> {
    use arm_harness::evidence::{
        ExitReason, ImagePin, Mechanism, Pinning, RunSetContext, assemble_run_set, hex_lower,
    };
    use arm_harness::run::{
        ArmedMigrationProbe, SampleSpec, run_sample, run_sample_exact, step_run,
    };
    use arm_harness::sys::{self, Machine, ParamsPage, PerfCounter, perf_config, pin_to_core};
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    // Pin the thread that will call KVM_RUN: the perf context follows the thread, and on
    // this lineage an unpinned sample is not a slower sample, it is an untrusted one (rr
    // #3607). The ONE sanctioned exception is AA-1's bounded migration probe — and it must
    // migrate the thread WHILE its overflow is armed, which is the rr #3607 missed-PMI mode.
    // Re-pinning between samples (r13) never does this: the counter is opened after the move
    // and dropped before the next, so no armed context migrates. So the probe instead runs a
    // background churner that rotates the *live* vCPU thread across the allowed cpuset while
    // the sample loop below arms and reads its counter — an armed KVM_RUN in progress is
    // forced across cores mid-run. A single-core lease cannot move, so the probe refuses
    // rather than certify a no-op; the evidence records `pinned: false` / no single core.
    let churner = if args.migration_probe {
        let cores = sys::allowed_cores().map_err(|e| format!("read the allowed cpuset: {e}"))?;
        if cores.len() < 2 {
            return Err(format!(
                "the migration probe needs at least two allowed cores to move between, but the \
                 process cpuset is {cores:?}: a single-core lease cannot exercise the rr #3607 \
                 cross-core-migration failure mode"
            ));
        }
        Some(
            sys::MigrationChurner::start(sys::current_tid(), cores)
                .map_err(|e| format!("start the migration churner: {e}"))?,
        )
    } else {
        pin_to_core(args.core).map_err(|e| format!("pin to core {}: {e}", args.core))?;
        None
    };

    // Content-verify the host kernel against its TRUSTED expected pin: hash the image
    // and compare to the sha256 the operator vouched is the running kernel. A mismatch
    // means the file is not the one attested, and is a hard error — hashing whatever
    // bytes are present and asserting `verified_before_boot` would hand a stale or
    // swapped kernel a fresh accepted identity (§Evidence integrity #3, which names
    // host kernels).
    let host_kernel_bytes = std::fs::read(&args.host_kernel_image)
        .map_err(|e| format!("read host kernel {}: {e}", args.host_kernel_image.display()))?;
    let host_kernel_sha256 = {
        let mut h = Sha256::new();
        h.update(&host_kernel_bytes);
        hex_lower(&h.finalize())
    };
    let expected_kernel = normalise_sha256(&args.host_kernel_sha256);
    if expected_kernel != host_kernel_sha256 {
        return Err(format!(
            "host kernel {} hashes to {host_kernel_sha256}, but its trusted pin is \
             {expected_kernel}: refusing to attest a kernel that is not the one pinned",
            args.host_kernel_image.display()
        ));
    }

    // Bind the file pin to the RUNNING kernel. The hash above proves only that a FILE matches
    // a pin; it does not prove that file is the image actually executing — a stale or
    // newly-installed /boot/Image hashes fine while another kernel is booted. So read the live
    // kernel's boot measurement (its GNU build-id from /sys/kernel/notes) and require it to
    // match the operator's expected build-id, and cross-check the live `uname -r` against the
    // environment block's `host_kernel`. Only then may `verified_before_boot` be set.
    let running_build_id = sys::running_kernel_build_id()
        .map_err(|e| format!("read the running kernel build-id: {e}"))?
        .ok_or_else(|| {
            "the running kernel exposes no GNU build-id (/sys/kernel/notes absent or \
             build-id-less): cannot identify the running image, so refusing to attest it — \
             build the kernel with CONFIG_BUILD_SALT/a build-id"
                .to_string()
        })?;
    let expected_build_id = args.host_kernel_build_id.trim().to_ascii_lowercase();
    if running_build_id != expected_build_id {
        return Err(format!(
            "the running kernel's build-id is {running_build_id}, but the expected build-id is \
             {expected_build_id}: the image on disk hashes correctly, but a DIFFERENT kernel is \
             running — refusing to attest the wrong host kernel"
        ));
    }
    // BIND the hashed artifact to the RUNNING kernel. The two checks above are independent:
    // one hashes the file, the other matches the running build-id against an operator-supplied
    // expectation — nothing links the artifact the pin covers to the kernel that actually
    // booted (a stale image hashes fine while another kernel runs; the expected build-id is
    // just another operator value). The file's OWN build-id, extracted from the same bytes we
    // hashed, is that link: require it to equal the running kernel's. A file with no build-id
    // note (a stripped /boot/Image) cannot be bound and so cannot carry verified_before_boot.
    let file_build_id = sys::elf_gnu_build_id(&host_kernel_bytes).ok_or_else(|| {
        format!(
            "the host kernel image {} carries no GNU build-id note, so it cannot be bound to the \
             running kernel — hashing a file that is not provably the booted kernel is not \
             evidence. Supply the build-id-bearing vmlinux ELF as --host-kernel-image, not a \
             stripped Image.",
            args.host_kernel_image.display()
        )
    })?;
    if file_build_id != running_build_id {
        return Err(format!(
            "the hashed host kernel image has build-id {file_build_id}, but the RUNNING kernel's \
             build-id is {running_build_id}: the pinned, hashed artifact is NOT the booted \
             kernel — refusing to attest verified_before_boot for the wrong image"
        ));
    }
    let running_release = sys::running_kernel_release()
        .map_err(|e| format!("read the running kernel release: {e}"))?;
    if running_release != args.environment.host_kernel {
        return Err(format!(
            "the running kernel is {running_release}, but the environment block declares \
             host_kernel = {}: the recorded identity does not match the live kernel",
            args.environment.host_kernel
        ));
    }

    // Cross-check EVERY live-readable identity field the manifest records, not just the
    // kernel release. The manifest copies the operator-supplied environment verbatim, so a
    // reboot into a different KVM mode, or the same-release artifacts run on another machine,
    // would retain stale MIDR/KVM-mode values and the checker would treat different
    // environments as identical. Bind the run to the live host: MIDR (the exact silicon) and
    // the effective KVM mode (kvm_arm.mode, not the VHE feature bit) must match what is
    // recorded, before any measurement is written under that identity.
    let live_midr = sys::read_host_id_registers()
        .map_err(|e| format!("read the running MIDR: {e}"))?
        .midr;
    if live_midr != args.environment.midr {
        return Err(format!(
            "the live MIDR_EL1 is {live_midr:#x}, but the environment block declares \
             {:#x}: the recorded identity does not match the running silicon",
            args.environment.midr
        ));
    }
    let live_kvm_mode = sys::kvm_mode()
        .map_err(|e| format!("read the KVM mode: {e}"))?
        .unwrap_or_else(|| "unknown".to_string());
    if live_kvm_mode != args.environment.kvm_mode {
        return Err(format!(
            "the effective KVM mode is {live_kvm_mode}, but the environment block declares \
             {}: a reboot into a different mode makes the recorded identity stale",
            args.environment.kvm_mode
        ));
    }

    // Load every payload class the plan will run, up front — one ELF per class, each
    // verified against its trusted pin.
    let mut payloads: BTreeMap<String, LoadedPayload> = BTreeMap::new();
    for p in ALL_PAYLOADS.iter().copied().filter(|p| p.has_window()) {
        payloads.insert(
            p.name().to_string(),
            load_payload(&args.payload_dir, p, &args.payload_pins)?,
        );
    }

    let scales = if args.scales.is_empty() {
        vec![Scale::Smoke]
    } else {
        args.scales.clone()
    };
    // AA-3 exact landing (patched mechanism + a measured skid margin) draws targets no closer to
    // MARK_BEGIN than `skid_margin + LANDING_HEADROOM`, so every target can be armed that full
    // combined margin below and single-stepped up to its canonical landing PC; every other
    // configuration draws from 1.
    let target_lo = match args.skid_margin {
        Some(margin) if matches!(args.mechanism, MechanismArg::Patched) => margin
            .saturating_add(arm_harness::run::LANDING_HEADROOM)
            .saturating_add(1),
        _ => 1,
    };
    let samples = plan(&plan_spec(
        args.seed,
        args.cases,
        args.reps,
        args.with_targets,
        scales,
        &args.condition,
        target_lo,
        &args.exclude_payloads,
    ))
    .map_err(|e| e.to_string())?;
    let mut attempted = samples.len() as u64;
    // The plan length, captured BEFORE single-step mode reassigns `attempted` to the
    // step count. The totality checker binds every planned sample to this, so a dropped
    // planned sample cannot hide behind densely-renumbered step records.
    let planned = samples.len() as u64;
    // An empty plan measures nothing. `--reps 0` (or no payloads/scales) would
    // otherwise write an empty, all-passing run-set — a green verdict over zero
    // evidence, which is exactly the vacuity the whole apparatus refuses.
    if attempted == 0 {
        return Err(
            "the plan is empty (0 attempted samples): nothing would be measured, and an empty \
             run-set must never read as a pass"
                .to_string(),
        );
    }

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
    // The armed-migration probe, shared across every sample. `run_sample` sets its `observed`
    // flag only when the churner moves the thread STRICTLY between a sample's `arm_overflow`
    // and its landing — the rr #3607 live-armed migration. A lifetime move total is not
    // enough: the churner starts before kernel hashing/loading/planning/VM build, so its
    // moves can all fall before the first `arm_overflow`. Bounding the observation to the
    // armed interval is what makes it real (a whole-sample count is satisfied by boot moves).
    let migration_probe = churner
        .as_ref()
        .map(|c| ArmedMigrationProbe::new(c.moves_handle()));

    for (i, s) in samples.iter().enumerate() {
        // In migration-probe mode the background churner is moving this thread across cores
        // underneath the loop — no per-sample re-pin (that would fight it and, as r13 did,
        // migrate no armed context).
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
        // Both modes produce a Vec of records: counting mode one per sample, single-step mode
        // one per STEP. Uniform so the collection and failure paths below are shared.
        let result: Result<Vec<arm_harness::evidence::RunRecord>, String> = (|| {
            let mut machine = Machine::new(&loaded.image, &params)
                .map_err(|e| format!("create the machine: {e}"))?;
            machine.set_watchdog_secs(args.watchdog_secs);
            // The patch marker, probed on the VM actually running the sample — the
            // positive proof of §Evidence integrity #4, not a build-time assumption.
            patch_marker = machine
                .patch_marker_observed()
                .map_err(|e| format!("probe the patch marker: {e}"))?;
            if args.single_step {
                // AA-2: the counter is opened in COUNTING mode (no overflow armed — stepping
                // arms guest debug, not a deadline), and the single-step run emits one record
                // per KVM_EXIT_DEBUG. The migration probe is AA-1's alone, so no churner here.
                let mut counter = PerfCounter::open(&machine, mechanism_kind, None)
                    .map_err(|e| format!("open the work counter: {e}"))?;
                armed_attr = Some(*counter.attr());
                let spec = SampleSpec {
                    sample_id: s.sample_id,
                    payload: s.payload,
                    scale: s.scale,
                    seed: s.seed,
                    trips: oracle_model::trips(s.payload, s.scale),
                    condition: s.condition.clone(),
                    target_delta: None,
                    migration_probe: None,
                    // AA-2 single-step never injects (no armed landing to inject at).
                    inject: None,
                };
                step_run(&mut machine, &mut counter, &spec, args.max_steps)
                    .map_err(|e| e.to_string())
            } else {
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
                    migration_probe: migration_probe.clone(),
                    // AA-6 injection is config-gated (`--inject-ppi`). `None` is the negative
                    // control: the default AA-3/AA-5 path is byte-identical (no `KVM_IRQ_LINE`).
                    inject: args.inject,
                };
                // AA-3 exact landing rides the patched mechanism WITH a measured skid margin:
                // `run_sample_exact` re-arms the overflow `skid_margin` events below the target,
                // takes the `Preempt` below target, then single-steps up to `work == target`
                // (the run_until_overflow + single_step contract). A patched run WITHOUT a margin
                // stays the arm-at-target reliability proxy; the stock kick stays `run_sample`.
                match args.skid_margin {
                    Some(margin) if matches!(mechanism_kind, sys::Mechanism::Preempt) => {
                        run_sample_exact(&mut machine, &mut counter, &spec, margin)
                            .map(|r| vec![r])
                            .map_err(|e| e.to_string())
                    }
                    _ => run_sample(&mut machine, &mut counter, &spec)
                        .map(|r| vec![r])
                        .map_err(|e| e.to_string()),
                }
            }
        })();
        match result {
            Ok(mut recs) => records.append(&mut recs),
            Err(e) => {
                failure = Some(format!("sample {i} ({}): {e}", s.payload.name()));
                break;
            }
        }
    }

    // Single-step mode emits one record PER STEP, so the plan length is not the record count.
    // Reassign dense sample ids across every stepped run and record how many steps were
    // actually measured as `attempted` — the record-level totality check binds to the real
    // records. The PLANNED sample count is retained separately (`planned`, above), and every
    // step keeps the plan's stable `planned_sample_id`, so the checker requires distinct
    // coverage of `0..planned` — dense renumbering or a duplicated step-zero row cannot make a
    // dropped planned sample look complete.
    if args.single_step {
        for (i, r) in records.iter_mut().enumerate() {
            r.sample_id = i as u64;
        }
        attempted = records.len() as u64;
        if attempted == 0 && failure.is_none() {
            failure = Some(
                "the single-step run produced no step records: no KVM_EXIT_DEBUG landed, so the \
                 stepping run path stepped nothing — a finding, not an empty pass"
                    .to_string(),
            );
        }
    }

    // Stop the migration churner and confirm it moved the thread DURING an armed interval.
    // A lifetime move total > 0 is not enough — those moves can all fall before the first
    // arm_overflow — so the probe fails unless at least one sample saw the thread move
    // strictly between its arm_overflow and its landing (attested by `migration_probe`).
    let armed_migration_observed = migration_probe
        .as_ref()
        .is_some_and(ArmedMigrationProbe::observed);
    if let Some(churner) = churner {
        let moves = churner.stop();
        if failure.is_none() && !armed_migration_observed {
            failure = Some(format!(
                "the migration probe issued {moves} affinity move(s), but none fell between a \
                 sample's arm_overflow and its landing: no armed perf context migrated across \
                 cores, so the rr #3607 missed-overflow mode was never exercised"
            ));
        }
    }

    // Assemble and WRITE the evidence regardless of whether a sample failed — even if
    // the FIRST sample failed in Machine::new / the patch probe / PerfCounter::open,
    // so `armed_attr` is None. Losing that first-sample failure would let a reliability
    // failure vanish on rerun and hide the gap from the totality checker. The perf
    // block then reflects the INTENDED work-clock config (counting mode), which is
    // truthful: no overflow was armed, and the empty/short records make totality fail.
    {
        let attr = armed_attr.unwrap_or_else(|| sys::br_retired_attr(None));
        let mut images = vec![ImagePin {
            path: args.host_kernel_image.display().to_string(),
            sha256: host_kernel_sha256.clone(),
            // No md5 implementation is on the dependency whitelist; sha256 is the
            // identity. `None` keeps the pin schema-valid (an empty string would
            // violate `^[0-9a-f]{32}$`).
            md5: None,
            verified_before_boot: true,
        }];
        for (name, l) in &payloads {
            images.push(ImagePin {
                path: args.payload_dir.join(name).display().to_string(),
                sha256: l.sha256.clone(),
                md5: None,
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
            // The perf block comes from the armed attr, but `sample_period` is
            // per-sample (each cell draws its own target_delta). So it is derived from
            // the records: `Some(p)` only when EVERY armed record used one uniform
            // period p, else `None` — a varying-period run whose per-sample truth is
            // each record's `target - work_begin`. This is what the checker's uniform
            // cross-check enforces, so the manifest cannot claim one period while the
            // records used another.
            perf: {
                let mut cfg = perf_config(&attr);
                cfg.sample_period = uniform_period(&records);
                cfg
            },
            pinning: Pinning {
                // The migration probe is deliberately unpinned; every other run is pinned.
                pinned: !args.migration_probe,
                core: if args.migration_probe {
                    None
                } else {
                    Some(args.core)
                },
                // The governor of the core the vCPU is PINNED to, not CPU 0's —
                // frequency policy can differ per core, and the retained posture must
                // describe the core that actually ran. For the (unpinned) migration probe
                // it is the nominal starting core's, recorded for reference.
                governor: std::fs::read_to_string(format!(
                    "/sys/devices/system/cpu/cpu{}/cpufreq/scaling_governor",
                    args.core
                ))
                .unwrap_or_default()
                .trim()
                .to_string(),
                migration_probe: args.migration_probe,
            },
            condition: args.condition,
            weights: args.weights,
            skid_margin: args.skid_margin,
            // Stamp the injection config the loop actually executed — an AA-6 run with
            // injection OFF is stamped OFF (not omitted), so the matrix checker sees the slip.
            injection: injection_attestation(args.stage, args.inject.as_ref()),
            attempted,
            planned,
        };
        let (manifest, records_jsonl) = assemble_run_set(context, &records)
            .map_err(|e| format!("assemble the run-set: {e}"))?;
        // A run-set is immutable evidence: reusing `--out` must never silently truncate
        // a prior run's records (and a failure mid-write would leave new records beside
        // an old manifest). Refuse an output directory that already exists, and create
        // each file with exclusive creation so even a race cannot clobber one.
        if args.out.exists() {
            return Err(format!(
                "refusing to overwrite {}: a run-set is immutable evidence — choose a fresh \
                 --out or move the existing directory aside",
                args.out.display()
            ));
        }
        std::fs::create_dir_all(&args.out)
            .map_err(|e| format!("create {}: {e}", args.out.display()))?;
        let write_new = |name: &str, bytes: &[u8]| -> Result<(), String> {
            let path = args.out.join(name);
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
            std::io::Write::write_all(&mut f, bytes).map_err(|e| format!("write {name}: {e}"))
        };
        write_new("records.jsonl", records_jsonl.as_bytes())?;
        write_new("run-set.json", manifest.as_bytes())?;
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
            cases,
            reps,
            with_targets,
        } => emit_plan(seed, cases, reps, with_targets),
        Command::Probe {
            box_config,
            rulings,
            out,
        } => probe(box_config, rulings, out),
        Command::Run(opts) => {
            let environment: Environment = read_json(&opts.environment)?;
            let weights: Option<Weights> = match &opts.weights {
                Some(p) => Some(read_json(p)?),
                None => None,
            };
            let payload_pins: std::collections::BTreeMap<String, String> =
                read_json(&opts.payload_pins)?;
            execute(RunArgs {
                payload_dir: opts.payload_dir,
                payload_pins,
                host_kernel_image: opts.host_kernel_image,
                host_kernel_sha256: opts.host_kernel_sha256,
                host_kernel_build_id: opts.host_kernel_build_id,
                core: opts.core,
                migration_probe: opts.migration_probe,
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
                cases: opts.cases,
                reps: opts.reps,
                with_targets: opts.with_targets,
                exclude_payloads: opts.exclude_payloads,
                // AA-2 is the stepping stage, so it implies single-step even without the flag.
                single_step: opts.single_step || matches!(opts.stage, StageArg::Aa2),
                max_steps: opts.max_steps,
                watchdog_secs: opts.watchdog_secs,
                inject: opts
                    .inject_ppi
                    .map(|intid| arm_harness::run::InjectionConfig { intid }),
            })
        }
        Command::LinuxBoot(opts) => linux_boot(*opts),
        Command::Aa4GuardReject(opts) => aa4_guard_reject(*opts),
        Command::Aa4GuardWrite(opts) => aa4_guard_write(*opts),
        Command::Aa4GuardNotifier(opts) => aa4_guard_notifier(*opts),
        Command::Aa4GuardBacking(opts) => aa4_guard_backing(*opts),
        Command::Aa4GuardRace(opts) => aa4_guard_race(*opts),
        Command::IdFreeze { out } => id_freeze(out),
        Command::VgicRoundtrip { out } => vgic_roundtrip(out),
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, Parser};
    use std::ffi::OsStr;

    #[test]
    fn aa2_cli_defaults_to_a_finite_step_budget() {
        let command = Cli::command();
        let run = command
            .find_subcommand("run")
            .expect("run subcommand must exist");
        let max_steps = run
            .get_arguments()
            .find(|arg| arg.get_id() == "max_steps")
            .expect("--max-steps must exist");
        assert_eq!(
            max_steps.get_default_values(),
            [OsStr::new("12000")],
            "AA-2 must fail closed with a finite default; explicit --max-steps 0 is the opt-in"
        );
    }

    #[test]
    fn aa4_guard_reject_cli_requires_a_pinned_artifact_and_core() {
        let cli = Cli::try_parse_from([
            "arm-spike",
            "aa4-guard-reject",
            "--image",
            "planted-exclusive",
            "--image-sha256",
            "11aa",
            "--core",
            "7",
        ])
        .expect("the documented planted-proof command must parse");

        let Command::Aa4GuardReject(opts) = cli.command else {
            panic!("aa4-guard-reject must select its dedicated command");
        };
        assert_eq!(opts.image, PathBuf::from("planted-exclusive"));
        assert_eq!(opts.image_sha256, "11aa");
        assert_eq!(opts.core, 7);
        assert_eq!(opts.watchdog_secs, arm_harness::run::DEFAULT_WATCHDOG_SECS);
    }

    #[test]
    fn linux_boot_exposes_explicit_stage2_guard_opt_in() {
        let command = Cli::command();
        let linux_boot = command
            .find_subcommand("linux-boot")
            .expect("linux-boot subcommand must exist");
        assert!(
            linux_boot
                .get_arguments()
                .any(|arg| arg.get_id() == "stage2_exec_guard"),
            "the guarded Linux smoke must remain an explicit CLI opt-in"
        );
    }

    #[test]
    fn aa4_guard_write_cli_is_hash_pinned_and_bounded() {
        let cli = Cli::try_parse_from([
            "arm-spike",
            "aa4-guard-write",
            "--image",
            "aa4-self-modify",
            "--image-sha256",
            "22bb",
            "--core",
            "9",
        ])
        .expect("the documented write/rescan proof command must parse");

        let Command::Aa4GuardWrite(opts) = cli.command else {
            panic!("aa4-guard-write must select its dedicated command");
        };
        assert_eq!(opts.image, PathBuf::from("aa4-self-modify"));
        assert_eq!(opts.image_sha256, "22bb");
        assert_eq!(opts.core, 9);
        assert_eq!(opts.max_exits, 1_000_000);
        assert_eq!(opts.watchdog_secs, arm_harness::run::DEFAULT_WATCHDOG_SECS);
    }

    #[test]
    fn injection_attestation_stamps_actual_config_and_marks_aa6_off_slip() {
        use arm_harness::evidence::Stage;
        use arm_harness::run::InjectionConfig;

        // Injection configured → ON, PPI stamped, no single at-work index (bare lane injects at
        // every landing); the stamp is coherent.
        let on = injection_attestation(Stage::Aa6, Some(&InjectionConfig { intid: 20 }))
            .expect("a configured injection is stamped");
        assert!(on.enabled && on.inject_ppi == Some(20) && on.inject_at_work.is_none());
        assert!(on.is_coherent());

        // AA-6 with injection OFF → stamped OFF (not omitted), so the matrix checker sees the
        // slip as "stamp says OFF" rather than a missing stamp.
        let off = injection_attestation(Stage::Aa6, None).expect("AA-6 always carries a stamp");
        assert!(!off.enabled && off.inject_ppi.is_none() && off.is_coherent());

        // A stage that does not inject carries no stamp.
        assert!(injection_attestation(Stage::Aa3, None).is_none());
        // …but an injecting non-AA6 run still attests its actual config.
        assert!(
            injection_attestation(Stage::Aa3, Some(&InjectionConfig { intid: 22 }))
                .is_some_and(|a| a.enabled && a.inject_ppi == Some(22))
        );
    }
}
