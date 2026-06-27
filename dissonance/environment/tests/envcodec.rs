// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 7 — `EnvCodec`, the proposal seam. `compose` returns `Ok` only for the
//! one provably bit-identical case — a genesis splice (`at == 0`) of an
//! override-only, same-seed/same-policy tail — and **fails closed** for every
//! other composition the current single-`EnvSpec` model cannot represent
//! (`at != 0`, a standing fault, or a seed/policy mismatch — all deferred to task
//! 93). `seeded` is a pure seeded env; `mutate` is deterministic, host-only, and
//! never relocates a guest override out of context.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_point, arb_policy, arb_spec, config, run_guest_schedule};
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

    /// The one provable case: a **genesis splice** (`at == 0`) of an override-only
    /// tail with the same seed/policy as base. The `[0, 0)` base prefix is empty,
    /// so the composed env equals the tail's overrides under the shared
    /// seed/policy and **replays bit-identically to the tail** over any
    /// Moment-stamped schedule — including seed-serviced (unoverridden) decisions,
    /// since there is no prefix to advance the shared PRNG.
    #[test]
    fn compose_genesis_splice_is_bit_identical_to_tail(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
        seed in any::<u64>(),
        policy in arb_policy(),
        points in prop::collection::vec(arb_point(), 1..16),
    ) {
        let base = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: base_ov, standing: vec![],
        };
        let tail = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: tail_ov.clone(), standing: vec![],
        };
        let composed = EnvCodec::compose(&base, &tail, 0).expect("genesis splice, same seed/policy");

        // The composed overrides are exactly the tail's (empty [0,0) base prefix);
        // base seed/policy carried; no standing faults.
        prop_assert_eq!(composed.overrides(), &tail_ov);
        prop_assert_eq!(composed.seed(), seed);
        prop_assert_eq!(composed.policy(), &policy);
        prop_assert!(standing_of(&composed).is_empty());

        // Bit-identical replay against the tail, seed-serviced fallbacks included.
        let sched: Vec<(Moment, _)> =
            points.iter().enumerate().map(|(i, p)| (i as u64, *p)).collect();
        let a = run_guest_schedule(&mut composed.materialize(), &sched);
        let b = run_guest_schedule(&mut tail.materialize(), &sched);
        prop_assert_eq!(a, b);
    }

    /// `compose` **fails closed** whenever either input carries a standing fault,
    /// at any offset — never silently producing a cross-clock-shifted reproducer.
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

    /// `compose` **fails closed** on every non-genesis splice (`at != 0`),
    /// override-only and same-seed/policy notwithstanding — the round-4 P1: a
    /// `[0, at)` prefix can advance the shared PRNG and desync the tail's fresh
    /// seed stream, so no `at != 0` is provably bit-identical.
    #[test]
    fn compose_rejects_every_nonzero_splice(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
        seed in any::<u64>(),
        policy in arb_policy(),
        at in 1u64..=u64::MAX,
    ) {
        let base = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: base_ov, standing: vec![],
        };
        let tail = EnvSpec::Recorded { seed, policy, overrides: tail_ov, standing: vec![] };
        prop_assert_eq!(
            EnvCodec::compose(&base, &tail, at),
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
fn compose_genesis_splice_keeps_seed_policy_and_tail_overrides() {
    // at == 0: the [0, 0) base prefix is empty, so the result is the tail's
    // overrides under the shared seed/policy (a base override does NOT survive —
    // there is no prefix region).
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
            3,
            Action::Host(environment::HostFault::InjectInterrupt { vector: 1 }),
        )]),
        standing: vec![],
    };
    let composed = EnvCodec::compose(&base, &tail, 0).unwrap();
    assert_eq!(composed.seed(), 0xABCD, "base seed carried");
    assert_eq!(composed.policy(), &policy, "base policy carried");
    assert!(standing_of(&composed).is_empty(), "no standing faults");
    assert_eq!(
        composed.overrides(),
        tail.overrides(),
        "result is exactly the tail's overrides"
    );
    assert!(
        !composed.overrides().contains_key(&5),
        "the empty [0, 0) base prefix contributes nothing"
    );
}

