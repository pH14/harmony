// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — golden answers and host-plane wire format. A hand-frozen `Answer`
//! sequence for one seed under a known `FaultPolicy`, spanning every
//! `DecisionClass`, pins the PRNG and the sampling against silent drift; a
//! frozen `HostFault`/`Action`/`EnvSpec` byte layout pins the host-plane wire
//! format. Regenerate (and review) with `GOLDEN_CAPTURE=1`.

use std::collections::BTreeMap;

use fault_policy::{
    Action, Answer, BitMask, BlockOp, ConnId, DecisionClass, DecisionPoint as P, EnvSpec,
    Environment, Fault, FaultPolicy, HostFault, NodeId, Outcome, Ratio, SeededEnv, Span,
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
            Fault::NetLatency(Span(100)),
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
            Fault::BlockLatency(Span(50)),
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
            Fault::ProcPause(Span(10)),
            Fault::ProcKill,
            Fault::ProcRestart,
        ],
    )
    .unwrap();
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
        event: fault_policy::FlowEvent::Open,
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
    "01080000008c70b62c4782947c",
    "0110000000de281fbf925670d5c005e0a53b1eb788",
    "0104000000e6cbc0f0",
    "0100000000",
    "010400000004000000",
    "010400000000000000",
    "020d01000200",
    "00",
    "00",
    "020c6400000000000000",
    "020708000000",
    "00",
    "00",
    "00",
    "020a",
    "02090a00000000000000",
    "020a",
    "0210",
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

/// One host fault of every variant, with their frozen `HostFault::encode` hex.
/// These tag/field layouts are a stable contract a recorded reproducer's replay
/// (and the `perturb` transport) depends on. Regenerate with `GOLDEN_CAPTURE=1`.
fn host_faults() -> Vec<(HostFault, &'static str)> {
    vec![
        (
            HostFault::SkewTime(Span(0x0102_0304_0506_0708)),
            "000807060504030201",
        ),
        (
            HostFault::SetClockRate(Ratio::new(3, 2).unwrap()),
            "0103000000000000000200000000000000",
        ),
        (
            HostFault::CorruptMemory {
                gpa: 0x4000,
                mask: BitMask(0b1000),
            },
            "0200400000000000000800000000000000",
        ),
        (HostFault::InjectInterrupt { vector: 0x80 }, "0380000000"),
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
        assert_eq!(HostFault::decode(&f.encode()).unwrap(), f);
    }
}

/// The process faults the in-guest fault agent enforces, with their frozen
/// `Answer::encode` hex. A round-trip test cannot catch a tag renumbering, and
/// the agent decodes these bytes from a separately built binary.
#[test]
fn golden_process_fault_wire_format() {
    let capture = std::env::var_os("GOLDEN_CAPTURE").is_some();
    for (fault, expected) in [
        (Fault::RunHook(7), "021107000000"),
        (
            Fault::ProcPark {
                addr: 0x4b_0e86,
                hits: 28,
                hold: Span(2_000_000),
            },
            "0213860e4b00000000001c00000080841e0000000000",
        ),
    ] {
        let got = to_hex(&Answer::Fault(fault).encode());
        if capture {
            eprintln!("{fault:?} => {got}");
            continue;
        }
        assert_eq!(
            got, expected,
            "process fault wire format drifted for {fault:?}. If intentional and reviewed, \
             regenerate with GOLDEN_CAPTURE=1."
        );
    }
}

/// Byte tag 18 sits between the two and is permanently unassigned, so a blob
/// that names it is refused rather than reinterpreted.
#[test]
fn the_unassigned_process_fault_tag_is_refused() {
    assert!(Answer::decode(&[0x02, 18]).is_err());
    assert!(Answer::decode(&[0x02, 18, 0, 0, 0, 0]).is_err());
}

#[test]
fn golden_action_wire_format() {
    let host = Action::Host(HostFault::InjectInterrupt { vector: 0x80 });
    assert_eq!(
        to_hex(&host.encode()),
        "000380000000",
        "host plane tag 00 + payload"
    );

    let guest = Action::Guest(Answer::Fault(Fault::NetReset));
    assert_eq!(
        to_hex(&guest.encode()),
        "01020f",
        "guest plane tag 01 + payload"
    );
}

#[test]
fn golden_recorded_blob_with_host_overrides() {
    let spec = EnvSpec::Recorded {
        seed: 0,
        policy: FaultPolicy::none(),
        overrides: BTreeMap::from([
            (1, Action::Host(HostFault::InjectInterrupt { vector: 0x80 })),
            (2, Action::Guest(Answer::Nominal)),
        ]),
        standing: vec![],
        reseeds: BTreeMap::from([(3, 0xD1CE)]),
        payloads: None,
    };
    let hex = to_hex(&spec.encode());
    if std::env::var_os("GOLDEN_CAPTURE").is_some() {
        eprintln!("recorded blob => {hex}");
    } else {
        assert_eq!(
            hex,
            "4445563207000100000000000000003600000046504c31030000000000010000000000000000000000010000000000000000000000010000000000000000000000010000000000000002000000010000000000000006000000000380000000020000000000000002000000010000000000010000000300000000000000ced100000000000000",
            "recorded blob wire format drifted; regenerate with GOLDEN_CAPTURE=1"
        );
        assert_eq!(EnvSpec::decode(&spec.encode()).unwrap(), spec);
    }
}
