// SPDX-License-Identifier: AGPL-3.0-or-later
//! The hook output protocol: a hook reports its own assertions by writing one
//! directive per stdout line, which the agent forwards to the SDK.
//!
//! ```text
//! @sometimes <u32>          assert_sometimes hit at that point
//! @reachable <u32>          assert_reachable at that point
//! @always <u32> <0|1>       assert_always(cond) at that point
//! ```
//!
//! Any other line is ordinary hook output and is ignored. A line that starts
//! with `@` but does not parse is an error rather than silent output: a
//! workload whose oracle line is misspelled would otherwise report no bug and
//! look healthy.

/// The largest line the reader will accumulate before dropping it. A hook that
/// writes an unterminated multi-megabyte line is misbehaving, and the agent
/// must not grow with it.
const MAX_LINE: usize = 64 * 1024;

/// One directive from a hook's stdout.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Directive {
    /// `assert_sometimes` was satisfied at this point.
    Sometimes(u32),
    /// `assert_reachable` at this point.
    Reachable(u32),
    /// The workload units the hook has verified so far, cumulative and
    /// monotonic. It is a count, not an assertion point, so it lands in a
    /// register instead of the SDK.
    Verified(u64),
    /// `assert_always(cond)` at this point.
    Always {
        /// The assertion point id.
        point: u32,
        /// The condition the hook evaluated; `false` is a bug report.
        cond: bool,
    },
}

/// Why a `@`-prefixed line was rejected.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DirectiveError {
    /// The verb after `@` is not one of the three.
    #[error("unknown directive {0:?}")]
    UnknownVerb(String),
    /// The verb is known but its arguments are wrong in count or form.
    #[error("directive {verb:?} has malformed arguments")]
    BadArguments {
        /// The verb that was recognised.
        verb: &'static str,
    },
}

/// Parse one hook output line.
///
/// # Errors
///
/// Returns [`DirectiveError`] when the line claims to be a directive (a leading
/// `@`) but is not one. Ordinary output is `Ok(None)`.
pub fn parse_directive(line: &str) -> Result<Option<Directive>, DirectiveError> {
    let trimmed = line.trim();
    if !trimmed.starts_with('@') {
        return Ok(None);
    }
    let mut words = trimmed.split_whitespace();
    // The line is non-empty and starts with `@`, so there is a first word.
    let verb = words.next().unwrap_or_default();
    let directive = match verb {
        "@sometimes" => Directive::Sometimes(one_id(words, "@sometimes")?),
        "@reachable" => Directive::Reachable(one_id(words, "@reachable")?),
        "@verified" => Directive::Verified(one_count(words, "@verified")?),
        "@always" => {
            let point = words
                .next()
                .and_then(|w| w.parse::<u32>().ok())
                .ok_or(DirectiveError::BadArguments { verb: "@always" })?;
            let cond = match words.next() {
                Some("0") => false,
                Some("1") => true,
                _ => return Err(DirectiveError::BadArguments { verb: "@always" }),
            };
            if words.next().is_some() {
                return Err(DirectiveError::BadArguments { verb: "@always" });
            }
            Directive::Always { point, cond }
        }
        other => return Err(DirectiveError::UnknownVerb(other.to_string())),
    };
    Ok(Some(directive))
}

fn one_count<'a>(
    mut words: impl Iterator<Item = &'a str>,
    verb: &'static str,
) -> Result<u64, DirectiveError> {
    let count = words
        .next()
        .and_then(|w| w.parse::<u64>().ok())
        .ok_or(DirectiveError::BadArguments { verb })?;
    if words.next().is_some() {
        return Err(DirectiveError::BadArguments { verb });
    }
    Ok(count)
}

fn one_id<'a>(
    mut words: impl Iterator<Item = &'a str>,
    verb: &'static str,
) -> Result<u32, DirectiveError> {
    let id = words
        .next()
        .and_then(|w| w.parse::<u32>().ok())
        .ok_or(DirectiveError::BadArguments { verb })?;
    if words.next().is_some() {
        return Err(DirectiveError::BadArguments { verb });
    }
    Ok(id)
}

/// Splits the bytes read from a hook's output file into whole lines across
/// reads, so a directive split by a read boundary is still delivered once.
#[derive(Debug, Default)]
pub struct LineReader {
    buf: Vec<u8>,
    /// Set when the pending line has already exceeded [`MAX_LINE`]: the rest of
    /// it is discarded up to the next newline.
    dropping: bool,
}

