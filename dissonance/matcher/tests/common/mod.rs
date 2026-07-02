// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared generators and an independent reference matcher for the property
//! suites. Kept small and deliberately naive so it is an *independent* oracle,
//! not a paraphrase of the implementation (the glob here is recursive; the
//! implementation's is a two-pointer scan).
#![allow(dead_code)]

use std::collections::BTreeMap;

use explorer::{Environment, Moment, RunTrace, StopReason, VTime, Value};
use matcher::stub::RecordRec;
use matcher::{During, MatchExpr, Role, SignalDecl, SignalId, SignalSet};
use proptest::prelude::*;

/// Kinds records and expressions draw from — a small alphabet so matches are
/// common.
pub const KINDS: &[&str] = &["log", "span", "evt"];
/// Attribute keys expressions glob on and records populate.
pub const ATTR_KEYS: &[&str] = &["a", "b"];
/// Glob patterns, including the wildcard forms.
pub const GLOBS: &[&str] = &["x", "y", "*", "x*", "*y", "xy"];
/// String attribute values records draw from.
pub const STR_VALS: &[&str] = &["x", "y", "xy", "yx", ""];
/// The attribute name `state_max` folds; records populate it with an integer.
pub const MAX_KEY: &str = "n";

/// A minimal trace; the sources ignore its records (they serve owned lists), so
/// only `env`/`terminal` matter — they flow into `Bug`.
pub fn trace() -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(7) },
        env: Environment {
            blob_version: 1,
            bytes: vec![1, 2, 3],
        },
        coverage: None,
        events: vec![],
        records: vec![],
    }
}

fn arb_role() -> impl Strategy<Value = Role> {
    prop_oneof![
        Just(Role::Sometimes),
        Just(Role::Never),
        Just(Role::Cell),
        Just(Role::StateMax),
    ]
}

fn arb_expr() -> impl Strategy<Value = MatchExpr> {
    (
        prop::sample::select(KINDS),
        prop::collection::btree_map(
            prop::sample::select(ATTR_KEYS).prop_map(str::to_string),
            prop::sample::select(GLOBS).prop_map(str::to_string),
            0..=2,
        ),
        prop::option::of(Just(MAX_KEY.to_string())),
        prop::option::of(Just(During::NoFaults)),
    )
        .prop_map(|(kind, attr, attr_max, during)| MatchExpr {
            kind: kind.to_string(),
            attr,
            attr_max,
            during,
        })
}

/// An arbitrary signal set with **unique** names (`s0`, `s1`, …), so it is
/// always constructible. A `state_max` role is given an `attr_max` (the DSL
/// rejects a register with nothing to fold), matching the validation rule.
pub fn arb_signal_set() -> impl Strategy<Value = SignalSet> {
    prop::collection::vec((arb_role(), arb_expr()), 0..=6).prop_map(|items| {
        let decls: Vec<SignalDecl> = items
            .into_iter()
            .enumerate()
            .map(|(i, (role, mut expr))| {
                if role == Role::StateMax && expr.attr_max.is_none() {
                    expr.attr_max = Some(MAX_KEY.to_string());
                }
                SignalDecl {
                    name: SignalId(format!("s{i}")),
                    role,
                    expr,
                }
            })
            .collect();
        SignalSet::new(decls).expect("names unique, state_max carries attr_max")
    })
}

fn arb_record() -> impl Strategy<Value = RecordRec> {
    (
        0u64..20,
        prop::sample::select(KINDS),
        prop::collection::btree_map(
            prop::sample::select(ATTR_KEYS).prop_map(str::to_string),
            prop::sample::select(STR_VALS).prop_map(|s| Value::Str(s.to_string())),
            0..=2,
        ),
        prop::option::of(0u64..16),
    )
        .prop_map(|(m, kind, mut attrs, n)| {
            if let Some(n) = n {
                attrs.insert(MAX_KEY.to_string(), Value::UInt(n));
            }
            RecordRec {
                moment: Moment(m),
                kind: kind.to_string(),
                attrs,
            }
        })
}

/// An arbitrary record stream.
pub fn arb_records() -> impl Strategy<Value = Vec<RecordRec>> {
    prop::collection::vec(arb_record(), 0..=10)
}

/// An arbitrary fault-`Moment` list.
pub fn arb_faults() -> impl Strategy<Value = Vec<Moment>> {
    prop::collection::vec((0u64..20).prop_map(Moment), 0..=4)
}

/// Render a value to its glob-comparison bytes — an independent copy of the
/// implementation's rule, so the reference matcher does not import it.
fn render(v: &Value) -> Vec<u8> {
    match v {
        Value::Str(s) => s.as_bytes().to_vec(),
        Value::UInt(u) => u.to_string().into_bytes(),
        Value::Int(i) => i.to_string().into_bytes(),
        Value::Bool(true) => b"true".to_vec(),
        Value::Bool(false) => b"false".to_vec(),
        Value::Bytes(b) => b.clone(),
    }
}

/// A naive recursive glob (the independent reference).
fn naive_glob(pat: &[u8], text: &[u8]) -> bool {
    match pat.first() {
        None => text.is_empty(),
        Some(&b'*') => {
            naive_glob(&pat[1..], text) || (!text.is_empty() && naive_glob(pat, &text[1..]))
        }
        Some(&c) => !text.is_empty() && text[0] == c && naive_glob(&pat[1..], &text[1..]),
    }
}

/// The independent reference for "does this record match this expression?" —
/// exact kind, every attr glob (naive), and the `during` predicate.
pub fn ref_match(rec: &RecordRec, expr: &MatchExpr, earliest_fault: Option<Moment>) -> bool {
    if rec.kind != expr.kind {
        return false;
    }
    for (k, pat) in &expr.attr {
        match rec.attrs.get(k) {
            Some(v) => {
                if !naive_glob(pat.as_bytes(), &render(v)) {
                    return false;
                }
            }
            None => return false,
        }
    }
    match expr.during {
        Some(During::NoFaults) => earliest_fault.is_none_or(|ef| ef > rec.moment),
        None => true,
        // `During` is `#[non_exhaustive]`; a future predicate defaults to
        // "holds" here until the reference learns it.
        _ => true,
    }
}

/// The set of record moments matching a given expression (reference).
pub fn matching_moments(
    recs: &[RecordRec],
    expr: &MatchExpr,
    earliest_fault: Option<Moment>,
) -> std::collections::BTreeSet<u64> {
    recs.iter()
        .filter(|r| ref_match(r, expr, earliest_fault))
        .map(|r| r.moment.0)
        .collect()
}

/// A convenience for building attribute maps in focused tests.
pub fn attrs(pairs: &[(&str, Value)]) -> BTreeMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
