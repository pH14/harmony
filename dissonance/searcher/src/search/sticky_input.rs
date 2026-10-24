// SPDX-License-Identifier: AGPL-3.0-or-later

use std::num::{NonZeroU64, NonZeroUsize};

use super::rand::RomuDuoJrRand;

pub struct StickyInput {
    random: RomuDuoJrRand,
    buttons: u8,
    mask: u8,
    denominator: NonZeroU64,
}

impl StickyInput {
    pub fn new(seed: u64, previous: u8, mask: u8, longest_hold: NonZeroU64) -> Self {
        let mut random = RomuDuoJrRand::with_seed(seed);
        let scales = u64::BITS - longest_hold.get().leading_zeros();
        let exponent = random.below(NonZeroUsize::new(scales as usize).unwrap());
        Self {
            random,
            buttons: previous,
            mask,
            denominator: NonZeroU64::new(1_u64 << exponent).unwrap(),
        }
    }

    pub fn flip_denominator(&self) -> u64 {
        self.denominator.get()
    }

    pub fn next_frame(&mut self) -> u8 {
        let current = self.buttons;
        for bit in 0..u8::BITS {
            let mask = 1_u8 << bit;
            if self.mask & mask != 0 && self.random.below_u64(self.denominator) == 0 {
                self.buttons ^= mask;
            }
        }
        current
    }
}

#[cfg(test)]
mod tests {
    use super::StickyInput;
    use std::num::NonZeroU64;

    #[test]
    fn preserves_initial_byte_and_changes_only_enabled_buttons() {
        let mut input = StickyInput::new(17, 0b1010_0000, 0b0000_0011, NonZeroU64::MIN);
        assert_eq!(input.next_frame(), 0b1010_0000);
        assert_eq!(input.next_frame(), 0b1010_0011);
        assert_eq!(input.next_frame(), 0b1010_0000);
    }

    #[test]
    fn seeded_frames_are_identical_across_block_boundaries() {
        let mut continuous = StickyInput::new(43, 0x81, 0xff, NonZeroU64::new(120).unwrap());
        let expected: Vec<_> = (0..4096).map(|_| continuous.next_frame()).collect();
        let mut blocked = StickyInput::new(43, 0x81, 0xff, NonZeroU64::new(120).unwrap());
        let actual: Vec<_> = [17, 103, 1, 3975]
            .into_iter()
            .flat_map(|length| {
                (0..length)
                    .map(|_| blocked.next_frame())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(expected, actual);
    }

    #[test]
    fn holds_can_exceed_the_duration_support() {
        let longest = NonZeroU64::new(120).unwrap();
        let mut longest_observed = 0;
        let mut rates = std::collections::BTreeSet::new();
        for seed in 0..64 {
            let mut input = StickyInput::new(seed, 0, 1, longest);
            rates.insert(input.flip_denominator());
            let mut previous = None;
            let mut held = 0;
            for _ in 0..4096 {
                let current = input.next_frame();
                held = if previous == Some(current) {
                    held + 1
                } else {
                    1
                };
                previous = Some(current);
                longest_observed = longest_observed.max(held);
            }
        }
        assert_eq!(rates, [1, 2, 4, 8, 16, 32, 64].into_iter().collect());
        assert!(
            longest_observed > 120,
            "longest observed hold: {longest_observed}"
        );
    }
}
