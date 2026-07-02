// SPDX-License-Identifier: AGPL-3.0-or-later
//! The declarative signal DSL: [`SignalSet`] / [`SignalDecl`] / [`MatchExpr`] /
//! [`Role`] / [`During`], and the JSON parse + serialize path.
//!
//! On disk a signal set is JSON (`serde_json`), the task-66 ruling over the
//! doc's illustrative YAML. The normative shape:
//!
//! ```json
//! { "signals": [
//!   { "name": "leader.won", "role": "sometimes",
//!     "match": { "kind": "span", "attr": { "name": "raft.leader_election", "outcome": "won" } } }
//! ] }
//! ```
//!
//! Parsing goes through a private wire form so each malformed class maps to a
//! typed [`MatchError`] (unknown role / duplicate name / bad type) instead of a
//! panic; the validated domain types derive `Serialize` so
//! [`SignalSet::to_json`] round-trips back to the same shape.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::error::MatchError;

/// A signal's stable name — the catalog key and the never-fired-detection unit.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(transparent)]
pub struct SignalId(pub String);

/// The declared role of a signal — its **one** router destination. A match
/// routes to exactly this role's consumer and no other (router totality).
///
/// Stays extensible (`#[non_exhaustive]`): task 73's SDK catalog kinds fold in
/// here — reachable ⇒ [`Sometimes`](Role::Sometimes), unreachable ⇒
/// [`Never`](Role::Never), buggify points as their own future kind — with no
/// change to the router seam.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// Objective + checkpoint candidate → a `Feature` (+ a catalog fired-mark).
    Sometimes,
    /// Declarative always-assertion → an `Oracle` verdict (`Some(Bug)`).
    Never,
    /// Descriptor channel → a `Feature` on a cell-designated channel (CellFn
    /// input).
    Cell,
    /// The IJON `IJON_MAX` register moved from source to config → a `Feature`
    /// bucketing the running max of an integer attribute.
    StateMax,
}

/// A context predicate for `during:` — a run-scoped guard on when a record
/// counts as a match. v1 ships exactly [`NoFaults`](During::NoFaults); the enum
/// stays extensible (`#[non_exhaustive]`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum During {
    /// True iff no fault `Moment` at or before the record's `Moment` exists in
    /// the run's fault index ([`ContextSource`](crate::ContextSource)).
    NoFaults,
}

/// A parsed, validated match expression over a [`Matchable`](explorer::Matchable)
/// record.
///
/// - `kind` compares **exactly** against `Matchable::kind()`.
/// - each `attr` entry is a **glob** predicate (see [`crate::glob`]) over the
///   named attribute's rendered value; an absent attribute never matches.
/// - `attr_max` names an integer attribute the `state_max` role folds; a
///   non-integer or absent value is a counted decode miss, never a match
///   failure by itself.
/// - `during` is an optional context predicate that must also hold.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct MatchExpr {
    /// The record kind this expression matches, compared exactly.
    pub kind: String,
    /// Glob predicates keyed by attribute name (deterministically ordered).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attr: BTreeMap<String, String>,
    /// The integer attribute the `state_max` register folds, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attr_max: Option<String>,
    /// The context predicate that must also hold, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub during: Option<During>,
}

/// One declared signal: its name, its single router role, and its match
/// expression. Public fields per the task-66 contract.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct SignalDecl {
    /// The signal's stable name.
    pub name: SignalId,
    /// The signal's declared role.
    pub role: Role,
    /// The match expression that fires the signal.
    #[serde(rename = "match")]
    pub expr: MatchExpr,
}

/// A parsed, validated signal set — the [`SignalDecl`]s in **declaration
/// order** (the router visits every non-`never` signal per record; the catalog
/// is the declared set). Construct via [`SignalSet::from_json`]; the type is
/// not directly `Deserialize` so every parse runs validation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct SignalSet {
    signals: Vec<SignalDecl>,
}

// ---------------------------------------------------------------------------
// Wire form — the untrusted JSON shape, structurally deserialized then
// validated into the domain types with typed errors.
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WireConfig {
    #[serde(default)]
    signals: Vec<WireSignal>,
}

#[derive(Deserialize)]
struct WireSignal {
    name: String,
    role: String,
    #[serde(rename = "match")]
    expr: WireExpr,
}

#[derive(Deserialize)]
struct WireExpr {
    kind: String,
    #[serde(default)]
    attr: BTreeMap<String, String>,
    #[serde(default)]
    attr_max: Option<String>,
    #[serde(default)]
    during: Option<String>,
}

fn parse_role(s: &str) -> Result<Role, MatchError> {
    Ok(match s {
        "sometimes" => Role::Sometimes,
        "never" => Role::Never,
        "cell" => Role::Cell,
        "state_max" => Role::StateMax,
        other => return Err(MatchError::UnknownRole(other.to_string())),
    })
}

