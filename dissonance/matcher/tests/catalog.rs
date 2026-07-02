// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — **the declared set is the catalog.** `fired ⊎ never_fired =
//! declared` always, and identically whether a signal was config-declared
//! (scrape) or entered via `declare()` (link, task 73) — tier-blind detection.

mod common;

use std::collections::BTreeSet;

use common::arb_signal_set;
use matcher::{Catalog, Role, SignalId, SignalSet};
use proptest::prelude::*;

/// An arbitrary subset of the declared names (plus some undeclared noise), as
/// the "fired" set a run would produce.
fn arb_fired(declared: &[String]) -> impl Strategy<Value = BTreeSet<SignalId>> {
    let declared: Vec<String> = declared.to_vec();
    let n = declared.len();
    (
        proptest::collection::vec(any::<bool>(), n),
        proptest::collection::vec("[a-z]{1,4}", 0..=3),
    )
        .prop_map(move |(mask, noise)| {
            let mut fired: BTreeSet<SignalId> = declared
                .iter()
                .zip(mask)
                .filter(|(_, keep)| *keep)
                .map(|(name, _)| SignalId(name.clone()))
                .collect();
            // Undeclared ids must be ignored by `report`.
            for s in noise {
                fired.insert(SignalId(format!("undeclared-{s}")));
            }
            fired
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn report_partitions_declared_tier_blind(set in arb_signal_set()) {
        let declared_names: Vec<String> =
            set.signals().iter().map(|d| d.name.0.clone()).collect();

        // Two catalogs over the *same* declared set, built the two different
        // ways: entirely config-declared (scrape), vs entirely `declare()`d
        // (link). They must be indistinguishable to `report`.
        let scrape = Catalog::from_signals(&set);
        let mut link = Catalog::from_signals(&SignalSet::new(vec![]).unwrap());
        for d in set.signals() {
            link.declare(SignalId(d.name.0.clone()), Role::Sometimes);
        }

        // Same declared *key* set either way.
        let scrape_keys: BTreeSet<SignalId> = scrape.declared().map(|(n, _)| n.clone()).collect();
        let link_keys: BTreeSet<SignalId> = link.declared().map(|(n, _)| n.clone()).collect();
        prop_assert_eq!(&scrape_keys, &link_keys);

        proptest!(|(fired in arb_fired(&declared_names))| {
            let rs = scrape.report(&fired);
            let rl = link.report(&fired);

            // fired ⊎ never_fired == declared, disjoint — for both tiers.
            for r in [&rs, &rl] {
                prop_assert!(r.fired.is_disjoint(&r.never_fired));
                let union: BTreeSet<SignalId> =
                    r.fired.union(&r.never_fired).cloned().collect();
                prop_assert_eq!(&union, &scrape_keys);
                // fired == declared ∩ fired; never_fired == declared − fired.
                for name in &scrape_keys {
                    let is_fired = fired.contains(name);
                    prop_assert_eq!(r.fired.contains(name), is_fired);
                    prop_assert_eq!(r.never_fired.contains(name), !is_fired);
                }
            }

            // Tier-blind: identical reports.
            prop_assert_eq!(&rs.fired, &rl.fired);
            prop_assert_eq!(&rs.never_fired, &rl.never_fired);
        });
    }
}
