// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use std::collections::BTreeMap;

use common::{config, run_guest_schedule};
use fault_policy::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvSpec, Fault, FaultPolicy,
    FlowEvent, Moment, NodeId, Outcome, Span,
};
use proptest::prelude::*;

fn flow(c: u64) -> P {
    P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(c),
        event: FlowEvent::Open,
    }
}

fn net_faults() -> Vec<Fault> {
    vec![
        Fault::NetLatency(Span(100)),
        Fault::NetLoss { num: 1, den: 3 },
        Fault::NetThrottle { bps: 1_000_000 },
        Fault::NetReset,
    ]
}

fn arm(mask: u32, class_bit: u16) -> u32 {
    match 1u32.checked_shl(u32::from(class_bit)) {
        Some(bit) => mask | bit,
        None => mask,
    }
}
fn armed(mask: u32, class_bit: u16) -> bool {
    match 1u32.checked_shl(u32::from(class_bit)) {
        Some(bit) => mask & bit != 0,
        None => false,
    }
}

#[test]
fn netflow_discriminant_is_four() {
    assert_eq!(DecisionClass::NetFlow as u16, 4);

    assert_eq!(DecisionClass::Entropy as u16, 1);
    assert_eq!(DecisionClass::Payload as u16, 2);
    assert_eq!(DecisionClass::Scheduler as u16, 3);
    assert_eq!(DecisionClass::BlockIo as u16, 5);
    assert_eq!(DecisionClass::Process as u16, 6);
}

#[test]
fn stopmask_arming_network_class_selects_netflow_only() {
    let net_bit = DecisionClass::NetFlow as u16;
    let mask = arm(0, net_bit);

    assert_eq!(mask, 1u32 << 4);
    assert!(armed(mask, net_bit), "the network class is armed");

    for other in [
        DecisionClass::Entropy as u16,
        DecisionClass::Payload as u16,
        DecisionClass::Scheduler as u16,
        DecisionClass::BlockIo as u16,
        DecisionClass::Process as u16,
    ] {
        assert!(
            !armed(mask, other),
            "only the network class is armed (class_bit {other} must be clear)"
        );
    }
}

#[test]
fn net_fault_wire_bytes_are_pinned() {
    let cases: &[(Fault, &str)] = &[
        (Fault::NetLatency(Span(100)), "020c6400000000000000"),
        (Fault::NetLoss { num: 1, den: 3 }, "020d01000300"),
        (Fault::NetThrottle { bps: 1_000_000 }, "020e40420f00"),
        (Fault::NetReset, "020f"),
    ];
    for (f, expected) in cases {
        let enc = Answer::Fault(*f).encode();
        let hex: String = enc.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(&hex, expected, "wire bytes drifted for {f:?}");
        assert_eq!(Answer::decode(&enc).unwrap(), Answer::Fault(*f));
    }
}

#[test]
fn every_net_fault_is_netflow_class_and_admissible() {
    let point = flow(0);
    for f in net_faults() {
        assert_eq!(
            f.class(),
            DecisionClass::NetFlow,
            "{f:?} is a NetFlow fault"
        );
        assert!(
            point.admits(&Answer::Fault(f)),
            "{f:?} is admissible on a NetFlow point (no point-relative bound)"
        );
    }
    assert!(point.admits(&Answer::Nominal));
    assert!(!point.admits(&Answer::Fault(Fault::BlockEio)));
    assert!(!point.admits(&Answer::Supply(vec![1, 2, 3, 4])));
}

#[test]
fn stale_v2_blob_is_rejected_not_reinterpreted() {
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([(1, Action::Guest(Answer::Fault(Fault::NetReset)))]),
        standing: vec![],
        reseeds: std::collections::BTreeMap::new(),
        payloads: None,
    };
    let mut bytes = spec.encode();
    assert_eq!(
        bytes[4..6],
        EnvSpec::BLOB_VERSION.to_le_bytes(),
        "current blob is at BLOB_VERSION"
    );
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        EnvSpec::decode(&bytes),
        Err(fault_policy::EnvError::BadVersion(2)),
        "a v2 blob must reject, never reinterpret an old net fault"
    );
}

