// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;

pub struct Snap {
    base: Option<Arc<Snap>>,
    bytes: Vec<u8>,
    chain: usize,
    full_len: usize,
}

impl Snap {
    pub fn full(raw: &[u8]) -> Snap {
        let bytes = lz4_flex::compress_prepend_size(raw);
        let full_len = bytes.len();
        Snap {
            base: None,
            bytes,
            chain: 0,
            full_len,
        }
    }

    pub fn encode(raw: &[u8], start: &Arc<Snap>, start_raw: &[u8]) -> Snap {
        if raw.len() != start_raw.len() {
            return Snap::full(raw);
        }
        let xor: Vec<u8> = raw.iter().zip(start_raw).map(|(a, b)| a ^ b).collect();
        let bytes = lz4_flex::compress_prepend_size(&xor);
        let chain = start.chain + bytes.len();
        if chain > start.full_len {
            return Snap::full(raw);
        }
        Snap {
            base: Some(start.clone()),
            bytes,
            chain,
            full_len: start.full_len,
        }
    }

    pub fn raw(&self) -> Result<Vec<u8>, String> {
        let mut links = vec![self];
        while let Some(b) = &links[links.len() - 1].base {
            links.push(b);
        }
        let mut links = links.into_iter().rev();
        let root = links.next().ok_or("empty snapshot chain")?;
        let mut out = lz4_flex::decompress_size_prepended(&root.bytes)
            .map_err(|e| format!("snapshot: {e}"))?;
        for link in links {
            let xor = lz4_flex::decompress_size_prepended(&link.bytes)
                .map_err(|e| format!("snapshot: {e}"))?;
            if xor.len() != out.len() {
                return Err("snapshot delta length differs from its base".into());
            }
            for (o, x) in out.iter_mut().zip(xor) {
                *o ^= x;
            }
        }
        Ok(out)
    }

    pub fn stored_bytes(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_delta(&self) -> bool {
        self.base.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(seed: u8, len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(31) ^ seed)
            .collect()
    }

    #[test]
    fn deltas_rebuild_the_exact_state_through_a_chain() {
        let root_raw = state(7, 4096);
        let root = Arc::new(Snap::full(&root_raw));
        assert_eq!(root.raw().unwrap(), root_raw);
        let mut prev = root.clone();
        let mut prev_raw = root_raw.clone();
        for step in 0..5u8 {
            let mut next = prev_raw.clone();
            next[100 + usize::from(step)] ^= 0x5a;
            next[3000] = step;
            let s = Arc::new(Snap::encode(&next, &prev, &prev_raw));
            assert!(s.is_delta());
            assert!(s.stored_bytes() < root.stored_bytes());
            assert_eq!(s.raw().unwrap(), next);
            prev = s;
            prev_raw = next;
        }
    }

    #[test]
    fn chain_restarts_with_a_full_state_once_deltas_outweigh_it() {
        let root_raw = vec![0u8; 256];
        let root = Arc::new(Snap::full(&root_raw));
        let mut prev = root;
        let mut prev_raw = root_raw;
        let mut fulls = 0;
        for step in 0..200u32 {
            let next: Vec<u8> = (0..256u32)
                .map(|i| (i.wrapping_mul(step + 13) >> 3) as u8)
                .collect();
            let s = Arc::new(Snap::encode(&next, &prev, &prev_raw));
            if !s.is_delta() {
                fulls += 1;
            }
            assert!(s.chain <= s.full_len);
            assert_eq!(s.raw().unwrap(), next);
            prev = s;
            prev_raw = next;
        }
        assert!(fulls > 0);
        let other = Snap::encode(&[1, 2, 3], &prev, &prev_raw);
        assert!(!other.is_delta());
        assert_eq!(other.raw().unwrap(), vec![1, 2, 3]);
    }
}
