// SPDX-License-Identifier: AGPL-3.0-or-later
//! The AA-1(a) EL0 counting checker — `el0-check`'s logic.
//!
//! Grades an `arm-el0-count` run-set (`el0-set.json` + `el0-records.jsonl`,
//! shapes in [`arm_harness::el0`]) by the same doctrine as the guest checker:
//! recompute everything from the retained records, judge counts only against the
//! analytical oracle ([`oracle_model::expected`]), and never read a verdict the
//! harness wrote about itself.
//!
//! The stage's claim (`docs/ARM-ALTRA.md` §AA-1(a)): pinned EL0 counts equal
//! `oracle + a small constant offset`, the offset **measured and pinned per
//! class**, constant across scales, seeds and repetitions — "a variable offset is
//! a mismatch, not a calibration."

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use arm_harness::el0::{EL0_SCHEMA_VERSION, El0Manifest, El0Record};
use arm_harness::evidence::hex_lower;
use oracle_model::{Payload, Scale};
use sha2::{Digest, Sha256};

use crate::check::Status;

/// Which EL0 check produced an outcome.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum El0CheckId {
    /// Schema version, stage tag, sha pin, id contiguity, recognized classes.
    WellFormed,
    /// Exactly `attempted` records are present (a missing sample is a failure to
    /// account, not a pass).
    Totality,
    /// Every record's pinned event ran for its whole enabled window.
    Scheduling,
    /// The armed attr was the EL0 work-clock config (raw 0x21, pinned,
    /// kernel-excluded, user-included, host-included, counting mode).
    PerfConfig,
    /// Every record's returned accumulator equals the model's prediction — the
    /// witness that the executed predicates matched the modeled ones.
    Accumulator,
    /// Same `(class, scale, seed, condition)` ⇒ bit-identical count.
    ReplayIdentity,
    /// Per class, `count − oracle` is ONE constant across every scale and seed.
    OracleExactness,
    /// The differential needs the full 1e6/1e7/1e8 sweep per class.
    ScaleCoverage,
    /// The caller-named per-case repetition floor.
    RepFloor,
    /// The caller-named distinct-cases floor.
    CaseFloor,
    /// Multi-set aggregation: no duplicate evidence, comparable environments.
    Aggregation,
}

impl El0CheckId {
    /// Stable name for output.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            El0CheckId::WellFormed => "well-formed",
            El0CheckId::Totality => "totality",
            El0CheckId::Scheduling => "scheduling",
            El0CheckId::PerfConfig => "perf-config",
            El0CheckId::Accumulator => "accumulator",
            El0CheckId::ReplayIdentity => "replay-identity",
            El0CheckId::OracleExactness => "oracle-exactness",
            El0CheckId::ScaleCoverage => "scale-coverage",
            El0CheckId::RepFloor => "rep-floor",
            El0CheckId::CaseFloor => "case-floor",
            El0CheckId::Aggregation => "aggregation",
        }
    }
}

impl fmt::Display for El0CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One EL0 check's result.
#[derive(Clone, Debug)]
pub struct El0Outcome {
    /// Which check.
    pub id: El0CheckId,
    /// Its verdict.
    pub status: Status,
    /// The one-line explanation — on a pass, the exact numbers the verdict rests
    /// on (the measured per-class offsets ARE the stage deliverable).
    pub detail: String,
}

/// The caller-named floors.
#[derive(Clone, Copy, Debug, Default)]
pub struct El0Floors {
    /// Every `(class, scale, seed)` case must repeat at least this often.
    pub min_reps: Option<u64>,
    /// At least this many distinct seeds per `(class, scale)`.
    pub min_cases: Option<u64>,
    /// Permit smoke-scale-only coverage (dev runs); tagged, never normative.
    pub sub_normative: bool,
}

/// The full EL0 verdict.
#[derive(Clone, Debug)]
pub struct El0Report {
    /// The run-set ids graded (aggregation order).
    pub run_set_ids: Vec<String>,
    /// Per-check outcomes.
    pub outcomes: Vec<El0Outcome>,
}

impl El0Report {
    /// Every check passed.
    #[must_use]
    pub fn passed(&self) -> bool {
        self.outcomes.iter().all(|o| o.status == Status::Pass)
    }
}

