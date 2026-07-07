// SPDX-License-Identifier: AGPL-3.0-or-later
//! The projector — the session driver that turns a [`FilmPlan`] into a
//! [`CaptureBundle`] over the task-82 session client (task 87 §2).
//!
//! It is a **pure observation query over the one timeline**. For a clip it
//! materializes the reproducer once at the first frame's `Moment`, then walks
//! linearly: per frame it `read`s the billboard (chunked to the task-80 cap),
//! **verifies the header** (magic, version, and — the load-bearing check — that
//! the stamped frame counter equals the frame-clock `Moment`'s frame), stores the
//! capture, and `run`s to the next frame's `Moment`. Every verb it sends is an
//! observation (`read`) or a navigation (`run`/materialize) — never `exec`, never
//! a recorded move — so the filmed replay is **hash-neutral by construction**:
//! the same timeline the searcher found, which the box gate proves (task 87 §2b).
//!
//! On a **dropped session** it re-materializes at the failed frame and continues
//! (bounded retries); a header mismatch, by contrast, is a hard error — a
//! misaligned frame is never rendered.

use environment::EnvSpec;
use resolution::{MomentRef, Server, Session, SessionError, StopReason};

use crate::billboard::BillboardHeader;
use crate::capture::{CaptureBundle, FrameCapture};
use crate::error::FilmError;
use crate::plan::{FilmPlan, FrameShot, ReadChunk};

/// The maximum number of times the projector re-materializes at a frame after a
/// dropped session before giving up on that frame ([`FilmError::SessionDropped`]).
/// A drop is a transport fault, not a logical one — a small budget recovers the
/// common case without masking a persistently broken connection.
pub const MAX_DROP_RETRIES: u32 = 3;

/// Film `plan`'s clip of `reproducer` over `session`, returning the ordered
/// [`CaptureBundle`]. One materialization per clip, linear from there; drops are
/// recovered by re-materializing at the failed frame.
///
/// The reads are host-side and hash-neutral (observation verbs only); capture and
/// rendering are separate passes, so the returned bundle can be rendered later or
/// elsewhere.
pub fn film<S: Server>(
    session: &mut Session<S>,
    reproducer: &EnvSpec,
    plan: &FilmPlan,
) -> Result<CaptureBundle, FilmError> {
    let chunks = plan.read_chunks();
    let mut bundle = CaptureBundle::new();
    for (i, shot) in plan.frames.iter().enumerate() {
        let capture = film_frame(session, reproducer, *shot, i == 0, &chunks)?;
        bundle.frames.push(capture);
    }
    Ok(bundle)
}

/// How the projector positions the timeline at a frame.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Position {
    /// Advance linearly from the current point — `run(until = moment)` (the happy
    /// path for every frame after the first).
    Advance,
    /// (Re-)materialize the reproducer at `moment` from genesis — the first frame
    /// of a clip, and the recovery after a dropped session.
    Rematerialize,
}

/// A single attempt's failure: a recoverable transport drop, or a fatal error.
enum Fail {
    /// The session dropped mid-attempt — recover by re-materializing.
    Drop,
    /// An unrecoverable failure (a control error, a short run, a header
    /// mismatch).
    Fatal(FilmError),
}

/// Film one frame, recovering dropped sessions by re-materializing at this
/// frame's `Moment` (bounded by [`MAX_DROP_RETRIES`]).
fn film_frame<S: Server>(
    session: &mut Session<S>,
    reproducer: &EnvSpec,
    shot: FrameShot,
    is_first: bool,
    chunks: &[ReadChunk],
) -> Result<FrameCapture, FilmError> {
    // The first frame establishes the timeline (materialize); later frames
    // advance linearly. A drop flips either into re-materialize on the next try.
    let mut position = if is_first {
        Position::Rematerialize
    } else {
        Position::Advance
    };
    let mut retries = 0u32;
    loop {
        match attempt_frame(session, reproducer, shot, position, chunks) {
            Ok(capture) => return Ok(capture),
            Err(Fail::Fatal(e)) => return Err(e),
            Err(Fail::Drop) => {
                if retries >= MAX_DROP_RETRIES {
                    return Err(FilmError::SessionDropped {
                        frame: shot.frame,
                        retries,
                    });
                }
                retries += 1;
                // Recover at exactly this frame, then continue linearly.
                position = Position::Rematerialize;
            }
        }
    }
}