#[test]
fn a_v4_blob_is_rejected_at_the_version_gate() {
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides: std::collections::BTreeMap::new(),
        standing: vec![],
        reseeds: std::collections::BTreeMap::new(),
        payloads: None,
    };
    let mut bytes = spec.encode();
    bytes[4..6].copy_from_slice(&4u16.to_le_bytes());
    bytes.truncate(6);
    assert_eq!(
        EnvSpec::decode(&bytes),
        Err(fault_policy::EnvError::BadVersion(4)),
        "a v4 blob rejects at the version gate, not mid-parse"
    );
}

#[test]
fn retired_net_tags_reject_on_every_ungated_decode_path() {
    for old_tag in 0u8..=4 {
        assert_eq!(
            Answer::decode(&[2, old_tag]),
            Err(fault_policy::EnvError::Malformed),
            "Answer::decode (and thus Run::resolve) must reject retired net tag {old_tag}"
        );

        assert_eq!(
            Action::decode(&[1, 2, old_tag]),
            Err(fault_policy::EnvError::Malformed),
            "Action::decode must reject retired net tag {old_tag}"
        );
    }

    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    let pos = bytes
        .iter()
        .position(|&b| b == 0x0f)
        .expect("the NetReset tag (0x0f) is present");
    bytes[pos] = 3;
    assert_eq!(
        FaultPolicy::from_bytes(&bytes),
        Err(fault_policy::EnvError::Malformed),
        "a current-version FaultPolicy carrying a retired net tag is rejected by the tag check"
    );
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn netflow_recorded_replays_bit_identically(
        seed in any::<u64>(),
        net in (any::<u32>(), 1u32..=u32::MAX, prop::collection::vec(common::arb_net_fault(), 0..5)),
        conns in prop::collection::vec(any::<u64>(), 1..24),
        fault_moments in prop::collection::btree_set(0u64..64, 0..8),
    ) {
        let mut policy = FaultPolicy::none();
        policy.set_class(DecisionClass::NetFlow, net.0, net.1, &net.2)
            .expect("net is a fault class with in-class faults");

        let overrides: BTreeMap<Moment, Action> = fault_moments
            .iter()
            .map(|m| (*m, Action::Guest(Answer::Fault(Fault::NetReset))))
            .collect();
        let spec = EnvSpec::Recorded { seed, policy, overrides, standing: vec![], reseeds: Default::default(), payloads: None };

        let sched: Vec<(Moment, P)> = conns.iter().enumerate()
            .map(|(i, c)| (i as u64, flow(*c)))
            .collect();

        let a = run_guest_schedule(&mut spec.materialize(), &sched);
        let b = run_guest_schedule(&mut spec.materialize(), &sched);
        prop_assert_eq!(&a, &b, "the NetFlow trace replays bit-identically");
        for o in &a {
            prop_assert!(matches!(o, Outcome::Resolved(_)), "a pure backing never suspends");
        }
    }

    #[test]
    fn netflow_catalog_round_trips(
        seed in any::<u64>(),
        net in (any::<u32>(), 1u32..=u32::MAX, prop::collection::vec(common::arb_net_fault(), 0..5)),
        overrides in prop::collection::btree_map(
            any::<u64>(),
            common::arb_net_fault().prop_map(|f| Action::Guest(Answer::Fault(f))),
            0..10,
        ),
    ) {
        let mut policy = FaultPolicy::none();
        policy.set_class(DecisionClass::NetFlow, net.0, net.1, &net.2)
            .expect("net is a fault class with in-class faults");
        let spec = EnvSpec::Recorded { seed, policy, overrides, standing: vec![], reseeds: Default::default(), payloads: None };

        let bytes = spec.encode();
        let back = EnvSpec::decode(&bytes).expect("our own encoding decodes");
        prop_assert_eq!(&spec, &back);
        prop_assert_eq!(bytes, back.encode(), "re-encoding is byte-stable");
    }
}
