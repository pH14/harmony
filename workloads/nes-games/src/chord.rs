// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use machine::nes::ButtonChord;

use crate::search::rand::RomuDuoJrRand;

pub const CHORD_DRAW_FIELD: &str = "chord_draw";

pub const CHANGE_ONE_CONTROL_IDENTIFIER: &str = "change_one_control_within_vocabulary_v2";

pub const NES_PRESSABLE_BUTTON_MASKS: [u8; 36] = [
    0x00, 0x01, 0x02, 0x03, 0x80, 0x81, 0x82, 0x83, 0x40, 0x41, 0x42, 0x43, 0x10, 0x11, 0x12, 0x13,
    0x20, 0x21, 0x22, 0x23, 0x90, 0x91, 0x92, 0x93, 0xa0, 0xa1, 0xa2, 0xa3, 0x50, 0x51, 0x52, 0x53,
    0x60, 0x61, 0x62, 0x63,
];

pub const SHORT_HOLD_FRAMES: (u8, u8) = (2, 12);

pub const LONG_HOLD_FRAMES: (u8, u8) = (48, 120);

const DIRECTION_MASKS: [u8; 9] = [0x00, 0x80, 0x40, 0x10, 0x20, 0x90, 0xa0, 0x50, 0x60];

const DIRECTION_BITS: u8 = 0xf0;

const A_BUTTON: u8 = 0x01;

const B_BUTTON: u8 = 0x02;

