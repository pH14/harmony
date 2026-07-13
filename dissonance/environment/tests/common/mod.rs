// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers: proptest strategies over the catalog, the host plane,
//! and the reproducer; a canonicalizer; a reference admissibility check
//! independent of the crate's own (so the override-semantics gate is a real
//! cross-check, not a tautology); and a frontier-simulating runner that drives a
//! `RecordedEnv` over a `Moment`-stamped schedule. Each `tests/*.rs` pulls only
//! what it needs.
#![allow(dead_code)]

use std::collections::BTreeMap;

use proptest::prelude::*;

use environment::{
    Action, Answer, BlockOp, ConnId, DecisionClass, DecisionPoint, EnvSpec, Environment, Fault,
    FaultPolicy, FlowEvent, HostFault, Moment, NodeId, Outcome, Ratio, StandingFault, Span,
};

/// Proptest config: spec case count, cut hard under Miri (kept for portability
/// even though this crate has no `unsafe`).
pub fn config(cases: u32) -> ProptestConfig {
    let mut cfg = ProptestConfig::with_cases(if cfg!(miri) { 16 } else { cases });
    if cfg!(miri) {
        cfg.failure_persistence = None;
    }
    cfg
}

// ---- guest faults, by class -----------------------------------------------

pub fn arb_net_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        any::<u64>().prop_map(|d| Fault::NetLatency(Span(d))),
        // `den >= 1` so the fault round-trips through `set_class` and the codec
        // and never asks the enforcer to divide by zero.
        (any::<u16>(), 1u16..=u16::MAX).prop_map(|(num, den)| Fault::NetLoss { num, den }),
        any::<u32>().prop_map(|bps| Fault::NetThrottle { bps }),
        Just(Fault::NetReset),
    ]
}

pub fn arb_block_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        Just(Fault::BlockEio),
        any::<u64>().prop_map(|d| Fault::BlockLatency(Span(d))),
        any::<u32>().prop_map(Fault::BlockTorn),
        Just(Fault::BlockNospc),
    ]
}

pub fn arb_proc_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        any::<u64>().prop_map(|d| Fault::ProcPause(Span(d))),
        Just(Fault::ProcKill),
        Just(Fault::ProcRestart),
    ]
}

pub fn arb_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        arb_net_fault(),
        arb_block_fault(),
        arb_proc_fault(),
        // Task 73: the parameterless buggify fault — exercises tag 16 through the
        // answer/action/codec round-trips.
        Just(Fault::BuggifyFire),
    ]
}

// ---- host plane -----------------------------------------------------------

/// A non-zero-denominator [`Ratio`] (every constructed ratio is valid).
pub fn arb_ratio() -> impl Strategy<Value = Ratio> {
    (any::<u64>(), 1u64..=u64::MAX)
        .prop_map(|(num, den)| Ratio::new(num, den).expect("den >= 1 by strategy bound"))
}

pub fn arb_host_fault() -> impl Strategy<Value = HostFault> {
    prop_oneof![
        any::<u64>().prop_map(|d| HostFault::SkewTime(Span(d))),
        arb_ratio().prop_map(HostFault::SetClockRate),
        (any::<u64>(), any::<u64>()).prop_map(|(gpa, mask)| HostFault::CorruptMemory {
            gpa,
            mask: environment::BitMask(mask),
        }),
        any::<u8>().prop_map(|vector| HostFault::InjectInterrupt { vector }),
    ]
}

/// An arbitrary action from either plane (guest answers may be inadmissible — for
/// override/codec fuzzing).
pub fn arb_action() -> impl Strategy<Value = Action> {
    prop_oneof![
        arb_host_fault().prop_map(Action::Host),
        arb_answer().prop_map(Action::Guest),
    ]
}

// ---- answers, policies, points --------------------------------------------

/// An arbitrary (possibly inadmissible) answer — for override/codec fuzzing.
pub fn arb_answer() -> impl Strategy<Value = Answer> {
    prop_oneof![
        Just(Answer::Nominal),
        prop::collection::vec(any::<u8>(), 0..64).prop_map(Answer::Supply),
        arb_fault().prop_map(Answer::Fault),
    ]
}

