// SPDX-License-Identifier: AGPL-3.0-or-later
//! AA-1(a) host-side EL0 counting: the evidence shapes and their assembly.
//!
//! `docs/ARM-ALTRA.md` §AA-1(a): pinned EL0 counting of oracle payloads,
//! differentially across scales, judged against the analytical oracle — the
//! expected shape is `oracle + a small constant offset`, the offset measured and
//! pinned per class, and a **variable** offset is a mismatch, not a calibration.
//!
//! The measured windows are the SAME `.s` bodies the guest payloads boot
//! (`payloads/oracles/src/asm/`), linked into a Linux EL0 binary: the mark base in
//! `x0` becomes an ordinary writable page (the mark `strb`s are plain stores; the
//! PL011 FR poll reads 0 = idle, so its back-edge is never taken), and the perf
//! counter brackets the call from outside. The count therefore exceeds the window
//! model by a per-class constant (the `bl`/`ret` pair and the enable/disable
//! tail) — exactly the "small constant offset" the stage pins.
//!
//! This module is the portable half: shapes, assembly, sha pinning — natively
//! tested. The syscalls live in `sys` and the `arm-el0-count` binary.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::evidence::{Environment, PerfConfig, Pinning, hex_lower};

/// The EL0 evidence schema version.
pub const EL0_SCHEMA_VERSION: u32 = 1;

/// The **record ceiling**: the most EL0 samples (one sample ⇒ one record ⇒ one
/// `el0-records.jsonl` line) a single plan may produce.
///
/// `arm-el0-count` is an operator-run tool with no untrusted caller, so this is a
/// generous bound, not a quota: the AA-1(a) full sweep is a few hundred samples
/// (5 classes × 3 scales × a handful of cases × reps). It exists to turn a hostile
/// `--cases u64::MAX` (or `--reps`) into a NAMED refusal instead of an OOM kill, the
/// same way the sibling guest planner's [`crate::plan::MAX_PLANNED_SAMPLES`] does.
pub const MAX_EL0_RECORDS: u64 = 10_000_000;

/// A deliberately generous upper bound on one serialized `El0Record` JSONL line, used
/// only to compute the [`MAX_EL0_FILE_BYTES`] file ceiling. Real lines are ~180 bytes;
/// 512 leaves headroom for the longest class/scale names plus full-width `u64` fields,
/// so the file estimate never UNDER-counts.
pub const EL0_RECORD_BYTES_ESTIMATE: u64 = 512;

/// The **file ceiling**: the most estimated `el0-records.jsonl` bytes a plan may write.
///
/// A second, independent guard behind the record ceiling: a plan under
/// [`MAX_EL0_RECORDS`] is still refused if its records would not fit here, so a future
/// change to the record shape (or ceiling) cannot silently reintroduce a multi-gigabyte
/// write. 1 GiB is ~2 million max-width records — far past any real sweep.
pub const MAX_EL0_FILE_BYTES: u64 = 1 << 30;

/// Why an EL0 plan was refused before it was built.
///
/// Every variant NAMES the ceiling it hit and the offending value, so the operator sees
/// *which* bound a hostile `--cases`/`--reps` tripped — never a bare OOM. Checked in
/// order (arithmetic, then count, then bytes) so the first genuine failure is reported.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum El0PlanError {
    /// The **product ceiling**: `classes × scales × cases × reps` overflows `u64`, so a
    /// near-`u64::MAX` argument would wrap to a small, plausible count. Refused before it
    /// can be mistaken for a modest plan.
    #[error(
        "the EL0 plan size classes({classes}) × scales({scales}) × cases({cases}) × \
         reps({reps}) overflows u64 — a hostile argument, refused before it wraps to a \
         plausible-looking count"
    )]
    ProductOverflow {
        /// Number of measurement classes.
        classes: u64,
        /// Number of scales swept.
        scales: u64,
        /// Distinct cases per class × scale.
        cases: u64,
        /// Repetitions per case.
        reps: u64,
    },
    /// The **record ceiling**: the plan would emit more than [`MAX_EL0_RECORDS`] samples.
    #[error(
        "the EL0 plan would produce {records} records, over the record ceiling of \
         {MAX_EL0_RECORDS} — refuse rather than reserve a hostile allocation"
    )]
    RecordCeiling {
        /// The record count the plan would produce.
        records: u64,
    },
    /// The **file ceiling**: the plan's estimated `el0-records.jsonl` size (records ×
    /// [`EL0_RECORD_BYTES_ESTIMATE`]) exceeds [`MAX_EL0_FILE_BYTES`] (or overflows `u64`).
    #[error(
        "the EL0 plan's {records} records would write ~{bytes} bytes of el0-records.jsonl, \
         over the file ceiling of {MAX_EL0_FILE_BYTES} bytes — refused"
    )]
    FileCeiling {
        /// The record count the plan would produce.
        records: u64,
        /// The estimated JSONL size in bytes (saturated for the message).
        bytes: u64,
    },
}

