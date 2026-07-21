// SPDX-License-Identifier: AGPL-3.0-or-later
//! Production-relations tests (task 132, `hm-e6q`): staged evidence rows
//! flow through the committed-input drain into the in-host Differential
//! relations, and the materialized observation/cell/occupancy views agree
//! with a direct recomputation oracle — including lineage-composed prefixes,
//! all four reduce ops, occupancy domination, and the staging/read
//! discipline errors.

use revision_coordinator::{
    CampaignConfigId, Completion, CoordError, Coordinator, CutRow, EntryCommitRow, EvidenceBatchId,
    EvidenceRows, LineageRow, MemLedger, PendingProposal, PointRow, ReduceOp, ReducedRow, Revision,
    SealRow, StateEventRow, TerminalRecord, canonical_cell,
};

fn coordinator() -> Coordinator {
    Coordinator::genesis(
        Box::new(MemLedger::new()),
        CampaignConfigId::digest(b"relations-test"),
    )
    .expect("genesis")
}

fn obs(tag: u8) -> Vec<u8> {
    vec![0x01, tag]
}

fn ev(pos: u64, moment: u64, obs_key: &[u8], value: u64) -> StateEventRow {
    StateEventRow {
        pos,
        moment,
        obs: obs_key.to_vec(),
        value,
    }
}

/// Assign one proposal in its own cohort, stage its rows, and commit it.
fn commit(coord: &mut Coordinator, rows: EvidenceRows) -> PendingProposal {
    let cohort = coord.open_cohort().expect("cohort");
    let p = coord.assign(cohort).expect("assign");
    coord.stage_evidence(p.proposal, rows).expect("stage");
    coord
        .complete(Completion {
            proposal: p.proposal,
            batch: EvidenceBatchId::digest(&p.revision.get().to_le_bytes()),
            terminal: TerminalRecord {
                moment: p.revision.get(),
                work: 1,
            },
        })
        .expect("complete");
    coord.close_cohort(cohort).expect("close");
    p
}

/// All four base ops reduce per their declared semantics at a half-open
/// seal cut, and the cell is the canonical projection of the reduced map.
#[test]
fn base_ops_reduce_at_the_half_open_cut() {
    let mut coord = coordinator();
    let (set, max, min, acc) = (obs(1), obs(2), obs(3), obs(4));
    let rows = EvidenceRows {
        rollout: 1,
        lineage: None,
        declares: vec![
            (set.clone(), ReduceOp::Set),
            (max.clone(), ReduceOp::Max),
            (min.clone(), ReduceOp::Min),
            (acc.clone(), ReduceOp::Accumulate),
        ],
        events: vec![
            ev(0, 10, &set, 3),
            ev(1, 11, &max, 4),
            ev(2, 12, &min, 9),
            ev(3, 13, &acc, 7),
            ev(4, 14, &set, 8),
            ev(5, 15, &max, 2),
            ev(6, 16, &min, 5),
            ev(7, 17, &acc, 7),
            ev(8, 18, &acc, 1),
            // Excluded by the half-open cut below (pos >= 9 is out).
            ev(9, 30, &set, 99),
        ],
        obs_cuts: vec![CutRow {
            moment: 12,
            count: 3,
        }],
        seal: Some(SealRow {
            seal: 2,
            cut: CutRow {
                moment: 18,
                count: 9,
            },
        }),
        entry: Some(EntryCommitRow {
            entry: 2,
            quality: 18,
        }),
    };
    let p = commit(&mut coord, rows);
    let view = coord.probe_drive(p.revision).expect("drive");
    let m = coord.materialized(view.frontier).expect("materialized");

    // Seal-point reductions: latest set, running max/min, distinct set —
    // the excluded pos-9 write never reaches them.
    let seal = PointRow::Seal(2);
    let get = |k: &[u8]| {
        m.observations
            .iter()
            .find(|((r, pt, o), _)| *r == 1 && *pt == seal && o == k)
            .map(|(_, red)| red.clone())
    };
    assert_eq!(get(&set), Some(ReducedRow::Scalar(8)));
    assert_eq!(get(&max), Some(ReducedRow::Scalar(4)));
    assert_eq!(get(&min), Some(ReducedRow::Scalar(5)));
    assert_eq!(get(&acc), Some(ReducedRow::Accumulated(vec![1, 7])));

    // Provisional-cut reductions include only the first three events.
    let cut = PointRow::Cut(3);
    let at_cut: Vec<_> = m
        .observations
        .iter()
        .filter(|((r, pt, _), _)| *r == 1 && *pt == cut)
        .map(|((_, _, o), red)| (o.clone(), red.clone()))
        .collect();
    assert_eq!(
        at_cut,
        vec![
            (set.clone(), ReducedRow::Scalar(3)),
            (max.clone(), ReducedRow::Scalar(4)),
            (min.clone(), ReducedRow::Scalar(9)),
        ]
    );

    // The seal cell is the canonical projection of its reduced map.
    let expect_pairs = vec![
        (set.clone(), ReducedRow::Scalar(8)),
        (max.clone(), ReducedRow::Scalar(4)),
        (min.clone(), ReducedRow::Scalar(5)),
        (acc.clone(), ReducedRow::Accumulated(vec![1, 7])),
    ];
    let seal_cut = CutRow {
        moment: 18,
        count: 9,
    };
    let cell = m.cell_at(1, seal).expect("seal cell");
    assert_eq!(*cell, canonical_cell(seal_cut, &expect_pairs));

    // The committed entry occupies that cell.
    assert_eq!(m.occupant(cell), Some(2));
}