const CONTROL_KINDS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tap {
    pub buttons: u8,
    pub odds: usize,
    pub hold_frames: (u8, u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChordVocabulary {
    pub held: &'static [u8],
    pub tap: Option<Tap>,
    pub short_hold: (u8, u8),
    pub long_hold: (u8, u8),
}

impl ChordVocabulary {
    pub fn draw(
        &self,
        rand: &mut RomuDuoJrRand,
        previous: Option<&ButtonChord>,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        match previous {
            Some(previous) => self.change_one_control(rand, previous),
            None => self.sample(rand),
        }
    }

    pub fn sample(&self, rand: &mut RomuDuoJrRand) -> Result<ButtonChord, Box<dyn Error>> {
        if let Some(tap) = self.tap
            && below(rand, tap.odds)? == 0
        {
            return Ok(ButtonChord::new(
                tap.buttons,
                hold_in(rand, tap.hold_frames)?,
            ));
        }
        let buttons = self.held[below(rand, self.held.len())?];
        Ok(ButtonChord::new(buttons, self.stratified_hold(rand)?))
    }

    pub fn change_one_control(
        &self,
        rand: &mut RomuDuoJrRand,
        previous: &ButtonChord,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        let base = previous.buttons & !self.tap.map_or(0, |tap| tap.buttons);
        if !self.held.contains(&base) {
            return self.sample(rand);
        }
        if let Some(tap) = self.tap
            && below(rand, tap.odds)? == 0
        {
            return Ok(ButtonChord::new(
                base | tap.buttons,
                hold_in(rand, tap.hold_frames)?,
            ));
        }
        let held = |buttons: u8| self.held.contains(&buttons).then_some(buttons);
        let buttons = match below(rand, CONTROL_KINDS)? {
            0 => {
                let others: Vec<u8> = DIRECTION_MASKS
                    .into_iter()
                    .filter(|&direction| direction != base & DIRECTION_BITS)
                    .filter_map(|direction| held((base & !DIRECTION_BITS) | direction))
                    .collect();
                if others.is_empty() {
                    base
                } else {
                    others[below(rand, others.len())?]
                }
            }
            1 => held(base ^ A_BUTTON).unwrap_or(base),
            2 => held(base ^ B_BUTTON).unwrap_or(base),
            _ => base,
        };
        Ok(ButtonChord::new(buttons, self.stratified_hold(rand)?))
    }

    pub fn stratified_hold(&self, rand: &mut RomuDuoJrRand) -> Result<u8, Box<dyn Error>> {
        let stratum = if below(rand, 2)? == 0 {
            self.short_hold
        } else {
            self.long_hold
        };
        hold_in(rand, stratum)
    }

    #[must_use]
    pub fn longest_hold(&self) -> u8 {
        self.short_hold
            .1
            .max(self.long_hold.1)
            .max(self.tap.map_or(0, |tap| tap.hold_frames.1))
    }
}

fn below(rand: &mut RomuDuoJrRand, choices: usize) -> Result<usize, Box<dyn Error>> {
    Ok(rand.below(NonZeroUsize::new(choices).ok_or("empty chord draw")?))
}

fn hold_in(rand: &mut RomuDuoJrRand, (low, high): (u8, u8)) -> Result<u8, Box<dyn Error>> {
    let span = high.checked_sub(low).ok_or("inverted hold range")?;
    Ok(u8::try_from(
        usize::from(low) + below(rand, usize::from(span) + 1)?,
    )?)
}

#[cfg(test)]
mod tests {
    use super::{
        A_BUTTON, B_BUTTON, ButtonChord, ChordVocabulary, DIRECTION_BITS, LONG_HOLD_FRAMES,
        NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES, Tap,
    };
    use crate::search::rand::RomuDuoJrRand;

    const SELECT: u8 = 0x04;

    const SMALL: [u8; 5] = [0x00, 0x80, 0x81, 0x40, 0x02];

    fn vocabulary(held: &'static [u8], tap: Option<Tap>) -> ChordVocabulary {
        ChordVocabulary {
            held,
            tap,
            short_hold: SHORT_HOLD_FRAMES,
            long_hold: LONG_HOLD_FRAMES,
        }
    }

    fn select_taps() -> Tap {
        Tap {
            buttons: SELECT,
            odds: 12,
            hold_frames: (2, 7),
        }
    }

    fn kind(previous: u8, next: u8) -> usize {
        match (
            next & DIRECTION_BITS != previous & DIRECTION_BITS,
            (next ^ previous) & !DIRECTION_BITS,
        ) {
            (true, 0) => 0,
            (false, A_BUTTON) => 1,
            (false, B_BUTTON) => 2,
            (false, 0) => 3,
            other => panic!("{previous:#04x} -> {next:#04x} changed {other:?}"),
        }
    }

    #[test]
    fn a_change_moves_one_control_with_equal_odds_for_each_control() {
        let chords = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, None);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_c401);
        let mut kinds = [0_u32; 4];
        for &previous in &NES_PRESSABLE_BUTTON_MASKS {
            for _ in 0..400 {
                let next = chords
                    .change_one_control(&mut rand, &ButtonChord::new(previous, 30))
                    .expect("change");
                assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&next.buttons));
                assert!(
                    (2..=12).contains(&next.hold_frames) || (48..=120).contains(&next.hold_frames)
                );
                kinds[kind(previous, next.buttons)] += 1;
            }
        }
        for count in kinds {
            assert!(
                (3_300..=3_900).contains(&count),
                "control changes {kinds:?}"
            );
        }
    }

    #[test]
    fn every_draw_stays_inside_a_small_vocabulary() {
        let chords = vocabulary(&SMALL, None);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_c402);
        let mut reached = std::collections::BTreeSet::new();
        for &previous in &SMALL {
            for _ in 0..400 {
                let next = chords
                    .draw(&mut rand, Some(&ButtonChord::new(previous, 30)))
                    .expect("change");
                assert!(SMALL.contains(&next.buttons), "{next:?}");
                kind(previous, next.buttons);
                reached.insert(next.buttons);
            }
        }
        assert_eq!(reached.len(), SMALL.len());
    }

    #[test]
    fn a_previous_chord_outside_the_vocabulary_draws_a_fresh_chord() {
        let chords = vocabulary(&SMALL, None);
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_c403);
        for _ in 0..400 {
            let next = chords
                .draw(&mut rand, Some(&ButtonChord::new(0x08, 30)))
                .expect("fresh");
            assert!(SMALL.contains(&next.buttons), "{next:?}");
        }
    }

    #[test]
    fn a_tap_joins_the_chord_before_it_and_the_next_chord_drops_it() {
        let chords = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, Some(select_taps()));
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_c404);
        let mut taps = 0_u32;
        let draws = 12_000_u32;
        for _ in 0..draws {
            let next = chords
                .draw(&mut rand, Some(&ButtonChord::new(0x82, 30)))
                .expect("draw");
            if next.buttons & SELECT == 0 {
                kind(0x82, next.buttons);
                continue;
            }
            taps += 1;
            assert_eq!(next.buttons, 0x82 | SELECT);
            assert!((2..=7).contains(&next.hold_frames));
            let after = chords.draw(&mut rand, Some(&next)).expect("after a tap");
            if after.buttons & SELECT == 0 {
                kind(0x82, after.buttons);
            } else {
                assert_eq!(after.buttons, 0x82 | SELECT);
            }
        }
        assert!((850..=1_150).contains(&taps), "taps {taps} of {draws}");
    }

    #[test]
    fn a_first_chord_is_a_held_chord_or_a_tap_alone() {
        let chords = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, Some(select_taps()));
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_c405);
        let mut taps = 0_u32;
        for _ in 0..12_000 {
            let chord = chords.draw(&mut rand, None).expect("first chord");
            if chord.buttons == SELECT {
                taps += 1;
                assert!((2..=7).contains(&chord.hold_frames));
            } else {
                assert!(NES_PRESSABLE_BUTTON_MASKS.contains(&chord.buttons));
            }
        }
        assert!((850..=1_150).contains(&taps), "taps {taps}");
        assert_eq!(chords.longest_hold(), 120);
    }
}
