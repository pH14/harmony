// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — **config round-trip.** `parse → serialize → parse` is identity, on
//! the normative example and on arbitrary valid sets; each malformed class
//! (unknown role, duplicate name, bad type, unknown `during`) yields its typed
//! error, never a panic.

mod common;

use common::arb_signal_set;
use matcher::{MatchError, SignalSet};
use proptest::prelude::*;

/// The normative example from the task spec, verbatim.
const NORMATIVE: &str = r#"{ "signals": [
  { "name": "leader.won",   "role": "sometimes",
    "match": { "kind": "span", "attr": { "name": "raft.leader_election", "outcome": "won" } } },
  { "name": "wal.lsn",      "role": "state_max",
    "match": { "kind": "span", "attr": { "name": "wal.replay" }, "attr_max": "lsn" } },
  { "name": "pg.ready",     "role": "cell",
    "match": { "kind": "log",  "attr": { "msg": "database system is ready*" } } },
  { "name": "commit.clean", "role": "never",
    "match": { "kind": "span", "attr": { "name": "txn.commit", "error": "true" }, "during": "no_faults" } }
] }"#;

#[test]
fn normative_example_round_trips() {
    let a = SignalSet::from_json(NORMATIVE).expect("normative example parses");
    let json = a.to_json().expect("serializes");
    let b = SignalSet::from_json(&json).expect("re-parses");
    assert_eq!(a, b, "parse → serialize → parse is identity");
}

#[test]
fn malformed_classes_are_typed_errors() {
    // Unknown role.
    assert!(matches!(
        SignalSet::from_json(
            r#"{ "signals": [ { "name": "x", "role": "always", "match": { "kind": "log" } } ] }"#
        ),
        Err(MatchError::UnknownRole(_))
    ));
    // Duplicate name.
    assert!(matches!(
        SignalSet::from_json(
            r#"{ "signals": [
                { "name": "d", "role": "cell", "match": { "kind": "log" } },
                { "name": "d", "role": "never", "match": { "kind": "log" } } ] }"#
        ),
        Err(MatchError::DuplicateName(_))
    ));
    // Bad type: attr value is a number, not a string.
    assert!(matches!(
        SignalSet::from_json(
            r#"{ "signals": [ { "name": "x", "role": "cell", "match": { "kind": "log", "attr": { "k": 1 } } } ] }"#
        ),
        Err(MatchError::Parse(_))
    ));
    // Unknown during predicate.
    assert!(matches!(
        SignalSet::from_json(
            r#"{ "signals": [ { "name": "x", "role": "never", "match": { "kind": "span", "during": "someday" } } ] }"#
        ),
        Err(MatchError::UnknownDuring(_))
    ));
    // Malformed JSON never panics — it is a typed parse error.
    assert!(matches!(
        SignalSet::from_json("{ not json"),
        Err(MatchError::Parse(_))
    ));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Arbitrary valid sets round-trip to an identical value.
    #[test]
    fn arbitrary_sets_round_trip(set in arb_signal_set()) {
        let json = set.to_json().expect("serialize");
        let back = SignalSet::from_json(&json).expect("re-parse");
        prop_assert_eq!(set, back);
    }

    /// Parsing arbitrary UTF-8 never panics — it is always a typed `Result`.
    #[test]
    fn parsing_arbitrary_text_never_panics(s in ".{0,64}") {
        let _ = SignalSet::from_json(&s);
    }
}