/// Lineage composition: a child's seal reduces over the ancestor prefix
/// (through the fork cut) plus its own suffix — `set` overridden by the
/// child, `accumulate` unioned, ancestor-only state inherited.
#[test]
fn lineage_composes_the_ancestor_prefix() {
    let mut coord = coordinator();
    let (a, b, c) = (obs(1), obs(2), obs(3));
    // Parent rollout 1: positions 0..4.
    let p1 = commit(
        &mut coord,
        EvidenceRows {
            rollout: 1,
            lineage: None,
            declares: vec![
                (a.clone(), ReduceOp::Set),
                (b.clone(), ReduceOp::Accumulate),
                (c.clone(), ReduceOp::Max),
            ],
            events: vec![
                ev(0, 10, &a, 1),
                ev(1, 11, &b, 5),
                ev(2, 12, &c, 40),
                // Beyond the fork cut (pos >= 3): must NOT reach the child.
                ev(3, 20, &a, 77),
            ],
            obs_cuts: vec![],
            seal: None,
            entry: None,
        },
    );
    // Child rollout 2 branched at (moment 12, count 3): its own suffix is
    // positions 3.., overriding `a` and extending `b`.
    let p2 = commit(
        &mut coord,
        EvidenceRows {
            rollout: 2,
            lineage: Some(LineageRow {
                parent: 1,
                cut: CutRow {
                    moment: 12,
                    count: 3,
                },
            }),
            declares: vec![],
            events: vec![ev(3, 13, &a, 2), ev(4, 14, &b, 6)],
            obs_cuts: vec![],
            seal: Some(SealRow {
                seal: 9,
                cut: CutRow {
                    moment: 14,
                    count: 5,
                },
            }),
            entry: Some(EntryCommitRow {
                entry: 9,
                quality: 14,
            }),
        },
    );
    assert!(p2.revision > p1.revision);
    let view = coord.probe_drive(p2.revision).expect("drive");
    let m = coord.materialized(view.frontier).expect("materialized");

    let seal = PointRow::Seal(9);
    let get = |k: &[u8]| {
        m.observations
            .iter()
            .find(|((r, pt, o), _)| *r == 2 && *pt == seal && o == k)
            .map(|(_, red)| red.clone())
    };
    // `a`: parent wrote 1 (pos 0), child overrode with 2 (pos 3); the
    // parent's post-fork 77 (pos 3 on the parent) is not inherited.
    assert_eq!(get(&a), Some(ReducedRow::Scalar(2)));
    // `b`: union of parent {5} and child {6}.
    assert_eq!(get(&b), Some(ReducedRow::Accumulated(vec![5, 6])));
    // `c`: ancestor-only state is inherited.
    assert_eq!(get(&c), Some(ReducedRow::Scalar(40)));
}

