// SPDX-License-Identifier: AGPL-3.0-or-later
//! # `rekey` — the E-fails re-key harness (task 97)
//!
//! GO/NO-GO #2 closed **NO-GO**: the log-template signal configuration does not
//! beat a blind baseline on the sole real discriminator. `docs/SCORING.md`'s
//! **E-fails playbook** answers a gate FAIL with a procedure rather than a
//! judgment call — freeze the campaign, re-key candidate `CellFn` configs
//! offline over its retained traces, score them on three axes, hand a human a
//! ranked ratification menu. This crate is playbook steps 2–4.
//!
//! It re-keys **exactly**: a campaign's archive is a pure fold over `(trace,
//! forks, cells, sensors)`, so replaying the retained `RunTrace`s through a
//! different `CellFn` recomputes what that campaign's archive *would have been* —
//! no guest re-execution, no re-run, no approximation (`docs/SCORING.md` R1,
//! law 2: AURORA's container rebuild and Go-Explore's archive conversion, in
//! harmony's strong form). Everything downstream is integer/ordered arithmetic:
//! the same candidate over the same manifest yields a byte-identical
//! `REKEY-REPORT.md` on every run and every host.
//!
//! ## The shape of the harness
//!
//! | module | job |
//! |---|---|
//! | [`manifest`] | the corpus manifest: every trace pinned by sha256, every exclusion named. The only door onto the evaluation set |
//! | [`gz`] | dependency-free gzip + ustar, so the committed `.tar.gz` corpus reads in-process |
//! | [`observe`] | replay the sensor fold once per campaign — the stream every candidate keys |
//! | [`candidate`] | the candidate space: R2's knob-sets, plus IJON's chosen sparse state channel and its trigger-blind twin control |
//! | [`replay`] | reconstruct the recorded campaign's ancestry (checked against every recorded environment), and the control-fold gate |
//! | [`score`] | the three axes, and the diagnostics that keep them honest |
//! | [`fixed`] | Q32.32 arithmetic, because `f64::ln` is not portable to the last bit |
//! | [`report`] | `REKEY-REPORT.md` |
//!
//! ## What it does not do
//!
//! It never promotes a candidate. R2 is explicit — *fixed cell parameters beat
//! the adaptive tuner on Go-Explore's own headline domain, twice*; auto-tuning
//! only ever proposes, and a human ratifies. [`analyze`] produces a ranked menu
//! and stops.

pub mod candidate;
pub mod error;
pub mod fixed;
pub mod gz;
pub mod manifest;
pub mod observe;
pub mod replay;
pub mod report;
pub mod score;

use std::collections::BTreeMap;
use std::path::Path;

use benchmark::Benchmark;
use benchmark::report::CampaignLog;

pub use error::{Error, Result};

use crate::candidate::Candidate;
use crate::manifest::{Corpus, Totals};
use crate::observe::CampaignObs;
use crate::replay::Chains;
use crate::score::{AncestryStats, Constants, DebutAudit, ExploitLocality, SliceScore};

/// The slice the ranking is decided on: bug 3 is the only real discriminator
/// (bug 1 is degenerate — it fires on any canary bit-flip — and bug 2 was
/// deferred as structurally uncalibratable).
pub const PRIMARY_SLICE: &str = manifest::BUG3_CAMPAIGN;

/// One slice's re-key results.
pub struct SliceAnalysis {
    /// The slice id.
    pub id: String,
    /// The bug its campaigns targeted.
    pub bug: u16,
    /// What the slice is.
    pub description: String,
    /// The `explore_period` its campaigns ran under.
    pub explore_period: u64,
    /// How many campaigns it holds.
    pub campaigns: u64,
    /// One row per candidate, in candidate order.
    pub scores: Vec<SliceScore>,
    /// Where this slice's template species debut.
    pub debut: DebutAudit,
    /// How local the exploit kernel's seed twiddle actually is on this slice.
    pub locality: ExploitLocality,
    /// The measured parent-branch distribution — grounds axis (c)'s vacuity
    /// claim in the data rather than asserting it.
    pub ancestry: AncestryStats,
}

/// A bug-1 reference row: recorded, never re-keyed.
pub struct ReferenceRow {
    /// `Baseline` or `Signal`.
    pub config: String,
    /// The campaign seed.
    pub seed: u64,
    /// Distinct cell ids the campaign recorded.
    pub distinct_cells: u64,
    /// The branch its find landed on, if any.
    pub find_branch: Option<u64>,
}

