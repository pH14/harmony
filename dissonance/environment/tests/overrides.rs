// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — override semantics. For every overridden `Moment` a **guest**
//! override wins **iff** its `Answer` is admissible for the decision surfacing at
//! that `Moment`; an inadmissible (or host-plane) override is deterministically
//! ignored (the seeded base answers); every other decision is the seeded base.
//! The general property checks the implementation against an independent
//! restatement of the rule ([`ref_admissible`]); targeted cases pin each
//! inadmissibility class.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_point, arb_policy, config, ref_admissible};
use environment::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvSpec, Environment, Fault,
    FaultPolicy, HostFault, Moment, NodeId, Outcome, RecordedEnv, SeededEnv,
};
use proptest::prelude::*;

/// Build a materialized env from a seed/policy and a single **guest** override at
/// `Moment` `at`.
fn one_guest_override(seed: u64, policy: FaultPolicy, at: Moment, ans: Answer) -> RecordedEnv {
    EnvSpec::Recorded {
        seed,
        policy,
        overrides: BTreeMap::from([(at, Action::Guest(ans))]),
        standing: vec![],
    }
    .materialize()
}

proptest! {
    #![proptest_config(config(256))]

    /// The public `DecisionPoint::admits` (the single source of truth the
    /// `RecordedEnv` and the frontier both use) agrees with the independent
    /// restatement of the rule for every point/answer pairing.
    #[test]
    fn admits_matches_reference(p in arb_point(), ans in common::arb_answer()) {
        prop_assert_eq!(p.admits(&ans), ref_admissible(&p, &ans));
    }

    /// The general rule, cross-checked against `ref_admissible` and an
    /// independently-advanced seeded base. Each decision `i` runs at `Moment i`;
    /// overrides (host or guest) are keyed by `Moment`. A guest override fires
    /// iff admissible (consuming no PRNG); a host override or an inadmissible one
    /// falls through to the base.
    #[test]
    fn override_wins_iff_admissible(
        seed in any::<u64>(),
        policy in arb_policy(),
        (seq, overrides) in prop::collection::vec(arb_point(), 1..30).prop_flat_map(|seq| {
            let len = seq.len() as u64;
            let ov = prop::collection::btree_map(0u64..len, arb_action(), 0..=seq.len());
            (Just(seq), ov)
        }),
    ) {
        let spec = EnvSpec::Recorded {
            seed,
            policy: policy.clone(),
            overrides: overrides.clone(),
            standing: vec![],
        };
        let mut env = spec.materialize();
        // The reference base advances only when the base actually answers.
        let mut base = SeededEnv::new(seed, policy);

        for (i, p) in seq.iter().enumerate() {
            let at = i as u64;
            // Only an admissible *guest* override fires; a host action at this
            // Moment is filtered out of `decide`.
            let guest = overrides.get(&at).and_then(Action::guest_answer);
            let expected = match guest {
                Some(a) if ref_admissible(p, a) => a.clone(),
                _ => match base.decide(p) {
                    Outcome::Resolved(ans) => ans,
                    Outcome::NeedsHost => unreachable!("seeded base never suspends"),
                },
            };
            env.set_moment(at);
            prop_assert_eq!(env.decide(p), Outcome::Resolved(expected));
        }
    }
}

/// An admissible guest override fires at exactly its `Moment` and nowhere else —
/// and consumes no PRNG, so the base stays in lockstep for every other Moment.
#[test]
fn override_fires_at_its_moment_only() {
    let seed = 42;
    let policy = FaultPolicy::none();
    let seq: Vec<P> = (0..10).map(|_| P::Scheduler { ready: 8 }).collect();

    // A recognizable admissible scheduler selection (index 3) at Moment 5.
    let marker = Answer::Supply(3u32.to_le_bytes().to_vec());
    let mut env = one_guest_override(seed, policy.clone(), 5, marker.clone());
    let mut base = SeededEnv::new(seed, policy);

    for (i, p) in seq.iter().enumerate() {
        env.set_moment(i as u64);
        let got = env.decide(p);
        if i == 5 {
            // The override fires here and does NOT advance the base, so `base`
            // stays in lockstep with `env`'s base for the remaining decisions.
            assert_eq!(
                got,
                Outcome::Resolved(marker.clone()),
                "override at its Moment"
            );
        } else {
            assert_eq!(got, base.decide(p), "base everywhere else");
        }
    }
}

// ---- targeted inadmissibility cases (override at Moment 0, no prior shift) ----

/// Run `point` at `Moment 0` with a single guest `override0` installed, and a
/// parallel bare `SeededEnv`; return `(env answer, base answer)`.
fn first(point: &P, seed: u64, policy: FaultPolicy, override0: Answer) -> (Answer, Answer) {
    let mut env = one_guest_override(seed, policy.clone(), 0, override0);
    env.set_moment(0);
    let mut base = SeededEnv::new(seed, policy);
    let Outcome::Resolved(got) = env.decide(point) else {
        unreachable!()
    };
    let Outcome::Resolved(base_ans) = base.decide(point) else {
        unreachable!()
    };
    (got, base_ans)
}

