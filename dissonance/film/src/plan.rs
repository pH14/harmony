// SPDX-License-Identifier: AGPL-3.0-or-later
//! [`FilmPlan`] — the shot list, derived from a reproducer's recorded trace.
//!
//! Film is a **batch query**, and the query is itself a replayable artifact (the
//! transcript principle, `docs/RESOLUTION.md`): a `FilmPlan` is a pure,
//! serializable derivation from three inputs the recorded trace already carries,
//! and nothing else —
//!
//! - the **frame clock**: the `REG_FRAME` channel's `(frame, Moment)` ticks (the
//!   play-agent emits one per vblank, task 86 §2); these are the only `Moment`s
//!   film ever addresses,
//! - the **billboard window**: `(gpa, len)` from the billboard address registers
//!   published once at init (task 86 §3), split into task-80-capped read chunks,
//! - the **clip selection**: a `[start, end]` `Span` on the axis *or* a frame
//!   range, plus an optional stride (every Nth frame) for contact-sheet density.
//!
//! [`FilmPlan::derive`] validates and composes them; the result is inspectable
//! (Debug + serde) so a reviewer can read exactly which frames a clip will film
//! before a single verb is sent. It is pure logic — no ROM, no core, no session
//! (rule 4: no floats, integer chunk arithmetic only).

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::billboard::HEADER_LEN;
use environment::Moment;

/// One `REG_FRAME` observation from the recorded trace: the frame counter the
/// play-agent stamped and the frame-clock [`Moment`] it stamped it at. The input
/// unit of [`FilmPlan::derive`].
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrameTick {
    /// The `REG_FRAME` frame counter (the value the billboard header will carry).
    pub frame: u32,
    /// The frame-clock `Moment` this frame was reached at.
    pub moment: Moment,
}

/// The billboard buffer's location in guest physical memory: base `gpa` and
/// total `len`, from the billboard address registers. The read window every
/// frame's capture reads.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BillboardWindow {
    /// The billboard buffer's guest-physical base address.
    pub gpa: u64,
    /// The billboard buffer's total length in bytes (header + regions).
    pub len: u32,
}

impl BillboardWindow {
    /// Split this window into read chunks each at most `read_cap` bytes — the
    /// task-80 length cap ([`resolution::READ_CAP`]), respected by chunking so a
    /// billboard larger than one `read` (e.g. an SNES savestate) still films.
    /// Chunks are contiguous, in ascending `gpa` order, and reassemble to exactly
    /// `len` bytes.
    ///
    /// A `read_cap` of zero yields **no chunks** (never a non-terminating loop):
    /// [`FilmPlan::derive`] rejects a zero cap, but a `FilmPlan` reached any other
    /// way — deserialized from untrusted JSON, or built by hand (every field is
    /// `pub`) — must not be able to hang a reader on `read_cap == 0` (rule 4:
    /// library code never loops on untrusted input). [`crate::film`] rejects such
    /// a plan up front, so this empty list is a belt-and-braces guard, not a
    /// silent success.
    fn chunks(&self, read_cap: u32) -> Vec<ReadChunk> {
        if read_cap == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut done: u32 = 0;
        while done < self.len {
            let take = read_cap.min(self.len - done);
            out.push(ReadChunk {
                gpa: self.gpa + u64::from(done),
                len: take,
            });
            done += take;
        }
        out
    }
}

/// One capped read against the billboard window: `read(gpa, len)` with `len ≤`
/// the task-80 cap. The projector issues these in order and concatenates the
/// replies into the full billboard buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ReadChunk {
    /// The guest-physical base of this chunk.
    pub gpa: u64,
    /// The chunk length (`≤ read_cap`).
    pub len: u32,
}

