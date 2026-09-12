// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::BTreeMap;

use crate::error::VtimeError;

#[derive(Copy, Clone, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct TimerToken(pub u64);

#[derive(Debug, Clone, Copy)]
struct Entry {
    token: TimerToken,
    period: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct TimerQueue {
    entries: BTreeMap<(u64, u64), Entry>,
    index: BTreeMap<TimerToken, (u64, u64)>,
    next_seq: u64,
}

impl TimerQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn schedule_oneshot(&mut self, deadline_vns: u64, token: TimerToken) {
        self.insert(deadline_vns, token, None);
    }

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

    pub fn cancel(&mut self, token: TimerToken) -> bool {
        match self.index.remove(&token) {
            Some(key) => {
                self.entries.remove(&key);
                true
            }
            None => false,
        }
    }

    pub fn peek_next(&self) -> Option<(u64, TimerToken)> {
        self.entries
            .first_key_value()
            .map(|(&(deadline, _), entry)| (deadline, entry.token))
    }

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
