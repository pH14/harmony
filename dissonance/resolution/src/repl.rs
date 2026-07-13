// SPDX-License-Identifier: AGPL-3.0-or-later
//! The REPL: the line protocol ([`Command`] + [`parse`](Command::parse)) and the
//! recording [`Shell`] that maps each command 1:1 onto the client and appends a
//! transcript [`Record`].
//!
//! No cleverness (`docs/RESOLUTION.md`: "the REPL is a thin, scriptable shell"):
//! every command reads as one line and emits a deterministic, machine-parseable
//! [`Record`] plus a human rendering ([`render_line`](crate::render_line)) over
//! the same record — agent-first. The eight commands are exactly the spec's:
//! `open` · `regs` · `read` · `hash` · `run` · `exec` · `vary` · `transcript`.

use environment::{Action, Answer, BitMask, HostFault, Moment, Ratio, Span};
use thiserror::Error;

use crate::server::Server;
use crate::session::{Session, stop_detail, stop_kind, stop_vtime};
use crate::transcript::{Outcome, Record, transcript_digest};
use crate::{MRefParseError, MomentRef, OverrideEdit, SessionError, to_hex};

/// One REPL command — the thin line protocol, parsed from a single line and
/// mapping 1:1 onto a client verb.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    /// `open <momentref>` — materialize a [`MomentRef`].
    Open(MomentRef),
    /// `regs` — the register view.
    Regs,
    /// `read <gpa> <len>` — read guest physical memory.
    Read {
        /// Guest-physical base (decimal or `0x` hex).
        gpa: u64,
        /// Byte count (decimal or `0x` hex).
        len: u32,
    },
    /// `hash` — the whole-state digest.
    Hash,
    /// `run <until>` — advance toward a [`Moment`].
    Run {
        /// The target moment (decimal or `0x` hex).
        until: Moment,
    },
    /// `exec <cmd>` — run a command in the guest (taints the timeline). The rest
    /// of the line, verbatim.
    Exec(String),
    /// `vary <edit>` — the counterfactual: one override edit on the open
    /// `MomentRef`.
    Vary(OverrideEdit),
    /// `transcript` — record a deterministic checkpoint (count + view digest) of
    /// the investigation so far. Recorded like every command, so it replays
    /// byte-identically; the full re-render is `resolution --transcript <file>`.
    Transcript,
}

impl Command {
    /// Parse one command line. Total: any malformed line is a
    /// [`CommandParseError`], never a panic.
    pub fn parse(line: &str) -> Result<Command, CommandParseError> {
        let line = line.trim();
        if line.is_empty() {
            return Err(CommandParseError::Empty);
        }
        let (verb, rest) = match line.split_once(char::is_whitespace) {
            Some((v, r)) => (v, r.trim_start()),
            None => (line, ""),
        };
        match verb {
            "open" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    return Err(CommandParseError::MissingArg("open <momentref>"));
                }
                if arg.split_whitespace().count() != 1 {
                    return Err(CommandParseError::UnexpectedArgs);
                }
                Ok(Command::Open(MomentRef::parse(arg)?))
            }
            "regs" => no_args(rest, Command::Regs),
            "read" => {
                let mut it = rest.split_whitespace();
                let gpa = it
                    .next()
                    .ok_or(CommandParseError::MissingArg("read <gpa> <len>"))?;
                let len = it
                    .next()
                    .ok_or(CommandParseError::MissingArg("read <gpa> <len>"))?;
                if it.next().is_some() {
                    return Err(CommandParseError::UnexpectedArgs);
                }
                Ok(Command::Read {
                    gpa: parse_num(gpa)?,
                    len: parse_u32(len)?,
                })
            }
            "hash" => no_args(rest, Command::Hash),
            "run" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    return Err(CommandParseError::MissingArg("run <until>"));
                }
                if arg.split_whitespace().count() != 1 {
                    return Err(CommandParseError::UnexpectedArgs);
                }
                Ok(Command::Run {
                    until: parse_num(arg)?,
                })
            }
            "exec" => {
                if rest.is_empty() {
                    return Err(CommandParseError::MissingArg("exec <cmd>"));
                }
                Ok(Command::Exec(rest.to_string()))
            }
            "vary" => {
                let tokens: Vec<&str> = rest.split_whitespace().collect();
                if tokens.is_empty() {
                    return Err(CommandParseError::MissingArg("vary <edit>"));
                }
                Ok(Command::Vary(parse_edit(&tokens)?))
            }
            "transcript" => no_args(rest, Command::Transcript),
            other => Err(CommandParseError::UnknownVerb(other.to_string())),
        }
    }

    /// The canonical text of this command — a normalized round-trippable form
    /// (independent of input whitespace/radix), stamped into the transcript so
    /// replay renders identically regardless of how the line was typed.
    pub fn canonical(&self) -> String {
        match self {
            Command::Open(m) => format!("open {m}"),
            Command::Regs => "regs".to_string(),
            Command::Read { gpa, len } => format!("read {gpa:#x} {len}"),
            Command::Hash => "hash".to_string(),
            Command::Run { until } => format!("run {until}"),
            Command::Exec(c) => format!("exec {c}"),
            Command::Vary(e) => format!("vary {}", edit_canonical(e)),
            Command::Transcript => "transcript".to_string(),
        }
    }
}

