// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use std::collections::BTreeMap;

use common::{arb_overrides, arb_point, arb_policy, arb_spec, config, run_guest_schedule};
use fault_policy::{
    Action, Answer, DecisionClass, EnvSpec, Fault, FaultPolicy, HostFault, Moment, StandingFault,
};
use proptest::prelude::*;

fn standing_reversed(spec: &EnvSpec) -> EnvSpec {
    match spec.clone() {
        EnvSpec::Recorded {
            seed,
            policy,
            overrides,
            mut standing,
            reseeds,
            payloads,
        } => {
            standing.reverse();
            EnvSpec::Recorded {
                seed,
                policy,
                overrides,
                standing,
                reseeds,
                payloads,
            }
        }
        s => s,
    }
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn standing_order_does_not_reach_bytes(spec in arb_spec()) {
        let spec = common::canon(spec);
        prop_assert_eq!(spec.encode(), standing_reversed(&spec).encode());
    }

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
            reseeds: std::collections::BTreeMap::new(), payloads: None,
        };
        prop_assert_eq!(mk(forward).encode(), mk(backward).encode());
    }

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
            reseeds: std::collections::BTreeMap::new(), payloads: None,
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
        reseeds: std::collections::BTreeMap::new(),
        payloads: None,
    };

    let entries = [
        (1u64, Action::Guest(Answer::Nominal)),
        (5, Action::Guest(Answer::Supply(vec![1, 2, 3, 4]))),
        (9, Action::Host(HostFault::InjectInterrupt { vector: 7 })),
        (2, Action::Guest(Answer::Fault(Fault::NetReset))),
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
        window: (lo, hi),
    };
    let mk = |standing: Vec<StandingFault>| EnvSpec::Recorded {
        seed: 7,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::new(),
        standing,
        reseeds: Default::default(),
        payloads: None,
    };
    let a = mk(vec![
        sf(DecisionClass::NetFlow, vec![0, 1], 0, 10),
        sf(DecisionClass::BlockIo, vec![9], 5, 6),
    ]);
    let b = mk(vec![
        sf(DecisionClass::BlockIo, vec![9], 5, 6),
        sf(DecisionClass::NetFlow, vec![0, 1], 0, 10),
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
