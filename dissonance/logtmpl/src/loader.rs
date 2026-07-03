// SPDX-License-Identifier: AGPL-3.0-or-later
//! The thin fixture loader — **test scaffolding, not a decoder** (raw console →
//! `Record` recording is task 65's job; this crate consumes recorded records).
//!
//! It turns captured console text into the scrape-tier record stream the sensor
//! consumes. Task 65 pins a `Record` as one newline-delimited line of a
//! [`StreamId`]'s byte stream, kept **verbatim** (terminator retained) — so the
//! loader stamps each line under a single synthetic console stream at a
//! synthetic [`Moment`] equal to its zero-based line index. That one-for-one
//! line-index→moment mapping is what the fixtures' gates key on.

use explorer::{Moment, Record, StreamId};

/// The synthetic console byte stream the loader stamps every line under.
const CONSOLE_STREAM: StreamId = StreamId(0);

/// Decode console text into a scrape-tier record stream: one record per line, at
/// `Moment(line_index)`, each line's bytes kept **verbatim** — matching task 65's
/// lossless-partition contract. `split_inclusive('\n')` keeps each terminator
/// attached and, crucially, alters nothing: an unterminated final line gains no
/// spurious `\n`, and a `\r\n` (or a payload `\r`) survives intact — unlike
/// `lines()`, which strips terminators (so re-adding `\n` would rewrite the
/// bytes). Total over any `&str`.
pub fn load_console_log(text: &str) -> Vec<(Moment, Record)> {
    text.split_inclusive('\n')
        .enumerate()
        .map(|(i, line)| {
            (
                Moment(i as u64),
                Record {
                    stream: CONSOLE_STREAM,
                    line: line.as_bytes().to_vec(),
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_record_per_line_at_its_index() {
        let recs = load_console_log("first line\nsecond 2\n");
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0].0, Moment(0));
        assert_eq!(recs[1].0, Moment(1));
        assert_eq!(recs[0].1.stream, CONSOLE_STREAM);
        // The line bytes are kept verbatim, terminator included.
        assert_eq!(recs[0].1.line, b"first line\n");
        assert_eq!(recs[1].1.line, b"second 2\n");
    }

    #[test]
    fn empty_input_is_empty() {
        assert!(load_console_log("").is_empty());
    }

    #[test]
    fn preserves_line_bytes_verbatim() {
        // The loader must not rewrite bytes: `split_inclusive('\n')` keeps CRLF
        // and payload `\r` intact and adds no terminator to an unterminated final
        // line (`lines()` + `push('\n')` would corrupt all three).
        let recs = load_console_log("a\r\nb\rc\nlast");
        assert_eq!(recs.len(), 3);
        assert_eq!(recs[0].1.line, b"a\r\n", "CRLF terminator kept verbatim");
        assert_eq!(recs[1].1.line, b"b\rc\n", "payload \\r kept");
        assert_eq!(
            recs[2].1.line, b"last",
            "no \\n added to an unterminated line"
        );
    }
}
