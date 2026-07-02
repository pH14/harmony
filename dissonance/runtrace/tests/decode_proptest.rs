// SPDX-License-Identifier: AGPL-3.0-or-later
//! Gate 2 — the scrape decoder is **total** and **lossless** over arbitrary,
//! torn, and non-UTF-8 console streams, and its stamps are re-chunking-stable.
//!
//! Properties (≥256 cases each):
//! - *Totality* — arbitrary bytes never panic (the harness running to green is
//!   the proof; a panic aborts the case).
//! - *Losslessness* — the concatenation of every record's `line` equals the
//!   concatenation of every input chunk's bytes: each byte lands in exactly one
//!   record.
//! - *Re-chunking within identical stamps* — splitting each chunk into
//!   same-`Moment` sub-chunks never changes the decoded records.
//! - *Incremental ≡ batch* — feeding the stream byte-by-byte (same `Moment`)
//!   equals decoding it in one shot.

use explorer::{Moment, StreamId};
use proptest::collection::vec;
use proptest::prelude::*;
use runtrace::{ChunkDecoder, decode_chunks};

const STREAM: StreamId = StreamId(7);

/// Arbitrary chunk stream: each chunk a `(Moment, bytes)` where bytes range over
/// the full byte space (so `\n`, `\r`, and non-UTF-8 all appear), and moments
/// are small so collisions (identical stamps) are common.
fn chunks() -> impl Strategy<Value = Vec<(Moment, Vec<u8>)>> {
    vec((0u64..8, vec(any::<u8>(), 0..24)), 0..24)
        .prop_map(|v| v.into_iter().map(|(m, bytes)| (Moment(m), bytes)).collect())
}

fn concat_chunk_bytes(chunks: &[(Moment, Vec<u8>)]) -> Vec<u8> {
    chunks.iter().flat_map(|(_, b)| b.iter().copied()).collect()
}

fn concat_record_lines(records: &[(Moment, explorer::Record)]) -> Vec<u8> {
    records
        .iter()
        .flat_map(|(_, r)| r.line.iter().copied())
        .collect()
}

