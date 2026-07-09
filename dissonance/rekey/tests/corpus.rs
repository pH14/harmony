// SPDX-License-Identifier: AGPL-3.0-or-later
//! The gates that bind the harness to the **real committed corpus**.
//!
//! The unit tests prove the pieces behave; these prove the whole thing
//! reproduces the campaign that actually ran. Spec gate 2 lives here: if the
//! v1-as-shipped candidate does not reproduce every recorded discovery event on
//! every corpus slice, the replay is wrong and no candidate score means
//! anything.
//!
//! `analyze` walks 60 campaigns × 30 720 branches × 13 candidates, so it runs
//! once and is shared (a `OnceLock`), keeping the suite well inside the ~3-minute
//! bar.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rekey::score::{Constants, SliceScore};
use rekey::{Analysis, PRIMARY_SLICE};

/// The committed corpus, relative to this crate.
fn corpus_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../benchmark/campaign-data")
}

/// The analysis, computed once for the whole suite.
fn analysis() -> &'static Analysis {
    static ONCE: OnceLock<Analysis> = OnceLock::new();
    ONCE.get_or_init(|| rekey::analyze(&corpus_root()).expect("the corpus analyses"))
}

/// The primary slice's row for `candidate`.
fn primary(candidate: &str) -> &'static SliceScore {
    analysis().primary(candidate).expect("candidate is scored")
}

/// **Spec gate 2, the harness-correctness gate.** `analyze` refuses to score
/// anything unless the v1 control reproduces every campaign's recorded
/// discovery events, in order and with multiplicity, on every slice — and
/// unless the reconstructed selection stream reproduces every recorded branch
/// environment and every recorded find's `path_len` / `novel_on_path`.
///
/// Reaching this assertion at all *is* the gate; the assertions below pin the
/// numbers the correlation report published.
#[test]
fn the_control_reproduces_the_recorded_campaigns() {
    let a = analysis();
    assert_eq!(a.totals.trace_files, 60, "40 campaign + 20 ablation");
    assert_eq!(a.totals.branches, 30_720, "60 x 512 branches");
    assert_eq!(a.totals.excluded_traces, 5, "3 + 2 solo re-runs");

    // CORRELATION-REPORT: "Cell counts (fairness): ~4 cells for both bugs 1 and
    // 3 under both configs" and "cells@256 takes only two values (3 or 4)".
    // Every campaign's cells are counted in its own key namespace (R2), so the
    // slice total is exactly `4 x finders + 3 x non-finders`.
    for slice in &a.slices {
        let v1 = slice
            .scores
            .iter()
            .find(|s| s.candidate == "v1-shipped")
            .expect("the control is scored on every slice");
        assert_eq!(
            v1.total_cells,
            4 * v1.finders + 3 * (v1.campaigns - v1.finders),
            "{}: every campaign holds exactly 3 or 4 cells",
            slice.id
        );
        let mean = v1.mean_cells_q32 >> 32;
        assert!(
            (3..=4).contains(&mean),
            "{}: per-campaign cells are 3 or 4, got mean {mean}",
            slice.id
        );
    }
}

/// **R2's per-seed pin, on the real corpus.** Cell keys are never compared across
/// seeds: each campaign's archive is keyed in its own namespace, so the slice
/// total is the *sum* of per-campaign cell counts, not a cross-seed union. The
/// two coincide only if every campaign discovers the same cells, which they do
/// not.
#[test]
fn breadth_is_per_campaign_never_pooled_across_seeds() {
    for slice in &analysis().slices {
        for s in &slice.scores {
            let mean = s.mean_cells_q32;
            // total == mean x campaigns, to within the fixed-point mean's rounding.
            let reconstructed = (u128::from(mean) * u128::from(s.campaigns)) >> 32;
            assert!(
                (reconstructed as u64).abs_diff(s.total_cells) <= 1,
                "{}/{}: total {} is not the sum of per-campaign counts",
                slice.id,
                s.candidate,
                s.total_cells
            );
            // Coverage normalizes the per-campaign mean, never the slice total.
            assert_eq!(s.breadth_q32, rekey::fixed::div_int_q32(mean, s.key_space));
        }
    }
    // A cross-seed union would have collapsed the 40 bug-3 campaigns' 149 cells
    // into the 4 abstract cells their count-based keys share.
    let v1 = primary("v1-shipped");
    assert_eq!(v1.total_cells, 149, "40 campaigns: 29 x 4 + 11 x 3");
    assert!(v1.total_cells > 4, "not a cross-seed union");
}

