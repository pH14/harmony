// SPDX-License-Identifier: AGPL-3.0-or-later
//! Task 73 — the buggify decision class on the **fault** stream.
//!
//! Two properties the task-73 spec (gate 3) pins:
//!
//! 1. **Stream separation.** A [`DecisionClass::Buggify`] draw comes from the
//!    domain-separated *fault* PRNG, never the *supply* PRNG. So interleaving
//!    buggify decisions (or turning their probability up) leaves a run's
//!    entropy/payload/scheduler **supply** stream byte-identical — enabling
//!    buggify can never perturb the workload's own randomness.
//! 2. **Per-point biasing reproduces a golden draw sequence.** Given a seed and a
//!    [`FaultPolicy`] with per-point biases, the fire/nominal sequence for a
//!    fixed point order is deterministic and pinned.

mod common;

use environment::{
    Answer, DecisionClass, DecisionPoint, EnvError, Environment, Fault, FaultPolicy, Outcome,
    SeededEnv,
};

/// Collect the resolved [`Answer`]s a [`SeededEnv`] gives for a point sequence.
fn answers(seed: u64, policy: FaultPolicy, points: &[DecisionPoint]) -> Vec<Answer> {
    let mut env = SeededEnv::new(seed, policy);
    points
        .iter()
        .map(|p| match env.decide(p) {
            Outcome::Resolved(a) => a,
            Outcome::NeedsHost => panic!("SeededEnv never needs the host"),
        })
        .collect()
}

/// The entropy (supply) answers only, dropping the buggify answers.
fn supply_only(seed: u64, policy: FaultPolicy, points: &[DecisionPoint]) -> Vec<Answer> {
    answers(seed, policy, points)
        .into_iter()
        .zip(points)
        .filter(|(_, p)| {
            matches!(
                p,
                DecisionPoint::Entropy { .. } | DecisionPoint::Payload { .. }
            )
        })
        .map(|(a, _)| a)
        .collect()
}

/// A policy whose default buggify probability is `num/den`.
fn buggify_policy(num: u32, den: u32) -> FaultPolicy {
    let mut p = FaultPolicy::none();
    p.set_buggify_default(num, den).expect("den >= 1");
    p
}

/// GATE 3a — stream separation. The supply stream is byte-identical whether or
/// not buggify decisions are interleaved, and whatever the buggify probability
/// is: buggify draws from the fault stream, entropy from the supply stream.
#[test]
fn buggify_never_disturbs_the_supply_stream() {
    let seed = 0xABCD_1234_5678_9F01;

    // A schedule with NO buggify decisions — the baseline supply sequence.
    let plain: Vec<DecisionPoint> = (0..8)
        .map(|_| DecisionPoint::Entropy { bytes: 16 })
        .collect();
    let baseline = supply_only(seed, FaultPolicy::none(), &plain);

    // The SAME entropy decisions, now with a buggify decision wedged before each
    // one, and buggify fully enabled (fires every time, 1/1). The entropy answers
    // must be byte-for-byte the baseline: only the fault stream moved.
    let mut mixed: Vec<DecisionPoint> = Vec::new();
    for i in 0..8u32 {
        mixed.push(DecisionPoint::Buggify { point: i });
        mixed.push(DecisionPoint::Entropy { bytes: 16 });
    }
    let with_buggify_hot = supply_only(seed, buggify_policy(1, 1), &mixed);
    let with_buggify_cold = supply_only(seed, buggify_policy(0, 1), &mixed);

    assert_eq!(
        baseline, with_buggify_hot,
        "enabling+firing buggify must not shift the supply stream"
    );
    assert_eq!(
        baseline, with_buggify_cold,
        "an interleaved never-firing buggify must not shift the supply stream either"
    );
    assert_eq!(
        with_buggify_hot, with_buggify_cold,
        "buggify probability changes only the fault stream, never supply"
    );
}

