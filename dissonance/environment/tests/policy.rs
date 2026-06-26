// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — `FaultPolicy` byte-determinism. `to_bytes` is identical for equal
//! policies; `from_bytes` round-trips; malformed input errors cleanly (never a
//! panic).

mod common;

use common::{arb_policy, config};
use environment::{DecisionClass, EnvError, Fault, FaultPolicy, VTime};
use proptest::prelude::*;

proptest! {
    #![proptest_config(config(512))]

    /// `from_bytes(to_bytes(p)) == p` and `to_bytes` is byte-stable.
    #[test]
    fn policy_round_trips(p in arb_policy()) {
        let bytes = p.to_bytes();
        let back = FaultPolicy::from_bytes(&bytes).expect("our own encoding decodes");
        prop_assert_eq!(&p, &back);
        prop_assert_eq!(bytes, back.to_bytes());
    }

    /// `from_bytes` is total on arbitrary bytes.
    #[test]
    fn from_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = FaultPolicy::from_bytes(&bytes);
    }
}

#[test]
fn equal_policies_built_differently_encode_identically() {
    // Same probabilities and eligible sets, but the eligible faults are listed
    // in different orders → identical bytes (the eligible list is canonicalized).
    let mut a = FaultPolicy::none();
    a.set_class(
        DecisionClass::NetSend,
        1,
        4,
        &[Fault::NetDup, Fault::NetDrop, Fault::NetDelay(VTime(5))],
    )
    .unwrap();

    let mut b = FaultPolicy::none();
    b.set_class(
        DecisionClass::NetSend,
        1,
        4,
        &[Fault::NetDelay(VTime(5)), Fault::NetDrop, Fault::NetDup],
    )
    .unwrap();

    assert_eq!(a, b);
    assert_eq!(a.to_bytes(), b.to_bytes());
}

#[test]
fn duplicate_eligible_faults_are_deduplicated() {
    let mut p = FaultPolicy::none();
    p.set_class(
        DecisionClass::Process,
        1,
        2,
        &[Fault::ProcKill, Fault::ProcKill, Fault::ProcKill],
    )
    .unwrap();
    let mut q = FaultPolicy::none();
    q.set_class(DecisionClass::Process, 1, 2, &[Fault::ProcKill])
        .unwrap();
    assert_eq!(p, q);
    assert_eq!(p.to_bytes(), q.to_bytes());
}

#[test]
fn set_class_rejects_misuse() {
    let mut p = FaultPolicy::none();
    // Supply class never faults.
    assert_eq!(
        p.set_class(DecisionClass::Entropy, 1, 2, &[]),
        Err(EnvError::Malformed)
    );
    // Zero denominator.
    assert_eq!(
        p.set_class(DecisionClass::NetSend, 1, 0, &[]),
        Err(EnvError::Malformed)
    );
    // Foreign-class fault in a class's eligible set.
    assert_eq!(
        p.set_class(DecisionClass::NetSend, 1, 2, &[Fault::BlockEio]),
        Err(EnvError::Malformed)
    );
}

#[test]
fn from_bytes_rejects_off_version() {
    let mut bytes = FaultPolicy::none().to_bytes();
    // Layout: magic:u32 then version:u16.
    bytes[4] = bytes[4].wrapping_add(9);
    match FaultPolicy::from_bytes(&bytes) {
        Err(EnvError::BadVersion(_)) => {}
        other => panic!("expected BadVersion, got {other:?}"),
    }
}

#[test]
fn from_bytes_rejects_bad_magic_and_trailing_bytes() {
    let good = FaultPolicy::none().to_bytes();

    let mut bad_magic = good.clone();
    bad_magic[0] ^= 0xFF;
    assert_eq!(
        FaultPolicy::from_bytes(&bad_magic),
        Err(EnvError::Malformed)
    );

    let mut trailing = good.clone();
    trailing.push(0);
    assert_eq!(FaultPolicy::from_bytes(&trailing), Err(EnvError::Malformed));
}

#[test]
fn from_bytes_rejects_zero_denominator() {
    // Encode a policy with den=2, then zero the NetSend denominator in place.
    // Layout after magic(4)+version(2): net{ num:u32, den:u32, count:u32, ... }.
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetSend, 1, 2, &[Fault::NetDrop])
        .unwrap();
    let mut bytes = p.to_bytes();
    // num at offset 6..10, den at 10..14.
    for byte in bytes.iter_mut().skip(10).take(4) {
        *byte = 0;
    }
    assert_eq!(FaultPolicy::from_bytes(&bytes), Err(EnvError::Malformed));
}
