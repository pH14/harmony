// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — **purity + determinism.** The same `RunTrace` evaluated twice
//! yields byte-identical serialized feature streams and identical oracle
//! verdicts; permuting an unrelated signal's declaration never changes any
//! other signal's output.

mod common;

use common::{arb_faults, arb_records, arb_signal_set, trace};
use explorer::Oracle;
use matcher::stub::{FaultMoments, OwnedRecords};
use matcher::{MatchOracle, MatchSensor, SignalSet};
use proptest::prelude::*;

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
        );
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
        );
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
    }
}
