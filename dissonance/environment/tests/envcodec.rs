// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 7 — `EnvCodec`, the proposal seam. `compose` re-keys the `Moment`-keyed
//! overrides of an *override-only, same-seed/same-policy* tail correctly (one-axis
//! arithmetic — the task-45 acceptance gate, ≥256 cases) and **fails closed** for
//! the compositions the current model cannot faithfully represent (standing
//! faults, or a seed/policy mismatch — deferred to task 93); the re-keyed delta
//! reproduces its run; `seeded` is a pure seeded env; `mutate` is deterministic,
//! host-only, and never relocates a guest override out of context.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_policy, arb_spec, config, run_guest_schedule};
use environment::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvCodec, EnvError, EnvSpec,
    Environment, FaultPolicy, Moment, NodeId, Outcome, StandingFault, VTime,
};
use proptest::prelude::*;

/// The disjointness bound: all generated `Moment`s stay below it, and `compose`
/// is called at exactly this offset, so `base` (`m < BOUND`) and `tail`
/// (`m + BOUND`) never collide and `+ BOUND` never overflows — making the
/// re-keying exactly checkable.
const BOUND: Moment = 1 << 20;

/// A `Moment`-keyed override map with every `Moment` strictly below [`BOUND`].
fn arb_bounded_overrides() -> impl Strategy<Value = BTreeMap<Moment, Action>> {
    prop::collection::btree_map(0u64..BOUND, arb_action(), 0..12)
}

fn recorded_with(overrides: BTreeMap<Moment, Action>, standing: Vec<StandingFault>) -> EnvSpec {
    EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides,
        standing,
    }
}

fn recorded(overrides: BTreeMap<Moment, Action>) -> EnvSpec {
    recorded_with(overrides, vec![])
}

fn standing_of(spec: &EnvSpec) -> &[StandingFault] {
    match spec {
        EnvSpec::Recorded { standing, .. } => standing,
        EnvSpec::Seeded { .. } => &[],
    }
}

/// A single standing fault literal (for the fail-closed rejection tests).
fn sf(class: DecisionClass) -> StandingFault {
    StandingFault {
        class,
        target: vec![1, 2],
        window: (VTime(0), VTime(9)),
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// The supported case: an override-only tail with the same seed and policy as
    /// base. `compose(base, tail, BOUND)` keeps every `base` entry (all
    /// `m < BOUND`) at its `Moment`, places every `tail` entry at `m + BOUND`, and
    /// is genesis-complete — one-axis arithmetic, collision-free, base seed/policy
    /// carried, no standing faults.
    #[test]
    fn compose_rekeys_overrides_genesis_complete(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
        seed in any::<u64>(),
        policy in arb_policy(),
    ) {
        // Same seed and policy on both sides (the only faithfully composable case).
        let base = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: base_ov.clone(), standing: vec![],
        };
        let tail = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: tail_ov.clone(), standing: vec![],
        };
        let composed = EnvCodec::compose(&base, &tail, BOUND)
            .expect("override-only, same seed/policy, < BOUND");
        let out = composed.overrides();

        // Disjoint ranges ⇒ exact union, no entry lost or merged.
        prop_assert_eq!(out.len(), base_ov.len() + tail_ov.len());
        for (m, a) in &base_ov {
            prop_assert_eq!(out.get(m), Some(a));
        }
        for (m, a) in &tail_ov {
            prop_assert_eq!(out.get(&(m + BOUND)), Some(a));
        }
        // Genesis-complete: base seed/policy carried, no standing faults.
        prop_assert_eq!(composed.seed(), seed);
        prop_assert_eq!(composed.policy(), &policy);
        prop_assert!(standing_of(&composed).is_empty());
    }

    /// `compose` **fails closed** whenever either input carries a standing fault,
    /// regardless of overrides — never silently producing a cross-clock-shifted
    /// (wrong) reproducer.
    #[test]
    fn compose_rejects_any_standing_fault(
        ov in arb_bounded_overrides(),
        at in 0u64..BOUND,
    ) {
        let plain = recorded(ov.clone());
        let with_standing = recorded_with(ov, vec![sf(DecisionClass::NetSend)]);
        // standing in base, in tail, or both → always rejected.
        prop_assert_eq!(
            EnvCodec::compose(&with_standing, &plain, at),
            Err(EnvError::UnsupportedComposition)
        );
        prop_assert_eq!(
            EnvCodec::compose(&plain, &with_standing, at),
            Err(EnvError::UnsupportedComposition)
        );
        prop_assert_eq!(
            EnvCodec::compose(&with_standing, &with_standing, at),
            Err(EnvError::UnsupportedComposition)
        );
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

    /// `mutate` is **host-only**: every guest override in the input survives
    /// verbatim (same `Moment`, same `Answer`) in the output — it is never
    /// removed, relocated, or overwritten, so no out-of-context guest answer can
    /// be fabricated.
    #[test]
    fn mutate_preserves_every_guest_override(spec in arb_spec(), salt in any::<u64>()) {
        let mutated = EnvCodec::mutate(&spec, salt);
        let out = mutated.overrides();
        for (m, a) in spec.overrides() {
            if let Action::Guest(_) = a {
                prop_assert_eq!(
                    out.get(m),
                    Some(a),
                    "a guest override was moved/removed/overwritten by mutate"
                );
            }
        }
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
    let composed = EnvCodec::compose(&base, &tail, 10).unwrap();
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
fn compose_keeps_base_seed_and_policy_override_only() {
    // Override-only, same seed/policy: the result carries base's seed and policy
    // and no standing faults.
    let mut policy = FaultPolicy::none();
    policy
        .set_class(DecisionClass::NetSend, 1, 2, &[environment::Fault::NetDrop])
        .unwrap();
    let base = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([(5, Action::Guest(Answer::Nominal))]),
        standing: vec![],
    };
    let tail = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([(
            0,
            Action::Host(environment::HostFault::InjectInterrupt { vector: 1 }),
        )]),
        standing: vec![],
    };
    let composed = EnvCodec::compose(&base, &tail, 100).unwrap();
    assert_eq!(composed.seed(), 0xABCD, "base seed carried");
    assert_eq!(composed.policy(), &policy, "base policy carried");
    assert!(standing_of(&composed).is_empty(), "no standing faults");
    assert!(composed.overrides().contains_key(&5), "base override kept");
    assert!(
        composed.overrides().contains_key(&100),
        "tail re-keyed to at+0"
    );
}

