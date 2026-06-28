// SPDX-License-Identifier: AGPL-3.0-or-later
//! The deterministic action queue shared by every engine.
//!
//! Actions are keyed by `(VTime, seq)`: `VTime` is the due time, `seq` is a
//! per-engine monotonic counter assigned at schedule time. The `BTreeMap` keeps
//! the queue in `(VTime, seq)` order, so [`due`](Scheduler::due) drains strictly
//! by due time with insertion order breaking ties — never by map-iteration or
//! hash order (conventions rule 4). This single queue is the only place ordering
//! is decided, so every [`FlowEngine`](crate::FlowEngine) inherits the same
//! deterministic drain.

use std::collections::BTreeMap;

use crate::{FlowAction, VTime};

/// A V-time-ordered queue of pending [`FlowAction`]s.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scheduler {
    /// Pending actions keyed by `(due V-time, insertion seq)`.
    actions: BTreeMap<(u64, u64), FlowAction>,
    /// Monotonic tie-breaker; assigned to each action as it is scheduled.
    next_seq: u64,
}

impl Scheduler {
    /// An empty queue.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Enqueue `action` at its own due V-time ([`FlowAction::at`]), tagged with
    /// the next sequence number so same-V-time ties drain in insertion order.
    pub(crate) fn schedule(&mut self, action: FlowAction) {
        let key = (action.at().0, self.next_seq);
        // 2^64 schedulings is unreachable from any event stream; wrap rather than
        // risk a debug-overflow panic (conventions rule 4 — never panic).
        self.next_seq = self.next_seq.wrapping_add(1);
        self.actions.insert(key, action);
    }

    /// Pop every action due at or before `now`, ascending by `(VTime, seq)`.
    /// Actions due after `now` stay queued.
    pub(crate) fn due(&mut self, now: VTime) -> Vec<FlowAction> {
        let mut out = Vec::new();
        while let Some((&(vt, _seq), _)) = self.actions.iter().next() {
            if vt > now.0 {
                break;
            }
            // `pop_first` removes the smallest key — i.e. the entry we just peeked
            // — so the loop drains in `(VTime, seq)` order and always terminates.
            if let Some((_, action)) = self.actions.pop_first() {
                out.push(action);
            }
        }
        out
    }
}
