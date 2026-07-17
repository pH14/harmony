// SPDX-License-Identifier: AGPL-3.0-or-later
//! M1 crash-recovery gate: a `proptest-state-machine` model that kills the
//! coordinator at every await point and recovers.
//!
//! Transitions interleave cohort opens/closes, assigns, completions,
//! idempotent worker retries, aborts, and two kinds of kill:
//!
//! - **`Crash`** — clean process death between operations: the unsynced
//!   ledger tail (empty here, since every coordinator persist syncs before
//!   returning) is dropped and the coordinator is rebuilt via `recover`.
//! - **`Faulted*`** — death *inside* an operation, at each await point of
//!   the persist path: before the append (`MemFault::Append`) or between
//!   the append and the fsync barrier (`MemFault::Sync`). The operation
//!   fails, the handle is poisoned (an unrecoverable control failure), the
//!   staged-but-unsynced record dies with the process, and recovery
//!   proceeds from the durable prefix — the slot is retried, never skipped.
//!
//! After every transition the SUT's durable-state projection must equal the
//! reference model's byte-for-byte. After every recovery, the recovered
//! coordinator is additionally compared byte-for-byte — projection AND
//! probe-drive artifacts — against a **never-crashed twin** replaying the
//! same durable op log on a fresh ledger (the M1 acceptance gate).

use proptest::prelude::*;
use proptest::strategy::Union;
use proptest::test_runner::Config;
use proptest_state_machine::{ReferenceStateMachine, StateMachineTest, prop_state_machine};
use revision_coordinator::{
    CampaignConfigId, CohortId, Completion, CoordError, Coordinator, EvidenceBatchId, MemFault,
    MemLedger, PendingProposal, ProposalId, Revision, StateProjection, TerminalRecord,
};

fn config() -> CampaignConfigId {
    CampaignConfigId::digest(b"tests/crash_recovery")
}

fn batch(rev: u64) -> EvidenceBatchId {
    EvidenceBatchId::digest(&rev.to_le_bytes())
}

fn terminal(rev: u64) -> TerminalRecord {
    TerminalRecord {
        moment: 1_000 + rev,
        work: 10 * rev,
    }
}

/// The completion a (possibly retried) worker deterministically produces
/// for proposal index `i` (0-based; revision `i + 1`).
fn completion(i: usize) -> Completion {
    let rev = i as u64 + 1;
    Completion {
        proposal: ProposalId::new(rev),
        batch: batch(rev),
        terminal: terminal(rev),
    }
}

// ---------------------------------------------------------------------------
// Reference model
// ---------------------------------------------------------------------------

/// A durable, successful operation — the log the never-crashed twin replays.
#[derive(Clone, Debug)]
enum Op {
    Open,
    Close(usize),
    Assign(usize),
    Complete(usize),
    Retry(usize),
    Abort,
}

#[derive(Clone, Debug, Default)]
struct RefCohort {
    view: u64,
    closed: bool,
}

#[derive(Clone, Debug)]
struct RefProposal {
    cohort: usize,
    committed: bool,
}

/// Pure model of the coordinator's durable-derived state. Cohort index `i`
/// is `CohortId(i + 1)`; proposal index `i` is `ProposalId(i + 1)` with
/// `Revision(i + 1)` (the dense seeded mint order).
#[derive(Clone, Debug, Default)]
struct RefState {
    cohorts: Vec<RefCohort>,
    proposals: Vec<RefProposal>,
    aborted: bool,
    /// Durable op log for the never-crashed twin.
    log: Vec<Op>,
}

impl RefState {
    fn members(&self, cohort: usize) -> Vec<usize> {
        (0..self.proposals.len())
            .filter(|&i| self.proposals[i].cohort == cohort)
            .collect()
    }

    fn cohort_done(&self, cohort: usize) -> bool {
        self.cohorts[cohort].closed
            && self
                .members(cohort)
                .iter()
                .all(|&i| self.proposals[i].committed)
    }

    /// The cohort barrier is clear: every existing cohort is closed and
    /// fully committed, so a new cohort may open.
    fn barrier_clear(&self) -> bool {
        (0..self.cohorts.len()).all(|c| self.cohort_done(c))
    }

    fn contiguous(&self) -> u64 {
        let mut n = 0;
        while n < self.proposals.len() && self.proposals[n].committed {
            n += 1;
        }
        n as u64
    }

