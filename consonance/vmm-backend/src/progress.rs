// SPDX-License-Identifier: AGPL-3.0-or-later
//! Host-only progress for deciding whether a live guest run is stalled.

use std::sync::atomic::{AtomicU64, Ordering};

/// Monotonic count of guest exits returned by a backend.
///
/// The count is host-only: it is absent from guest state, snapshots, identity,
/// and determinism hashes. A wall watchdog samples it to bound time since the
/// last exit while allowing a slow but progressing request to continue.
#[derive(Debug, Default)]
pub struct RunProgress(AtomicU64);

impl RunProgress {
    /// Record one exit returned to the VMM.
    pub fn record_exit(&self) {
        self.0.fetch_add(1, Ordering::Release);
    }

    /// Current exit sequence.
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_counts_completed_exits() {
        let progress = RunProgress::default();
        assert_eq!(progress.sequence(), 0);
        progress.record_exit();
        progress.record_exit();
        assert_eq!(progress.sequence(), 2);
    }
}
