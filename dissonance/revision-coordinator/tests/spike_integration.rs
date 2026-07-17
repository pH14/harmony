// SPDX-License-Identifier: AGPL-3.0-or-later
//! M2 — integration against the merged spike dataflow (tasks/120,
//! `spikes/differential-lineage`).
//!
//! The coordinator is the control side; the spike crate is the proven
//! observation plane. The wiring under test: every spike fixture revision
//! becomes one already-durable evidence batch (identified by digest, opaque
//! to the coordinator); the coordinator assigns proposals in canonical
//! order, absorbs completions in ARBITRARY arrival order (with optional
//! crash + recovery in the middle, and optional cohort structure), and its
//! `probe_drive` view — the committed `(Revision, EvidenceBatchId)` log —
//! is what feeds the spike dataflow: the effective fixture is rebuilt FROM
//! the drained view (batch identities resolved back to records, revisions
//! restamped to the committed revision), so any ordering, coverage, or
//! frontier bug in the coordinator corrupts the fixture and diverges the
//! artifacts.
//!
//! Asserted byte-wise (a divergence is blocking):
//! - identical artifacts across (a) completion-order permutation, (b) a
//!   restart (crash + `recover` mid-campaign), and (c) cohort-frozen mint
//!   order;
//! - genesis replay equals cached lineage plus suffix: the spike's
//!   plain-Rust genesis-replay referee equals both dataflow formulations
//!   (naive prefix-join and shared segment aggregates) on the
//!   coordinator-produced fixture, encoded and compared as bytes.

use std::collections::BTreeMap;

use differential_lineage::data::{Fixture, Replay};
use differential_lineage::dataflow::{BuildOpts, Captured, run};
use differential_lineage::fixtures;
use differential_lineage::referee::Referee;
use revision_coordinator::{
    CampaignConfigId, Completion, Coordinator, DrainedView, EvidenceBatchId, MemLedger,
    PendingProposal, Revision, TerminalRecord,
};

fn config() -> CampaignConfigId {
    CampaignConfigId::digest(b"tests/spike_integration")
}

fn terminal(rev: u64) -> TerminalRecord {
    TerminalRecord {
        moment: 1_000 + rev,
        work: 10 * rev,
    }
}

/// The sorted, deduplicated set of revisions any fixture record commits at.
fn occupied(fx: &Fixture) -> Vec<u64> {
    let mut revs: Vec<u64> = fx
        .registers
        .iter()
        .map(|r| r.rev)
        .chain(fx.sources.iter().map(|r| r.rev))
        .chain(fx.properties.iter().map(|r| r.rev))
        .chain(fx.events.iter().map(|r| r.rev))
        .chain(fx.scrape.iter().map(|r| r.rev))
        .chain(fx.lineage.iter().map(|r| r.rev))
        .chain(fx.obs_cuts.iter().map(|r| r.rev))
        .chain(fx.seals.iter().map(|r| r.rev))
        .chain(fx.entry_commits.iter().map(|r| r.rev))
        .chain(fx.working.iter().map(|r| r.rev))
        .chain(fx.seq_queries.iter().map(|r| r.rev))
        .chain(fx.finalizations.iter().map(|r| r.rev))
        .collect();
    revs.sort_unstable();
    revs.dedup();
    revs
}

/// The sub-fixture of records committing exactly at `rev` — one
/// already-durable evidence batch (what hm-bbx.4 would persist before the
/// coordinator ever sees its identity).
fn batch_fixture(fx: &Fixture, rev: u64) -> Fixture {
    Fixture {
        name: fx.name.clone(),
        registers: fx
            .registers
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        sources: fx
            .sources
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        properties: fx
            .properties
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        events: fx.events.iter().filter(|r| r.rev == rev).cloned().collect(),
        scrape: fx.scrape.iter().filter(|r| r.rev == rev).cloned().collect(),
        lineage: fx
            .lineage
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        obs_cuts: fx
            .obs_cuts
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        seals: fx.seals.iter().filter(|r| r.rev == rev).cloned().collect(),
        entry_commits: fx
            .entry_commits
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        working: fx
            .working
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        seq_queries: fx
            .seq_queries
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
        finalizations: fx
            .finalizations
            .iter()
            .filter(|r| r.rev == rev)
            .cloned()
            .collect(),
    }
}

