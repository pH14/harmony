// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 7 — `EnvCodec`, the proposal seam. `compose` performs one-axis `Moment`
//! override re-keying (the task-45 acceptance gate) for an override-only,
//! same-seed/same-policy composition at any offset, and **fails closed** for the
//! cases outside that one-axis scope (a standing fault, a pure `Seeded` input, or
//! a seed/policy mismatch — all deferred to task 93). `seeded` is a pure seeded
//! env; `mutate` is deterministic, host-only, and never relocates a guest override
//! out of context.

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_policy, arb_spec, config, run_guest_schedule};
use environment::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvCodec, EnvError, EnvSpec,
    Environment, FaultPolicy, Moment, NodeId, Outcome, StandingFault, Span,
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
        reseeds: Default::default(),
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
        window: (0, 9),
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// **The spec gate (line 67): one-axis `Moment` override re-keying, at any
    /// offset.** For two override-only (Recorded) envs with the same seed/policy,
    /// `compose(base, tail, at)` keeps `base`'s prefix `[0, at)` at its `Moment`s
    /// and re-keys every `tail` entry to `m + at` — collision-free, genesis prefix
    /// + shifted tail, base seed/policy carried, no standing faults. `at` is
    /// bounded so `m + at` never overflows. The `base` *suffix* `[at, ∞)` is
    /// discarded and governed entirely by the tail's re-keyed timeline: at any
    /// `m >= at`, `out[m]` is the tail's entry at `m - at` (or absent) — never the
    /// dropped base entry, even when a tail Moment aligns exactly onto it.
    #[test]
    fn compose_rekeys_overrides_at_any_offset(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
        seed in any::<u64>(),
        policy in arb_policy(),
        at in 0u64..BOUND,
    ) {
        let base = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: base_ov.clone(), standing: vec![], reseeds: Default::default(),
        };
        let tail = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: tail_ov.clone(), standing: vec![], reseeds: Default::default(),
        };
        let composed = EnvCodec::compose(&base, &tail, at).expect("override-only, same seed/policy");
        let out = composed.overrides();

        // base keeps only m < at; tail shifts to m + at (>= at). Disjoint ranges.
        let kept_base = base_ov.iter().filter(|(m, _)| **m < at).count();
        prop_assert_eq!(out.len(), kept_base + tail_ov.len());
        for (m, a) in &base_ov {
            if *m < at {
                prop_assert_eq!(out.get(m), Some(a), "base prefix entry kept at its Moment");
            } else {
                // A base entry at m >= at is in the discarded suffix: the tail's
                // spliced-in timeline governs [at, ∞), so out[m] is whatever the
                // *tail* re-keys there (its entry at m - at), never the base's
                // dropped entry. Usually that is nothing (out has no key m); when
                // a tail Moment aligns to m - at it legitimately re-keys onto m —
                // the rare coincidence that made this test flaky (issue #72) when
                // the assertion was the too-strong `!out.contains_key(m)`.
                prop_assert_eq!(
                    out.get(m),
                    tail_ov.get(&(m - at)),
                    "dropped base suffix entry is replaced by the tail's re-keyed timeline"
                );
            }
        }
        for (m, a) in &tail_ov {
            prop_assert_eq!(out.get(&(m + at)), Some(a), "tail entry re-keyed by +at");
        }
        prop_assert_eq!(composed.seed(), seed);
        prop_assert_eq!(composed.policy(), &policy);
        prop_assert!(standing_of(&composed).is_empty());
    }

    /// **Bit-identical replay at `at > 0`** for an override-only composition: a
    /// branch-local delta of admissible (always-firing) overrides, composed onto a
    /// base, reproduces its own run at the re-keyed Moments — the task-93 property
    /// `branch(genesis, compose(base, delta))` reproduces delta, for the
    /// override-covered (no seed draw) case the task-45 gate covers.
    #[test]
    fn compose_override_only_replays_bit_identical(
        moments in prop::collection::btree_set(0u64..BOUND, 0..10),
        seed in any::<u64>(),
        at in 1u64..BOUND,
    ) {
        // Nominal is admissible on a fault-class NetFlow point, so every override
        // fires and the seed is never drawn — no cross-splice desync.
        let net = P::NetFlow { src: NodeId(0), dst: NodeId(1), conn: ConnId(0), event: environment::FlowEvent::Open };
        let tail_ov: BTreeMap<Moment, Action> =
            moments.iter().map(|m| (*m, Action::Guest(Answer::Nominal))).collect();
        let tail = EnvSpec::Recorded {
            seed, policy: FaultPolicy::none(), overrides: tail_ov, standing: vec![], reseeds: Default::default(),
        };
        let base = EnvSpec::Recorded {
            seed, policy: FaultPolicy::none(), overrides: BTreeMap::new(), standing: vec![], reseeds: Default::default(),
        };
        let composed = EnvCodec::compose(&base, &tail, at).expect("override-only");

        let tail_sched: Vec<(Moment, P)> = moments.iter().map(|m| (*m, net)).collect();
        let comp_sched: Vec<(Moment, P)> = moments.iter().map(|m| (m + at, net)).collect();
        let a = run_guest_schedule(&mut tail.materialize(), &tail_sched);
        let b = run_guest_schedule(&mut composed.materialize(), &comp_sched);
        prop_assert_eq!(a, b, "the re-keyed delta reproduces its run");
    }

    /// `compose` **fails closed** whenever either input carries a standing fault
    /// (a V-time axis the `Moment` offset cannot re-key), at any offset.
    #[test]
    fn compose_rejects_any_standing_fault(
        ov in arb_bounded_overrides(),
        at in 0u64..BOUND,
    ) {
        let plain = recorded(ov.clone());
        let with_standing = recorded_with(ov, vec![sf(DecisionClass::NetFlow)]);
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
fn compose_rekeys_at_nonzero_concrete() {
    // Non-genesis splice at = 10: base keeps its prefix [0, 10), tail shifts to
    // [10, ∞). A base override >= at is dropped (it is in the discarded suffix).
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            DecisionClass::NetFlow,
            1,
            2,
            &[environment::Fault::NetReset],
        )
        .unwrap();
    let base = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([
            (5, Action::Guest(Answer::Nominal)),          // < at → kept at 5
            (20, Action::Guest(Answer::Supply(vec![1]))), // >= at → dropped
        ]),
        standing: vec![],
        reseeds: Default::default(),
    };
    let tail = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([
            (
                0,
                Action::Host(environment::HostFault::InjectInterrupt { vector: 1 }),
            ),
            (3, Action::Guest(Answer::Nominal)),
        ]),
        standing: vec![],
        reseeds: Default::default(),
    };
    let composed = EnvCodec::compose(&base, &tail, 10).unwrap();
    let out = composed.overrides();
    assert_eq!(composed.seed(), 0xABCD, "base seed carried");
    assert_eq!(composed.policy(), &policy, "base policy carried");
    assert!(standing_of(&composed).is_empty());
    assert!(out.contains_key(&5), "base prefix entry kept");
    assert!(!out.contains_key(&20), "base entry >= at dropped");
    assert_eq!(
        out.get(&10),
        Some(&Action::Host(environment::HostFault::InjectInterrupt {
            vector: 1
        })),
        "tail Moment 0 re-keyed to at+0 = 10"
    );
    assert_eq!(
        out.get(&13),
        Some(&Action::Guest(Answer::Nominal)),
        "tail 3 → 13"
    );
    assert_eq!(out.len(), 3);
}

