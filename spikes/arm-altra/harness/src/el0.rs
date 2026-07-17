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

use crate::evidence::{Environment, PerfConfig, Pinning, hex_lower};

/// The EL0 evidence schema version.
pub const EL0_SCHEMA_VERSION: u32 = 1;

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
    };
    let manifest_json = format!("{}\n", serde_json::to_string_pretty(&manifest)?);
    Ok((manifest_json, jsonl))
}

/// One sample of the deterministic EL0 plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct El0Sample {
    /// The payload class.
    pub payload: oracle_model::Payload,
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
    classes: &[oracle_model::Payload],
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
    for &payload in classes {
        for &scale in scales {
            for _ in 0..cases {
                let seed = next();
                for rep in 0..reps {
                    out.push(El0Sample {
                        payload,
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

#[cfg(test)]
mod tests {
    use super::*;
    use oracle_model::{Payload, Scale};

    #[test]
    fn the_plan_is_deterministic_and_repeats_the_same_seed_per_case() {
        let classes = [Payload::StraightLine, Payload::BranchDense];
        let scales = [Scale::Smoke, Scale::S1e6];
        let a = el0_plan(&classes, &scales, 7, 2, 3);
        let b = el0_plan(&classes, &scales, 7, 2, 3);
        assert_eq!(a, b, "same spec, same plan");
        assert_eq!(a.len(), 2 * 2 * 2 * 3);
        // Within one case, every rep repeats the SAME seed (replay identity needs
        // repeated inputs, not fresh draws — the round-2 lesson from the guest plan).
        let case: Vec<_> = a
            .iter()
            .filter(|s| s.payload == Payload::StraightLine && s.scale == Scale::Smoke)
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
}
