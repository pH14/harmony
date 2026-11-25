// SPDX-License-Identifier: AGPL-3.0-or-later

use std::collections::VecDeque;

#[derive(Debug)]
pub struct ConsoleTail {
    bytes: VecDeque<u8>,
    capacity: usize,
    total: u64,
    position: u64,
}

impl ConsoleTail {
    pub fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
            total: 0,
            position: 0,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        self.total = self.total.saturating_add(chunk.len() as u64);
        self.position = self.position.saturating_add(chunk.len() as u64);
        let kept = &chunk[chunk.len().saturating_sub(self.capacity)..];
        let overflow = (self.bytes.len() + kept.len()).saturating_sub(self.capacity);
        self.bytes.drain(..overflow);
        self.bytes.extend(kept);
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn position(&self) -> u64 {
        self.position
    }

    pub fn rewind(&mut self, position: u64) {
        self.bytes.clear();
        self.position = position;
    }

    pub fn bytes(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    #[must_use]
    pub fn since(&self, offset: u64) -> Vec<u8> {
        let after = usize::try_from(self.position.saturating_sub(offset)).unwrap_or(usize::MAX);
        let skip = self.bytes.len().saturating_sub(after);
        self.bytes.iter().skip(skip).copied().collect()
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
    fn since_returns_the_bytes_after_a_total_even_across_a_wrap() {
        let mut tail = ConsoleTail::new(8);
        tail.push(b"ok\nok\n");
        let mark = tail.position();
        tail.push(b"ok\n");
        assert_eq!(tail.since(mark), b"ok\n");
        tail.push(b"ok\nok\n");
        assert_eq!(tail.since(mark), b"k\nok\nok\n");
        assert_eq!(tail.since(tail.position()), b"");
        assert_eq!(tail.since(0), tail.bytes());
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

    #[test]
    fn rewinding_to_a_checkpoint_keeps_the_host_output_budget() {
        let mut tail = ConsoleTail::new(8);
        tail.push(b"boot\n");
        let checkpoint = tail.position();
        tail.push(b"old text");
        tail.rewind(checkpoint);
        assert!(tail.bytes().is_empty());
        assert_eq!(tail.total(), 13);
        assert_eq!(tail.position(), 5);
        tail.push(b"new");
        assert_eq!(tail.bytes(), b"new");
        assert_eq!(tail.total(), 16);
        assert_eq!(tail.position(), 8);
        assert_eq!(tail.since(checkpoint), b"new");
    }
}