#[test]
fn compose_fails_closed_on_standing_seed_or_policy_mismatch() {
    // 3b — a StandingFault's V-time window is a different clock than the Moment
    // splice offset, so compose cannot faithfully re-key it: reject.
    let base_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::NetSend)]);
    let plain = recorded(BTreeMap::new()); // seed 0, policy none, no standing
    assert_eq!(
        EnvCodec::compose(&base_standing, &plain, 10),
        Err(EnvError::UnsupportedComposition),
        "standing in base is rejected"
    );
    let tail_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::BlockIo)]);
    assert_eq!(
        EnvCodec::compose(&plain, &tail_standing, 10),
        Err(EnvError::UnsupportedComposition),
        "standing in tail is rejected"
    );

    // 3a — one EnvSpec cannot carry a piecewise base-then-tail seed; using base's
    // seed for the fallback is only correct when seeds match. Reject mismatches.
    let seed_a = EnvSpec::Seeded {
        seed: 1,
        policy: FaultPolicy::none(),
    };
    let seed_b = EnvSpec::Seeded {
        seed: 2,
        policy: FaultPolicy::none(),
    };
    assert_eq!(
        EnvCodec::compose(&seed_a, &seed_b, 10),
        Err(EnvError::UnsupportedComposition),
        "seed mismatch is rejected"
    );

    // Likewise a policy mismatch (a seed alone cannot reproduce a policy-dependent
    // answer sequence).
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            DecisionClass::Process,
            1,
            2,
            &[environment::Fault::ProcKill],
        )
        .unwrap();
    let other_policy = EnvSpec::Seeded { seed: 1, policy };
    assert_eq!(
        EnvCodec::compose(&seed_a, &other_policy, 10),
        Err(EnvError::UnsupportedComposition),
        "policy mismatch is rejected"
    );
}

#[test]
fn compose_offset_overflow_is_rejected() {
    // P2 boundary: an offset that maps a tail Moment past u64::MAX must reject,
    // never saturate two overrides onto one colliding key.
    let tail = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (1, Action::Guest(Answer::Supply(vec![9]))),
    ]));
    let base = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };

    // at = u64::MAX: Moment 0 → u64::MAX (fits), Moment 1 → overflow.
    assert_eq!(
        EnvCodec::compose(&base, &tail, u64::MAX),
        Err(EnvError::Overflow),
        "a tail Moment shifted past u64::MAX is rejected, not saturated"
    );

    // Exactly representable: a single tail Moment 0 at u64::MAX lands at u64::MAX.
    let single = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let ok = EnvCodec::compose(&base, &single, u64::MAX).unwrap();
    assert!(ok.overrides().contains_key(&u64::MAX));
    assert_eq!(ok.overrides().len(), 1);
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

    // Compose onto a base at offset `at`. Base must share the delta's seed/policy
    // (the only faithfully composable case); `recorded` uses seed 0 / policy none,
    // so the base does too. The overrides are all admissible and fire regardless
    // of the seed, isolating the re-keying as the only variable.
    let at: Moment = 1_000;
    let base = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };
    let composed = EnvCodec::compose(&base, &delta, at).unwrap();
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
fn mutate_never_disturbs_a_guest_only_spec() {
    // A spec with only guest overrides: across many salts, every guest entry is
    // preserved and the only change mutate can make is to *add* a host action.
    let guest = BTreeMap::from([
        (10, Action::Guest(Answer::Nominal)),
        (
            20,
            Action::Guest(Answer::Fault(environment::Fault::NetDrop)),
        ),
        (30, Action::Guest(Answer::Supply(vec![1, 2, 3, 4]))),
    ]);
    let spec = recorded(guest.clone());
    for salt in 0u64..64 {
        let mutated = EnvCodec::mutate(&spec, salt);
        let out = mutated.overrides();
        for (m, a) in &guest {
            assert_eq!(
                out.get(m),
                Some(a),
                "guest override preserved at salt {salt}"
            );
        }
        // The only legal op on a host-free map is insert (one host action added).
        assert_eq!(out.len(), guest.len() + 1, "exactly one host action added");
    }
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

#[test]
fn set_moment_is_reflected_by_moment_accessor() {
    // `moment()` returns the exact value last set — not `Moment::default()`. A
    // non-zero value distinguishes the real getter from a `-> Default` mutant.
    let mut env = recorded(BTreeMap::new()).materialize();
    env.set_moment(0xDEAD_BEEF_0000_1234);
    assert_eq!(env.moment(), 0xDEAD_BEEF_0000_1234);
    env.set_moment(7);
    assert_eq!(env.moment(), 7, "tracks the most recent set_moment");
}
