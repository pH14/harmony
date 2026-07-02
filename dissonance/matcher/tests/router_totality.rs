// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — **router totality.** Over arbitrary signal sets + record streams,
//! every match appears in exactly its declared role's output, no role leaks
//! into another, and the routed set equals the set of matching `(signal,
//! record)` pairs.

mod common;

use std::collections::BTreeSet;

use common::{arb_faults, arb_records, arb_signal_set, matching_moments, ref_match, trace};
use explorer::{ChannelId, Oracle, Sensor};
use matcher::stub::{FaultMoments, OwnedRecords};
use matcher::{MatchOracle, MatchSensor, Role};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn every_match_routes_to_its_declared_role(
        set in arb_signal_set(),
        recs in arb_records(),
        faults in arb_faults(),
    ) {
        let earliest = faults.iter().min().copied();
        let sensor = MatchSensor::new(
            set.clone(),
            OwnedRecords(recs.clone()),
            FaultMoments(faults.clone()),
            ChannelId(1),
        )
        .unwrap();
        let oracle = MatchOracle::new(
            set.clone(),
            OwnedRecords(recs.clone()),
            FaultMoments(faults.clone()),
        );
        let t = trace();

        // Drive through the spine trait objects (the real integration surface).
        let feats = Sensor::observe(&sensor, &t);
        let sfired = sensor.fired(&t);
        let ofired = oracle.fired(&t);
        let verdicts = oracle.verdicts(&t);

        for decl in set.signals() {
            let channel = sensor.channel_of(&decl.name).expect("declared signal has a channel");
            let expected = matching_moments(&recs, &decl.expr, earliest);
            let matched = !expected.is_empty();
            let on_channel: BTreeSet<u64> = feats
                .iter()
                .filter(|(_, f)| f.channel == channel)
                .map(|(m, _)| m.0)
                .collect();

            match decl.role {
                // A feature at exactly the matching moments; catalog fired iff
                // matched; never in the oracle's fired set.
                Role::Sometimes | Role::Cell => {
                    prop_assert_eq!(
                        &on_channel, &expected,
                        "feature moments must equal match moments for {:?}", decl.name
                    );
                    prop_assert_eq!(sfired.contains(&decl.name), matched);
                    prop_assert!(!ofired.contains(&decl.name));
                }
                // Bucket-increase features are a subset of match moments; fired
                // iff matched.
                Role::StateMax => {
                    prop_assert!(on_channel.is_subset(&expected));
                    prop_assert_eq!(sfired.contains(&decl.name), matched);
                    prop_assert!(!ofired.contains(&decl.name));
                }
                // Routes to the oracle only — no feature on this channel, never
                // in the sensor's fired set.
                Role::Never => {
                    prop_assert!(
                        on_channel.is_empty(),
                        "never role leaked a feature onto {:?}", decl.name
                    );
                    prop_assert!(!sfired.contains(&decl.name));
                    prop_assert_eq!(ofired.contains(&decl.name), matched);
                }
                // `Role` is `#[non_exhaustive]`; a future role is inert here.
                _ => {}
            }
        }

        // The routed `never` set equals every matching (never-signal, record)
        // pair: one verdict per match.
        let never_matches: usize = set
            .signals()
            .iter()
            .filter(|d| d.role == Role::Never)
            .map(|d| recs.iter().filter(|r| ref_match(r, &d.expr, earliest)).count())
            .sum();
        prop_assert_eq!(verdicts.len(), never_matches);

        // `judge` reports the earliest verdict (or `None`).
        prop_assert_eq!(Oracle::judge(&oracle, &t), verdicts.first().cloned());

        // The two fired sets are disjoint (role partition), and every fired
        // signal actually matched.
        prop_assert!(sfired.is_disjoint(&ofired));
        for name in sfired.iter().chain(ofired.iter()) {
            let decl = set.signals().iter().find(|d| &d.name == name).unwrap();
            prop_assert!(!matching_moments(&recs, &decl.expr, earliest).is_empty());
        }
    }
}
