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

use arm_harness::el0::{EL0_SCHEMA_VERSION, El0Class, El0Manifest, El0Record};
use arm_harness::evidence::hex_lower;
use oracle_model::Scale;
use sha2::{Digest, Sha256};

use crate::check::Status;

/// The contamination conditions the AA-1(a) EL0 matrix must cover — the same four
/// as the guest AA-1 matrix. A normative verdict requires every one present.
const REQUIRED_EL0_CONDITIONS: &[&str] = &[
    "pinned-solo",
    "co-tenant-other-core",
    "co-tenant-same-core",
    "memory-pressure",
];

/// The five EL0 classes AA-1(a) must characterize under every condition: the two
/// window classes (`straight-line`, `branch-dense`) and the three kernel-mediated
/// classes (`el0-syscall`, `el0-signal`, `el0-pagefault`). A submission missing a
/// class in a condition never measured that condition's contamination for it.
const REQUIRED_EL0_CLASSES: &[El0Class] = &[
    El0Class::StraightLine,
    El0Class::BranchDense,
    El0Class::Syscall,
    El0Class::Signal,
    El0Class::PageFault,
];

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
    /// Every required EL0 class is measured under every required contamination
    /// condition — the full class×condition matrix, rectangular (no partially
    /// measured condition, no missing condition).
    CoverageMatrix,
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
            El0CheckId::CoverageMatrix => "coverage-matrix",
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