/// The manifest committed at `campaign-data/rekey-corpus.json` is the one the
/// corpus hashes to. A stale manifest would silently score a different corpus.
#[test]
fn the_committed_manifest_is_fresh() {
    let root = corpus_root();
    let rebuilt = rekey::manifest::render(&rekey::manifest::build(&root).expect("build"));
    let committed = std::fs::read_to_string(rekey::manifest::manifest_path(&root)).expect("read");
    assert_eq!(
        rebuilt, committed,
        "the committed manifest is stale: rerun `cargo run -p rekey -- manifest --write`"
    );
}

/// A corpus that does not hash as the manifest pins it aborts loudly. The
/// `hm-xdp` lesson: never trust a mutable path.
#[test]
fn a_hash_mismatch_aborts_the_run() {
    let scratch = tempfile::tempdir().expect("tempdir");
    let root = corpus_root();

    // A manifest whose first trace's hash is wrong, over the real corpus.
    let mut manifest = rekey::manifest::build(&root).expect("build");
    manifest.slices[0].traces[0].sha256 = "0".repeat(64);

    // Mirror the corpus into the scratch dir: the two files the damaged
    // manifest reaches are the archive and the first campaign log.
    let slice = &manifest.slices[0];
    for rel in [slice.archive.clone(), slice.traces[0].log.clone()] {
        let dst = scratch.path().join(&rel);
        std::fs::create_dir_all(dst.parent().expect("has a parent")).expect("mkdir");
        std::fs::copy(root.join(&rel), &dst).expect("copy");
    }
    std::fs::write(
        rekey::manifest::manifest_path(scratch.path()),
        rekey::manifest::render(&manifest),
    )
    .expect("write manifest");

    match rekey::manifest::Corpus::load(scratch.path()) {
        Err(rekey::Error::HashMismatch { .. }) => {}
        Err(other) => panic!("expected a loud hash mismatch, got {other}"),
        Ok(_) => panic!("a corrupted manifest must never load"),
    }
}

/// The report is a pure function of the corpus: two renders are byte-identical,
/// and it carries no generated-date line to make them differ.
#[test]
fn the_report_is_byte_identical_across_runs() {
    let a = analysis();
    let first = rekey::report::render(a);
    let second = rekey::report::render(a);
    assert_eq!(first, second, "rendering is pure");

    // A second, independent analysis of the same corpus renders the same bytes.
    let fresh = rekey::analyze(&corpus_root()).expect("re-analyse");
    assert_eq!(
        rekey::report::render(&fresh),
        first,
        "the whole pipeline is deterministic"
    );

    // No wall-clock anywhere: the report must not embed a date. (`Date:` /
    // `Generated:` are the shapes a future edit would reach for.)
    for banned in ["Generated:", "Date:", "generated on"] {
        assert!(!first.contains(banned), "the report embeds `{banned}`");
    }
}

/// The committed report on disk is the one this harness renders.
#[test]
fn the_committed_report_is_fresh() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../benchmark/REKEY-REPORT.md");
    let committed = std::fs::read_to_string(&path).expect("REKEY-REPORT.md is committed");
    assert_eq!(
        rekey::report::render(analysis()),
        committed,
        "the committed report is stale: rerun `cargo run -p rekey -- score`"
    );
}

/// **The report's headline, as a gate.** On bug 3, v1's every species debuts on
/// branch 0 or on the finding branch; every finder's last species debuts exactly
/// at the find (it is the kernel's fault message); every non-finder's archive is
/// frozen from branch 0 onward. So the shipped cell function discovers *nothing*
/// while the search is still searching.
#[test]
fn v1s_only_post_genesis_cell_is_the_crash() {
    for slice in &analysis().slices {
        let d = &slice.debut;
        assert_eq!(
            d.debut_at_zero_or_find, d.campaigns,
            "{}: every species debuts at branch 0 or at the find",
            slice.id
        );
        assert_eq!(
            d.terminal_debut_at_find, d.finders,
            "{}: every finder's last species debuts exactly at the find",
            slice.id
        );
        assert_eq!(
            d.frozen_non_finders,
            d.campaigns - d.finders,
            "{}: every non-finder's archive is frozen after branch 0",
            slice.id
        );

        let v1 = slice
            .scores
            .iter()
            .find(|s| s.candidate == "v1-shipped")
            .expect("control");
        assert_eq!(
            v1.cells_before_find, 0,
            "{}: zero steering signal",
            slice.id
        );
        assert_eq!(
            v1.cells_after_branch0, d.finders,
            "{}: one post-genesis cell per finder — the crash",
            slice.id
        );
        assert_eq!(
            v1.crash_only_cells, d.finders,
            "{}: exactly one crash-only cell per finding campaign",
            slice.id
        );
    }
}

