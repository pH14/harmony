// SPDX-License-Identifier: AGPL-3.0-or-later
//! The harmony in-guest play-agent (task 86) — the portable brain.
//!
//! This library is the target-agnostic half of the play-agent: the weighted
//! chord input policy ([`chord`]), the SMB RAM-map decode ([`ram`]), the
//! billboard layout writer ([`billboard`]), the state-register catalog
//! ([`regs`]), and the per-frame agent loop ([`agent`]) — all pure logic,
//! exercised against a **mock core** ([`core_seam::MockCore`]) so no test ever
//! crosses the libretro FFI, needs a ROM, or needs an emulator. The binary
//! (`src/main.rs`) supplies the Linux glue: the dlopen'd libretro core, the
//! `/dev/mem` doorbell transport, and the hugetlb-pinned billboard buffer.
//!
//! Determinism discipline (conventions rule 4): every decision is a pure
//! function of the entropy bytes the harness hands the agent; frame counters,
//! coordinates, buckets, and depth ordinals are integers; nothing here reads a
//! clock or an unseeded RNG, and no `HashMap`/`HashSet` exists in the crate.

pub mod agent;
pub mod billboard;
pub mod chord;
pub mod core_seam;
pub mod ram;
pub mod regs;

pub use agent::{Agent, AgentConfig, AgentError, Harness, StepReport};
pub use billboard::{
    BILLBOARD_LAYOUT_VERSION, BILLBOARD_MAGIC, BillboardError, BillboardLayout, HEADER_LEN,
};
pub use chord::{ChordAlphabet, ChordError};
pub use core_seam::{Core, MockCore};
pub use ram::{SmbState, WORK_RAM_LEN};
