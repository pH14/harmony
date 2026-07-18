// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — **purity + determinism.** The same `RunTrace` evaluated twice
//! yields byte-identical serialized feature streams and identical oracle
//! verdicts; permuting an unrelated signal's declaration never changes any
//! other signal's output.

mod common;

use std::collections::BTreeMap;

use common::{arb_faults, arb_records, arb_signal_set, trace};
use explorer::{Moment, Oracle};
use matcher::stub::{FaultMoments, OwnedRecords, RecordRec};
use matcher::{ChannelId, Feature};
use matcher::{MatchOracle, MatchSensor, SignalSet};
use proptest::prelude::*;
use std::collections::BTreeSet;

/// Group a feature stream into the per-`Moment` [`BTreeSet<Feature>`] a `CellFn` keys —
/// the spine-level view the router feeds downstream.
fn feature_sets(stream: &[(Moment, Feature)]) -> BTreeMap<Moment, BTreeSet<Feature>> {
    let mut m: BTreeMap<Moment, BTreeSet<Feature>> = BTreeMap::new();
    for (moment, f) in stream {
        m.entry(*moment).or_default().insert(*f);
    }
    m
}

/// A record stream paired with a random shuffle of itself.
fn records_and_shuffle() -> impl Strategy<Value = (Vec<RecordRec>, Vec<RecordRec>)> {
    arb_records().prop_flat_map(|recs| (Just(recs.clone()), Just(recs).prop_shuffle()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn pure_and_permutation_invariant(
        set in arb_signal_set(),
        recs in arb_records(),
        faults in arb_faults(),
    ) {
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

        // (1) Purity: two evaluations are byte-identical after serialization,
        // and the oracle agrees with itself.
        let f1 = sensor.features(&t);
        let f2 = sensor.features(&t);
        let j1 = serde_json::to_string(&f1).unwrap();
        let j2 = serde_json::to_string(&f2).unwrap();
        prop_assert_eq!(&j1, &j2);
        prop_assert_eq!(oracle.verdicts(&t), oracle.verdicts(&t));
        prop_assert_eq!(oracle.judge(&t), oracle.judge(&t));

        // (2) Permutation invariance: reverse the declaration order (a
        // non-trivial permutation) and re-evaluate. Because channels are
        // name-derived and the outputs are canonically ordered, the *entire*
        // feature stream and every oracle verdict are unchanged — so, a
        // fortiori, no individual signal's output changed.
        let mut reversed = set.signals().to_vec();
        reversed.reverse();
        let permuted = SignalSet::new(reversed).unwrap();

        let sensor_p = MatchSensor::new(
            permuted.clone(),
            OwnedRecords(recs.clone()),
            FaultMoments(faults.clone()),
            ChannelId(1),
        )
        .unwrap();
        let oracle_p = MatchOracle::new(
            permuted,
            OwnedRecords(recs.clone()),
            FaultMoments(faults.clone()),
        );

        let fp = sensor_p.features(&t);
        prop_assert_eq!(
            serde_json::to_string(&f1).unwrap(),
            serde_json::to_string(&fp).unwrap(),
            "permuting declaration order changed the feature stream"
        );
        prop_assert_eq!(oracle.verdicts(&t), oracle_p.verdicts(&t));
        prop_assert_eq!(oracle.judge(&t), oracle_p.judge(&t));

        // And per-signal channels are stable across the permutation.
        for decl in set.signals() {
            prop_assert_eq!(
                sensor.channel_of(&decl.name),
                sensor_p.channel_of(&decl.name)
            );
        }

        // (3) Emission-order invariance (round-3): reversing the source's record
        // order — which permutes any same-Moment records — changes nothing. The
        // output is a pure function of record *content*, not emission order.
        let reversed_recs: Vec<_> = recs.iter().rev().cloned().collect();
        let sensor_e = MatchSensor::new(
            set.clone(),
            OwnedRecords(reversed_recs.clone()),
            FaultMoments(faults.clone()),
            ChannelId(1),
        )
        .unwrap();
        let oracle_e = MatchOracle::new(
            set.clone(),
            OwnedRecords(reversed_recs),
            FaultMoments(faults.clone()),
        );
        prop_assert_eq!(
            serde_json::to_string(&f1).unwrap(),
            serde_json::to_string(&sensor_e.features(&t)).unwrap(),
            "reversing emission order changed the feature stream"
        );
        prop_assert_eq!(oracle.verdicts(&t), oracle_e.verdicts(&t));
        prop_assert_eq!(oracle.judge(&t), oracle_e.judge(&t));
    }

    /// Round-3 P1, the dedicated shuffle proptest: a **random permutation** of
    /// the source's records (which reorders same-Moment records arbitrarily)
    /// yields the identical per-`Moment` `BTreeSet<Feature>`, the identical raw feature
    /// stream, and the identical `judge()` / `verdicts()` output. The router's
    /// result is a pure function of record content, never of emission order.
    #[test]
    fn shuffling_emission_order_preserves_featureset_and_judge(
        (recs, shuffled) in records_and_shuffle(),
        set in arb_signal_set(),
        faults in arb_faults(),
    ) {
        let t = trace();
        let eval = |r: Vec<RecordRec>| {
            let sensor = MatchSensor::new(
                set.clone(),
                OwnedRecords(r.clone()),
                FaultMoments(faults.clone()),
                ChannelId(1),
            )
            .unwrap();
            let oracle = MatchOracle::new(set.clone(), OwnedRecords(r), FaultMoments(faults.clone()));
            (sensor.features(&t), oracle.judge(&t), oracle.verdicts(&t))
        };
        let (f1, j1, v1) = eval(recs);
        let (f2, j2, v2) = eval(shuffled);

        prop_assert_eq!(feature_sets(&f1), feature_sets(&f2), "BTreeSet<Feature> differs under shuffle");
        prop_assert_eq!(f1, f2, "raw feature stream differs under shuffle");
        prop_assert_eq!(j1, j2, "judge() differs under shuffle");
        prop_assert_eq!(v1, v2, "verdicts() differ under shuffle");
    }
}
