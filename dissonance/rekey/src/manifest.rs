// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **corpus manifest** — the harness's only door onto the evaluation set.
//!
//! `campaign-data/rekey-corpus.json` names every trace file the harness may
//! read, pinned by sha256, together with the exclusions and the reason for each.
//! Loading goes through [`Corpus::load`], which re-hashes every archive, every
//! archive member, and every campaign log before a single byte is parsed, and
//! fails loudly on any mismatch. This is the **hm-xdp lesson** applied to the
//! re-key substrate: reference artifacts by content, never by mutable path.
//!
//! ## What is in the corpus, and what is not
//!
//! - **`bug3-campaign`** — the 40 GO/NO-GO #2 trace sets (20 seeds × baseline /
//!   signal). The 3 `-solo` determinism re-runs are **excluded**: they are
//!   replicas of seeds 1–3, and double-counting a seed biases every axis.
//! - **`bug3-ablation`** — the 20 `explore_period = 1` trace sets. A separate
//!   slice: by construction it is baseline's trajectory with sensor observations
//!   attached, i.e. the only slice showing what the sensor sees on an
//!   **unsteered** search. Its 2 `-solo` re-runs are excluded likewise.
//! - **`bug1-reference`** — recorded campaign logs **only**. Bug 1's campaign
//!   ran before the trace-retention amendment, so no `RunTrace`s exist for it
//!   and it **cannot be re-keyed at all** (`docs/SCORING.md` R1: the traces are
//!   the substrate). Its recorded per-campaign cell counts appear in the report
//!   as a reference row and nothing more. Filed as bead `hm-5sv`; the
//!   trigger-orthogonal twin candidate replaces it as the noise-fitting control
//!   (tasks/97 amendment).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};
use crate::gz;

/// The manifest schema version.
pub const MANIFEST_VERSION: u32 = 1;

/// The manifest's file name, under the corpus root.
pub const MANIFEST_FILE: &str = "rekey-corpus.json";

/// The `bug3` campaign slice id.
pub const BUG3_CAMPAIGN: &str = "bug3-campaign";
/// The `bug3` `explore_period = 1` ablation slice id.
pub const BUG3_ABLATION: &str = "bug3-ablation";
/// The bug-1 recorded-log-only reference slice id (no traces; not re-keyable).
pub const BUG1_REFERENCE: &str = "bug1-reference";

/// One re-keyable trace file: the archive member, its content hash, and the
/// campaign log that records what the campaign actually discovered.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TraceEntry {
    /// The member name inside the slice's archive.
    pub member: String,
    /// sha256 of the member's bytes.
    pub sha256: String,
    /// The campaign log, relative to the corpus root.
    pub log: String,
    /// sha256 of the campaign log's bytes.
    pub log_sha256: String,
    /// `Baseline` or `Signal`.
    pub config: String,
    /// The campaign seed.
    pub seed: u64,
    /// How many `(branch, RunTrace)` pairs the member holds.
    pub branches: u64,
}

/// A re-keyable slice: one archive, its members, and the search knobs the
/// campaigns ran under.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CorpusSlice {
    /// The slice id ([`BUG3_CAMPAIGN`] / [`BUG3_ABLATION`]).
    pub slice: String,
    /// The bug the campaigns targeted.
    pub bug: u16,
    /// One line on what this slice is and why it is in the evaluation set.
    pub description: String,
    /// The `.tar.gz` holding the traces, relative to the corpus root.
    pub archive: String,
    /// sha256 of the archive's bytes.
    pub archive_sha256: String,
    /// The `explore_period` every campaign in the slice ran under.
    pub explore_period: u64,
    /// The included traces, in `(config, seed)` order.
    pub traces: Vec<TraceEntry>,
}

/// A recorded campaign log with no retained trace — present for reference, and
/// explicitly **not** part of the re-key evaluation set.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReferenceLog {
    /// The campaign log, relative to the corpus root.
    pub log: String,
    /// sha256 of the campaign log's bytes.
    pub log_sha256: String,
    /// `Baseline` or `Signal`.
    pub config: String,
    /// The campaign seed.
    pub seed: u64,
}