/// The manifest of one EL0 counting run-set (`el0-set.json`).
///
/// Deliberately carries **no result totals**: every verdict is recomputed from the
/// records, whose sha256 this manifest pins (the same discipline as the guest
/// run-set manifest).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct El0Manifest {
    /// [`EL0_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The stage this evidence belongs to — always `"aa1a"`.
    pub stage: String,
    /// Identifier for this run-set. Golden evidence is immutable; a rerun makes a
    /// new run-set.
    pub run_set_id: String,
    /// The machine, as found (same shape as the guest manifest).
    pub environment: Environment,
    /// The perf configuration, derived from the attr that was armed.
    pub perf: PerfConfig,
    /// `exclude_kernel`, derived from the armed attr. [`PerfConfig`] does not
    /// project it (the guest work clock never sets it), but for EL0 counting it is
    /// load-bearing — without it, scheduler/IRQ branches inflate every count — so
    /// the manifest attests it and the checker demands it.
    pub exclude_kernel: bool,
    /// `exclude_user`, derived from the armed attr. Must be `false`: EL0 *is* the
    /// counted execution.
    pub exclude_user: bool,
    /// Core pinning and governor posture.
    pub pinning: Pinning,
    /// The experimental condition (`pinned-solo`, …).
    pub condition: String,
    /// How many samples the plan attempted. The totality check demands exactly
    /// this many records.
    pub attempted: u64,
    /// sha256 of the exact `el0-records.jsonl` bytes.
    pub records_sha256: String,
    /// sha256 of the MEASURING BINARY itself (`/proc/self/exe`), when the tool
    /// could read it. The per-class constant offsets are properties of one built
    /// binary (its call/dispatch path is inside the counted region) — the smoke
    /// evidence caught straight-line's offset moving +12 → +14 across a rebuild —
    /// so run-sets from different binaries must never be summed into one offset
    /// claim. Optional for backward shape-compatibility; the aggregation check
    /// refuses to sum sets that do not all carry the SAME attested hash.
    #[serde(default)]
    pub tool_sha256: Option<String>,
}

/// One EL0 counting sample (`el0-records.jsonl`, one JSON object per line).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct El0Record {
    /// Position in the deterministic plan (0-based, contiguous).
    pub sample_id: u64,
    /// The payload class (`oracle_model::Payload::name` — `straight-line`,
    /// `branch-dense`).
    pub class: String,
    /// The scale name (`smoke`/`1e6`/`1e7`/`1e8`).
    pub scale: String,
    /// The seed this sample ran with (feeds the branch-dense PRNG; inert for
    /// straight-line).
    pub seed: u64,
    /// The trip count actually passed to the window (`oracle_model::trips`).
    pub trips: u64,
    /// Which repetition of this `(class, scale, seed)` case this is (0-based).
    pub rep: u64,
    /// The `BR_RETIRED` count read across the window call.
    pub count: u64,
    /// The accumulator the window returned — the executed predicates' witness,
    /// checked against the model's predicted accumulator by the checker.
    pub accumulator: u64,
    /// `PERF_FORMAT_TOTAL_TIME_ENABLED` at read.
    pub time_enabled: u64,
    /// `PERF_FORMAT_TOTAL_TIME_RUNNING` at read. Must equal `time_enabled` (the
    /// pinned event was never multiplexed off).
    pub time_running: u64,
}

