// SPDX-License-Identifier: AGPL-3.0-or-later

mod common;

use std::collections::BTreeMap;

use common::{arb_action, arb_policy, arb_spec, config, run_guest_schedule};
use fault_policy::{
    Action, Answer, ConnId, DecisionClass, DecisionPoint as P, EnvCodec, EnvError, EnvSpec,
    Environment, FaultPolicy, Moment, NodeId, Outcome, Span, StandingFault,
};
use proptest::prelude::*;

const BOUND: Moment = 1 << 20;

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
        payloads: None,
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

fn sf(class: DecisionClass) -> StandingFault {
    StandingFault {
        class,
        target: vec![1, 2],
        window: (0, 9),
    }
}

proptest! {
    #![proptest_config(config(256))]

    #[test]
    fn compose_rekeys_overrides_at_any_offset(
        base_ov in arb_bounded_overrides(),
        tail_ov in arb_bounded_overrides(),
        seed in any::<u64>(),
        policy in arb_policy(),
        at in 0u64..BOUND,
    ) {
        let base = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: base_ov.clone(), standing: vec![], reseeds: Default::default(), payloads: None,
        };
        let tail = EnvSpec::Recorded {
            seed, policy: policy.clone(), overrides: tail_ov.clone(), standing: vec![], reseeds: Default::default(), payloads: None,
        };
        let composed = EnvCodec::compose(&base, &tail, at).expect("override-only, same seed/policy");
        let out = composed.overrides();

        let kept_base = base_ov.iter().filter(|(m, _)| **m < at).count();
        prop_assert_eq!(out.len(), kept_base + tail_ov.len());
        for (m, a) in &base_ov {
            if *m < at {
                prop_assert_eq!(out.get(m), Some(a), "base prefix entry kept at its Moment");
            } else {
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

    #[test]
    fn compose_override_only_replays_bit_identical(
        moments in prop::collection::btree_set(0u64..BOUND, 0..10),
        seed in any::<u64>(),
        at in 1u64..BOUND,
    ) {
        let net = P::NetFlow { src: NodeId(0), dst: NodeId(1), conn: ConnId(0), event: fault_policy::FlowEvent::Open };
        let tail_ov: BTreeMap<Moment, Action> =
            moments.iter().map(|m| (*m, Action::Guest(Answer::Nominal))).collect();
        let tail = EnvSpec::Recorded {
            seed, policy: FaultPolicy::none(), overrides: tail_ov, standing: vec![], reseeds: Default::default(), payloads: None,
        };
        let base = EnvSpec::Recorded {
            seed, policy: FaultPolicy::none(), overrides: BTreeMap::new(), standing: vec![], reseeds: Default::default(), payloads: None,
        };
        let composed = EnvCodec::compose(&base, &tail, at).expect("override-only");

        let tail_sched: Vec<(Moment, P)> = moments.iter().map(|m| (*m, net)).collect();
        let comp_sched: Vec<(Moment, P)> = moments.iter().map(|m| (m + at, net)).collect();
        let a = run_guest_schedule(&mut tail.materialize(), &tail_sched);
        let b = run_guest_schedule(&mut composed.materialize(), &comp_sched);
        prop_assert_eq!(a, b, "the re-keyed delta reproduces its run");
    }

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

    #[test]
    fn mutate_is_deterministic_and_legal(spec in arb_spec(), salt in any::<u64>()) {
        let a = EnvCodec::mutate(&spec, salt);
        let b = EnvCodec::mutate(&spec, salt);
        prop_assert_eq!(&a, &b, "same (env, salt) ⇒ same proposal");
        prop_assert!(matches!(a, EnvSpec::Recorded { .. }), "mutate yields Recorded");
        let decoded = EnvSpec::decode(&a.encode()).expect("legal blob");
        prop_assert_eq!(decoded.encode(), a.encode(), "byte-stable round-trip");
    }

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
    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            DecisionClass::NetFlow,
            1,
            2,
            &[fault_policy::Fault::NetReset],
        )
        .unwrap();
    let base = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([
            (5, Action::Guest(Answer::Nominal)),
            (20, Action::Guest(Answer::Supply(vec![1]))),
        ]),
        standing: vec![],
        reseeds: Default::default(),
        payloads: None,
    };
    let tail = EnvSpec::Recorded {
        seed: 0xABCD,
        policy: policy.clone(),
        overrides: BTreeMap::from([
            (
                0,
                Action::Host(fault_policy::HostFault::InjectInterrupt { vector: 1 }),
            ),
            (3, Action::Guest(Answer::Nominal)),
        ]),
        standing: vec![],
        reseeds: Default::default(),
        payloads: None,
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
        Some(&Action::Host(fault_policy::HostFault::InjectInterrupt {
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
    let at: Moment = 172_752;
    assert_eq!(
        677_257 + at,
        850_009,
        "the tail Moment re-keys onto the base's"
    );
    let base = recorded(BTreeMap::from([(
        850_009,
        Action::Host(fault_policy::HostFault::SkewTime(Span(0))),
    )]));
    let tail = recorded(BTreeMap::from([(
        677_257,
        Action::Host(fault_policy::HostFault::SkewTime(Span(7))),
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
        Some(&Action::Host(fault_policy::HostFault::SkewTime(Span(7)))),
        "the aligned Moment carries the TAIL's re-keyed override, not the base's"
    );
    assert_ne!(
        m.get(&850_009),
        Some(&Action::Host(fault_policy::HostFault::SkewTime(Span(0)))),
        "the base's dropped suffix value must not leak through"
    );
}

#[test]
fn compose_prefix_filter_is_strict_less_than() {
    let at: Moment = 50;
    let base = recorded(BTreeMap::from([
        (at - 1, Action::Guest(Answer::Nominal)),
        (at, Action::Guest(Answer::Nominal)),
        (at + 1, Action::Guest(Answer::Nominal)),
    ]));
    let tail = recorded(BTreeMap::new());
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
    let seeded = EnvSpec::Seeded {
        seed: 0,
        policy: FaultPolicy::none(),
    };
    let rec = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
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
    let base_standing = recorded_with(BTreeMap::new(), vec![sf(DecisionClass::NetFlow)]);
    let plain = recorded(BTreeMap::new());
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

    let mut base_payloads = plain.clone();
    base_payloads.set_payloads(Some(vec![vec![1, 2, 3]]));
    assert_eq!(
        EnvCodec::compose(&base_payloads, &plain, 0),
        Err(EnvError::UnsupportedComposition),
        "payload tape in base is rejected"
    );
    let mut tail_payloads = plain.clone();
    tail_payloads.set_payloads(Some(vec![vec![4, 5]]));
    assert_eq!(
        EnvCodec::compose(&plain, &tail_payloads, 0),
        Err(EnvError::UnsupportedComposition),
        "payload tape in tail is rejected"
    );

    let rec = |seed, policy| EnvSpec::Recorded {
        seed,
        policy,
        overrides: BTreeMap::new(),
        standing: vec![],
        reseeds: Default::default(),
        payloads: None,
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

    let mut policy = FaultPolicy::none();
    policy
        .set_class(
            DecisionClass::Process,
            1,
            2,
            &[fault_policy::Fault::ProcKill],
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
    let base = recorded(BTreeMap::new());
    let tail = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (1, Action::Guest(Answer::Supply(vec![9]))),
    ]));
    assert_eq!(
        EnvCodec::compose(&base, &tail, u64::MAX),
        Err(EnvError::Overflow),
        "a tail Moment shifted past u64::MAX is rejected, not saturated"
    );
    let single = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let ok = EnvCodec::compose(&base, &single, u64::MAX).unwrap();
    assert!(ok.overrides().contains_key(&u64::MAX));
    assert_eq!(ok.overrides().len(), 1);
}

#[test]
fn compose_override_only_reproduces_at_nonzero() {
    let net = P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(0),
        event: fault_policy::FlowEvent::Open,
    };
    let delta = recorded(BTreeMap::from([
        (0, Action::Guest(Answer::Nominal)),
        (
            3,
            Action::Guest(Answer::Fault(fault_policy::Fault::NetReset)),
        ),
        (7, Action::Guest(Answer::Nominal)),
    ]));
    let base = recorded(BTreeMap::new());
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
    assert_eq!(EnvSpec::decode(&mutated.encode()).unwrap(), mutated);
}

#[test]
fn mutate_never_disturbs_a_guest_only_spec() {
    let guest = BTreeMap::from([
        (10, Action::Guest(Answer::Nominal)),
        (
            20,
            Action::Guest(Answer::Fault(fault_policy::Fault::NetReset)),
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
        assert_eq!(out.len(), guest.len() + 1, "exactly one host action added");
    }
}

#[test]
fn materialized_recorded_default_moment_is_zero() {
    let env_spec = recorded(BTreeMap::from([(0, Action::Guest(Answer::Nominal))]));
    let mut env = env_spec.materialize();
    let p = P::Process { node: NodeId(0) };
    assert_eq!(env.moment(), 0);
    assert_eq!(env.decide(&p), Outcome::Resolved(Answer::Nominal));
}

#[test]
fn set_moment_is_reflected_by_moment_accessor() {
    let mut env = recorded(BTreeMap::new()).materialize();
    env.set_moment(0xDEAD_BEEF_0000_1234);
    assert_eq!(env.moment(), 0xDEAD_BEEF_0000_1234);
    env.set_moment(7);
    assert_eq!(env.moment(), 7, "tracks the most recent set_moment");
}

fn reseed_spec(seed: u64, reseeds: &[(Moment, u64)]) -> EnvSpec {
    EnvSpec::Recorded {
        seed,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::new(),
        standing: vec![],
        reseeds: reseeds.iter().copied().collect(),
        payloads: None,
    }
}

#[test]
fn compose_splices_reseed_markers_positionally_like_overrides() {
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
        Err(fault_policy::EnvError::Overflow),
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
    let spec = reseed_spec(0, &[(1, 10), (2, 20)]);
    let bytes = spec.encode();
    let n = bytes.len();
    let mut swapped = bytes.clone();
    swapped[n - 32..n - 16].copy_from_slice(&bytes[n - 16..]);
    swapped[n - 16..].copy_from_slice(&bytes[n - 32..n - 16]);
    assert_eq!(
        EnvSpec::decode(&swapped),
        Err(fault_policy::EnvError::Malformed),
        "a non-ascending reseed table must reject"
    );
}
