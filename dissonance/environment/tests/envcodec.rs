// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 7 — `EnvCodec`, the proposal seam. `compose` re-keys `Moment`s correctly
//! (one-axis arithmetic — the task-45 acceptance gate, ≥256 cases) and the
//! re-keyed delta reproduces its run; `seeded` is a pure seeded env; `mutate` is
//! deterministic and proposes only legal actions.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_spec, config, run_guest_schedule};
use environment::{
    Action, Answer, ConnId, DecisionPoint as P, EnvCodec, EnvSpec, Environment, FaultPolicy,
    Moment, NodeId, Outcome, StandingFault, VTime,
};
use proptest::prelude::*;

/// The disjointness bound: all generated `Moment`s stay below it, and `compose`
/// is called at exactly this offset, so `base` (`m < BOUND`) and `tail`
/// (`m + BOUND`) never collide — making the re-keying exactly checkable.
const BOUND: Moment = 1 << 20;

/// A `Moment`-keyed override map with every `Moment` strictly below [`BOUND`].
fn arb_bounded_overrides() -> impl Strategy<Value = BTreeMap<Moment, Action>> {
    prop::collection::btree_map(0u64..BOUND, arb_action(), 0..12)
}

fn recorded(overrides: BTreeMap<Moment, Action>) -> EnvSpec {
    EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides,
        standing: vec![],
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// `compose(base, tail, BOUND)` keeps every `base` entry (all `m < BOUND`) at
    /// its `Moment` and places every `tail` entry at `m + BOUND` — one-axis
    /// integer arithmetic, collision-free, genesis-complete.
    #[test]
    fn compose_rekeys_moments(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
    ) {
        let base = recorded(base_ov.clone());
        let tail = recorded(tail_ov.clone());
        let composed = EnvCodec::compose(&base, &tail, BOUND);
        let out = composed.overrides();

        // Disjoint ranges ⇒ exact union, no entry lost or merged.
        prop_assert_eq!(out.len(), base_ov.len() + tail_ov.len());

        // Every base entry survives unmoved (it is in the [0, BOUND) prefix).
        for (m, a) in &base_ov {
            prop_assert_eq!(out.get(m), Some(a));
        }
        // Every tail entry is re-keyed by +BOUND, value unchanged.
        for (m, a) in &tail_ov {
            prop_assert_eq!(out.get(&(m + BOUND)), Some(a));
        }
    }

    /// `mutate` is a pure function of `(env, salt)`: identical inputs give the
    /// identical proposal, and the proposal is always a well-formed, round-trip
    /// `Recorded` spec (a legal vocabulary element).
    #[test]
    fn mutate_is_deterministic_and_legal(spec in arb_spec(), salt in any::<u64>()) {
        let a = EnvCodec::mutate(&spec, salt);
        let b = EnvCodec::mutate(&spec, salt);
        prop_assert_eq!(&a, &b, "same (env, salt) ⇒ same proposal");
        prop_assert!(matches!(a, EnvSpec::Recorded { .. }), "mutate yields Recorded");
        // The proposal is legal: it serializes to a well-formed, byte-stable blob
        // (the input's standing-fault order is canonicalized on encode, so we
        // compare bytes rather than structure).
        let decoded = EnvSpec::decode(&a.encode()).expect("legal blob");
        prop_assert_eq!(decoded.encode(), a.encode(), "byte-stable round-trip");
    }
}

#[test]
fn compose_truncates_base_at_the_splice_point() {
    // A base override at or beyond the splice Moment is dropped — `compose` keeps
    // only the genesis prefix `[0, at)` of the base.
    let base = recorded(BTreeMap::from([
        (5, Action::Guest(Answer::Nominal)),
        (20, Action::Guest(Answer::Supply(vec![1]))), // >= at, dropped
    ]));
    let tail = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let composed = EnvCodec::compose(&base, &tail, 10);
    let out = composed.overrides();

    assert!(out.contains_key(&5), "prefix entry kept");
    assert!(
        !out.contains_key(&20),
        "entry at/after the splice is dropped"
    );
    assert!(out.contains_key(&10), "tail entry re-keyed to at + 0");
    assert_eq!(out.len(), 2);
}

