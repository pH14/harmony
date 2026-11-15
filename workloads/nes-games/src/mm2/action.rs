// SPDX-License-Identifier: AGPL-3.0-or-later

use std::{error::Error, num::NonZeroUsize};

use machine::nes::ButtonChord;
use serde::{Deserialize, Serialize};

use crate::{
    chord::SHORT_HOLD_FRAMES,
    mm2::{
        archive::{CHORDS, chord_time},
        target::{
            JOYPAD_DOWN, JOYPAD_START, MENU_CLOSED, Mm2MechanicalState, menu_next_row, menu_page,
            menu_row_is_open,
        },
    },
    search::rand::RomuDuoJrRand,
};

pub const SWITCH_ODDS: usize = 12;

pub const SWITCH_WAIT_FRAMES: u8 = SHORT_HOLD_FRAMES.1;

pub const SWITCH_PLANNED_FRAMES: u64 = 40;

const SWITCH_OPEN_WAIT_FRAMES: u32 = 30;

const SWITCH_FRAME_LIMIT: u32 = 240;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum Mm2Action {
    Chord(ButtonChord),
    Switch(u32),
}

impl From<ButtonChord> for Mm2Action {
    fn from(chord: ButtonChord) -> Self {
        Self::Chord(chord)
    }
}

impl Mm2Action {
    pub fn draw(rand: &mut RomuDuoJrRand, previous: Option<&Self>) -> Result<Self, Box<dyn Error>> {
        let odds = NonZeroUsize::new(SWITCH_ODDS).ok_or("switch odds are zero")?;
        if rand.below(odds) == 0 {
            return Ok(Self::Switch(u32::try_from(rand.next_u64() >> 32)?));
        }
        let previous = previous.and_then(|action| match action {
            Self::Chord(chord) => Some(chord),
            Self::Switch(_) => None,
        });
        Ok(Self::Chord(CHORDS.draw(rand, previous)?))
    }

    #[must_use]
    pub fn planned_frames(&self) -> u64 {
        match self {
            Self::Chord(chord) => chord_time(chord),
            Self::Switch(_) => SWITCH_PLANNED_FRAMES,
        }
    }
}

