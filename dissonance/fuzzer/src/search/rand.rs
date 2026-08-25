// SPDX-License-Identifier: AGPL-3.0-or-later

//! RomuDuoJr pseudo-random generator with splitmix64 seeding.
//!
//! Every recorded stream's draws come from this generator, so its output must
//! stay draw-for-draw identical to the generator the recordings were made
//! with. Algorithms: RomuDuoJr from
//! <https://arxiv.org/pdf/2002.11331> and splitmix64 from
//! <https://prng.di.unimi.it/splitmix64.c>; bounded draws use the 128-bit
//! multiply-shift reduction, not modulo.

use std::num::NonZeroUsize;

fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// Deterministic draw source for selection, mutation, and suffix derivation.
#[derive(Clone, Copy, Debug)]
pub struct RomuDuoJrRand {
    x_state: u64,
    y_state: u64,
}

impl RomuDuoJrRand {
    /// Seed both state words through splitmix64.
    #[must_use]
    pub fn with_seed(mut seed: u64) -> Self {
        Self {
            x_state: splitmix64(&mut seed),
            y_state: splitmix64(&mut seed),
        }
    }

    /// Next 64-bit draw.
    #[expect(clippy::unreadable_literal)]
    pub fn next_u64(&mut self) -> u64 {
        let xp = self.x_state;
        self.x_state = 15241094284759029579_u64.wrapping_mul(self.y_state);
        self.y_state = self.y_state.wrapping_sub(xp).rotate_left(27);
        xp
    }

    /// Draw below the exclusive bound via the multiply-shift reduction.
    pub fn below(&mut self, upper_bound_excl: NonZeroUsize) -> usize {
        let mul =
            u128::from(self.next_u64()).wrapping_mul(u128::from(upper_bound_excl.get() as u64));
        (mul >> 64) as usize
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use super::RomuDuoJrRand;

    // Reference draws captured from libafl_bolts 0.15.4 StdRand; recorded
    // streams depend on this exact sequence.
    #[test]
    fn sequence_matches_the_recorded_generator() {
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_0903);
        let first: Vec<u64> = (0..4).map(|_| rand.next_u64()).collect();
        let mut again = RomuDuoJrRand::with_seed(0x5eed_0903);
        let bound = NonZeroUsize::new(97).expect("nonzero bound");
        let bounded: Vec<usize> = (0..4).map(|_| again.below(bound)).collect();
        assert_eq!(first, REFERENCE_NEXT);
        assert_eq!(bounded, REFERENCE_BELOW_97);
    }

    const REFERENCE_NEXT: [u64; 4] = [
        12213431933298621606,
        3036258437508424578,
        10882873578750246974,
        3543664003024287084,
    ];
    const REFERENCE_BELOW_97: [usize; 4] = [64, 15, 57, 18];
}
