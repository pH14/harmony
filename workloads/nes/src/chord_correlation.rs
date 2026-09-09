// SPDX-License-Identifier: AGPL-3.0-or-later
//! Explicit suffix-local controller correlation; no game observations or routes.

use crate::search::rand::RomuDuoJrRand;
use machine::nes::ButtonChord;
use sha2::{Digest, Sha256};
use std::{error::Error, num::NonZeroUsize};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ChordCorrelation {
    #[default]
    Independent,
    ComponentHalf,
    MatchedWholeRepeat,
}

impl ChordCorrelation {
    #[must_use]
    pub fn identifier(self) -> &'static str {
        match self {
            Self::Independent => "independent_v1",
            Self::ComponentHalf => "component_refresh_half_v1",
            Self::MatchedWholeRepeat => "whole_repeat_37_of_210_v1",
        }
    }

    pub fn parse(value: &str) -> Result<Self, Box<dyn Error>> {
        match value {
            "independent_v1" => Ok(Self::Independent),
            "component_refresh_half_v1" => Ok(Self::ComponentHalf),
            "whole_repeat_37_of_210_v1" => Ok(Self::MatchedWholeRepeat),
            _ => Err("unknown NES action correlation policy".into()),
        }
    }

    /// Transform at most six freshly sampled alphabet actions. The ordinary
    /// sampler runs first: lengths, durations, and special taps stay identical
    /// for a given mutation seed. A separate deterministic draw stream chooses
    /// refreshes; seed separation is not a statistical independence proof.
    pub fn apply(
        self,
        suffix: &mut [ButtonChord],
        mutation_seed: u64,
        special_tap: u8,
    ) -> Result<(), Box<dyn Error>> {
        if self == Self::Independent {
            return Ok(());
        }
        if suffix.len() > 6 || !matches!(special_tap, 0x04 | 0x08) {
            return Err("correlation requires at most six chords and one supported tap".into());
        }
        // Validate before modifying any input. Keep the existing direction
        // constraints and never manufacture a menu/gameplay combination.
        if suffix
            .iter()
            .any(|a| a.buttons != special_tap && !ordinary(a.buttons))
        {
            return Err("correlation received a chord outside the ordinary alphabet".into());
        }
        let mut hash = Sha256::new();
        hash.update(b"nes-action-correlation-v1\0");
        hash.update(mutation_seed.to_le_bytes());
        let seed = u64::from_le_bytes(hash.finalize()[..8].try_into()?);
        let mut rand = RomuDuoJrRand::with_seed(seed);
        let mut previous = None;
        for action in suffix {
            if action.buttons == special_tap {
                previous = None;
                continue;
            }
            if let Some(before) = previous {
                action.buttons = match self {
                    Self::Independent => action.buttons,
                    Self::ComponentHalf => component_choice(
                        before,
                        action.buttons,
                        rand.below(NonZeroUsize::new(6).ok_or("invalid component draw")?),
                    ),
                    Self::MatchedWholeRepeat => whole_choice(
                        before,
                        action.buttons,
                        rand.below(NonZeroUsize::new(210).ok_or("invalid repeat draw")?),
                    ),
                };
            }
            previous = Some(action.buttons);
        }
        Ok(())
    }
}

fn ordinary(buttons: u8) -> bool {
    buttons & 0x0c == 0
        && matches!(
            buttons & 0xf0,
            0 | 0x10 | 0x20 | 0x40 | 0x80 | 0x50 | 0x90 | 0x60 | 0xa0
        )
}

fn component_choice(before: u8, fresh: u8, choice: usize) -> u8 {
    let mask = match choice {
        0 => 0xf0,
        1 => 0x01,
        2 => 0x02,
        _ => return fresh,
    };
    (before & !mask) | (fresh & mask)
}

