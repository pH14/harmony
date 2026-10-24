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

    pub fn block(&mut self, frames: usize, chunk_limit: NonZeroU64) -> Vec<(u8, u64)> {
        let mut output: Vec<(u8, u64)> = Vec::new();
        for _ in 0..frames {
            let buttons = self.next_frame();
            if let Some((previous, length)) = output.last_mut()
                && *previous == buttons
                && *length < chunk_limit.get()
            {
                *length += 1;
            } else {
                output.push((buttons, 1));
            }
        }
        output
    }

    pub fn block_with(
        &mut self,
        frames: usize,
        chunk_limit: NonZeroU64,
        mut step: impl FnMut(&mut RomuDuoJrRand, u8) -> Result<u8, Box<dyn std::error::Error>>,
    ) -> Result<Vec<(u8, u64)>, Box<dyn std::error::Error>> {
        let mut output: Vec<(u8, u64)> = Vec::new();
        for _ in 0..frames {
            let buttons = self.buttons;
            if self.random.below_u64(self.denominator) == 0 {
                self.buttons = step(&mut self.random, self.buttons)?;
            }
            if let Some((previous, length)) = output.last_mut()
                && *previous == buttons
                && *length < chunk_limit.get()
            {
                *length += 1;
            } else {
                output.push((buttons, 1));
            }
        }
        Ok(output)
    }

    pub fn next_frame(&mut self) -> u8 {
        let current = self.buttons;
        if self.random.below_u64(self.denominator) == 0 {
            let choices = self.mask.count_ones() as usize + 1;
            let chosen = self.random.below(NonZeroUsize::new(choices).unwrap());
            if let Some(bit) = (0..u8::BITS)
                .filter(|bit| self.mask & (1 << bit) != 0)
                .nth(chosen)
            {
                self.buttons ^= 1 << bit;
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
        let mut previous = 0b1010_0000;
        for _ in 0..4096 {
            let next = input.next_frame();
            assert_eq!(next & !3, 0b1010_0000);
            assert!((next ^ previous).count_ones() <= 1);
            previous = next;
        }
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
    fn adapter_transition_preserves_control_groups_across_chunks() {
        fn step(
            random: &mut super::RomuDuoJrRand,
            previous: u8,
        ) -> Result<u8, Box<dyn std::error::Error>> {
            Ok(
                match random.below(std::num::NonZeroUsize::new(3).unwrap()) {
                    0 => previous ^ 1,
                    1 => previous ^ 2,
                    _ => previous,
                },
            )
        }
        let mut a = StickyInput::new(51, 0, 3, NonZeroU64::new(120).unwrap());
        let mut b = StickyInput::new(51, 0, 3, NonZeroU64::new(120).unwrap());
        let expand = |chunks: Vec<(u8, u64)>| {
            chunks
                .into_iter()
                .flat_map(|(buttons, n)| std::iter::repeat_n(buttons, n as usize))
                .collect::<Vec<_>>()
        };
        let whole = expand(
            a.block_with(4096, NonZeroU64::new(120).unwrap(), step)
                .unwrap(),
        );
        let mut parts = Vec::new();
        for n in [17, 103, 1, 3975] {
            parts.extend(expand(
                b.block_with(n, NonZeroU64::new(64).unwrap(), step).unwrap(),
            ));
        }
        assert_eq!(whole, parts);
        assert!(
            whole
                .windows(2)
                .all(|pair| (pair[0] ^ pair[1]).count_ones() <= 1)
        );
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
