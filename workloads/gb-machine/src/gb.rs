// SPDX-License-Identifier: AGPL-3.0-or-later

use serde::{Deserialize, Serialize};

pub const WRAM_SIZE: usize = 8 * 1024;
pub const WRAM_BASE: u64 = 0xc000;
pub const MAX_HOLD_FRAMES: u8 = 120;

pub const A: u8 = 0x01;
pub const B: u8 = 0x02;
pub const SELECT: u8 = 0x04;
pub const START: u8 = 0x08;
pub const RIGHT: u8 = 0x10;
pub const LEFT: u8 = 0x20;
pub const UP: u8 = 0x40;
pub const DOWN: u8 = 0x80;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ButtonChord {
    pub buttons: u8,
    pub hold_frames: u8,
}

impl ButtonChord {
    #[must_use]
    pub fn new(buttons: u8, hold_frames: u8) -> Self {
        Self {
            buttons,
            hold_frames: hold_frames.clamp(1, MAX_HOLD_FRAMES),
        }
    }

    #[must_use]
    pub fn bounded_hold_frames(self) -> u8 {
        self.hold_frames.clamp(1, MAX_HOLD_FRAMES)
    }
}

#[cfg(test)]
mod tests {
    use super::{A, B, ButtonChord, DOWN, LEFT, MAX_HOLD_FRAMES, RIGHT, SELECT, START, UP};

    #[test]
    fn the_button_byte_is_the_game_boy_joypad_order() {
        assert_eq!(
            [A, B, SELECT, START, RIGHT, LEFT, UP, DOWN],
            [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80]
        );
    }

    #[test]
    fn chord_duration_is_total_and_bounded() {
        assert_eq!(ButtonChord::new(A, 0).hold_frames, 1);
        assert_eq!(ButtonChord::new(A, u8::MAX).hold_frames, MAX_HOLD_FRAMES);
        assert_eq!(ButtonChord::new(A, 7).bounded_hold_frames(), 7);
    }
}
