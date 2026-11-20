// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use machine::nes::{ButtonChord, MAX_HOLD_FRAMES};

use crate::search::{
    choice::{CERTAIN, ChoiceProfile, ChoiceRecord, ChoiceSite, Chooser},
    rand::RomuDuoJrRand,
};

pub const CHORD_DRAW_FIELD: &str = "chord_draw";

pub const CHANGE_ONE_CONTROL_IDENTIFIER: &str = "change_one_control_within_vocabulary_v2";

pub const CHORD_TREE_IDENTIFIER: &str = "chord_tree_v1";

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

pub const CHORD_SITES: u16 = 5;

pub const CHORD_EDIT_SITE: u16 = 0;

pub const CHORD_DIRECTION_SITE: u16 = 1;

pub const CHORD_FRESH_SITE: u16 = 2;

pub const HOLD_STRATUM_SITE: u16 = 3;

pub const HOLD_FRAMES_SITE: u16 = 4;

const EDIT_KEEP: u16 = 0;

const EDIT_DIRECTION: u16 = 1;

const EDIT_A: u16 = 2;

const EDIT_B: u16 = 3;

const EDIT_TAP: u16 = 4;

const DIRECTION_NAMES: [&str; 9] = [
    "none",
    "right",
    "left",
    "up",
    "down",
    "up_right",
    "down_right",
    "up_left",
    "down_left",
];

const NO_PREVIOUS: u8 = 0xff;

#[must_use]
pub const fn ask(previous: Option<&ButtonChord>) -> ButtonChord {
    ButtonChord {
        buttons: match previous {
            Some(previous) => previous.buttons,
            None => NO_PREVIOUS,
        },
        hold_frames: 0,
    }
}

#[must_use]
pub fn edited(asked: &ButtonChord, previous: Option<&ButtonChord>) -> Option<ButtonChord> {
    Some(*previous.unwrap_or(asked)).filter(|chord| chord.buttons != NO_PREVIOUS)
}

