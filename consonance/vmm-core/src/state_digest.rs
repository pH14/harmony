// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::host_telemetry::HashWork;

pub(crate) const PAGE_SIZE: usize = 4096;
const MEMORY_DOMAIN: &[u8] = b"consonance-state-memory-pages-v2\0";
const STATE_DOMAIN: &[u8] = b"consonance-state-v2\0";

fn zero_page(page: &[u8]) -> bool {
    page.chunks_exact(16).fold(0u128, |acc, word| {
        acc | u128::from_ne_bytes(word.try_into().unwrap())
    }) == 0
}

fn zero_page_digest() -> [u8; 32] {
    static DIGEST: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();
    *DIGEST.get_or_init(|| *blake3::hash(&[0u8; PAGE_SIZE]).as_bytes())
}

fn page_digest(page: &[u8], work: &mut HashWork) -> [u8; 32] {
    if page.len() == PAGE_SIZE && zero_page(page) {
        return zero_page_digest();
    }
    work.ram(page.len());
    *blake3::hash(page).as_bytes()
}

fn combine(memory_len: usize, digests: &[[u8; 32]], work: &mut HashWork) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(MEMORY_DOMAIN);
    hasher.update(&(memory_len as u64).to_le_bytes());
    work.metadata(MEMORY_DOMAIN.len() + 8);
    hasher.update(digests.as_flattened());
    work.metadata(digests.as_flattened().len());
    *hasher.finalize().as_bytes()
}

pub(crate) fn memory_digest(memory: &[u8], work: &mut HashWork) -> [u8; 32] {
    let digests = memory
        .chunks(PAGE_SIZE)
        .map(|page| page_digest(page, work))
        .collect::<Vec<_>>();
    combine(memory.len(), &digests, work)
}

pub(crate) fn state_hash(memory: &[u8; 32], suffix: &[u8], work: &mut HashWork) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(STATE_DOMAIN);
    hasher.update(memory);
    hasher.update(suffix);
    work.metadata(STATE_DOMAIN.len() + memory.len());
    work.suffix(suffix.len());
    *hasher.finalize().as_bytes()
}

pub fn memory_digest_of(memory: &[u8]) -> [u8; 32] {
    memory_digest(memory, &mut HashWork::new())
}

pub fn state_hash_of(memory_digest: &[u8; 32], suffix: &[u8]) -> [u8; 32] {
    state_hash(memory_digest, suffix, &mut HashWork::new())
}

#[cfg(test)]
pub(crate) fn hashed_page_bytes(memory: &[u8]) -> u64 {
    memory
        .chunks(PAGE_SIZE)
        .filter(|page| page.len() != PAGE_SIZE || !zero_page(page))
        .map(|page| page.len() as u64)
        .sum()
}

#[cfg(test)]
pub(crate) fn metadata_bytes(memory_len: usize) -> u64 {
    (MEMORY_DOMAIN.len() + 8 + 32 * memory_len.div_ceil(PAGE_SIZE) + STATE_DOMAIN.len() + 32) as u64
}

pub(crate) struct PageDigests {
    digests: Vec<[u8; 32]>,
    stale: Vec<u64>,
}

impl PageDigests {
    pub(crate) fn new(memory_len: usize) -> Self {
        let pages = memory_len.div_ceil(PAGE_SIZE);
        let mut digests = Self {
            digests: vec![[0; 32]; pages],
            stale: vec![0; pages.div_ceil(64)],
        };
        digests.mark_all();
        digests
    }

    pub(crate) fn mark(&mut self, page: u64) {
        if let Ok(page) = usize::try_from(page)
            && page < self.digests.len()
        {
            self.stale[page / 64] |= 1 << (page % 64);
        }
    }

    pub(crate) fn set(&mut self, page: u64, digest: [u8; 32]) {
        if let Ok(page) = usize::try_from(page)
            && let Some(slot) = self.digests.get_mut(page)
        {
            *slot = digest;
            self.stale[page / 64] &= !(1 << (page % 64));
        }
    }

    pub(crate) fn mark_range(&mut self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let first = offset / PAGE_SIZE;
        let last = offset.saturating_add(len - 1) / PAGE_SIZE;
        for page in first..=last.min(self.digests.len().saturating_sub(1)) {
            self.stale[page / 64] |= 1 << (page % 64);
        }
    }

    pub(crate) fn mark_all(&mut self) {
        self.stale.fill(u64::MAX);
    }