/// A slice the harness can read but cannot re-key.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReferenceSlice {
    /// The slice id ([`BUG1_REFERENCE`]).
    pub slice: String,
    /// The bug the campaigns targeted.
    pub bug: u16,
    /// Why it cannot be re-keyed.
    pub reason: String,
    /// The recorded logs, in `(config, seed)` order.
    pub logs: Vec<ReferenceLog>,
}

/// A trace file deliberately kept out of the evaluation set.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Exclusion {
    /// The slice whose archive holds it.
    pub slice: String,
    /// The excluded member.
    pub member: String,
    /// sha256 of the member's bytes — pinned so the exclusion names a *known*
    /// artifact, not merely an absent one.
    pub sha256: String,
    /// Why it is excluded.
    pub reason: String,
}

/// Corpus-wide counts, restated so the report never has to recount them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Totals {
    /// Re-keyable trace files across all slices.
    pub trace_files: u64,
    /// Branches (recorded `RunTrace`s) across all slices.
    pub branches: u64,
    /// Trace files excluded (the `-solo` determinism re-runs).
    pub excluded_traces: u64,
    /// Recorded logs present for reference but not re-keyable.
    pub reference_logs: u64,
}

/// The corpus manifest.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CorpusManifest {
    /// Schema version.
    pub version: u32,
    /// What this file is, for a reader who found it without the report.
    pub note: String,
    /// The re-keyable slices.
    pub slices: Vec<CorpusSlice>,
    /// Slices present only as recorded logs.
    pub references: Vec<ReferenceSlice>,
    /// Deliberate exclusions, each with its reason.
    pub exclusions: Vec<Exclusion>,
    /// Corpus-wide counts.
    pub totals: Totals,
}

/// The seeds every GO/NO-GO #2 campaign ran (Klees-style, ≥ 20 per config).
const SEEDS: std::ops::RangeInclusive<u64> = 1..=20;

/// Lowercase-hex sha256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Read a file, tagging any I/O failure with its path.
fn read(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Resolve a manifest-supplied relative path against the corpus root, refusing
/// anything that leaves it.
///
/// `Path::join` is not a containment primitive: it **discards the root entirely**
/// for an absolute path (`root.join("/etc/passwd")` is `/etc/passwd`) and happily
/// walks `..` upwards. And content-addressing cannot save us here — the manifest
/// supplies both the path *and* the sha256 it is checked against, so a hostile
/// manifest simply pins the hash of whatever it points at.
///
/// Two lines of defence: reject any component that is absolute, a prefix, a root,
/// or `..` before touching the filesystem; then canonicalize and require the
/// result to stay beneath the canonicalized root, which also closes the symlink
/// path (a `traces.tar.gz` that is a link to `~/.ssh/id_rsa`).
fn resolve(root: &Path, rel: &str) -> Result<PathBuf> {
    let escape = || Error::PathEscape {
        path: rel.to_string(),
        root: root.to_path_buf(),
    };
    let candidate = Path::new(rel);
    let lexically_contained = candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if !lexically_contained {
        return Err(escape());
    }

    let root = std::fs::canonicalize(root).map_err(|source| Error::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let joined = root.join(candidate);
    let resolved = std::fs::canonicalize(&joined).map_err(|source| Error::Io {
        path: joined,
        source,
    })?;
    if !resolved.starts_with(&root) {
        return Err(escape());
    }
    Ok(resolved)
}

/// How many `(branch, RunTrace)` pairs a trace member holds, without decoding
/// the traces themselves.
fn branch_count(member: &str, bytes: &[u8]) -> Result<u64> {
    let pairs: Vec<(u64, serde::de::IgnoredAny)> =
        serde_json::from_slice(bytes).map_err(|source| Error::Json {
            what: member.to_string(),
            source,
        })?;
    Ok(pairs.len() as u64)
}

/// The layout of one re-keyable slice: how its member and log names are formed.
/// Names are **generated**, never read from a directory listing — no filesystem
/// iteration order can reach the manifest (conventions rule 4).
struct SliceLayout {
    slice: &'static str,
    bug: u16,
    description: &'static str,
    archive: &'static str,
    log_dir: &'static str,
    explore_period: u64,
    /// `(config, stem)` pairs: the campaign configuration and the file stem
    /// prefix its runs use.
    configs: &'static [(&'static str, &'static str)],
    /// The excluded `-solo` stems, with the seeds they replicate.
    solos: &'static [(&'static str, u64)],
}

