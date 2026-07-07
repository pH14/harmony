// SPDX-License-Identifier: AGPL-3.0-or-later
//! The [`FrameRenderer`] seam — billboard capture → an RGB [`Frame`] — and the
//! deterministic test renderer [`StampRenderer`].
//!
//! The seam is the whole design of the render pass (task 87 §3): a
//! [`FrameRenderer`] turns a [`FrameCapture`] into pixels, and the *only*
//! production implementor is `core_replay::CoreReplay`, which renders with **zero
//! interpretation of pixels** — it loads the capture's savestate into the same
//! commit-pinned core and runs exactly one frame, so the picture is the core's
//! own, **1:1 by construction**. A hand-written PPU compositor was rejected by
//! the integrator (recorded in `IMPLEMENTATION.md`): a reconstruction is
//! approximate, and an investigator shown an approximated frame can reach a wrong
//! conclusion.
//!
//! The seam keeps the FFI off the default path: [`StampRenderer`] is a pure,
//! `unsafe`-free fake that stamps deterministic pixels from the header, so the
//! whole projector/output pipeline gates with no ROM and no core (and runs under
//! Miri). `CoreReplay` lives behind the `core-replay` feature.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::capture::FrameCapture;

/// The NES visible resolution — the dimensions [`StampRenderer`] and the
/// task-86 NES core both produce.
pub const NES_WIDTH: u32 = 256;
/// The NES visible resolution height.
pub const NES_HEIGHT: u32 = 240;

/// A rendered RGB frame: `width × height` pixels, three bytes (R, G, B) each, row
/// major, top-left origin. The invariant `rgb.len() == width * height * 3` holds
/// for every `Frame` a constructor returns.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Frame {
    width: u32,
    height: u32,
    rgb: Vec<u8>,
}

impl Frame {
    /// Build a frame from raw RGB bytes, checking the length invariant. Returns
    /// [`RenderError::FrameShape`] if `rgb.len() != width * height * 3` — so a
    /// bad-sized core frame is a loud error, never a silently wrong image.
    pub fn from_rgb(width: u32, height: u32, rgb: Vec<u8>) -> Result<Frame, RenderError> {
        let want = Self::rgb_len(width, height)?;
        if rgb.len() != want {
            return Err(RenderError::FrameShape {
                width,
                height,
                got: rgb.len(),
                want,
            });
        }
        Ok(Frame { width, height, rgb })
    }

    /// A frame filled with one solid RGB color — the contact-sheet background and
    /// a building block for tests.
    pub fn solid(width: u32, height: u32, color: [u8; 3]) -> Result<Frame, RenderError> {
        let want = Self::rgb_len(width, height)?;
        let mut rgb = Vec::with_capacity(want);
        for _ in 0..(want / 3) {
            rgb.extend_from_slice(&color);
        }
        Ok(Frame { width, height, rgb })
    }

    /// The frame width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The frame height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The RGB bytes (row major, `width * height * 3` long).
    pub fn rgb(&self) -> &[u8] {
        &self.rgb
    }

    /// The RGB triple at `(x, y)`, or `None` if out of range. Used by the contact
    /// sheet to copy a cell without indexing arithmetic in the caller.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = ((y as usize * self.width as usize) + x as usize) * 3;
        Some([self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]])
    }

    /// `width * height * 3`, or [`RenderError::FrameShape`] on overflow / a
    /// zero dimension — the size guard shared by the constructors.
    fn rgb_len(width: u32, height: u32) -> Result<usize, RenderError> {
        let px = (width as usize).checked_mul(height as usize);
        let bytes = px.and_then(|p| p.checked_mul(3));
        match bytes {
            Some(n) if width > 0 && height > 0 => Ok(n),
            _ => Err(RenderError::FrameShape {
                width,
                height,
                got: 0,
                want: 0,
            }),
        }
    }
}

/// A billboard capture → an RGB [`Frame`]. The one seam every renderer meets;
/// console-agnostic (Metroid reuses `CoreReplay` as-is; Super Mario World changes
/// only the core pin).
pub trait FrameRenderer {
    /// The dimensions this renderer emits — every [`render`](Self::render)'d
    /// frame has exactly these (checked by the projector so a contact sheet's
    /// cells are uniform).
    fn dimensions(&self) -> (u32, u32);

    /// Render one captured frame to pixels. A pure function of the capture (the
    /// core pin/ROM are fixed at construction): rendering the same capture twice
    /// yields byte-identical output (the render-determinism invariant).
    fn render(&mut self, capture: &FrameCapture) -> Result<Frame, RenderError>;
}

/// A deterministic, `unsafe`-free test renderer. It does **not** decode the
/// savestate — no fake claims to — it stamps a pattern that is a pure function of
/// the header's frame counter, joypad byte, and the first savestate byte, so a
/// clip round-trips to as many distinct, reproducible frames as it has distinct
/// captures. It exists only to gate the plan/projector/output pipeline with no
/// core present; the box gate proves the real picture with `CoreReplay`.
#[derive(Clone, Copy, Debug)]
pub struct StampRenderer {
    width: u32,
    height: u32,
}

