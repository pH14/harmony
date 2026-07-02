// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 4 — the **mutator gates** (≥256 proptests over arbitrary recorded envs):
//!
//! - **determinism** — same `(env, salt)` ⇒ same output.
//! - **well-formedness round-trip** — the result encodes and decodes cleanly per
//!   `environment`'s codec, and re-encoding is stable (a canonical blob).
//! - **guest overrides verbatim** — no [`Action::Guest`] is removed, relocated,
//!   or overwritten.
//! - **no standing faults introduced** — the output's standing set equals the
//!   base's.
//! - **no `Moment` overflow/collision** — every output is a valid, strictly
//!   ascending map (guaranteed by clean decode).
//! - **vocabulary confinement** — only enforced v1 host faults
//!   ([`CorruptMemory`]/[`InjectInterrupt`]) appear.

use std::collections::BTreeMap;

use environment::{
    Action, Answer, BitMask, DecisionClass, EnvSpec, Fault, FaultPolicy, HostFault, Moment,
    StandingFault, VTime,
};
use proptest::prelude::*;
use tactics_regime::SeqMutators;

/// An arbitrary v1 host fault (the vocabulary this task's mutators operate in).
fn v1_host() -> impl Strategy<Value = HostFault> {
    prop_oneof![
        (any::<u64>(), any::<u64>()).prop_map(|(gpa, m)| HostFault::CorruptMemory {
            gpa,
            mask: BitMask(m)
        }),
        any::<u8>().prop_map(|vector| HostFault::InjectInterrupt { vector }),
    ]
}

/// An arbitrary guest answer for an override.
fn guest_answer() -> impl Strategy<Value = Answer> {
    prop_oneof![
        Just(Answer::Nominal),
        Just(Answer::Fault(Fault::BlockEio)),
        prop::collection::vec(any::<u8>(), 0..8).prop_map(Answer::Supply),
    ]
}

/// An arbitrary override action (host confined to v1, or a guest answer).
fn action() -> impl Strategy<Value = Action> {
    prop_oneof![
        v1_host().prop_map(Action::Host),
        guest_answer().prop_map(Action::Guest),
    ]
}

/// An arbitrary standing fault (to prove the mutators carry it through verbatim
/// and never add one).
fn standing() -> impl Strategy<Value = StandingFault> {
    (
        any::<u64>(),
        any::<u64>(),
        prop::collection::vec(any::<u8>(), 0..4),
    )
        .prop_map(|(a, b, target)| StandingFault {
            class: DecisionClass::NetFlow,
            target,
            window: (VTime(a.min(b)), VTime(a.max(b))),
        })
}

/// An arbitrary recorded env: a seed, a set of `Moment → Action` overrides at
/// unique moments, and some standing faults.
fn arb_env() -> impl Strategy<Value = EnvSpec> {
    (
        any::<u64>(),
        prop::collection::btree_map(any::<u64>(), action(), 0..24),
        prop::collection::vec(standing(), 0..3),
    )
        .prop_map(|(seed, overrides, standing)| EnvSpec::Recorded {
            seed,
            policy: FaultPolicy::none(),
            overrides,
            standing,
        })
}

/// The guest overrides of a spec.
fn guest_overrides(spec: &EnvSpec) -> BTreeMap<Moment, Answer> {
    spec.overrides()
        .iter()
        .filter_map(|(m, a)| a.guest_answer().map(|ans| (*m, ans.clone())))
        .collect()
}

/// The standing faults of a spec, canonicalized to a set for comparison.
fn standing_set(spec: &EnvSpec) -> Vec<StandingFault> {
    let mut v = match spec {
        EnvSpec::Recorded { standing, .. } => standing.clone(),
        EnvSpec::Seeded { .. } => Vec::new(),
    };
    v.sort_by_key(|s| (s.class as u16, s.target.clone(), s.window.0.0, s.window.1.0));
    v.dedup();
    v
}

fn is_v1(f: HostFault) -> bool {
    matches!(
        f,
        HostFault::CorruptMemory { .. } | HostFault::InjectInterrupt { .. }
    )
}

/// Assert every schedule-safety invariant on one mutator output against its base.
fn assert_well_formed(base: &EnvSpec, out: &EnvSpec) {
    // Well-formedness round-trip: encodes and decodes cleanly, and re-encoding
    // the decoded blob is byte-stable (canonical, strictly-ascending map).
    let bytes = out.encode();
    let back = EnvSpec::decode(&bytes).expect("output must decode cleanly");
    assert_eq!(back.encode(), bytes, "re-encode must be byte-stable");

    // Guest overrides verbatim.
    assert_eq!(
        guest_overrides(base),
        guest_overrides(out),
        "guest overrides must be preserved verbatim"
    );

    // No standing fault introduced.
    assert_eq!(
        standing_set(base),
        standing_set(out),
        "no standing fault may be introduced or dropped"
    );

    // Vocabulary confinement: only v1 host faults appear.
    for (_, f) in out.host_faults() {
        assert!(is_v1(f), "only enforced v1 host faults may appear: {f:?}");
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Every operator is deterministic and preserves every schedule-safety
    /// invariant on arbitrary recorded envs.
    #[test]
    fn operators_are_safe_and_deterministic(env in arb_env(), salt in any::<u64>()) {
        type MutOp = fn(&EnvSpec, u64) -> EnvSpec;
        let ops: [(&str, MutOp); 5] = [
            ("insert", SeqMutators::insert),
            ("delete", SeqMutators::delete),
            ("retarget", SeqMutators::retarget),
            ("shift", SeqMutators::shift),
            ("mutate", SeqMutators::mutate),
        ];
        for (name, op) in ops {
            let out = op(&env, salt);
            prop_assert_eq!(op(&env, salt), out.clone(), "{} must be deterministic", name);
            assert_well_formed(&env, &out);
        }
    }

    /// `shift` only relocates: it never adds or drops a host fault, so the
    /// multiset of host-fault values is invariant (a translation moves the region
    /// but preserves its contents; a collision/overflow fails closed, identical).
    #[test]
    fn shift_only_relocates(env in arb_env(), salt in any::<u64>()) {
        let sorted = |spec: &EnvSpec| {
            let mut v: Vec<HostFault> = spec.host_faults().map(|(_, f)| f).collect();
            v.sort();
            v
        };
        prop_assert_eq!(
            sorted(&env),
            sorted(&SeqMutators::shift(&env, salt)),
            "shift must neither add nor drop a host fault"
        );
    }
}
