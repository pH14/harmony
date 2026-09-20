// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

pub(crate) const ROUTE_ATTEMPTS_CAPACITY: usize = 4_096;
pub(crate) const ROUTE_ATTEMPTS_PAYLOAD_BYTES: usize = 1 << 20;
pub(crate) const ROUTE_ATTEMPTS_MEMORY_RESERVE_BYTES: usize = 2 << 20;

#[derive(Clone, Debug, Eq, PartialEq)]
struct RouteAttemptKey {
    parent_id: u64,
    donor_id: u64,
    leaf_id: u64,
    effective_cap: usize,
    tail: Box<[u8]>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct RouteAttempts {
    entries: VecDeque<RouteAttemptKey>,
    payload_bytes: usize,
}

impl RouteAttempts {
    pub(crate) const fn new() -> Self {
        Self {
            entries: VecDeque::new(),
            payload_bytes: 0,
        }
    }

    pub(crate) fn repeated(
        &mut self,
        parent_id: u64,
        donor_id: u64,
        leaf_id: u64,
        effective_cap: usize,
        tail: &[u8],
    ) -> bool {
        if tail.len() > ROUTE_ATTEMPTS_PAYLOAD_BYTES {
            return false;
        }
        if self.entries.iter().any(|entry| {
            entry.parent_id == parent_id
                && entry.donor_id == donor_id
                && entry.leaf_id == leaf_id
                && entry.effective_cap == effective_cap
                && entry.tail.as_ref() == tail
        }) {
            return true;
        }
        while self.entries.len() >= ROUTE_ATTEMPTS_CAPACITY
            || self.payload_bytes.saturating_add(tail.len()) > ROUTE_ATTEMPTS_PAYLOAD_BYTES
        {
            let Some(evicted) = self.entries.pop_front() else {
                break;
            };
            self.payload_bytes = self.payload_bytes.saturating_sub(evicted.tail.len());
        }
        self.payload_bytes = self.payload_bytes.saturating_add(tail.len());
        self.entries.push_back(RouteAttemptKey {
            parent_id,
            donor_id,
            leaf_id,
            effective_cap,
            tail: tail.to_vec().into_boxed_slice(),
        });
        false
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    fn payload_bytes(&self) -> usize {
        self.payload_bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{ROUTE_ATTEMPTS_CAPACITY, ROUTE_ATTEMPTS_PAYLOAD_BYTES, RouteAttempts};

    #[test]
    fn identical_attempts_are_suppressed_without_refreshing_fifo_order() {
        let mut attempts = RouteAttempts::new();
        assert!(!attempts.repeated(1, 2, 3, 8, &[4, 5]));
        assert!(attempts.repeated(1, 2, 3, 8, &[4, 5]));
        assert!(!attempts.repeated(9, 2, 3, 8, &[4, 5]));
        assert_eq!(attempts.len(), 2);
    }

    #[test]
    fn cap_and_tail_are_part_of_attempt_identity() {
        let mut attempts = RouteAttempts::new();
        assert!(!attempts.repeated(1, 2, 3, 8, &[4]));
        assert!(!attempts.repeated(1, 2, 3, 9, &[4]));
        assert!(!attempts.repeated(1, 2, 3, 8, &[5]));
        assert_eq!(attempts.len(), 3);
    }

    #[test]
    fn fifo_eviction_permits_an_old_attempt_again() {
        let mut attempts = RouteAttempts::new();
        for index in 0..ROUTE_ATTEMPTS_CAPACITY {
            assert!(!attempts.repeated(index as u64, 2, 3, 8, &[4]));
        }
        assert!(attempts.repeated(0, 2, 3, 8, &[4]));
        assert!(!attempts.repeated(ROUTE_ATTEMPTS_CAPACITY as u64, 2, 3, 8, &[4]));
        assert!(!attempts.repeated(0, 2, 3, 8, &[4]));
        assert_eq!(attempts.len(), ROUTE_ATTEMPTS_CAPACITY);
    }

    #[test]
    fn payload_bound_evicts_oldest_entries() {
        let mut attempts = RouteAttempts::new();
        let payload = vec![7; ROUTE_ATTEMPTS_PAYLOAD_BYTES / 3 + 1];
        assert!(!attempts.repeated(1, 2, 3, 8, &payload));
        assert!(!attempts.repeated(4, 5, 6, 8, &payload));
        assert!(!attempts.repeated(7, 8, 9, 8, &payload));
        assert!(attempts.payload_bytes() <= ROUTE_ATTEMPTS_PAYLOAD_BYTES);
        assert!(!attempts.repeated(1, 2, 3, 8, &payload));
    }

    #[test]
    fn oversized_payload_bypasses_cache() {
        let mut attempts = RouteAttempts::new();
        let payload = vec![7; ROUTE_ATTEMPTS_PAYLOAD_BYTES + 1];
        assert!(!attempts.repeated(1, 2, 3, 8, &payload));
        assert!(!attempts.repeated(1, 2, 3, 8, &payload));
        assert_eq!(attempts.len(), 0);
        assert_eq!(attempts.payload_bytes(), 0);
    }
}
