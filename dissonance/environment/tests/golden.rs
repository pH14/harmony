// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 3 — golden answers. A hand-frozen `Answer` sequence for one seed under a
//! known `FaultPolicy`, spanning every `DecisionClass`, pins the PRNG and the
//! sampling against silent drift. The expected column is the `Answer::encode`
//! hex of each decision; regenerate (and review) with `GOLDEN_CAPTURE=1`.

use environment::{
    Answer, BlockOp, ConnId, DecisionClass, DecisionPoint as P, Environment, Fault, FaultPolicy,
    NodeId, Outcome, SeededEnv, VTime,
};

const SEED: u64 = 0x0123_4567_89AB_CDEF;

/// A policy that faults often, so every fault class shows concrete faults.
fn policy() -> FaultPolicy {
    let mut p = FaultPolicy::none();
    p.set_class(
        DecisionClass::NetSend,
        3,
        4,
        &[
            Fault::NetDrop,
            Fault::NetDelay(VTime(100)),
            Fault::NetReorder,
            Fault::NetDup,
            Fault::NetCorrupt(environment::CorruptSpec {
                offset: 2,
                xor: 0xFF,
            }),
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
    p
}

/// The decision sequence — at least one of every class, fault classes repeated
/// so the sampling distribution is exercised.
fn sequence() -> Vec<P> {
    let net = |c: u64| P::NetSend {
        src: NodeId(0),
        dst: NodeId(1),
        conn: ConnId(c),
        len: 64,
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
    "0203",                       // NetSend       → Fault(NetDup)
    "00",                         // NetSend       → Nominal
    "00",                         // NetSend       → Nominal
    "0200",                       // NetSend       → Fault(NetDrop)
    "020708000000",               // BlockIo Read  → Fault(BlockTorn(8))
    "00",                         // BlockIo Write → Nominal
    "00",                         // BlockIo Flush → Nominal
    "00",                         // BlockIo Read  → Nominal
    "020a",                       // Process       → Fault(ProcKill)
    "02090a00000000000000",       // Process       → Fault(ProcPause(10))
    "020a",                       // Process       → Fault(ProcKill)
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
