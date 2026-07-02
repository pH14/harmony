// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — the property tests (≥ 256 cases each):
//!
//! 1. **Totality:** every line clusters; arbitrary bytes never panic.
//! 2. **Masking:** lines differing only in masked parameter positions land in
//!    the same template.
//! 3. **Codebook round-trip:** serialize → reload is identity, and reloading
//!    mid-stream is transparent.
//! 4. **CellKey encoding:** injective over distinct channel-value tuples and
//!    stable under re-encoding.

use explorer::{CellFn, ChannelId, Feature, FeatureId, FeatureSet, Moment};
use logtmpl::{CellConfig, CellFnV1, Codebook, decode_cell_key, encode_cell_key};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Totality: any sequence of arbitrary strings clusters without panic, and
    /// every assigned id is a real template (`< len`).
    #[test]
    fn every_line_clusters_totally(lines in prop::collection::vec(any::<String>(), 0..40)) {
        let mut cb = Codebook::default();
        for line in &lines {
            let a = cb.ingest(line);
            prop_assert!(a.template < cb.len() as u64);
            // A brand-new species is exactly the freshly-minted last id.
            if a.is_new {
                prop_assert_eq!(a.template, cb.len() as u64 - 1);
            }
        }
        // Serialization stays total on whatever tree those bytes produced.
        prop_assert!(Codebook::from_json(&cb.to_json()).is_ok());
    }

    /// Masking: two lines built from the same literal skeleton but different
    /// digit-bearing tokens at the parameter slots cluster into one template.
    #[test]
    fn masked_parameter_differences_share_a_template(
        skeleton in prop::collection::vec(
            prop_oneof![
                // a fixed literal slot (no digits): stays identical across variants
                "[a-z]{1,8}".prop_map(|s| (false, s)),
                // a parameter slot (marker): filled with distinct digit tokens
                Just((true, String::new())),
            ],
            1..10,
        ),
        fills in prop::collection::vec(("p[0-9]{1,6}", "q[0-9]{1,6}"), 10),
    ) {
        // Ensure at least one parameter slot so the property is non-trivial.
        let mut skeleton = skeleton;
        if !skeleton.iter().any(|(is_param, _)| *is_param) {
            skeleton[0] = (true, String::new());
        }

        let mut fi = fills.into_iter();
        let (mut a_toks, mut b_toks) = (Vec::new(), Vec::new());
        for (is_param, lit) in &skeleton {
            if *is_param {
                let (p, q) = fi.next().unwrap_or(("p0".into(), "q0".into()));
                a_toks.push(p);
                b_toks.push(q);
            } else {
                a_toks.push(lit.clone());
                b_toks.push(lit.clone());
            }
        }
        let (line_a, line_b) = (a_toks.join(" "), b_toks.join(" "));

        let mut cb = Codebook::default();
        let ta = cb.ingest(&line_a).template;
        let tb = cb.ingest(&line_b).template;
        prop_assert_eq!(ta, tb, "masked-only differences must share a template");
        // No second species was minted.
        prop_assert_eq!(cb.len(), 1);
    }

    /// Round-trip: a folded codebook serializes → reloads identically, and
    /// re-encoding is byte-stable. Reloading mid-stream then finishing matches
    /// the uninterrupted fold.
    #[test]
    fn codebook_roundtrips_and_reload_is_transparent(
        lines in prop::collection::vec("[a-z0-9 ]{0,24}", 0..40),
    ) {
        // Reference: uninterrupted fold.
        let mut whole = Codebook::default();
        let ref_ids: Vec<u64> = lines.iter().map(|l| whole.ingest(l).template).collect();
        let ref_bytes = whole.to_json();

        // Identity round-trip.
        let reloaded = Codebook::from_json(&ref_bytes).unwrap();
        prop_assert_eq!(&whole, &reloaded);
        prop_assert_eq!(&ref_bytes, &reloaded.to_json());

        // Transparent mid-stream reload at every split point.
        for split in 0..=lines.len() {
            let mut a = Codebook::default();
            let mut ids: Vec<u64> =
                lines[..split].iter().map(|l| a.ingest(l).template).collect();
            let mut b = Codebook::from_json(&a.to_json()).unwrap();
            ids.extend(lines[split..].iter().map(|l| b.ingest(l).template));
            prop_assert_eq!(&ids, &ref_ids);
            prop_assert_eq!(&b.to_json(), &ref_bytes);
        }
    }

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

    /// Adversarial totality: `Codebook::from_json` never panics on arbitrary
    /// bytes, and any codebook it *accepts* is safe to keep clustering — the next
    /// `ingest` cannot index out of bounds (the dangling-template regression).
    #[test]
    fn from_json_is_total_on_arbitrary_bytes(
        bytes in prop::collection::vec(any::<u8>(), 0..256),
        line in "[a-z0-9 ]{0,24}",
    ) {
        if let Ok(mut cb) = Codebook::from_json(&bytes) {
            let a = cb.ingest(&line);
            prop_assert!(a.template < cb.len() as u64);
        }
    }

    /// Targeted fuzz of the dangling-template guard: build a real codebook, then
    /// rewrite every parse-tree id to an arbitrary value. `from_json` must either
    /// reject it (typed error) or return a codebook whose every tree id is in
    /// range — in which case further `ingest`s never panic.
    #[test]
    fn corrupting_tree_ids_never_yields_a_panicking_codebook(
        seed_lines in prop::collection::vec("[a-z]{1,4} [a-z0-9]{1,4}", 1..12),
        // Mix small (often in-range → accepted) and arbitrary (usually
        // out-of-range → rejected) ids so both branches get exercised.
        replacements in prop::collection::vec(prop_oneof![0u64..6, any::<u64>()], 1..40),
        probe in "[a-z0-9 ]{0,24}",
    ) {
        let mut src = Codebook::default();
        for l in &seed_lines { src.ingest(l); }

        let mut v: serde_json::Value = serde_json::from_slice(&src.to_json()).unwrap();
        let mut r = replacements.iter().cloned().cycle();
        if let Some(tree) = v["tree"].as_array_mut() {
            for pair in tree.iter_mut() {
                if let Some(ids) = pair[1].as_array_mut() {
                    for id in ids.iter_mut() {
                        *id = serde_json::json!(r.next().unwrap());
                    }
                }
            }
        }
        let bytes = serde_json::to_vec(&v).unwrap();

        match Codebook::from_json(&bytes) {
            Err(_) => {} // rejected — safe
            Ok(mut cb) => {
                // Accepted ⇒ ingest is index-safe, however adversarial the probe.
                let a = cb.ingest(&probe);
                prop_assert!(a.template < cb.len() as u64);
            }
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
