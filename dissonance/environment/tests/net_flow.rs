// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 50 — the `NetFlow` seam: per-flow network decisions, host-decided and
//! guest-enforced. Covers the two task-specific gates beyond the standard suite:
//!
//! 1. **Catalog replay + codec.** A recorded `NetFlow` answer sequence replays
//!    bit-identically through a `RecordedEnv`; the reshaped net-flow catalog
//!    (points, policy, every flow-policy `Fault`) round-trips through
//!    `EnvSpec::encode`/`decode`; per-variant golden wire bytes pin the codec; a
//!    stale task-45 (`v2`) blob is rejected, never reinterpreted.
//! 2. **Discriminant stability.** `DecisionClass::NetFlow as u16 == 4`, so
//!    `control-proto`'s `StopMask` bit (`1 << class_bit`) is unchanged across the
//!    `NetSend` → `NetFlow` rename; a round-trip through a `StopMask` arming the
//!    network class still selects it and nothing else.

mod common;

use std::collections::BTreeMap;

use common::{config, run_guest_schedule};
use environment::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvSpec, Fault, FaultPolicy,
    FlowEvent, Moment, NodeId, Outcome, VTime,
};
use proptest::prelude::*;

/// One `NetFlow` decision point on connection `c`.
fn flow(c: u64) -> P {
    P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(c),
        event: FlowEvent::Open,
    }
}

/// The four flow-level policies, in catalog order.
fn net_faults() -> Vec<Fault> {
    vec![
        Fault::NetLatency(VTime(100)),
        Fault::NetLoss { num: 1, den: 3 },
        Fault::NetThrottle { bps: 1_000_000 },
        Fault::NetReset,
    ]
}

// ---- Gate 2: discriminant stability + StopMask round-trip ------------------

/// `control-proto`'s `StopMask` is a `u32` bitset where the bit for a class is
/// `1 << class_bit` (the integrator-pinned mapping, mirrored locally per
/// conventions rule 2 — no sibling dependency). A `class_bit >= 32` is a
/// panic-free no-op, exactly as `StopMask::arm`/`armed` do.
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
    // The load-bearing pin: discriminant 4 is preserved across the rename, so
    // `control-proto`'s `StopMask` bit 4 still means "the network class".
    assert_eq!(DecisionClass::NetFlow as u16, 4);

    // The other discriminants are pinned too, so a reshape can never silently
    // shift a neighbour into the network slot.
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

    // The armed bit is exactly bit 4 (0x10) — unchanged from the per-frame
    // `NetSend` mapping, so the wire is unaffected by the rename.
    assert_eq!(mask, 1u32 << 4);
    assert!(armed(mask, net_bit), "the network class is armed");

    // Arming the network class selects it and nothing else.
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

// ---- Gate 1: catalog replay + codec ---------------------------------------

#[test]
fn net_fault_wire_bytes_are_pinned() {
    // Per-variant golden wire bytes for the flow policies — the stable
    // discriminants a recorded reproducer's replay depends on. The per-flow net
    // tags are FRESH (12..=15), disjoint from the retired per-frame net tags 0..=4
    // (now undefined), so a stale net byte rejects on every decode path.
    let cases: &[(Fault, &str)] = &[
        // ANS_FAULT(02) + F_NET_LATENCY(0c=12) + VTime u64 (100 = 0x64, LE).
        (Fault::NetLatency(VTime(100)), "020c6400000000000000"),
        // ANS_FAULT(02) + F_NET_LOSS(0d=13) + num u16 (1) + den u16 (3).
        (Fault::NetLoss { num: 1, den: 3 }, "020d01000300"),
        // ANS_FAULT(02) + F_NET_THROTTLE(0e=14) + bps u32 (1_000_000 = 0x0F4240, LE).
        (Fault::NetThrottle { bps: 1_000_000 }, "020e40420f00"),
        // ANS_FAULT(02) + F_NET_RESET(0f=15).
        (Fault::NetReset, "020f"),
    ];
    for (f, expected) in cases {
        let enc = Answer::Fault(*f).encode();
        let hex: String = enc.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(&hex, expected, "wire bytes drifted for {f:?}");
        // And the bytes decode back to the same fault.
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
    // Nominal is admissible (deliver normally); a foreign-class fault is not.
    assert!(point.admits(&Answer::Nominal));
    assert!(!point.admits(&Answer::Fault(Fault::BlockEio)));
    assert!(!point.admits(&Answer::Supply(vec![1, 2, 3, 4])));
}

#[test]
fn stale_v2_blob_is_rejected_not_reinterpreted() {
    // The BLOB_VERSION bump (2 → 3) makes a task-45 `v2` blob reject with
    // BadVersion rather than silently reinterpret an old per-frame net fault as a
    // new flow policy. Build a current blob, rewrite its version field to 2.
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([(1, Action::Guest(Answer::Fault(Fault::NetReset)))]),
        standing: vec![],
    };
    let mut bytes = spec.encode();
    // Layout: magic:u32 then version:u16. The current version is BLOB_VERSION (3).
    assert_eq!(bytes[4..6], 3u16.to_le_bytes(), "current blob is version 3");
    bytes[4..6].copy_from_slice(&2u16.to_le_bytes());
    assert_eq!(
        EnvSpec::decode(&bytes),
        Err(environment::EnvError::BadVersion(2)),
        "a v2 blob must reject, never reinterpret an old net fault"
    );
}