/// Why a command line failed to parse. Every variant is a total, panic-free
/// rejection of untrusted input.
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum CommandParseError {
    /// The line was empty (after trimming).
    #[error("empty command")]
    Empty,
    /// The first token is not a known verb.
    #[error("unknown command verb `{0}`")]
    UnknownVerb(String),
    /// A required argument was missing; the payload names the expected form.
    #[error("missing argument: expected `{0}`")]
    MissingArg(&'static str),
    /// Extra arguments followed a complete command.
    #[error("unexpected extra arguments")]
    UnexpectedArgs,
    /// A numeric argument was not a decimal or `0x` hex integer in range.
    #[error("malformed number `{0}`")]
    BadNumber(String),
    /// The `open` argument was not a valid [`MomentRef`].
    #[error("malformed moment reference: {0}")]
    BadMomentRef(#[from] MRefParseError),
    /// A `vary` edit did not match any known form.
    #[error(
        "unrecognized vary edit (try `set <at> skew|irq|corrupt|clock|nominal|raw …` or `remove <at>`)"
    )]
    UnknownEdit,
    /// A `set … clock` edit named a zero denominator.
    #[error("clock ratio denominator must be non-zero")]
    BadRatio,
    /// A `set … raw` edit's hex was malformed.
    #[error("malformed hex in raw action")]
    BadHex,
    /// A `set … raw` edit's bytes were not a valid `Action`.
    #[error("raw action bytes are not a valid Action")]
    BadRawAction,
}

/// The result of [`Shell::dispatch`]: a recorded command (its [`Record`] was
/// appended — every command, `transcript` included) or a no-op view (a
/// blank/comment line, nothing recorded).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum DispatchOutput {
    /// A recorded command — render it with [`render_line`](crate::render_line).
    /// Every command (including `transcript`) produces this.
    Recorded(Record),
    /// A no-op with nothing to record (a blank/comment line): print `String`
    /// verbatim (empty). Never carries a `transcript` dump anymore — `transcript`
    /// is a recorded command.
    View(String),
}

/// The recording shell: owns a [`Session`] and the growing transcript. Each
/// [`dispatch`](Shell::dispatch) runs one command against the session and
/// appends one [`Record`] (except `transcript`, a view). The bin wraps this with
/// stdin/file I/O; tests drive it directly.
pub struct Shell<S: Server> {
    session: Session<S>,
    records: Vec<Record>,
    seq: u64,
}

impl<S: Server> Shell<S> {
    /// Build a shell over a connected session.
    pub fn new(session: Session<S>) -> Self {
        Self {
            session,
            records: Vec::new(),
            seq: 0,
        }
    }

    /// The transcript so far.
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// The underlying session (e.g. to inspect the current `MomentRef`).
    pub fn session(&self) -> &Session<S> {
        &self.session
    }

