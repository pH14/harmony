// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use common::{arb_policy, config};
use fault_policy::{DecisionClass, EnvError, Fault, FaultPolicy, Span};
use proptest::prelude::*;

proptest! {
    #![proptest_config(config(512))]

    #[test]
    fn policy_round_trips(p in arb_policy()) {
        let bytes = p.to_bytes();
        let back = FaultPolicy::from_bytes(&bytes).expect("our own encoding decodes");
        prop_assert_eq!(&p, &back);
        prop_assert_eq!(bytes, back.to_bytes());
    }

    #[test]
    fn from_bytes_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..256)) {
        let _ = FaultPolicy::from_bytes(&bytes);
    }
}

#[test]
fn equal_policies_built_differently_encode_identically() {
    let mut a = FaultPolicy::none();
    a.set_class(
        DecisionClass::NetFlow,
        1,
        4,
        &[
            Fault::NetReset,
            Fault::NetLatency(Span(5)),
            Fault::NetThrottle { bps: 1000 },
        ],
    )
    .unwrap();

    let mut b = FaultPolicy::none();
    b.set_class(
        DecisionClass::NetFlow,
        1,
        4,
        &[
            Fault::NetThrottle { bps: 1000 },
            Fault::NetLatency(Span(5)),
            Fault::NetReset,
        ],
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
    assert_eq!(
        p.set_class(DecisionClass::Entropy, 1, 2, &[]),
        Err(EnvError::Malformed)
    );
    assert_eq!(
        p.set_class(DecisionClass::NetFlow, 1, 0, &[]),
        Err(EnvError::Malformed)
    );
    assert_eq!(
        p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::BlockEio]),
        Err(EnvError::Malformed)
    );
}

#[test]
fn is_enforceable_only_admits_buggify_and_net_but_not_block_or_process() {
    let none = FaultPolicy::none();
    assert!(none.is_enforceable_only());
    assert!(none.is_buggify_only());

    let mut net = FaultPolicy::none();
    net.set_class(DecisionClass::NetFlow, 1, 1, &[Fault::NetReset])
        .unwrap();
    assert!(net.is_enforceable_only(), "net has a decide-seam enforcer");
    assert!(!net.is_buggify_only());

    let mut bug = FaultPolicy::none();
    bug.set_buggify_point(7, 1, 1).unwrap();
    assert!(bug.is_enforceable_only());
    assert!(bug.is_buggify_only());

    let mut both = FaultPolicy::none();
    both.set_buggify_point(7, 1, 1).unwrap();
    both.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetLatency(Span(5))])
        .unwrap();
    assert!(both.is_enforceable_only());
    assert!(!both.is_buggify_only());

    let mut block = FaultPolicy::none();
    block
        .set_class(DecisionClass::BlockIo, 1, 2, &[Fault::BlockEio])
        .unwrap();
    assert!(
        !block.is_enforceable_only(),
        "block has no decide-seam enforcer"
    );

    let mut proc = FaultPolicy::none();
    proc.set_class(DecisionClass::Process, 1, 2, &[Fault::ProcKill])
        .unwrap();
    assert!(!proc.is_enforceable_only());
}

#[test]
fn is_enforceable_only_rejects_fractional_netloss_but_keeps_binary() {
    let mut frac = FaultPolicy::none();
    frac.set_class(
        DecisionClass::NetFlow,
        1,
        1,
        &[Fault::NetLoss { num: 1, den: 3 }],
    )
    .unwrap();
    assert!(
        !frac.is_enforceable_only(),
        "a fractional NetLoss is not in-kernel enforceable"
    );

    let mut mixed = FaultPolicy::none();
    mixed
        .set_class(
            DecisionClass::NetFlow,
            1,
            2,
            &[Fault::NetReset, Fault::NetLoss { num: 2, den: 5 }],
        )
        .unwrap();
    assert!(!mixed.is_enforceable_only());

    let mut full = FaultPolicy::none();
    full.set_class(
        DecisionClass::NetFlow,
        1,
        1,
        &[Fault::NetLoss { num: 1, den: 1 }],
    )
    .unwrap();
    assert!(
        full.is_enforceable_only(),
        "a full drop is enforceable (nft drop)"
    );

    let mut binary = FaultPolicy::none();
    binary
        .set_class(
            DecisionClass::NetFlow,
            1,
            3,
            &[
                Fault::NetReset,
                Fault::NetLatency(Span(5)),
                Fault::NetThrottle { bps: 1000 },
            ],
        )
        .unwrap();
    assert!(binary.is_enforceable_only());
}

#[test]
fn from_bytes_rejects_off_version() {
    let mut bytes = FaultPolicy::none().to_bytes();
    bytes[4] = bytes[4].wrapping_add(9);
    match FaultPolicy::from_bytes(&bytes) {
        Err(EnvError::BadVersion(_)) => {}
        other => panic!("expected BadVersion, got {other:?}"),
    }
}

#[test]
fn from_bytes_rejects_stale_v1_net_policy() {
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    assert_eq!(
        bytes[4..6],
        3u16.to_le_bytes(),
        "current policy is version 3"
    );
    bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
    assert_eq!(
        FaultPolicy::from_bytes(&bytes),
        Err(EnvError::BadVersion(1)),
        "a v1 net policy must reject, never reinterpret an old net tag"
    );
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
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    for byte in bytes.iter_mut().skip(10).take(4) {
        *byte = 0;
    }
    assert_eq!(FaultPolicy::from_bytes(&bytes), Err(EnvError::Malformed));
}
