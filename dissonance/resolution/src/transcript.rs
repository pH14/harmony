// SPDX-License-Identifier: AGPL-3.0-or-later
//! The transcript: every command + result summary as one `MomentRef`-stamped
//! JSONL [`Record`], plus the **one renderer** ([`render_line`]) that live and
//! replay both go through.
//!
//! Task 29's design principle — *one renderer, keyed on V-time, so live and
//! replay are identical* (`docs/RESOLUTION.md` §"The human layer") — is realised
//! here concretely: a live session appends a `Record` per command and prints
//! [`render_line`] of it; a replay reads the `Record`s back and prints
//! [`render_line`] of each. Because the human rendering is a **pure function of
//! the record** and the record round-trips losslessly through JSON, the two
//! renderings are byte-identical. Records are deterministic given the same
//! session inputs: no wall-clock, only V-time (the [`Moment`] inside the stamp)
//! and a monotonic [`seq`](Record::seq).

use serde::{Deserialize, Serialize};

use crate::MomentRef;
use crate::server::RegsView;

/// One transcript entry: a monotonic sequence number, the `MomentRef` the
/// command acted at (its textual encoding — the copyable stamp), the canonical
/// command text, and the structured [`Outcome`].
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Monotonic per-session sequence number (starts at 1). No wall-clock — the
    /// only ordering signal besides the `Moment` in [`mref`](Record::mref).
    pub seq: u64,
    /// The `MomentRef` textual encoding this command acted at — env + the moment
    /// after the command. Self-contained: paste it into another session to
    /// re-reach the point.
    pub mref: String,
    /// The canonical command text (verb + normalized args), e.g.
    /// `read 0x1000 64`.
    pub cmd: String,
    /// The structured result summary.
    pub outcome: Outcome,
}

/// A structured, serde-round-tripping result summary — one variant per command
/// outcome, plus the two error categories. Every field is a primitive or a
/// small view, so the JSON is compact and the [`render_line`] over it is a pure
/// function.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    /// `open` landed a materialization: the moment reached, the landing
    /// [`StopReason`](control_proto::StopReason) kind (so an early crash /
    /// quiescence *before* the requested moment is visible, never a swallowed
    /// clean open), the whole-state hash (hex), and the lineage taint bit.
    Opened {
        /// The moment the materialization landed at (may be *earlier* than the
        /// requested moment if the guest stopped short — compare with the `open`
        /// command's `MomentRef`).
        moment: u64,
        /// The landing stop kind: `deadline` (reached the requested moment) vs
        /// `crash` / `quiescent` (stopped short).
        stop: String,
        /// Optional stop detail (a crash's kind + message).
        detail: Option<String>,
        /// The `hash(Whole)` at the landing (hex).
        hash: String,
        /// Whether the landed timeline is tainted.
        tainted: bool,
    },
    /// `regs`: the full versioned register view. Boxed because it dwarfs the
    /// other variants (16 GPRs + control registers), keeping [`Outcome`] and
    /// the [`DispatchOutput`](crate::DispatchOutput) that carries a [`Record`]
    /// small.
    Regs {
        /// The register view.
        view: Box<RegsView>,
    },
    /// `read`: the bytes read (hex).
    Bytes {
        /// The read bytes, lower-hex.
        hex: String,
    },
    /// `hash`: the digest (hex).
    Hash {
        /// The 32-byte digest, lower-hex.
        hex: String,
    },
    /// `run`: a guest-observable [`StopReason`](control_proto::StopReason) —
    /// the *data* result category (never an error).
    Stop {
        /// The stop kind: `deadline` / `quiescent` / `crash` / `decision` /
        /// `snapshot_point` / `assertion`.
        stop: String,
        /// The V-time the run stopped at.
        vtime: u64,
        /// Optional detail (a crash's kind + message).
        detail: Option<String>,
    },
    /// `exec`: an improvisation outcome. `tainted` is displayed prominently —
    /// the timeline is now off the record.
    Exec {
        /// Whether the command completed cleanly.
        ok: bool,
        /// The timeline's taint bit (always `true` after an exec).
        tainted: bool,
        /// The captured serial output (UTF-8 text; the mock's is ASCII).
        output: String,
    },
    /// `vary`: the counterfactual `MomentRef` (its textual encoding) — copy it
    /// and `open` it to run the one-change replay.
    Varied {
        /// The varied `MomentRef` encoding.
        mref: String,
    },
    /// A control failure — the *error* result category, kept strictly apart from
    /// [`Stop`](Outcome::Stop). Carries the `SessionError` display verbatim
    /// (including a `Tainted` guard).
    Error {
        /// A short category label: a [`SessionError`](crate::SessionError)
        /// category (`control` / `tainted` / `read` / `nothing_open` /
        /// `negotiation` / `transport`) or a REPL-level one (`parse`).
        category: String,
        /// The error's `Display` string, verbatim.
        message: String,
    },
}