/// The context [`assemble_el0_set`] needs beyond the records.
#[derive(Clone, Debug)]
pub struct El0Context {
    /// Run-set identifier.
    pub run_set_id: String,
    /// The machine, as found.
    pub environment: Environment,
    /// The perf configuration, derived from the armed attr.
    pub perf: PerfConfig,
    /// `exclude_kernel`, derived from the armed attr (see [`El0Manifest`]).
    pub exclude_kernel: bool,
    /// `exclude_user`, derived from the armed attr.
    pub exclude_user: bool,
    /// Pinning posture.
    pub pinning: Pinning,
    /// The experimental condition.
    pub condition: String,
    /// The full plan size (records may be fewer if a sample failed — the gap is
    /// the totality checker's to catch).
    pub attempted: u64,
    /// sha256 of the measuring binary (see [`El0Manifest::tool_sha256`]).
    pub tool_sha256: Option<String>,
}

/// Serialize the records to canonical JSONL and the manifest that pins them.
///
/// # Errors
/// A serialization failure (shapes are plain data; practically infallible).
pub fn assemble_el0_set(
    ctx: El0Context,
    records: &[El0Record],
) -> Result<(String, String), serde_json::Error> {
    let mut jsonl = String::new();
    for r in records {
        jsonl.push_str(&serde_json::to_string(r)?);
        jsonl.push('\n');
    }
    let mut h = Sha256::new();
    h.update(jsonl.as_bytes());
    let manifest = El0Manifest {
        schema_version: EL0_SCHEMA_VERSION,
        stage: "aa1a".to_string(),
        run_set_id: ctx.run_set_id,
        environment: ctx.environment,
        perf: ctx.perf,
        exclude_kernel: ctx.exclude_kernel,
        exclude_user: ctx.exclude_user,
        pinning: ctx.pinning,
        condition: ctx.condition,
        attempted: ctx.attempted,
        records_sha256: hex_lower(&h.finalize()),
        tool_sha256: ctx.tool_sha256,
    };
    let manifest_json = format!("{}\n", serde_json::to_string_pretty(&manifest)?);
    Ok((manifest_json, jsonl))
}

/// An EL0 measurement class — the AA-1(a) class set.
///
/// Two kinds:
///
/// - **Window classes** ([`El0Class::StraightLine`], [`El0Class::BranchDense`]):
///   the guest payloads' own counted `.s` windows, linked into the EL0 binary.
///   Their expected count is oracle-anchored ([`oracle_model::expected`]).
/// - **Kernel-mediated classes** ([`El0Class::Syscall`], [`El0Class::Signal`],
///   [`El0Class::PageFault`]): Linux-kernel round trips (SVC, signal delivery,
///   translation-fault + skip) whose per-trip `BR_RETIRED` contribution is an
///   unknown this stage MEASURES — the checker fits `count = a·trips + b` with
///   exact integer arithmetic and reports `(a, b)` as constants-pack output; a
///   record the fit does not explain exactly is a mismatch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum El0Class {
    /// The straight-line guest window (oracle-anchored).
    StraightLine,
    /// The branch-dense guest window (oracle-anchored).
    BranchDense,
    /// `getpid` via raw `SVC #0` per trip.
    Syscall,
    /// `kill(self, SIGUSR1)` per trip, delivered to an asm handler with a known
    /// branch count, returning through an owned `rt_sigreturn` restorer.
    Signal,
    /// A store to a `PROT_NONE` page per trip; the SIGSEGV handler skips the
    /// faulting instruction (`pc += 4`) — the EL0 mirror of the guest
    /// exception-abort payload.
    PageFault,
}

/// Every EL0 class, in plan order.
pub const ALL_EL0_CLASSES: [El0Class; 5] = [
    El0Class::StraightLine,
    El0Class::BranchDense,
    El0Class::Syscall,
    El0Class::Signal,
    El0Class::PageFault,
];