impl Default for StampRenderer {
    fn default() -> Self {
        Self {
            width: NES_WIDTH,
            height: NES_HEIGHT,
        }
    }
}

impl StampRenderer {
    /// A stamp renderer at explicit dimensions (small sizes keep tests fast).
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// The pattern: a pure, integer-only function of `(frame, joypad, salt, x,
    /// y)` — deterministic and cheap (rule 4, no floats). Distinct headers stamp
    /// distinct frames, which is all the fake must guarantee.
    fn stamp(frame: u32, joypad: u8, salt: u8, x: u32, y: u32) -> [u8; 3] {
        let r = frame
            .wrapping_mul(7)
            .wrapping_add(x)
            .wrapping_add(u32::from(salt)) as u8;
        let g = frame.wrapping_mul(13).wrapping_add(y) as u8;
        let b = joypad ^ salt ^ (frame as u8);
        [r, g, b]
    }
}

impl FrameRenderer for StampRenderer {
    fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn render(&mut self, capture: &FrameCapture) -> Result<Frame, RenderError> {
        let frame = capture.frame;
        let joypad = capture.joypad();
        // A stable salt from the savestate so two frames that differ only in
        // savestate still stamp differently (the fake never *decodes* it).
        let salt = capture.savestate().first().copied().unwrap_or(0);
        let mut rgb = Vec::with_capacity((self.width as usize) * (self.height as usize) * 3);
        for y in 0..self.height {
            for x in 0..self.width {
                rgb.extend_from_slice(&Self::stamp(frame, joypad, salt, x, y));
            }
        }
        Frame::from_rgb(self.width, self.height, rgb)
    }
}

/// Why a render failed. [`StampRenderer`] never produces one; `CoreReplay` maps
/// the core's failure modes here (a missing core/ROM SKIP, an `unserialize`
/// rejection, an unexpected frame geometry).
#[derive(Clone, PartialEq, Eq, Debug, Error)]
pub enum RenderError {
    /// A frame's RGB length did not match its dimensions (or a dimension was
    /// zero / overflowed).
    #[error("frame shape mismatch: {width}x{height} needs {want} rgb bytes, got {got}")]
    FrameShape {
        /// The frame width.
        width: u32,
        /// The frame height.
        height: u32,
        /// The RGB byte count received.
        got: usize,
        /// The RGB byte count required (`0` if the dimensions themselves were
        /// invalid).
        want: usize,
    },
    /// The core rejected the capture's savestate (`retro_unserialize` failed) —
    /// a hard error, never a blank or approximated frame (`core_replay`).
    #[error("core rejected the savestate at frame {frame} (retro_unserialize failed)")]
    Unserialize {
        /// The frame whose savestate was rejected.
        frame: u32,
    },
    /// The pinned core or the user-supplied ROM was not available — the renderer
    /// reports SKIP loudly rather than rendering nothing (`core_replay`).
    #[error("core/ROM unavailable: {0}")]
    Unavailable(String),
    /// The core handed back a frame whose geometry did not match
    /// [`dimensions`](FrameRenderer::dimensions) (`core_replay`).
    #[error("core produced a {got_w}x{got_h} frame, expected {want_w}x{want_h}")]
    CoreGeometry {
        /// The width the core produced.
        got_w: u32,
        /// The height the core produced.
        got_h: u32,
        /// The expected width.
        want_w: u32,
        /// The expected height.
        want_h: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::billboard::{BillboardHeader, encode_billboard};

    fn capture(frame: u32, joypad: u8, save0: u8) -> FrameCapture {
        let bytes = encode_billboard(frame, joypad, &[save0; 8], &[0u8; 4]);
        let header = BillboardHeader::parse(&bytes).unwrap();
        FrameCapture {
            frame,
            moment: u64::from(frame) * 10,
            header,
            bytes,
        }
    }

    #[test]
    fn frame_from_rgb_checks_the_length_invariant() {
        assert!(Frame::from_rgb(2, 2, vec![0; 12]).is_ok());
        assert!(matches!(
            Frame::from_rgb(2, 2, vec![0; 11]),
            Err(RenderError::FrameShape { .. })
        ));
        assert!(matches!(
            Frame::from_rgb(0, 2, vec![]),
            Err(RenderError::FrameShape { .. })
        ));
    }

    #[test]
    fn stamp_render_is_pure_and_correctly_sized() {
        let mut r = StampRenderer::new(8, 6);
        let c = capture(3, 0b0000_0011, 0xAB);
        let a = r.render(&c).unwrap();
        let b = r.render(&c).unwrap();
        assert_eq!(a, b, "render is a pure function of the capture");
        assert_eq!(a.width(), 8);
        assert_eq!(a.height(), 6);
        assert_eq!(a.rgb().len(), 8 * 6 * 3);
    }

    #[test]
    fn distinct_frames_stamp_distinct_pixels() {
        let mut r = StampRenderer::new(8, 6);
        let f0 = r.render(&capture(0, 0, 0)).unwrap();
        let f1 = r.render(&capture(1, 0, 0)).unwrap();
        let f2 = r.render(&capture(2, 0, 0)).unwrap();
        assert_ne!(f0, f1);
        assert_ne!(f1, f2);
        assert_ne!(f0, f2);
    }
}