/// GATE 3b — per-point biasing golden. Distinct per-point biases produce a
/// deterministic, pinned fire/nominal sequence for a fixed point order under a
/// fixed seed. Point 0 always fires (1/1), point 1 never fires (0/1), point 2
/// uses the default (1/2, seed-dependent).
#[test]
fn per_point_biasing_reproduces_a_golden_sequence() {
    let seed = 42;
    let mut policy = FaultPolicy::none();
    policy.set_buggify_default(1, 2).unwrap();
    policy.set_buggify_point(0, 1, 1).unwrap(); // always fire
    policy.set_buggify_point(1, 0, 1).unwrap(); // never fire

    // Ask each point a few times in a fixed order.
    let points: Vec<DecisionPoint> = [0u32, 1, 2, 0, 1, 2, 2, 2]
        .into_iter()
        .map(|point| DecisionPoint::Buggify { point })
        .collect();

    let fired: Vec<bool> = answers(seed, policy.clone(), &points)
        .iter()
        .map(|a| matches!(a, Answer::Fault(Fault::BuggifyFire)))
        .collect();

    // Points 0 (always) and 1 (never) are pinned by construction; the point-2
    // draws are pinned by the fault PRNG given seed 42.
    assert_eq!(
        fired,
        vec![true, false, false, true, false, true, true, false],
        "per-point buggify draw sequence drifted"
    );

    // Same (seed, policy, points) ⇒ same sequence (determinism).
    let again: Vec<bool> = answers(seed, policy, &points)
        .iter()
        .map(|a| matches!(a, Answer::Fault(Fault::BuggifyFire)))
        .collect();
    assert_eq!(fired, again, "buggify is deterministic per (seed, policy)");
}

/// `set_class(Buggify, …)` is rejected (buggify has no per-class slot — it is
/// keyed per point), and a buggify-only policy set the sanctioned way round-trips
/// through its **own** bytes. Regression for the review's finding that routing
/// buggify through `set_class` lands a `BuggifyFire` in the net slot and makes
/// `from_bytes(to_bytes())` reject the policy's own bytes.
#[test]
fn set_class_rejects_buggify_and_policy_round_trips() {
    let mut p = FaultPolicy::none();
    assert_eq!(
        p.set_class(DecisionClass::Buggify, 1, 2, &[]),
        Err(EnvError::Malformed),
        "buggify has no class slot — use set_buggify_*"
    );
    assert_eq!(
        p.set_class(DecisionClass::Buggify, 1, 2, &[Fault::BuggifyFire]),
        Err(EnvError::Malformed)
    );

    // The sanctioned per-point path, then a self round-trip.
    p.set_buggify_default(1, 3).unwrap();
    p.set_buggify_point(50, 1, 1).unwrap();
    p.set_buggify_point(7, 2, 5).unwrap();
    let bytes = p.to_bytes();
    assert_eq!(
        FaultPolicy::from_bytes(&bytes).unwrap(),
        p,
        "a buggify policy must read its own bytes"
    );
}

/// The dynamic stream state round-trips: an env resumed at a captured position
/// produces the **identical** continuation across BOTH the supply (entropy) and
/// fault (buggify) streams — the SDK-channel snapshot fix (a fork from a mid-run
/// snapshot must continue the seeded streams from where they were).
#[test]
fn stream_state_resumes_both_streams_exactly() {
    let seed = 0x1234_5678_9ABC_DEF0;
    let mut policy = FaultPolicy::none();
    policy.set_buggify_point(1, 1, 2).unwrap();

    let points = [
        DecisionPoint::Entropy { bytes: 8 },
        DecisionPoint::Buggify { point: 1 },
        DecisionPoint::Entropy { bytes: 4 },
        DecisionPoint::Buggify { point: 1 },
    ];
    let seq = |env: &mut SeededEnv| -> Vec<Answer> {
        points
            .iter()
            .map(|p| match env.decide(p) {
                Outcome::Resolved(a) => a,
                Outcome::NeedsHost => unreachable!(),
            })
            .collect()
    };

    // Advance to a mid-run position, capture it, and record the continuation.
    let mut a = SeededEnv::new(seed, policy.clone());
    let _prefix = seq(&mut a);
    let mid = a.stream_state();
    let continuation = seq(&mut a);

    // A fresh env resumed at `mid` produces the identical continuation — and a
    // wrong-position env (fresh, not resumed) does not.
    let mut b = SeededEnv::new(seed, policy.clone());
    b.restore_stream_state(&mid);
    assert_eq!(seq(&mut b), continuation, "resumed streams match exactly");

    let mut fresh = SeededEnv::new(seed, policy);
    assert_ne!(
        seq(&mut fresh),
        continuation,
        "a fresh (position-0) env is NOT the mid-run continuation — the bug the fix closes"
    );
}

/// A buggify point only admits `Nominal` or `Fault(BuggifyFire)` — never a supply
/// answer, and never a foreign-class fault.
#[test]
fn buggify_point_admits_only_nominal_or_buggify_fire() {
    let p = DecisionPoint::Buggify { point: 7 };
    assert!(p.admits(&Answer::Nominal));
    assert!(p.admits(&Answer::Fault(Fault::BuggifyFire)));
    assert!(!p.admits(&Answer::Fault(Fault::BlockEio)));
    assert!(!p.admits(&Answer::Supply(vec![1, 2, 3, 4])));
}
