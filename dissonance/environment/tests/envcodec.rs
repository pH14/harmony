// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 7 — `EnvCodec`, the proposal seam. `compose` re-keys `Moment`s correctly
//! (one-axis arithmetic — the task-45 acceptance gate, ≥256 cases), carries the
//! tail's standing faults into genesis, and rejects offset overflow; the re-keyed
//! delta reproduces its run; `seeded` is a pure seeded env; `mutate` is
//! deterministic, host-only, and never relocates a guest override out of context.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_spec, config, run_guest_schedule};
use environment::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvCodec, EnvError, EnvSpec,
    Environment, FaultPolicy, Moment, NodeId, Outcome, StandingFault, VTime,
};
use proptest::prelude::*;

/// The disjointness bound: all generated `Moment`s and standing-window bounds
/// stay below it, and `compose` is called at exactly this offset, so `base`
/// (`m < BOUND`) and `tail` (`m + BOUND`) never collide and `+ BOUND` never
/// overflows — making the re-keying exactly checkable.
const BOUND: Moment = 1 << 20;

/// A `Moment`-keyed override map with every `Moment` strictly below [`BOUND`].
fn arb_bounded_overrides() -> impl Strategy<Value = BTreeMap<Moment, Action>> {
    prop::collection::btree_map(0u64..BOUND, arb_action(), 0..12)
}

/// A standing fault whose V-time window bounds are strictly below [`BOUND`], so
/// shifting by `+ BOUND` cannot overflow.
fn arb_bounded_standing() -> impl Strategy<Value = StandingFault> {
    arb_standing_upto(BOUND)
}

/// A standing fault whose V-time window bounds are in `0..max`.
fn arb_standing_upto(max: u64) -> impl Strategy<Value = StandingFault> {
    (
        prop_oneof![
            Just(DecisionClass::NetSend),
            Just(DecisionClass::BlockIo),
            Just(DecisionClass::Process),
        ],
        prop::collection::vec(any::<u8>(), 0..8),
        0u64..max,
        0u64..max,
    )
        .prop_map(|(class, target, a, b)| StandingFault {
            class,
            target,
            window: (VTime(a), VTime(b)),
        })
}

/// The splice point for the prefix-filter property: base windows straddle it
/// (`0..2*SPLIT`), tail windows stay in `0..SPLIT` so `+SPLIT` cannot overflow.
const SPLIT: Moment = 1_000;

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

/// Shift a standing fault's window by `+at` (for asserting tail survival).
fn shifted(s: &StandingFault, at: Moment) -> StandingFault {
    StandingFault {
        class: s.class,
        target: s.target.clone(),
        window: (VTime(s.window.0.0 + at), VTime(s.window.1.0 + at)),
    }
}

