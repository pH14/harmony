// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — manifest round-trip (property) + deterministic field order, and
//! Gate 6 — `load_manifest` never panics on garbage input.

use acceptance_suite::{CorpusItem, CorpusKind, OracleKind, load_manifest, to_manifest, validate};
use proptest::prelude::*;

/// Printable, control-char-free strings (paths / names): rich enough to be
/// non-vacuous (spaces, punctuation, unicode, quotes, backslashes) while staying
/// clear of TOML control-char escaping corner cases.
fn text() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[^\\x00-\\x1f\\x7f]{0,24}").unwrap()
}

fn kind() -> impl Strategy<Value = CorpusKind> {
    prop_oneof![
        Just(CorpusKind::Micro),
        Just(CorpusKind::Workload),
        Just(CorpusKind::FuzzSeed),
    ]
}

fn oracle() -> impl Strategy<Value = OracleKind> {
    prop_oneof![
        Just(OracleKind::Determinism),
        Just(OracleKind::Conformance),
        Just(OracleKind::SeedSensitivity {
            rng_consuming: true
        }),
        Just(OracleKind::SeedSensitivity {
            rng_consuming: false
        }),
    ]
}

fn item() -> impl Strategy<Value = CorpusItem> {
    (
        text(),
        kind(),
        text(),
        prop::collection::vec(oracle(), 0..5),
        prop::option::of(text()),
    )
        .prop_map(|(name, kind, source, oracles, golden)| CorpusItem {
            name,
            kind,
            source,
            oracles,
            golden,
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn round_trip(items in prop::collection::vec(item(), 0..8)) {
        let text = to_manifest(&items);
        let parsed = load_manifest(&text).expect("serialized manifest must re-parse");
        prop_assert_eq!(parsed, items);
        // Serialization is deterministic (no map iteration reaches the bytes).
        prop_assert_eq!(&to_manifest(&load_manifest(&text).unwrap()), &text);
    }

    /// Never panic on arbitrary input — just `Ok` or `Err`.
    #[test]
    fn never_panics_on_arbitrary_text(s in "\\PC{0,200}") {
        let _ = load_manifest(&s);
    }
}

#[test]
fn field_order_is_fixed_and_readable() {
    let items = vec![CorpusItem {
        name: "tsc-monotonic".to_string(),
        kind: CorpusKind::Micro,
        source: "consonance/acceptance-suite/payloads/tsc.bin".to_string(),
        oracles: vec![OracleKind::Determinism, OracleKind::Conformance],
        golden: Some("consonance/acceptance-suite/golden/tsc.digest".to_string()),
    }];
    let text = to_manifest(&items);
    // name before kind before source before oracles before golden.
    let pos = |needle: &str| text.find(needle).unwrap();
    assert!(pos("name") < pos("kind"));
    assert!(pos("kind") < pos("source"));
    assert!(pos("source") < pos("oracles"));
    assert!(pos("oracles") < pos("golden"));
    assert!(text.contains("\"determinism\""));
    assert!(text.contains("\"conformance\""));
}

#[test]
fn validate_rejects_conformance_without_golden() {
    let bad = vec![CorpusItem {
        name: "needs-golden".to_string(),
        kind: CorpusKind::Micro,
        source: "s".to_string(),
        oracles: vec![OracleKind::Determinism, OracleKind::Conformance],
        golden: None,
    }];
    let err = validate(&bad).unwrap_err();
    assert!(err.to_string().contains("golden"));
    assert!(err.to_string().contains("needs-golden"));

    // Round-trips and validates once the golden is supplied.
    let good = vec![CorpusItem {
        golden: Some("g.digest".to_string()),
        ..bad.into_iter().next().unwrap()
    }];
    assert_eq!(load_manifest(&to_manifest(&good)).unwrap(), good);
    assert!(validate(&good).is_ok());
}