fn whole_choice(before: u8, fresh: u8, choice: usize) -> u8 {
    if choice < 37 { before } else { fresh }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::draw::{DrawMixture, SuffixShape, draw_suffix};

    fn alphabet() -> Vec<u8> {
        [0, 0x10, 0x20, 0x40, 0x80, 0x50, 0x90, 0x60, 0xa0]
            .into_iter()
            .flat_map(|d| (0..4).map(move |ab| d | ab))
            .collect()
    }

    #[test]
    fn whole_repeat_control_matches_the_candidate_full_command_stay_probability() {
        for before in alphabet() {
            let mut same = 0;
            for fresh in alphabet() {
                for choice in 0..210 {
                    same += usize::from(whole_choice(before, fresh, choice) == before);
                }
            }
            assert_eq!(same * 216, 43 * 210 * 36);
        }
    }

    #[test]
    fn every_transition_matches_the_independently_derived_finite_kernel() {
        let alphabet = alphabet();
        for &before in &alphabet {
            let mut counts = [0; 256];
            for &fresh in &alphabet {
                for choice in 0..6 {
                    let after = component_choice(before, fresh, choice);
                    assert!(ordinary(after));
                    counts[usize::from(after)] += 1;
                }
            }
            for &after in &alphabet {
                // 216 equiprobable (fresh command, choice) pairs. The fresh
                // half contributes three; preserving two components adds the
                // size of those two component alphabets.
                let same_d = before & 0xf0 == after & 0xf0;
                let same_a = before & 1 == after & 1;
                let same_b = before & 2 == after & 2;
                let expected = 3
                    + 4 * usize::from(same_a && same_b)
                    + 18 * usize::from(same_d && same_b)
                    + 18 * usize::from(same_d && same_a);
                assert_eq!(counts[usize::from(after)], expected);
            }
            assert_eq!(counts[usize::from(before)], 43);
            assert_eq!(counts.iter().sum::<usize>(), 216);
        }
    }

    #[test]
    fn all_modes_preserve_duration_tap_positions_and_independent_starts() {
        for (sampler, tap) in [
            (
                crate::metroid::archive::sample_chord
                    as fn(&mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>>,
                4,
            ),
            (crate::mm2::archive::sample_chord, 8),
        ] {
            let mut changed = [0; 2];
            for seed in 0..4096 {
                let original = draw_suffix(
                    SuffixShape::OneToSix,
                    DrawMixture::AlphabetOnly,
                    0,
                    seed,
                    |_| Ok(None),
                    sampler,
                )
                .unwrap();
                let mut legacy = original.clone();
                ChordCorrelation::Independent
                    .apply(&mut legacy, seed, tap)
                    .unwrap();
                assert_eq!(legacy, original);
                for (index, policy) in [
                    ChordCorrelation::ComponentHalf,
                    ChordCorrelation::MatchedWholeRepeat,
                ]
                .into_iter()
                .enumerate()
                {
                    let mut actual = original.clone();
                    policy.apply(&mut actual, seed, tap).unwrap();
                    let mut again = original.clone();
                    policy.apply(&mut again, seed, tap).unwrap();
                    assert_eq!(actual, again);
                    changed[index] += usize::from(actual != original);
                    assert_eq!(actual.len(), original.len());
                    for (i, (before, after)) in original.iter().zip(&actual).enumerate() {
                        assert_eq!(before.hold_frames, after.hold_frames);
                        assert_eq!(before.buttons == tap, after.buttons == tap);
                        if i == 0 || before.buttons == tap || original[i - 1].buttons == tap {
                            assert_eq!(before, after);
                        }
                    }
                }
            }
            assert!(changed.into_iter().all(|n| n > 0));
        }
    }

    #[test]
    fn malformed_input_is_rejected_before_mutation_and_policies_are_strict() {
        let mut input = vec![
            ButtonChord::new(0x81, 4),
            ButtonChord::new(0x82, 5),
            ButtonChord::new(0xff, 6),
        ];
        let original = input.clone();
        assert!(
            ChordCorrelation::ComponentHalf
                .apply(&mut input, 3, 4)
                .is_err()
        );
        assert_eq!(input, original);
        assert!(
            ChordCorrelation::ComponentHalf
                .apply(&mut [ButtonChord::new(0, 1); 7], 3, 4)
                .is_err()
        );
        assert!(
            ChordCorrelation::ComponentHalf
                .apply(&mut [], 3, 12)
                .is_err()
        );
        for policy in [
            ChordCorrelation::Independent,
            ChordCorrelation::ComponentHalf,
            ChordCorrelation::MatchedWholeRepeat,
        ] {
            assert_eq!(
                ChordCorrelation::parse(policy.identifier()).unwrap(),
                policy
            );
        }
        assert!(ChordCorrelation::parse("component_refresh_half_v2").is_err());
    }
}
