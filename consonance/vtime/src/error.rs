// SPDX-License-Identifier: AGPL-3.0-or-later
//! Error types for the vtime crate.

use thiserror::Error;

/// Errors produced by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VtimeError {
    /// A periodic timer was scheduled with `period_vns == 0`.
    #[error("invalid periodic timer: period is zero")]
    ZeroPeriod,
}
