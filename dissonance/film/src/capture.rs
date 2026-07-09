// SPDX-License-Identifier: AGPL-3.0-or-later
//! The in-timeline capture: what the projector reads at each frame `Moment`,
//! separated from rendering so a bundle can be filmed once and rendered later or
//! elsewhere (task 87 §2).
//!
//! A [`FrameCapture`] is one frame's billboard buffer plus its verified header;
//! a [`CaptureBundle`] is the ordered clip. Both are pure data (serde) — reading
//! them touches no core and no ROM, exactly the observation/rendering split the
//! task rules: capture is host-side and hash-neutral (verbs only), rendering
//! (`render`) is the host-side, out-of-timeline second pass.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::billboard::{BillboardHeader, HeaderError};
use environment::Moment;

/// One filmed frame's raw capture: the frame counter and `Moment` it was read
/// at, the parsed+verified [`BillboardHeader`], and the full billboard buffer the
/// projector reassembled from its capped read chunks. The savestate and work-RAM
/// bytes are borrowed back out of [`bytes`](Self::bytes) via the header, so the
/// capture stays one contiguous allocation.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FrameCapture {
    /// The frame counter (verified equal to the header's).
    pub frame: u32,
    /// The frame-clock `Moment` this billboard was read at.
    pub moment: Moment,
    /// The parsed, validated header.
    pub header: BillboardHeader,
    /// The full billboard buffer (`header + regions`) read at this `Moment`.
    pub bytes: Vec<u8>,
}

impl FrameCapture {
    /// The core savestate region — what `core_replay` hands `retro_unserialize`.
    pub fn savestate(&self) -> &[u8] {
        self.header.savestate(&self.bytes)
    }

    /// The console work-RAM region — the stable window for ad-hoc RAM inspection.
    pub fn work_ram(&self) -> &[u8] {
        self.header.work_ram(&self.bytes)
    }

    /// The joypad byte the renderer presents to `retro_run` for this frame.
    pub fn joypad(&self) -> u8 {
        self.header.joypad
    }

    /// **Revalidate a loaded capture** against itself: a `FrameCapture` from
    /// untrusted JSON (a load-later artifact — task 87 §2) carries a stored
    /// `header` and `bytes` that serde does not cross-check. This re-derives the
    /// header from `bytes` and asserts it equals the stored one, and that the
    /// frame counters agree — so a corrupt/tampered bundle fails **self-
    /// describingly** here rather than rendering garbage downstream. Total and
    /// panic-free (rule 4).
    pub fn validate(&self) -> Result<(), CaptureError> {
        let parsed =
            BillboardHeader::parse(&self.bytes).map_err(|source| CaptureError::Header {
                frame: self.frame,
                source,
            })?;
        if parsed != self.header {
            return Err(CaptureError::HeaderMismatch { frame: self.frame });
        }
        if self.header.frame != self.frame {
            return Err(CaptureError::FrameCounter {
                frame: self.frame,
                header_frame: self.header.frame,
            });
        }
        Ok(())
    }
}

/// An ordered clip of [`FrameCapture`]s — the artifact the projector produces and
/// the renderer consumes. Rendering it twice must be byte-identical (the box
/// gate's render-determinism assertion); it carries no wall-clock, only frame
/// counters and `Moment`s (rule 4).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize, Default)]
pub struct CaptureBundle {
    /// The captured frames, in ascending `Moment` order (the plan's frame order).
    pub frames: Vec<FrameCapture>,
}

impl CaptureBundle {
    /// An empty bundle.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of captured frames.
    pub fn len(&self) -> usize {
        self.frames.len()
    }

    /// Whether the bundle has no frames.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// [`validate`](FrameCapture::validate) every frame — the load-time integrity
    /// check for a JSON-loaded bundle. Reports the first inconsistent frame.
    pub fn validate(&self) -> Result<(), CaptureError> {
        for frame in &self.frames {
            frame.validate()?;
        }
        Ok(())
    }
}

/// Why a loaded [`CaptureBundle`]/[`FrameCapture`] failed
/// [`validate`](FrameCapture::validate) — a corrupt or tampered on-disk artifact
/// (rule 4: distinct, panic-free).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum CaptureError {
    /// A frame's `bytes` did not parse as a billboard header.
    #[error("frame {frame}: billboard bytes do not parse ({source})")]
    Header {
        /// The frame index in the bundle.
        frame: u32,
        /// The underlying parse error.
        #[source]
        source: HeaderError,
    },
    /// A frame's stored header did not match the header re-derived from its bytes.
    #[error("frame {frame}: stored header does not match its bytes (corrupt artifact)")]
    HeaderMismatch {
        /// The frame index in the bundle.
        frame: u32,
    },
    /// A frame's stored counter did not match its header's frame counter.
    #[error("frame {frame}: stored counter disagrees with the header's ({header_frame})")]
    FrameCounter {
        /// The stored frame counter.
        frame: u32,
        /// The header's frame counter.
        header_frame: u32,
    },
}