/// A load failure (I/O or parse) — not a verdict.
#[derive(Debug, thiserror::Error)]
pub enum El0LoadError {
    /// A file could not be read.
    #[error("cannot read {path}: {source}")]
    Read {
        /// The path.
        path: std::path::PathBuf,
        /// The underlying error.
        source: std::io::Error,
    },
    /// A file could not be parsed.
    #[error("cannot parse {path}: {source}")]
    Parse {
        /// The path.
        path: std::path::PathBuf,
        /// The underlying error.
        source: serde_json::Error,
    },
}

/// One loaded run-set.
struct Loaded {
    manifest: El0Manifest,
    records: Vec<El0Record>,
    records_bytes: Vec<u8>,
}

fn load(dir: &Path) -> Result<Loaded, El0LoadError> {
    let mpath = dir.join("el0-set.json");
    let mbytes = std::fs::read(&mpath).map_err(|source| El0LoadError::Read {
        path: mpath.clone(),
        source,
    })?;
    let manifest: El0Manifest =
        serde_json::from_slice(&mbytes).map_err(|source| El0LoadError::Parse {
            path: mpath,
            source,
        })?;
    let rpath = dir.join("el0-records.jsonl");
    let records_bytes = std::fs::read(&rpath).map_err(|source| El0LoadError::Read {
        path: rpath.clone(),
        source,
    })?;
    let mut records = Vec::new();
    for line in records_bytes.split(|&b| b == b'\n') {
        if line.is_empty() {
            continue;
        }
        records.push(serde_json::from_slice::<El0Record>(line).map_err(|source| {
            El0LoadError::Parse {
                path: rpath.clone(),
                source,
            }
        })?);
    }
    Ok(Loaded {
        manifest,
        records,
        records_bytes,
    })
}

fn payload_of(class: &str) -> Option<Payload> {
    match class {
        "straight-line" => Some(Payload::StraightLine),
        "branch-dense" => Some(Payload::BranchDense),
        _ => None,
    }
}

fn scale_of(name: &str) -> Option<Scale> {
    match name {
        "smoke" => Some(Scale::Smoke),
        "1e6" => Some(Scale::S1e6),
        "1e7" => Some(Scale::S1e7),
        "1e8" => Some(Scale::S1e8),
        _ => None,
    }
}

/// Check one or more EL0 run-set directories as ONE verdict.
///
/// # Errors
/// [`El0LoadError`] if a directory's files cannot be read or parsed — a load
/// failure, not a verdict.
pub fn check_el0_sets(dirs: &[&Path], floors: &El0Floors) -> Result<El0Report, El0LoadError> {
    let mut loaded = Vec::new();
    for d in dirs {
        loaded.push(load(d)?);
    }
    Ok(grade(&loaded, floors))
}