/// Digest-based batch identity: the canonical encoding of the batch content.
fn batch_id(sub: &Fixture) -> EvidenceBatchId {
    EvidenceBatchId::digest(&serde_json::to_vec(sub).unwrap())
}

/// Restamp every record's revision through `map` (original -> committed) in
/// both the fixture and the replay vectors (the referee filters replay
/// events by revision, so both sides must move together).
fn restamp(fx: &Fixture, replay: &Replay, map: &BTreeMap<u64, u64>) -> (Fixture, Replay) {
    let m = |rev: u64| -> u64 { map[&rev] };
    let mut fx2 = fx.clone();
    for r in &mut fx2.registers {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.sources {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.properties {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.events {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.scrape {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.lineage {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.obs_cuts {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.seals {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.entry_commits {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.working {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.seq_queries {
        r.rev = m(r.rev);
    }
    for r in &mut fx2.finalizations {
        r.rev = m(r.rev);
    }
    let mut rp2 = replay.clone();
    for (_, vector) in &mut rp2.full {
        for e in vector {
            e.rev = m(e.rev);
        }
    }
    (fx2, rp2)
}

/// How proposals are minted.
#[derive(Clone, Copy)]
enum Plan {
    /// One frozen cohort holding every proposal.
    Single,
    /// Two frozen cohorts: the first half and the second half of the
    /// canonical mint order.
    Split,
}

/// Drive one full coordination scenario over `k` batches: mint under
/// `plan`, complete in `order` (an arbitrary permutation, applied within
/// each cohort phase — the PR #124 cohort barrier makes cohorts strictly
/// sequential), optionally crash and recover after `crash_at` completions,
/// and return the final probe-drive view.
fn coordinate(
    batches: &[EvidenceBatchId],
    order: &[usize],
    plan: Plan,
    crash_at: Option<usize>,
) -> DrainedView {
    let k = batches.len();
    let mut ledger = MemLedger::new();
    let mut c = Coordinator::genesis(Box::new(ledger.clone()), config()).unwrap();

    let mut ranges: Vec<std::ops::Range<usize>> = Vec::new();
    match plan {
        Plan::Single => ranges.push(0..k),
        Plan::Split => {
            let half = k.div_ceil(2);
            ranges.push(0..half);
            ranges.push(half..k);
        }
    }

    let mut pendings: Vec<PendingProposal> = Vec::new();
    let mut completed: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut t = 0usize; // global completion step, for the crash point
    for range in &ranges {
        let cohort = c.open_cohort().unwrap();
        // The frozen view is constant by construction: everything before
        // this cohort.
        assert_eq!(
            c.cohort_view(cohort),
            Some(Revision::new(range.start as u64))
        );
        for _ in range.clone() {
            pendings.push(c.assign(cohort).unwrap());
        }
        c.close_cohort(cohort).unwrap();

        for &pi in order.iter().filter(|&&pi| range.contains(&pi)) {
            if crash_at == Some(t) {
                // Process death mid-campaign: recover from the durable
                // ledger and re-learn the pending set (same ProposalIds,
                // never fresh slots).
                ledger.crash();
                c = Coordinator::recover(&ledger).unwrap();
                let still_pending = c.pending();
                for p in &pendings {
                    let idx = (p.revision.get() - 1) as usize;
                    assert_eq!(still_pending.contains(p), !completed.contains(&idx));
                }
            }
            let p = pendings[pi];
            c.complete(Completion {
                proposal: p.proposal,
                batch: batches[pi],
                terminal: terminal(p.revision.get()),
            })
            .unwrap();
            completed.insert(pi);
            c.drain_ready();
            t += 1;
        }
    }

    c.probe_drive(Revision::new(k as u64)).unwrap()
}

/// Rebuild the effective fixture from the coordinator's committed view:
/// resolve each row's batch identity back to its records and restamp
/// revisions to the committed revision. Coverage and order both come from
/// the coordinator — a hole, duplicate, or misorder corrupts the result.
fn effective(
    fx: &Fixture,
    replay: &Replay,
    resolver: &BTreeMap<EvidenceBatchId, u64>,
    view: &DrainedView,
) -> (Fixture, Replay) {
    assert_eq!(
        view.frontier.get() as usize,
        resolver.len(),
        "frontier short of the full campaign"
    );
    let mut map: BTreeMap<u64, u64> = BTreeMap::new();
    for (committed_rev, batch) in &view.rows {
        let original = *resolver.get(batch).expect("unknown batch identity in view");
        assert!(
            map.insert(original, committed_rev.get()).is_none(),
            "batch committed twice"
        );
    }
    assert_eq!(map.len(), resolver.len(), "incomplete batch coverage");
    restamp(fx, replay, &map)
}

/// Run the spike dataflow on the effective fixture and encode every
/// captured view (both point-observation formulations) at the final
/// revision as canonical bytes. Also asserts, byte-wise, that genesis
/// replay (the referee) equals cached lineage plus suffix (both dataflow
/// formulations).
fn artifacts(fx: &Fixture, replay: &Replay, order_seed: u64) -> Vec<u8> {
    let final_rev = *occupied(fx).last().expect("non-empty fixture");
    let cap = run(fx, BuildOpts::default(), order_seed).expect("valid fixture");
    let referee = Referee::new(fx, replay).expect("valid fixture + replay");

    let mut doc: BTreeMap<&str, serde_json::Value> = BTreeMap::new();
    let mut put = |name: &'static str, value: serde_json::Value| {
        doc.insert(name, value);
    };
    macro_rules! view {
        ($field:ident, $ref_fn:ident, $name:expr) => {{
            let dd = Captured::flat(&cap.$field, final_rev);
            let genesis = referee.$ref_fn(final_rev);
            let dd_bytes = serde_json::to_vec(&dd).unwrap();
            let genesis_bytes = serde_json::to_vec(&genesis).unwrap();
            assert_eq!(
                dd_bytes, genesis_bytes,
                "{}: genesis replay != cached lineage plus suffix",
                $name
            );
            put($name, serde_json::to_value(&dd).unwrap());
        }};
    }
    // Both formulations must match the genesis-replay referee byte-wise.
    view!(obs_naive, obs, "obs_naive");
    view!(obs_shared, obs, "obs_shared");
    view!(seal_prefix, seal_prefix, "seal_prefix");
    view!(cells, cells, "cells");
    view!(transitions, transitions, "transitions");
    view!(occupancy, occupancy, "occupancy");
    view!(property_results, property_results, "property_results");
    view!(site_coverage, site_coverage, "site_coverage");
    view!(absence, absence, "absence");
    view!(working_species, working_species, "working_species");
    view!(seq_pairs, seq_pairs, "seq_pairs");
    view!(seq_rejections, seq_rejections, "seq_rejections");
    view!(scrape_terminal, scrape_terminal, "scrape_terminal");
    serde_json::to_vec(&doc).unwrap()
}

/// Deterministic permutation of `0..n` from a splitmix-style seed.
fn permutation(n: usize, seed: u64) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    let mut s = seed;
    for i in (1..n).rev() {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        order.swap(i, (s >> 33) as usize % (i + 1));
    }
    order
}

/// The full M2 gate over one spike fixture.
fn m2_gate(fx: &Fixture, replay: &Replay) {
    let occ = occupied(fx);
    let k = occ.len();
    let batches: Vec<EvidenceBatchId> = occ
        .iter()
        .map(|&r| batch_id(&batch_fixture(fx, r)))
        .collect();
    let resolver: BTreeMap<EvidenceBatchId, u64> =
        batches.iter().copied().zip(occ.iter().copied()).collect();

    // Baseline: in-order completions, one cohort, no crash.
    let in_order: Vec<usize> = (0..k).collect();
    let base_view = coordinate(&batches, &in_order, Plan::Single, None);
    let (bfx, brp) = effective(fx, replay, &resolver, &base_view);
    let base = artifacts(&bfx, &brp, 0);

    // (a) Completion-order permutations.
    for seed in [7u64, 42] {
        let view = coordinate(&batches, &permutation(k, seed), Plan::Single, None);
        assert_eq!(view.encode(), base_view.encode(), "drained view diverged");
        let (efx, erp) = effective(fx, replay, &resolver, &view);
        assert_eq!(artifacts(&efx, &erp, 0), base, "permutation diverged");
    }

    // (b) Restart: crash + recover mid-campaign.
    let view = coordinate(&batches, &permutation(k, 7), Plan::Single, Some(k / 2));
    assert_eq!(view.encode(), base_view.encode(), "restart view diverged");
    let (efx, erp) = effective(fx, replay, &resolver, &view);
    assert_eq!(artifacts(&efx, &erp, 0), base, "restart diverged");

    // (c) Cohort-frozen mint order.
    let view = coordinate(&batches, &permutation(k, 42), Plan::Split, Some(k / 3));
    assert_eq!(view.encode(), base_view.encode(), "cohort view diverged");
    let (efx, erp) = effective(fx, replay, &resolver, &view);
    assert_eq!(artifacts(&efx, &erp, 0), base, "cohort mint order diverged");

    // Spike-internal feed shuffle must not matter either (ties the chain).
    assert_eq!(artifacts(&bfx, &brp, 99), base, "spike feed order diverged");
}

#[test]
fn tree_lineage_through_the_coordinator() {
    let (fx, replay) = fixtures::tree_lineage();
    m2_gate(&fx, &replay);
}

#[test]
fn two_pass_through_the_coordinator() {
    let (fx, replay) = fixtures::two_pass();
    m2_gate(&fx, &replay);
}

#[test]
fn retention_properties_through_the_coordinator() {
    let (fx, replay) = fixtures::retention_properties();
    m2_gate(&fx, &replay);
}

/// The hand fixtures use dense source revisions, making the committed-order
/// restamp an identity; this case dilates the source revisions (r -> 3r+1)
/// so the coordinator's dense committed revisions genuinely re-timestamp
/// every record and replay vector — the production story, where the
/// ledger's committed revision IS the Differential timestamp regardless of
/// any upstream numbering.
#[test]
fn sparse_source_revisions_restamp_to_committed_order() {
    let (fx, replay) = fixtures::tree_lineage();
    let dilate: BTreeMap<u64, u64> = occupied(&fx).iter().map(|&r| (r, 3 * r + 1)).collect();
    let (sparse_fx, sparse_rp) = restamp(&fx, &replay, &dilate);
    assert_ne!(
        occupied(&sparse_fx),
        occupied(&fx),
        "dilation had no effect"
    );
    m2_gate(&sparse_fx, &sparse_rp);
}

/// A partial cohort's batches never reach the search-visible view: with the
/// second cohort still incomplete, the frontier stops at the first cohort's
/// boundary and only its batches are drained into the visible input log.
#[test]
fn partial_cohort_stays_out_of_the_visible_input_log() {
    let (fx, _replay) = fixtures::tree_lineage();
    let occ = occupied(&fx);
    let k = occ.len();
    assert!(k >= 2, "fixture too small to split");
    let batches: Vec<EvidenceBatchId> = occ
        .iter()
        .map(|&r| batch_id(&batch_fixture(&fx, r)))
        .collect();

    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    let half = k.div_ceil(2);
    let a = c.open_cohort().unwrap();
    let mut pendings = Vec::new();
    for _ in 0..half {
        pendings.push(c.assign(a).unwrap());
    }
    c.close_cohort(a).unwrap();
    // The barrier: cohort B cannot open until A is fully committed.
    assert!(matches!(
        c.open_cohort(),
        Err(revision_coordinator::CoordError::CohortBarrier { blocking }) if blocking == a
    ));
    for (i, p) in pendings.iter().enumerate().take(half) {
        c.complete(Completion {
            proposal: p.proposal,
            batch: batches[i],
            terminal: terminal(p.revision.get()),
        })
        .unwrap();
    }
    let b = c.open_cohort().unwrap();
    for _ in half..k {
        pendings.push(c.assign(b).unwrap());
    }
    // Cohort B stays OPEN; complete all its members anyway.
    for (i, p) in pendings.iter().enumerate().skip(half) {
        c.complete(Completion {
            proposal: p.proposal,
            batch: batches[i],
            terminal: terminal(p.revision.get()),
        })
        .unwrap();
    }
    let view = c.probe_drive(Revision::new(half as u64)).unwrap();
    assert_eq!(view.frontier, Revision::new(half as u64));
    assert_eq!(view.rows.len(), half);
    for (i, (rev, batch)) in view.rows.iter().enumerate() {
        assert_eq!(rev.get() as usize, i + 1);
        assert_eq!(*batch, batches[i], "cohort B's batch leaked into the view");
    }
    // The full frontier is unreachable while B is open...
    assert!(c.probe_drive(Revision::new(k as u64)).is_err());
    // ...and closing B atomically releases the rest.
    c.close_cohort(b).unwrap();
    let view = c.probe_drive(Revision::new(k as u64)).unwrap();
    assert_eq!(view.rows.len(), k);
}