/// Which frames of the trace a clip selects. Either a `[start, end]` inclusive
/// `Span` on the `Moment` axis, or an inclusive `[first, last]` frame-counter
/// range, or the whole trace.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ClipSelect {
    /// Every recorded frame.
    All,
    /// Frames whose `Moment` lies in `[start, end]` (inclusive).
    MomentSpan {
        /// The span's first `Moment` (inclusive).
        start: Moment,
        /// The span's last `Moment` (inclusive).
        end: Moment,
    },
    /// Frames whose counter lies in `[first, last]` (inclusive).
    FrameRange {
        /// The first frame counter (inclusive).
        first: u32,
        /// The last frame counter (inclusive).
        last: u32,
    },
}

/// One frame the plan will film: the frame counter (to verify the billboard
/// header against) and the `Moment` to materialize/run to.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrameShot {
    /// The frame counter — the value [`crate::BillboardHeader::verify`] asserts.
    pub frame: u32,
    /// The frame-clock `Moment` to position the timeline at.
    pub moment: Moment,
}

/// The shot list: the billboard window (and its capped read chunks), the ordered
/// frames to film, and the clip parameters that produced them — a pure,
/// serializable, inspectable artifact of the query.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FilmPlan {
    /// The billboard buffer location every frame reads.
    pub billboard: BillboardWindow,
    /// The per-call read cap the chunks respect (task-80 length cap).
    pub read_cap: u32,
    /// The frames to film, in ascending `Moment` order.
    pub frames: Vec<FrameShot>,
    /// The clip selection that produced [`frames`](Self::frames) (recorded for
    /// the artifact).
    pub clip: ClipSelect,
    /// The resolved stride (every Nth selected frame; `1` = every frame).
    pub stride: u32,
}

impl FilmPlan {
    /// Derive a plan from a recorded trace's frame clock and billboard window.
    ///
    /// `ticks` must be **strictly increasing** in both `moment` and `frame` (a
    /// run only advances; frame gaps — non-contiguous frame counters from
    /// unrecorded frames — are allowed). `clip` selects a sub-range; `stride`
    /// (`None` = every frame, `Some(0)` rejected) thins for a contact sheet;
    /// `read_cap` is the task-80 per-`read` cap ([`resolution::READ_CAP`]).
    ///
    /// Pure and total: every malformed input is a distinct [`PlanError`], never a
    /// panic (rule 4).
    pub fn derive(
        ticks: &[FrameTick],
        billboard: BillboardWindow,
        clip: ClipSelect,
        stride: Option<u32>,
        read_cap: u32,
    ) -> Result<FilmPlan, PlanError> {
        if ticks.is_empty() {
            return Err(PlanError::EmptyInput);
        }
        if read_cap == 0 {
            return Err(PlanError::ZeroReadCap);
        }
        // The window must at least hold a header, and gpa+len must not overflow
        // the guest-physical space.
        if (billboard.len as usize) < HEADER_LEN {
            return Err(PlanError::BillboardTooSmall {
                len: billboard.len,
                need: HEADER_LEN,
            });
        }
        if billboard
            .gpa
            .checked_add(u64::from(billboard.len))
            .is_none()
        {
            return Err(PlanError::BillboardOverflow {
                gpa: billboard.gpa,
                len: billboard.len,
            });
        }
        let stride = match stride {
            None => 1,
            Some(0) => return Err(PlanError::ZeroStride),
            Some(n) => n,
        };
        // The frame clock must strictly advance on both axes.
        for w in ticks.windows(2) {
            if w[1].moment <= w[0].moment {
                return Err(PlanError::NonMonotonicMoment {
                    prev: w[0].moment,
                    next: w[1].moment,
                });
            }
            if w[1].frame <= w[0].frame {
                return Err(PlanError::NonMonotonicFrame {
                    prev: w[0].frame,
                    next: w[1].frame,
                });
            }
        }
        // Select the clip.
        let selected: Vec<&FrameTick> = match clip {
            ClipSelect::All => ticks.iter().collect(),
            ClipSelect::MomentSpan { start, end } => {
                if start > end {
                    return Err(PlanError::BadSpan { start, end });
                }
                ticks
                    .iter()
                    .filter(|t| t.moment >= start && t.moment <= end)
                    .collect()
            }
            ClipSelect::FrameRange { first, last } => {
                if first > last {
                    return Err(PlanError::BadFrameRange { first, last });
                }
                ticks
                    .iter()
                    .filter(|t| t.frame >= first && t.frame <= last)
                    .collect()
            }
        };
        // Thin by stride (every Nth selected frame, starting at the first).
        let frames: Vec<FrameShot> = selected
            .iter()
            .step_by(stride as usize)
            .map(|t| FrameShot {
                frame: t.frame,
                moment: t.moment,
            })
            .collect();
        if frames.is_empty() {
            return Err(PlanError::EmptyClip);
        }
        Ok(FilmPlan {
            billboard,
            read_cap,
            frames,
            clip,
            stride,
        })
    }

