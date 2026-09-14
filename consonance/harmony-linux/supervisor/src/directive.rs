// SPDX-License-Identifier: AGPL-3.0-or-later

const MAX_LINE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Directive {
    Sometimes(u32),
    Reachable(u32),
    Always { point: u32, cond: bool },
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum DirectiveError {
    #[error("unknown directive {0:?}")]
    UnknownVerb(String),
    #[error("directive {verb:?} has malformed arguments")]
    BadArguments { verb: &'static str },
}

pub fn parse_directive(line: &str) -> Result<Option<Directive>, DirectiveError> {
    let trimmed = line.trim();
    if !trimmed.starts_with('@') {
        return Ok(None);
    }
    let mut words = trimmed.split_whitespace();
    let verb = words.next().unwrap_or_default();
    let directive = match verb {
        "@sometimes" => Directive::Sometimes(one_id(words, "@sometimes")?),
        "@reachable" => Directive::Reachable(one_id(words, "@reachable")?),
        "@always" => {
            let point = words
                .next()
                .and_then(|word| word.parse::<u32>().ok())
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

fn one_id<'a>(
    mut words: impl Iterator<Item = &'a str>,
    verb: &'static str,
) -> Result<u32, DirectiveError> {
    let id = words
        .next()
        .and_then(|word| word.parse::<u32>().ok())
        .ok_or(DirectiveError::BadArguments { verb })?;
    if words.next().is_some() {
        return Err(DirectiveError::BadArguments { verb });
    }
    Ok(id)
}

#[derive(Debug, Default)]
pub struct LineReader {
    buf: Vec<u8>,
    dropping: bool,
}

impl LineReader {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

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
            } else if self.buf.len() < MAX_LINE {
                self.buf.push(byte);
            } else {
                self.buf.clear();
                self.dropping = true;
            }
        }
        lines
    }

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
        assert_eq!(reader.push(b"junk\n@reachable 5\n"), ["@reachable 5"]);
        assert!(reader.flush().is_none());
    }
}