#[test]
fn compose_tail_rekeys_onto_dropped_base_moment() {
    // The issue-#72 intermittent-flake counterexample, pinned deterministic. A
    // base override sits at a suffix Moment (>= at) and a *tail* override re-keys
    // exactly onto that same Moment (677_257 + 172_752 == 850_009). `compose` is
    // correct: the base suffix is discarded and the tail's timeline governs
    // [at, ∞), so out[850_009] carries the TAIL's override, not the base's.
    //
    // This is the case the proptest's old `!out.contains_key(m)` assertion could
    // not tolerate; distinct SkewTime values here (base 0, tail 7) also pin that
    // the tail — not the base — wins the aligned Moment.
    let at: Moment = 172_752;
    assert_eq!(
        677_257 + at,
        850_009,
        "the tail Moment re-keys onto the base's"
    );
    let base = recorded(BTreeMap::from([(
        850_009,
        Action::Host(environment::HostFault::SkewTime(Span(0))),
    )]));
    let tail = recorded(BTreeMap::from([(
        677_257,
        Action::Host(environment::HostFault::SkewTime(Span(7))),
    )]));
    let out = EnvCodec::compose(&base, &tail, at).unwrap();
    let m = out.overrides();
    assert_eq!(
        m.len(),
        1,
        "base suffix dropped; only the tail's entry remains"
    );
    assert_eq!(
        m.get(&850_009),
        Some(&Action::Host(environment::HostFault::SkewTime(Span(7)))),
        "the aligned Moment carries the TAIL's re-keyed override, not the base's"
    );
    assert_ne!(
        m.get(&850_009),
        Some(&Action::Host(environment::HostFault::SkewTime(Span(0)))),
        "the base's dropped suffix value must not leak through"
    );
}