/// The two re-keyable slices, in report order.
const LAYOUTS: [SliceLayout; 2] = [
    SliceLayout {
        slice: BUG3_CAMPAIGN,
        bug: 3,
        description: "GO/NO-GO #2 bug-3 campaign: 20 seeds x {baseline, signal}, \
                      explore_period = 4, 512 branches each",
        archive: "bug3/results/traces.tar.gz",
        log_dir: "bug3/results",
        explore_period: 4,
        configs: &[("Baseline", "b3-baseline"), ("Signal", "b3-signal")],
        solos: &[
            ("b3-baseline-1-solo", 1),
            ("b3-baseline-2-solo", 2),
            ("b3-baseline-3-solo", 3),
        ],
    },
    SliceLayout {
        slice: BUG3_ABLATION,
        bug: 3,
        description: "The Paul-authorized explore/exploit ablation: signal config at \
                      explore_period = 1 (never exploits), 20 seeds. The only slice \
                      showing what the sensor sees on an UNSTEERED search",
        archive: "bug3/ablation/results/traces.tar.gz",
        log_dir: "bug3/ablation/results",
        explore_period: 1,
        configs: &[("Signal", "b3-signal-ep1")],
        solos: &[("b3-signal-ep1-1-solo", 1), ("b3-signal-ep1-2-solo", 2)],
    },
];

/// Build the manifest by hashing the corpus at `root` (the `campaign-data`
/// directory). Every name is generated from [`LAYOUTS`], so the manifest is a
/// pure function of the corpus bytes.
pub fn build(root: &Path) -> Result<CorpusManifest> {
    let mut slices = Vec::new();
    let mut exclusions = Vec::new();
    let mut totals = Totals::default();

    for layout in &LAYOUTS {
        let archive_bytes = read(&resolve(root, layout.archive)?)?;
        let members = archive_members(layout.archive, &archive_bytes)?;

        let mut traces = Vec::new();
        for (config, stem) in layout.configs {
            for seed in SEEDS {
                let member = format!("./{stem}-{seed}.traces.json");
                let data = members.get(&member).ok_or_else(|| Error::MissingMember {
                    archive: layout.archive.to_string(),
                    member: member.clone(),
                })?;
                let log = format!("{}/{stem}-{seed}.json", layout.log_dir);
                let log_bytes = read(&resolve(root, &log)?)?;
                totals.branches += branch_count(&member, data)?;
                traces.push(TraceEntry {
                    branches: branch_count(&member, data)?,
                    sha256: sha256_hex(data),
                    log_sha256: sha256_hex(&log_bytes),
                    member,
                    log,
                    config: (*config).to_string(),
                    seed,
                });
            }
        }
        totals.trace_files += traces.len() as u64;

        for (stem, replicates) in layout.solos {
            let member = format!("./{stem}.traces.json");
            let data = members.get(&member).ok_or_else(|| Error::MissingMember {
                archive: layout.archive.to_string(),
                member: member.clone(),
            })?;
            exclusions.push(Exclusion {
                slice: layout.slice.to_string(),
                sha256: sha256_hex(data),
                member,
                reason: format!(
                    "solo determinism re-run: a replica of seed {replicates}, not an additional \
                     seed. Double-counting it would bias every axis"
                ),
            });
            totals.excluded_traces += 1;
        }

        slices.push(CorpusSlice {
            slice: layout.slice.to_string(),
            bug: layout.bug,
            description: layout.description.to_string(),
            archive: layout.archive.to_string(),
            archive_sha256: sha256_hex(&archive_bytes),
            explore_period: layout.explore_period,
            traces,
        });
    }

    // Bug 1: logs only. Its campaign predates `--record`, so no traces exist.
    let mut logs = Vec::new();
    for (config, stem) in [("Baseline", "b1-baseline"), ("Signal", "b1-signal")] {
        for seed in SEEDS {
            let log = format!("bug1/results/{stem}-{seed}.json");
            let bytes = read(&resolve(root, &log)?)?;
            logs.push(ReferenceLog {
                log_sha256: sha256_hex(&bytes),
                log,
                config: config.to_string(),
                seed,
            });
        }
    }
    totals.reference_logs = logs.len() as u64;

    Ok(CorpusManifest {
        version: MANIFEST_VERSION,
        note: "The tasks/97 E-fails re-key evaluation set. The harness loads ONLY through this \
               manifest and fails loudly on a hash mismatch. Regenerate with `rekey manifest`."
            .to_string(),
        slices,
        references: vec![ReferenceSlice {
            slice: BUG1_REFERENCE.to_string(),
            bug: 1,
            reason: "Bug 1's campaign ran before the trace-retention amendment, so no RunTraces \
                     were retained and it CANNOT be re-keyed (docs/SCORING.md R1: retained traces \
                     are the substrate). Its recorded per-campaign cell counts appear in \
                     REKEY-REPORT.md as a reference row only. Bead hm-5sv."
                .to_string(),
            logs,
        }],
        exclusions,
        totals,
    })
}