    /// Parse and dispatch a raw command line. A parse error is itself recorded
    /// (category `parse`) so the transcript stays complete; a blank line records
    /// nothing and returns an empty view.
    pub fn execute_line(&mut self, line: &str) -> DispatchOutput {
        if line.trim().is_empty() {
            return DispatchOutput::View(String::new());
        }
        match Command::parse(line) {
            Ok(cmd) => self.dispatch(cmd),
            Err(e) => {
                self.seq += 1;
                let record = Record {
                    seq: self.seq,
                    mref: self.stamp(),
                    cmd: line.trim().to_string(),
                    outcome: Outcome::Error {
                        category: "parse".to_string(),
                        message: e.to_string(),
                    },
                };
                self.records.push(record.clone());
                DispatchOutput::Recorded(record)
            }
        }
    }

    /// Dispatch a parsed command. **Every** command — including `transcript` —
    /// runs, appends a stamped [`Record`], and returns it, so `--record` captures
    /// the whole session and `--transcript` replay reproduces the live stdout
    /// byte-for-byte.
    pub fn dispatch(&mut self, cmd: Command) -> DispatchOutput {
        self.seq += 1;
        let cmd_text = cmd.canonical();
        let outcome = self.run_cmd(&cmd);
        let record = Record {
            seq: self.seq,
            mref: self.stamp(),
            cmd: cmd_text,
            outcome,
        };
        self.records.push(record.clone());
        DispatchOutput::Recorded(record)
    }

    /// The `MomentRef` stamp for the next record: the current open moment
    /// (env + position), or `-` if nothing is open.
    ///
    /// A **tainted** timeline's coordinate is not reproducible (its state is off
    /// the record — see task 81), so it is stamped with
    /// [`MomentRef::TAINTED_STAMP_PREFIX`] rather than a bare, reproducible-
    /// claiming `MomentRef`. The inner address (the pre-`exec` origin) is kept
    /// for provenance, but `open` refuses the marked form
    /// ([`MRefParseError::Tainted`](crate::MRefParseError)) rather than reopening
    /// the untainted state it would mis-address.
    fn stamp(&self) -> String {
        match self.session.current_mref() {
            Some(m) if self.session.tainted() => {
                format!("{}{m}", MomentRef::TAINTED_STAMP_PREFIX)
            }
            Some(m) => m.to_string(),
            None => "-".to_string(),
        }
    }