#[test]
fn compose_keeps_base_seed_policy_and_standing() {
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            environment::DecisionClass::NetSend,
            1,
            2,
            &[environment::Fault::NetDrop],
        )
        .unwrap();
    let standing = vec![StandingFault {
        class: environment::DecisionClass::NetSend,
        target: vec![1, 2],
        window: (VTime(0), VTime(9)),
    }];
    let base = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::new(),
        standing: standing.clone(),
    };
    let tail = EnvSpec::Seeded {
        seed: 0x9999,
        policy: FaultPolicy::none(),
    };
    let composed = EnvCodec::compose(&base, &tail, 100);
    assert_eq!(composed.seed(), 0xABCD, "base seed wins");
    assert_eq!(composed.policy(), &policy, "base policy wins");
    let EnvSpec::Recorded { standing: st, .. } = &composed else {
        panic!("compose yields Recorded");
    };
    assert_eq!(st, &standing, "base standing faults kept");
}

#[test]
fn composed_delta_reproduces_its_run() {
    // Task-93 property: a delta's run replays identically after being composed
    // onto a base and re-keyed. Use always-admissible `Nominal` overrides on a
    // fault-class point so each fires regardless of the seeded base — isolating
    // the re-keying as the only variable.
    let point = P::NetSend {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(0),
        len: 64,
    };
    let delta = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (3, Action::Guest(Answer::Fault(environment::Fault::NetDrop))),
        (7, Action::Guest(Answer::Nominal)),
    ]));

    // The delta's own run, at its unshifted Moments.
    let delta_sched: Vec<(Moment, P)> = [0u64, 3, 7].iter().map(|m| (*m, point)).collect();
    let delta_trace = run_guest_schedule(&mut delta.materialize(), &delta_sched);

    // Compose onto an arbitrary base at offset `at`; run at shifted Moments.
    let at: Moment = 1_000;
    let base = EnvSpec::Seeded {
        seed: 0xDEAD_BEEF,
        policy: FaultPolicy::none(),
    };
    let composed = EnvCodec::compose(&base, &delta, at);
    let shifted_sched: Vec<(Moment, P)> = delta_sched.iter().map(|(m, p)| (m + at, *p)).collect();
    let composed_trace = run_guest_schedule(&mut composed.materialize(), &shifted_sched);

    assert_eq!(
        delta_trace, composed_trace,
        "the re-keyed delta reproduces its run"
    );
    // And the overrides really did move.
    assert!(composed.overrides().contains_key(&(3 + at)));
}

#[test]
fn seeded_is_a_pure_seeded_env() {
    let policy = FaultPolicy::none();
    let env = EnvCodec::seeded(0x1234, policy.clone());
    assert_eq!(
        env,
        EnvSpec::Seeded {
            seed: 0x1234,
            policy
        }
    );
    assert!(env.overrides().is_empty());
    assert_eq!(env.host_faults().count(), 0);
}

#[test]
fn mutate_of_empty_inserts_one_host_fault() {
    // An env with no overrides has only the "insert" branch available, so mutate
    // adds exactly one host-plane action (always legal — no admissibility).
    let env = EnvSpec::Seeded {
        seed: 1,
        policy: FaultPolicy::none(),
    };
    let mutated = EnvCodec::mutate(&env, 0xFEED);
    assert_eq!(mutated.overrides().len(), 1, "one action inserted");
    let (_m, action) = mutated.overrides().iter().next().unwrap();
    assert!(
        matches!(action, Action::Host(_)),
        "mutate proposes a host-plane action"
    );
    // It must also be a legal, round-tripping blob.
    assert_eq!(EnvSpec::decode(&mutated.encode()).unwrap(), mutated);
}

#[test]
fn materialized_recorded_default_moment_is_zero() {
    // A freshly materialized env answers for Moment 0 until `set_moment` is
    // called, so an override at Moment 0 fires without any explicit set.
    let env_spec = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let mut env = env_spec.materialize();
    let p = P::Process { node: NodeId(0) };
    assert_eq!(env.moment(), 0);
    assert_eq!(env.decide(&p), Outcome::Resolved(Answer::Nominal));
}