/// Split each chunk into same-`Moment` sub-chunks at arbitrary interior points —
/// a re-chunking that preserves every byte's arrival `Moment` and the terminal
/// `Moment`, so the decode must be invariant to it.
fn resplit(chunks: &[(Moment, Vec<u8>)], seed: u64) -> Vec<(Moment, Vec<u8>)> {
    let mut rng = seed;
    let mut next = || {
        // xorshift64* — deterministic, no host RNG (conventions rule 4).
        rng ^= rng >> 12;
        rng ^= rng << 25;
        rng ^= rng >> 27;
        rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let mut out = Vec::new();
    for (m, bytes) in chunks {
        if bytes.is_empty() {
            out.push((*m, Vec::new()));
            continue;
        }
        let mut i = 0;
        while i < bytes.len() {
            // A 1..=len step so sub-chunks are non-empty and the whole chunk is
            // covered; same Moment throughout.
            let step = 1 + (next() as usize % bytes.len());
            let end = (i + step).min(bytes.len());
            out.push((*m, bytes[i..end].to_vec()));
            i = end;
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Losslessness: every input byte appears in exactly one record's `line`,
    /// in order. (Totality is implicit — a panic fails the case.)
    #[test]
    fn bytes_partition_exactly_into_records(chunks in chunks()) {
        let records = decode_chunks(STREAM, &chunks);
        prop_assert_eq!(concat_record_lines(&records), concat_chunk_bytes(&chunks));
        // Every record carries the stream and a non-empty line (a record is one
        // non-empty line — empty input yields no records).
        for (_, r) in &records {
            prop_assert_eq!(r.stream, STREAM);
            prop_assert!(!r.line.is_empty());
        }
    }

    /// A terminated line ends in `\n`; only the final record may lack one (the
    /// trailing unterminated line).
    #[test]
    fn only_the_last_record_may_be_unterminated(chunks in chunks()) {
        let records = decode_chunks(STREAM, &chunks);
        for (i, (_, r)) in records.iter().enumerate() {
            let terminated = r.line.last() == Some(&b'\n');
            if i + 1 < records.len() {
                prop_assert!(terminated, "interior record {i} not newline-terminated");
            }
        }
    }

    /// Re-chunking within identical stamps changes nothing.
    #[test]
    fn resplitting_preserves_records(chunks in chunks(), seed in any::<u64>()) {
        let base = decode_chunks(STREAM, &chunks);
        let split = decode_chunks(STREAM, &resplit(&chunks, seed | 1));
        prop_assert_eq!(base, split);
    }

    /// Incremental (byte-by-byte, same Moment) decoding equals the batch decode.
    #[test]
    fn incremental_equals_batch(chunks in chunks()) {
        let batch = decode_chunks(STREAM, &chunks);

        let mut d = ChunkDecoder::new(STREAM);
        let mut incremental = Vec::new();
        for (m, bytes) in &chunks {
            if bytes.is_empty() {
                incremental.extend(d.push(*m, &[]));
            }
            for &b in bytes {
                incremental.extend(d.push(*m, &[b]));
            }
        }
        incremental.extend(d.finish());
        prop_assert_eq!(batch, incremental);
    }

    /// Stamps are monotone non-decreasing whenever the input moments are — the
    /// property the box gate asserts (each record inherits its completing/
    /// terminal chunk's Moment).
    #[test]
    fn stamps_track_monotone_input(bytes in vec(any::<u8>(), 0..64)) {
        // A single ascending sequence: split the bytes into three chunks at
        // moments 10, 20, 30 and confirm record stamps never decrease.
        let n = bytes.len();
        let (a, b) = bytes.split_at(n / 3);
        let (b, c) = b.split_at(b.len() / 2);
        let chunks = vec![
            (Moment(10), a.to_vec()),
            (Moment(20), b.to_vec()),
            (Moment(30), c.to_vec()),
        ];
        let records = decode_chunks(STREAM, &chunks);
        let mut prev = Moment(0);
        for (at, _) in &records {
            prop_assert!(*at >= prev, "stamp went backwards: {:?} < {:?}", at, prev);
            prev = *at;
        }
    }
}

// --- Targeted edge cases the random generator hits only rarely -----------------

#[test]
fn torn_line_across_chunks_is_stamped_at_its_completing_chunk() {
    // "hel" at Moment 1, "lo\nrest" at Moment 2: the completed line "hello\n" is
    // stamped 2 (the chunk with the \n), the trailing "rest" at the terminal 2.
    let chunks = vec![
        (Moment(1), b"hel".to_vec()),
        (Moment(2), b"lo\nrest".to_vec()),
    ];
    let records = decode_chunks(STREAM, &chunks);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].0, Moment(2));
    assert_eq!(records[0].1.line, b"hello\n");
    assert_eq!(records[1].0, Moment(2));
    assert_eq!(records[1].1.line, b"rest");
}

#[test]
fn non_utf8_bytes_survive_verbatim() {
    let chunks = vec![(Moment(5), vec![0xff, 0xfe, b'\n', 0x00, 0x80])];
    let records = decode_chunks(STREAM, &chunks);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].1.line, vec![0xff, 0xfe, b'\n']);
    assert_eq!(records[1].1.line, vec![0x00, 0x80]);
}

#[test]
fn empty_and_newline_only_streams() {
    assert!(decode_chunks(STREAM, &[]).is_empty());
    assert!(decode_chunks(STREAM, &[(Moment(0), Vec::new())]).is_empty());
    // A lone "\n" is one empty-content-but-terminated line.
    let recs = decode_chunks(STREAM, &[(Moment(3), b"\n".to_vec())]);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].1.line, b"\n");
}

#[test]
fn trailing_line_uses_the_terminal_stamp_even_after_an_empty_final_chunk() {
    // "abc" at Moment 1, then an empty chunk at Moment 9: the unterminated "abc"
    // is emitted at the terminal stamp 9.
    let chunks = vec![(Moment(1), b"abc".to_vec()), (Moment(9), Vec::new())];
    let records = decode_chunks(STREAM, &chunks);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].0, Moment(9));
    assert_eq!(records[0].1.line, b"abc");
}