#[test]
fn supply_length_mismatch_is_ignored() {
    // Entropy{32} with a 1-byte Supply override → ignored, base answers (32 bytes).
    let (got, base) = first(
        &P::Entropy { bytes: 32 },
        7,
        FaultPolicy::none(),
        Answer::Supply(vec![0xAB]),
    );
    assert_eq!(got, base);
    assert!(matches!(&got, Answer::Supply(v) if v.len() == 32));
}

#[test]
fn exact_length_supply_is_admissible() {
    let supply = Answer::Supply((0..16u8).collect());
    let (got, _base) = first(
        &P::Entropy { bytes: 16 },
        7,
        FaultPolicy::none(),
        supply.clone(),
    );
    assert_eq!(got, supply, "an exact-length Supply wins");
}

#[test]
fn scheduler_three_byte_supply_is_ignored() {
    let (got, base) = first(
        &P::Scheduler { ready: 5 },
        7,
        FaultPolicy::none(),
        Answer::Supply(vec![0, 0, 0]),
    );
    assert_eq!(got, base);
}

#[test]
fn scheduler_out_of_range_index_is_ignored() {
    let (got, base) = first(
        &P::Scheduler { ready: 5 },
        7,
        FaultPolicy::none(),
        Answer::Supply(7u32.to_le_bytes().to_vec()),
    );
    assert_eq!(got, base, "selection >= ready is ignored");
}

#[test]
fn scheduler_in_range_index_is_admissible() {
    let sel = Answer::Supply(2u32.to_le_bytes().to_vec());
    let (got, _base) = first(
        &P::Scheduler { ready: 5 },
        7,
        FaultPolicy::none(),
        sel.clone(),
    );
    assert_eq!(got, sel);
}

#[test]
fn wrong_class_fault_is_ignored() {
    // A BlockEio fault on a NetFlow point → wrong class → ignored.
    let net = P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(3),
        event: environment::FlowEvent::Open,
    };
    let (got, base) = first(&net, 7, FaultPolicy::none(), Answer::Fault(Fault::BlockEio));
    assert_eq!(got, base);
}

#[test]
fn supply_on_fault_class_is_ignored() {
    let net = P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(3),
        event: environment::FlowEvent::Open,
    };
    let (got, base) = first(
        &net,
        7,
        FaultPolicy::none(),
        Answer::Supply(vec![1, 2, 3, 4]),
    );
    assert_eq!(got, base);
}

#[test]
fn same_class_fault_is_admissible() {
    let net = P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(3),
        event: environment::FlowEvent::Open,
    };
    let reset = Answer::Fault(Fault::NetReset);
    let (got, _base) = first(&net, 7, FaultPolicy::none(), reset.clone());
    assert_eq!(got, reset);
}

#[test]
fn block_torn_within_bounds_is_admissible_oversize_ignored() {
    let io = P::BlockIo {
        op: environment::BlockOp::Write,
        lba: 0,
        len: 512,
    };
    // n <= len wins.
    let small = Answer::Fault(Fault::BlockTorn(256));
    let (got, _b) = first(&io, 7, FaultPolicy::none(), small.clone());
    assert_eq!(got, small);
    // n > len is ignored.
    let big = Answer::Fault(Fault::BlockTorn(1024));
    let (got2, base2) = first(&io, 7, FaultPolicy::none(), big);
    assert_eq!(got2, base2);
}

#[test]
fn nominal_on_fault_class_is_admissible() {
    // A Nominal override on a fault class forces the happy path even under a
    // fault-heavy policy.
    let mut policy = FaultPolicy::none();
    policy
        .set_class(DecisionClass::Process, 1, 1, &[Fault::ProcKill])
        .unwrap();
    let (got, _base) = first(&P::Process { node: NodeId(9) }, 7, policy, Answer::Nominal);
    assert_eq!(got, Answer::Nominal);
}

#[test]
fn host_action_at_a_decision_moment_is_ignored_by_decide() {
    // A host-plane override sharing a Moment with a guest decision is never
    // surfaced as a guest answer — the base answers, exactly as if absent.
    let seed = 7;
    let at = 3u64;
    let mut env = EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([(at, Action::Host(HostFault::InjectInterrupt { vector: 9 }))]),
        standing: vec![],
    }
    .materialize();
    env.set_moment(at);
    let point = P::Process { node: NodeId(1) };
    let got = env.decide(&point);

    let mut base = SeededEnv::new(seed, FaultPolicy::none());
    assert_eq!(got, base.decide(&point));
}
