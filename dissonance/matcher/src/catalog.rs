// SPDX-License-Identifier: AGPL-3.0-or-later
//! The signal catalog and never-fired detection.
//!
//! **The declared set IS the catalog.** A [`Catalog`] holds every declared
//! signal — whether config-declared (the *scrape* tier, via
//! [`from_signals`](Catalog::from_signals)) or SDK-declared (the *link* tier,
//! via [`declare`](Catalog::declare), task 73). [`report`](Catalog::report)
//! partitions that declared set against the fired set into `fired ⊎
//! never_fired`, **tier-blind**: a declared `sometimes` that never matched is
//! your never-fired detection, identically however the signal was declared
//! (task-66 semantics 2).

use std::collections::{BTreeMap, BTreeSet};

use crate::signal::{Role, SignalId, SignalSet};

/// The declared signal set, any tier. Maps each declared name to its role, so a
/// report can be sliced by role if a caller wants (e.g. "which *sometimes*
/// signals never fired").
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    /// Declared name → role, deterministically ordered.
    declared: BTreeMap<SignalId, Role>,
}

/// The partition of the declared set against a run's (or campaign's) fired set:
/// `fired ⊎ never_fired = declared`, always, and the two are disjoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CatalogReport {
    /// Declared signals that fired (declared ∩ fired).
    pub fired: BTreeSet<SignalId>,
    /// Declared signals that never fired (declared − fired) — the never-fired
    /// detection.
    pub never_fired: BTreeSet<SignalId>,
}

impl Catalog {
    /// Scrape tier: seed the catalog from a config-declared signal set.
    pub fn from_signals(s: &SignalSet) -> Catalog {
        let declared = s
            .signals()
            .iter()
            .map(|d| (d.name.clone(), d.role))
            .collect();
        Catalog { declared }
    }

    /// Link tier (task 73): declare an SDK-registered signal. Idempotent on the
    /// name; a re-declare updates the recorded role.
    pub fn declare(&mut self, name: SignalId, role: Role) {
        self.declared.insert(name, role);
    }

    /// Partition the declared set against `fired` into `fired ⊎ never_fired`.
    /// Ids in `fired` that were never declared are ignored — the report is over
    /// the *declared* set, so the union is exactly `declared` by construction,
    /// tier-blind.
    pub fn report(&self, fired: &BTreeSet<SignalId>) -> CatalogReport {
        let mut fired_out = BTreeSet::new();
        let mut never_fired = BTreeSet::new();
        for name in self.declared.keys() {
            if fired.contains(name) {
                fired_out.insert(name.clone());
            } else {
                never_fired.insert(name.clone());
            }
        }
        CatalogReport {
            fired: fired_out,
            never_fired,
        }
    }

    /// The declared signals with their roles, deterministically ordered.
    pub fn declared(&self) -> impl Iterator<Item = (&SignalId, &Role)> {
        self.declared.iter()
    }

    /// The number of declared signals.
    pub fn len(&self) -> usize {
        self.declared.len()
    }

    /// Whether nothing has been declared.
    pub fn is_empty(&self) -> bool {
        self.declared.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> SignalId {
        SignalId(s.into())
    }

    #[test]
    fn report_partitions_the_declared_set() {
        let signals = SignalSet::from_json(
            r#"{ "signals": [
                { "name": "a", "role": "sometimes", "match": { "kind": "log" } },
                { "name": "b", "role": "never", "match": { "kind": "log" } }
            ] }"#,
        )
        .unwrap();
        let mut cat = Catalog::from_signals(&signals);
        // Link-tier declare (task 73): identical treatment.
        cat.declare(id("c"), Role::Cell);

        let fired: BTreeSet<SignalId> =
            [id("a"), id("c"), id("not-declared")].into_iter().collect();
        let report = cat.report(&fired);

        // fired ⊎ never_fired == declared, and disjoint.
        assert_eq!(report.fired, [id("a"), id("c")].into_iter().collect());
        assert_eq!(report.never_fired, [id("b")].into_iter().collect());
        assert!(report.fired.is_disjoint(&report.never_fired));
        let union: BTreeSet<SignalId> = report.fired.union(&report.never_fired).cloned().collect();
        let declared: BTreeSet<SignalId> = cat.declared().map(|(n, _)| n.clone()).collect();
        assert_eq!(union, declared);
        // An undeclared fired id is ignored (not invented into the report).
        assert!(!union.contains(&id("not-declared")));
    }
}