impl El0Class {
    /// The class name — the record key and the CLI spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            El0Class::StraightLine => "straight-line",
            El0Class::BranchDense => "branch-dense",
            El0Class::Syscall => "el0-syscall",
            El0Class::Signal => "el0-signal",
            El0Class::PageFault => "el0-pagefault",
        }
    }

    /// Parse a record's class name.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        ALL_EL0_CLASSES.iter().copied().find(|c| c.name() == name)
    }

    /// The oracle payload behind a window class (`None` for the kernel-mediated
    /// classes, whose expected count is a measured fit, not an oracle).
    #[must_use]
    pub const fn oracle_payload(self) -> Option<oracle_model::Payload> {
        match self {
            El0Class::StraightLine => Some(oracle_model::Payload::StraightLine),
            El0Class::BranchDense => Some(oracle_model::Payload::BranchDense),
            _ => None,
        }
    }

    /// Trip count per scale. Window classes use the guest table
    /// ([`oracle_model::trips`]); the kernel-mediated classes scale down (a
    /// signal round trip costs microseconds, not nanoseconds — the same
    /// per-payload shortening precedent as the guest WFI class), keeping three
    /// distinct magnitudes for the differential fit.
    #[must_use]
    pub const fn trips(self, scale: oracle_model::Scale) -> u64 {
        use oracle_model::Scale;
        match self {
            El0Class::StraightLine => {
                oracle_model::trips(oracle_model::Payload::StraightLine, scale)
            }
            El0Class::BranchDense => oracle_model::trips(oracle_model::Payload::BranchDense, scale),
            El0Class::Syscall => match scale {
                Scale::Smoke => 1_000,
                Scale::S1e6 => 200_000,
                Scale::S1e7 => 2_000_000,
                Scale::S1e8 => 20_000_000,
            },
            El0Class::Signal | El0Class::PageFault => match scale {
                Scale::Smoke => 200,
                Scale::S1e6 => 30_000,
                Scale::S1e7 => 300_000,
                Scale::S1e8 => 3_000_000,
            },
        }
    }
}

/// One sample of the deterministic EL0 plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct El0Sample {
    /// The measurement class.
    pub class: El0Class,
    /// The scale.
    pub scale: oracle_model::Scale,
    /// The per-case seed.
    pub seed: u64,
    /// The repetition index within the case.
    pub rep: u64,
}

/// The deterministic EL0 plan: for each class × scale × case, `reps` repetitions
/// of the same `(seed)` input. Case seeds derive from the master seed by
/// splitmix64 (stable, documented), so a run-set is a pure function of its spec.
#[must_use]
pub fn el0_plan(
    classes: &[El0Class],
    scales: &[oracle_model::Scale],
    master_seed: u64,
    cases: u64,
    reps: u64,
) -> Vec<El0Sample> {
    let mut out = Vec::new();
    let mut state = master_seed;
    let mut next = move || {
        // splitmix64 — the standard finalizer; deterministic and portable.
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    for &class in classes {
        for &scale in scales {
            for _ in 0..cases {
                let seed = next();
                for rep in 0..reps {
                    out.push(El0Sample {
                        class,
                        scale,
                        seed,
                        rep,
                    });
                }
            }
        }
    }
    out
}

/// The bounded entry point to [`el0_plan`]: refuse a plan that would overflow, exceed
/// the record ceiling, or write past the file ceiling BEFORE allocating anything, then
/// build it.
///
/// This is what the `arm-el0-count` binary calls. The pure [`el0_plan`] stays
/// infallible for internal/deterministic use; the check lives here so a hostile
/// `--cases`/`--reps` becomes an [`El0PlanError`] rather than an OOM. The three ceilings
/// are checked in order — arithmetic overflow, then record count, then estimated file
/// size — and the first one hit is reported with its offending value.
///
/// # Errors
/// [`El0PlanError`] when any of the product / record / file ceilings is exceeded.
pub fn el0_plan_bounded(
    classes: &[El0Class],
    scales: &[oracle_model::Scale],
    master_seed: u64,
    cases: u64,
    reps: u64,
) -> Result<Vec<El0Sample>, El0PlanError> {
    let n_classes = classes.len() as u64;
    let n_scales = scales.len() as u64;
    // Product ceiling: the multiplication itself must not overflow u64 — a near-u64::MAX
    // argument would otherwise wrap to a small, plausible count and plan silently.
    let records = n_classes
        .checked_mul(n_scales)
        .and_then(|a| a.checked_mul(cases))
        .and_then(|a| a.checked_mul(reps))
        .ok_or(El0PlanError::ProductOverflow {
            classes: n_classes,
            scales: n_scales,
            cases,
            reps,
        })?;
    // Record ceiling: a large-but-non-overflowing plan is still refused.
    if records > MAX_EL0_RECORDS {
        return Err(El0PlanError::RecordCeiling { records });
    }
    // File ceiling: even under the record ceiling, refuse a plan whose estimated JSONL
    // would exceed the byte budget. Saturating so a pathological estimate reports u64::MAX
    // rather than wrapping (records ≤ MAX_EL0_RECORDS here, so it never actually saturates).
    let bytes = records.saturating_mul(EL0_RECORD_BYTES_ESTIMATE);
    if bytes > MAX_EL0_FILE_BYTES {
        return Err(El0PlanError::FileCeiling { records, bytes });
    }
    Ok(el0_plan(classes, scales, master_seed, cases, reps))
}

/// One sample's raw EL0 measurement — the four numbers the perf counter and window
/// produced, before they are assembled into an [`El0Record`].
///
/// This is the boundary between the genuinely unsafe measurement (the `rt_sigaction` /
/// `mmap` / `ucontext` syscalls and the aarch64 window `global_asm`, which Miri cannot
/// execute and which stay in the `arm-el0-count` binary behind a `cfg(aarch64-linux)`
/// seam) and the portable record-assembly and partial-failure bookkeeping (which Miri
/// can). The measurement produces one of these; [`collect_el0_records`] turns a plan
/// plus a measurement seam into records.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct El0Measurement {
    /// The `BR_RETIRED` count read across the window call.
    pub count: u64,
    /// The accumulator the window returned (the executed-predicate witness).
    pub accumulator: u64,
    /// `PERF_FORMAT_TOTAL_TIME_ENABLED` at read.
    pub time_enabled: u64,
    /// `PERF_FORMAT_TOTAL_TIME_RUNNING` at read.
    pub time_running: u64,
}