fn standing_of(spec: &EnvSpec) -> &[StandingFault] {
    match spec {
        EnvSpec::Recorded { standing, .. } => standing,
        EnvSpec::Seeded { .. } => &[],
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// `compose(base, tail, BOUND)` keeps every `base` entry (all `m < BOUND`) at
    /// its `Moment`, places every `tail` entry at `m + BOUND`, keeps every `base`
    /// standing fault, and carries every `tail` standing fault shifted by `+BOUND`
    /// — one-axis arithmetic, collision-free, genesis-complete (nothing dropped).
    #[test]
    fn compose_rekeys_moments_and_carries_standing(
        base_ov in arb_bounded_overrides(),
        base_st in prop::collection::vec(arb_bounded_standing(), 0..4),
        tail_ov in arb_bounded_overrides(),
        tail_st in prop::collection::vec(arb_bounded_standing(), 0..4),
    ) {
        let base = recorded_with(base_ov.clone(), base_st.clone());
        let tail = recorded_with(tail_ov.clone(), tail_st.clone());
        let composed = EnvCodec::compose(&base, &tail, BOUND).expect("all < BOUND, no overflow");
        let out = composed.overrides();

        // Disjoint ranges ⇒ exact union, no entry lost or merged.
        prop_assert_eq!(out.len(), base_ov.len() + tail_ov.len());
        for (m, a) in &base_ov {
            prop_assert_eq!(out.get(m), Some(a));
        }
        for (m, a) in &tail_ov {
            prop_assert_eq!(out.get(&(m + BOUND)), Some(a));
        }

        // Standing faults: every base one survives, every tail one survives
        // shifted by +BOUND (none silently dropped — the genesis-completeness fix).
        let result_standing = standing_of(&composed);
        for s in &base_st {
            prop_assert!(result_standing.contains(s), "base standing fault dropped");
        }
        for s in &tail_st {
            prop_assert!(
                result_standing.contains(&shifted(s, BOUND)),
                "tail standing fault dropped from genesis"
            );
        }
    }

    /// With base AND tail standing faults straddling the splice point, the
    /// composed genesis applies EXACTLY `{base faults active in [0, SPLIT),
    /// truncated to SPLIT} ∪ {tail faults shifted by +SPLIT}` — no base fault
    /// from the discarded `[SPLIT, ∞)` region leaks in, and nothing is dropped.
    #[test]
    fn compose_standing_is_exactly_kept_prefix_union_shifted_tail(
        base_st in prop::collection::vec(arb_standing_upto(2 * SPLIT), 0..6),
        tail_st in prop::collection::vec(arb_standing_upto(SPLIT), 0..6),
    ) {
        let base = recorded_with(BTreeMap::new(), base_st.clone());
        let tail = recorded_with(BTreeMap::new(), tail_st.clone());
        let composed = EnvCodec::compose(&base, &tail, SPLIT).expect("tail < SPLIT, no overflow");

        // The exact expected set, built independently of compose, in compose's
        // order (filtered/truncated base, then shifted tail).
        let expected: Vec<StandingFault> = base_st
            .iter()
            .filter(|s| s.window.0.0 < SPLIT) // drop windows starting in [SPLIT, ∞)
            .map(|s| StandingFault {
                class: s.class,
                target: s.target.clone(),
                window: (s.window.0, VTime(s.window.1.0.min(SPLIT))), // truncate to SPLIT
            })
            .chain(tail_st.iter().map(|s| shifted(s, SPLIT)))
            .collect();

        prop_assert_eq!(standing_of(&composed), expected.as_slice());
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
fn compose_keeps_base_seed_policy_and_merges_standing() {
    let mut policy = FaultPolicy::none();
    policy
        .set_class(DecisionClass::NetSend, 1, 2, &[environment::Fault::NetDrop])
        .unwrap();
    let base_standing = vec![StandingFault {
        class: DecisionClass::NetSend,
        target: vec![1, 2],
        window: (VTime(0), VTime(9)),
    }];
    let base = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::new(),
        standing: base_standing.clone(),
    };
    // Tail carries its OWN standing fault (the P1 case).
    let tail_standing = StandingFault {
        class: DecisionClass::BlockIo,
        target: vec![7],
        window: (VTime(3), VTime(8)),
    };
    let tail = EnvSpec::Recorded {
        seed: 0x9999,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::new(),
        standing: vec![tail_standing.clone()],
    };
    let composed = EnvCodec::compose(&base, &tail, 100).unwrap();
    assert_eq!(composed.seed(), 0xABCD, "base seed wins");
    assert_eq!(composed.policy(), &policy, "base policy wins");
    let st = standing_of(&composed);
    assert!(st.contains(&base_standing[0]), "base standing kept");
    assert!(
        st.contains(&shifted(&tail_standing, 100)),
        "tail standing carried, shifted by +at"
    );
    assert_eq!(st.len(), 2, "no standing fault dropped or duplicated");
}

#[test]
fn compose_drops_base_standing_in_the_discarded_region() {
    // Concrete cases at the splice boundary at=100:
    //   [10, 50)  — wholly before  → kept whole
    //   [80, 150) — straddles      → truncated to [80, 100)
    //   [100,120) — starts AT at   → dropped (window prefix is half-open [0,100))
    //   [200,300) — wholly after   → dropped (lives in the discarded region)
    let at: Moment = 100;
    let sf = |c, lo, hi| StandingFault {
        class: c,
        target: vec![1],
        window: (VTime(lo), VTime(hi)),
    };
    let base = recorded_with(
        BTreeMap::new(),
        vec![
            sf(DecisionClass::NetSend, 10, 50),
            sf(DecisionClass::BlockIo, 80, 150),
            sf(DecisionClass::Process, 100, 120),
            sf(DecisionClass::NetSend, 200, 300),
        ],
    );
    // One tail fault, shifted to [at+5, at+15).
    let tail = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::BlockIo, 5, 15)]);

    let composed = EnvCodec::compose(&base, &tail, at).unwrap();
    let got = standing_of(&composed);

    let expected = vec![
        sf(DecisionClass::NetSend, 10, 50),   // kept whole
        sf(DecisionClass::BlockIo, 80, 100),  // truncated from [80,150)
        sf(DecisionClass::BlockIo, 105, 115), // tail [5,15) shifted by +100
    ];
    assert_eq!(
        got,
        expected.as_slice(),
        "no leak from [at, ∞), no drop, exact truncation"
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
fn compose_standing_overflow_is_rejected() {
    // A tail standing-fault window bound that overflows when shifted must also
    // reject (the V-time axis re-keys consistently with the Moment axis).
    let base = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };
    let tail = recorded_with(
        BTreeMap::new(),
        vec![StandingFault {
            class: DecisionClass::NetSend,
            target: vec![],
            window: (VTime(1), VTime(2)),
        }],
    );
    assert_eq!(
        EnvCodec::compose(&base, &tail, u64::MAX),
        Err(EnvError::Overflow),
        "a tail standing window shifted past u64::MAX is rejected"
    );
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