/// Occupancy domination: same cell, strictly higher quality replaces; equal
/// quality keeps the earlier (lower-id) entry.
#[test]
fn occupancy_keeps_the_best_entry_per_cell() {
    let mut coord = coordinator();
    let a = obs(1);
    // Three rollouts, all reducing to the same state {a: 5} at their seals,
    // with qualities 10, 30, 30.
    let mut last = None;
    for (rollout, seal, entry, quality, moment) in [
        (1u64, 1u64, 1u64, 10u64, 10u64),
        (2, 2, 2, 30, 30),
        (3, 3, 3, 30, 30),
    ] {
        let p = commit(
            &mut coord,
            EvidenceRows {
                rollout,
                lineage: None,
                declares: if rollout == 1 {
                    vec![(a.clone(), ReduceOp::Set)]
                } else {
                    vec![]
                },
                events: vec![ev(0, moment, &a, 5)],
                obs_cuts: vec![],
                seal: Some(SealRow {
                    seal,
                    cut: CutRow { moment, count: 1 },
                }),
                entry: Some(EntryCommitRow { entry, quality }),
            },
        );
        last = Some(p.revision);
    }
    let view = coord
        .probe_drive(last.expect("three commits"))
        .expect("drive");
    let m = coord.materialized(view.frontier).expect("materialized");
    // One cell; entry 2 dominates (higher quality than 1, earlier than 3).
    assert_eq!(m.occupancy.len(), 1);
    assert_eq!(m.occupancy[0].1, 2);
}

/// The staging discipline errors are typed and loud: divergent restage,
/// post-drain staging, declaration conflicts, and late projection installs.
#[test]
fn staging_discipline_is_loud() {
    let mut coord = coordinator();
    let a = obs(1);
    let rows = EvidenceRows {
        rollout: 1,
        lineage: None,
        declares: vec![(a.clone(), ReduceOp::Set)],
        events: vec![ev(0, 10, &a, 3)],
        obs_cuts: vec![],
        seal: None,
        entry: None,
    };
    let cohort = coord.open_cohort().expect("cohort");
    let p = coord.assign(cohort).expect("assign");
    coord
        .stage_evidence(p.proposal, rows.clone())
        .expect("stage");
    // Byte-identical restage: absorbed.
    coord
        .stage_evidence(p.proposal, rows.clone())
        .expect("identical restage absorbs");
    // Divergent restage: loud.
    let mut divergent = rows.clone();
    divergent.events[0].value = 4;
    assert!(matches!(
        coord.stage_evidence(p.proposal, divergent),
        Err(CoordError::StageConflict { .. })
    ));
    // Declaring the same identity under a different op: loud.
    let p2 = coord.assign(cohort).expect("assign 2");
    let conflicted = EvidenceRows {
        rollout: 2,
        declares: vec![(a.clone(), ReduceOp::Max)],
        ..EvidenceRows::default()
    };
    assert!(matches!(
        coord.stage_evidence(p2.proposal, conflicted),
        Err(CoordError::DeclarationConflict { .. })
    ));
    // Unknown proposal: loud.
    assert!(matches!(
        coord.stage_evidence(
            revision_coordinator::ProposalId::new(99),
            EvidenceRows::default()
        ),
        Err(CoordError::UnknownProposal(_))
    ));
    // Drain p1+p2, then staging p2 again is too late.
    for prop in [p.proposal, p2.proposal] {
        coord
            .complete(Completion {
                proposal: prop,
                batch: EvidenceBatchId::digest(&prop.get().to_le_bytes()),
                terminal: TerminalRecord { moment: 1, work: 1 },
            })
            .expect("complete");
    }
    coord.close_cohort(cohort).expect("close");
    coord.probe_drive(p2.revision).expect("drive");
    assert!(matches!(
        coord.stage_evidence(p2.proposal, EvidenceRows::default()),
        Err(CoordError::StagedTooLate { .. })
    ));
    // The projection cannot be swapped once inputs were fed.
    assert!(matches!(
        coord.set_cell_projection(std::rc::Rc::new(canonical_cell)),
        Err(CoordError::ProjectionTooLate { .. })
    ));
}

/// The materialized read discipline: reading past the driven/visible
/// frontier is a loud stall, never a partial view.
#[test]
fn materialized_respects_the_probe_barrier() {
    let mut coord = coordinator();
    // Nothing driven yet: even revision 1 is unreadable.
    assert!(matches!(
        coord.materialized(Revision::new(1)),
        Err(CoordError::FrontierStalled { .. })
    ));
    let p = commit(&mut coord, EvidenceRows::default());
    let view = coord.probe_drive(p.revision).expect("drive");
    // Readable at the frontier, stalled past it.
    coord.materialized(view.frontier).expect("readable");
    assert!(matches!(
        coord.materialized(Revision::new(view.frontier.get() + 1)),
        Err(CoordError::FrontierStalled { .. })
    ));
}