/// Decompress an archive and index its members by name.
fn archive_members(archive: &str, bytes: &[u8]) -> Result<BTreeMap<String, Vec<u8>>> {
    let tar = gz::gunzip(archive, bytes)?;
    Ok(gz::untar(archive, &tar)?
        .into_iter()
        .map(|e| (e.name, e.data))
        .collect())
}

/// A campaign's raw bytes, verified against the manifest and ready to parse.
pub struct VerifiedCampaign {
    /// The slice this campaign belongs to.
    pub slice: String,
    /// The archive member the traces came from — named in identity errors.
    pub member: String,
    /// `Baseline` or `Signal`.
    pub config: String,
    /// The campaign seed.
    pub seed: u64,
    /// The `explore_period` the slice ran under.
    pub explore_period: u64,
    /// The `Vec<(branch, RunTrace)>` JSON.
    pub traces_json: Vec<u8>,
    /// The `CampaignLog` JSON.
    pub log_json: Vec<u8>,
}

/// The corpus, loaded through its manifest with every hash checked.
pub struct Corpus {
    /// The manifest as committed.
    pub manifest: CorpusManifest,
    /// sha256 of the manifest file's bytes — the report's corpus fingerprint.
    pub manifest_sha256: String,
    /// The verified campaigns, in slice then `(config, seed)` order.
    pub campaigns: Vec<VerifiedCampaign>,
    /// The bug-1 reference logs, verified and in `(config, seed)` order.
    pub reference_logs: Vec<(ReferenceLog, Vec<u8>)>,
}

