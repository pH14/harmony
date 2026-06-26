// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 6 — no order leakage. No `HashMap`/`HashSet` iteration reaches an
//! `Answer` or an encoded byte: the override map is a `BTreeMap`, eligible lists
//! and the override/standing vectors are canonicalized before encoding, and the
//! disallowed-types clippy lint bans the hash containers outright. These tests
//! assert the *observable* consequence: permuting any input vector changes
//! neither the encoded bytes nor the decided answer sequence.

mod common;

use common::{arb_answer, arb_point, arb_policy, arb_spec, config};
use environment::{
    Answer, DecisionClass, DecisionId, EnvSpec, Environment, Fault, FaultPolicy, Outcome,
    StandingFault, VTime,
};
use proptest::prelude::*;

/// Reverse the order of the override and standing vectors of a spec (a no-op for
/// `Seeded`).
fn reversed(spec: &EnvSpec) -> EnvSpec {
    match spec.clone() {
        EnvSpec::Recorded {
            seed,
            policy,
            mut overrides,
            mut standing,
        } => {
            overrides.reverse();
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

    /// Reversing a spec's `Vec`s leaves its encoded bytes unchanged: encode walks
    /// canonical (sorted, deduplicated) order, so the input order cannot reach a
    /// byte. `canon` first makes ids unique (last-wins, as encode does), so the
    /// reversal here is a pure reordering with no duplicate-resolution ambiguity.
    #[test]
    fn vec_order_does_not_reach_bytes(spec in arb_spec()) {
        let spec = common::canon(spec);
        prop_assert_eq!(spec.encode(), reversed(&spec).encode());
    }

    /// And it does not reach a decided answer either. The override ids are
    /// **unique and in range**, so they actually fire across the whole sequence;
    /// each spec is materialized **once** and the full point sequence run through
    /// it, so a regression where vector order changes a *later* decision's id
    /// (not just id 0) is caught.
    #[test]
    fn vec_order_does_not_reach_answers(
        seed in any::<u64>(),
        policy in arb_policy(),
        (seq, overrides) in prop::collection::vec(arb_point(), 1..30).prop_flat_map(|seq| {
            let len = seq.len() as u64;
            // Unique ids (BTreeMap keys) within the sequence range.
            let ov = prop::collection::btree_map(0u64..len, arb_answer(), 0..=seq.len());
            (Just(seq), ov)
        }),
    ) {
        let forward: Vec<(DecisionId, Answer)> =
            overrides.iter().map(|(k, v)| (DecisionId(*k), v.clone())).collect();
        let mut backward = forward.clone();
        backward.reverse();
        let mk = |ov| EnvSpec::Recorded {
            seed,
            policy: policy.clone(),
            overrides: ov,
            standing: vec![],
        };

        let mut env_a = mk(forward).materialize();
        let a: Vec<Outcome> = seq.iter().map(|p| env_a.decide(p)).collect();
        let mut env_b = mk(backward).materialize();
        let b: Vec<Outcome> = seq.iter().map(|p| env_b.decide(p)).collect();
        prop_assert_eq!(a, b);
    }
}

#[test]
fn override_vec_permutation_is_byte_identical() {
    let policy = FaultPolicy::none();
    let mk = |overrides: Vec<(DecisionId, Answer)>| EnvSpec::Recorded {
        seed: 99,
        policy: policy.clone(),
        overrides,
        standing: vec![],
    };

    let forward = mk(vec![
        (DecisionId(1), Answer::Nominal),
        (DecisionId(5), Answer::Supply(vec![1, 2, 3, 4])),
        (DecisionId(9), Answer::Fault(Fault::NetDrop)),
    ]);
    let shuffled = mk(vec![
        (DecisionId(9), Answer::Fault(Fault::NetDrop)),
        (DecisionId(1), Answer::Nominal),
        (DecisionId(5), Answer::Supply(vec![1, 2, 3, 4])),
    ]);
    assert_eq!(forward.encode(), shuffled.encode());
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
        overrides: vec![],
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