#[test]
fn retired_net_tags_reject_on_every_ungated_decode_path() {
    // Root-cause fix (review r2): the reshaped per-flow net faults got FRESH byte
    // tags (12..=15); the retired per-frame net tags 0..=4 (`NetDrop`/`NetDelay`/
    // `NetReorder`/`NetDup`/`NetCorrupt`) are UNDEFINED. So a stale byte carrying an
    // old net tag hard-fails at `read_fault` on EVERY decode path — no per-path
    // version guard needed. The dangerous case was old `NetDup` (tag 3,
    // payload-free): under the round-1 numbering it was byte-identical to a reused
    // tag-3 variant and silently reinterpreted; now tag 3 is undefined.
    //
    // These paths are NOT version-gated (or are reached with a current version), so
    // they prove the *tag* check — not the version bump — closes the hazard.
    for old_tag in 0u8..=4 {
        // (1) Answer::decode — also the path control-proto's `Run { resolve }` uses
        // (vmm-core decodes the opaque resolve bytes through `Answer::decode`).
        // ANS_FAULT = 2, then the retired fault tag.
        assert_eq!(
            Answer::decode(&[2, old_tag]),
            Err(environment::EnvError::Malformed),
            "Answer::decode (and thus Run::resolve) must reject retired net tag {old_tag}"
        );

        // (2) Action::decode — the `EnvSpec` override value path (ACT_GUEST = 1,
        // then the Answer above).
        assert_eq!(
            Action::decode(&[1, 2, old_tag]),
            Err(environment::EnvError::Malformed),
            "Action::decode must reject retired net tag {old_tag}"
        );
    }

    // (3) Standalone FaultPolicy net-eligible list, at the CURRENT version — so it
    // is the undefined-tag check, not the version gate, that rejects. Encode a
    // valid policy whose net eligible set is [NetReset] (tag 15 = 0x0f, the sole
    // 0x0f in the blob), then rewrite that tag to the retired NetDup tag (3).
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    let pos = bytes
        .iter()
        .position(|&b| b == 0x0f)
        .expect("the NetReset tag (0x0f) is present");
    bytes[pos] = 3; // retired NetDup tag
    assert_eq!(
        FaultPolicy::from_bytes(&bytes),
        Err(environment::EnvError::Malformed),
        "a current-version FaultPolicy carrying a retired net tag is rejected by the tag check"
    );
}

proptest! {
    #![proptest_config(config(256))]

    /// A `NetFlow` decision sequence answered by a `RecordedEnv` reproduces
    /// bit-identically: two materializations of the same spec over the same
    /// flow-stamped schedule give the same answer trace. Overrides mix admissible
    /// net faults (which fire) with arbitrary moments (which fall through to the
    /// seeded base under a fault-heavy net policy).
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

        // Guest overrides: a net fault (NetReset, always admissible) at each
        // chosen Moment; the seeded base answers everywhere else.
        let overrides: BTreeMap<Moment, Action> = fault_moments
            .iter()
            .map(|m| (*m, Action::Guest(Answer::Fault(Fault::NetReset))))
            .collect();
        let spec = EnvSpec::Recorded { seed, policy, overrides, standing: vec![] };

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

    /// The reshaped net-flow catalog round-trips through `EnvSpec::encode`/`decode`:
    /// a spec whose overrides and policy are all net-flow faults re-encodes
    /// byte-stably and decodes back unchanged.
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
        let spec = EnvSpec::Recorded { seed, policy, overrides, standing: vec![] };

        let bytes = spec.encode();
        let back = EnvSpec::decode(&bytes).expect("our own encoding decodes");
        prop_assert_eq!(&spec, &back);
        prop_assert_eq!(bytes, back.encode(), "re-encoding is byte-stable");
    }
}