/// One positioning + read + verify attempt.
fn attempt_frame<S: Server>(
    session: &mut Session<S>,
    reproducer: &EnvSpec,
    shot: FrameShot,
    position: Position,
    chunks: &[ReadChunk],
) -> Result<FrameCapture, Fail> {
    position_at(session, reproducer, shot, position)?;
    let bytes = read_billboard(session, chunks)?;
    // A header mismatch is a hard error — never a silently misaligned frame.
    let header = BillboardHeader::parse(&bytes).map_err(|source| {
        Fail::Fatal(FilmError::Header {
            frame: shot.frame,
            moment: shot.moment,
            source,
        })
    })?;
    header.verify(shot.frame).map_err(|source| {
        Fail::Fatal(FilmError::Header {
            frame: shot.frame,
            moment: shot.moment,
            source,
        })
    })?;
    Ok(FrameCapture {
        frame: shot.frame,
        moment: shot.moment,
        header,
        bytes,
    })
}

/// Position the timeline at `shot.moment`, verifying it lands exactly there (a
/// short landing — the guest crashed/quiesced first — is a fatal
/// [`FilmError::ShortRun`], the recorded frame being unreachable).
fn position_at<S: Server>(
    session: &mut Session<S>,
    reproducer: &EnvSpec,
    shot: FrameShot,
    position: Position,
) -> Result<(), Fail> {
    let (landed, kind) = match position {
        Position::Rematerialize => {
            let mref = MomentRef::new(reproducer.clone(), shot.moment);
            let mat = session.materialize(&mref).map_err(classify)?;
            (mat.moment(), stop_kind(mat.stop()))
        }
        Position::Advance => {
            let mut mat = session.materialized().map_err(classify)?;
            let stop = mat.run(shot.moment).map_err(classify)?;
            (mat.moment(), stop_kind(&stop))
        }
    };
    if landed != shot.moment {
        return Err(Fail::Fatal(FilmError::ShortRun {
            frame: shot.frame,
            landed,
            target: shot.moment,
            stop_kind: kind,
        }));
    }
    Ok(())
}

/// Read the full billboard buffer by issuing the plan's capped read chunks in
/// order and concatenating the replies. Reads are pure observation (hash-neutral).
fn read_billboard<S: Server>(
    session: &mut Session<S>,
    chunks: &[ReadChunk],
) -> Result<Vec<u8>, Fail> {
    let total: usize = chunks.iter().map(|c| c.len as usize).sum();
    let mut buf = Vec::with_capacity(total);
    let mut mat = session.materialized().map_err(classify)?;
    for chunk in chunks {
        let part = mat.read(chunk.gpa, chunk.len).map_err(classify)?;
        buf.extend_from_slice(&part);
    }
    Ok(buf)
}

/// Classify a [`SessionError`]: a transport drop is recoverable
/// ([`Fail::Drop`]); everything else is fatal. `NothingOpen` in the advance path
/// would be a projector logic bug, so it too surfaces loudly as fatal.
fn classify(err: SessionError) -> Fail {
    match err {
        SessionError::Transport(_) => Fail::Drop,
        other => Fail::Fatal(FilmError::Session(other)),
    }
}

/// A short, stable label for a [`StopReason`] kind — for the [`FilmError::ShortRun`]
/// message (the client's own `stop_kind` is crate-private).
fn stop_kind(stop: &StopReason) -> &'static str {
    match stop {
        StopReason::Deadline { .. } => "deadline",
        StopReason::Quiescent { .. } => "quiescent",
        StopReason::Crash { .. } => "crash",
        StopReason::Decision { .. } => "decision",
        StopReason::SnapshotPoint { .. } => "snapshot_point",
        StopReason::Assertion { .. } => "assertion",
    }
}
