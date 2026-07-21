// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 (public-API half) — the CellFn v1 encoding properties (≥ 256 cases):
//! `CellKey` encoding is injective over distinct channel-value tuples and stable
//! under re-encoding, and the `CellFn` key tracks its derived tuple.
//!
//! The clustering-side gate-4 properties (totality, masked-parameter merging,
//! codebook round-trip / reload transparency, and the adversarial `from_json`
//! fuzz) live as **unit** tests in `src/cluster.rs`: they exercise the codebook,
//! which is `pub(crate)` (the internality ruling) and so not reachable from an
//! integration test. `cargo nextest` runs both, so gate 4 is fully covered.

use explorer::Moment;
use logtmpl::{CellConfig, CellFnV1, decode_cell_key, encode_cell_key};
use logtmpl::{ChannelId, Feature, FeatureId, FeatureSet};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// CellKey encoding: injective over distinct tuples and stable under
    /// re-encoding (decode is a left inverse).
    #[test]
    fn cellkey_encoding_is_injective_and_stable(
        t1 in prop::collection::vec(prop::option::of(any::<u64>()), 0..8),
        t2 in prop::collection::vec(prop::option::of(any::<u64>()), 0..8),
    ) {
        let e1 = encode_cell_key(&t1);
        let e2 = encode_cell_key(&t2);
        // Stable under re-encoding.
        prop_assert_eq!(&e1, &encode_cell_key(&t1));
        // Left inverse ⇒ injective.
        prop_assert_eq!(decode_cell_key(&e1), Some(t1.clone()));
        // Distinct tuples ⇔ distinct keys.
        prop_assert_eq!(e1 == e2, t1 == t2);
    }

    /// The CellFn key equals the encoding of the derived tuple, and distinct
    /// species slices give distinct keys under the default knobs — the
    /// end-to-end injectivity the archive relies on.
    #[test]
    fn cellfn_key_tracks_its_tuple(
        ids_a in prop::collection::btree_set(0u64..500, 1..30),
        ids_b in prop::collection::btree_set(0u64..500, 1..30),
    ) {
        let cell = CellFnV1::new();
        let template_channel = cell.config().template_channel;
        let set = |ids: &std::collections::BTreeSet<u64>| -> FeatureSet {
            ids.iter().map(|&id| Feature { channel: template_channel, id: FeatureId(id) }).collect()
        };
        let (fa, fb) = (set(&ids_a), set(&ids_b));

        // key == encode(fields).
        prop_assert_eq!(cell.key(Moment(0), &fa), encode_cell_key(&cell.fields(&fa)));
        // Same tuple ⇒ same key; different tuple ⇒ different key.
        let same_tuple = cell.fields(&fa) == cell.fields(&fb);
        prop_assert_eq!(cell.key(Moment(0), &fa) == cell.key(Moment(1), &fb), same_tuple);
    }

    /// Adversarial totality: `decode_cell_key` never panics (and never over-
    /// allocates) on arbitrary bytes — the forged-count regression, fuzzed.
    /// Whatever it accepts must re-encode to a prefix-consistent form.
    #[test]
    fn decode_cell_key_is_total_on_arbitrary_bytes(bytes in prop::collection::vec(any::<u8>(), 0..64)) {
        if let Some(fields) = decode_cell_key(&bytes) {
            // A successful decode is an exact left inverse: re-encoding reproduces
            // the input, so acceptance is never lossy.
            prop_assert_eq!(encode_cell_key(&fields), bytes);
        }
    }
}

/// A state channel's *latest* value is what the key folds — exercised outside
/// `proptest!` because it constructs a two-channel config.
#[test]
fn state_channel_latest_value_is_folded() {
    let config = CellConfig {
        cell_channels: vec![ChannelId(9)],
        fold_k: 16,
        ..CellConfig::default()
    };
    let cell = CellFnV1::with_config(config);
    // The state channel carries a single (latest) value 20 → 20 mod 16 = 4.
    let feats: FeatureSet = [
        Feature {
            channel: ChannelId(1),
            id: FeatureId(0),
        },
        Feature {
            channel: ChannelId(9),
            id: FeatureId(20),
        },
    ]
    .into_iter()
    .collect();
    assert_eq!(cell.fields(&feats), vec![Some(1), Some(0), Some(4)]);
}