    pub(crate) fn digest(&mut self, memory: &[u8], work: &mut HashWork) -> [u8; 32] {
        if memory.len().div_ceil(PAGE_SIZE) != self.digests.len() {
            *self = Self::new(memory.len());
        }
        for (index, word) in self.stale.iter_mut().enumerate() {
            while *word != 0 {
                let page = index * 64 + word.trailing_zeros() as usize;
                *word &= *word - 1;
                if let Some(digest) = self.digests.get_mut(page) {
                    let start = page * PAGE_SIZE;
                    let end = memory.len().min(start + PAGE_SIZE);
                    *digest = page_digest(&memory[start..end], work);
                }
            }
        }
        combine(memory.len(), &self.digests, work)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory(pages: usize) -> Vec<u8> {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut bytes = vec![0u8; pages * PAGE_SIZE];
        for page in (0..pages).step_by(3) {
            for byte in &mut bytes[page * PAGE_SIZE..page * PAGE_SIZE + 64] {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                *byte = seed as u8;
            }
        }
        bytes
    }

    #[test]
    fn zero_pages_use_the_digest_of_a_zero_page() {
        let mut work = HashWork::new();
        assert_eq!(
            page_digest(&[0; PAGE_SIZE], &mut work),
            *blake3::hash(&[0; PAGE_SIZE]).as_bytes()
        );
        let mut page = [0u8; PAGE_SIZE];
        page[PAGE_SIZE - 1] = 1;
        assert_eq!(
            page_digest(&page, &mut work),
            *blake3::hash(&page).as_bytes()
        );
    }

    #[test]
    fn incremental_digest_matches_a_full_digest_after_marked_writes() {
        let mut bytes = memory(130);
        let mut digests = PageDigests::new(bytes.len());
        let mut work = HashWork::new();
        assert_eq!(
            digests.digest(&bytes, &mut work),
            memory_digest(&bytes, &mut work)
        );
        bytes[5 * PAGE_SIZE + 7] ^= 1;
        digests.mark(5);
        bytes[63 * PAGE_SIZE + PAGE_SIZE - 1] ^= 2;
        bytes[64 * PAGE_SIZE] ^= 2;
        digests.mark_range(64 * PAGE_SIZE - 1, 2);
        bytes[129 * PAGE_SIZE + 9] ^= 4;
        digests.mark(129);
        digests.mark(10_000);
        assert_eq!(
            digests.digest(&bytes, &mut work),
            memory_digest(&bytes, &mut work)
        );
    }

    #[test]
    fn an_unmarked_write_is_not_observed_until_the_page_is_marked() {
        let mut bytes = memory(4);
        let mut digests = PageDigests::new(bytes.len());
        let mut work = HashWork::new();
        let before = digests.digest(&bytes, &mut work);
        bytes[2 * PAGE_SIZE] ^= 1;
        assert_eq!(digests.digest(&bytes, &mut work), before);
        digests.mark_all();
        assert_ne!(digests.digest(&bytes, &mut work), before);
        assert_eq!(
            digests.digest(&bytes, &mut work),
            memory_digest(&bytes, &mut work)
        );
    }

    #[test]
    fn memory_length_page_order_and_suffix_change_the_state_hash() {
        let mut work = HashWork::new();
        let mut first = memory(3);
        let base = memory_digest(&first, &mut work);
        assert_ne!(memory_digest(&first[..2 * PAGE_SIZE], &mut work), base);
        first.swap(0, PAGE_SIZE);
        assert_ne!(memory_digest(&first, &mut work), base);
        assert_ne!(
            state_hash(&base, b"a", &mut work),
            state_hash(&base, b"b", &mut work)
        );
        let zero = vec![0u8; 2 * PAGE_SIZE];
        assert_ne!(
            memory_digest(&zero, &mut work),
            memory_digest(&zero[..PAGE_SIZE], &mut work)
        );
    }

    #[test]
    fn hashed_bytes_count_only_nonzero_pages_that_were_rehashed() {
        let bytes = memory(6);
        let mut digests = PageDigests::new(bytes.len());
        let mut work = HashWork::new();
        digests.digest(&bytes, &mut work);
        assert_eq!(work.ram_bytes(), 2 * PAGE_SIZE as u64);
        let mut work = HashWork::new();
        digests.mark(0);
        digests.mark(1);
        digests.digest(&bytes, &mut work);
        assert_eq!(work.ram_bytes(), PAGE_SIZE as u64);
    }
}