    fn visible(&self) -> u64 {
        let mut v = 0;
        while v < self.proposals.len() && self.cohort_done(self.proposals[v].cohort) {
            v += 1;
        }
        v as u64
    }

    /// The projection the SUT must match byte-for-byte.
    fn projection(&self) -> StateProjection {
        StateProjection {
            config: config(),
            aborted: self.aborted.then(|| "model abort".to_owned()),
            next_proposal: self.proposals.len() as u64 + 1,
            next_revision: self.proposals.len() as u64 + 1,
            next_cohort: self.cohorts.len() as u64 + 1,
            cohorts: (0..self.cohorts.len())
                .map(|ci| {
                    (
                        CohortId::new(ci as u64 + 1),
                        Revision::new(self.cohorts[ci].view),
                        self.cohorts[ci].closed,
                        self.members(ci)
                            .iter()
                            .map(|&i| Revision::new(i as u64 + 1))
                            .collect(),
                    )
                })
                .collect(),
            commits: (0..self.proposals.len())
                .filter(|&i| self.proposals[i].committed)
                .map(|i| {
                    let rev = i as u64 + 1;
                    (
                        Revision::new(rev),
                        ProposalId::new(rev),
                        batch(rev),
                        terminal(rev),
                    )
                })
                .collect(),
            committed_frontier: Revision::new(self.contiguous()),
            visible_frontier: Revision::new(self.visible()),
            pending: (0..self.proposals.len())
                .filter(|&i| !self.proposals[i].committed)
                .map(|i| PendingProposal {
                    proposal: ProposalId::new(i as u64 + 1),
                    revision: Revision::new(i as u64 + 1),
                    cohort: CohortId::new(self.proposals[i].cohort as u64 + 1),
                })
                .collect(),
        }
    }
}

/// The operation a fault is injected into (kill-at-every-await-point: the
/// same op set as [`Op`], each with both fault points).
#[derive(Clone, Debug)]
enum FaultOp {
    Open,
    Close(usize),
    Assign(usize),
    Complete(usize),
    Abort,
}

#[derive(Clone, Debug)]
enum Transition {
    Open,
    Close(usize),
    Assign(usize),
    Complete(usize),
    /// Idempotent worker retry of an already-committed proposal.
    Retry(usize),
    /// Drain + probe-drive and check the view against the model.
    DrainProbe,
    Abort,
    /// Clean kill between operations, then recovery.
    Crash,
    /// Kill inside an operation at an await point, then recovery.
    Faulted(FaultOp, MemFault),
}

struct RefMachine;

impl ReferenceStateMachine for RefMachine {
    type State = RefState;
    type Transition = Transition;

    fn init_state() -> BoxedStrategy<RefState> {
        Just(RefState::default()).boxed()
    }