#[must_use]
pub fn switch_choice(choices: &[u8], word: u32) -> Option<u8> {
    let index = usize::try_from(word).ok()? % NonZeroUsize::new(choices.len())?;
    choices.get(index).copied()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Open { pressed: bool, waited: u32 },
    Move { released: bool },
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WeaponSwitch {
    row: u8,
    step: Step,
    frames: u32,
    last_menu: u8,
}

impl WeaponSwitch {
    #[must_use]
    pub fn to_row(row: u8) -> Self {
        Self {
            row,
            step: Step::Open {
                pressed: false,
                waited: 0,
            },
            frames: 0,
            last_menu: MENU_CLOSED,
        }
    }

    pub fn next(&mut self, state: Mm2MechanicalState) -> Option<ButtonChord> {
        if self.frames >= SWITCH_FRAME_LIMIT {
            return None;
        }
        let settled = menu_row_is_open(state.menu) && state.menu == self.last_menu;
        self.last_menu = state.menu;
        let buttons = match self.step {
            Step::Open { pressed, waited } => {
                if settled {
                    self.step = Step::Move { released: true };
                    self.choose(state.menu)
                } else if !pressed {
                    if state.menu != MENU_CLOSED || !state.playable() {
                        if !menu_row_is_open(state.menu) {
                            return None;
                        }
                        0
                    } else {
                        self.step = Step::Open {
                            pressed: true,
                            waited,
                        };
                        JOYPAD_START
                    }
                } else if waited >= SWITCH_OPEN_WAIT_FRAMES && state.menu == MENU_CLOSED {
                    return None;
                } else {
                    self.step = Step::Open {
                        pressed,
                        waited: waited + 1,
                    };
                    0
                }
            }
            Step::Move { released: false } => {
                self.step = Step::Move { released: true };
                0
            }
            Step::Move { released: true } => {
                if settled {
                    self.choose(state.menu)
                } else {
                    0
                }
            }
            Step::Close => {
                if state.menu == MENU_CLOSED {
                    return None;
                }
                0
            }
        };
        self.frames += 1;
        Some(ButtonChord::new(buttons, 1))
    }

    fn choose(&mut self, menu: u8) -> u8 {
        if menu == self.row {
            self.step = Step::Close;
            return JOYPAD_START;
        }
        self.step = Step::Move { released: false };
        if menu_page(menu) != menu_page(self.row) && menu == menu_next_row(menu) {
            JOYPAD_START
        } else {
            JOYPAD_DOWN
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mm2::target::{GAMEPLAY_BANK, PLAYER_STATE_STANDING};

    fn playing(menu: u8) -> Mm2MechanicalState {
        Mm2MechanicalState {
            bank: GAMEPLAY_BANK,
            health: 28,
            player_state: PLAYER_STATE_STANDING,
            menu,
            ..Mm2MechanicalState::default()
        }
    }

    fn steps(switch: &mut WeaponSwitch, menus: &[u8]) -> Vec<Option<u8>> {
        menus
            .iter()
            .map(|&menu| switch.next(playing(menu)).map(|chord| chord.buttons))
            .collect()
    }

    #[test]
    fn a_switch_opens_the_menu_moves_down_to_the_row_and_closes_it() {
        let mut switch = WeaponSwitch::to_row(5);
        assert_eq!(
            steps(
                &mut switch,
                &[MENU_CLOSED, 0xfe, 0xfe, 1, 1, 3, 3, 5, 5, 2, 0xfe]
            ),
            [
                Some(JOYPAD_START),
                Some(0),
                Some(0),
                Some(0),
                Some(JOYPAD_DOWN),
                Some(0),
                Some(JOYPAD_DOWN),
                Some(0),
                Some(JOYPAD_START),
                Some(0),
                Some(0),
            ]
        );
        assert_eq!(switch.next(playing(MENU_CLOSED)), None);
    }

    #[test]
    fn a_switch_waits_out_the_rows_the_menu_counts_through_while_opening() {
        let mut switch = WeaponSwitch::to_row(11);
        assert_eq!(
            steps(&mut switch, &[MENU_CLOSED, 9, 10, 11, 12, 0xfe, 9, 9]),
            [
                Some(JOYPAD_START),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(0),
                Some(JOYPAD_DOWN),
            ]
        );
    }

    #[test]
    fn a_switch_to_the_other_page_presses_start_on_next() {
        let mut switch = WeaponSwitch::to_row(11);
        assert_eq!(
            steps(&mut switch, &[MENU_CLOSED, 1, 1, 0, 0, 0xfe, 8, 8, 11, 11]),
            [
                Some(JOYPAD_START),
                Some(0),
                Some(JOYPAD_DOWN),
                Some(0),
                Some(JOYPAD_START),
                Some(0),
                Some(0),
                Some(JOYPAD_DOWN),
                Some(0),
                Some(JOYPAD_START),
            ]
        );
    }

    #[test]
    fn a_switch_outside_play_or_with_a_closed_menu_that_never_opens_ends() {
        let mut dead = playing(MENU_CLOSED);
        dead.health = 0;
        assert_eq!(WeaponSwitch::to_row(3).next(dead), None);
        let mut switch = WeaponSwitch::to_row(3);
        switch.next(playing(MENU_CLOSED));
        let waits = std::iter::from_fn(|| switch.next(playing(MENU_CLOSED))).count();
        assert_eq!(waits, usize::try_from(SWITCH_OPEN_WAIT_FRAMES).unwrap());
    }

    #[test]
    fn a_switch_that_finds_the_menu_open_resumes_from_it() {
        let mut switch = WeaponSwitch::to_row(2);
        assert_eq!(steps(&mut switch, &[7, 7]), [Some(0), Some(JOYPAD_DOWN)]);
    }

    #[test]
    fn a_switch_stops_at_the_frame_limit() {
        let mut switch = WeaponSwitch::to_row(3);
        switch.next(playing(MENU_CLOSED));
        let steps = std::iter::from_fn(|| switch.next(playing(0xfe))).count();
        assert_eq!(steps, usize::try_from(SWITCH_FRAME_LIMIT).unwrap() - 1);
    }

    #[test]
    fn the_word_picks_each_choice_and_no_choice_picks_nothing() {
        assert_eq!(switch_choice(&[], 7), None);
        let picks = (0..6)
            .filter_map(|word| switch_choice(&[0, 6, 8], word))
            .collect::<Vec<_>>();
        assert_eq!(picks, [0, 6, 8, 0, 6, 8]);
    }

    #[test]
    fn draws_mix_switches_and_chords_without_start() {
        let mut rand = RomuDuoJrRand::with_seed(0x5eed_3300);
        let mut switches = 0_u32;
        let mut previous = None;
        for _ in 0..12_000 {
            let action = Mm2Action::draw(&mut rand, previous.as_ref()).expect("draw");
            match action {
                Mm2Action::Switch(_) => switches += 1,
                Mm2Action::Chord(chord) => assert_eq!(chord.buttons & JOYPAD_START, 0),
            }
            previous = Some(action);
        }
        assert!((850..=1_150).contains(&switches), "switches {switches}");
    }
}
