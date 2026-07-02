// SPDX-License-Identifier: AGPL-3.0-or-later
//! The scrape-tier decoder: console bytes → timestamped [`Record`]s.
//!
//! A run's console arrives as an ordered sequence of `(Moment, bytes)` chunks
//! (drained from `Vmm::serial()` at each stop, or ingested from a telemetry
//! `Console` recording — [`crate::ingest_ndjson`]). This decoder splits that
//! byte stream into newline-delimited [`Record`]s. It is deliberately **not** in
//! the conductor (it must run offline over recorded chunk streams) and **not**
//! in a plugin (task 67's codebook *consumes* records, never produces them):
//! the concrete `Record` decode lives here.
//!
//! It is **total and lossless** (task 65 §2, gate 2):
//!
//! - *Total.* Never panics on torn or non-UTF-8 input; UTF-8-lossy decoding is a
//!   display concern applied nowhere here — the bytes are kept verbatim.
//! - *Lossless.* Every input byte lands in exactly one record. A record's
//!   `line` retains its terminating `\n`, so the concatenation of all records'
//!   `line`s equals the concatenation of all chunk bytes. A trailing
//!   unterminated line is emitted (with no terminator) at the terminal stamp.
//! - *Stamp.* A terminated line is stamped with the [`Moment`] of the chunk that
//!   contained its `\n` ("the chunk that completed the line"). The trailing line
//!   is stamped with the terminal chunk's `Moment`. Re-chunking that preserves
//!   each byte's arrival `Moment` never changes the decoded records, and feeding
//!   chunks incrementally equals decoding them in one batch.
//!
//! Stamps are **stop-granular in v1**: the conductor drains the whole console
//! produced by a run under a single stop `Moment`, so all of a run's records
//! share it. Per-exit stamps wait on the `telemetry::Observer` wiring (task 65
//! non-goal); nothing here changes when they arrive — only the chunk stream gets
//! finer-grained `Moment`s.

use explorer::{Moment, Record, StreamId};

/// A streaming console→record decoder. Push chunks in arrival order; each
/// [`push`](ChunkDecoder::push) returns the records completed by that chunk, and
/// [`finish`](ChunkDecoder::finish) emits any trailing unterminated line. Held
/// state is just the in-progress line and the last chunk's `Moment`.
///
/// [`decode_chunks`] is the batch convenience built on this; the two agree by
/// construction (gate 2's incremental ≡ batch property).
#[derive(Clone, Debug)]
pub struct ChunkDecoder {
    stream: StreamId,
    /// Bytes of the line currently being assembled (no `\n` seen yet).
    line: Vec<u8>,
    /// The `Moment` of the most recent chunk — the terminal stamp for a
    /// trailing unterminated line.
    last: Option<Moment>,
}

impl ChunkDecoder {
    /// A decoder that stamps every record with `stream`.
    pub fn new(stream: StreamId) -> Self {
        ChunkDecoder {
            stream,
            line: Vec::new(),
            last: None,
        }
    }

    /// Feed one chunk that arrived at `at`. Returns the records this chunk
    /// completed (each terminated by a `\n` within `bytes`, or straddling a
    /// prior chunk); an in-progress line carries over to the next call.
    pub fn push(&mut self, at: Moment, bytes: &[u8]) -> Vec<(Moment, Record)> {
        self.last = Some(at);
        let mut out = Vec::new();
        for &b in bytes {
            self.line.push(b);
            if b == b'\n' {
                out.push((
                    at,
                    Record {
                        stream: self.stream,
                        line: std::mem::take(&mut self.line),
                    },
                ));
            }
        }
        out
    }

    /// Emit the trailing unterminated line, if any, stamped at the terminal
    /// (last chunk's) `Moment`. `None` when the stream ended on a `\n` or was
    /// empty. Consumes the decoder — there is nothing more to push.
    pub fn finish(self) -> Option<(Moment, Record)> {
        if self.line.is_empty() {
            return None;
        }
        // `line` is non-empty ⇒ some chunk delivered bytes ⇒ `last` is `Some`.
        let at = self.last.unwrap_or_default();
        Some((
            at,
            Record {
                stream: self.stream,
                line: self.line,
            },
        ))
    }
}

/// Decode a whole recorded chunk stream into timestamped [`Record`]s (task 65
/// §2). `stream` labels every record; `chunks` are `(arrival Moment, bytes)` in
/// order. See the module doc for the totality/losslessness/stamp guarantees.
pub fn decode_chunks(stream: StreamId, chunks: &[(Moment, Vec<u8>)]) -> Vec<(Moment, Record)> {
    let mut d = ChunkDecoder::new(stream);
    let mut out = Vec::new();
    for (at, bytes) in chunks {
        out.extend(d.push(*at, bytes));
    }
    out.extend(d.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_lines_keeps_terminators_and_stamps_the_completing_chunk() {
        let recs = decode_chunks(
            StreamId(1),
            &[
                (Moment(1), b"a\nbc".to_vec()),
                (Moment(2), b"d\ne".to_vec()),
            ],
        );
        let got: Vec<(u64, Vec<u8>)> = recs.iter().map(|(m, r)| (m.0, r.line.clone())).collect();
        assert_eq!(
            got,
            vec![
                (1, b"a\n".to_vec()),   // completed within chunk 1
                (2, b"bcd\n".to_vec()), // straddles; completed by chunk 2
                (2, b"e".to_vec()),     // trailing unterminated, terminal stamp
            ]
        );
    }

    #[test]
    fn finish_yields_nothing_when_the_stream_ends_on_a_newline() {
        let mut d = ChunkDecoder::new(StreamId(0));
        let completed = d.push(Moment(9), b"line\n");
        assert_eq!(completed.len(), 1);
        assert!(d.finish().is_none(), "no trailing line after a terminator");
    }
}
