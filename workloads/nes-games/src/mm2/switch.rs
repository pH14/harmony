// SPDX-License-Identifier: AGPL-3.0-or-later

use machine::nes::ButtonChord;

use crate::mm2::game::{JOYPAD_DOWN, JOYPAD_START, MENU_CLOSED, MENU_PAGE_ROWS, MENU_ROWS};

pub const SWITCH_ODDS: usize = 12;

pub const SWITCH_PLANNED_FRAMES: u64 = 40;

const SWITCH_OPEN_WAIT_FRAMES: u32 = 30;

pub const SWITCH_FRAME_LIMIT: u32 = 240;

const LAST_FIRST_PAGE_WEAPON: u8 = 5;

fn menu_row_is_open(menu: u8) -> bool {
    usize::from(menu / MENU_PAGE_ROWS) < MENU_ROWS.len()
        && menu % MENU_PAGE_ROWS < MENU_ROWS[usize::from(menu / MENU_PAGE_ROWS)]
}

fn menu_page(menu: u8) -> u8 {
    menu / MENU_PAGE_ROWS
}

fn menu_next_row(menu: u8) -> u8 {
    menu_page(menu) * MENU_PAGE_ROWS
}

#[must_use]
pub const fn weapon_menu_row(weapon: u8) -> u8 {
    if weapon <= LAST_FIRST_PAGE_WEAPON {
        weapon + 1
    } else {
        weapon - LAST_FIRST_PAGE_WEAPON - 1 + MENU_PAGE_ROWS + 1
    }
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
    pub const fn to_row(row: u8) -> Self {
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

    #[must_use]
    pub fn starts(menu: u8, playable: bool) -> bool {
        Self::to_row(0).step(menu, playable).is_some()
    }

    pub fn step(&mut self, menu: u8, playable: bool) -> Option<ButtonChord> {
        if self.frames >= SWITCH_FRAME_LIMIT {
            return None;
        }
        let settled = menu_row_is_open(menu) && menu == self.last_menu;
        self.last_menu = menu;
        let buttons = match self.step {
            Step::Open { pressed, waited } => {
                if settled {
                    self.step = Step::Move { released: true };
                    self.choose(menu)
                } else if !pressed {
                    if menu != MENU_CLOSED || !playable {
                        if !menu_row_is_open(menu) {
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
                } else if waited >= SWITCH_OPEN_WAIT_FRAMES && menu == MENU_CLOSED {
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
                    self.choose(menu)
                } else {
                    0
                }
            }
            Step::Close => {
                if menu == MENU_CLOSED {
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

    fn steps(switch: &mut WeaponSwitch, menus: &[u8]) -> Vec<Option<u8>> {
        menus
            .iter()
            .map(|&menu| switch.step(menu, true).map(|chord| chord.buttons))
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
        assert_eq!(switch.step(MENU_CLOSED, true), None);
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
        assert_eq!(WeaponSwitch::to_row(3).step(MENU_CLOSED, false), None);
        let mut switch = WeaponSwitch::to_row(3);
        switch.step(MENU_CLOSED, true);
        let waits = std::iter::from_fn(|| switch.step(MENU_CLOSED, true)).count();
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
        switch.step(MENU_CLOSED, true);
        let steps = std::iter::from_fn(|| switch.step(0xfe, true)).count();
        assert_eq!(steps, usize::try_from(SWITCH_FRAME_LIMIT).unwrap() - 1);
    }
}
