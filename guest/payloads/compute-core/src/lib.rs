//! The `compute` payload's deterministic integer workload, shared verbatim
//! between the bare-metal payload and a host-side test so the expected digest
//! in `guest/golden/compute.txt` is derived from the same code that runs in
//! the guest. Pure integer arithmetic, all wrapping: the result depends only
//! on the constants below, never on timing, addresses or the environment.
#![cfg_attr(not(test), no_std)]

/// PRNG seed (fixed by the task spec).
pub const SEED: u64 = 0x5EED_5EED_5EED_5EED;
/// Number of workload iterations.
pub const ITERATIONS: u64 = 10_000_000;
/// Scratch buffer size: 1 MiB.
pub const SCRATCH_LEN: usize = 1 << 20;

/// xorshift64* (Vigna, "An experimental exploration of Marsaglia's xorshift
/// generators, scrambled"): shifts 12/25/27, scrambler multiplier
/// 0x2545F4914F6CDD1D. State must be nonzero.
pub struct XorShift64Star(u64);

impl XorShift64Star {
    /// Create a generator from a nonzero seed.
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64-bit output.
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Run the workload over a zeroed 1 MiB scratch buffer and return the digest.
///
/// Eight u64 registers are seeded from the PRNG; each iteration draws one
/// PRNG value `r` and uses its bits to pick a register, an 8-byte-aligned
/// scratch offset and one of four add/xor/rotate mixes of the register with
/// the scratch word, then writes a mix of two registers back to the same
/// offset. The digest folds the final registers with the same scrambler
/// multiplier.
pub fn run(scratch: &mut [u8; SCRATCH_LEN]) -> u64 {
    let mut rng = XorShift64Star::new(SEED);
    let mut regs = [0u64; 8];
    for r in regs.iter_mut() {
        *r = rng.next_u64();
    }

    for _ in 0..ITERATIONS {
        let r = rng.next_u64();
        let k = (r & 7) as usize;
        let off = ((r >> 16) as usize % (SCRATCH_LEN / 8)) * 8;
        // Aligned, in-bounds by construction; indexing cannot panic.
        let word = u64::from_le_bytes([
            scratch[off],
            scratch[off + 1],
            scratch[off + 2],
            scratch[off + 3],
            scratch[off + 4],
            scratch[off + 5],
            scratch[off + 6],
            scratch[off + 7],
        ]);
        match (r >> 3) & 3 {
            0 => regs[k] = regs[k].wrapping_add(word ^ r),
            1 => regs[k] ^= word.wrapping_add(r),
            2 => regs[k] = regs[k].rotate_left(((r >> 5) & 63) as u32) ^ word,
            _ => regs[k] = regs[k].wrapping_add(r).rotate_right(((r >> 5) & 63) as u32),
        }
        let out = regs[k] ^ regs[(k + 1) & 7];
        scratch[off..off + 8].copy_from_slice(&out.to_le_bytes());
    }

    let mut digest = SEED;
    for r in regs {
        digest = (digest ^ r)
            .wrapping_mul(0x2545_F491_4F6C_DD1D)
            .rotate_left(27);
    }
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// First outputs of xorshift64* from our seed, computed independently
    /// (pinned here so an accidental algorithm edit fails loudly).
    #[test]
    fn xorshift_first_outputs_are_stable() {
        let mut rng = XorShift64Star::new(SEED);
        let a = rng.next_u64();
        let b = rng.next_u64();
        assert_ne!(a, 0);
        assert_ne!(a, b);
        // Re-derive by hand: one round of shifts then the scrambler multiply.
        let mut x = SEED;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        assert_eq!(a, x.wrapping_mul(0x2545_F491_4F6C_DD1D));
    }

    /// The digest the payload must print, checked against the committed
    /// golden output byte for byte. This is the host half of the
    /// "same work => same state" contract.
    #[test]
    fn digest_matches_golden() {
        let mut scratch = vec![0u8; SCRATCH_LEN].into_boxed_slice();
        let scratch: &mut [u8; SCRATCH_LEN] = (&mut *scratch).try_into().unwrap();
        let digest = run(scratch);
        let golden = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../golden/compute.txt"
        ))
        .expect("guest/golden/compute.txt must exist");
        let line = golden
            .lines()
            .find(|l| l.starts_with("DIGEST "))
            .expect("golden compute output must contain a DIGEST line");
        assert_eq!(line, format!("DIGEST {digest:016x}"));
    }
}
