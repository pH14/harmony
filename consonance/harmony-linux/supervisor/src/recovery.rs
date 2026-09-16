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
    queued: Vec<u32>,
}

impl RecoveryReadiness {
    #[must_use]
    pub fn initially_ready() -> Self {
        Self {
            generation: 0,
            ready: true,
            queued: Vec::new(),
        }
    }

    pub fn restarted(&mut self) -> Result<(), RecoveryError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(RecoveryError::GenerationExhausted)?;
        self.ready = false;
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
        self.queued.drain(..).collect()
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        !self.ready
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::{RecoveryError, RecoveryReadiness};

    #[test]
    fn a_slow_probe_keeps_hooks_queued_until_the_current_generation_is_ready() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted().unwrap();
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
        readiness.restarted().unwrap();
        let stale = readiness.generation();
        assert!(readiness.request_hook(3).is_empty());

        readiness.restarted().unwrap();
        let current = readiness.generation();
        assert_ne!(current, stale);
        assert!(readiness.mark_ready(stale).is_empty());
        assert!(readiness.is_pending());
        assert_eq!(readiness.mark_ready(current), [3]);
    }

    #[test]
    fn ready_hooks_preserve_request_order_and_duplicates() {
        let mut readiness = RecoveryReadiness::initially_ready();
        readiness.restarted().unwrap();
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
    fn generation_exhaustion_does_not_reuse_a_stale_generation() {
        let mut readiness = RecoveryReadiness {
            generation: u64::MAX,
            ready: true,
            queued: Vec::new(),
        };

        assert_eq!(
            readiness.restarted(),
            Err(RecoveryError::GenerationExhausted)
        );
        assert_eq!(readiness.generation(), u64::MAX);
        assert!(!readiness.is_pending());
    }
}