    /// Run one command against the session and summarize it as an [`Outcome`].
    fn run_cmd(&mut self, cmd: &Command) -> Outcome {
        match cmd {
            Command::Open(mref) => match self.session.materialize(mref) {
                Ok(mut ms) => {
                    let moment = ms.moment();
                    let tainted = ms.tainted();
                    // Surface the landing stop: a crash/quiescence before the
                    // requested moment is visible here, not a swallowed open.
                    let stop = stop_kind(ms.stop()).to_string();
                    let detail = stop_detail(ms.stop());
                    match ms.hash() {
                        Ok(h) => Outcome::Opened {
                            moment,
                            stop,
                            detail,
                            hash: to_hex(&h),
                            tainted,
                        },
                        Err(e) => err_outcome(&e),
                    }
                }
                Err(e) => err_outcome(&e),
            },
            Command::Regs => match self.session.materialized() {
                Ok(mut ms) => match ms.regs() {
                    Ok(view) => Outcome::Regs {
                        view: Box::new(view),
                    },
                    Err(e) => err_outcome(&e),
                },
                Err(e) => err_outcome(&e),
            },
            Command::Read { gpa, len } => match self.session.materialized() {
                Ok(mut ms) => match ms.read(*gpa, *len) {
                    Ok(bytes) => Outcome::Bytes {
                        hex: to_hex(&bytes),
                    },
                    Err(e) => err_outcome(&e),
                },
                Err(e) => err_outcome(&e),
            },
            Command::Hash => match self.session.materialized() {
                Ok(mut ms) => match ms.hash() {
                    Ok(h) => Outcome::Hash { hex: to_hex(&h) },
                    Err(e) => err_outcome(&e),
                },
                Err(e) => err_outcome(&e),
            },
            Command::Run { until } => match self.session.materialized() {
                Ok(mut ms) => match ms.run(*until) {
                    Ok(stop) => Outcome::Stop {
                        stop: stop_kind(&stop).to_string(),
                        vtime: stop_vtime(&stop),
                        detail: stop_detail(&stop),
                    },
                    Err(e) => err_outcome(&e),
                },
                Err(e) => err_outcome(&e),
            },
            Command::Exec(c) => match self.session.materialized() {
                Ok(mut ms) => match ms.exec(c) {
                    Ok(r) => Outcome::Exec {
                        ok: r.ok,
                        tainted: r.tainted,
                        // Lossless: guest serial bytes are arbitrary, and the
                        // transcript is the replayable artifact.
                        output_hex: to_hex(&r.output),
                    },
                    Err(e) => err_outcome(&e),
                },
                Err(e) => err_outcome(&e),
            },
            Command::Vary(edit) => {
                // A tainted timeline has no reproducer, so its counterfactual
                // would be a bare pasteable address that replays the *un-exec'd*
                // env at the post-exec moment — a misleading reproducer dressed
                // as a counterfactual. Route through the SAME structural
                // choke-point as `mref`/`recorded_env` and fail loudly (the taint
                // rule). Wind back to a clean moment to vary.
                if self.session.guard_reproducible().is_err() {
                    err_outcome(&SessionError::Tainted)
                } else {
                    match self.session.current_mref() {
                        Some(base) => Outcome::Varied {
                            mref: base.vary(edit).to_string(),
                        },
                        None => err_outcome(&SessionError::NothingOpen),
                    }
                }
            }
            // `transcript` is a recorded command: a deterministic checkpoint of
            // the view so far — the count of preceding records and a digest of
            // their rendering (NOT the full text, which would recurse). Computed
            // over `self.records`, which at this point holds exactly the records
            // that precede this `transcript` record.
            Command::Transcript => Outcome::Transcript {
                count: self.records.len() as u64,
                digest: transcript_digest(&self.records),
            },
        }
    }
}

/// Summarize a [`SessionError`] as an error [`Outcome`] — the *error* result
/// category, with the message verbatim.
fn err_outcome(e: &SessionError) -> Outcome {
    Outcome::Error {
        category: e.category().to_string(),
        message: e.to_string(),
    }
}

/// `Ok(cmd)` if `rest` holds no further tokens, else
/// [`CommandParseError::UnexpectedArgs`].
fn no_args(rest: &str, cmd: Command) -> Result<Command, CommandParseError> {
    if rest.trim().is_empty() {
        Ok(cmd)
    } else {
        Err(CommandParseError::UnexpectedArgs)
    }
}

/// Parse a `vary` edit from its whitespace tokens.
fn parse_edit(tokens: &[&str]) -> Result<OverrideEdit, CommandParseError> {
    match tokens {
        ["remove", at] => Ok(OverrideEdit::Remove { at: parse_num(at)? }),
        ["set", at, "skew", v] => Ok(set(
            parse_num(at)?,
            Action::Host(HostFault::SkewTime(Span(parse_num(v)?))),
        )),
        ["set", at, "irq", vector] => Ok(set(
            parse_num(at)?,
            Action::Host(HostFault::InjectInterrupt {
                vector: parse_u8(vector)?,
            }),
        )),
        ["set", at, "corrupt", gpa, mask] => Ok(set(
            parse_num(at)?,
            Action::Host(HostFault::CorruptMemory {
                gpa: parse_num(gpa)?,
                mask: BitMask(parse_num(mask)?),
            }),
        )),
        ["set", at, "clock", num, den] => {
            let ratio =
                Ratio::new(parse_num(num)?, parse_num(den)?).ok_or(CommandParseError::BadRatio)?;
            Ok(set(
                parse_num(at)?,
                Action::Host(HostFault::SetClockRate(ratio)),
            ))
        }
        ["set", at, "nominal"] => Ok(set(parse_num(at)?, Action::Guest(Answer::Nominal))),
        ["set", at, "raw", hex] => {
            let bytes = crate::from_hex(hex, crate::MAX_HEX_FIELD_BYTES)
                .ok_or(CommandParseError::BadHex)?;
            let action = Action::decode(&bytes).map_err(|_| CommandParseError::BadRawAction)?;
            Ok(set(parse_num(at)?, action))
        }
        _ => Err(CommandParseError::UnknownEdit),
    }
}

