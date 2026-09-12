// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic gating of hook launches while supervised nodes recover.

/// A hook whose launch has been authorized by the current ready generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyHook {
    pub id: u32,
    pub generation: u64,
}

/// Tracks the latest node generation and queues hooks until it is ready.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryGate {
    generation: u64,
    ready: bool,
    probing: bool,
    queued: Vec<u32>,
}

impl RecoveryGate {
    pub fn initially_ready() -> Self {
        Self {
            generation: 0,
            ready: true,
            probing: false,
            queued: Vec::new(),
        }
    }

    pub fn restarted(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.ready = false;
        self.probing = true;
    }

    /// Invalidate readiness while a node is down. Queued hooks remain queued
    /// for the generation that a later restart makes ready.
    pub fn stopped(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.ready = false;
        self.probing = false;
    }

    pub fn request_hook(&mut self, id: u32) -> Vec<ReadyHook> {
        if self.ready {
            vec![ReadyHook {
                id,
                generation: self.generation,
            }]
        } else {
            self.queued.push(id);
            Vec::new()
        }
    }

    pub fn mark_ready(&mut self, generation: u64) -> Vec<ReadyHook> {
        if generation != self.generation || self.ready || !self.probing {
            return Vec::new();
        }
        self.ready = true;
        self.probing = false;
        self.queued
            .drain(..)
            .map(|id| ReadyHook { id, generation })
            .collect()
    }

    pub fn is_pending(&self) -> bool {
        self.probing
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn accepts(&self, generation: u64) -> bool {
        self.ready && generation == self.generation
    }
}

#[cfg(test)]
mod tests {
    use super::{ReadyHook, RecoveryGate};

    #[test]
    fn only_the_latest_generation_can_release_queued_hooks() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted();
        assert!(gate.request_hook(7).is_empty());
        let stale = gate.generation();
        gate.restarted();
        assert!(gate.mark_ready(stale).is_empty());
        assert!(gate.is_pending());
        let current = gate.generation();
        assert_eq!(
            gate.mark_ready(current),
            [ReadyHook {
                id: 7,
                generation: current
            }]
        );
    }

    #[test]
    fn queued_hooks_keep_request_order_and_duplicates() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted();
        assert!(gate.request_hook(2).is_empty());
        assert!(gate.request_hook(1).is_empty());
        assert!(gate.request_hook(2).is_empty());
        let generation = gate.generation();
        assert_eq!(
            gate.mark_ready(generation),
            [
                ReadyHook { id: 2, generation },
                ReadyHook { id: 1, generation },
                ReadyHook { id: 2, generation },
            ]
        );
    }

    #[test]
    fn bundles_without_a_ready_command_remain_immediately_ready() {
        let mut gate = RecoveryGate::initially_ready();
        gate.restarted();
        let generation = gate.generation();
        assert!(gate.request_hook(3).is_empty());
        assert_eq!(
            gate.mark_ready(generation),
            [ReadyHook { id: 3, generation }]
        );
        assert!(gate.accepts(generation));
    }

    #[test]
    fn stopping_a_ready_generation_queues_hooks_until_a_restart() {
        let mut gate = RecoveryGate::initially_ready();
        gate.stopped();
        let stopped = gate.generation();
        assert!(gate.request_hook(2).is_empty());
        assert!(gate.mark_ready(stopped).is_empty());
        gate.restarted();
        assert_eq!(
            gate.mark_ready(gate.generation()),
            [ReadyHook {
                id: 2,
                generation: gate.generation()
            }]
        );
        assert_eq!(
            gate.request_hook(3),
            [ReadyHook {
                id: 3,
                generation: gate.generation()
            }]
        );
    }
}