    /// The ordered, task-80-capped read chunks that reassemble to the full
    /// billboard buffer. Every frame's capture reads these (they are the same for
    /// every frame — the billboard sits at one fixed `gpa`).
    pub fn read_chunks(&self) -> Vec<ReadChunk> {
        self.billboard.chunks(self.read_cap)
    }
}

/// Why [`FilmPlan::derive`] rejected its inputs. Every variant is a distinct,
/// panic-free rejection (rule 4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum PlanError {
    /// The frame clock was empty — nothing to film.
    #[error("empty frame clock")]
    EmptyInput,
    /// A `read_cap` of zero would make chunking non-terminating.
    #[error("read cap must be non-zero")]
    ZeroReadCap,
    /// A stride of zero selects no frames.
    #[error("stride must be non-zero")]
    ZeroStride,
    /// The frame clock did not strictly advance in `Moment`.
    #[error("frame clock is not strictly increasing in moment ({prev} then {next})")]
    NonMonotonicMoment {
        /// The earlier `Moment`.
        prev: Moment,
        /// The offending later `Moment` (`≤ prev`).
        next: Moment,
    },
    /// The frame clock did not strictly advance in frame counter.
    #[error("frame clock is not strictly increasing in frame ({prev} then {next})")]
    NonMonotonicFrame {
        /// The earlier frame counter.
        prev: u32,
        /// The offending later frame counter (`≤ prev`).
        next: u32,
    },
    /// A [`ClipSelect::MomentSpan`] had `start > end`.
    #[error("clip span start {start} is after end {end}")]
    BadSpan {
        /// The span start.
        start: Moment,
        /// The span end.
        end: Moment,
    },
    /// A [`ClipSelect::FrameRange`] had `first > last`.
    #[error("clip frame range first {first} is after last {last}")]
    BadFrameRange {
        /// The range's first frame.
        first: u32,
        /// The range's last frame.
        last: u32,
    },
    /// The clip and stride selected no frames from the trace.
    #[error("clip selects no frames from the trace")]
    EmptyClip,
    /// The billboard window is too small to hold even a header.
    #[error("billboard window is {len} bytes, smaller than the {need}-byte header")]
    BillboardTooSmall {
        /// The window length given.
        len: u32,
        /// The minimum ([`HEADER_LEN`]).
        need: usize,
    },
    /// `gpa + len` overflows the guest-physical address space.
    #[error("billboard window [{gpa:#x}, +{len}) overflows the address space")]
    BillboardOverflow {
        /// The window base.
        gpa: u64,
        /// The window length.
        len: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: u32 = 1 << 16;

    fn clock(n: u32) -> Vec<FrameTick> {
        (0..n)
            .map(|i| FrameTick {
                frame: i,
                moment: 1000 + u64::from(i) * 10,
            })
            .collect()
    }

    fn window() -> BillboardWindow {
        BillboardWindow {
            gpa: 0x1_0000,
            len: 30 * 1024,
        }
    }

    #[test]
    fn derive_all_selects_every_frame() {
        let plan = FilmPlan::derive(&clock(3), window(), ClipSelect::All, None, CAP).unwrap();
        assert_eq!(plan.frames.len(), 3);
        assert_eq!(
            plan.frames[0],
            FrameShot {
                frame: 0,
                moment: 1000
            }
        );
        assert_eq!(plan.stride, 1);
    }

    #[test]
    fn moment_span_clips_inclusive() {
        let plan = FilmPlan::derive(
            &clock(5),
            window(),
            ClipSelect::MomentSpan {
                start: 1010,
                end: 1030,
            },
            None,
            CAP,
        )
        .unwrap();
        // moments 1010,1020,1030 → frames 1,2,3.
        assert_eq!(
            plan.frames.iter().map(|f| f.frame).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn stride_thins_the_clip() {
        let plan = FilmPlan::derive(&clock(6), window(), ClipSelect::All, Some(2), CAP).unwrap();
        assert_eq!(
            plan.frames.iter().map(|f| f.frame).collect::<Vec<_>>(),
            vec![0, 2, 4]
        );
        assert_eq!(plan.stride, 2);
    }

    #[test]
    fn chunks_reassemble_to_the_full_window() {
        // A 30 KiB window with a 64 KiB cap → one chunk; with a small cap →
        // several contiguous chunks summing to len.
        let plan = FilmPlan::derive(&clock(1), window(), ClipSelect::All, None, CAP).unwrap();
        assert_eq!(plan.read_chunks().len(), 1);

        let small = FilmPlan::derive(&clock(1), window(), ClipSelect::All, None, 4096).unwrap();
        let chunks = small.read_chunks();
        assert_eq!(chunks.iter().map(|c| c.len).sum::<u32>(), window().len);
        // contiguous, ascending
        for w in chunks.windows(2) {
            assert_eq!(w[0].gpa + u64::from(w[0].len), w[1].gpa);
        }
        assert_eq!(chunks[0].gpa, window().gpa);
    }

    #[test]
    fn rejects_non_monotonic_and_empty() {
        assert_eq!(
            FilmPlan::derive(&[], window(), ClipSelect::All, None, CAP),
            Err(PlanError::EmptyInput)
        );
        let bad = vec![
            FrameTick {
                frame: 0,
                moment: 100,
            },
            FrameTick {
                frame: 1,
                moment: 100,
            }, // moment not advancing
        ];
        assert!(matches!(
            FilmPlan::derive(&bad, window(), ClipSelect::All, None, CAP),
            Err(PlanError::NonMonotonicMoment { .. })
        ));
    }

    #[test]
    fn rejects_zero_stride_and_empty_clip() {
        assert_eq!(
            FilmPlan::derive(&clock(3), window(), ClipSelect::All, Some(0), CAP),
            Err(PlanError::ZeroStride)
        );
        assert_eq!(
            FilmPlan::derive(
                &clock(3),
                window(),
                ClipSelect::FrameRange {
                    first: 100,
                    last: 200
                },
                None,
                CAP,
            ),
            Err(PlanError::EmptyClip)
        );
    }

    #[test]
    fn chunks_of_a_zero_cap_plan_are_empty_not_a_hang() {
        // `derive` rejects a zero cap, but a `FilmPlan` built by hand (all fields
        // are `pub`) or deserialized must never hang the chunker (rule 4).
        let plan = FilmPlan {
            billboard: window(),
            read_cap: 0,
            frames: vec![FrameShot {
                frame: 0,
                moment: 0,
            }],
            clip: ClipSelect::All,
            stride: 1,
        };
        assert!(plan.read_chunks().is_empty());
    }

    #[test]
    fn rejects_tiny_or_overflowing_window() {
        assert!(matches!(
            FilmPlan::derive(
                &clock(1),
                BillboardWindow { gpa: 0, len: 4 },
                ClipSelect::All,
                None,
                CAP,
            ),
            Err(PlanError::BillboardTooSmall { .. })
        ));
        assert!(matches!(
            FilmPlan::derive(
                &clock(1),
                BillboardWindow {
                    gpa: u64::MAX - 10,
                    len: 1024
                },
                ClipSelect::All,
                None,
                CAP,
            ),
            Err(PlanError::BillboardOverflow { .. })
        ));
    }
}
