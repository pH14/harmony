// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — replay determinism, the core invariant. Two
//! `SeededEnv::new(seed, policy)` answer an identical `DecisionPoint` sequence
//! identically, and a `RecordedEnv` materialized from an `EnvSpec::Recorded`
//! reproduces its answers exactly across repeated materializations.

mod common;

use common::{arb_point, arb_policy, arb_spec, config};
use environment::{Environment, Outcome, SeededEnv};
use proptest::prelude::*;

fn run<E: Environment>(env: &mut E, seq: &[environment::DecisionPoint]) -> Vec<Outcome> {
    seq.iter().map(|p| env.decide(p)).collect()
}

proptest! {
    #![proptest_config(config(256))]

    /// Same seed + same policy + same point sequence ⇒ identical answer sequence.
    /// A `HashMap` reaching any answer would make this flaky.
    #[test]
    fn two_seeded_envs_agree(
        seed in any::<u64>(),
        policy in arb_policy(),
        seq in prop::collection::vec(arb_point(), 0..40),
    ) {
        let a = run(&mut SeededEnv::new(seed, policy.clone()), &seq);
        let b = run(&mut SeededEnv::new(seed, policy), &seq);
        prop_assert_eq!(&a, &b);
        // A pure backing never suspends.
        for o in &a {
            prop_assert!(matches!(o, Outcome::Resolved(_)));
        }
    }

    /// A `RecordedEnv` is a pure function of its `EnvSpec`: two materializations
    /// of the same spec reproduce the same answers over the same points.
    #[test]
    fn recorded_env_reproduces_exactly(
        spec in arb_spec(),
        seq in prop::collection::vec(arb_point(), 0..40),
    ) {
        let a = run(&mut spec.materialize(), &seq);
        let b = run(&mut spec.materialize(), &seq);
        prop_assert_eq!(&a, &b);
        for o in &a {
            prop_assert!(matches!(o, Outcome::Resolved(_)), "a pure backing never suspends");
        }
    }
}

#[test]
fn seeded_and_seeded_recorded_baseline_agree() {
    // With no overrides, an `EnvSpec::Seeded` materializes to a `RecordedEnv`
    // whose answers match a bare `SeededEnv` decision-for-decision (the override
    // map is empty, so the base answers everything).
    use environment::{DecisionPoint as P, FaultPolicy, NodeId, VTime};
    let seed = 0x1234_5678_9abc_def0;
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            environment::DecisionClass::NetSend,
            1,
            2,
            &[
                environment::Fault::NetDrop,
                environment::Fault::NetDelay(VTime(7)),
            ],
        )
        .unwrap();

    let seq = [
        P::Entropy { bytes: 8 },
        P::Payload { bytes: 5 },
        P::Scheduler { ready: 4 },
        P::NetSend {
            src: NodeId(0),
            dst: NodeId(1),
            conn: environment::ConnId(9),
            len: 64,
        },
        P::Process { node: NodeId(2) },
    ];

    let mut seeded = SeededEnv::new(seed, policy.clone());
    let mut recorded = environment::EnvSpec::Seeded { seed, policy }.materialize();
    for p in &seq {
        assert_eq!(seeded.decide(p), recorded.decide(p));
    }
}