    fn transitions(state: &RefState) -> BoxedStrategy<Transition> {
        let open_cohorts: Vec<usize> = (0..state.cohorts.len())
            .filter(|&i| !state.cohorts[i].closed)
            .collect();
        let pending: Vec<usize> = (0..state.proposals.len())
            .filter(|&i| !state.proposals[i].committed)
            .collect();
        let committed: Vec<usize> = (0..state.proposals.len())
            .filter(|&i| state.proposals[i].committed)
            .collect();

        let mut options: Vec<(u32, BoxedStrategy<Transition>)> = vec![
            (2, Just(Transition::DrainProbe).boxed()),
            (2, Just(Transition::Crash).boxed()),
            (1, Just(Transition::Abort).boxed()),
            (
                1,
                prop_oneof![Just(MemFault::Append), Just(MemFault::Sync)]
                    .prop_map(|f| Transition::Faulted(FaultOp::Abort, f))
                    .boxed(),
            ),
        ];
        // The cohort barrier (PR #124 FAM-COHORT): a new cohort may only
        // open once every existing cohort is closed and fully committed.
        if state.barrier_clear() {
            options.push((3, Just(Transition::Open).boxed()));
            options.push((
                1,
                prop_oneof![Just(MemFault::Append), Just(MemFault::Sync)]
                    .prop_map(|f| Transition::Faulted(FaultOp::Open, f))
                    .boxed(),
            ));
        }
        if !open_cohorts.is_empty() {
            let oc = open_cohorts.clone();
            options.push((
                6,
                prop::sample::select(oc.clone())
                    .prop_map(Transition::Assign)
                    .boxed(),
            ));
            options.push((
                3,
                prop::sample::select(oc.clone())
                    .prop_map(Transition::Close)
                    .boxed(),
            ));
            options.push((
                1,
                (
                    prop::sample::select(oc.clone()),
                    prop_oneof![Just(MemFault::Append), Just(MemFault::Sync)],
                )
                    .prop_map(|(i, f)| Transition::Faulted(FaultOp::Assign(i), f))
                    .boxed(),
            ));
            options.push((
                1,
                (
                    prop::sample::select(oc),
                    prop_oneof![Just(MemFault::Append), Just(MemFault::Sync)],
                )
                    .prop_map(|(i, f)| Transition::Faulted(FaultOp::Close(i), f))
                    .boxed(),
            ));
        }
        if !pending.is_empty() {
            options.push((
                6,
                prop::sample::select(pending.clone())
                    .prop_map(Transition::Complete)
                    .boxed(),
            ));
            options.push((
                1,
                (
                    prop::sample::select(pending),
                    prop_oneof![Just(MemFault::Append), Just(MemFault::Sync)],
                )
                    .prop_map(|(i, f)| Transition::Faulted(FaultOp::Complete(i), f))
                    .boxed(),
            ));
        }
        if !committed.is_empty() {
            options.push((
                2,
                prop::sample::select(committed)
                    .prop_map(Transition::Retry)
                    .boxed(),
            ));
        }
        Union::new_weighted(options).boxed()
    }

    fn apply(mut state: RefState, transition: &Transition) -> RefState {
        match transition {
            Transition::Open => {
                state.cohorts.push(RefCohort {
                    view: state.visible(),
                    closed: false,
                });
                state.log.push(Op::Open);
            }
            Transition::Close(i) => {
                state.cohorts[*i].closed = true;
                state.log.push(Op::Close(*i));
            }
            Transition::Assign(i) => {
                state.proposals.push(RefProposal {
                    cohort: *i,
                    committed: false,
                });
                state.log.push(Op::Assign(*i));
            }
            Transition::Complete(i) => {
                state.proposals[*i].committed = true;
                state.log.push(Op::Complete(*i));
            }
            Transition::Retry(i) => {
                state.log.push(Op::Retry(*i));
            }
            Transition::Abort => {
                state.aborted = true;
                state.log.push(Op::Abort);
            }
            // Kills change no durable state: every successful persist was
            // synced, and a faulted op never became durable.
            Transition::DrainProbe | Transition::Crash | Transition::Faulted(..) => {}
        }
        state
    }

