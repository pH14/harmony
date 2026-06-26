// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — codec. `EnvSpec::encode`→`decode` round-trips for arbitrary specs;
//! `Answer::encode`→`decode` round-trips; `decode` on arbitrary/mutated bytes
//! never panics and rejects off-version with `EnvError::BadVersion`.

mod common;

use common::{arb_answer, arb_spec, canon, config};
use environment::{Answer, EnvError, EnvSpec};
use proptest::prelude::*;

proptest! {
    #![proptest_config(config(512))]

    /// `decode(encode(canon(spec))) == canon(spec)`.
    #[test]
    fn envspec_round_trips(spec in arb_spec()) {
        let spec = canon(spec);
        let bytes = spec.encode();
        let back = EnvSpec::decode(&bytes).expect("our own encoding decodes");
        prop_assert_eq!(&spec, &back);
        // Re-encoding is byte-stable.
        prop_assert_eq!(bytes, back.encode());
    }

    /// `Answer::encode`→`decode` round-trips for any structurally-valid answer.
    #[test]
    fn answer_round_trips(ans in arb_answer()) {
        let bytes = ans.encode();
        prop_assert_eq!(&ans, &Answer::decode(&bytes).expect("our own encoding decodes"));
    }

    /// `EnvSpec::decode` is total on arbitrary bytes — only Ok or Err, never a
    /// panic or an out-of-bounds read.
    #[test]
    fn envspec_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = EnvSpec::decode(&bytes);
    }

    /// `Answer::decode` is total on arbitrary bytes.
    #[test]
    fn answer_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = Answer::decode(&bytes);
    }

    /// A valid blob with its version field bumped decodes to `BadVersion`.
    #[test]
    fn off_version_is_bad_version(spec in arb_spec()) {
        let mut bytes = canon(spec).encode();
        // Layout: magic:u32 then version:u16. Flip the version to an unknown value.
        bytes[4] = bytes[4].wrapping_add(7);
        bytes[5] = bytes[5].wrapping_add(11);
        match EnvSpec::decode(&bytes) {
            Err(EnvError::BadVersion(_)) => {}
            other => prop_assert!(false, "expected BadVersion, got {other:?}"),
        }
    }
}

#[test]
fn truncations_of_a_valid_blob_never_panic() {
    // Every prefix of a real blob is rejected cleanly (or, at full length,
    // accepted) — a classic source of untrusted-length panics.
    let mut policy = environment::FaultPolicy::none();
    policy
        .set_class(
            environment::DecisionClass::BlockIo,
            1,
            3,
            &[
                environment::Fault::BlockEio,
                environment::Fault::BlockTorn(8),
            ],
        )
        .unwrap();
    let spec = EnvSpec::Recorded {
        seed: 0xDEAD_BEEF,
        policy,
        overrides: vec![
            (
                environment::DecisionId(1),
                Answer::Fault(environment::Fault::BlockEio),
            ),
            (environment::DecisionId(4), Answer::Supply(vec![1, 2, 3, 4])),
        ],
        standing: vec![environment::StandingFault {
            class: environment::DecisionClass::NetSend,
            target: vec![0, 1, 2, 3],
            window: (environment::VTime(10), environment::VTime(20)),
        }],
    };
    let bytes = spec.encode();
    for n in 0..bytes.len() {
        let _ = EnvSpec::decode(&bytes[..n]); // must not panic
    }
    assert_eq!(EnvSpec::decode(&bytes).unwrap(), spec);
}

#[test]
fn trailing_bytes_are_rejected() {
    let spec = EnvSpec::Seeded {
        seed: 1,
        policy: environment::FaultPolicy::none(),
    };
    let mut bytes = spec.encode();
    bytes.push(0); // one extra byte
    assert_eq!(EnvSpec::decode(&bytes), Err(EnvError::Malformed));
}

#[test]
fn duplicate_override_ids_are_deduped_on_encode() {
    use environment::{ConnId, DecisionPoint as P, Environment, Fault, NodeId, Outcome};

    // Two overrides at id 3 with *different* answers. `encode` must collapse them
    // to one (last write wins, matching `materialize`), so the blob is canonical
    // and round-trips — a duplicate can never reach the bytes.
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: environment::FaultPolicy::none(),
        overrides: vec![
            (environment::DecisionId(3), Answer::Nominal),
            (environment::DecisionId(3), Answer::Fault(Fault::NetDrop)),
        ],
        standing: vec![],
    };

    let decoded = EnvSpec::decode(&spec.encode()).expect("dedup makes the blob well-formed");
    let EnvSpec::Recorded { overrides, .. } = &decoded else {
        panic!("expected Recorded");
    };
    assert_eq!(overrides.len(), 1, "duplicate id collapses to one entry");
    assert_eq!(overrides[0].0, environment::DecisionId(3));
    assert_eq!(
        overrides[0].1,
        Answer::Fault(Fault::NetDrop),
        "last write wins"
    );
    assert_eq!(decoded.encode(), spec.encode(), "now byte-stable");

    // `materialize` resolves the duplicate the same way: decision id 3 (the 4th
    // `decide`) takes the last answer, NetDrop.
    let mut env = spec.materialize();
    let net = P::NetSend {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(0),
        len: 8,
    };
    let proc = P::Process { node: NodeId(0) };
    let mut last = Outcome::Resolved(Answer::Nominal);
    for i in 0..4u64 {
        last = env.decide(if i == 3 { &net } else { &proc });
    }
    assert_eq!(last, Outcome::Resolved(Answer::Fault(Fault::NetDrop)));
}

#[test]
fn decode_rejects_non_ascending_override_ids() {
    // A hand-crafted blob with two overrides at the same id (which `encode` now
    // never emits) must still be rejected — the untrusted-bytes guard. Build a
    // valid two-id blob, then rewrite the second (sentinel) id down to the first.
    let sentinel: u64 = 0x1122_3344_5566_7788;
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: environment::FaultPolicy::none(),
        overrides: vec![
            (environment::DecisionId(3), Answer::Nominal),
            (environment::DecisionId(sentinel), Answer::Nominal),
        ],
        standing: vec![],
    };
    let mut bytes = spec.encode();
    let pat = sentinel.to_le_bytes();
    let pos = bytes
        .windows(8)
        .position(|w| w == pat)
        .expect("sentinel id present in the blob");
    bytes[pos..pos + 8].copy_from_slice(&3u64.to_le_bytes());
    assert_eq!(EnvSpec::decode(&bytes), Err(EnvError::Malformed));
}
