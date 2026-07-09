// SPDX-License-Identifier: AGPL-3.0-or-later
//! # film — the visible replay, rendered by the core itself
//!
//! `film` is the **resolution layer's showpiece**: `(reproducer, Moment) → what
//! the screen showed`. The obvious way to watch a discovered game reproducer —
//! re-run it with the emulator's rendering on — is forbidden by the
//! one-reproducer rule (`docs/LAYERS.md`): extra render instructions would change
//! the guest instruction stream, so the `Moment`s and every `state_hash` would
//! diverge and the thing filmed would no longer be the thing the searcher found.
//!
//! Film is instead a **pure observation query over the one timeline**. It:
//!
//! 1. derives a [`FilmPlan`] from a reproducer's recorded trace — the `REG_FRAME`
//!    frame clock gives the `Moment`s, the billboard address registers give the
//!    read window,
//! 2. drives the task-82 [`Session`] client ([`film`]): materialize the
//!    reproducer once, then per frame `read` the [billboard](BillboardHeader),
//!    **verify** its header (a frame-counter mismatch is a hard error), and `run`
//!    to the next frame — sending only observation/navigation verbs, so the
//!    filmed replay is **hash-neutral** (the same timeline; the box gate proves
//!    it),
//! 3. renders each capture through the [`FrameRenderer`] seam. The only
//!    production impl, [`CoreReplay`](core_replay) (behind the `core-replay`
//!    feature), loads the capture's savestate into the *same commit-pinned core*
//!    and runs exactly one frame — the picture is the core's own, **1:1 by
//!    construction**. [`StampRenderer`] is the pure, `unsafe`-free test renderer
//!    the default gates use,
//! 4. writes a PPM sequence and a [`contact_sheet`] — [`blake3_hex`] digests are
//!    the committed artifact; rendered game frames are never committed.
//!
//! ## The wire contracts this crate models locally (conventions rule 2)
//!
//! The [billboard header](BillboardHeader) (task 86) and the `read`/`regs` verbs
//! (tasks 80/81, via [`resolution`]) are sibling specs unmerged on this branch,
//! so film **defines the header layout locally** and codes against
//! `resolution`'s client, exactly the pattern task 82 uses. When those land the
//! integrator reconciles them onto one layout (see `IMPLEMENTATION.md`);
//! nothing observable here depends on which side owns the constant.

mod billboard;
mod capture;
mod error;
mod mock;
mod output;
mod plan;
mod projector;
mod render;

#[cfg(feature = "core-replay")]
mod core_replay;

pub use billboard::{
    BILLBOARD_LAYOUT_VERSION, BILLBOARD_MAGIC, BillboardHeader, HEADER_LEN, HeaderError, Region,
    encode_billboard,
};
pub use capture::{CaptureBundle, CaptureError, FrameCapture};
pub use error::FilmError;
pub use mock::{BillboardScenario, Corruption, MockBillboardServer};
pub use output::{OutputError, blake3_hex, contact_sheet, write_ppm};
pub use plan::{BillboardWindow, ClipSelect, FilmPlan, FrameShot, FrameTick, PlanError, ReadChunk};
pub use projector::{MAX_DROP_RETRIES, film};
pub use render::{Frame, FrameRenderer, NES_HEIGHT, NES_WIDTH, RenderError, StampRenderer};

#[cfg(feature = "core-replay")]
pub use core_replay::CoreReplay;

// The session/reproducer types a film pipeline is built from, re-exported so a
// consumer (the `film` bin, the box harness) need not also name `resolution` /
// `environment` directly.
pub use environment::{EnvSpec, Moment};
pub use resolution::{MomentRef, Server, Session};
