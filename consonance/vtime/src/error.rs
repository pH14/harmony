// SPDX-License-Identifier: AGPL-3.0-or-later
//! Error types for the vtime crate.

use thiserror::Error;

/// Opaque failure reported by a [`CpuBackend`](crate::CpuBackend)
/// implementation.
///
/// The pure-logic planner cannot know what can go wrong inside a real
/// backend (a failed `ioctl`, a closed perf fd, ...), so the payload is an
/// opaque message constructed by the backend via [`BackendError::new`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("cpu backend failure: {0}")]
pub struct BackendError(String);

impl BackendError {
    /// Builds a backend error from a human-readable description.
    pub fn new(msg: impl Into<String>) -> Self {
        BackendError(msg.into())
    }
}

/// Errors produced by this crate.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VtimeError {
    /// `VClockConfig::ratio_den` was zero.
    #[error("invalid VClock config: ratio denominator is zero")]
    ZeroRatioDen,
    /// `VClockConfig::ratio_num` was zero: V-time would never advance, so no
    /// timer deadline could ever be mapped back to a work count.
    #[error("invalid VClock config: ratio numerator is zero (V-time would never advance)")]
    ZeroRatioNum,
    /// The config would saturate V-time at a trivially small work count:
    /// already at `work == 1`, `vns_base + floor(ratio_num / ratio_den)`
    /// exceeds `u64::MAX`.
    #[error(
        "invalid VClock config: saturates at work = 1 \
         (vns_base = {vns_base}, vns step per work unit = {step_vns})"
    )]
    ImmediateSaturation {
        /// The offending `vns_base`.
        vns_base: u64,
        /// `floor(ratio_num / ratio_den)`, the V-time advance of one work unit.
        step_vns: u64,
    },
    /// A periodic timer was scheduled with `period_vns == 0`.
    #[error("invalid periodic timer: period is zero")]
    ZeroPeriod,
    /// A [`SimCpuConfig`](crate::sim::SimCpuConfig) was invalid; the message
    /// names the offending field.
    #[error("invalid simulator config: {0}")]
    InvalidSimConfig(&'static str),
    /// The backend stopped *past* the injection target. With a correctly
    /// sized `skid_margin` this must never happen; it destroys determinism
    /// (the interrupt can no longer be injected at the exact work count), so
    /// it is reported loudly with full diagnostics instead of being absorbed.
    #[error(
        "PMU skid exceeded the configured margin: armed at work {armed_at}, \
         target {target}, but execution stopped at {stopped_at}"
    )]
    SkidExceeded {
        /// Work count at which the overflow interrupt was armed
        /// (the work count stepping started from, if nothing was armed).
        armed_at: u64,
        /// The exact work count the caller asked to stop at.
        target: u64,
        /// Where execution actually stopped (`> target`).
        stopped_at: u64,
    },
    /// A [`CpuBackend`](crate::CpuBackend) call failed.
    #[error(transparent)]
    Backend(#[from] BackendError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_error_message_is_preserved() {
        let err = BackendError::new("perf fd closed");
        assert_eq!(err.to_string(), "cpu backend failure: perf fd closed");
        let wrapped: VtimeError = err.into();
        assert_eq!(wrapped.to_string(), "cpu backend failure: perf fd closed");
    }

    #[test]
    fn skid_exceeded_carries_diagnostics() {
        let err = VtimeError::SkidExceeded {
            armed_at: 90,
            target: 100,
            stopped_at: 105,
        };
        let msg = err.to_string();
        assert!(msg.contains("90") && msg.contains("100") && msg.contains("105"));
    }
}
