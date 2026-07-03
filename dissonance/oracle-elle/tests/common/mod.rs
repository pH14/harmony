// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test scaffolding: builders that plant an operation history into a
//! [`RunTrace`]'s link-tier `events`, and a verb-counting mock [`Machine`] that
//! witnesses the offline property (judging touches no guest).
#![allow(dead_code)]

use explorer::{
    Answer, Environment, GuestEvent, Machine, MachineError, Moment, RunTrace, SnapId,
    StopConditions, StopReason, VTime, Value,
};

// ---------------------------------------------------------------------------
// Planted-history builders (over link-tier events)
// ---------------------------------------------------------------------------

/// A register write of value `v` by `(session, txn)` on `key`, at `at`.
pub fn write(at: u64, session: u64, txn: u64, key: &str, v: i64) -> (Moment, GuestEvent) {
    op_event(at, session, txn, key, "W", Value::Int(v))
}

/// An append of value `v` by `(session, txn)` on `key`, at `at`.
pub fn append(at: u64, session: u64, txn: u64, key: &str, v: i64) -> (Moment, GuestEvent) {
    op_event(at, session, txn, key, "A", Value::Int(v))
}

/// A read by `(session, txn)` on `key` observing the list `vs`, at `at`.
pub fn read(at: u64, session: u64, txn: u64, key: &str, vs: &[i64]) -> (Moment, GuestEvent) {
    let list = vs.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
    op_event(at, session, txn, key, "R", Value::Str(list))
}

fn op_event(
    at: u64,
    session: u64,
    txn: u64,
    key: &str,
    verb: &str,
    payload: Value,
) -> (Moment, GuestEvent) {
    let attrs = [
        ("s".to_string(), Value::UInt(session)),
        ("t".to_string(), Value::UInt(txn)),
        ("k".to_string(), Value::Str(key.to_string())),
        (verb.to_string(), payload),
    ]
    .into_iter()
    .collect();
    (
        Moment(at),
        GuestEvent {
            kind: "op".to_string(),
            attrs,
        },
    )
}

/// A commit of `txn` at `at`.
pub fn commit(at: u64, txn: u64) -> (Moment, GuestEvent) {
    lifecycle(at, "commit", txn)
}

/// An abort of `txn` at `at`.
pub fn abort(at: u64, txn: u64) -> (Moment, GuestEvent) {
    lifecycle(at, "abort", txn)
}

fn lifecycle(at: u64, kind: &str, txn: u64) -> (Moment, GuestEvent) {
    (
        Moment(at),
        GuestEvent {
            kind: kind.to_string(),
            attrs: [("t".to_string(), Value::UInt(txn))].into_iter().collect(),
        },
    )
}

/// A [`RunTrace`] carrying `events`, a quiescent terminal, and the given
/// genesis-complete reproducer seed (so a corpus of runs has distinct
/// reproducers).
pub fn trace(events: Vec<(Moment, GuestEvent)>, env_seed: u8) -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent {
            vtime: VTime(events.last().map(|(m, _)| m.0 + 10).unwrap_or(10)),
        },
        env: Environment {
            blob_version: 1,
            bytes: vec![env_seed],
        },
        coverage: None,
        events,
        records: Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// The verb-counting mock machine (the offline-property witness)
// ---------------------------------------------------------------------------

/// A [`Machine`] that counts every driver-verb call. The offline gate holds one
/// alongside the judging path and asserts its count stays **zero**: an
/// [`Oracle`](explorer::Oracle) judges a `RunTrace` with no `Machine` in hand,
/// so re-judging a stored corpus costs no VM time.
#[derive(Debug, Default)]
pub struct CountingMachine {
    calls: u64,
    coverage: Vec<u8>,
}

impl CountingMachine {
    /// A fresh witness machine.
    pub fn new() -> Self {
        Self {
            calls: 0,
            coverage: Vec::new(),
        }
    }

    /// How many driver verbs have been invoked.
    pub fn calls(&self) -> u64 {
        self.calls
    }
}

impl Machine for CountingMachine {
    fn branch(&mut self, _snap: SnapId, _env: &Environment) -> Result<(), MachineError> {
        self.calls += 1;
        Ok(())
    }
    fn replay(&mut self, _snap: SnapId) -> Result<(), MachineError> {
        self.calls += 1;
        Ok(())
    }
    fn run(
        &mut self,
        _until: &StopConditions,
        _resolve: Option<&Answer>,
    ) -> Result<StopReason, MachineError> {
        self.calls += 1;
        Ok(StopReason::Quiescent { vtime: VTime(0) })
    }
    fn snapshot(&mut self) -> Result<SnapId, MachineError> {
        self.calls += 1;
        Ok(SnapId(0))
    }
    fn drop_snap(&mut self, _snap: SnapId) -> Result<(), MachineError> {
        self.calls += 1;
        Ok(())
    }
    fn hash(&mut self) -> Result<[u8; 32], MachineError> {
        self.calls += 1;
        Ok([0u8; 32])
    }
    fn coverage(&self) -> &[u8] {
        &self.coverage
    }
    fn recorded_env(&self) -> Result<Environment, MachineError> {
        Ok(Environment {
            blob_version: 1,
            bytes: Vec::new(),
        })
    }
}