/// Everything `REKEY-REPORT.md` renders.
pub struct Analysis {
    /// sha256 of `rekey-corpus.json` — the corpus fingerprint.
    pub manifest_sha256: String,
    /// Corpus-wide counts, from the manifest.
    pub totals: Totals,
    /// The exclusions the manifest records, verbatim.
    pub exclusions: Vec<manifest::Exclusion>,
    /// Why bug 1 is a reference slice and not an evaluation slice.
    pub reference_reason: String,
    /// The corpus constants the key-space normalizer uses.
    pub constants: Constants,
    /// The candidate space, in report order.
    pub candidates: Vec<Candidate>,
    /// One entry per re-keyable slice, in manifest order.
    pub slices: Vec<SliceAnalysis>,
    /// The bug-1 recorded-log reference rows.
    pub reference: Vec<ReferenceRow>,
    /// The ranking over [`PRIMARY_SLICE`], best first — indices into
    /// [`Analysis::candidates`].
    pub ranking: Vec<usize>,
}

impl Analysis {
    /// The primary slice's row for candidate `id`.
    pub fn primary(&self, id: &str) -> Option<&SliceScore> {
        self.slices
            .iter()
            .find(|s| s.id == PRIMARY_SLICE)?
            .scores
            .iter()
            .find(|s| s.candidate == id)
    }
}

/// Load the corpus at `root`, verify it, gate the harness against the recorded
/// campaigns, and score every candidate on every slice.
///
/// Fails loudly rather than reporting a number it cannot justify:
///
/// 1. every archive, member, and log must hash as the manifest pins it;
/// 2. the **v1-as-shipped control must reproduce every campaign's recorded
///    discovery events exactly** — if it cannot, the replay is wrong (spec gate
///    2), and no candidate is scored;
/// 3. the reconstructed selection stream must reproduce every recorded branch
///    environment and every recorded find's `path_len` / `novel_on_path`, or
///    axis (c) has no ancestry to stand on.
pub fn analyze(root: &Path) -> Result<Analysis> {
    let corpus = Corpus::load(root)?;
    let bugs = Benchmark::wave5();
    let candidates = candidate::candidates();
    // The control is the first candidate by construction (see `candidates()`).
    let control = &candidates[0];

    let mut observed: Vec<CampaignObs> = Vec::with_capacity(corpus.campaigns.len());
    for raw in &corpus.campaigns {
        observed.push(observe::observe_campaign(raw, &bugs)?);
    }

    // Gate 2, before anything is scored.
    for obs in &observed {
        replay::check_control(obs, control)?;
    }
    let chains: Vec<Chains> = observed
        .iter()
        .map(replay::reconstruct)
        .collect::<Result<_>>()?;

    let constants = score::corpus_constants(&observed);

    let mut slices = Vec::new();
    for slice in &corpus.manifest.slices {
        let members: Vec<usize> = observed
            .iter()
            .enumerate()
            .filter(|(_, o)| o.slice == slice.slice)
            .map(|(i, _)| i)
            .collect();
        let obs: Vec<&CampaignObs> = members.iter().map(|&i| &observed[i]).collect();
        let ch: Vec<&Chains> = members.iter().map(|&i| &chains[i]).collect();

        let scores = candidates
            .iter()
            .map(|c| score::score_slice(c, &slice.slice, &obs, &ch, &constants))
            .collect();

        slices.push(SliceAnalysis {
            id: slice.slice.clone(),
            bug: slice.bug,
            description: slice.description.clone(),
            explore_period: slice.explore_period,
            campaigns: obs.len() as u64,
            locality: score::exploit_locality(&obs, &ch),
            ancestry: score::ancestry_stats(&ch),
            scores,
            debut: score::debut_audit(&obs),
        });
    }

    let mut reference = Vec::new();
    for (meta, bytes) in &corpus.reference_logs {
        let log: CampaignLog = serde_json::from_slice(bytes).map_err(|source| Error::Json {
            what: meta.log.clone(),
            source,
        })?;
        let distinct: BTreeMap<u64, ()> = log
            .events
            .iter()
            .flat_map(|e| e.touched.iter().map(|&c| (c, ())))
            .collect();
        reference.push(ReferenceRow {
            config: meta.config.clone(),
            seed: meta.seed,
            distinct_cells: distinct.len() as u64,
            find_branch: log.finds.first().map(|f| f.branch),
        });
    }

    let primary = slices
        .iter()
        .find(|s| s.id == PRIMARY_SLICE)
        .ok_or_else(|| Error::Corpus {
            campaign: PRIMARY_SLICE.to_string(),
            why: "the manifest has no primary slice to rank on".to_string(),
        })?;
    let ranking = score::rank(&primary.scores);

    Ok(Analysis {
        manifest_sha256: corpus.manifest_sha256,
        totals: corpus.manifest.totals,
        exclusions: corpus.manifest.exclusions.clone(),
        reference_reason: corpus
            .manifest
            .references
            .first()
            .map(|r| r.reason.clone())
            .unwrap_or_default(),
        constants,
        candidates,
        slices,
        reference,
        ranking,
    })
}