#[test]
fn compose_prefix_filter_is_strict_less_than() {
    // A base override exactly AT the splice Moment is dropped (prefix is [0, at)),
    // with NO tail entry there to mask it — so the filter is strict `<`, not `<=`.
    let at: Moment = 50;
    let base = recorded(BTreeMap::from([
        (at - 1, Action::Guest(Answer::Nominal)), // kept
        (at, Action::Guest(Answer::Nominal)),     // dropped by strict `<`
        (at + 1, Action::Guest(Answer::Nominal)), // dropped (> at)
    ]));
    let tail = recorded(BTreeMap::new()); // empty (Recorded) → nothing re-keyed to `at`
    let out = EnvCodec::compose(&base, &tail, at).unwrap();
    let m = out.overrides();
    assert!(m.contains_key(&(at - 1)), "prefix entry (< at) kept");
    assert!(
        !m.contains_key(&at),
        "entry exactly at the splice is dropped by the strict `<` (a `<=` mutant keeps it)"
    );
    assert!(!m.contains_key(&(at + 1)));
    assert_eq!(m.len(), 1);
}

#[test]
fn compose_rejects_seeded_input() {
    // A pure Seeded env's decisions are all seed-serviced, so splicing it would
    // desync the fresh PRNG stream (task 93). Rejected at any offset, either side.
    let seeded = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };
    let rec = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))])); // seed 0, policy none
    for at in [0u64, 1, 10, u64::MAX] {
        assert_eq!(
            EnvCodec::compose(&seeded, &rec, at),
            Err(EnvError::UnsupportedComposition),
            "Seeded base rejected at {at}"
        );
        assert_eq!(
            EnvCodec::compose(&rec, &seeded, at),
            Err(EnvError::UnsupportedComposition),
            "Seeded tail rejected at {at}"
        );
    }
}

#[test]
fn compose_fails_closed_on_standing_seed_or_policy_mismatch() {
    // Standing fault (V-time axis ≠ Moment offset) → reject. Both Recorded so the
    // Seeded check does not preempt; the cause is the standing fault.
    let base_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::NetFlow)]);
    let plain = recorded(BTreeMap::new()); // seed 0, policy none, no standing
    assert_eq!(
        EnvCodec::compose(&base_standing, &plain, 0),
        Err(EnvError::UnsupportedComposition),
        "standing in base is rejected"
    );
    let tail_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::BlockIo)]);
    assert_eq!(
        EnvCodec::compose(&plain, &tail_standing, 7),
        Err(EnvError::UnsupportedComposition),
        "standing in tail is rejected"
    );

    // Seed mismatch — two Recorded envs (so the Seeded check passes) with
    // different seeds. One EnvSpec cannot carry a piecewise stream.
    let rec = |seed, policy| EnvSpec::Recorded {
        seed,
        policy,
        overrides: BTreeMap::new(),
        standing: vec![],
        reseeds: Default::default(),
    };
    assert_eq!(
        EnvCodec::compose(
            &rec(1, FaultPolicy::none()),
            &rec(2, FaultPolicy::none()),
            0
        ),
        Err(EnvError::UnsupportedComposition),
        "seed mismatch is rejected"
    );

    // Policy mismatch — same seed, different policy.
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            DecisionClass::Process,
            1,
            2,
            &[environment::Fault::ProcKill],
        )
        .unwrap();
    assert_eq!(
        EnvCodec::compose(&rec(1, FaultPolicy::none()), &rec(1, policy), 0),
        Err(EnvError::UnsupportedComposition),
        "policy mismatch is rejected"
    );
}

