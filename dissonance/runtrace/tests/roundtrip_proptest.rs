// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — the journal round-trips and re-derivation is stable.
//!
//! - `decode(encode(t)) == t` for any trace.
//! - `encode(decode(b)) == b` for any bytes `decode` accepts (canonical form —
//!   no journal has two byte representations, and arbitrary bytes never panic
//!   `decode`).
//! - serialize → reload → **re-derive**: a test-local [`MarkerSensor`] yields
//!   the identical `(Moment, Feature)` stream over the reloaded trace and the
//!   original (via both `decode(encode(..))` and a round-trip through the
//!   [`TraceStore`]).

mod common;

use common::{MarkerSensor, arb_run_trace};
use explorer::Sensor;
use proptest::prelude::*;
use runtrace::{Retain, TraceStore, decode, encode};

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// `decode(encode(t)) == t`.
    #[test]
    fn decode_of_encode_is_identity(t in arb_run_trace()) {
        let bytes = encode(&t).expect("arb trace encodes");
        let back = decode(&bytes).expect("a self-encoded journal decodes");
        prop_assert_eq!(back, t);
    }

    /// `encode(t)` is canonical and `encode(decode(encode(t))) == encode(t)`.
    #[test]
    fn encode_is_stable_and_canonical(t in arb_run_trace()) {
        let b1 = encode(&t).expect("arb trace encodes");
        let b2 = encode(&decode(&b1).expect("decode")).expect("re-encode");
        prop_assert_eq!(b1, b2);
    }

    /// `decode` is total over arbitrary bytes: it never panics, and whenever it
    /// *accepts* bytes, re-encoding reproduces them exactly (canonical form).
    #[test]
    fn decode_is_total_and_canonical_over_arbitrary_bytes(bytes in proptest::collection::vec(any::<u8>(), 0..128)) {
        if let Ok(t) = decode(&bytes) {
            prop_assert_eq!(encode(&t).expect("arb trace encodes"), bytes);
        }
    }

    /// Re-derivation is stable across `encode`/`decode`: a new Sensor over the
    /// reloaded trace yields the identical timestamped feature stream.
    #[test]
    fn sensor_rederives_identically_after_encode_decode(t in arb_run_trace()) {
        let sensor = MarkerSensor::new(b"a");
        let reloaded = decode(&encode(&t).expect("arb trace encodes")).expect("decode");
        prop_assert_eq!(sensor.observe(&reloaded), sensor.observe(&t));
    }
}

/// Re-derivation is stable across a full [`TraceStore`] round-trip
/// (record→load), not just in-memory encode/decode — the replay-plane path the
/// box gate re-derives over.
#[test]
fn sensor_rederives_identically_after_store_roundtrip() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    let dir = tempfile::tempdir().expect("tempdir");
    let store = TraceStore::open(dir.path()).expect("open store");
    let sensor = MarkerSensor::new(b"READY");

    // A handful of deterministic draws from the strategy, each recorded and
    // reloaded; the sensor must agree on the original and the reloaded trace.
    let mut runner = TestRunner::deterministic();
    for _ in 0..32 {
        let t = arb_run_trace()
            .new_tree(&mut runner)
            .expect("value tree")
            .current();
        let id = store.record(&t, Retain::Full).expect("record");
        let reloaded = store.load(id).expect("load");
        assert_eq!(reloaded, t, "store round-trip is lossless");
        assert_eq!(
            sensor.observe(&reloaded),
            sensor.observe(&t),
            "sensor re-derives identically over the reloaded trace"
        );
    }
}
