// SPDX-License-Identifier: AGPL-3.0-or-later
//! Versioned controller hold distributions shared by native NES adapters.
use crate::search::rand::RomuDuoJrRand;
use std::{error::Error, num::NonZeroUsize};

/// Ordinary controller holds; adapters retain their own special menu taps.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum NesDurationPolicy {
    /// Historical equally likely short and long bands.
    #[default]
    ShortOrLong,
    /// Equally likely short, middle and long bands, with the same draw cadence.
    ThreeBands,
    /// Two historical bands with the same expected hold as three bands.
    MeanMatchedTwoBands,
}

impl NesDurationPolicy {
    /// Stable workload-policy identity.
    #[must_use]
    pub fn identifier(self) -> &'static str {
        match self {
            Self::ShortOrLong => "stratified_short_or_long_v1",
            Self::ThreeBands => "stratified_short_middle_long_v2",
            Self::MeanMatchedTwoBands => "stratified_two_band_mean_matched_v1",
        }
    }

    /// Resolve only supported duration semantics.
    pub fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "stratified_short_or_long_v1" => Ok(Self::ShortOrLong),
            "stratified_short_middle_long_v2" => Ok(Self::ThreeBands),
            "stratified_two_band_mean_matched_v1" => Ok(Self::MeanMatchedTwoBands),
            _ => Err("unknown NES duration policy".into()),
        }
    }

    /// Draw exactly two random words, matching the historical sampler.
    pub fn sample(self, rand: &mut RomuDuoJrRand) -> Result<u8, Box<dyn Error>> {
        let bands = match self {
            Self::ShortOrLong => 2,
            Self::ThreeBands => 3,
            Self::MeanMatchedTwoBands => 231,
        };
        let band = rand.below(NonZeroUsize::new(bands).ok_or("empty duration bands")?);
        let (start, count) = match (self, band) {
            (Self::MeanMatchedTwoBands, 0..131) | (_, 0) => (2, 11),
            (Self::ThreeBands, 1) => (13, 35),
            _ => (48, 73),
        };
        Ok(u8::try_from(
            start + rand.below(NonZeroUsize::new(count).ok_or("empty duration band")?),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean_matched_control_excludes_middle_holds_and_preserves_draws() {
        // Band means are 7, 30, 84; the weighted two-band mean is 121/3.
        assert_eq!((131 * 7 + 100 * 84) * 3, 231 * (7 + 30 + 84));
        let mut control = RomuDuoJrRand::with_seed(907);
        let mut middle = control;
        let mut short = 0;
        for _ in 0..10_000 {
            let hold = NesDurationPolicy::MeanMatchedTwoBands
                .sample(&mut control)
                .unwrap();
            assert!((2..=12).contains(&hold) || (48..=120).contains(&hold));
            short += usize::from(hold <= 12);
            NesDurationPolicy::ThreeBands.sample(&mut middle).unwrap();
        }
        assert!((5400..5900).contains(&short));
        assert_eq!(control.next_u64(), middle.next_u64());
        for sampler in [
            crate::metroid::archive::sample_chord_with_duration,
            crate::mm2::archive::sample_chord_with_duration,
        ] {
            let mut control = RomuDuoJrRand::with_seed(907);
            let mut middle = control;
            for _ in 0..10_000 {
                let a = sampler(&mut control, NesDurationPolicy::MeanMatchedTwoBands).unwrap();
                let b = sampler(&mut middle, NesDurationPolicy::ThreeBands).unwrap();
                assert_eq!(a.buttons, b.buttons);
                if a.buttons == 4 || a.buttons == 8 {
                    assert_eq!(a.hold_frames, b.hold_frames);
                }
            }
            assert_eq!(control.next_u64(), middle.next_u64());
        }
    }

    #[test]
    fn legacy_draws_match_and_middle_band_keeps_rng_and_masks_aligned() {
        let mut historical = RomuDuoJrRand::with_seed(843);
        let mut legacy = historical;
        let mut middle = historical;
        let mut seen = [false; 121];
        for _ in 0..10_000 {
            let old = if historical.below(NonZeroUsize::new(2).unwrap()) == 0 {
                2 + historical.below(NonZeroUsize::new(11).unwrap())
            } else {
                48 + historical.below(NonZeroUsize::new(73).unwrap())
            };
            assert_eq!(
                usize::from(NesDurationPolicy::ShortOrLong.sample(&mut legacy).unwrap()),
                old
            );
            seen[usize::from(NesDurationPolicy::ThreeBands.sample(&mut middle).unwrap())] = true;
        }
        assert!(seen[2..=120].iter().all(|value| *value));
        assert_eq!(legacy.next_u64(), middle.next_u64());
        for sampler in [
            crate::metroid::archive::sample_chord_with_duration,
            crate::mm2::archive::sample_chord_with_duration,
        ] {
            let mut left = RomuDuoJrRand::with_seed(91);
            let mut right = left;
            let mut changed = 0;
            for _ in 0..10_000 {
                let a = sampler(&mut left, NesDurationPolicy::ShortOrLong).unwrap();
                let b = sampler(&mut right, NesDurationPolicy::ThreeBands).unwrap();
                assert_eq!(a.buttons, b.buttons);
                changed += usize::from(a.hold_frames != b.hold_frames);
                if a.buttons == 4 || a.buttons == 8 {
                    assert_eq!(a.hold_frames, b.hold_frames);
                }
            }
            assert!(changed > 2500);
            assert_eq!(left.next_u64(), right.next_u64());
        }
        for policy in [
            NesDurationPolicy::ShortOrLong,
            NesDurationPolicy::ThreeBands,
            NesDurationPolicy::MeanMatchedTwoBands,
        ] {
            assert_eq!(
                NesDurationPolicy::parse(policy.identifier()).unwrap(),
                policy
            );
        }
        assert!(NesDurationPolicy::parse("stratified_short_middle_long_v999").is_err());
    }
}
