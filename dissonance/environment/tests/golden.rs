// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — golden answers and host-plane wire format. A hand-frozen `Answer`
//! sequence for one seed under a known `FaultPolicy`, spanning every
//! `DecisionClass`, pins the PRNG and the sampling against silent drift; a
//! frozen `HostFault`/`Action`/`EnvSpec` byte layout pins the host-plane wire
//! format. Regenerate (and review) with `GOLDEN_CAPTURE=1`.

use std::collections::BTreeMap;

use environment::{
    Action, Answer, BitMask, BlockOp, ConnId, DecisionClass, DecisionPoint as P, EnvSpec,
    Environment, Fault, FaultPolicy, HostFault, NodeId, Outcome, Ratio, SeededEnv, VTime,
};

const SEED: u64 = 0x0123_4567_89AB_CDEF;

/// A policy that faults often, so every fault class shows concrete faults.
fn policy() -> FaultPolicy {
    let mut p = FaultPolicy::none();
    p.set_class(
        DecisionClass::NetFlow,
        3,
        4,
        &[
            Fault::NetLatency(VTime(100)),
            Fault::NetLoss { num: 1, den: 2 },
            Fault::NetThrottle { bps: 1_000_000 },
            Fault::NetReset,
        ],
    )
    .unwrap();
    p.set_class(
        DecisionClass::BlockIo,
        1,
        2,
        &[
            Fault::BlockEio,
            Fault::BlockLatency(VTime(50)),
            Fault::BlockTorn(8),
            Fault::BlockNospc,
        ],
    )
    .unwrap();
    p.set_class(
        DecisionClass::Process,
        2,
        3,
        &[
            Fault::ProcPause(VTime(10)),
            Fault::ProcKill,
            Fault::ProcRestart,
        ],
    )
    .unwrap();
    // A buggify point that always fires (per-point, not per-class), so the golden
    // pins the `Fault::BuggifyFire` wire tag (16) byte-exactly — a round-trip test
    // would not catch a tag renumbering.
    p.set_buggify_point(99, 1, 1).unwrap();
    p
}

/// The decision sequence — at least one of every class, fault classes repeated
/// so the sampling distribution is exercised.
fn sequence() -> Vec<P> {
    let net = |c: u64| P::NetFlow {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(c),
        event: environment::FlowEvent::Open,
    };
    let io = |op, lba| P::BlockIo { op, lba, len: 4096 };
    vec![
        P::Entropy { bytes: 8 },
        P::Entropy { bytes: 16 },
        P::Payload { bytes: 4 },
        P::Payload { bytes: 0 },
        P::Scheduler { ready: 5 },
        P::Scheduler { ready: 1 },
        net(10),
        net(11),
        net(12),
        net(13),
        io(BlockOp::Read, 0),
        io(BlockOp::Write, 8),
        io(BlockOp::Flush, 0),
        io(BlockOp::Read, 16),
        P::Process { node: NodeId(2) },
        P::Process { node: NodeId(3) },
        P::Process { node: NodeId(4) },
        // A buggify decision — the always-firing point declared in `policy()`, so
        // the golden covers `Fault::BuggifyFire` (the one class the sequence missed).
        P::Buggify { point: 99 },
    ]
}

fn to_hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

fn answers() -> Vec<String> {
    let mut env = SeededEnv::new(SEED, policy());
    sequence()
        .iter()
        .map(|p| match env.decide(p) {
            Outcome::Resolved(a) => to_hex(&a.encode()),
            Outcome::NeedsHost => unreachable!("seeded backing never suspends"),
        })
        .collect()
}

/// Frozen expectations — `Answer::encode` hex per decision, captured once and
/// reviewed. Regenerate with `GOLDEN_CAPTURE=1 cargo test -p environment --test golden`.
const EXPECTED: &[&str] = &[
    "01080000008c70b62c4782947c", // Entropy{8}    → Supply(8)
    "0110000000de281fbf925670d5c005e0a53b1eb788", // Entropy{16}   → Supply(16)
    "0104000000e6cbc0f0",         // Payload{4}    → Supply(4)
    "0100000000",                 // Payload{0}    → Supply(0)
    "010400000004000000",         // Scheduler{5}  → Supply(idx=4)
    "010400000000000000",         // Scheduler{1}  → Supply(idx=0)
    "020d01000200",               // NetFlow       → Fault(NetLoss{num:1,den:2}) (tag 13)
    "00",                         // NetFlow       → Nominal
    "00",                         // NetFlow       → Nominal
    "020c6400000000000000",       // NetFlow       → Fault(NetLatency(100)) (tag 12)
    "020708000000",               // BlockIo Read  → Fault(BlockTorn(8))
    "00",                         // BlockIo Write → Nominal
    "00",                         // BlockIo Flush → Nominal
    "00",                         // BlockIo Read  → Nominal
    "020a",                       // Process       → Fault(ProcKill)
    "02090a00000000000000",       // Process       → Fault(ProcPause(10))
    "020a",                       // Process       → Fault(ProcKill)
    "0210",                       // Buggify       → Fault(BuggifyFire) (tag 16)
];

#[test]
fn golden_answer_sequence() {
    let got = answers();
    if std::env::var_os("GOLDEN_CAPTURE").is_some() {
        eprintln!("const EXPECTED: &[&str] = &[");
        for h in &got {
            eprintln!("    \"{h}\",");
        }
        eprintln!("];");
        return;
    }
    let expected: Vec<String> = EXPECTED.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(
        got, expected,
        "golden answer sequence drifted. If the PRNG/sampling change is intentional and reviewed, \
         regenerate with: GOLDEN_CAPTURE=1 cargo test -p environment --test golden"
    );
}

