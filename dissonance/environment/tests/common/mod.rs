// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared test helpers: proptest strategies over the catalog and the reproducer,
//! a canonicalizer, and a reference admissibility check independent of the
//! crate's own (so the override-semantics gate is a real cross-check, not a
//! tautology). Each `tests/*.rs` pulls only what it needs.
#![allow(dead_code)]

use std::collections::BTreeMap;

use proptest::prelude::*;

use environment::{
    Answer, BlockOp, ConnId, CorruptSpec, DecisionClass, DecisionId, DecisionPoint, EnvSpec, Fault,
    FaultPolicy, NodeId, StandingFault, VTime,
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

// ---- faults, by class -----------------------------------------------------

pub fn arb_net_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        Just(Fault::NetDrop),
        any::<u64>().prop_map(|d| Fault::NetDelay(VTime(d))),
        Just(Fault::NetReorder),
        Just(Fault::NetDup),
        (any::<u32>(), any::<u8>())
            .prop_map(|(offset, xor)| Fault::NetCorrupt(CorruptSpec { offset, xor })),
    ]
}

pub fn arb_block_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        Just(Fault::BlockEio),
        any::<u64>().prop_map(|d| Fault::BlockLatency(VTime(d))),
        any::<u32>().prop_map(Fault::BlockTorn),
        Just(Fault::BlockNospc),
    ]
}

pub fn arb_proc_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![
        any::<u64>().prop_map(|d| Fault::ProcPause(VTime(d))),
        Just(Fault::ProcKill),
        Just(Fault::ProcRestart),
    ]
}

pub fn arb_fault() -> impl Strategy<Value = Fault> {
    prop_oneof![arb_net_fault(), arb_block_fault(), arb_proc_fault()]
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
    )
        .prop_map(|(net, block, proc)| {
            let mut p = FaultPolicy::none();
            p.set_class(DecisionClass::NetSend, net.0, net.1, &net.2)
                .expect("net class is a fault class with in-class faults");
            p.set_class(DecisionClass::BlockIo, block.0, block.1, &block.2)
                .expect("block class is a fault class with in-class faults");
            p.set_class(DecisionClass::Process, proc.0, proc.1, &proc.2)
                .expect("process class is a fault class with in-class faults");
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
        Just(DecisionClass::NetSend),
        Just(DecisionClass::BlockIo),
        Just(DecisionClass::Process),
    ]
}

/// An arbitrary decision point. Supply lengths stay small so the suite is quick
/// and supply allocations stay bounded.
pub fn arb_point() -> impl Strategy<Value = DecisionPoint> {
    prop_oneof![
        (0u32..=4096).prop_map(|bytes| DecisionPoint::Entropy { bytes }),
        (0u32..=4096).prop_map(|bytes| DecisionPoint::Payload { bytes }),
        (0u32..=64).prop_map(|ready| DecisionPoint::Scheduler { ready }),
        (any::<u32>(), any::<u32>(), any::<u64>(), any::<u32>()).prop_map(|(s, d, c, l)| {
            DecisionPoint::NetSend {
                src: NodeId(s),
                dst: NodeId(d),
                conn: ConnId(c),
                len: l,
            }
        }),
        (arb_blockop(), any::<u64>(), any::<u32>())
            .prop_map(|(op, lba, len)| DecisionPoint::BlockIo { op, lba, len }),
        any::<u32>().prop_map(|n| DecisionPoint::Process { node: NodeId(n) }),
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
            window: (VTime(a), VTime(b)),
        })
}

/// An arbitrary reproducer spec (overrides/standing in arbitrary order; use
/// [`canon`] before a structural round-trip comparison).
pub fn arb_spec() -> impl Strategy<Value = EnvSpec> {
    prop_oneof![
        (any::<u64>(), arb_policy()).prop_map(|(seed, policy)| EnvSpec::Seeded { seed, policy }),
        (
            any::<u64>(),
            arb_policy(),
            prop::collection::vec((any::<u64>().prop_map(DecisionId), arb_answer()), 0..12),
            prop::collection::vec(arb_standing(), 0..6),
        )
            .prop_map(|(seed, policy, overrides, standing)| EnvSpec::Recorded {
                seed,
                policy,
                overrides,
                standing,
            }),
    ]
}

/// Canonicalize a spec the way [`EnvSpec::encode`] does: sort overrides by id and
/// standing by its key, dropping duplicates. `decode(encode(canon(s)))` equals
/// `canon(s)`.
pub fn canon(spec: EnvSpec) -> EnvSpec {
    match spec {
        EnvSpec::Recorded {
            seed,
            policy,
            overrides,
            mut standing,
        } => {
            // Dedup overrides by id, last-wins — identically to `EnvSpec::encode`
            // and `materialize` (a `BTreeMap` built in vector order).
            let mut map: BTreeMap<u64, Answer> = BTreeMap::new();
            for (id, ans) in overrides {
                map.insert(id.0, ans);
            }
            let overrides: Vec<(DecisionId, Answer)> =
                map.into_iter().map(|(k, v)| (DecisionId(k), v)).collect();
            standing.sort_by(|a, b| standing_key(a).cmp(&standing_key(b)));
            standing.dedup_by(|a, b| standing_key(a) == standing_key(b));
            EnvSpec::Recorded {
                seed,
                policy,
                overrides,
                standing,
            }
        }
        s => s,
    }
}

fn standing_key(s: &StandingFault) -> (u16, &[u8], u64, u64) {
    (
        s.class as u16,
        s.target.as_slice(),
        s.window.0.0,
        s.window.1.0,
    )
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
        DecisionPoint::NetSend { .. } | DecisionPoint::Process { .. } => match ans {
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