    fn preconditions(state: &RefState, transition: &Transition) -> bool {
        let open = |i: &usize| *i < state.cohorts.len() && !state.cohorts[*i].closed;
        let pending = |i: &usize| *i < state.proposals.len() && !state.proposals[*i].committed;
        match transition {
            Transition::DrainProbe | Transition::Crash => true,
            _ if state.aborted => false,
            Transition::Open => state.barrier_clear(),
            Transition::Abort => true,
            Transition::Close(i) | Transition::Assign(i) => open(i),
            Transition::Complete(i) => pending(i),
            Transition::Retry(i) => *i < state.proposals.len() && state.proposals[*i].committed,
            Transition::Faulted(op, _) => match op {
                FaultOp::Open => state.barrier_clear(),
                FaultOp::Abort => true,
                FaultOp::Close(i) | FaultOp::Assign(i) => open(i),
                FaultOp::Complete(i) => pending(i),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// System under test
// ---------------------------------------------------------------------------

struct Sut {
    ledger: MemLedger,
    coord: Coordinator,
}

/// Replay the durable op log on a fresh ledger with no crashes — the twin
/// every recovery must match byte-for-byte.
fn never_crashed_twin(log: &[Op]) -> Coordinator {
    let mut c = Coordinator::genesis(Box::new(MemLedger::new()), config()).unwrap();
    for op in log {
        match op {
            Op::Open => {
                c.open_cohort().unwrap();
            }
            Op::Close(i) => c.close_cohort(CohortId::new(*i as u64 + 1)).unwrap(),
            Op::Assign(i) => {
                c.assign(CohortId::new(*i as u64 + 1)).unwrap();
            }
            Op::Complete(i) | Op::Retry(i) => c.complete(completion(*i)).unwrap(),
            Op::Abort => c.abort("model abort").unwrap(),
        }
    }
    c
}

/// After a kill: rebuild from the durable ledger and assert byte-identical
/// frontier, pending set, and probe artifacts against the never-crashed
/// twin of the same durable op log.
fn recover_and_compare(ledger: &mut MemLedger, ref_state: &RefState) -> Coordinator {
    // Process death: staged-but-unsynced ledger records die with it.
    ledger.crash();
    let mut recovered = Coordinator::recover(&*ledger).unwrap();
    let mut twin = never_crashed_twin(&ref_state.log);
    assert_eq!(
        recovered.state_projection().encode(),
        twin.state_projection().encode(),
        "recovered state diverged from the never-crashed twin"
    );
    if !ref_state.aborted {
        let target = Revision::new(ref_state.visible());
        let a = recovered.probe_drive(target).unwrap();
        let b = twin.probe_drive(target).unwrap();
        assert_eq!(
            a.encode(),
            b.encode(),
            "recovered probe artifacts diverged from the never-crashed twin"
        );
    }
    recovered
}

struct Machine;

impl StateMachineTest for Machine {
    type SystemUnderTest = Sut;
    type Reference = RefMachine;

    fn init_test(_ref_state: &RefState) -> Sut {
        let ledger = MemLedger::new();
        let coord = Coordinator::genesis(Box::new(ledger.clone()), config()).unwrap();
        Sut { ledger, coord }
    }

    fn apply(mut sut: Sut, ref_state: &RefState, transition: Transition) -> Sut {
        match transition {
            Transition::Open => {
                let id = sut.coord.open_cohort().unwrap();
                assert_eq!(id.get() as usize, ref_state.cohorts.len());
            }
            Transition::Close(i) => {
                sut.coord.close_cohort(CohortId::new(i as u64 + 1)).unwrap();
            }
            Transition::Assign(i) => {
                let p = sut.coord.assign(CohortId::new(i as u64 + 1)).unwrap();
                // Dense seeded mint order; the slot is never skipped.
                assert_eq!(p.revision.get() as usize, ref_state.proposals.len());
            }
            Transition::Complete(i) | Transition::Retry(i) => {
                sut.coord.complete(completion(i)).unwrap();
            }
            Transition::DrainProbe => {
                if ref_state.aborted {
                    // No later frontier advancement after an abort.
                    assert_eq!(sut.coord.drain_ready(), vec![]);
                    assert!(matches!(
                        sut.coord.probe_drive(Revision::ZERO),
                        Err(CoordError::Aborted { .. })
                    ));
                } else {
                    // No-frontier-holes at every drain.
                    let before = ref_state.contiguous();
                    for (rev, b) in sut.coord.drain_ready() {
                        assert!(rev.get() <= before);
                        assert_eq!(b, batch(rev.get()), "batch mismatch at {rev:?}");
                    }
                    let target = Revision::new(ref_state.visible());
                    let view = sut.coord.probe_drive(target).unwrap();
                    assert_eq!(view.frontier, target);
                    let expect: Vec<(Revision, EvidenceBatchId)> = (1..=target.get())
                        .map(|r| (Revision::new(r), batch(r)))
                        .collect();
                    assert_eq!(view.rows, expect, "partial-cohort result leaked");
                }
            }
            Transition::Abort => {
                sut.coord.abort("model abort").unwrap();
            }
            Transition::Crash => {
                sut.coord = recover_and_compare(&mut sut.ledger, ref_state);
            }
            Transition::Faulted(op, fault) => {
                sut.ledger.fail_next(fault);
                let err = match &op {
                    FaultOp::Open => sut.coord.open_cohort().map(|_| ()).unwrap_err(),
                    FaultOp::Close(i) => sut
                        .coord
                        .close_cohort(CohortId::new(*i as u64 + 1))
                        .unwrap_err(),
                    FaultOp::Assign(i) => sut
                        .coord
                        .assign(CohortId::new(*i as u64 + 1))
                        .map(|_| ())
                        .unwrap_err(),
                    FaultOp::Complete(i) => sut.coord.complete(completion(*i)).unwrap_err(),
                    FaultOp::Abort => sut.coord.abort("model abort").unwrap_err(),
                };
                assert!(matches!(err, CoordError::Ledger(_)), "{err}");
                // The handle is poisoned: nothing else persists or drains.
                assert!(matches!(sut.coord.open_cohort(), Err(CoordError::Poisoned)));
                assert_eq!(sut.coord.drain_ready(), vec![]);
                // Process death + recovery from the durable prefix.
                sut.coord = recover_and_compare(&mut sut.ledger, ref_state);
            }
        }
        sut
    }

    fn check_invariants(sut: &Sut, ref_state: &RefState) {
        assert_eq!(
            sut.coord.state_projection().encode(),
            ref_state.projection().encode(),
            "durable-state projection diverged from the model"
        );
    }
}

prop_state_machine! {
    #![proptest_config(Config { cases: 256, ..Config::default() })]

    /// Drive 1..24 operations (with kills at every await point) against the
    /// coordinator and the reference model.
    #[test]
    fn crash_recovery_matches_model(sequential 1..24 => Machine);
}

// ---------------------------------------------------------------------------
// File-backed recovery: the same twin comparison over a real on-disk WAL.
// ---------------------------------------------------------------------------

proptest! {
    // fsync-bound (every persist hits the disk barrier), so fewer cases than
    // the in-memory model above; the >=256-case crash gate is
    // `crash_recovery_matches_model`, this adds the real-WAL end-to-end.
    #![proptest_config(Config { cases: 48, ..Config::default() })]

    /// A scenario on the file-backed ledger, killed at an arbitrary point
    /// (process drop, recovery from disk), finishes byte-identical — durable
    /// projection AND probe artifacts — to a never-crashed in-memory twin of
    /// the same seed and completion set.
    #[test]
    fn file_ledger_recovery_matches_twin(
        sizes in prop::collection::vec(1..=3usize, 1..=3),
        seed in any::<u64>(),
        crash_at in 0..64usize,
    ) {
        // Op tape, barrier-shaped (PR #124 FAM-COHORT): each cohort opens,
        // mints, closes, and fully completes (members permuted by the seed)
        // before the next cohort may open.
        let mut log: Vec<Op> = Vec::new();
        let mut s = seed;
        let mut offset = 0usize;
        for (ci, &size) in sizes.iter().enumerate() {
            log.push(Op::Open);
            for _ in 0..size {
                log.push(Op::Assign(ci));
            }
            log.push(Op::Close(ci));
            // Within-cohort completion permutation (splitmix-ish).
            let mut order: Vec<usize> = (offset..offset + size).collect();
            for i in (1..size).rev() {
                s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                order.swap(i, (s >> 33) as usize % (i + 1));
            }
            log.extend(order.iter().map(|&p| Op::Complete(p)));
            offset += size;
        }
        let n = offset;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wal");
        let file = revision_coordinator::FileLedger::open(&path).unwrap();
        let mut coord = Coordinator::genesis(Box::new(file), config()).unwrap();
        let crash_at = crash_at % (log.len() + 1);
        for (k, op) in log.iter().enumerate() {
            if k == crash_at {
                // Process death: drop the coordinator and its ledger handle,
                // then recover from the on-disk WAL.
                drop(coord);
                let reopened = revision_coordinator::FileLedger::open(&path).unwrap();
                coord = Coordinator::recover(&reopened).unwrap();
            }
            match op {
                Op::Open => {
                    coord.open_cohort().unwrap();
                }
                Op::Close(i) => coord.close_cohort(CohortId::new(*i as u64 + 1)).unwrap(),
                Op::Assign(i) => {
                    coord.assign(CohortId::new(*i as u64 + 1)).unwrap();
                }
                Op::Complete(i) | Op::Retry(i) => coord.complete(completion(*i)).unwrap(),
                Op::Abort => coord.abort("model abort").unwrap(),
            }
        }

        let mut twin = never_crashed_twin(&log);
        prop_assert_eq!(
            coord.state_projection().encode(),
            twin.state_projection().encode()
        );
        let target = Revision::new(n as u64);
        let a = coord.probe_drive(target).unwrap();
        let b = twin.probe_drive(target).unwrap();
        prop_assert_eq!(a.encode(), b.encode());
    }
}
