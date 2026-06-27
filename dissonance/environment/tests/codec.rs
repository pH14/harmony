// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — codec. `EnvSpec::encode`→`decode` round-trips for arbitrary specs;
//! `Answer`/`HostFault`/`Action`::encode`→`decode` round-trip; `decode` on
//! arbitrary/mutated bytes never panics and rejects off-version with
//! `EnvError::BadVersion`.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_answer, arb_host_fault, arb_spec, canon, config};
use environment::{Action, Answer, EnvError, EnvSpec, HostFault};
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

    /// `HostFault::encode`→`decode` round-trips — the `perturb` transport form.
    #[test]
    fn host_fault_round_trips(f in arb_host_fault()) {
        let bytes = f.encode();
        prop_assert_eq!(f, HostFault::decode(&bytes).expect("our own encoding decodes"));
    }

    /// `Action::encode`→`decode` round-trips for either plane.
    #[test]
    fn action_round_trips(a in arb_action()) {
        let bytes = a.encode();
        prop_assert_eq!(&a, &Action::decode(&bytes).expect("our own encoding decodes"));
    }

    /// `EnvSpec::decode` is total on arbitrary bytes — only Ok or Err, never a
    /// panic or an out-of-bounds read.
    #[test]
    fn envspec_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = EnvSpec::decode(&bytes);
    }

    /// `Answer::decode`, `HostFault::decode`, `Action::decode` are total on
    /// arbitrary bytes.
    #[test]
    fn catalog_decode_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..512)) {
        let _ = Answer::decode(&bytes);
        let _ = HostFault::decode(&bytes);
        let _ = Action::decode(&bytes);
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
    // accepted) — a classic source of untrusted-length panics. The blob mixes
    // host and guest overrides on one Moment axis.
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
        overrides: BTreeMap::from([
            (
                1,
                Action::Guest(Answer::Fault(environment::Fault::BlockEio)),
            ),
            (
                4,
                Action::Host(HostFault::CorruptMemory {
                    gpa: 0x4000,
                    mask: environment::BitMask(0b1000),
                }),
            ),
            (9, Action::Guest(Answer::Supply(vec![1, 2, 3, 4]))),
            (
                12,
                Action::Host(HostFault::InjectInterrupt { vector: 0x80 }),
            ),
        ]),
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
fn perturb_and_record_stamp_one_action_per_moment() {
    // `record`/`perturb` stamp both planes on one Moment axis; a second stamp at
    // the same Moment overwrites (last write wins), so the map stays canonical
    // and round-trips.
    let mut spec = EnvSpec::Seeded {
        seed: 0,
        policy: environment::FaultPolicy::none(),
    };
    // perturb promotes Seeded -> Recorded.
    spec.perturb(HostFault::InjectInterrupt { vector: 1 }, 100);
    spec.record(50, Action::Guest(Answer::Nominal));
    // Overwrite Moment 100 with a different action.
    spec.perturb(HostFault::InjectInterrupt { vector: 2 }, 100);

    let EnvSpec::Recorded { overrides, .. } = &spec else {
        panic!("perturb promotes to Recorded");
    };
    assert_eq!(overrides.len(), 2, "two distinct Moments");
    assert_eq!(
        overrides[&100],
        Action::Host(HostFault::InjectInterrupt { vector: 2 }),
        "last write wins at Moment 100"
    );
    assert_eq!(overrides[&50], Action::Guest(Answer::Nominal));

    // The host_faults view returns only the host plane, in Moment order.
    let hosts: Vec<_> = spec.host_faults().collect();
    assert_eq!(hosts, vec![(100, HostFault::InjectInterrupt { vector: 2 })]);

    assert_eq!(
        EnvSpec::decode(&spec.encode()).unwrap(),
        spec,
        "round-trips"
    );
}

#[test]
fn decode_rejects_non_ascending_moments() {
    // A hand-crafted blob with two overrides at the same Moment (which `encode`
    // never emits — the map has unique keys) must still be rejected: the
    // untrusted-bytes guard. Build a valid two-Moment blob, then rewrite the
    // second (sentinel) Moment down to the first.
    let sentinel: u64 = 0x1122_3344_5566_7788;
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: environment::FaultPolicy::none(),
        overrides: BTreeMap::from([
            (3, Action::Guest(Answer::Nominal)),
            (sentinel, Action::Guest(Answer::Nominal)),
        ]),
        standing: vec![],
    };
    let mut bytes = spec.encode();
    let pat = sentinel.to_le_bytes();
    let pos = bytes
        .windows(8)
        .position(|w| w == pat)
        .expect("sentinel Moment present in the blob");
    bytes[pos..pos + 8].copy_from_slice(&3u64.to_le_bytes());
    assert_eq!(EnvSpec::decode(&bytes), Err(EnvError::Malformed));
}

#[test]
fn host_fault_decode_rejects_zero_clock_rate_denominator() {
    // A constructed Ratio can never hold den==0, but a mutated blob might. Encode
    // a SetClockRate(num/den), zero the denominator in place, expect Malformed.
    let f = HostFault::SetClockRate(environment::Ratio::new(3, 2).unwrap());
    let mut bytes = f.encode();
    // Layout: tag(1) + num:u64(8) + den:u64(8). Zero the den.
    for byte in bytes.iter_mut().skip(1 + 8).take(8) {
        *byte = 0;
    }
    assert_eq!(HostFault::decode(&bytes), Err(EnvError::Malformed));
}

#[test]
fn dev1_magic_is_rejected() {
    // A task-24 `DEV1` blob (the pre-amendment magic) must not silently decode
    // under the new `DEV2` layout — the magic differs, so it is rejected.
    let mut bytes = EnvSpec::Seeded {
        seed: 7,
        policy: environment::FaultPolicy::none(),
    }
    .encode();
    // Overwrite the 4-byte magic with "DEV1".
    bytes[0..4].copy_from_slice(b"DEV1");
    assert_eq!(EnvSpec::decode(&bytes), Err(EnvError::Malformed));
}