/// A non-baseline policy: every fault class gets a random probability (`den ≥ 1`)
/// and a random eligible subset.
pub fn arb_policy() -> impl Strategy<Value = FaultPolicy> {
    (
        (
            any::<u32>(),
            1u32..=u32::MAX,
            prop::collection::vec(arb_net_fault(), 0..6),
        ),
        (
            any::<u32>(),
            1u32..=u32::MAX,
            prop::collection::vec(arb_block_fault(), 0..5),
        ),
        (
            any::<u32>(),
            1u32..=u32::MAX,
            prop::collection::vec(arb_proc_fault(), 0..4),
        ),
        // Task 73 buggify biasing: a default `num/den` plus a few per-point
        // `(point, num, den)` overrides (`den >= 1`), so the buggify section of
        // the policy codec round-trips under the same proptests.
        (
            any::<u32>(),
            1u32..=u32::MAX,
            prop::collection::vec((any::<u32>(), any::<u32>(), 1u32..=u32::MAX), 0..4),
        ),
    )
        .prop_map(|(net, block, proc, buggify)| {
            let mut p = FaultPolicy::none();
            p.set_class(DecisionClass::NetFlow, net.0, net.1, &net.2)
                .expect("net class is a fault class with in-class faults");
            p.set_class(DecisionClass::BlockIo, block.0, block.1, &block.2)
                .expect("block class is a fault class with in-class faults");
            p.set_class(DecisionClass::Process, proc.0, proc.1, &proc.2)
                .expect("process class is a fault class with in-class faults");
            p.set_buggify_default(buggify.0, buggify.1)
                .expect("den >= 1 by strategy bound");
            for (point, num, den) in buggify.2 {
                p.set_buggify_point(point, num, den)
                    .expect("den >= 1 by strategy bound");
            }
            p
        })
}

pub fn arb_blockop() -> impl Strategy<Value = BlockOp> {
    prop_oneof![
        Just(BlockOp::Read),
        Just(BlockOp::Write),
        Just(BlockOp::Flush)
    ]
}

pub fn arb_class() -> impl Strategy<Value = DecisionClass> {
    prop_oneof![
        Just(DecisionClass::Entropy),
        Just(DecisionClass::Payload),
        Just(DecisionClass::Scheduler),
        Just(DecisionClass::NetFlow),
        Just(DecisionClass::BlockIo),
        Just(DecisionClass::Process),
        Just(DecisionClass::Buggify),
    ]
}

/// An arbitrary decision point. Supply lengths stay small so the suite is quick
/// and supply allocations stay bounded.
pub fn arb_point() -> impl Strategy<Value = DecisionPoint> {
    prop_oneof![
        (0u32..=4096).prop_map(|bytes| DecisionPoint::Entropy { bytes }),
        (0u32..=4096).prop_map(|bytes| DecisionPoint::Payload { bytes }),
        (0u32..=64).prop_map(|ready| DecisionPoint::Scheduler { ready }),
        (any::<u32>(), any::<u32>(), any::<u64>()).prop_map(|(s, d, c)| DecisionPoint::NetFlow {
            src: NodeId(s),
            dst: NodeId(d),
            conn: ConnId(c),
            event: FlowEvent::Open,
        }),
        (arb_blockop(), any::<u64>(), any::<u32>())
            .prop_map(|(op, lba, len)| DecisionPoint::BlockIo { op, lba, len }),
        any::<u32>().prop_map(|n| DecisionPoint::Process { node: NodeId(n) }),
        any::<u32>().prop_map(|point| DecisionPoint::Buggify { point }),
    ]
}

pub fn arb_standing() -> impl Strategy<Value = StandingFault> {
    (
        arb_class(),
        prop::collection::vec(any::<u8>(), 0..16),
        any::<u64>(),
        any::<u64>(),
    )
        .prop_map(|(class, target, a, b)| StandingFault {
            class,
            target,
            window: (a, b),
        })
}

/// An arbitrary `Moment`-keyed override map (host and guest actions on one axis).
pub fn arb_overrides() -> impl Strategy<Value = BTreeMap<Moment, Action>> {
    prop::collection::btree_map(any::<u64>(), arb_action(), 0..12)
}

