// SPDX-License-Identifier: AGPL-3.0-or-later

use crate::ram::{WORK_RAM_LEN, addr};

pub trait Core {
    fn serialize_size(&mut self) -> usize;

    fn serialize(&mut self, out: &mut [u8]) -> bool;

    fn run_frame(&mut self, joypad: u8);

    fn read_work_ram(&mut self, out: &mut [u8]) -> bool;

    fn read_save_ram(&mut self, _out: &mut [u8]) -> Option<usize> {
        None
    }
}

#[derive(Clone, Debug)]
pub struct MockCore {
    ram: [u8; WORK_RAM_LEN],
    save_ram: [u8; 8 * 1024],
    frame: u32,
    savestate_len: usize,
    start_countdown: Option<u8>,
    prev_joypad: u8,
}

pub const MOCK_SAVESTATE_LEN: usize = 96;

const MOCK_START_LOAD_FRAMES: u8 = 3;

impl Default for MockCore {
    fn default() -> Self {
        MockCore::new()
    }
}

impl MockCore {
    pub fn new() -> Self {
        MockCore {
            ram: [0u8; WORK_RAM_LEN],
            save_ram: [0u8; 8 * 1024],
            frame: 0,
            savestate_len: MOCK_SAVESTATE_LEN,
            start_countdown: None,
            prev_joypad: 0,
        }
    }

    pub fn in_gameplay() -> Self {
        let mut core = MockCore::new();
        core.ram[addr::OPER_MODE] = crate::ram::OPER_MODE_GAMEPLAY;
        core.ram[addr::PLAYER_X_POSITION] = 40;
        core.ram[addr::NUMBER_OF_LIVES] = 2;
        core
    }

    pub fn ram_mut(&mut self) -> &mut [u8; WORK_RAM_LEN] {
        &mut self.ram
    }

    pub fn save_ram_mut(&mut self) -> &mut [u8; 8 * 1024] {
        &mut self.save_ram
    }

    pub fn frames_run(&self) -> u32 {
        self.frame
    }
}

impl Core for MockCore {
    fn serialize_size(&mut self) -> usize {
        self.savestate_len
    }

    fn serialize(&mut self, out: &mut [u8]) -> bool {
        let frame = self.frame.to_le_bytes();
        for (i, b) in out.iter_mut().enumerate() {
            *b = frame[i % 4]
                .wrapping_add(i as u8)
                .wrapping_add(self.ram[addr::PLAYER_X_POSITION]);
        }
        true
    }

    fn run_frame(&mut self, joypad: u8) {
        use crate::chord::joypad::{LEFT, RIGHT, START};
        self.frame = self.frame.wrapping_add(1);
        if self.ram[addr::OPER_MODE] == crate::ram::OPER_MODE_TITLE {
            if joypad & START != 0
                && self.prev_joypad & START == 0
                && self.start_countdown.is_none()
            {
                self.start_countdown = Some(MOCK_START_LOAD_FRAMES);
            }
            if let Some(c) = self.start_countdown {
                if c == 0 {
                    self.start_countdown = None;
                    self.ram[addr::OPER_MODE] = crate::ram::OPER_MODE_GAMEPLAY;
                    self.ram[addr::PLAYER_X_POSITION] = 40;
                    self.ram[addr::NUMBER_OF_LIVES] = 2;
                } else {
                    self.start_countdown = Some(c - 1);
                }
            }
        }
        self.prev_joypad = joypad;
        if self.ram[addr::OPER_MODE] == crate::ram::OPER_MODE_GAMEPLAY {
            if joypad & RIGHT != 0 {
                let x = u16::from(self.ram[addr::PLAYER_X_POSITION]) + 2;
                self.ram[addr::PLAYER_X_POSITION] = (x & 0xFF) as u8;
                if x > 0xFF {
                    self.ram[addr::PLAYER_PAGE_LOC] =
                        self.ram[addr::PLAYER_PAGE_LOC].wrapping_add(1);
                }
            } else if joypad & LEFT != 0 {
                self.ram[addr::PLAYER_X_POSITION] =
                    self.ram[addr::PLAYER_X_POSITION].saturating_sub(1);
            }
        }
    }

    fn read_work_ram(&mut self, out: &mut [u8]) -> bool {
        if out.len() < WORK_RAM_LEN {
            return false;
        }
        out[..WORK_RAM_LEN].copy_from_slice(&self.ram);
        true
    }

    fn read_save_ram(&mut self, out: &mut [u8]) -> Option<usize> {
        if out.len() < self.save_ram.len() {
            return None;
        }
        out[..self.save_ram.len()].copy_from_slice(&self.save_ram);
        Some(self.save_ram.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::joypad::RIGHT;

    #[test]
    fn right_advances_x_with_page_carry() {
        let mut core = MockCore::in_gameplay();
        core.ram_mut()[addr::PLAYER_X_POSITION] = 0xFE;
        core.run_frame(RIGHT);
        assert_eq!(core.ram[addr::PLAYER_X_POSITION], 0x00);
        assert_eq!(core.ram[addr::PLAYER_PAGE_LOC], 1);
    }

    #[test]
    fn title_screen_ignores_directional_input() {
        let mut core = MockCore::new();
        core.run_frame(RIGHT);
        assert_eq!(core.ram[addr::PLAYER_X_POSITION], 0);
        assert_eq!(core.ram[addr::OPER_MODE], crate::ram::OPER_MODE_TITLE);
    }

    #[test]
    fn start_edge_enters_gameplay_after_the_load() {
        use crate::chord::joypad::START;
        let mut core = MockCore::new();
        for _ in 0..MOCK_START_LOAD_FRAMES {
            core.run_frame(START);
            assert_eq!(core.ram[addr::OPER_MODE], crate::ram::OPER_MODE_TITLE);
        }
        core.run_frame(START);
        assert_eq!(core.ram[addr::OPER_MODE], crate::ram::OPER_MODE_GAMEPLAY);
        assert_eq!(core.ram[addr::PLAYER_X_POSITION], 40);
    }

    #[test]
    fn serialize_is_deterministic_and_moment_dependent() {
        let mut a = MockCore::in_gameplay();
        let mut b = MockCore::in_gameplay();
        let mut buf_a = vec![0u8; MOCK_SAVESTATE_LEN];
        let mut buf_b = vec![0u8; MOCK_SAVESTATE_LEN];
        assert!(a.serialize(&mut buf_a));
        assert!(b.serialize(&mut buf_b));
        assert_eq!(buf_a, buf_b, "same moment, same bytes");
        a.run_frame(RIGHT);
        assert!(a.serialize(&mut buf_a));
        assert_ne!(buf_a, buf_b, "different moment, different bytes");
    }
}
