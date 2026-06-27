// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 1 — replay determinism, the core invariant. Two
//! `SeededEnv::new(seed, policy)` answer an identical `DecisionPoint` sequence
//! identically; a `RecordedEnv` materialized from a spec reproduces its answers
//! exactly; and a **mixed host+guest** `Environment` replays bit-identically
//! through a `record`→`replay` (encode→decode) round-trip — the task-45
//! acceptance gate `replay(record(env)) == env`'s run, with host overrides
//! present.

mod common;

use std::collections::BTreeMap;

use common::{
    arb_host_fault, arb_overrides, arb_point, arb_policy, arb_spec, config, run_guest_schedule,
};
use environment::{
    Action, DecisionPoint, EnvSpec, Environment, HostFault, Moment, Outcome, SeededEnv,
};
use proptest::prelude::*;

fn run<E: Environment>(env: &mut E, seq: &[DecisionPoint]) -> Vec<Outcome> {
    seq.iter().map(|p| env.decide(p)).collect()
}

/// An override map guaranteed to carry at least one host-plane action (the gate
/// requires "host overrides present"), mixed with the arbitrary host+guest map.
fn arb_overrides_with_host() -> impl Strategy<Value = BTreeMap<Moment, Action>> {
    (arb_overrides(), any::<u64>(), arb_host_fault()).prop_map(|(mut m, hm, hf)| {
        m.insert(hm, Action::Host(hf));
        m
    })
}

/// Build a guest schedule that stamps a decision at every override `Moment` (so
/// guest overrides get a chance to fire and host-action Moments fall through to
/// the seeded base), plus a spread of extra Moments.
fn build_schedule(
    overrides: &BTreeMap<Moment, Action>,
    points: &[DecisionPoint],
) -> Vec<(Moment, DecisionPoint)> {
    let mut sched = Vec::new();
    for (i, m) in overrides.keys().enumerate() {
        sched.push((*m, points[i % points.len()]));
    }
    for (i, p) in points.iter().enumerate() {
        // An arbitrary, deterministic spread of Moments; collisions with the
        // override Moments are harmless (both runs see the same schedule).
        let m = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xABCD;
        sched.push((m, *p));
    }
    sched
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
    /// of the same spec reproduce the same answers over the same Moment-stamped
    /// schedule.
    #[test]
    fn recorded_env_reproduces_exactly(
        spec in arb_spec(),
        points in prop::collection::vec(arb_point(), 1..24),
    ) {
        let sched = build_schedule(spec.overrides(), &points);
        let a = run_guest_schedule(&mut spec.materialize(), &sched);
        let b = run_guest_schedule(&mut spec.materialize(), &sched);
        prop_assert_eq!(&a, &b);
        for o in &a {
            prop_assert!(matches!(o, Outcome::Resolved(_)), "a pure backing never suspends");
        }
    }

    /// **The acceptance gate.** A mixed host+guest `Environment` replays
    /// bit-identically across a `record`→`replay` (encode→decode) round-trip:
    /// the serialized reproducer reconstructs the exact same guest answer trace
    /// and host-fault timeline as the in-memory one.
    #[test]
    fn mixed_host_guest_replays_bit_identically(
        seed in any::<u64>(),
        policy in arb_policy(),
        overrides in arb_overrides_with_host(),
        points in prop::collection::vec(arb_point(), 1..24),
    ) {
        let spec = EnvSpec::Recorded { seed, policy, overrides, standing: vec![] };
        let sched = build_schedule(spec.overrides(), &points);

        // record: serialize. replay: decode it back.
        let replayed = EnvSpec::decode(&spec.encode()).expect("our own blob decodes");
        prop_assert_eq!(&spec, &replayed, "encode/decode round-trips");

        // The guest plane: same Moment-stamped trace from both.
        let trace_a = run_guest_schedule(&mut spec.materialize(), &sched);
        let trace_b = run_guest_schedule(&mut replayed.materialize(), &sched);
        prop_assert_eq!(trace_a, trace_b, "guest replay is bit-identical");

        // The host plane: same imperative timeline from both, in Moment order.
        let host_a: Vec<(Moment, HostFault)> = spec.host_faults().collect();
        let host_b: Vec<(Moment, HostFault)> = replayed.host_faults().collect();
        prop_assert_eq!(host_a, host_b, "host timeline is bit-identical");
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
            environment::DecisionClass::NetFlow,
            1,
            2,
            &[
                environment::Fault::NetReset,
                environment::Fault::NetLatency(VTime(7)),
            ],
        )
        .unwrap();

    let seq = [
        P::Entropy { bytes: 8 },
        P::Payload { bytes: 5 },
        P::Scheduler { ready: 4 },
        P::NetFlow {
            src: NodeId(0),
            dst: NodeId(1),
            conn: environment::ConnId(9),
            event: environment::FlowEvent::Open,
        },
        P::Process { node: NodeId(2) },
    ];

    let mut seeded = SeededEnv::new(seed, policy.clone());
    let mut recorded = environment::EnvSpec::Seeded { seed, policy }.materialize();
    for p in &seq {
        assert_eq!(seeded.decide(p), recorded.decide(p));
    }
}

#[test]
fn host_overrides_never_leak_into_guest_answers() {
    // A host-plane action at a Moment where a guest decision surfaces is NOT
    // applied as a guest answer — it is filtered out of `materialize`, so the
    // seeded base answers (exactly as if no override were there). The host fault
    // remains available to the frontier via `host_faults`.
    use environment::{DecisionPoint as P, FaultPolicy};
    let seed = 0xFEED_FACE;
    let host_moment = 7u64;
    let spec = EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([(
            host_moment,
            Action::Host(HostFault::CorruptMemory {
                gpa: 0x1000,
                mask: environment::BitMask(0xFF),
            }),
        )]),
        standing: vec![],
    };

    let point = P::BlockIo {
        op: environment::BlockOp::Read,
        lba: 0,
        len: 512,
    };

    let mut env = spec.materialize();
    env.set_moment(host_moment);
    let got = env.decide(&point);

    let mut base = SeededEnv::new(seed, FaultPolicy::none());
    let want = base.decide(&point);

    assert_eq!(
        got, want,
        "host action at this Moment does not answer the decision"
    );
    assert_eq!(
        spec.host_faults().collect::<Vec<_>>(),
        vec![(
            host_moment,
            HostFault::CorruptMemory {
                gpa: 0x1000,
                mask: environment::BitMask(0xFF)
            }
        )],
        "the host fault is still on the frontier's timeline"
    );
}
