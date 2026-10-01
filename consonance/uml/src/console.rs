// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

#[derive(Debug)]
pub struct ConsoleTail {
    bytes: VecDeque<u8>,
    capacity: usize,
    total: u64,
}

impl ConsoleTail {
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
            total: 0,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        let kept = &chunk[chunk.len().saturating_sub(self.capacity)..];
        let overflow = (self.bytes.len() + kept.len()).saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(kept);
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    pub fn contains(&self, needle: &[u8]) -> bool {
        if needle.is_empty() {
            return true;
        }
        let (front, back) = self.bytes.as_slices();
        if front.windows(needle.len()).any(|window| window == needle)
            || back.windows(needle.len()).any(|window| window == needle)
        {
            return true;
        }
        let overlap = needle.len() - 1;
        let joined: Vec<u8> = front[front.len().saturating_sub(overlap)..]
            .iter()
            .chain(&back[..overlap.min(back.len())])
            .copied()
            .collect();
        joined.windows(needle.len()).any(|window| window == needle)
    }
}

#[cfg(test)]
mod tests {
    use super::ConsoleTail;

    #[test]
    fn keeps_only_the_newest_bytes() {
        let mut tail = ConsoleTail::new(4);
        tail.push(b"abc");
        tail.push(b"def");
        assert_eq!(tail.bytes(), b"cdef");
        assert_eq!(tail.total(), 6);
        tail.push(b"0123456789");
        assert_eq!(tail.bytes(), b"6789");
        assert_eq!(tail.total(), 16);
    }

    #[test]
    fn finds_text_across_the_ring_boundary() {
        let mut tail = ConsoleTail::new(6);
        tail.push(b"xxxxAB");
        tail.push(b"CD");
        assert!(tail.contains(b"ABCD"));
        assert!(tail.contains(b"xxAB"));
        assert!(!tail.contains(b"DA"));
    }

    #[test]
    fn zero_capacity_counts_without_keeping() {
        let mut tail = ConsoleTail::new(0);
        tail.push(b"abc");
        assert!(tail.bytes().is_empty());
        assert_eq!(tail.total(), 3);
    }
}