impl LineReader {
    /// A reader with no pending bytes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `chunk` and return every line it completed. Invalid UTF-8 is
    /// replaced rather than rejected: hook output is untrusted bytes, and a
    /// non-UTF-8 byte in ordinary output must not stop the agent.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                if !self.dropping {
                    lines.push(String::from_utf8_lossy(&self.buf).into_owned());
                }
                self.buf.clear();
                self.dropping = false;
            } else if self.dropping {
                // The rest of an oversized line is discarded up to its newline.
            } else if self.buf.len() < MAX_LINE {
                self.buf.push(byte);
            } else {
                self.buf.clear();
                self.dropping = true;
            }
        }
        lines
    }

    /// Take the trailing bytes as a final line, for a hook that exited without
    /// a closing newline.
    pub fn flush(&mut self) -> Option<String> {
        let dropping = core::mem::replace(&mut self.dropping, false);
        let buf = core::mem::take(&mut self.buf);
        if dropping || buf.is_empty() {
            return None;
        }
        Some(String::from_utf8_lossy(&buf).into_owned())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_verified_count_parses_as_a_count_not_an_assertion_point() {
        assert_eq!(
            parse_directive("@verified 4096"),
            Ok(Some(Directive::Verified(4096)))
        );
        assert!(parse_directive("@verified").is_err());
        assert!(parse_directive("@verified -1").is_err());
        assert!(parse_directive("@verified 1 2").is_err());
    }

    use super::*;

    #[test]
    fn the_three_verbs_parse() {
        assert_eq!(
            parse_directive("@sometimes 7"),
            Ok(Some(Directive::Sometimes(7)))
        );
        assert_eq!(
            parse_directive("  @reachable 0  "),
            Ok(Some(Directive::Reachable(0)))
        );
        assert_eq!(
            parse_directive("@always 2 0"),
            Ok(Some(Directive::Always {
                point: 2,
                cond: false
            }))
        );
        assert_eq!(
            parse_directive("@always 4294967295 1"),
            Ok(Some(Directive::Always {
                point: u32::MAX,
                cond: true
            }))
        );
    }

    #[test]
    fn ordinary_output_is_not_a_directive() {
        for line in [
            "",
            "   ",
            "inserted 200 keys",
            "email@example.com",
            "-- @always 1 0",
        ] {
            assert_eq!(parse_directive(line), Ok(None), "line: {line:?}");
        }
    }

    #[test]
    fn a_misspelled_or_malformed_directive_is_an_error() {
        assert_eq!(
            parse_directive("@sometime 7"),
            Err(DirectiveError::UnknownVerb("@sometime".to_string()))
        );
        for line in [
            "@sometimes",
            "@sometimes x",
            "@sometimes 7 8",
            "@sometimes -1",
        ] {
            assert_eq!(
                parse_directive(line),
                Err(DirectiveError::BadArguments { verb: "@sometimes" }),
                "line: {line:?}"
            );
        }
        for line in [
            "@always",
            "@always 1",
            "@always 1 2",
            "@always 1 true",
            "@always 1 0 0",
        ] {
            assert_eq!(
                parse_directive(line),
                Err(DirectiveError::BadArguments { verb: "@always" }),
                "line: {line:?}"
            );
        }
        assert_eq!(
            parse_directive("@reachable"),
            Err(DirectiveError::BadArguments { verb: "@reachable" })
        );
    }

    #[test]
    fn lines_are_reassembled_across_read_boundaries() {
        let mut reader = LineReader::new();
        assert!(reader.push(b"@some").is_empty());
        assert!(reader.push(b"times 3").is_empty());
        assert_eq!(
            reader.push(b"\nnoise\n@always 1 0"),
            ["@sometimes 3", "noise"]
        );
        assert_eq!(reader.flush().unwrap(), "@always 1 0");
        assert_eq!(reader.flush(), None);
    }

    #[test]
    fn carriage_returns_and_invalid_utf8_survive() {
        let mut reader = LineReader::new();
        // A CR is trimmed by the parser, so a CRLF hook still reports.
        let lines = reader.push(b"@sometimes 1\r\n\xff\xfe raw\n");
        assert_eq!(
            parse_directive(&lines[0]),
            Ok(Some(Directive::Sometimes(1)))
        );
        assert_eq!(parse_directive(&lines[1]), Ok(None));
    }

    #[test]
    fn an_unterminated_giant_line_is_dropped_not_accumulated() {
        let mut reader = LineReader::new();
        for _ in 0..40 {
            assert!(reader.push(&vec![b'x'; 4096]).is_empty());
        }
        // The tail of the oversized line is discarded with it, and the reader
        // recovers at the next newline.
        assert_eq!(reader.push(b"junk\n@reachable 5\n"), ["@reachable 5"]);
        assert!(reader.flush().is_none());
    }
}
