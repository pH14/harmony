// SPDX-License-Identifier: AGPL-3.0-or-later
//! Strict replay validation: `Coordinator::recover` must refuse — with a
//! typed `CorruptLedger` — any durable record stream the coordinator's own
//! writer could not have produced. Each case hand-crafts one protocol
//! violation on an otherwise-valid ledger (scoped-mutants follow-up on the
//! PR #124 batch: the `mint under closed` / `double close` replay guards
//! were previously unexercised).

use revision_coordinator::{
    CampaignConfigId, CohortId, CoordError, Coordinator, EvidenceBatchId, Ledger, LedgerRecord,
    MemLedger, ProposalId, Revision, TerminalRecord,
};

fn config() -> CampaignConfigId {
    CampaignConfigId::digest(b"tests/replay_validation")
}

/// A MemLedger preloaded (and synced) with `records`.
fn ledger_with(records: &[LedgerRecord]) -> MemLedger {
    let mut l = MemLedger::new();
    for r in records {
        l.append(r).unwrap();
    }
    l.sync().unwrap();
    l
}

fn genesis() -> LedgerRecord {
    LedgerRecord::Genesis { config: config() }
}

fn open(c: u64, view: u64) -> LedgerRecord {
    LedgerRecord::CohortOpen {
        cohort: CohortId::new(c),
        view: Revision::new(view),
    }
}

fn mint(p: u64, r: u64, c: u64) -> LedgerRecord {
    LedgerRecord::Proposal {
        proposal: ProposalId::new(p),
        revision: Revision::new(r),
        cohort: CohortId::new(c),
    }
}

fn commit(p: u64, r: u64) -> LedgerRecord {
    LedgerRecord::Commit {
        proposal: ProposalId::new(p),
        revision: Revision::new(r),
        batch: EvidenceBatchId::digest(&r.to_le_bytes()),
        terminal: TerminalRecord {
            moment: 1_000 + r,
            work: 10 * r,
        },
    }
}

fn close(c: u64) -> LedgerRecord {
    LedgerRecord::CohortClose {
        cohort: CohortId::new(c),
    }
}

fn expect_corrupt(records: &[LedgerRecord], what: &str) {
    match Coordinator::recover(&ledger_with(records)) {
        Err(CoordError::CorruptLedger { detail }) => {
            eprintln!("{what}: refused as expected ({detail})");
        }
        Ok(_) => panic!("{what}: recover ACCEPTED a corrupt stream"),
        Err(other) => panic!("{what}: wrong error {other:?}"),
    }
}

#[test]
fn a_valid_stream_recovers() {
    let records = [genesis(), open(1, 0), mint(1, 1, 1), commit(1, 1), close(1)];
    let c = Coordinator::recover(&ledger_with(&records)).unwrap();
    assert_eq!(c.visible_frontier(), Revision::new(1));
}

#[test]
fn mint_under_a_closed_cohort_is_corrupt() {
    expect_corrupt(
        &[genesis(), open(1, 0), close(1), mint(1, 1, 1)],
        "mint under closed cohort",
    );
}

#[test]
fn double_close_is_corrupt() {
    expect_corrupt(&[genesis(), open(1, 0), close(1), close(1)], "double close");
}

#[test]
fn open_across_the_barrier_is_corrupt() {
    // Cohort 1 still open (not closed, member pending): cohort 2 cannot
    // have been opened by our writer.
    expect_corrupt(
        &[genesis(), open(1, 0), mint(1, 1, 1), open(2, 0)],
        "open across the barrier (cohort 1 open)",
    );
    // Cohort 1 closed but not fully committed: same refusal.
    expect_corrupt(
        &[genesis(), open(1, 0), mint(1, 1, 1), close(1), open(2, 0)],
        "open across the barrier (cohort 1 closed, pending)",
    );
}

#[test]
fn duplicate_genesis_and_missing_genesis_are_refused() {
    expect_corrupt(&[genesis(), genesis()], "duplicate genesis");
    expect_corrupt(&[open(1, 0)], "first record not genesis");
}

#[test]
fn non_dense_ids_and_mismatched_commit_are_corrupt() {
    expect_corrupt(&[genesis(), open(2, 0)], "non-dense cohort id");
    expect_corrupt(
        &[genesis(), open(1, 0), mint(2, 1, 1)],
        "non-dense proposal",
    );
    expect_corrupt(
        &[genesis(), open(1, 0), mint(1, 2, 1)],
        "non-dense revision",
    );
    expect_corrupt(
        &[genesis(), open(1, 0), mint(1, 1, 1), commit(1, 2)],
        "commit revision != assigned revision",
    );
    expect_corrupt(
        &[
            genesis(),
            open(1, 0),
            mint(1, 1, 1),
            commit(1, 1),
            commit(1, 1),
        ],
        "double commit",
    );
    expect_corrupt(
        &[genesis(), open(1, 1)],
        "recorded view != visible frontier",
    );
}

#[test]
fn records_after_an_abort_are_corrupt() {
    expect_corrupt(
        &[
            genesis(),
            LedgerRecord::Abort {
                reason: "x".to_owned(),
            },
            open(1, 0),
        ],
        "record after abort",
    );
}
