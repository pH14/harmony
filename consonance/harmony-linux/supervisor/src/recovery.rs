// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecoveryError {
    #[error("recovery generation exhausted")]
    GenerationExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryReadiness {
    generation: u64,
    ready: bool,
    fault_pending: bool,
    queued: Vec<u32>,
}

impl RecoveryReadiness {
    #[must_use]
    pub fn initially_ready() -> Self {
        Self {
            generation: 0,
            ready: true,
            fault_pending: false,
            queued: Vec::new(),
        }
    }

    pub fn restarted(&mut self, after_fault: bool) -> Result<(), RecoveryError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(RecoveryError::GenerationExhausted)?;
        self.ready = false;
        self.fault_pending |= after_fault;
        Ok(())
    }

    pub fn request_hook(&mut self, id: u32) -> Vec<u32> {
        if self.ready {
            vec![id]
        } else {
            self.queued.push(id);
            Vec::new()
        }
    }

    pub fn mark_ready(&mut self, generation: u64) -> Vec<u32> {
        if generation != self.generation || self.ready {
            return Vec::new();
        }
        self.ready = true;
        self.fault_pending = false;
        self.queued.drain(..).collect()
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        !self.ready
    }

    #[must_use]
    pub fn is_fault_pending(&self) -> bool {
        self.fault_pending
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

pub const RECOVERY_NANOS: u64 = 30_000_000_000;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RecoveryWatch {
    quiet_since: u64,
    reported: Option<u64>,
}

impl RecoveryWatch {
    pub fn observe(
        &mut self,
        tick: u64,
        disturbed: bool,
        readiness: &RecoveryReadiness,
    ) -> Option<u64> {
        if disturbed {
            self.quiet_since = tick;
            return None;
        }
        if !readiness.is_pending() || self.reported == Some(readiness.generation()) {
            return None;
        }
        let quiet = tick.saturating_sub(self.quiet_since);
        if quiet < RECOVERY_NANOS / crate::TICK_NANOS {
            return None;
        }
        self.reported = Some(readiness.generation());
        Some(quiet)
    }
}

#[cfg(test)]
mod tests {
    use super::{RecoveryError, RecoveryReadiness, RecoveryWatch};

    const BOUND: u64 = super::RECOVERY_NANOS / crate::TICK_NANOS;

    #[test]
    fn a_workload_that_stays_unready_after_faults_end_is_reported_once() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        readiness.restarted(true).unwrap();
        assert_eq!(watch.observe(10, true, &readiness), None);
        assert_eq!(watch.observe(10 + BOUND - 1, false, &readiness), None);
        assert_eq!(watch.observe(10 + BOUND, false, &readiness), Some(BOUND));
        assert_eq!(watch.observe(20 + BOUND, false, &readiness), None);
        readiness.restarted(false).unwrap();
        assert_eq!(
            watch.observe(30 + BOUND, false, &readiness),
            Some(20 + BOUND)
        );
    }

    #[test]
    fn faults_and_readiness_keep_the_watch_silent() {
        let mut readiness = RecoveryReadiness::initially_ready();
        let mut watch = RecoveryWatch::default();
        assert_eq!(watch.observe(2 * BOUND, false, &readiness), None);
        readiness.restarted(true).unwrap();
        for tick in 0..3 * BOUND {
            assert_eq!(watch.observe(tick, true, &readiness), None);
        }
        readiness.mark_ready(readiness.generation());
        assert_eq!(watch.observe(10 * BOUND, false, &readiness), None);
    }

    #[test]
    fn a_slow_probe_keeps_hooks_queued_until_the_current_generation_is_ready() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        let generation = readiness.generation();

        assert!(readiness.is_pending());
        assert!(readiness.request_hook(7).is_empty());
        assert!(readiness.request_hook(7).is_empty());
        assert!(readiness.mark_ready(generation - 1).is_empty());
        assert!(readiness.is_pending());

        assert_eq!(readiness.mark_ready(generation), [7, 7]);
        assert!(!readiness.is_pending());
    }

    #[test]
    fn a_stale_probe_cannot_release_hooks_after_a_new_restart() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        let stale = readiness.generation();
        assert!(readiness.request_hook(3).is_empty());

        readiness.restarted(true).unwrap();
        let current = readiness.generation();
        assert_ne!(current, stale);
        assert!(readiness.mark_ready(stale).is_empty());
        assert!(readiness.is_pending());
        assert_eq!(readiness.mark_ready(current), [3]);
    }

    #[test]
    fn ready_hooks_preserve_request_order_and_duplicates() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(true).unwrap();
        assert!(readiness.request_hook(2).is_empty());
        assert!(readiness.request_hook(1).is_empty());
        assert!(readiness.request_hook(2).is_empty());
        let generation = readiness.generation();

        assert_eq!(readiness.mark_ready(generation), [2, 1, 2]);
    }

    #[test]
    fn a_bundle_without_a_ready_probe_stays_immediately_ready() {
        let mut readiness = RecoveryReadiness::initially_ready();

        assert_eq!(readiness.request_hook(3), [3]);
        assert_eq!(readiness.request_hook(4), [4]);
    }

    #[test]
    fn natural_restart_does_not_exempt_crashes_but_fault_recovery_survives_restarts() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted(false).unwrap();
        assert!(readiness.is_pending());
        assert!(!readiness.is_fault_pending());

        readiness.restarted(true).unwrap();
        let fault_generation = readiness.generation();
        readiness.restarted(false).unwrap();
        assert!(readiness.mark_ready(fault_generation).is_empty());
        assert!(readiness.is_fault_pending());
        readiness.mark_ready(readiness.generation());
        assert!(!readiness.is_fault_pending());
    }

    #[test]
    fn generation_exhaustion_does_not_reuse_a_stale_generation() {
        let mut readiness = RecoveryReadiness {
            generation: u64::MAX,
            ready: true,
            fault_pending: false,
            queued: Vec::new(),
        };

        assert_eq!(
            readiness.restarted(true),
            Err(RecoveryError::GenerationExhausted)
        );
        assert_eq!(readiness.generation(), u64::MAX);
        assert!(!readiness.is_pending());
    }
}
