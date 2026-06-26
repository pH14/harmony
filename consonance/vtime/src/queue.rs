// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic V-time deadline queue: [`TimerQueue`].

use std::collections::BTreeMap;

use crate::error::VtimeError;

/// Caller-chosen identifier for a scheduled timer.
#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct TimerToken(pub u64);

#[derive(Debug, Clone, Copy)]
struct Entry {
    token: TimerToken,
    /// `None` for one-shots; `Some(period)` (period >= 1) for periodics.
    period: Option<u64>,
}

/// Deadline queue in V-time. Pure data structure, `BTreeMap`-based.
///
/// # Total order (determinism)
///
/// Pending timers are ordered by `(deadline_vns, insertion sequence)`: equal
/// deadlines fire in FIFO order of scheduling. Re-arming a periodic timer
/// (during [`TimerQueue::pop_due`]) and re-scheduling an existing token both
/// count as fresh insertions for tie-breaking purposes. This total order is
/// what makes firing sequences replayable.
///
/// # Token semantics
///
/// At most one pending entry exists per [`TimerToken`]: scheduling a token
/// that is already pending **replaces** the previous entry (and moves the
/// token to the back of its new deadline's FIFO class).
#[derive(Debug, Clone, Default)]
pub struct TimerQueue {
    /// `(deadline_vns, seq)` → entry; the BTreeMap order is the firing order.
    entries: BTreeMap<(u64, u64), Entry>,
    /// token → its key in `entries`, for O(log n) cancel/replace.
    index: BTreeMap<TimerToken, (u64, u64)>,
    next_seq: u64,
}

impl TimerQueue {
    /// Creates an empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules a one-shot timer at the given V-time deadline. If `token`
    /// is already pending, the previous entry is replaced.
    pub fn schedule_oneshot(&mut self, deadline_vns: u64, token: TimerToken) {
        self.insert(deadline_vns, token, None);
    }

    /// Schedules a periodic timer first firing at `first_vns`, then every
    /// `period_vns` nanoseconds of V-time (fixed cadence; see
    /// [`TimerQueue::pop_due`]). If `token` is already pending, the previous
    /// entry is replaced.
    ///
    /// # Errors
    ///
    /// [`VtimeError::ZeroPeriod`] if `period_vns == 0`.
    pub fn schedule_periodic(
        &mut self,
        first_vns: u64,
        period_vns: u64,
        token: TimerToken,
    ) -> Result<(), VtimeError> {
        if period_vns == 0 {
            return Err(VtimeError::ZeroPeriod);
        }
        self.insert(first_vns, token, Some(period_vns));
        Ok(())
    }

    /// Cancels the pending timer for `token`. Returns `true` if one was
    /// pending (periodic timers are removed entirely), `false` otherwise.
    pub fn cancel(&mut self, token: TimerToken) -> bool {
        match self.index.remove(&token) {
            Some(key) => {
                self.entries.remove(&key);
                true
            }
            None => false,
        }
    }

    /// Earliest pending deadline, if any (ties resolved by the FIFO order).
    pub fn peek_next(&self) -> Option<(u64, TimerToken)> {
        self.entries
            .first_key_value()
            .map(|(&(deadline, _), entry)| (deadline, entry.token))
    }

    /// Pops every deadline with `deadline_vns <= now_vns`, in the
    /// deterministic `(deadline, FIFO)` order, returning `(deadline, token)`
    /// pairs. Periodic timers are re-armed at `fired deadline + period` —
    /// fixed cadence, so firing times are exactly `first + k * period` with
    /// no drift accumulation even when popped late. A re-armed deadline that
    /// is still `<= now_vns` fires again in the same call (catch-up: a
    /// periodic popped `n` periods late returns `n + 1` firings).
    ///
    /// If re-arming overflows `u64` V-time (`deadline + period > u64::MAX`,
    /// i.e. beyond ~584 years of V-time), the periodic timer is dropped:
    /// its next deadline is unrepresentable.
    pub fn pop_due(&mut self, now_vns: u64) -> Vec<(u64, TimerToken)> {
        let mut fired = Vec::new();
        while let Some((&key, &entry)) = self.entries.first_key_value() {
            let (deadline, _seq) = key;
            if deadline > now_vns {
                break;
            }
            self.entries.remove(&key);
            self.index.remove(&entry.token);
            fired.push((deadline, entry.token));
            if let Some(period) = entry.period
                && let Some(next) = deadline.checked_add(period)
            {
                self.insert(next, entry.token, Some(period));
            }
        }
        fired
    }

    fn insert(&mut self, deadline_vns: u64, token: TimerToken, period: Option<u64>) {
        if let Some(old_key) = self.index.remove(&token) {
            self.entries.remove(&old_key);
        }
        let key = (deadline_vns, self.next_seq);
        self.next_seq += 1;
        self.entries.insert(key, Entry { token, period });
        self.index.insert(token, key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_queue() {
        let mut q = TimerQueue::new();
        assert_eq!(q.peek_next(), None);
        assert_eq!(q.pop_due(u64::MAX), vec![]);
        assert!(!q.cancel(TimerToken(1)));
    }

    #[test]
    fn oneshot_fires_once() {
        let mut q = TimerQueue::new();
        q.schedule_oneshot(100, TimerToken(1));
        assert_eq!(q.peek_next(), Some((100, TimerToken(1))));
        assert_eq!(q.pop_due(99), vec![]);
        assert_eq!(q.pop_due(100), vec![(100, TimerToken(1))]);
        assert_eq!(q.peek_next(), None);
    }

    #[test]
    fn reschedule_replaces() {
        let mut q = TimerQueue::new();
        q.schedule_oneshot(100, TimerToken(1));
        q.schedule_oneshot(200, TimerToken(1));
        assert_eq!(q.peek_next(), Some((200, TimerToken(1))));
        assert_eq!(q.pop_due(u64::MAX), vec![(200, TimerToken(1))]);
    }

    #[test]
    fn cancel_periodic() {
        let mut q = TimerQueue::new();
        q.schedule_periodic(10, 10, TimerToken(3)).unwrap();
        assert_eq!(
            q.pop_due(20),
            vec![(10, TimerToken(3)), (20, TimerToken(3))]
        );
        assert!(q.cancel(TimerToken(3)));
        assert!(!q.cancel(TimerToken(3)));
        assert_eq!(q.pop_due(u64::MAX), vec![]);
    }

    #[test]
    fn zero_period_rejected() {
        let mut q = TimerQueue::new();
        assert_eq!(
            q.schedule_periodic(10, 0, TimerToken(1)),
            Err(VtimeError::ZeroPeriod)
        );
        assert_eq!(q.peek_next(), None);
    }

    #[test]
    fn rearm_overflow_drops_timer() {
        let mut q = TimerQueue::new();
        q.schedule_periodic(u64::MAX - 10, 100, TimerToken(9))
            .unwrap();
        assert_eq!(q.pop_due(u64::MAX), vec![(u64::MAX - 10, TimerToken(9))]);
        assert_eq!(q.peek_next(), None);
    }

    #[test]
    fn catchup_interleaves_with_other_timers() {
        let mut q = TimerQueue::new();
        q.schedule_periodic(100, 100, TimerToken(1)).unwrap();
        q.schedule_oneshot(250, TimerToken(2));
        assert_eq!(
            q.pop_due(300),
            vec![
                (100, TimerToken(1)),
                (200, TimerToken(1)),
                (250, TimerToken(2)),
                (300, TimerToken(1)),
            ]
        );
        assert_eq!(q.peek_next(), Some((400, TimerToken(1))));
    }
}
