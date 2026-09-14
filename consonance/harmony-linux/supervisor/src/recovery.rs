// SPDX-License-Identifier: AGPL-3.0-or-later

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RecoveryError {
    #[error("recovery generation exhausted")]
    GenerationExhausted,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryGate {
    generation: u64,
    ready: bool,
    queued: Vec<u32>,
}

impl RecoveryGate {
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
    use super::{RecoveryError, RecoveryGate};

    #[test]
    fn a_slow_probe_keeps_hooks_queued_until_the_current_generation_is_ready() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted().unwrap();
        let generation = gate.generation();

        assert!(gate.is_pending());
        assert!(gate.request_hook(7).is_empty());
        assert!(gate.request_hook(7).is_empty());
        assert!(gate.mark_ready(generation - 1).is_empty());
        assert!(gate.is_pending());

        assert_eq!(gate.mark_ready(generation), [7, 7]);
        assert!(!gate.is_pending());
    }

    #[test]
    fn a_stale_probe_cannot_release_hooks_after_a_new_restart() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted().unwrap();
        let stale = gate.generation();
        assert!(gate.request_hook(3).is_empty());

        gate.restarted().unwrap();
        let current = gate.generation();
        assert_ne!(current, stale);
        assert!(gate.mark_ready(stale).is_empty());
        assert!(gate.is_pending());
        assert_eq!(gate.mark_ready(current), [3]);
    }

    #[test]
    fn ready_hooks_preserve_request_order_and_duplicates() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted().unwrap();
        assert!(gate.request_hook(2).is_empty());
        assert!(gate.request_hook(1).is_empty());
        assert!(gate.request_hook(2).is_empty());
        let generation = gate.generation();

        assert_eq!(gate.mark_ready(generation), [2, 1, 2]);
    }

    #[test]
    fn a_bundle_without_a_ready_probe_stays_immediately_ready() {
        let mut gate = RecoveryGate::initially_ready();

        assert_eq!(gate.request_hook(3), [3]);
        assert_eq!(gate.request_hook(4), [4]);
    }

    #[test]
    fn generation_exhaustion_does_not_reuse_a_stale_generation() {
        let mut gate = RecoveryGate {
            generation: u64::MAX,
            ready: true,
            queued: Vec::new(),
        };

        assert_eq!(gate.restarted(), Err(RecoveryError::GenerationExhausted));
        assert_eq!(gate.generation(), u64::MAX);
        assert!(!gate.is_pending());
    }
}