#[must_use]
pub const fn is_ask(chord: &ButtonChord) -> bool {
    chord.hold_frames == 0
}

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
    #[must_use]
    pub fn identifier(&self) -> String {
        let held: Vec<String> = self.held.iter().map(|mask| format!("{mask:02x}")).collect();
        let tap = self.tap.map_or_else(
            || "none".to_owned(),
            |tap| {
                format!(
                    "{:02x}x{}@{}-{}",
                    tap.buttons, tap.odds, tap.hold_frames.0, tap.hold_frames.1
                )
            },
        );
        format!(
            "held={};tap={tap};short={}-{};long={}-{}",
            held.join(","),
            self.short_hold.0,
            self.short_hold.1,
            self.long_hold.0,
            self.long_hold.1
        )
    }

    #[must_use]
    pub fn sites(&self) -> Vec<ChoiceSite> {
        let fresh = self
            .held
            .iter()
            .map(|mask| format!("{mask:02x}"))
            .chain(self.tap.map(|tap| format!("tap_{:02x}", tap.buttons)));
        vec![
            ChoiceSite::new(
                "chord_edit",
                ["keep", "direction", "a", "b", "tap"].map(str::to_owned),
            ),
            ChoiceSite::new("chord_direction", DIRECTION_NAMES.map(str::to_owned)),
            ChoiceSite::new("chord_fresh", fresh),
            ChoiceSite::new("hold_stratum", ["short", "long"].map(str::to_owned)),
            ChoiceSite::new(
                "hold_frames",
                (0..=MAX_HOLD_FRAMES).map(|frames| frames.to_string()),
            ),
        ]
    }

    #[must_use]
    pub fn reproducing_profile(&self) -> ChoiceProfile {
        let contrary = match self.tap {
            Some(tap) => {
                let odds = u64::try_from(tap.odds).unwrap_or(u64::MAX).max(1);
                u64::from(CERTAIN) * (3 * odds + 1) / (4 * odds)
            }
            None => u64::from(CERTAIN) * 3 / 4,
        };
        ChoiceProfile::new(u32::try_from(contrary).unwrap_or(CERTAIN), 0)
    }

    pub fn resolve(
        &self,
        chooser: &mut Chooser,
        asked: &ButtonChord,
        previous: Option<&ButtonChord>,
        records: &mut Vec<ChoiceRecord>,
    ) -> Result<ButtonChord, Box<dyn Error>> {
        let mut ask = |site: u16, offered: &[(u16, u32)], default: u16, contrary: bool| {
            let taken = if contrary {
                chooser.choose_weighted(site, offered, default)
            } else {
                chooser.draw(site, offered, default)
            };
            let labels: Vec<u16> = offered.iter().map(|(label, _)| *label).collect();
            let record = ChoiceRecord::new(site, labels, default, taken);
            let resolved = record.resolved;
            records.push(record);
            resolved
        };
        let tap_buttons = self.tap.map_or(0, |tap| tap.buttons);
        let base = edited(asked, previous)
            .map(|chord| chord.buttons & !tap_buttons)
            .filter(|base| self.held.contains(base));
        let Some(base) = base else {
            let held_weight = self
                .tap
                .map_or(Ok(1), |tap| u32::try_from(tap.odds.saturating_sub(1)))?;
            let mut offered: Vec<(u16, u32)> = (0..self.held.len())
                .map(|index| Ok((u16::try_from(index)?, held_weight)))
                .collect::<Result<_, Box<dyn Error>>>()?;
            if self.tap.is_some() {
                offered.push((
                    u16::try_from(self.held.len())?,
                    u32::try_from(self.held.len())?,
                ));
            }
            let pick = usize::from(ask(CHORD_FRESH_SITE, &offered, 0, false));
            return match (self.held.get(pick), self.tap) {
                (Some(&buttons), _) => Ok(ButtonChord::new(buttons, self.hold(&mut ask, None)?)),
                (None, Some(tap)) => Ok(ButtonChord::new(
                    tap.buttons,
                    self.hold(&mut ask, Some(tap.hold_frames))?,
                )),
                (None, None) => Err("a fresh chord label is outside the vocabulary".into()),
            };
        };
        let edit_weight = self
            .tap
            .map_or(Ok(1), |tap| u32::try_from(tap.odds.saturating_sub(1)))?;
        let mut edits = vec![
            (EDIT_KEEP, 1),
            (EDIT_DIRECTION, edit_weight),
            (EDIT_A, edit_weight),
            (EDIT_B, edit_weight),
        ];
        if self.tap.is_some() {
            edits.push((EDIT_TAP, 4));
        }
        let held = |buttons: u8| self.held.contains(&buttons).then_some(buttons);
        let buttons = match ask(CHORD_EDIT_SITE, &edits, EDIT_KEEP, true) {
            EDIT_TAP => {
                if let Some(tap) = self.tap {
                    return Ok(ButtonChord::new(
                        base | tap.buttons,
                        self.hold(&mut ask, Some(tap.hold_frames))?,
                    ));
                }
                base
            }
            EDIT_DIRECTION => {
                let others: Vec<(u16, u32)> = DIRECTION_MASKS
                    .into_iter()
                    .enumerate()
                    .filter(|&(_, direction)| direction != base & DIRECTION_BITS)
                    .filter(|&(_, direction)| held((base & !DIRECTION_BITS) | direction).is_some())
                    .map(|(index, _)| Ok((u16::try_from(index)?, 1)))
                    .collect::<Result<_, Box<dyn Error>>>()?;
                match others.first() {
                    Some(&(first, _)) => {
                        let pick = usize::from(ask(CHORD_DIRECTION_SITE, &others, first, false));
                        (base & !DIRECTION_BITS) | DIRECTION_MASKS[pick]
                    }
                    None => base,
                }
            }
            EDIT_A => held(base ^ A_BUTTON).unwrap_or(base),
            EDIT_B => held(base ^ B_BUTTON).unwrap_or(base),
            _ => base,
        };
        Ok(ButtonChord::new(buttons, self.hold(&mut ask, None)?))
    }

    fn hold(
        &self,
        ask: &mut impl FnMut(u16, &[(u16, u32)], u16, bool) -> u16,
        range: Option<(u8, u8)>,
    ) -> Result<u8, Box<dyn Error>> {
        let (low, high) = match range {
            Some(range) => range,
            None => {
                let strata = [(0, 1), (1, 1)];
                if ask(HOLD_STRATUM_SITE, &strata, 0, false) == 0 {
                    self.short_hold
                } else {
                    self.long_hold
                }
            }
        };
        if low > high {
            return Err("inverted hold range".into());
        }
        let frames: Vec<(u16, u32)> = (low..=high).map(|frames| (u16::from(frames), 1)).collect();
        let pick = ask(HOLD_FRAMES_SITE, &frames, u16::from(low), false);
        Ok(u8::try_from(pick)?)
    }

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
        NES_PRESSABLE_BUTTON_MASKS, SHORT_HOLD_FRAMES, Tap, ask, is_ask,
    };
    use crate::search::{
        choice::{ChoicePlan, Chooser},
        rand::RomuDuoJrRand,
    };

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

    fn shares(
        chords: &ChordVocabulary,
        previous: Option<ButtonChord>,
        mut next: impl FnMut(Option<&ButtonChord>) -> ButtonChord,
    ) -> [f64; 7] {
        let draws = 60_000;
        let mut counts = [0_u32; 7];
        let tap = chords.tap.map_or(0, |tap| tap.buttons);
        for _ in 0..draws {
            let chord = next(previous.as_ref());
            let slot = if chord.buttons & tap != 0 && tap != 0 {
                4
            } else if let Some(previous) = previous {
                kind(previous.buttons, chord.buttons)
            } else {
                3
            };
            counts[slot] += 1;
            if chord.buttons & tap == 0 || tap == 0 {
                counts[if chord.hold_frames <= SHORT_HOLD_FRAMES.1 {
                    5
                } else {
                    6
                }] += 1;
            }
        }
        counts.map(|count| f64::from(count) / f64::from(draws))
    }

    fn reproduced(chords: &ChordVocabulary, previous: Option<ButtonChord>, seed: u64) {
        reproduced_after(chords, previous, previous, seed);
    }

    fn reproduced_after(
        chords: &ChordVocabulary,
        previous: Option<ButtonChord>,
        replayed: Option<ButtonChord>,
        seed: u64,
    ) {
        let mut rand = RomuDuoJrRand::with_seed(seed);
        let today = shares(chords, previous, |previous| {
            chords.draw(&mut rand, previous).expect("draw")
        });
        let mut chooser = Chooser::new(&ChoicePlan {
            profile: chords.reproducing_profile(),
            redrawn: false,
            seed,
            untried: None,
            held: Vec::new(),
            declared: Vec::new(),
        });
        let mut records = Vec::new();
        let tree = shares(chords, previous, |previous| {
            chords
                .resolve(
                    &mut chooser,
                    &ask(previous),
                    replayed.as_ref(),
                    &mut records,
                )
                .expect("resolve")
        });
        for (today, tree) in today.iter().zip(tree) {
            assert!((today - tree).abs() < 0.012, "{today:?} {tree:?}");
        }
    }

    #[test]
    fn the_reproducing_profile_draws_the_shares_of_the_change_one_control_draw() {
        let tapped = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, Some(select_taps()));
        let plain = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, None);
        let previous = Some(ButtonChord::new(0x82, 30));
        reproduced(&tapped, previous, 0x5eed_c406);
        reproduced(&plain, previous, 0x5eed_c407);
        reproduced(&tapped, None, 0x5eed_c408);
        reproduced(&plain, None, 0x5eed_c409);
        reproduced(
            &vocabulary(&SMALL, None),
            Some(ButtonChord::new(0x81, 30)),
            0x5eed_c40a,
        );
    }

    #[test]
    fn a_job_from_a_snapshot_edits_the_chord_its_parent_ended_on() {
        let tapped = vocabulary(&NES_PRESSABLE_BUTTON_MASKS, Some(select_taps()));
        let previous = Some(ButtonChord::new(0x82, 30));
        reproduced_after(&tapped, previous, None, 0x5eed_c40b);
        reproduced_after(
            &vocabulary(&SMALL, None),
            Some(ButtonChord::new(0x81, 30)),
            None,
            0x5eed_c40c,
        );
    }

    #[test]
    fn a_resolved_chord_stays_in_the_vocabulary_and_records_each_site_it_asked() {
        let chords = vocabulary(&SMALL, None);
        let sites = chords.sites();
        let mut chooser = Chooser::new(&ChoicePlan {
            profile: chords.reproducing_profile(),
            redrawn: false,
            seed: 3,
            untried: None,
            held: Vec::new(),
            declared: Vec::new(),
        });
        let mut previous = None;
        for _ in 0..2_000 {
            let mut records = Vec::new();
            let next = chords
                .resolve(
                    &mut chooser,
                    &ask(previous.as_ref()),
                    previous.as_ref(),
                    &mut records,
                )
                .expect("resolve");
            assert!(SMALL.contains(&next.buttons), "{next:?}");
            assert!(!is_ask(&next));
            assert!(records.len() >= 2, "{records:?}");
            for record in &records {
                let site = &sites[usize::from(record.site)];
                assert!(
                    record
                        .offered
                        .iter()
                        .all(|label| usize::from(*label) < site.labels.len())
                );
                assert!(record.offered.contains(&record.resolved));
            }
            assert_eq!(
                u16::from(next.hold_frames),
                records.last().expect("a hold").resolved
            );
            previous = Some(next);
        }
        assert!(is_ask(&ask(previous.as_ref())));
        assert_eq!(ask(None).buttons, 0xff);
    }
}
