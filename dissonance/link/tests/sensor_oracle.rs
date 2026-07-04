// SPDX-License-Identifier: AGPL-3.0-or-later
//! The link [`LinkSensor`] and [`AlwaysViolation`] oracle (task 73 gate 2).
//!
//! - A `sometimes` hit / state change yields the right `(Moment, Feature)`, and a
//!   hit is admitted as a checkpoint candidate by the spine `Archive` on the toy.
//! - A planted always-violation makes `AlwaysViolation` mint a `Bug` with a
//!   stable fingerprint — byte-identical to the explorer's own `Assertion`
//!   fingerprint (cross-checked against `TerminalOracle`), so the two dedup.

use explorer::{
    Archive, ChannelId, CoverageArchive, Environment, Feature, FeatureId, Fork, GuestEvent,
    IdentityCells, Moment, Oracle, RunTrace, Sensor, SnapId, StopReason, TerminalOracle, VTime,
    VirtualExemplar,
};
use link::{AlwaysViolation, LINK_ASSERT_CHANNEL, LINK_STATE_CHANNEL, LinkSensor, decode_event};

const NS_SHIFT: u32 = 24;
const NS_ASSERT: u32 = 1;
const NS_STATE: u32 = 2;

fn eid(ns: u32, local: u32) -> u32 {
    (ns << NS_SHIFT) | local
}

fn env(bytes: Vec<u8>) -> Environment {
    Environment {
        blob_version: 4,
        bytes,
    }
}

/// A run whose event stream is `events`, ending quiescent.
fn trace(events: Vec<(Moment, GuestEvent)>) -> RunTrace {
    RunTrace {
        terminal: StopReason::Quiescent { vtime: VTime(80) },
        env: env(vec![1, 2, 3]),
        coverage: None,
        events,
        records: vec![],
    }
}

/// A `state_max` mints novelty only on a per-register **increase** — a repeated
/// or *decreased* maximum is not new (round-5 P3, else a decrease mints false
/// novelty); and maxima are tracked per register independently.
#[test]
fn state_max_emits_only_on_per_register_increase() {
    let max = |v: u64| {
        let mut b = vec![1u8]; // STATE_MAX
        b.extend_from_slice(&v.to_le_bytes());
        b
    };
    let pack = |reg: u64, v: u64| ((reg & 0xFFFF) << 48) | (v & 0x0000_FFFF_FFFF_FFFF);
    let state_feat = |m: u64, reg: u64, v: u64| {
        (
            Moment(m),
            Feature {
                channel: LINK_STATE_CHANNEL,
                id: FeatureId(pack(reg, v)),
            },
        )
    };

    // Register 40: 5 → 10 → 3 → 10 → 12. Register 41: 7 → 7 (a repeat).
    let t = trace(vec![
        (Moment(10), decode_event(eid(NS_STATE, 40), &max(5))), // increase (first)
        (Moment(15), decode_event(eid(NS_STATE, 41), &max(7))), // increase (first, reg 41)
        (Moment(20), decode_event(eid(NS_STATE, 40), &max(10))), // increase
        (Moment(30), decode_event(eid(NS_STATE, 40), &max(3))), // DECREASE — no novelty
        (Moment(40), decode_event(eid(NS_STATE, 40), &max(10))), // repeat — no novelty
        (Moment(45), decode_event(eid(NS_STATE, 41), &max(7))), // repeat (reg 41) — no novelty
        (Moment(50), decode_event(eid(NS_STATE, 40), &max(12))), // increase
    ]);

    // Only the genuine increases mint, keyed per register.
    assert_eq!(
        LinkSensor::new().observe(&t),
        vec![
            state_feat(10, 40, 5),
            state_feat(15, 41, 7),
            state_feat(20, 40, 10),
            state_feat(50, 40, 12),
        ]
    );
}

/// The sensor emits a link-assert feature per hit and a link-state feature
/// encoding (reg, value).
#[test]
fn sensor_emits_assert_and_state_features() {
    let mut state = vec![1u8]; // STATE_MAX
    state.extend_from_slice(&7u64.to_le_bytes());
    let t = trace(vec![
        (Moment(40), decode_event(eid(NS_ASSERT, 5), &[0, 0, 0])),
        (Moment(50), decode_event(eid(NS_STATE, 40), &state)),
    ]);

    let feats = LinkSensor::new().observe(&t);
    let pack = ((40u64 & 0xFFFF) << 48) | (7u64 & 0x0000_FFFF_FFFF_FFFF);
    assert_eq!(
        feats,
        vec![
            (
                Moment(40),
                Feature {
                    channel: LINK_ASSERT_CHANNEL,
                    id: FeatureId(5)
                }
            ),
            (
                Moment(50),
                Feature {
                    channel: LINK_STATE_CHANNEL,
                    id: FeatureId(pack)
                }
            ),
        ]
    );
}