fn class_of(class: &str) -> Option<El0Class> {
    El0Class::from_name(class)
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
        // Comparable sets share the measured posture: environment, the armed attr, the
        // EL0/EL1 exclusions, AND the pinning (core + governor). A count is core- and
        // frequency-independent by the stage's own claim, but summing sets measured under
        // different pinning postures would sum evidence gathered under different contracts —
        // the aggregation must attest one posture, not paper over a drift in it.
        let comparable = sets.windows(2).all(|w| {
            w[0].manifest.environment == w[1].manifest.environment
                && w[0].manifest.perf == w[1].manifest.perf
                && w[0].manifest.exclude_kernel == w[1].manifest.exclude_kernel
                && w[0].manifest.exclude_user == w[1].manifest.exclude_user
                && w[0].manifest.pinning == w[1].manifest.pinning
        });
        // The per-class offsets are properties of one built binary (the dispatch
        // path is inside the counted region; a rebuild moved straight-line's
        // offset +12 → +14). Summing across binaries would manufacture a
        // variable-offset failure — or worse, mask one. Sets may sum only when
        // every one attests the SAME tool hash.
        let same_tool = sets.len() < 2
            || sets.windows(2).all(|w| {
                w[0].manifest.tool_sha256.is_some()
                    && w[0].manifest.tool_sha256 == w[1].manifest.tool_sha256
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
        } else if !same_tool {
            push(
                El0CheckId::Aggregation,
                Status::Fail,
                "aggregated run-sets were not all measured by one attested tool binary — \
                 per-class offsets are per-binary constants and may not be summed"
                    .to_string(),
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
            if class_of(&r.class).is_none() || scale_of(&r.scale).is_none() {
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
        // EL0 host counting: raw 0x21 (BR_RETIRED), pinned, counting mode (no period),
        // count EL0 user (exclude_user=false) and NOT EL1 (exclude_kernel=true) on the
        // host (exclude_host=false), and NOT the hypervisor (exclude_hv=true) — EL2
        // branches are apparatus, not the measured EL0 window, and leaving them counted
        // inflates the offset. `exclude_hv` was attested but never demanded until now.
        if p.raw_event != 0x21
            || !p.pinned
            || p.exclude_host
            || !p.exclude_hv
            || p.sample_period.is_some()
            || !m.exclude_kernel
            || m.exclude_user
        {
            perf_fail.push(format!(
                "{id}: armed attr is not the EL0 work clock (raw={:#x} pinned={} exclude_host={} \
                 exclude_hv={} exclude_kernel={} exclude_user={} period={:?})",
                p.raw_event,
                p.pinned,
                p.exclude_host,
                p.exclude_hv,
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

    // -- accumulator: the executed-path witness, per record. Window classes match
    // the model's predicted accumulator; kernel classes return their per-trip
    // witness (getpid matches / handler hits), which must equal `trips` exactly —
    // the kernel path engaged once per trip, no more, no less.
    // Memoized: branch-dense recomputes a full PRNG sweep per (seed, trips), and
    // straight-line an O(trips) fold per record — both keyed so the 1e8 sweep is
    // computed ONCE per (seed,)trips, not once per record. Without the straight-line
    // memo, grading the documented 1e8 records is impractical (an un-memoized pass
    // over 30 reps × 1e8 trips does not terminate in a usable time).
    {
        let mut memo: BTreeMap<(u64, u64), u64> = BTreeMap::new();
        let mut sl_memo: BTreeMap<u64, u64> = BTreeMap::new();
        let mut fail = None;
        for r in &pooled {
            let Some(c) = class_of(&r.class) else {
                continue;
            };
            let want = match c {
                El0Class::StraightLine => *sl_memo
                    .entry(r.trips)
                    .or_insert_with(|| oracle_model::straight_line_accumulator(r.trips)),
                El0Class::BranchDense => *memo
                    .entry((r.seed, r.trips))
                    .or_insert_with(|| oracle_model::branch_dense_accumulator(r.seed, r.trips)),
                El0Class::Syscall | El0Class::Signal | El0Class::PageFault => r.trips,
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

    // -- oracle exactness. Window classes: ONE constant offset per class over the
    // oracle's certain_branches, across scales and seeds. Kernel-mediated
    // classes: no oracle exists — the per-trip event contribution IS the
    // measured unknown — so the check demands an EXACT integer linear model
    // `count = a·trips + b` over every record (≥2 distinct trip magnitudes), and
    // REPORTS (a, b) as constants-pack output. Either way, a record the model
    // does not explain exactly is a mismatch, never a calibration.
    {
        let mut expected_memo: BTreeMap<(String, String, u64), u64> = BTreeMap::new();
        let mut per_class: BTreeMap<String, BTreeMap<i128, u64>> = BTreeMap::new();
        let mut kernel_points: BTreeMap<String, BTreeMap<u64, u64>> = BTreeMap::new();
        let mut arithmetic_fail = None;
        for r in &pooled {
            let (Some(c), Some(sc)) = (class_of(&r.class), scale_of(&r.scale)) else {
                continue;
            };
            if c.trips(sc) != r.trips {
                arithmetic_fail = Some(format!(
                    "{}#{}: recorded trips {} is not the class's {} for scale {}",
                    r.class,
                    r.sample_id,
                    r.trips,
                    c.trips(sc),
                    r.scale
                ));
                break;
            }
            if let Some(p) = c.oracle_payload() {
                let want = *expected_memo
                    .entry((r.class.clone(), r.scale.clone(), r.seed))
                    .or_insert_with(|| oracle_model::expected(p, sc, r.seed).certain_branches);
                let offset = i128::from(r.count) - i128::from(want);
                *per_class
                    .entry(r.class.clone())
                    .or_default()
                    .entry(offset)
                    .or_insert(0) += 1;
            } else {
                match kernel_points
                    .entry(r.class.clone())
                    .or_default()
                    .entry(r.trips)
                {
                    std::collections::btree_map::Entry::Vacant(v) => {
                        v.insert(r.count);
                    }
                    std::collections::btree_map::Entry::Occupied(o) => {
                        if *o.get() != r.count {
                            arithmetic_fail = Some(format!(
                                "{}: two records at trips {} disagree ({} vs {}) — no \
                                 deterministic model exists",
                                r.class,
                                r.trips,
                                o.get(),
                                r.count
                            ));
                            break;
                        }
                    }
                }
            }
        }
        if let Some(d) = arithmetic_fail {
            push(El0CheckId::OracleExactness, Status::Fail, d);
        } else {
            let mut fails = Vec::new();
            let mut oks = Vec::new();
            let mut unexercised = Vec::new();
            // Kernel-mediated classes: the exact integer fit.
            for (class, points) in &kernel_points {
                if points.len() < 2 {
                    unexercised.push(format!(
                        "{class}: only {} distinct trip magnitude(s) — the fit needs ≥ 2",
                        points.len()
                    ));
                    continue;
                }
                let (t1, c1) = points
                    .iter()
                    .next()
                    .map(|(t, c)| (*t, *c))
                    .unwrap_or((0, 0));
                let (t2, c2) = points
                    .iter()
                    .last()
                    .map(|(t, c)| (*t, *c))
                    .unwrap_or((0, 0));
                let (dt, dc) = (
                    i128::from(t2) - i128::from(t1),
                    i128::from(c2) - i128::from(c1),
                );
                if dt == 0 || dc % dt != 0 {
                    fails.push(format!(
                        "{class}: no integer slope fits ({dc} events over {dt} trips)"
                    ));
                    continue;
                }
                let a = dc / dt;
                let b = i128::from(c1) - a * i128::from(t1);
                if a < 1 {
                    fails.push(format!(
                        "{class}: fitted slope {a} < 1 — fewer events than back-edges is \
                         impossible for a real window"
                    ));
                    continue;
                }
                let bad: Vec<String> = points
                    .iter()
                    .filter(|(t, c)| a * i128::from(**t) + b != i128::from(**c))
                    .map(|(t, c)| format!("trips {t} → count {c}"))
                    .collect();
                if bad.is_empty() {
                    oks.push(format!(
                        "{class}: count = {a}·trips {b:+} exactly over {} magnitude(s)",
                        points.len()
                    ));
                } else {
                    fails.push(format!(
                        "{class}: count = {a}·trips {b:+} does not explain {}",
                        bad.join(", ")
                    ));
                }
            }
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
            if per_class.is_empty() && kernel_points.is_empty() {
                fails.push("no gradeable records".to_string());
            }
            if !fails.is_empty() {
                push(El0CheckId::OracleExactness, Status::Fail, fails.join("; "));
            } else if !unexercised.is_empty() {
                push(
                    El0CheckId::OracleExactness,
                    Status::NotRequested,
                    format!(
                        "{}{}",
                        if oks.is_empty() {
                            String::new()
                        } else {
                            format!("{}; UNEXERCISED: ", oks.join("; "))
                        },
                        unexercised.join("; ")
                    ),
                );
            } else {
                push(El0CheckId::OracleExactness, Status::Pass, oks.join("; "));
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

    // -- coverage matrix: every required class under every required condition. The stage's
    // claim is the full 5-class × 4-condition contamination grid; the checker must demand it,
    // not grade whatever classes happen to be present under whatever conditions were submitted.
    // Condition comes from each set's manifest, classes from its records.
    {
        let mut matrix: BTreeMap<String, std::collections::BTreeSet<El0Class>> = BTreeMap::new();
        for s in sets {
            let entry = matrix.entry(s.manifest.condition.clone()).or_default();
            for r in &s.records {
                if let Some(c) = class_of(&r.class) {
                    entry.insert(c);
                }
            }
        }
        let mut gaps: Vec<String> = Vec::new();
        for &cond in REQUIRED_EL0_CONDITIONS {
            match matrix.get(cond) {
                None => gaps.push(format!("condition '{cond}' not measured")),
                Some(classes) => {
                    let missing: Vec<&str> = REQUIRED_EL0_CLASSES
                        .iter()
                        .filter(|c| !classes.contains(c))
                        .map(|c| c.name())
                        .collect();
                    if !missing.is_empty() {
                        gaps.push(format!("condition '{cond}' missing class(es) {missing:?}"));
                    }
                }
            }
        }
        if gaps.is_empty() {
            push(
                El0CheckId::CoverageMatrix,
                Status::Pass,
                format!(
                    "the {}×{} class×condition matrix is complete: {:?} under {:?}",
                    REQUIRED_EL0_CLASSES.len(),
                    REQUIRED_EL0_CONDITIONS.len(),
                    REQUIRED_EL0_CLASSES
                        .iter()
                        .map(|c| c.name())
                        .collect::<Vec<_>>(),
                    REQUIRED_EL0_CONDITIONS
                ),
            );
        } else if floors.sub_normative {
            push(
                El0CheckId::CoverageMatrix,
                Status::Pass,
                format!(
                    "[SUB-NORMATIVE] class×condition matrix incomplete: {}",
                    gaps.join("; ")
                ),
            );
        } else {
            push(El0CheckId::CoverageMatrix, Status::Fail, gaps.join("; "));
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

#[cfg(test)]
mod tests {
    use super::*;
    use arm_harness::evidence::{Environment, PerfConfig, Pinning};

    const ALL5: &[&str] = &[
        "straight-line",
        "branch-dense",
        "el0-syscall",
        "el0-signal",
        "el0-pagefault",
    ];

    fn env() -> Environment {
        Environment {
            midr: 1,
            soc: "test".into(),
            firmware: BTreeMap::new(),
            host_kernel: "k".into(),
            kvm_mode: "vhe".into(),
        }
    }
    fn perf(exclude_hv: bool) -> PerfConfig {
        PerfConfig {
            raw_event: 0x21,
            exclude_host: false,
            exclude_guest: false,
            exclude_hv,
            pinned: true,
            sample_period: None,
        }
    }
    fn pinning(core: u32) -> Pinning {
        Pinning {
            pinned: true,
            core: Some(core),
            governor: "performance".into(),
            migration_probe: false,
        }
    }

    /// A well-formed run-set for `condition` carrying exactly `classes` (one small record
    /// each). Kept trivial (`trips = 100`) so the coverage/perf/aggregation checks under test
    /// are exercised without any O(trips) oracle work.
    fn loaded(
        cond: &str,
        tool: Option<&str>,
        perf: PerfConfig,
        pin: Pinning,
        classes: &[&str],
    ) -> Loaded {
        let records: Vec<El0Record> = classes
            .iter()
            .enumerate()
            .map(|(i, c)| El0Record {
                sample_id: i as u64,
                class: (*c).into(),
                scale: "1e6".into(),
                seed: 7,
                trips: 100,
                rep: 0,
                count: 100,
                accumulator: 100,
                time_enabled: 1,
                time_running: 1,
            })
            .collect();
        let mut bytes = Vec::new();
        for r in &records {
            bytes.extend_from_slice(serde_json::to_string(r).unwrap().as_bytes());
            bytes.push(b'\n');
        }
        let mut h = Sha256::new();
        h.update(&bytes);
        let manifest = El0Manifest {
            schema_version: EL0_SCHEMA_VERSION,
            stage: "aa1a".into(),
            run_set_id: format!("t-{cond}"),
            environment: env(),
            perf,
            exclude_kernel: true,
            exclude_user: false,
            pinning: pin,
            condition: cond.into(),
            attempted: records.len() as u64,
            records_sha256: hex_lower(&h.finalize()),
            tool_sha256: tool.map(String::from),
        };
        Loaded {
            manifest,
            records,
            records_bytes: bytes,
        }
    }

    fn status_of(rep: &El0Report, id: El0CheckId) -> Status {
        rep.outcomes.iter().find(|o| o.id == id).unwrap().status
    }

    fn full_matrix(perf_hv: bool, core: u32) -> Vec<Loaded> {
        REQUIRED_EL0_CONDITIONS
            .iter()
            .map(|c| loaded(c, Some("tool-hash"), perf(perf_hv), pinning(core), ALL5))
            .collect()
    }

    #[test]
    fn coverage_matrix_complete_passes() {
        let rep = grade(&full_matrix(true, 61), &El0Floors::default());
        assert_eq!(status_of(&rep, El0CheckId::CoverageMatrix), Status::Pass);
    }

    #[test]
    fn coverage_matrix_missing_class_fails() {
        let mut sets = full_matrix(true, 61);
        // Drop three classes from the first condition — a partially-measured condition.
        sets[0] = loaded(
            REQUIRED_EL0_CONDITIONS[0],
            Some("tool-hash"),
            perf(true),
            pinning(61),
            &["straight-line", "branch-dense"],
        );
        let rep = grade(&sets, &El0Floors::default());
        assert_eq!(status_of(&rep, El0CheckId::CoverageMatrix), Status::Fail);
    }

    #[test]
    fn coverage_matrix_missing_condition_fails() {
        // Only three of the four required conditions present.
        let sets: Vec<Loaded> = REQUIRED_EL0_CONDITIONS[..3]
            .iter()
            .map(|c| loaded(c, Some("tool-hash"), perf(true), pinning(61), ALL5))
            .collect();
        let rep = grade(&sets, &El0Floors::default());
        assert_eq!(status_of(&rep, El0CheckId::CoverageMatrix), Status::Fail);
    }

    #[test]
    fn coverage_matrix_incomplete_passes_sub_normative() {
        let sets: Vec<Loaded> = REQUIRED_EL0_CONDITIONS[..1]
            .iter()
            .map(|c| loaded(c, Some("tool-hash"), perf(true), pinning(61), ALL5))
            .collect();
        let floors = El0Floors {
            sub_normative: true,
            ..El0Floors::default()
        };
        let rep = grade(&sets, &floors);
        assert_eq!(status_of(&rep, El0CheckId::CoverageMatrix), Status::Pass);
    }

    #[test]
    fn exclude_hv_false_fails_perf_config() {
        let rep = grade(&full_matrix(false, 61), &El0Floors::default());
        assert_eq!(status_of(&rep, El0CheckId::PerfConfig), Status::Fail);
    }

    #[test]
    fn differing_pinning_fails_aggregation() {
        let sets = vec![
            loaded(
                "pinned-solo",
                Some("tool-hash"),
                perf(true),
                pinning(61),
                ALL5,
            ),
            loaded(
                "co-tenant-other-core",
                Some("tool-hash"),
                perf(true),
                pinning(62),
                ALL5,
            ),
        ];
        let rep = grade(&sets, &El0Floors::default());
        assert_eq!(status_of(&rep, El0CheckId::Aggregation), Status::Fail);
    }
}