/// Run each planned sample through `measure`, assembling the [`El0Record`] list and the
/// first-failure bookkeeping.
///
/// Portable and free of the measurement's `unsafe`: `measure` is the seam the real
/// (Linux/aarch64, syscall + window-asm) counter loop plugs into in the binary, while a
/// loopback fake drives THIS function — the per-class trip derivation, the record
/// assembly, the stop-at-first-failure that keeps partial evidence — under Miri, which
/// cannot execute the real measurement. That is what brings the EL0 bookkeeping under
/// the `unsafe ⇒ Miri` discipline the syscall/asm surface itself can never satisfy.
///
/// On a measurement failure it stops at that sample, KEEPING the records gathered so far
/// (a partial run-set with the full `attempted` count is how the totality checker sees
/// the gap) and returns the failure text with them.
pub fn collect_el0_records<M>(
    samples: &[El0Sample],
    mut measure: M,
) -> (Vec<El0Record>, Option<String>)
where
    M: FnMut(usize, &El0Sample) -> Result<El0Measurement, String>,
{
    let mut records = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        match measure(i, s) {
            Ok(m) => records.push(El0Record {
                sample_id: i as u64,
                class: s.class.name().to_string(),
                scale: s.scale.name().to_string(),
                seed: s.seed,
                trips: s.class.trips(s.scale),
                rep: s.rep,
                count: m.count,
                accumulator: m.accumulator,
                time_enabled: m.time_enabled,
                time_running: m.time_running,
            }),
            Err(e) => {
                return (
                    records,
                    Some(format!("sample {i} ({}): {e}", s.class.name())),
                );
            }
        }
    }
    (records, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oracle_model::Scale;

    #[test]
    fn the_plan_is_deterministic_and_repeats_the_same_seed_per_case() {
        let classes = [El0Class::StraightLine, El0Class::BranchDense];
        let scales = [Scale::Smoke, Scale::S1e6];
        let a = el0_plan(&classes, &scales, 7, 2, 3);
        let b = el0_plan(&classes, &scales, 7, 2, 3);
        assert_eq!(a, b, "same spec, same plan");
        assert_eq!(a.len(), 2 * 2 * 2 * 3);
        // Within one case, every rep repeats the SAME seed (replay identity needs
        // repeated inputs, not fresh draws — the round-2 lesson from the guest plan).
        let case: Vec<_> = a
            .iter()
            .filter(|s| s.class == El0Class::StraightLine && s.scale == Scale::Smoke)
            .collect();
        assert_eq!(case.len(), 6, "2 cases x 3 reps");
        assert_eq!(case[0].seed, case[1].seed);
        assert_eq!(case[1].seed, case[2].seed);
        assert_ne!(case[2].seed, case[3].seed, "a new case draws a new seed");
        assert_eq!(case[3].seed, case[4].seed);
        // A different master seed derives different case seeds.
        let c = el0_plan(&classes, &scales, 8, 2, 3);
        assert_ne!(a[0].seed, c[0].seed);
    }

    #[test]
    fn assembly_pins_the_exact_record_bytes() {
        let ctx = El0Context {
            run_set_id: "t".into(),
            environment: Environment {
                midr: 1,
                soc: "s".into(),
                firmware: std::collections::BTreeMap::new(),
                host_kernel: "k".into(),
                kvm_mode: "vhe".into(),
            },
            perf: PerfConfig {
                raw_event: 0x21,
                exclude_host: false,
                exclude_guest: true,
                exclude_hv: true,
                pinned: true,
                sample_period: None,
            },
            exclude_kernel: true,
            exclude_user: false,
            pinning: Pinning {
                pinned: true,
                core: Some(60),
                governor: "performance".into(),
                migration_probe: false,
            },
            condition: "pinned-solo".into(),
            attempted: 1,
            tool_sha256: Some("ab".repeat(32)),
        };
        let rec = El0Record {
            sample_id: 0,
            class: "straight-line".into(),
            scale: "smoke".into(),
            seed: 1,
            trips: 512,
            rep: 0,
            count: 513,
            accumulator: 3,
            time_enabled: 10,
            time_running: 10,
        };
        let (manifest, jsonl) = assemble_el0_set(ctx, std::slice::from_ref(&rec)).unwrap();
        let parsed: El0Manifest = serde_json::from_str(&manifest).unwrap();
        assert_eq!(parsed.schema_version, EL0_SCHEMA_VERSION);
        assert_eq!(parsed.stage, "aa1a");
        // The pinned sha is of the exact serialized bytes.
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(jsonl.as_bytes());
        assert_eq!(parsed.records_sha256, hex_lower(&h.finalize()));
        // Tampering with one record byte breaks the pin.
        let tampered = jsonl.replace("\"count\":513", "\"count\":514");
        let mut h2 = Sha256::new();
        h2.update(tampered.as_bytes());
        assert_ne!(parsed.records_sha256, hex_lower(&h2.finalize()));
    }

    // hm-8z7 (J14): the EL0 plan had no preflight bound, so `--cases u64::MAX` OOM-killed
    // the operator tool instead of failing with a message. Each ceiling is a hostile
    // argument that must be refused (by NAME) rather than allocated. Pre-fix `el0_plan`
    // has no bounded entry point at all; post-fix `el0_plan_bounded` refuses before the
    // Vec is ever reserved — so these never OOM even at u64::MAX.

    #[test]
    fn a_hostile_cases_overflows_the_product_and_is_refused_not_allocated() {
        // 5 classes × 1 scale × u64::MAX cases × 4 reps overflows u64. The bounded planner
        // must report the PRODUCT ceiling, naming the dimensions — never wrap to a small
        // count and plan silently, and never reserve a hostile allocation.
        let err = el0_plan_bounded(&ALL_EL0_CLASSES, &[Scale::Smoke], 0, u64::MAX, 4)
            .expect_err("a u64::MAX cases count must be refused");
        assert!(
            matches!(err, El0PlanError::ProductOverflow { cases, reps, .. } if cases == u64::MAX && reps == 4),
            "product overflow must name the offending dimensions, got: {err}"
        );
    }

    #[test]
    fn a_large_but_finite_plan_hits_the_record_ceiling_by_name() {
        // 5 × 4 × 1_000_000 × 1 = 20_000_000 records > MAX_EL0_RECORDS (10_000_000): fits
        // u64 (no overflow) but is absurd, so the RECORD ceiling refuses it with the count.
        let scales = [Scale::Smoke, Scale::S1e6, Scale::S1e7, Scale::S1e8];
        let err = el0_plan_bounded(&ALL_EL0_CLASSES, &scales, 0, 1_000_000, 1)
            .expect_err("20M records must be refused");
        assert!(
            matches!(err, El0PlanError::RecordCeiling { records } if records == 20_000_000),
            "record ceiling must name the count, got: {err}"
        );
    }

    #[test]
    fn a_plan_under_the_record_ceiling_can_still_hit_the_file_ceiling() {
        // 5 × 4 × 150_000 × 1 = 3_000_000 records: UNDER the 10M record ceiling, but
        // 3_000_000 × 512 ≈ 1.53 GiB is over the 1 GiB file ceiling. The two ceilings are
        // independent guards, and the file one must fire here — by name, with the bytes.
        let scales = [Scale::Smoke, Scale::S1e6, Scale::S1e7, Scale::S1e8];
        let err = el0_plan_bounded(&ALL_EL0_CLASSES, &scales, 0, 150_000, 1)
            .expect_err("a 3M-record plan exceeds the file ceiling");
        // 3_000_000 is under MAX_EL0_RECORDS (10M), so the RECORD ceiling did not fire —
        // only the independent FILE ceiling did, which is the point of this case.
        assert!(
            matches!(err, El0PlanError::FileCeiling { records, bytes }
                if records == 3_000_000 && bytes == 3_000_000 * EL0_RECORD_BYTES_ESTIMATE),
            "file ceiling must name the record count and estimated bytes, got: {err}"
        );
    }

    #[test]
    fn a_realistic_sweep_plans_normally_through_the_bounded_entry_point() {
        // The bound is a backstop, not a quota: the real AA-1(a) sweep passes unchanged and
        // yields exactly the same plan as the unchecked core.
        let scales = [Scale::Smoke, Scale::S1e6, Scale::S1e7];
        let checked =
            el0_plan_bounded(&ALL_EL0_CLASSES, &scales, 7, 4, 3).expect("a real sweep plans");
        let raw = el0_plan(&ALL_EL0_CLASSES, &scales, 7, 4, 3);
        assert_eq!(
            checked, raw,
            "the bounded planner delegates to the pure core unchanged"
        );
        assert_eq!(checked.len(), 5 * 3 * 4 * 3);
    }

    // hm-fou (J13): the record-assembly + partial-failure bookkeeping used to live inside
    // the binary's `cfg(aarch64-linux)` measurement module, so Miri (running on an
    // x86-64/aarch64-macOS host where that module is compiled out) never exercised it.
    // `collect_el0_records` is the portable seam; these drive it with a LOOPBACK fake in
    // place of the real syscall+asm measurement, so the bookkeeping now runs under Miri.

    fn plan_2x2() -> Vec<El0Sample> {
        el0_plan(
            &[El0Class::StraightLine, El0Class::Syscall],
            &[Scale::Smoke, Scale::S1e6],
            42,
            1,
            2,
        )
    }

    #[test]
    fn collect_assembles_one_record_per_sample_with_derived_trips() {
        let samples = plan_2x2();
        // A loopback measurement: echo the sample index into the counts, so the record's
        // provenance is checkable without any real counter.
        let (records, failure) = collect_el0_records(&samples, |i, _s| {
            Ok(El0Measurement {
                count: 1000 + i as u64,
                accumulator: i as u64,
                time_enabled: 7,
                time_running: 7,
            })
        });
        assert!(failure.is_none(), "every sample measured, so no failure");
        assert_eq!(records.len(), samples.len());
        for (i, (r, s)) in records.iter().zip(&samples).enumerate() {
            assert_eq!(r.sample_id, i as u64, "sample ids are dense 0..n");
            assert_eq!(r.class, s.class.name());
            assert_eq!(r.scale, s.scale.name());
            assert_eq!(r.seed, s.seed);
            assert_eq!(r.rep, s.rep);
            // Trips are DERIVED from the sample by the bookkeeping, not supplied by the seam.
            assert_eq!(r.trips, s.class.trips(s.scale));
            assert_eq!(r.count, 1000 + i as u64);
        }
    }

    #[test]
    fn collect_stops_at_the_first_failure_but_keeps_the_partial_evidence() {
        let samples = plan_2x2();
        // Fail on the third sample: the first two records must survive (the gap is the
        // totality checker's to catch), and the failure must name the sample and its class.
        let (records, failure) = collect_el0_records(&samples, |i, _s| {
            if i == 2 {
                Err("counter wedged".to_string())
            } else {
                Ok(El0Measurement {
                    count: i as u64,
                    accumulator: 0,
                    time_enabled: 0,
                    time_running: 0,
                })
            }
        });
        assert_eq!(
            records.len(),
            2,
            "partial evidence up to the failing sample is kept"
        );
        let msg = failure.expect("the third sample failed");
        assert!(
            msg.contains("sample 2"),
            "the failure names the sample: {msg}"
        );
        assert!(
            msg.contains("counter wedged"),
            "the failure carries the cause: {msg}"
        );
    }
}
