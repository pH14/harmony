// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 5 — `FaultPolicy` byte-determinism. `to_bytes` is identical for equal
//! policies; `from_bytes` round-trips; malformed input errors cleanly (never a
//! panic).

mod common;

use common::{arb_policy, config};
use environment::{DecisionClass, EnvError, Fault, FaultPolicy, Span};
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
    // Supply class never faults.
    assert_eq!(
        p.set_class(DecisionClass::Entropy, 1, 2, &[]),
        Err(EnvError::Malformed)
    );
    // Zero denominator.
    assert_eq!(
        p.set_class(DecisionClass::NetFlow, 1, 0, &[]),
        Err(EnvError::Malformed)
    );
    // Foreign-class fault in a class's eligible set.
    assert_eq!(
        p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::BlockEio]),
        Err(EnvError::Malformed)
    );
}

/// `is_enforceable_only` (task 61) accepts a policy whose faults are limited to the
/// classes that have a live decide-seam enforcer — buggify (SDK) and/or net (the
/// flow agent) — and rejects one that faults the still-unenforced block/process
/// classes. It is the task-73 `is_buggify_only` predicate widened for the net
/// vertical.
#[test]
fn is_enforceable_only_admits_buggify_and_net_but_not_block_or_process() {
    // The empty policy: trivially enforceable (and buggify-only).
    let none = FaultPolicy::none();
    assert!(none.is_enforceable_only());
    assert!(none.is_buggify_only());

    // A net-only policy: enforceable now (the flow agent), but NOT buggify-only.
    let mut net = FaultPolicy::none();
    net.set_class(DecisionClass::NetFlow, 1, 1, &[Fault::NetReset])
        .unwrap();
    assert!(net.is_enforceable_only(), "net has a decide-seam enforcer");
    assert!(!net.is_buggify_only());

    // A buggify-only policy: enforceable (and buggify-only).
    let mut bug = FaultPolicy::none();
    bug.set_buggify_point(7, 1, 1).unwrap();
    assert!(bug.is_enforceable_only());
    assert!(bug.is_buggify_only());

    // Buggify + net together: still enforceable (both seams live).
    let mut both = FaultPolicy::none();
    both.set_buggify_point(7, 1, 1).unwrap();
    both.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetLatency(Span(5))])
        .unwrap();
    assert!(both.is_enforceable_only());
    assert!(!both.is_buggify_only());

    // A block fault (no decide-seam yet): NOT enforceable.
    let mut block = FaultPolicy::none();
    block
        .set_class(DecisionClass::BlockIo, 1, 2, &[Fault::BlockEio])
        .unwrap();
    assert!(
        !block.is_enforceable_only(),
        "block has no decide-seam enforcer"
    );

    // A process fault: NOT enforceable.
    let mut proc = FaultPolicy::none();
    proc.set_class(DecisionClass::Process, 1, 2, &[Fault::ProcKill])
        .unwrap();
    assert!(!proc.is_enforceable_only());
}

/// Fractional `NetLoss` (`0 < num < den`) is NOT enforceable by the in-kernel
/// prototype (it needs the deferred 61b proxy), so a policy whose net class could
/// sample it is rejected fail-loud — while the binary net faults (full drop,
/// reset, latency, throttle) stay enforceable.
#[test]
fn is_enforceable_only_rejects_fractional_netloss_but_keeps_binary() {
    // Fractional loss 1/3 in the eligible set → not enforceable.
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

    // A fractional loss anywhere in a multi-fault eligible set still taints it.
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

    // Full drop (num >= den) is a binary drop → enforceable.
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

    // Reset / latency / throttle are all binary → enforceable.
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
    // Layout: magic:u32 then version:u16.
    bytes[4] = bytes[4].wrapping_add(9);
    match FaultPolicy::from_bytes(&bytes) {
        Err(EnvError::BadVersion(_)) => {}
        other => panic!("expected BadVersion, got {other:?}"),
    }
}

#[test]
fn from_bytes_rejects_stale_v1_net_policy() {
    // Task 50: the network `Fault` byte tags were reshaped (per-frame → per-flow),
    // so a task-45 `v1` policy blob must reject rather than silently reinterpret an
    // old net fault under the new tag vocabulary — the symmetric codec to the
    // EnvSpec BLOB_VERSION gate. The hazard is concrete for the reused payload-free
    // tag 3: old `NetDup` (tag 3) and new `NetReset` (tag 3) are byte-identical, so
    // a stale blob would stay byte-aligned and decode to the wrong fault.
    //
    // Build a current (v2) policy whose eligible set uses tag 3 (`NetReset`), then
    // rewrite the version field down to 1 — exactly the bytes an old recorder emitted
    // for a `NetDup`-eligible net policy.
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    // Layout: magic:u32 (0..4) then version:u16 (4..6). The current version is 3
    // (task 73 added the trailing buggify section; a stale v1/v2 blob must still
    // reject rather than reinterpret an old net tag).
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
    // Encode a policy with den=2, then zero the NetFlow denominator in place.
    // Layout after magic(4)+version(2): net{ num:u32, den:u32, count:u32, ... }.
    let mut p = FaultPolicy::none();
    p.set_class(DecisionClass::NetFlow, 1, 2, &[Fault::NetReset])
        .unwrap();
    let mut bytes = p.to_bytes();
    // num at offset 6..10, den at 10..14.
    for byte in bytes.iter_mut().skip(10).take(4) {
        *byte = 0;
    }
    assert_eq!(FaultPolicy::from_bytes(&bytes), Err(EnvError::Malformed));
}