/// A custom cell projection installed before the first drain governs the
/// cells view (and the occupancy keyed off it).
#[test]
fn custom_projection_governs_cells() {
    let mut coord = coordinator();
    coord
        .set_cell_projection(std::rc::Rc::new(|cut: CutRow, _obs: &[_]| {
            // A moment-keyed projection (unlike the moment-blind default).
            cut.moment.to_le_bytes().to_vec()
        }))
        .expect("projection installs before the first drain");
    let a = obs(1);
    let p = commit(
        &mut coord,
        EvidenceRows {
            rollout: 1,
            lineage: None,
            declares: vec![(a.clone(), ReduceOp::Set)],
            events: vec![ev(0, 10, &a, 3)],
            obs_cuts: vec![],
            seal: Some(SealRow {
                seal: 1,
                cut: CutRow {
                    moment: 10,
                    count: 1,
                },
            }),
            entry: Some(EntryCommitRow {
                entry: 1,
                quality: 10,
            }),
        },
    );
    let view = coord.probe_drive(p.revision).expect("drive");
    let m = coord.materialized(view.frontier).expect("materialized");
    let cell = m.cell_at(1, PointRow::Seal(1)).expect("cell");
    assert_eq!(*cell, 10u64.to_le_bytes().to_vec());
    assert_eq!(m.occupant(cell), Some(1));
}

/// Same committed inputs ⇒ byte-identical materialized views, including
/// across a crash + recovery with re-staged evidence (restart replays
/// committed ledger inputs, never a live arrangement).
#[test]
fn views_are_deterministic_and_survive_recovery() {
    let build = |coord: &mut Coordinator| -> Vec<(EvidenceRows, PendingProposal)> {
        let a = obs(1);
        let rows1 = EvidenceRows {
            rollout: 1,
            lineage: None,
            declares: vec![(a.clone(), ReduceOp::Set)],
            events: vec![ev(0, 10, &a, 3), ev(1, 12, &a, 5)],
            obs_cuts: vec![CutRow {
                moment: 10,
                count: 1,
            }],
            seal: Some(SealRow {
                seal: 1,
                cut: CutRow {
                    moment: 12,
                    count: 2,
                },
            }),
            entry: Some(EntryCommitRow {
                entry: 1,
                quality: 12,
            }),
        };
        let p1 = commit(coord, rows1.clone());
        let rows2 = EvidenceRows {
            rollout: 2,
            lineage: Some(LineageRow {
                parent: 1,
                cut: CutRow {
                    moment: 12,
                    count: 2,
                },
            }),
            declares: vec![],
            events: vec![ev(2, 13, &a, 9)],
            obs_cuts: vec![],
            seal: Some(SealRow {
                seal: 2,
                cut: CutRow {
                    moment: 13,
                    count: 3,
                },
            }),
            entry: Some(EntryCommitRow {
                entry: 2,
                quality: 13,
            }),
        };
        let p2 = commit(coord, rows2.clone());
        vec![(rows1, p1), (rows2, p2)]
    };

    let mut live = coordinator();
    let staged = build(&mut live);
    let last = staged.last().expect("two commits").1.revision;
    let view = live.probe_drive(last).expect("drive");
    let live_views = live.materialized(view.frontier).expect("materialized");

    // Recover from the durable coordinator ledger; re-stage the evidence
    // rows (the controller's job, from its own durable evidence ledger) and
    // re-drive.
    let ledger = MemLedger::new();
    let mut original = Coordinator::genesis(
        Box::new(ledger.clone()),
        CampaignConfigId::digest(b"relations-test"),
    )
    .expect("genesis");
    let staged2 = build(&mut original);
    drop(original);
    let mut recovered = Coordinator::recover(&ledger).expect("recover");
    let committed = recovered.committed_inputs();
    assert_eq!(committed.len(), 2);
    for (rows, p) in &staged2 {
        recovered
            .stage_evidence(p.proposal, rows.clone())
            .expect("re-stage");
    }
    let view2 = recovered.probe_drive(last).expect("drive recovered");
    assert_eq!(view2.encode(), view.encode(), "drained views agree");
    let rec_views = recovered
        .materialized(view2.frontier)
        .expect("materialized");
    assert_eq!(
        serde_json::to_vec(&live_views).expect("views encode"),
        serde_json::to_vec(&rec_views).expect("views encode"),
        "materialized views are byte-identical across recovery"
    );
    let _ = ledger;
}