/// Sanity: the sequence really does cover every class, and the answers decode
/// back to the expected shapes (supplies on supply classes, nominal-or-fault on
/// fault classes) — so the golden is not pinning a degenerate all-nominal run.
#[test]
fn golden_covers_every_class_with_faults() {
    let mut env = SeededEnv::new(SEED, policy());
    let seq = sequence();
    let mut saw_fault = false;
    let mut saw_supply = false;
    for p in &seq {
        let Outcome::Resolved(a) = env.decide(p) else {
            unreachable!()
        };
        match (p.class().is_supply(), &a) {
            (true, Answer::Supply(_)) => saw_supply = true,
            (true, _) => panic!("supply class produced a non-supply answer"),
            (false, Answer::Fault(f)) => {
                assert_eq!(f.class(), p.class(), "fault belongs to the point's class");
                saw_fault = true;
            }
            (false, Answer::Nominal) => {}
            (false, Answer::Supply(_)) => panic!("fault class produced a supply answer"),
        }
    }
    assert!(
        saw_supply && saw_fault,
        "golden exercises supplies and faults"
    );
}

// ---- host-plane wire format -----------------------------------------------

/// One host fault of every variant, with their frozen `HostFault::encode` hex.
/// These tag/field layouts are a stable contract a recorded reproducer's replay
/// (and the `perturb` transport) depends on. Regenerate with `GOLDEN_CAPTURE=1`.
fn host_faults() -> Vec<(HostFault, &'static str)> {
    vec![
        // tag 00 + VTime u64 (0x0102030405060708, little-endian).
        (
            HostFault::SkewTime(VTime(0x0102_0304_0506_0708)),
            "000807060504030201",
        ),
        // tag 01 + num u64 (3) + den u64 (2).
        (
            HostFault::SetClockRate(Ratio::new(3, 2).unwrap()),
            "0103000000000000000200000000000000",
        ),
        // tag 02 + gpa u64 (0x4000) + mask u64 (0b1000 = 8).
        (
            HostFault::CorruptMemory {
                gpa: 0x4000,
                mask: BitMask(0b1000),
            },
            "0200400000000000000800000000000000",
        ),
        // tag 03 + vector u8 (0x80).
        (HostFault::InjectInterrupt { vector: 0x80 }, "0380"),
    ]
}

#[test]
fn golden_host_fault_wire_format() {
    let capture = std::env::var_os("GOLDEN_CAPTURE").is_some();
    for (f, expected) in host_faults() {
        let got = to_hex(&f.encode());
        if capture {
            eprintln!("{f:?} => {got}");
            continue;
        }
        assert_eq!(
            got, expected,
            "HostFault wire format drifted for {f:?}. If intentional and reviewed, \
             regenerate with GOLDEN_CAPTURE=1."
        );
        // Round-trips.
        assert_eq!(HostFault::decode(&f.encode()).unwrap(), f);
    }
}

#[test]
fn golden_action_wire_format() {
    // Action = one plane-tag byte (00 host / 01 guest) then the plane's encoding.
    let host = Action::Host(HostFault::InjectInterrupt { vector: 0x80 });
    assert_eq!(
        to_hex(&host.encode()),
        "000380",
        "host plane tag 00 + payload"
    );

    let guest = Action::Guest(Answer::Fault(Fault::NetReset));
    // 01 (guest) + 02 (Answer::Fault) + 0f (Fault::NetReset, tag 15).
    assert_eq!(
        to_hex(&guest.encode()),
        "01020f",
        "guest plane tag 01 + payload"
    );
}

#[test]
fn golden_recorded_blob_with_host_overrides() {
    // A small mixed reproducer, frozen, so the whole `EnvSpec` layout (magic +
    // version + Moment-keyed Action map + reseed-marker table) is pinned
    // against silent drift.
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([
            (1, Action::Host(HostFault::InjectInterrupt { vector: 0x80 })),
            (2, Action::Guest(Answer::Nominal)),
        ]),
        standing: vec![],
        reseeds: BTreeMap::from([(3, 0xD1CE)]),
    };
    let hex = to_hex(&spec.encode());
    if std::env::var_os("GOLDEN_CAPTURE").is_some() {
        eprintln!("recorded blob => {hex}");
    } else {
        assert_eq!(
            hex,
            // "DEV2"(44455632) + version(0400) + variant(01) + seed(00 x8) +
            // length-prefixed policy(FPL1 magic + version 0300, baseline, len 0x36=54:
            //   three empty classes 0x2a=42 + trailing buggify section
            //   [default_num 0, default_den 1, per_point count 0] = 12, task 73) +
            // overrides count(02000000) +
            //   Moment 1 + len-prefixed Action::Host(InjectInterrupt 0x80) = [00 03 80] +
            //   Moment 2 + len-prefixed Action::Guest(Nominal) = [01 00] +
            // standing count(00000000) +
            // reseed count(01000000) + Moment 3 + seed 0xD1CE (both u64 LE, task 78).
            "4445563204000100000000000000003600000046504c31030000000000010000000000000000000000010000000000000000000000010000000000000000000000010000000000000002000000010000000000000003000000000380020000000000000002000000010000000000010000000300000000000000ced1000000000000",
            "recorded blob wire format drifted; regenerate with GOLDEN_CAPTURE=1"
        );
        assert_eq!(EnvSpec::decode(&spec.encode()).unwrap(), spec);
    }
}
