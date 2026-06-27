// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — no order leakage. No `HashMap`/`HashSet` iteration reaches an
//! `Answer`/`Action` or an encoded byte: the override map is a `BTreeMap`,
//! eligible lists and the standing-fault vector are canonicalized before
//! encoding, and the disallowed-types clippy lint bans the hash containers
//! outright. These tests assert the *observable* consequence: how an input map
//! was built, or how a vector was ordered, changes neither the encoded bytes nor
//! the decided answer sequence.

mod common;

use std::collections::BTreeMap;

use common::{arb_overrides, arb_point, arb_policy, arb_spec, config, run_guest_schedule};
use environment::{
    Action, Answer, DecisionClass, EnvSpec, Fault, FaultPolicy, HostFault, Moment, StandingFault,
    VTime,
};
use proptest::prelude::*;

/// Reverse the standing vector of a spec (a no-op for `Seeded`). The override map
/// is a `BTreeMap`, so it has no input order to permute.
fn standing_reversed(spec: &EnvSpec) -> EnvSpec {
    match spec.clone() {
        EnvSpec::Recorded {
            seed,
            policy,
            overrides,
            mut standing,
        } => {
            standing.reverse();
            EnvSpec::Recorded {
                seed,
                policy,
                overrides,
                standing,
            }
        }
        s => s,
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// Reversing a spec's standing vector leaves its encoded bytes unchanged:
    /// encode walks canonical (sorted, deduplicated) order, so input order cannot
    /// reach a byte.
    #[test]
    fn standing_order_does_not_reach_bytes(spec in arb_spec()) {
        let spec = common::canon(spec);
        prop_assert_eq!(spec.encode(), standing_reversed(&spec).encode());
    }

    /// Building the same `Moment`→`Action` overrides in reversed insertion order
    /// yields byte-identical blobs — a `BTreeMap` is canonical by construction.
    #[test]
    fn override_map_build_order_does_not_reach_bytes(overrides in arb_overrides()) {
        let forward: BTreeMap<Moment, Action> = overrides.clone();
        let backward: BTreeMap<Moment, Action> =
            overrides.into_iter().rev().collect();
        let mk = |ov| EnvSpec::Recorded {
            seed: 5,
            policy: FaultPolicy::none(),
            overrides: ov,
            standing: vec![],
        };
        prop_assert_eq!(mk(forward).encode(), mk(backward).encode());
    }

    /// And build order does not reach a decided answer either. The same overrides
    /// inserted in reversed order produce the same Moment-stamped trace.
    #[test]
    fn override_map_build_order_does_not_reach_answers(
        seed in any::<u64>(),
        policy in arb_policy(),
        overrides in arb_overrides(),
        points in prop::collection::vec(arb_point(), 1..20),
    ) {
        let sched: Vec<(Moment, _)> = overrides
            .keys()
            .enumerate()
            .map(|(i, m)| (*m, points[i % points.len()]))
            .collect();

        let forward: BTreeMap<Moment, Action> = overrides.clone();
        let backward: BTreeMap<Moment, Action> = overrides.into_iter().rev().collect();
        let mk = |ov| EnvSpec::Recorded {
            seed,
            policy: policy.clone(),
            overrides: ov,
            standing: vec![],
        };

        let a = run_guest_schedule(&mut mk(forward).materialize(), &sched);
        let b = run_guest_schedule(&mut mk(backward).materialize(), &sched);
        prop_assert_eq!(a, b);
    }
}

#[test]
fn override_map_permutation_is_byte_identical() {
    let mk = |overrides: BTreeMap<Moment, Action>| EnvSpec::Recorded {
        seed: 99,
        policy: FaultPolicy::none(),
        overrides,
        standing: vec![],
    };

    let entries = [
        (1u64, Action::Guest(Answer::Nominal)),
        (5, Action::Guest(Answer::Supply(vec![1, 2, 3, 4]))),
        (9, Action::Host(HostFault::InjectInterrupt { vector: 7 })),
        (2, Action::Guest(Answer::Fault(Fault::NetDrop))),
    ];
    let forward: BTreeMap<Moment, Action> = entries.iter().cloned().collect();
    let shuffled: BTreeMap<Moment, Action> = entries.iter().rev().cloned().collect();
    assert_eq!(mk(forward).encode(), mk(shuffled).encode());
}

#[test]
fn standing_vec_permutation_is_byte_identical() {
    let sf = |c: DecisionClass, t: Vec<u8>, lo: u64, hi: u64| StandingFault {
        class: c,
        target: t,
        window: (VTime(lo), VTime(hi)),
    };
    let mk = |standing: Vec<StandingFault>| EnvSpec::Recorded {
        seed: 7,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::new(),
        standing,
    };
    let a = mk(vec![
        sf(DecisionClass::NetSend, vec![0, 1], 0, 10),
        sf(DecisionClass::BlockIo, vec![9], 5, 6),
    ]);
    let b = mk(vec![
        sf(DecisionClass::BlockIo, vec![9], 5, 6),
        sf(DecisionClass::NetSend, vec![0, 1], 0, 10),
    ]);
    assert_eq!(a.encode(), b.encode());
}

#[test]
fn eligible_order_does_not_reach_policy_bytes() {
    let mut a = FaultPolicy::none();
    a.set_class(
        DecisionClass::BlockIo,
        2,
        5,
        &[Fault::BlockNospc, Fault::BlockEio, Fault::BlockTorn(4)],
    )
    .unwrap();
    let mut b = FaultPolicy::none();
    b.set_class(
        DecisionClass::BlockIo,
        2,
        5,
        &[Fault::BlockTorn(4), Fault::BlockNospc, Fault::BlockEio],
    )
    .unwrap();
    assert_eq!(a.to_bytes(), b.to_bytes());
}