impl Corpus {
    /// Load and verify the whole corpus under `root`, reading `rekey-corpus.json`
    /// from it. Any archive, member, or log whose sha256 differs from the pin is
    /// an [`Error::HashMismatch`] — the corpus drifted, and no score computed
    /// against it would mean anything.
    pub fn load(root: &Path) -> Result<Corpus> {
        let manifest_path = root.join(MANIFEST_FILE);
        let manifest_bytes = read(&manifest_path)?;
        let manifest: CorpusManifest =
            serde_json::from_slice(&manifest_bytes).map_err(|source| Error::Json {
                what: manifest_path.display().to_string(),
                source,
            })?;
        // A future (or zeroed) schema must not be read with v1 semantics: the
        // field a later version adds is exactly the one this build would ignore.
        if manifest.version != MANIFEST_VERSION {
            return Err(Error::ManifestVersion {
                found: manifest.version,
                expected: MANIFEST_VERSION,
            });
        }

        validate_exclusions(&manifest)?;
        validate_trace_uniqueness(&manifest)?;

        let mut campaigns = Vec::new();
        // Recompute every count from the bytes rather than trusting the manifest's
        // declared figures: the hash checks pin content, but scoring reads the
        // live JSON, so a stale `branches`/`totals` could report a full corpus as
        // a verified empty one (or the reverse). Any disagreement is a loud error.
        let mut recomputed = Totals::default();
        for slice in &manifest.slices {
            let archive_bytes = read(&resolve(root, &slice.archive)?)?;
            check_hash(&slice.archive, &slice.archive_sha256, &archive_bytes)?;
            let members = archive_members(&slice.archive, &archive_bytes)?;

            // The exclusions are hash-checked too: an excluded artifact must be
            // the one we *chose* to exclude, present and unchanged.
            for ex in manifest
                .exclusions
                .iter()
                .filter(|e| e.slice == slice.slice)
            {
                let data = members
                    .get(&ex.member)
                    .ok_or_else(|| Error::MissingMember {
                        archive: slice.archive.clone(),
                        member: ex.member.clone(),
                    })?;
                check_hash(&ex.member, &ex.sha256, data)?;
            }

            for t in &slice.traces {
                let data = members.get(&t.member).ok_or_else(|| Error::MissingMember {
                    archive: slice.archive.clone(),
                    member: t.member.clone(),
                })?;
                check_hash(&t.member, &t.sha256, data)?;
                // The declared branch count must equal the trace's actual pairs.
                let actual = branch_count(&t.member, data)?;
                check_count(&format!("{} branches", t.member), t.branches, actual)?;
                recomputed.branches += actual;
                recomputed.trace_files += 1;
                let log_json = read(&resolve(root, &t.log)?)?;
                check_hash(&t.log, &t.log_sha256, &log_json)?;
                campaigns.push(VerifiedCampaign {
                    slice: slice.slice.clone(),
                    member: t.member.clone(),
                    config: t.config.clone(),
                    seed: t.seed,
                    explore_period: slice.explore_period,
                    traces_json: data.clone(),
                    log_json,
                });
            }
        }

        let mut reference_logs = Vec::new();
        for r in &manifest.references {
            for l in &r.logs {
                let bytes = read(&resolve(root, &l.log)?)?;
                check_hash(&l.log, &l.log_sha256, &bytes)?;
                reference_logs.push((l.clone(), bytes));
            }
        }
        recomputed.excluded_traces = manifest.exclusions.len() as u64;
        recomputed.reference_logs = reference_logs.len() as u64;

        // The aggregate totals the report prints must be the ones the corpus
        // holds — a declared total is never echoed unchecked.
        for (field, declared, actual) in [
            (
                "trace_files",
                manifest.totals.trace_files,
                recomputed.trace_files,
            ),
            ("branches", manifest.totals.branches, recomputed.branches),
            (
                "excluded_traces",
                manifest.totals.excluded_traces,
                recomputed.excluded_traces,
            ),
            (
                "reference_logs",
                manifest.totals.reference_logs,
                recomputed.reference_logs,
            ),
        ] {
            check_count(&format!("totals.{field}"), declared, actual)?;
        }

        Ok(Corpus {
            manifest_sha256: sha256_hex(&manifest_bytes),
            manifest,
            campaigns,
            reference_logs,
        })
    }
}

/// Every exclusion must name **exactly one** loaded slice.
///
/// Exclusions are hash-checked inside [`Corpus::load`]'s per-slice loop via
/// `.filter(|e| e.slice == slice.slice)`, so an exclusion naming a misspelled or
/// stale slice would be visited by no iteration — never hash-checked, yet `load`
/// would succeed and the report would present it as verified. That silently
/// breaks the crate's core guarantee (every excluded artifact present and
/// hash-checked), so a slice with no — or more than one — match is a loud
/// failure, checked before any file is read.
fn validate_exclusions(manifest: &CorpusManifest) -> Result<()> {
    for ex in &manifest.exclusions {
        if manifest
            .slices
            .iter()
            .filter(|s| s.slice == ex.slice)
            .count()
            != 1
        {
            return Err(Error::UnknownExclusionSlice {
                slice: ex.slice.clone(),
                member: ex.member.clone(),
            });
        }
    }
    Ok(())
}

