// SPDX-License-Identifier: AGPL-3.0-or-later
//! End-to-end coordinator scenarios: the persist-then-dispatch handshake,
//! out-of-order completion buffering, cohort freeze/visibility, abort,
//! recovery equality across ledger backends, and the golden encoded
//! projection (the determinism gate: byte-wise divergence is blocking).

use revision_coordinator::{
    CampaignConfigId, CohortId, Completion, CoordError, Coordinator, EvidenceBatchId, MemLedger,
    ProposalId, Revision, TerminalRecord,
};

fn config() -> CampaignConfigId {
    CampaignConfigId::digest(b"tests/coordinator_flow")
}

/// Deterministic batch identity for a revision (stands in for hm-bbx.4's
/// durable evidence batches).
fn batch(rev: u64) -> EvidenceBatchId {
    EvidenceBatchId::digest(&rev.to_le_bytes())
}

/// Deterministic terminal record for a revision.
fn terminal(rev: u64) -> TerminalRecord {
    TerminalRecord {
        moment: 1_000 + rev,
        work: 10 * rev,
    }
}

fn completion(p: revision_coordinator::PendingProposal) -> Completion {
    Completion {
        proposal: p.proposal,
        batch: batch(p.revision.get()),
        terminal: terminal(p.revision.get()),
    }
}

#[test]
fn out_of_order_completions_drain_contiguously() {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    let cohort = c.open_cohort().unwrap();
    let p1 = c.assign(cohort).unwrap();
    let p2 = c.assign(cohort).unwrap();
    let p3 = c.assign(cohort).unwrap();
    assert_eq!(
        (p1.revision, p2.revision, p3.revision),
        (Revision::new(1), Revision::new(2), Revision::new(3))
    );

    // Completion order 3, 1, 2 — the frontier never passes a gap.
    c.complete(completion(p3)).unwrap();
    assert_eq!(c.drain_ready(), vec![]);
    c.complete(completion(p1)).unwrap();
    assert_eq!(c.drain_ready(), vec![(Revision::new(1), batch(1))]);
    c.complete(completion(p2)).unwrap();
    assert_eq!(
        c.drain_ready(),
        vec![(Revision::new(2), batch(2)), (Revision::new(3), batch(3))]
    );
    // Draining is exactly-once per process.
    assert_eq!(c.drain_ready(), vec![]);
    assert_eq!(c.committed_frontier(), Revision::new(3));

    // Not yet search-visible: the cohort is still open.
    assert_eq!(c.visible_frontier(), Revision::ZERO);
    let err = c.probe_drive(Revision::new(1)).unwrap_err();
    assert!(matches!(err, CoordError::FrontierStalled { .. }), "{err}");

    c.close_cohort(cohort).unwrap();
    assert_eq!(c.visible_frontier(), Revision::new(3));
    let view = c.probe_drive(Revision::new(3)).unwrap();
    assert_eq!(view.frontier, Revision::new(3));
    assert_eq!(
        view.rows,
        vec![
            (Revision::new(1), batch(1)),
            (Revision::new(2), batch(2)),
            (Revision::new(3), batch(3)),
        ]
    );
}

#[test]
fn cohort_freeze_hides_partial_results() {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    let a = c.open_cohort().unwrap();
    assert_eq!(c.cohort_view(a), Some(Revision::ZERO));
    let a1 = c.assign(a).unwrap();
    let a2 = c.assign(a).unwrap();

    // A later cohort's frozen view excludes A entirely while A is partial.
    let b = c.open_cohort().unwrap();
    assert_eq!(c.cohort_view(b), Some(Revision::ZERO));

    c.complete(completion(a1)).unwrap();
    // Partial cohort: committed but not visible, even though contiguous.
    let view = c.probe_drive(Revision::ZERO).unwrap();
    assert_eq!(view.frontier, Revision::ZERO);
    assert_eq!(view.rows, vec![]);

    c.close_cohort(a).unwrap();
    // Closed but still partial (a2 uncommitted): still invisible.
    assert_eq!(c.probe_drive(Revision::ZERO).unwrap().rows, vec![]);

    c.complete(completion(a2)).unwrap();
    // Closed + fully committed: both members become visible atomically.
    let view = c.probe_drive(Revision::new(2)).unwrap();
    assert_eq!(view.frontier, Revision::new(2));
    assert_eq!(view.rows.len(), 2);

    // A cohort opened now freezes the post-A view.
    let d = c.open_cohort().unwrap();
    assert_eq!(c.cohort_view(d), Some(Revision::new(2)));
}

#[test]
fn retry_is_idempotent_and_divergence_is_refused() {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    let cohort = c.open_cohort().unwrap();
    let p = c.assign(cohort).unwrap();
    c.complete(completion(p)).unwrap();
    // A crashed worker retries the SAME ProposalId with identical results.
    c.complete(completion(p)).unwrap();
    // A divergent retry is a determinism violation, not a new result.
    let divergent = Completion {
        proposal: p.proposal,
        batch: batch(999),
        terminal: terminal(p.revision.get()),
    };
    assert!(matches!(
        c.complete(divergent),
        Err(CoordError::CommitConflict { .. })
    ));
}

#[test]
fn typed_errors_for_misuse() {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    assert!(matches!(
        c.assign(CohortId::new(9)),
        Err(CoordError::UnknownCohort(_))
    ));
    let cohort = c.open_cohort().unwrap();
    c.close_cohort(cohort).unwrap();
    assert!(matches!(c.assign(cohort), Err(CoordError::CohortClosed(_))));
    assert!(matches!(
        c.close_cohort(cohort),
        Err(CoordError::CohortClosed(_))
    ));
    assert!(matches!(
        c.complete(Completion {
            proposal: ProposalId::new(42),
            batch: batch(1),
            terminal: terminal(1),
        }),
        Err(CoordError::UnknownProposal(_))
    ));

    // genesis on a used ledger / recover on an empty one.
    let used = MemLedger::new();
    let _ = Coordinator::genesis(Box::new(used.clone()), config()).unwrap();
    assert!(matches!(
        Coordinator::genesis(Box::new(used), config()),
        Err(CoordError::AlreadyInitialized)
    ));
    assert!(matches!(
        Coordinator::recover(&MemLedger::new()),
        Err(CoordError::MissingGenesis)
    ));
}