#[test]
fn compose_rejects_non_genesis_splice() {
    // The round-4 P1: a non-genesis splice (at != 0) can desync the tail's fresh
    // seed stream. The composed env has one seeded backing; replaying its [0, at)
    // prefix advances the shared PRNG before the tail starts, but a fresh tail
    // starts at word 0 — so a pre-`at` seed-serviced (e.g. entropy) decision makes
    // every later seed-serviced answer use a later word than the tail's. Not
    // bit-identical, so every at != 0 fails closed (the reviewer's same-seed
    // Seeded/Seeded/10 example included).
    let s = EnvSpec::Seeded {
        seed: 7,
        policy: FaultPolicy::none(),
    };
    for at in [1u64, 5, 10, 1_000, u64::MAX] {
        assert_eq!(
            EnvCodec::compose(&s, &s, at),
            Err(EnvError::UnsupportedComposition),
            "non-genesis splice at {at} is rejected (seeded-fallback desync)"
        );
    }
    // Override-only, same seed/policy, but at != 0 → still rejected.
    let base = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let tail = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    assert_eq!(
        EnvCodec::compose(&base, &tail, 5),
        Err(EnvError::UnsupportedComposition)
    );
}

#[test]
fn compose_fails_closed_on_standing_seed_or_policy_mismatch() {
    // Each rejection below uses at == 0 so the cause is unambiguously the standing
    // fault / seed / policy, not the non-genesis offset.

    // A StandingFault's V-time window is a different clock than the Moment splice
    // offset, so compose cannot faithfully re-key it: reject.
    let base_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::NetSend)]);
    let plain = recorded(BTreeMap::new()); // seed 0, policy none, no standing
    assert_eq!(
        EnvCodec::compose(&base_standing, &plain, 0),
        Err(EnvError::UnsupportedComposition),
        "standing in base is rejected"
    );
    let tail_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::BlockIo)]);
    assert_eq!(
        EnvCodec::compose(&plain, &tail_standing, 0),
        Err(EnvError::UnsupportedComposition),
        "standing in tail is rejected"
    );

    // One EnvSpec cannot carry a piecewise base-then-tail seed; reject mismatches.
    let seed_a = EnvSpec::Seeded {
        seed: 1,
        policy: FaultPolicy::none(),
    };
    let seed_b = EnvSpec::Seeded {
        seed: 2,
        policy: FaultPolicy::none(),
    };
    assert_eq!(
        EnvCodec::compose(&seed_a, &seed_b, 0),
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
        EnvCodec::compose(&seed_a, &other_policy, 0),
        Err(EnvError::UnsupportedComposition),
        "policy mismatch is rejected"
    );
}

#[test]
fn compose_genesis_splice_reproduces_tail_run() {
    // The accepted (at == 0) set replays bit-identical to the tail — including the
    // seed-serviced fallback, since there is no prefix to desync the stream. The
    // schedule deliberately mixes overridden and unoverridden (entropy) decisions.
    let base = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };
    let tail = recorded(BTreeMap::from([
        (1, Action::Guest(Answer::Nominal)),
        (4, Action::Guest(Answer::Fault(environment::Fault::NetDrop))),
    ]));
    let composed = EnvCodec::compose(&base, &tail, 0).unwrap();

    let net = P::NetSend {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(0),
        len: 64,
    };
    let sched: Vec<(Moment, P)> = vec![
        (0, P::Entropy { bytes: 8 }), // seed-serviced
        (1, net),                     // overridden (Nominal)
        (2, P::Entropy { bytes: 4 }), // seed-serviced
        (4, net),                     // overridden (NetDrop)
    ];
    let a = run_guest_schedule(&mut composed.materialize(), &sched);
    let b = run_guest_schedule(&mut tail.materialize(), &sched);
    assert_eq!(a, b, "genesis splice reproduces the tail exactly");
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