/// An arbitrary `Moment`-keyed reseed-marker table (task 78).
pub fn arb_reseeds() -> impl Strategy<Value = BTreeMap<Moment, u64>> {
    prop::collection::btree_map(any::<u64>(), any::<u64>(), 0..6)
}

/// An arbitrary reproducer spec (standing in arbitrary order; use [`canon`]
/// before a structural round-trip comparison — the override map is already
/// canonical, being a `BTreeMap`).
pub fn arb_spec() -> impl Strategy<Value = EnvSpec> {
    prop_oneof![
        (any::<u64>(), arb_policy()).prop_map(|(seed, policy)| EnvSpec::Seeded { seed, policy }),
        (
            any::<u64>(),
            arb_policy(),
            arb_overrides(),
            prop::collection::vec(arb_standing(), 0..6),
            arb_reseeds(),
        )
            .prop_map(
                |(seed, policy, overrides, standing, reseeds)| EnvSpec::Recorded {
                    seed,
                    policy,
                    overrides,
                    standing,
                    reseeds,
                }
            ),
    ]
}

/// Canonicalize a spec the way [`EnvSpec::encode`] does: sort standing by its
/// key, dropping duplicates. (The override map is a `BTreeMap`, so it is already
/// canonical.) `decode(encode(canon(s)))` equals `canon(s)`.
pub fn canon(spec: EnvSpec) -> EnvSpec {
    match spec {
        EnvSpec::Recorded {
            seed,
            policy,
            overrides,
            mut standing,
            reseeds,
        } => {
            standing.sort_by(|a, b| standing_key(a).cmp(&standing_key(b)));
            standing.dedup_by(|a, b| standing_key(a) == standing_key(b));
            EnvSpec::Recorded {
                seed,
                policy,
                overrides,
                standing,
                reseeds,
            }
        }
        s => s,
    }
}

fn standing_key(s: &StandingFault) -> (u16, &[u8], u64, u64) {
    (
        s.class as u16,
        s.target.as_slice(),
        s.window.0,
        s.window.1,
    )
}

// ---- a frontier-simulating runner -----------------------------------------

/// Drive a `RecordedEnv` over a `Moment`-stamped guest schedule the way the
/// frontier would: set the `Moment` for each decision, then `decide`. Returns the
/// answer sequence. This is the pure-crate stand-in for a real reactive run — the
/// frontier supplies the retired-instruction count; here the test does.
pub fn run_guest_schedule(
    env: &mut environment::RecordedEnv,
    sched: &[(Moment, DecisionPoint)],
) -> Vec<Outcome> {
    sched
        .iter()
        .map(|(at, p)| {
            env.set_moment(*at);
            env.decide(p)
        })
        .collect()
}

// ---- a reference admissibility check, independent of the crate's own -------

/// Re-derives the spec's admissibility prose so the override-semantics gate
/// checks the implementation against an independent statement of the rule, not
/// against itself.
pub fn ref_admissible(point: &DecisionPoint, ans: &Answer) -> bool {
    match point {
        DecisionPoint::Entropy { bytes } | DecisionPoint::Payload { bytes } => match ans {
            Answer::Supply(v) => v.len() as u64 == *bytes as u64,
            _ => false,
        },
        DecisionPoint::Scheduler { ready } => match ans {
            Answer::Supply(v) => {
                v.len() == 4 && u32::from_le_bytes([v[0], v[1], v[2], v[3]]) < *ready
            }
            _ => false,
        },
        DecisionPoint::NetFlow { .. }
        | DecisionPoint::Process { .. }
        | DecisionPoint::Buggify { .. } => match ans {
            Answer::Nominal => true,
            Answer::Fault(f) => f.class() == point.class(),
            Answer::Supply(_) => false,
        },
        DecisionPoint::BlockIo { len, .. } => match ans {
            Answer::Nominal => true,
            Answer::Fault(f) => {
                f.class() == point.class()
                    && match f {
                        Fault::BlockTorn(n) => *n as u64 <= *len as u64,
                        _ => true,
                    }
            }
            Answer::Supply(_) => false,
        },
    }
}