#[test]
fn compose_offset_overflow_is_rejected() {
    // A tail Moment shifted past u64::MAX must reject, never saturate two overrides
    // onto one colliding key.
    let base = recorded(BTreeMap::new()); // seed 0, policy none, no standing
    let tail = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (1, Action::Guest(Answer::Supply(vec![9]))),
    ]));
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
fn compose_override_only_reproduces_at_nonzero() {
    // The spec's task-93 property for the override-covered case: a branch-local
    // delta composed onto a base at at > 0 reproduces its run at the re-keyed
    // Moments. All overrides admissible (always fire) → no seed draw → no desync.
    let net = P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(0),
        event: environment::FlowEvent::Open,
    };
    let delta = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (
            3,
            Action::Guest(Answer::Fault(environment::Fault::NetReset)),
        ),
        (7, Action::Guest(Answer::Nominal)),
    ]));
    let base = recorded(BTreeMap::new()); // seed 0, policy none — matches delta
    let at: Moment = 1_000;
    let composed = EnvCodec::compose(&base, &delta, at).unwrap();

    let delta_sched: Vec<(Moment, P)> = [0u64, 3, 7].iter().map(|m| (*m, net)).collect();
    let comp_sched: Vec<(Moment, P)> = delta_sched.iter().map(|(m, p)| (m + at, *p)).collect();
    let delta_trace = run_guest_schedule(&mut delta.materialize(), &delta_sched);
    let comp_trace = run_guest_schedule(&mut composed.materialize(), &comp_sched);
    assert_eq!(
        delta_trace, comp_trace,
        "the re-keyed delta reproduces its run"
    );
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
            Action::Guest(Answer::Fault(environment::Fault::NetReset)),
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

// ---- reseed-marker splicing (task 78) ---------------------------------------

/// A `Recorded` spec with only a reseed table (no overrides/standing).
fn reseed_spec(seed: u64, reseeds: &[(Moment, u64)]) -> EnvSpec {
    EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::new(),
        standing: vec![],
        reseeds: reseeds.iter().copied().collect(),
    }
}

#[test]
fn compose_splices_reseed_markers_positionally_like_overrides() {
    // base: markers at 0 (its own branch reseed) and 300 (past the cut —
    // superseded by the tail's branch); tail: marker at 0 (its branch reseed)
    // and a mid-window one at 40.
    let base = reseed_spec(7, &[(0, 111), (300, 222)]);
    let tail = reseed_spec(7, &[(0, 333), (40, 444)]);
    let composed = EnvCodec::compose(&base, &tail, 250).expect("override-free, same seed/policy");
    let got: Vec<(Moment, u64)> = composed.reseeds().iter().map(|(m, s)| (*m, *s)).collect();
    assert_eq!(
        got,
        vec![(0, 111), (250, 333), (290, 444)],
        "base keeps markers < at; tail markers re-key by + at"
    );
}

#[test]
fn compose_rejects_reseed_rekey_overflow() {
    let base = reseed_spec(7, &[]);
    let tail = reseed_spec(7, &[(10, 1)]);
    assert_eq!(
        EnvCodec::compose(&base, &tail, u64::MAX - 5),
        Err(environment::EnvError::Overflow),
        "a wrapping marker re-key must reject, never collapse"
    );
}

#[test]
fn mutate_preserves_reseed_markers_verbatim() {
    let spec = reseed_spec(7, &[(0, 111), (500, 222)]);
    for salt in 0u64..32 {
        let out = EnvCodec::mutate(&spec, salt);
        assert_eq!(
            out.reseeds(),
            spec.reseeds(),
            "reseed markers are timeline facts — never mutated (salt {salt})"
        );
    }
}

#[test]
fn record_reseed_promotes_and_round_trips() {
    let mut spec = EnvCodec::seeded(9, FaultPolicy::none());
    spec.record_reseed(100, 0xAB);
    spec.record_reseed(40, 0xCD);
    assert!(matches!(spec, EnvSpec::Recorded { .. }));
    let got: Vec<(Moment, u64)> = spec.reseeds().iter().map(|(m, s)| (*m, *s)).collect();
    assert_eq!(got, vec![(40, 0xCD), (100, 0xAB)]);
    assert_eq!(EnvSpec::decode(&spec.encode()).unwrap(), spec);
}

#[test]
fn non_ascending_reseed_table_is_rejected_on_decode() {
    // Encode a two-marker spec, then swap the marker records (each is 16
    // bytes: moment u64 + seed u64) so the table is descending — Malformed.
    let spec = reseed_spec(0, &[(1, 10), (2, 20)]);
    let bytes = spec.encode();
    let n = bytes.len();
    let mut swapped = bytes.clone();
    swapped[n - 32..n - 16].copy_from_slice(&bytes[n - 16..]);
    swapped[n - 16..].copy_from_slice(&bytes[n - 32..n - 16]);
    assert_eq!(
        EnvSpec::decode(&swapped),
        Err(environment::EnvError::Malformed),
        "a non-ascending reseed table must reject"
    );
}