/// **The knob space is inert.** Every `fold_k` in the sweep exceeds the largest
/// species id and every quantization collapses the same three counts, so the
/// whole R2 knob-set space scores exactly as the control — a proof from the
/// corpus, not a sampling accident. Only `species-only` (which drops a channel)
/// and `no-channels` (the one-cell floor) move, and both move *down*.
#[test]
fn no_v1_knob_setting_changes_anything() {
    let v1 = primary("v1-shipped");
    for knob in [
        "foldk-16",
        "foldk-32",
        "foldk-128",
        "foldk-256",
        "quant-identity",
        "lastnew-only",
    ] {
        let s = primary(knob);
        assert_eq!(
            s.partition_digest, v1.partition_digest,
            "{knob}: the same cell partition, arrival for arrival"
        );
        assert_eq!(s.total_cells, v1.total_cells, "{knob}: same cells");
        assert_eq!(
            s.objective_q32, v1.objective_q32,
            "{knob}: same granularity"
        );
        assert_eq!(s.cells_before_find, 0, "{knob}: still no steering");
    }
    // `|K|` is a property of the config, not of what it discovered, so an
    // identical partition can still normalize to a different coverage. The menu
    // says so where it collapses these rows.
    assert_ne!(
        primary("quant-identity").breadth_q32,
        v1.breadth_q32,
        "identical partition, different key-space denominator"
    );

    assert!(primary("species-only").total_cells < v1.total_cells);
    assert_ne!(
        primary("species-only").partition_digest,
        v1.partition_digest
    );
    assert_eq!(
        primary("no-channels").total_cells,
        v1.campaigns,
        "the floor: one cell per campaign"
    );
}

/// **Axis (c) is vacuous on this corpus, and the report must not pretend
/// otherwise.** Every finding chain's proper ancestors are branch 0, which every
/// candidate admits — so even the one-cell floor "preserves" every chain.
#[test]
fn chain_preservation_discriminates_nothing_here() {
    let primary_slice = analysis()
        .slices
        .iter()
        .find(|s| s.id == PRIMARY_SLICE)
        .expect("primary slice");
    for s in &primary_slice.scores {
        assert!(
            s.chain_preserved(),
            "{}: nothing is disqualified on this corpus",
            s.candidate
        );
    }
    // 29 finds; only the 4 exploit-borne ones have a proper ancestor at all.
    let any = &primary_slice.scores[0];
    assert_eq!(any.chains_checked, 29);
    assert_eq!(any.ancestors_checked, 4, "depth-2 chains only");

    // The ablation never exploits, so its finds have no ancestors whatsoever.
    let ablation = analysis()
        .slices
        .iter()
        .find(|s| s.id == rekey::manifest::BUG3_ABLATION)
        .expect("ablation slice");
    assert_eq!(ablation.scores[0].ancestors_checked, 0);
    assert!(ablation.scores[0].chain_cell().contains("vacuous"));
}

