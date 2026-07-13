// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — two error categories.
//!
//! A `MachineError` (backend/transport failure) aborts the step loudly and is
//! never recorded as a `Bug`; only `StopReason::Crash`/`Assertion` become `Bug`s.
//! The two result categories are never confused (`docs/DISSONANCE.md`).

mod common;

use common::{ToyCodec, ToyMachine, pin_composition, seed_composition};
use explorer::{Explorer, MachineError, StopConditions, StopMask};

/// A backend fault mid-campaign aborts `explore` with the `MachineError`, not a
/// bug — the error propagates out of the loop loudly.
#[test]
fn backend_fault_aborts_explore_loudly() {
    let machine = ToyMachine::new().fail_after(5);
    let mut ex = Explorer::new(machine, Box::new(ToyCodec), pin_composition(), 1).unwrap();

    let result = ex.explore(100);
    match result {
        Err(MachineError::Transport(_)) => {}
        other => panic!("expected a transport MachineError, got {other:?}"),
    }
}

/// The same fault inside `step` surfaces as `Err`, never `Ok(Some(bug))`
/// — a transport failure is categorically not a bug.
#[test]
fn backend_fault_is_never_a_bug() {
    let machine = ToyMachine::new().fail_after(1);
    let mut ex = Explorer::new(machine, Box::new(ToyCodec), pin_composition(), 2).unwrap();

    // The very first run fails; the step returns Err, not a bug.
    assert!(matches!(ex.step(), Err(MachineError::Transport(_))));
}

/// A clean, bug-free campaign (no class surfaces, the seed never trips a crash
/// condition) reports zero bugs — non-crash/assertion stops never become bugs.
#[test]
fn quiescent_campaign_reports_no_bugs() {
    let mut ex =
        Explorer::new(ToyMachine::new(), Box::new(ToyCodec), seed_composition(), 0).unwrap();
    // No classes surface and no snapshot fork — pure quiescent seed runs.
    ex.set_stop_conditions(StopConditions {
        deadline: Some(explorer::Moment(30)), // stop before any crash/assert index
        on: StopMask::NONE,
    });

    let bugs = ex.explore(64).unwrap();
    assert!(bugs.is_empty(), "deadline/quiescent stops are not bugs");
}

/// `Explorer::new` returns `Err` (never panics) when the initial genesis snapshot
/// cannot be taken.
#[test]
fn new_errors_when_genesis_snapshot_fails() {
    let machine = ToyMachine::new().fail_snapshot();
    let r = Explorer::new(machine, Box::new(ToyCodec), pin_composition(), 0);
    assert!(matches!(r.err(), Some(MachineError::NotQuiescent)));
}
