// SPDX-License-Identifier: AGPL-3.0-or-later
//! # vtime — deterministic virtual-time arithmetic
//!
//! The guest never observes host time. The VMM assigns a fixed virtual-time
//! delta to each normalized VM exit and stores the accumulated value in
//! [`VClock`]. [`TimerQueue`] supplies deterministic deadline ordering and
//! [`IdlePlanner`] jumps an idle guest to its next scheduled event. All
//! arithmetic is integer-only and saturating.
//!
//! ## Determinism rules embedded here
//!
//! Everything is integer arithmetic; saturation (to `u64::MAX`) is the
//! documented, deterministic overflow behavior everywhere. [`TimerQueue`]
//! fires equal deadlines in FIFO scheduling order (a documented total
//! order), and periodic timers re-arm at `fired deadline + period` — fixed
//! cadence with no drift accumulation. Nothing in this crate reads wall
//! clocks, uses unseeded randomness, or iterates a hash map.

mod clock;
mod error;
mod idle;
pub mod pvclock;
mod queue;

pub use clock::{VClock, VClockConfig};
pub use error::VtimeError;
pub use idle::{IdleAdvance, IdlePlanner};
pub use queue::{TimerQueue, TimerToken};