/// **The twin control.** `draw-top-256` reads the byte bug 3's trigger compares;
/// `draw-low-256` reads a byte no trigger reads. On the unsteered ablation slice
/// — the only slice free of the exploit's confound — **no axis separates them**.
/// Where they part company, it is crash fragmentation or the exploit kernel's
/// bit-locality, never the trigger. Law 6, on our corpus.
#[test]
fn the_trigger_blind_twin_is_not_distinguishable() {
    let ablation = analysis()
        .slices
        .iter()
        .find(|s| s.id == rekey::manifest::BUG3_ABLATION)
        .expect("ablation slice");
    let row = |id: &str| {
        ablation
            .scores
            .iter()
            .find(|s| s.candidate == id)
            .expect("scored")
    };
    let (top, low) = (row("draw-top-256"), row("draw-low-256"));

    // Both have identical key spaces — the twin is exact by construction.
    assert_eq!(top.key_space, low.key_space);

    // Both objectives agree to well under 1% of `1.0` in Q32.
    let tolerance = rekey::fixed::ONE / 100;
    assert!(
        top.objective_q32.abs_diff(low.objective_q32) < tolerance,
        "O@64: {} vs {}",
        top.objective_q32,
        low.objective_q32
    );
    assert!(
        top.objective_alt_q32.abs_diff(low.objective_alt_q32) < tolerance,
        "O@256: {} vs {}",
        top.objective_alt_q32,
        low.objective_alt_q32
    );
    // Mean cells per campaign, and hence coverage, agree within two cells.
    assert!(top.mean_cells_q32.abs_diff(low.mean_cells_q32) < rekey::fixed::ONE * 2);
    assert!(top.breadth_q32.abs_diff(low.breadth_q32) < tolerance);
    // Total cells agree within 1%.
    assert!(top.total_cells.abs_diff(low.total_cells) * 100 < top.total_cells);
    // Steering agrees within 1%.
    assert!(top.cells_before_find.abs_diff(low.cells_before_find) * 100 < top.cells_before_find);
    // And chain preservation is vacuous for both.
    assert_eq!(top.chain_cell(), low.chain_cell());

    // The one place they part company on the clean slice cuts AGAINST the
    // trigger-aligned candidate: the top byte pins every crashing branch to one
    // cell per campaign; the low byte fragments the crash across many.
    assert_eq!(
        top.crash_only_cells, top.finders,
        "one crash cell per finding campaign — every crash draws 0xA5"
    );
    assert!(
        low.crash_only_cells > top.crash_only_cells,
        "the low byte scatters the crash: {} vs {}",
        low.crash_only_cells,
        top.crash_only_cells
    );

    // On the STEERED slice they do differ — and the difference is the exploit's,
    // not the trigger's (see `the_exploit_preserves_the_low_byte_but_not_the_top_byte`).
    let (ptop, plow) = (primary("draw-top-256"), primary("draw-low-256"));
    assert!(
        ptop.total_cells > plow.total_cells,
        "the exploit resamples the low byte less, so the low twin sees fewer cells"
    );
}

/// The exploit kernel's locality, measured rather than assumed — the number the
/// report cites to explain why the steered slice is the wrong place to compare
/// the twins.
#[test]
fn the_exploit_preserves_the_low_byte_but_not_the_top_byte() {
    let primary_slice = analysis()
        .slices
        .iter()
        .find(|s| s.id == PRIMARY_SLICE)
        .expect("primary slice");
    let l = primary_slice.locality;
    assert!(
        l.exploits > 7_000,
        "the signal config exploits ~3/4 of 10240"
    );

    // Twiddling a low seed bit always changes the draw's low byte.
    assert_eq!(l.low_bit_shares_low, 0, "a low-bit flip never preserves it");
    // Twiddling a high one preserves it about half the time.
    let high = l.high_bit_exploits();
    let kept = l.high_bit_shares_low();
    assert!(
        kept * 100 > high * 45 && kept * 100 < high * 55,
        "{kept}/{high}"
    );
    // The top byte survives only at chance (1/256 ≈ 0.4%).
    assert!(l.shares_top * 100 < l.exploits, "top byte survives < 1%");

    // The ablation never exploits at all — that is what makes it the clean slice.
    let ablation = analysis()
        .slices
        .iter()
        .find(|s| s.id == rekey::manifest::BUG3_ABLATION)
        .expect("ablation slice");
    assert_eq!(ablation.locality.exploits, 0);
}

/// The corpus constants the key-space normalizer uses are the ones the corpus
/// actually contains: four template species (three, plus the crash), and both
/// draw-byte projections span the full 256-value alphabet.
#[test]
fn the_corpus_constants_are_what_the_report_prints() {
    assert_eq!(
        analysis().constants,
        Constants {
            max_species: 4,
            top_alphabet: 256,
            low_alphabet: 256,
        }
    );
}

/// Bug 1 is present as recorded logs and **not** as a re-keyable slice: its
/// campaign predates trace retention. The report says so; the manifest says why.
#[test]
fn bug1_is_a_reference_slice_with_no_traces() {
    let a = analysis();
    assert_eq!(a.reference.len(), 40, "20 seeds x 2 configs of logs");
    assert!(a.reference.iter().all(|r| r.find_branch.is_some()), "20/20");
    assert!(
        a.reference.iter().all(|r| r.distinct_cells == 2),
        "a two-cell vocabulary, thinner even than bug 3's"
    );
    assert!(
        a.slices.iter().all(|s| s.bug == 3),
        "only bug 3 is re-keyed"
    );
    assert!(a.reference_reason.contains("CANNOT be re-keyed"));
    assert!(a.reference_reason.contains("hm-5sv"), "the bead is cited");
}