/// **The one renderer.** Format a single [`Record`] into its human line — a pure
/// function, shared by live output and replay so they are byte-identical. The
/// machine-parseable JSONL is the record itself; this is the human view over it.
///
/// Layout: `[<seq>] <mref-fp> <cmd> => <outcome>`, where `<mref-fp>` is a short
/// deterministic fingerprint of the (long) `MomentRef` encoding so the line
/// stays scannable while the full stamp lives in the JSONL.
pub fn render_line(r: &Record) -> String {
    let fp = mref_fingerprint(&r.mref);
    // Flag a tainted (off-the-record, non-reproducible) coordinate in the human
    // view too — the JSONL stamp already carries the marker; this surfaces it.
    let taint_mark = if r.mref.starts_with(MomentRef::TAINTED_STAMP_PREFIX) {
        "!"
    } else {
        ""
    };
    let rendered = match &r.outcome {
        Outcome::Opened {
            moment,
            stop,
            detail,
            hash,
            tainted,
        } => {
            let d = match detail {
                Some(d) => format!(" ({d})"),
                None => String::new(),
            };
            format!(
                "opened @moment {moment} stop={stop}{d} hash {}{}",
                short(hash),
                taint_flag(*tainted)
            )
        }
        Outcome::Regs { view } => format!(
            "rip={:#018x} rflags={:#010x} cr3={:#x} cs={:#06x} @moment {} (regs v{})",
            view.rip, view.rflags, view.cr3, view.seg[0], view.moment, view.version
        ),
        Outcome::Bytes { hex } => {
            format!("read {} bytes: {hex}", hex.len() / 2)
        }
        Outcome::Hash { hex } => format!("hash {hex}"),
        Outcome::Stop {
            stop,
            vtime,
            detail,
        } => match detail {
            Some(d) => format!("stop {stop} @vtime {vtime} ({d})"),
            None => format!("stop {stop} @vtime {vtime}"),
        },
        Outcome::Exec {
            ok,
            tainted,
            output,
        } => format!(
            "exec ok={ok}{} output({}b): {}",
            taint_flag(*tainted),
            output.len(),
            output.escape_default()
        ),
        // The whole point of `vary` is the counterfactual address, so render it
        // in full — never `short` — so an agent/human consuming the rendered
        // output (not the JSONL) can paste it straight into `open`.
        Outcome::Varied { mref } => format!("varied => {mref}"),
        Outcome::Error { category, message } => format!("ERROR[{category}] {message}"),
    };
    format!("[{}] {taint_mark}{fp} {} => {rendered}", r.seq, r.cmd)
}

/// Render a whole transcript: [`render_line`] of each record, newline-joined.
/// Both the interactive `transcript` command and `--replay` call this, so the
/// live dump and the file re-render are identical.
pub fn render_transcript(records: &[Record]) -> String {
    let mut out = String::new();
    for r in records {
        out.push_str(&render_line(r));
        out.push('\n');
    }
    out
}

/// Serialize the records to JSONL (one compact JSON object per line).
pub fn to_jsonl(records: &[Record]) -> Result<String, serde_json::Error> {
    let mut out = String::new();
    for r in records {
        out.push_str(&serde_json::to_string(r)?);
        out.push('\n');
    }
    Ok(out)
}

/// Parse JSONL back into records. Blank lines are skipped; a malformed line is a
/// loud error (never a panic).
pub fn from_jsonl(text: &str) -> Result<Vec<Record>, serde_json::Error> {
    let mut records = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        records.push(serde_json::from_str(line)?);
    }
    Ok(records)
}

/// A short, deterministic fingerprint of a `MomentRef` encoding — the first 8
/// hex digits of an FNV-1a hash. Purely for the human line; the full stamp is in
/// the JSONL.
fn mref_fingerprint(mref: &str) -> String {
    let mut h = 0xCBF2_9CE4_8422_2325u64;
    for &b in mref.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    format!("{:08x}", (h >> 32) as u32)
}

/// The first 16 chars of a hex/encoded string (a stable, scannable prefix).
fn short(s: &str) -> String {
    s.chars().take(16).collect()
}

/// The prominent taint marker for the human line.
fn taint_flag(tainted: bool) -> &'static str {
    if tainted { " [TAINTED]" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Record> {
        vec![
            Record {
                seq: 1,
                mref: "mref1:100:00ff".to_string(),
                cmd: "open mref1:100:00ff".to_string(),
                outcome: Outcome::Opened {
                    moment: 100,
                    stop: "deadline".to_string(),
                    detail: None,
                    hash: "abcdef0123456789".to_string(),
                    tainted: false,
                },
            },
            Record {
                seq: 2,
                mref: "mref1:100:00ff".to_string(),
                cmd: "exec ls /".to_string(),
                outcome: Outcome::Exec {
                    ok: true,
                    tainted: true,
                    output: "# ls /\n".to_string(),
                },
            },
        ]
    }

    #[test]
    fn jsonl_round_trips_losslessly() {
        let recs = sample();
        let text = to_jsonl(&recs).unwrap();
        assert_eq!(from_jsonl(&text).unwrap(), recs);
    }

    #[test]
    fn render_is_pure_over_records() {
        let recs = sample();
        // Rendering the originals and the round-tripped records is identical:
        // this is the one-renderer property in miniature.
        let text = to_jsonl(&recs).unwrap();
        let reparsed = from_jsonl(&text).unwrap();
        assert_eq!(render_transcript(&recs), render_transcript(&reparsed));
    }

    #[test]
    fn from_jsonl_rejects_garbage_without_panicking() {
        assert!(from_jsonl("not json\n").is_err());
    }
}