/// Render an [`OverrideEdit`] back to its canonical `vary` text (round-trips
/// with [`parse_edit`]; a non-host, non-nominal action uses the generic `raw`
/// form so it is always representable).
fn edit_canonical(edit: &OverrideEdit) -> String {
    match edit {
        OverrideEdit::Remove { at } => format!("remove {at}"),
        OverrideEdit::Set { at, action } => match action {
            Action::Host(HostFault::SkewTime(v)) => format!("set {at} skew {}", v.0),
            Action::Host(HostFault::InjectInterrupt { vector }) => {
                format!("set {at} irq {vector}")
            }
            Action::Host(HostFault::CorruptMemory { gpa, mask }) => {
                format!("set {at} corrupt {gpa:#x} {:#x}", mask.0)
            }
            Action::Host(HostFault::SetClockRate(r)) => {
                format!("set {at} clock {} {}", r.num(), r.den())
            }
            Action::Guest(Answer::Nominal) => format!("set {at} nominal"),
            other => format!("set {at} raw {}", to_hex(&other.encode())),
        },
    }
}

/// Build a `Set` edit.
fn set(at: Moment, action: Action) -> OverrideEdit {
    OverrideEdit::Set { at, action }
}

/// Parse a `u64` argument: `0x`/`0X` hex or decimal.
fn parse_num(s: &str) -> Result<u64, CommandParseError> {
    parse_u64_opt(s).ok_or_else(|| CommandParseError::BadNumber(s.to_string()))
}

/// The panic-free `u64` parse behind [`parse_num`].
fn parse_u64_opt(s: &str) -> Option<u64> {
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u64::from_str_radix(h, 16).ok()
    } else {
        s.parse().ok()
    }
}

/// Parse a `u32` argument (range-checked).
fn parse_u32(s: &str) -> Result<u32, CommandParseError> {
    u32::try_from(parse_num(s)?).map_err(|_| CommandParseError::BadNumber(s.to_string()))
}

/// Parse a `u8` argument (range-checked).
fn parse_u8(s: &str) -> Result<u8, CommandParseError> {
    u8::try_from(parse_num(s)?).map_err(|_| CommandParseError::BadNumber(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_canonicalizes_every_verb() {
        // read: radix-normalized canonical.
        assert_eq!(
            Command::parse("read 4096 64").unwrap().canonical(),
            "read 0x1000 64"
        );
        assert_eq!(Command::parse("regs").unwrap(), Command::Regs);
        assert_eq!(Command::parse("hash").unwrap(), Command::Hash);
        assert_eq!(
            Command::parse("run 0x2000").unwrap(),
            Command::Run { until: 0x2000 }
        );
        assert_eq!(
            Command::parse("exec ps aux").unwrap(),
            Command::Exec("ps aux".to_string())
        );
        assert_eq!(Command::parse("transcript").unwrap(), Command::Transcript);
    }

    #[test]
    fn vary_edits_round_trip_through_canonical() {
        for line in [
            "vary set 100 skew 9",
            "vary set 100 irq 32",
            "vary set 100 corrupt 0x1000 0xff",
            "vary set 100 clock 3 2",
            "vary set 100 nominal",
            "vary remove 100",
        ] {
            let cmd = Command::parse(line).unwrap();
            let reparsed = Command::parse(&cmd.canonical()).unwrap();
            assert_eq!(cmd, reparsed, "round-trip failed for {line}");
        }
    }

    #[test]
    fn parse_rejects_garbage_without_panicking() {
        for bad in [
            "",
            "bogus",
            "read",
            "read 1",
            "read 1 2 3",
            "run",
            "run a b",
            "open",
            "open not-a-mref",
            "vary set 1 clock 3 0", // zero denominator
            "vary wat",
            "exec", // missing command
        ] {
            assert!(Command::parse(bad).is_err(), "expected error for {bad:?}");
        }
    }
}
