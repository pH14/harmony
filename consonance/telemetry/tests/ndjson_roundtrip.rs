// SPDX-License-Identifier: AGPL-3.0-or-later
//! Property test: the NDJSON wire is a lossless codec.
//!
//! For every `Event` — across every `EventKind` variant, including the additive
//! `Dropped` notice and the `[u8; 32]` checkpoint hash — `from_ndjson(to_ndjson(ev))`
//! reproduces it exactly, and the encoding is a single line (NDJSON framing). This
//! is the lossless-recording guarantee the replay path depends on.

use proptest::prelude::*;
use telemetry::{Event, EventKind, ExitCounts, from_ndjson, to_ndjson};

fn exit_counts() -> impl Strategy<Value = ExitCounts> {
    proptest::collection::vec(any::<u64>(), 13).prop_map(|v| ExitCounts {
        io: v[0],
        mmio: v[1],
        rdmsr: v[2],
        wrmsr: v[3],
        hypercall: v[4],
        cpuid: v[5],
        rdtsc: v[6],
        rdtscp: v[7],
        rdrand: v[8],
        rdseed: v[9],
        hlt: v[10],
        shutdown: v[11],
        deadline: v[12],
    })
}

fn event_kind() -> impl Strategy<Value = EventKind> {
    prop_oneof![
        // `any::<String>()` exercises arbitrary UTF-8 incl. control chars and
        // non-ASCII — JSON must escape and recover them byte-for-byte.
        any::<String>().prop_map(|text| EventKind::Console { text }),
        (any::<u32>(), proptest::collection::vec(any::<u8>(), 0..48))
            .prop_map(|(id, data)| EventKind::GuestEvent { id, data }),
        (any::<u16>(), any::<u8>(), any::<u64>(), any::<bool>()).prop_map(
            |(port, size, value, write)| EventKind::Io {
                port,
                size,
                value,
                write
            }
        ),
        (any::<u64>(), any::<u8>(), any::<u64>(), any::<bool>()).prop_map(
            |(addr, size, value, write)| EventKind::Mmio {
                addr,
                size,
                value,
                write
            }
        ),
        (any::<u8>(), any::<u16>(), any::<u16>()).prop_map(|(service, opcode, status)| {
            EventKind::Hypercall {
                service,
                opcode,
                status,
            }
        }),
        (any::<u32>(), any::<u64>(), any::<bool>()).prop_map(|(index, value, write)| {
            EventKind::Msr {
                index,
                value,
                write,
            }
        }),
        any::<u64>().prop_map(|value| EventKind::Tsc { value }),
        any::<u64>().prop_map(|value| EventKind::Rng { value }),
        (any::<u32>(), any::<u32>()).prop_map(|(leaf, subleaf)| EventKind::Cpuid { leaf, subleaf }),
        any::<u8>().prop_map(|vector| EventKind::Inject { vector }),
        proptest::array::uniform32(any::<u8>())
            .prop_map(|state_hash| EventKind::Checkpoint { state_hash }),
        exit_counts().prop_map(EventKind::Counts),
        any::<String>().prop_map(|reason| EventKind::Terminal { reason }),
        any::<u64>().prop_map(|count| EventKind::Dropped { count }),
    ]
}

fn event() -> impl Strategy<Value = Event> {
    (any::<u64>(), any::<u64>(), any::<u64>(), event_kind())
        .prop_map(|(seq, work, vns, kind)| Event::new(seq, work, vns, kind))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn ndjson_roundtrips(ev in event()) {
        let line = to_ndjson(&ev).expect("encode");
        prop_assert!(!line.contains('\n'), "NDJSON line must be single-line");
        let back = from_ndjson(&line).expect("decode");
        prop_assert_eq!(back, ev);
    }

    /// A whole stream of events round-trips line-by-line (the recorder/replay
    /// shape: one JSON object per line, decoded independently).
    #[test]
    fn ndjson_stream_roundtrips(evs in proptest::collection::vec(event(), 0..64)) {
        let mut buf = String::new();
        for ev in &evs {
            buf.push_str(&to_ndjson(ev).expect("encode"));
            buf.push('\n');
        }
        let decoded: Vec<Event> = buf
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| from_ndjson(l).expect("decode"))
            .collect();
        prop_assert_eq!(decoded, evs);
    }
}