#[test]
fn abort_freezes_the_frontier_forever() {
    let ledger = MemLedger::new();
    let mut c = Coordinator::genesis(Box::new(ledger.clone()), config()).unwrap();
    let cohort = c.open_cohort().unwrap();
    let p1 = c.assign(cohort).unwrap();
    let p2 = c.assign(cohort).unwrap();
    c.complete(completion(p1)).unwrap();
    c.abort("injected unrecoverable control failure").unwrap();
    c.abort("idempotent").unwrap();

    assert!(matches!(c.assign(cohort), Err(CoordError::Aborted { .. })));
    assert!(matches!(
        c.complete(completion(p2)),
        Err(CoordError::Aborted { .. })
    ));
    assert!(matches!(
        c.probe_drive(Revision::ZERO),
        Err(CoordError::Aborted { .. })
    ));
    assert_eq!(c.drain_ready(), vec![]);

    // The abort is durable: recovery refuses to advance too, and the
    // pending slot was never skipped.
    let r = Coordinator::recover(&ledger).unwrap();
    assert_eq!(r.aborted(), Some("injected unrecoverable control failure"));
    assert_eq!(r.pending(), vec![p2]);
    assert_eq!(r.state_projection().encode(), c.state_projection().encode());
}

#[test]
fn recovery_is_byte_identical_and_replays_committed_inputs() {
    let ledger = MemLedger::new();
    let mut c = Coordinator::genesis(Box::new(ledger.clone()), config()).unwrap();
    let a = c.open_cohort().unwrap();
    let p1 = c.assign(a).unwrap();
    let p2 = c.assign(a).unwrap();
    let p3 = c.assign(a).unwrap();
    c.complete(completion(p2)).unwrap();
    c.complete(completion(p1)).unwrap();
    c.close_cohort(a).unwrap();
    // Drain before the crash: the recovered instance must re-feed its own
    // fresh dataflow from the ledger, not inherit this one's arrangement.
    c.drain_ready();

    // Simulated process death (all records were synced; nothing is lost).
    ledger.crash();
    let mut r = Coordinator::recover(&ledger).unwrap();
    assert_eq!(r.state_projection().encode(), c.state_projection().encode());
    assert_eq!(r.pending(), vec![p3]);

    // The retried worker completes the SAME pending slot on both timelines.
    c.complete(completion(p3)).unwrap();
    r.complete(completion(p3)).unwrap();
    let cv = c.probe_drive(Revision::new(3)).unwrap();
    let rv = r.probe_drive(Revision::new(3)).unwrap();
    assert_eq!(cv.encode(), rv.encode());
    assert_eq!(r.state_projection().encode(), c.state_projection().encode());
}

#[test]
fn file_and_mem_ledgers_produce_identical_artifacts() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wal");
    let file = revision_coordinator::FileLedger::open(&path).unwrap();
    let mut on_file = Coordinator::genesis(Box::new(file), config()).unwrap();
    let mut on_mem = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();

    for c in [&mut on_file, &mut on_mem] {
        let cohort = c.open_cohort().unwrap();
        let p1 = c.assign(cohort).unwrap();
        let p2 = c.assign(cohort).unwrap();
        c.complete(completion(p2)).unwrap();
        c.complete(completion(p1)).unwrap();
        c.close_cohort(cohort).unwrap();
    }
    let fv = on_file.probe_drive(Revision::new(2)).unwrap();
    let mv = on_mem.probe_drive(Revision::new(2)).unwrap();
    assert_eq!(fv.encode(), mv.encode());
    assert_eq!(
        on_file.state_projection().encode(),
        on_mem.state_projection().encode()
    );

    // And a real process restart from disk agrees byte-wise.
    drop(on_file);
    let reopened = revision_coordinator::FileLedger::open(&path).unwrap();
    let mut recovered = Coordinator::recover(&reopened).unwrap();
    assert_eq!(
        recovered.probe_drive(Revision::new(2)).unwrap().encode(),
        mv.encode()
    );
}

/// The golden determinism gate: the encoded projection of a fixed scenario
/// is asserted byte-wise against a committed golden. A divergence is
/// blocking, not a nit. Refresh (after a reviewed contract change) with:
/// `UPDATE_GOLDEN=1 cargo test -p revision-coordinator --test coordinator_flow`.
#[test]
fn golden_encoded_projection() {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    let a = c.open_cohort().unwrap();
    let p1 = c.assign(a).unwrap();
    let p2 = c.assign(a).unwrap();
    let b = c.open_cohort().unwrap();
    let p3 = c.assign(b).unwrap();
    c.complete(completion(p2)).unwrap();
    c.complete(completion(p3)).unwrap();
    c.complete(completion(p1)).unwrap();
    c.close_cohort(a).unwrap();
    c.close_cohort(b).unwrap();

    let view = c.probe_drive(Revision::new(3)).unwrap();
    let got = format!(
        "{}\n{}\n",
        String::from_utf8(view.encode()).unwrap(),
        String::from_utf8(c.state_projection().encode()).unwrap()
    );

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/goldens/projection.json");
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(path).expect("committed golden exists");
    assert_eq!(got, want, "encoded projection drifted from the golden");
}
