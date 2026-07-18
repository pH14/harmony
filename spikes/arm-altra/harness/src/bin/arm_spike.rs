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
    /// AA-2: cap a single-step run at N steps (0 = unbounded, run to the console sentinel).
    /// A bounded run stops at N steps OR the sentinel, whichever comes first — hitting N first
    /// is normal, not a failure. This is how the `llsc-atomics` livelock is bounded: each step
    /// clears the exclusive monitor, so `STXR` never succeeds and the retry loops forever,
    /// never reaching MARK_END or the sentinel. Every stepped record is registers-only except
    /// the last, which carries the full-payload digest, so replay-identity still catches memory
    /// divergence across the stepped window. Only meaningful with `--single-step`/`--stage aa2`.
    #[arg(long, default_value_t = 0)]
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
    // EXACT landing it is `skid_margin + 1`, because a target closer to `MARK_BEGIN` than the
    // margin cannot be landed by the Preempt (its 0..margin latency would overshoot such a small
    // target, or arming at/above it fails mechanism-attestation) — those deltas are simply not
    // drawn, rather than drawn and then failed.
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
    h.strip_prefix("sha256:").unwrap_or(h).to_ascii_lowercase()
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
        Some(sys::MigrationChurner::start(sys::current_tid(), cores))
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
    // MARK_BEGIN than `skid_margin`, so every target can be armed a full margin below and landed
    // exactly by the Preempt; every other configuration draws from 1.
    let target_lo = if matches!(args.mechanism, MechanismArg::Patched) && args.skid_margin.is_some()
    {
        args.skid_margin.unwrap().saturating_add(1)
    } else {
        1
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
                };
                // AA-3 exact landing rides the patched mechanism WITH a measured skid margin:
                // `run_sample_exact` re-arms the overflow `skid_margin` events below the target,
                // takes the `Preempt` below target, then single-steps up to `work == target`
                // (the run_until_overflow + single_step contract). A patched run WITHOUT a margin
                // stays the arm-at-target reliability proxy; the stock kick stays `run_sample`.
                if matches!(mechanism_kind, sys::Mechanism::Preempt) && args.skid_margin.is_some() {
                    run_sample_exact(&mut machine, &mut counter, &spec, args.skid_margin.unwrap())
                        .map(|r| vec![r])
                        .map_err(|e| e.to_string())
                } else {
                    run_sample(&mut machine, &mut counter, &spec)
                        .map(|r| vec![r])
                        .map_err(|e| e.to_string())
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
    // actually measured as `attempted` — the totality check then binds to the real records.
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
            attempted,
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