/// A violation event does NOT become a feature (only hits + state changes do).
#[test]
fn sensor_ignores_violations_and_buggify() {
    let t = trace(vec![
        (Moment(40), decode_event(eid(NS_ASSERT, 5), &[1, 0, 0])), // violation
        (Moment(50), decode_event(3 << NS_SHIFT | 9, &[1])),       // buggify
    ]);
    assert!(LinkSensor::new().observe(&t).is_empty());
}

/// GATE 2 — a sometimes hit is admitted as a checkpoint candidate by the spine
/// `Archive` on the toy: the link feature makes a fork at the hit's moment novel.
#[test]
fn sometimes_hit_is_admitted_as_a_checkpoint_candidate() {
    let t = trace(vec![(
        Moment(40),
        decode_event(eid(NS_ASSERT, 5), &[0, 0, 0]),
    )]);

    // A fork at the hit's moment, no coverage — only the sensor feature can make
    // it novel.
    let fork = Fork {
        exemplar: VirtualExemplar {
            parent: SnapId(1),
            seed: 0,
            suffix: env(vec![]),
            at: Moment(40),
        },
        env: env(vec![]),
        coverage: None,
    };

    let mut archive = CoverageArchive::new();
    let sensors: Vec<Box<dyn Sensor>> = vec![Box::new(LinkSensor::new())];
    let reward = archive.admit(&t, &[fork], &IdentityCells, &sensors);
    assert_eq!(
        reward.new_cells, 1,
        "the link feature claims one fresh cell"
    );
    assert_eq!(archive.frontier().len(), 1);

    // A fork at a DIFFERENT moment sees none of the hit's features.
    let elsewhere = Fork {
        exemplar: VirtualExemplar {
            parent: SnapId(1),
            seed: 0,
            suffix: env(vec![]),
            at: Moment(60),
        },
        env: env(vec![]),
        coverage: None,
    };
    let mut a2 = CoverageArchive::new();
    let r2 = a2.admit(&t, &[elsewhere], &IdentityCells, &sensors);
    assert_eq!(
        r2.new_cells, 0,
        "a fork at another moment is not made novel"
    );
}

/// GATE 2 — a planted always-violation makes `AlwaysViolation` mint a `Bug` with
/// a stable fingerprint, byte-identical to the explorer's own `Assertion`
/// fingerprint (cross-checked against `TerminalOracle`).
#[test]
fn always_violation_mints_a_bug_with_a_stable_fingerprint() {
    let mut t = trace(vec![]);
    t.terminal = StopReason::Assertion {
        vtime: VTime(80),
        id: 20,
        data: vec![7, 7],
    };
    t.env = env(vec![9, 9]);

    let bug = AlwaysViolation::new()
        .judge(&t)
        .expect("assertion is a bug");
    assert_eq!(bug.env, t.env);
    assert_eq!(bug.stop, t.terminal);

    // Fingerprint parity with the explorer's own oracle (so the two dedup).
    let explorer_bug = TerminalOracle::new()
        .judge(&t)
        .expect("crash/assert is a bug");
    assert_eq!(
        bug.fingerprint, explorer_bug.fingerprint,
        "link fingerprint matches the explorer's Assertion fingerprint"
    );

    // Stable across calls.
    let again = AlwaysViolation::new().judge(&t).unwrap();
    assert_eq!(bug.fingerprint, again.fingerprint);
    assert_ne!(bug.fingerprint, [0u8; 32]);
}

/// `AlwaysViolation` mints a `Bug` only on an `Assertion` terminal — never on a
/// crash or a clean stop (that is the terminal oracle's job).
#[test]
fn always_violation_is_assertion_specific() {
    let mut t = trace(vec![]);
    assert!(
        AlwaysViolation::new().judge(&t).is_none(),
        "quiescent is no bug"
    );

    t.terminal = StopReason::Crash {
        vtime: VTime(80),
        info: vec![1],
    };
    assert!(
        AlwaysViolation::new().judge(&t).is_none(),
        "a crash is the terminal oracle's, not the always-violation oracle's"
    );
}

/// The assertion fingerprint keys on the assertion **id** (its class), not the
/// per-run `data` payload — the shared task-75 v2 scheme: distinct ids split
/// (dedup keeps distinct bugs), while the same assertion with different data
/// dedups to one bug rather than fanning out a bug per reproducer.
#[test]
fn assertion_fingerprint_keys_on_id_not_data() {
    let base = |id: u32, data: Vec<u8>| {
        let mut t = trace(vec![]);
        t.terminal = StopReason::Assertion {
            vtime: VTime(80),
            id,
            data,
        };
        AlwaysViolation::new().judge(&t).unwrap().fingerprint
    };
    assert_ne!(base(1, vec![]), base(2, vec![]), "distinct ids split");
    assert_eq!(
        base(1, vec![1]),
        base(1, vec![2]),
        "one assertion dedups across its data payload"
    );
    let _ = ChannelId(0); // keep the import honest across refactors
}