fn parse_during(s: &str) -> Result<During, MatchError> {
    match s {
        "no_faults" => Ok(During::NoFaults),
        other => Err(MatchError::UnknownDuring(other.to_string())),
    }
}

impl SignalSet {
    /// Parse and validate a signal set from JSON.
    ///
    /// Structural / type errors surface as [`MatchError::Parse`]; an unknown
    /// role, unknown `during`, or duplicate name surface as their dedicated
    /// variants. Never panics on untrusted input.
    pub fn from_json(json: &str) -> Result<SignalSet, MatchError> {
        let wire: WireConfig = serde_json::from_str(json)?;
        let mut signals = Vec::with_capacity(wire.signals.len());
        let mut seen = BTreeSet::new();
        for w in wire.signals {
            if !seen.insert(w.name.clone()) {
                return Err(MatchError::DuplicateName(w.name));
            }
            let role = parse_role(&w.role)?;
            let during = w.expr.during.as_deref().map(parse_during).transpose()?;
            signals.push(SignalDecl {
                name: SignalId(w.name),
                role,
                expr: MatchExpr {
                    kind: w.expr.kind,
                    attr: w.expr.attr,
                    attr_max: w.expr.attr_max,
                    during,
                },
            });
        }
        Ok(SignalSet { signals })
    }

    /// Serialize back to the normative JSON shape. Round-trips: parsing the
    /// output reproduces `self` (`serialize ∘ parse` is identity on the value).
    pub fn to_json(&self) -> Result<String, MatchError> {
        serde_json::to_string(self).map_err(MatchError::Parse)
    }

    /// Build a set directly from validated declarations (the in-memory
    /// constructor; enforces name uniqueness like [`from_json`](Self::from_json)).
    pub fn new(signals: Vec<SignalDecl>) -> Result<SignalSet, MatchError> {
        let mut seen = BTreeSet::new();
        for d in &signals {
            if !seen.insert(d.name.clone()) {
                return Err(MatchError::DuplicateName(d.name.0.clone()));
            }
        }
        Ok(SignalSet { signals })
    }

    /// The declarations, in declaration order.
    pub fn signals(&self) -> &[SignalDecl] {
        &self.signals
    }

    /// The number of declared signals.
    pub fn len(&self) -> usize {
        self.signals.len()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.signals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The normative example, verbatim from the task spec.
    pub(crate) const NORMATIVE: &str = r#"{ "signals": [
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
    fn parses_the_normative_example() {
        let s = SignalSet::from_json(NORMATIVE).expect("parse");
        assert_eq!(s.len(), 4);
        let decls = s.signals();
        assert_eq!(decls[0].name, SignalId("leader.won".into()));
        assert_eq!(decls[0].role, Role::Sometimes);
        assert_eq!(
            decls[0].expr.attr.get("outcome").map(String::as_str),
            Some("won")
        );
        assert_eq!(decls[1].role, Role::StateMax);
        assert_eq!(decls[1].expr.attr_max.as_deref(), Some("lsn"));
        assert_eq!(decls[2].role, Role::Cell);
        assert_eq!(decls[3].role, Role::Never);
        assert_eq!(decls[3].expr.during, Some(During::NoFaults));
    }

    #[test]
    fn round_trip_is_identity_on_the_value() {
        let s1 = SignalSet::from_json(NORMATIVE).expect("parse");
        let json = s1.to_json().expect("serialize");
        let s2 = SignalSet::from_json(&json).expect("re-parse");
        assert_eq!(s1, s2);
    }

    #[test]
    fn unknown_role_is_typed() {
        let json =
            r#"{ "signals": [ { "name": "x", "role": "whenever", "match": { "kind": "log" } } ] }"#;
        assert!(matches!(
            SignalSet::from_json(json),
            Err(MatchError::UnknownRole(r)) if r == "whenever"
        ));
    }

    #[test]
    fn duplicate_name_is_typed() {
        let json = r#"{ "signals": [
            { "name": "dup", "role": "cell", "match": { "kind": "log" } },
            { "name": "dup", "role": "never", "match": { "kind": "log" } }
        ] }"#;
        assert!(matches!(
            SignalSet::from_json(json),
            Err(MatchError::DuplicateName(n)) if n == "dup"
        ));
    }

    #[test]
    fn bad_type_is_typed() {
        // A non-string attribute value is a type error (bad type class).
        let json = r#"{ "signals": [ { "name": "x", "role": "cell", "match": { "kind": "log", "attr": { "k": 5 } } } ] }"#;
        assert!(matches!(
            SignalSet::from_json(json),
            Err(MatchError::Parse(_))
        ));
    }

    #[test]
    fn unknown_during_is_typed() {
        let json = r#"{ "signals": [ { "name": "x", "role": "never", "match": { "kind": "span", "during": "eclipse" } } ] }"#;
        assert!(matches!(
            SignalSet::from_json(json),
            Err(MatchError::UnknownDuring(d)) if d == "eclipse"
        ));
    }
}