/// Every re-keyable trace must have a unique `(slice, config, seed)` identity.
///
/// A repeated entry would load twice and pass every hash and ancestry check, yet
/// double-weight that campaign in breadth, granularity, and the debut audit — a
/// silent scoring bias exactly like counting a `-solo` re-run would be. Checked
/// before any campaign is scored, per slice (the same `(config, seed)` may
/// legitimately appear in a *different* slice — e.g. seed 1 signal in both the
/// campaign and the ablation).
fn validate_trace_uniqueness(manifest: &CorpusManifest) -> Result<()> {
    for slice in &manifest.slices {
        let mut seen: BTreeSet<(&str, u64)> = BTreeSet::new();
        for t in &slice.traces {
            if !seen.insert((t.config.as_str(), t.seed)) {
                return Err(Error::DuplicateTrace {
                    slice: slice.slice.clone(),
                    config: t.config.clone(),
                    seed: t.seed,
                });
            }
        }
    }
    Ok(())
}

/// A declared count must equal the one recomputed from the bytes. The hash
/// checks pin content; this pins the *counts* scoring and the report depend on,
/// so a stale declared figure can never be echoed as verified.
fn check_count(what: &str, declared: u64, actual: u64) -> Result<()> {
    if declared == actual {
        Ok(())
    } else {
        Err(Error::CountMismatch {
            what: what.to_string(),
            declared,
            actual,
        })
    }
}

/// The loud hash check. Never a warning, never a repair.
fn check_hash(what: &str, expected: &str, bytes: &[u8]) -> Result<()> {
    let found = sha256_hex(bytes);
    if found == expected {
        Ok(())
    } else {
        Err(Error::HashMismatch {
            what: what.to_string(),
            expected: expected.to_string(),
            found,
        })
    }
}

/// Render the manifest as the committed pretty JSON (trailing newline), so
/// `rekey manifest` is byte-stable across runs and hosts.
pub fn render(manifest: &CorpusManifest) -> String {
    let mut s = serde_json::to_string_pretty(manifest)
        // Statically infallible: every field is a string, integer, or Vec of them.
        .expect("the manifest contains no non-serializable value");
    s.push('\n');
    s
}

