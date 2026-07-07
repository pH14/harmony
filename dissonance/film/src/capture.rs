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

use crate::billboard::BillboardHeader;
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
}