#[allow(clippy::too_many_lines)] // one linear pass per check, kept side by side deliberately
fn grade(sets: &[Loaded], floors: &El0Floors) -> El0Report {
    let mut outcomes: Vec<El0Outcome> = Vec::new();
    let mut push = |id, status, detail: String| {
        outcomes.push(El0Outcome { id, status, detail });
    };

    // -- aggregation: duplicates and comparability, before anything is summed.
    {
        let mut seen_sha: BTreeMap<String, usize> = BTreeMap::new();
        let mut dup = None;
        for (i, s) in sets.iter().enumerate() {
            if let Some(prev) = seen_sha.insert(s.manifest.records_sha256.clone(), i) {
                dup = Some((prev, i));
            }
        }
        let comparable = sets.windows(2).all(|w| {
            w[0].manifest.environment == w[1].manifest.environment
                && w[0].manifest.perf == w[1].manifest.perf
                && w[0].manifest.exclude_kernel == w[1].manifest.exclude_kernel
                && w[0].manifest.exclude_user == w[1].manifest.exclude_user
        });
        if let Some((a, b)) = dup {
            push(
                El0CheckId::Aggregation,
                Status::Fail,
                format!(
                    "run-sets #{a} and #{b} carry identical records_sha256 — the same evidence may not be summed twice"
                ),
            );
        } else if !comparable {
            push(
                El0CheckId::Aggregation,
                Status::Fail,
                "aggregated run-sets differ in environment or perf config — not comparable, may not be summed".to_string(),
            );
        } else {
            push(
                El0CheckId::Aggregation,
                Status::Pass,
                format!("{} distinct, comparable run-set(s)", sets.len()),
            );
        }
    }

    // -- well-formed + totality + scheduling + perf-config, per set; records pooled after.
    let mut wf_fail: Vec<String> = Vec::new();
    let mut tot_fail: Vec<String> = Vec::new();
    let mut sched_fail: Vec<String> = Vec::new();
    let mut perf_fail: Vec<String> = Vec::new();
    let mut total_records = 0u64;
    for s in sets {
        let m = &s.manifest;
        let id = &m.run_set_id;
        if m.schema_version != EL0_SCHEMA_VERSION {
            wf_fail.push(format!("{id}: schema_version {}", m.schema_version));
        }
        if m.stage != "aa1a" {
            wf_fail.push(format!("{id}: stage {:?} (must be aa1a)", m.stage));
        }
        let mut h = Sha256::new();
        h.update(&s.records_bytes);
        if hex_lower(&h.finalize()) != m.records_sha256 {
            wf_fail.push(format!(
                "{id}: records_sha256 does not match the records file"
            ));
        }
        for (i, r) in s.records.iter().enumerate() {
            if r.sample_id != i as u64 {
                wf_fail.push(format!("{id}: sample_id {} at line {i}", r.sample_id));
                break;
            }
        }
        for r in &s.records {
            if payload_of(&r.class).is_none() || scale_of(&r.scale).is_none() {
                wf_fail.push(format!(
                    "{id}: unrecognized class/scale {:?}/{:?}",
                    r.class, r.scale
                ));
                break;
            }
        }
        if !m.pinning.pinned || m.pinning.core.is_none() {
            wf_fail.push(format!(
                "{id}: EL0 counting must be pinned to a recorded core"
            ));
        }
        if s.records.len() as u64 != m.attempted {
            tot_fail.push(format!(
                "{id}: {} record(s) of {} attempted",
                s.records.len(),
                m.attempted
            ));
        }
        if m.attempted == 0 {
            tot_fail.push(format!(
                "{id}: attempted is 0 — an empty run-set is not evidence"
            ));
        }
        total_records += s.records.len() as u64;
        for r in &s.records {
            if r.time_running == 0 || r.time_running != r.time_enabled {
                sched_fail.push(format!(
                    "{id}#{}: running {} of enabled {} — multiplexed or never scheduled",
                    r.sample_id, r.time_running, r.time_enabled
                ));
                break;
            }
        }
        let p = &m.perf;
        if p.raw_event != 0x21
            || !p.pinned
            || p.exclude_host
            || p.sample_period.is_some()
            || !m.exclude_kernel
            || m.exclude_user
        {
            perf_fail.push(format!(
                "{id}: armed attr is not the EL0 work clock (raw={:#x} pinned={} exclude_host={} \
                 exclude_kernel={} exclude_user={} period={:?})",
                p.raw_event,
                p.pinned,
                p.exclude_host,
                m.exclude_kernel,
                m.exclude_user,
                p.sample_period
            ));
        }
    }
    let verdict = |fails: &[String], ok: String| -> (Status, String) {
        if fails.is_empty() {
            (Status::Pass, ok)
        } else {
            (Status::Fail, fails.join("; "))
        }
    };
    let (st, d) = verdict(&wf_fail, format!("{} set(s) well-formed", sets.len()));
    push(El0CheckId::WellFormed, st, d);
    let (st, d) = verdict(
        &tot_fail,
        format!("{total_records} record(s), every attempt present"),
    );
    push(El0CheckId::Totality, st, d);
    let (st, d) = verdict(
        &sched_fail,
        "every record pinned-scheduled for its whole window".to_string(),
    );
    push(El0CheckId::Scheduling, st, d);
    let (st, d) = verdict(
        &perf_fail,
        "raw 0x21, pinned, kernel-excluded, user+host-included, counting mode".to_string(),
    );
    push(El0CheckId::PerfConfig, st, d);

    // Pool the records (aggregation holds or the verdict already fails).
    let pooled: Vec<&El0Record> = sets.iter().flat_map(|s| s.records.iter()).collect();

    // -- accumulator: the executed predicates match the model, per record.
    // Memoized: branch-dense recomputes a full PRNG sweep per (seed, trips).
    {
        let mut memo: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        let mut fail = None;
        for r in &pooled {
            let Some(p) = payload_of(&r.class) else {
                continue;
            };
            let want = match p {
                Payload::StraightLine => oracle_model::straight_line_accumulator(r.trips),
                Payload::BranchDense => *memo
                    .entry((r.seed, r.trips))
                    .or_insert_with(|| oracle_model::branch_dense_accumulator(r.seed, r.trips)),
                _ => continue,
            };
            if r.accumulator != want {
                fail = Some(format!(
                    "{}#{} ({}): accumulator {:#x}, model predicts {:#x} — the executed \
                     predicates are not the modeled ones",
                    r.class, r.sample_id, r.scale, r.accumulator, want
                ));
                break;
            }
        }
        match fail {
            Some(d) => push(El0CheckId::Accumulator, Status::Fail, d),
            None => push(
                El0CheckId::Accumulator,
                Status::Pass,
                format!("{} record(s) match the predicted accumulator", pooled.len()),
            ),
        }
    }

    // -- replay identity: same (class, scale, seed, condition) ⇒ identical count.
    {
        let mut groups: BTreeMap<(String, String, u64, String), Vec<u64>> = BTreeMap::new();
        for (s, r) in sets
            .iter()
            .flat_map(|s| s.records.iter().map(move |r| (s, r)))
        {
            groups
                .entry((
                    r.class.clone(),
                    r.scale.clone(),
                    r.seed,
                    s.manifest.condition.clone(),
                ))
                .or_default()
                .push(r.count);
        }
        let repeated: Vec<_> = groups.iter().filter(|(_, v)| v.len() >= 2).collect();
        let divergent: Vec<String> = repeated
            .iter()
            .filter(|(_, v)| v.iter().any(|&c| c != v[0]))
            .map(|((c, sc, seed, cond), v)| {
                format!("({c}, {sc}, seed {seed:#x}, {cond}): counts {v:?} diverge")
            })
            .collect();
        if !divergent.is_empty() {
            push(
                El0CheckId::ReplayIdentity,
                Status::Fail,
                divergent.join("; "),
            );
        } else if repeated.is_empty() {
            push(
                El0CheckId::ReplayIdentity,
                Status::NotRequested,
                "no case was repeated — replay identity was never exercised (run with --reps ≥ 2)"
                    .to_string(),
            );
        } else {
            push(
                El0CheckId::ReplayIdentity,
                Status::Pass,
                format!(
                    "{} repeated case(s), every repetition bit-identical",
                    repeated.len()
                ),
            );
        }
    }

    // -- oracle exactness: per class ONE constant offset across scales and seeds.
    {
        let mut expected_memo: BTreeMap<(String, String, u64), u64> = BTreeMap::new();
        let mut per_class: BTreeMap<String, BTreeMap<i128, u64>> = BTreeMap::new();
        let mut arithmetic_fail = None;
        for r in &pooled {
            let (Some(p), Some(sc)) = (payload_of(&r.class), scale_of(&r.scale)) else {
                continue;
            };
            let want = *expected_memo
                .entry((r.class.clone(), r.scale.clone(), r.seed))
                .or_insert_with(|| oracle_model::expected(p, sc, r.seed).certain_branches);
            if oracle_model::trips(p, sc) != r.trips {
                arithmetic_fail = Some(format!(
                    "{}#{}: recorded trips {} is not the model's {} for scale {}",
                    r.class,
                    r.sample_id,
                    r.trips,
                    oracle_model::trips(p, sc),
                    r.scale
                ));
                break;
            }
            let offset = i128::from(r.count) - i128::from(want);
            *per_class
                .entry(r.class.clone())
                .or_default()
                .entry(offset)
                .or_insert(0) += 1;
        }
        if let Some(d) = arithmetic_fail {
            push(El0CheckId::OracleExactness, Status::Fail, d);
        } else {
            let mut fails = Vec::new();
            let mut oks = Vec::new();
            for (class, offsets) in &per_class {
                if offsets.len() == 1 {
                    let (off, n) = offsets
                        .iter()
                        .next()
                        .map(|(o, n)| (*o, *n))
                        .unwrap_or((0, 0));
                    if off < 0 {
                        fails.push(format!(
                            "{class}: count BELOW the oracle by {} — impossible for a real \
                             superset window; a mismatch, not an offset",
                            -off
                        ));
                    } else {
                        oks.push(format!("{class}: offset {off:+} over {n} record(s)"));
                    }
                } else {
                    fails.push(format!(
                        "{class}: offsets vary across records ({:?}) — a variable offset is a \
                         mismatch, not a calibration",
                        offsets
                    ));
                }
            }
            if per_class.is_empty() {
                fails.push("no gradeable records".to_string());
            }
            if fails.is_empty() {
                push(El0CheckId::OracleExactness, Status::Pass, oks.join("; "));
            } else {
                push(El0CheckId::OracleExactness, Status::Fail, fails.join("; "));
            }
        }
    }

    // -- scale coverage: the differential sweep, per class.
    {
        let mut cover: BTreeMap<String, std::collections::BTreeSet<String>> = BTreeMap::new();
        for r in &pooled {
            cover
                .entry(r.class.clone())
                .or_default()
                .insert(r.scale.clone());
        }
        let need = ["1e6", "1e7", "1e8"];
        let missing: Vec<String> = cover
            .iter()
            .filter(|(_, s)| !need.iter().all(|n| s.contains(*n)))
            .map(|(c, s)| format!("{c}: has {s:?}, needs 1e6+1e7+1e8"))
            .collect();
        if missing.is_empty() && !cover.is_empty() {
            push(
                El0CheckId::ScaleCoverage,
                Status::Pass,
                format!(
                    "{} class(es) cover the 1e6/1e7/1e8 differential",
                    cover.len()
                ),
            );
        } else if floors.sub_normative {
            push(
                El0CheckId::ScaleCoverage,
                Status::Pass,
                format!(
                    "[SUB-NORMATIVE] differential sweep incomplete: {}",
                    missing.join("; ")
                ),
            );
        } else {
            push(El0CheckId::ScaleCoverage, Status::Fail, missing.join("; "));
        }
    }

    // -- caller-named floors. A floor of zero is met by measuring nothing — fail closed.
    if let Some(min) = floors.min_reps {
        if min == 0 {
            push(
                El0CheckId::RepFloor,
                Status::Fail,
                "--min-reps 0 is met by repeating nothing — a zero floor is not a floor".into(),
            );
        } else {
            let mut groups: BTreeMap<(String, String, u64), u64> = BTreeMap::new();
            for r in &pooled {
                *groups
                    .entry((r.class.clone(), r.scale.clone(), r.seed))
                    .or_insert(0) += 1;
            }
            let worst = groups.iter().min_by_key(|(_, n)| **n);
            match worst {
                Some((k, n)) if *n < min => push(
                    El0CheckId::RepFloor,
                    Status::Fail,
                    format!("least-repeated case {k:?} has {n} rep(s), floor is {min}"),
                ),
                Some((_, n)) => push(
                    El0CheckId::RepFloor,
                    Status::Pass,
                    format!("every case repeated ≥ {n} time(s), meets the floor of {min}"),
                ),
                None => push(El0CheckId::RepFloor, Status::Fail, "no cases at all".into()),
            }
        }
    }
    if let Some(min) = floors.min_cases {
        if min == 0 {
            push(
                El0CheckId::CaseFloor,
                Status::Fail,
                "--min-cases 0 is met by measuring nothing — a zero floor is not a floor".into(),
            );
        } else {
            let mut cases: BTreeMap<(String, String), std::collections::BTreeSet<u64>> =
                BTreeMap::new();
            for r in &pooled {
                cases
                    .entry((r.class.clone(), r.scale.clone()))
                    .or_default()
                    .insert(r.seed);
            }
            let worst = cases.iter().min_by_key(|(_, s)| s.len());
            match worst {
                Some((k, s)) if (s.len() as u64) < min => push(
                    El0CheckId::CaseFloor,
                    Status::Fail,
                    format!("{k:?} has {} distinct seed(s), floor is {min}", s.len()),
                ),
                Some(_) => push(
                    El0CheckId::CaseFloor,
                    Status::Pass,
                    format!("every (class, scale) meets the distinct-case floor of {min}"),
                ),
                None => push(
                    El0CheckId::CaseFloor,
                    Status::Fail,
                    "no cases at all".into(),
                ),
            }
        }
    }

    El0Report {
        run_set_ids: sets.iter().map(|s| s.manifest.run_set_id.clone()).collect(),
        outcomes,
    }
}