/// The manifest path under a corpus root.
pub fn manifest_path(root: &Path) -> PathBuf {
    root.join(MANIFEST_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **A manifest path may never leave the corpus root.** `Path::join` is not a
    /// containment primitive: it discards the root for an absolute path and walks
    /// `..` upwards. And the manifest supplies the expected hash as well as the
    /// path, so content-addressing cannot enforce containment either.
    #[test]
    fn manifest_paths_cannot_escape_the_corpus_root() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub")).expect("mkdir");
        std::fs::write(root.join("sub/inside.txt"), b"ok").expect("write");
        std::fs::write(root.join("sibling.txt"), b"ok").expect("write");

        // Contained paths resolve, with or without a `./` prefix.
        assert!(resolve(root, "sub/inside.txt").is_ok());
        assert!(resolve(root, "./sub/inside.txt").is_ok());

        // Absolute: `join` would silently return the absolute path itself.
        let absolute = root.join("sibling.txt").display().to_string();
        assert!(matches!(
            resolve(root, &absolute),
            Err(Error::PathEscape { .. })
        ));
        assert!(matches!(
            resolve(root, "/etc/passwd"),
            Err(Error::PathEscape { .. })
        ));

        // Upward traversal, both bare and disguised by a descent first.
        for escape in ["../outside.txt", "sub/../../outside.txt", ".."] {
            assert!(
                matches!(resolve(root, escape), Err(Error::PathEscape { .. })),
                "{escape} must be refused"
            );
        }

        // A path that stays inside lexically but resolves out through a symlink
        // is refused too — canonicalization is the second line of defence.
        #[cfg(unix)]
        {
            let outside = dir.path().parent().expect("parent").join("escape-target");
            std::fs::write(&outside, b"secret").expect("write");
            std::os::unix::fs::symlink(&outside, root.join("link.txt")).expect("symlink");
            assert!(matches!(
                resolve(root, "link.txt"),
                Err(Error::PathEscape { .. })
            ));
            std::fs::remove_file(&outside).ok();
        }

        // A contained path that does not exist is an I/O error, not an escape.
        assert!(matches!(resolve(root, "sub/absent"), Err(Error::Io { .. })));
    }

    /// An exclusion must name **exactly one** loaded slice. Its member is
    /// hash-checked only inside `Corpus::load`'s per-slice loop, so an exclusion
    /// naming a slice that is missing (misspelled) — or duplicated — would never
    /// be visited, and the harness would call an unverified artifact verified.
    #[test]
    fn an_exclusion_naming_an_unknown_slice_is_refused() {
        let slice = CorpusSlice {
            slice: "bug3-campaign".into(),
            bug: 3,
            description: String::new(),
            archive: "a.tar.gz".into(),
            archive_sha256: String::new(),
            explore_period: 4,
            traces: Vec::new(),
        };
        let exclusion = |s: &str| Exclusion {
            slice: s.into(),
            member: "solo.json".into(),
            sha256: String::new(),
            reason: String::new(),
        };
        let manifest = |exclusions| CorpusManifest {
            version: MANIFEST_VERSION,
            note: String::new(),
            slices: vec![slice.clone()],
            references: Vec::new(),
            exclusions,
            totals: Totals::default(),
        };

        // Names the loaded slice exactly once — accepted.
        assert!(validate_exclusions(&manifest(vec![exclusion("bug3-campaign")])).is_ok());
        // No exclusions at all — trivially accepted.
        assert!(validate_exclusions(&manifest(Vec::new())).is_ok());

        // A misspelled / unknown slice: refused, and it names the offending
        // member so the failure points at what went unchecked.
        match validate_exclusions(&manifest(vec![exclusion("bug3-campgin")])) {
            Err(Error::UnknownExclusionSlice { slice, member }) => {
                assert_eq!(slice, "bug3-campgin");
                assert_eq!(member, "solo.json");
            }
            other => panic!("expected UnknownExclusionSlice, got {other:?}"),
        }

        // Two slices sharing the id → "exactly one" fails on the *high* side too,
        // not only the missing side.
        let mut dup = manifest(vec![exclusion("bug3-campaign")]);
        dup.slices.push(slice.clone());
        assert!(matches!(
            validate_exclusions(&dup),
            Err(Error::UnknownExclusionSlice { .. })
        ));
    }

    /// A declared count is verified against the recomputed one: equal passes,
    /// any disagreement is a loud `CountMismatch` that names what drifted. This
    /// is the comparison behind both the per-trace branch check and the totals
    /// check, so scoring can never run against a corpus the manifest miscounts.
    #[test]
    fn check_count_refuses_a_declared_count_that_drifted() {
        assert!(check_count("branches", 512, 512).is_ok(), "equal passes");
        assert!(check_count("branches", 0, 0).is_ok());
        match check_count("some.member branches", 512, 0) {
            Err(Error::CountMismatch {
                what,
                declared,
                actual,
            }) => {
                assert_eq!(what, "some.member branches");
                assert_eq!(declared, 512, "a 512-branch trace...");
                assert_eq!(actual, 0, "...must never verify as an empty corpus");
            }
            other => panic!("expected CountMismatch, got {other:?}"),
        }
        // Drift in either direction is caught, not just under-counting.
        assert!(matches!(
            check_count("totals.branches", 0, 512),
            Err(Error::CountMismatch { .. })
        ));
    }

    /// A repeated `(slice, config, seed)` trace is refused before scoring: it
    /// would load twice, pass every hash check, and double-weight the campaign in
    /// every axis. A `(config, seed)` shared across *different* slices is fine.
    #[test]
    fn a_duplicate_campaign_identity_is_refused() {
        let trace = |config: &str, seed: u64| TraceEntry {
            member: format!("{config}-{seed}.json"),
            sha256: String::new(),
            log: format!("{config}-{seed}.log"),
            log_sha256: String::new(),
            config: config.into(),
            seed,
            branches: 512,
        };
        let slice = |id: &str, traces: Vec<TraceEntry>| CorpusSlice {
            slice: id.into(),
            bug: 3,
            description: String::new(),
            archive: format!("{id}.tar.gz"),
            archive_sha256: String::new(),
            explore_period: 4,
            traces,
        };
        let manifest = |slices| CorpusManifest {
            version: MANIFEST_VERSION,
            note: String::new(),
            slices,
            references: Vec::new(),
            exclusions: Vec::new(),
            totals: Totals::default(),
        };

        // Distinct identities within a slice, and the same (config, seed) across
        // two different slices — both legal.
        let ok = manifest(vec![
            slice(
                "bug3-campaign",
                vec![trace("Signal", 1), trace("Baseline", 1), trace("Signal", 2)],
            ),
            slice("bug3-ablation", vec![trace("Signal", 1)]),
        ]);
        assert!(validate_trace_uniqueness(&ok).is_ok());

        // A repeat *within* a slice is refused, naming the collision.
        let dup = manifest(vec![slice(
            "bug3-campaign",
            vec![trace("Signal", 1), trace("Signal", 1)],
        )]);
        match validate_trace_uniqueness(&dup) {
            Err(Error::DuplicateTrace {
                slice,
                config,
                seed,
            }) => {
                assert_eq!(slice, "bug3-campaign");
                assert_eq!(config, "Signal");
                assert_eq!(seed, 1);
            }
            other => panic!("expected DuplicateTrace, got {other:?}"),
        }
    }

    /// A manifest from another schema version is refused, not reinterpreted.
    #[test]
    fn a_foreign_manifest_version_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut m = CorpusManifest {
            version: MANIFEST_VERSION + 1,
            note: "n".into(),
            slices: Vec::new(),
            references: Vec::new(),
            exclusions: Vec::new(),
            totals: Totals::default(),
        };
        std::fs::write(manifest_path(dir.path()), render(&m)).expect("write");
        assert!(matches!(
            Corpus::load(dir.path()),
            Err(Error::ManifestVersion { .. })
        ));

        // The current version loads (an empty corpus is legal, if useless).
        m.version = MANIFEST_VERSION;
        std::fs::write(manifest_path(dir.path()), render(&m)).expect("write");
        assert!(Corpus::load(dir.path()).is_ok());
    }

    #[test]
    fn sha256_matches_the_canonical_empty_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hash_mismatch_is_an_error_not_a_warning() {
        let err = check_hash("x", &sha256_hex(b"a"), b"b").expect_err("must reject");
        assert!(matches!(err, Error::HashMismatch { .. }));
        assert!(check_hash("x", &sha256_hex(b"a"), b"a").is_ok());
    }

    /// The layout tables generate exactly the corpus the spec pins: 40 + 20
    /// included traces, 3 + 2 excluded solos, all names deterministic.
    #[test]
    fn layouts_generate_the_specified_corpus_shape() {
        let included: usize = LAYOUTS
            .iter()
            .map(|l| l.configs.len() * SEEDS.count())
            .sum();
        let excluded: usize = LAYOUTS.iter().map(|l| l.solos.len()).sum();
        assert_eq!(included, 60, "40 campaign + 20 ablation trace sets");
        assert_eq!(excluded, 5, "3 campaign + 2 ablation solo re-runs");
    }

    #[test]
    fn branch_count_reads_the_pair_array_without_decoding_traces() {
        let json = br#"[[0,{"anything":1}],[1,{"x":[2,3]}]]"#;
        assert_eq!(branch_count("m", json).expect("count"), 2);
        assert!(branch_count("m", b"{}").is_err());
    }

    /// The rendered manifest is stable: same value, same bytes, ending in one
    /// newline (so the committed file is diff-clean).
    #[test]
    fn render_is_byte_stable_and_newline_terminated() {
        let m = CorpusManifest {
            version: MANIFEST_VERSION,
            note: "n".into(),
            slices: Vec::new(),
            references: Vec::new(),
            exclusions: Vec::new(),
            totals: Totals::default(),
        };
        let a = render(&m);
        assert_eq!(a, render(&m));
        assert!(a.ends_with("}\n"));
        let back: CorpusManifest = serde_json::from_str(&a).expect("round-trip");
        assert_eq!(back, m);
    }
}
