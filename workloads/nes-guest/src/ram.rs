// SPDX-License-Identifier: AGPL-3.0-or-later

use std::fmt;

pub const WORK_RAM_LEN: usize = 0x800;

pub mod addr {
    pub const OPER_MODE: usize = 0x0770;
    pub const PLAYER_STATUS: usize = 0x0756;
    pub const NUMBER_OF_LIVES: usize = 0x075A;
    pub const LEVEL_NUMBER: usize = 0x075C;
    pub const COIN_TALLY: usize = 0x075E;
    pub const WORLD_NUMBER: usize = 0x075F;
    pub const PLAYER_PAGE_LOC: usize = 0x6D;
    pub const PLAYER_X_POSITION: usize = 0x86;
}

pub const OPER_MODE_TITLE: u8 = 0;
pub const OPER_MODE_GAMEPLAY: u8 = 1;
pub const OPER_MODE_VICTORY: u8 = 2;
pub const OPER_MODE_GAME_OVER: u8 = 3;

pub const LEVELS_PER_WORLD: u64 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SmbState {
    pub game_mode: u8,
    pub world: u8,
    pub level: u8,
    pub x_abs: u32,
    pub powerup: u8,
    pub lives: u8,
    pub coins: u8,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RamError {
    TooShort { got: usize },
}

impl fmt::Display for RamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RamError::TooShort { got } => {
                write!(f, "work RAM buffer is {got} bytes, need {WORK_RAM_LEN}")
            }
        }
    }
}

impl std::error::Error for RamError {}

pub fn decode(ram: &[u8]) -> Result<SmbState, RamError> {
    if ram.len() < WORK_RAM_LEN {
        return Err(RamError::TooShort { got: ram.len() });
    }
    Ok(SmbState {
        game_mode: ram[addr::OPER_MODE],
        world: ram[addr::WORLD_NUMBER],
        level: ram[addr::LEVEL_NUMBER],
        x_abs: u32::from(ram[addr::PLAYER_PAGE_LOC]) * 256
            + u32::from(ram[addr::PLAYER_X_POSITION]),
        powerup: ram[addr::PLAYER_STATUS],
        lives: ram[addr::NUMBER_OF_LIVES],
        coins: ram[addr::COIN_TALLY],
    })
}

impl SmbState {
    pub fn in_gameplay(&self) -> bool {
        self.game_mode == OPER_MODE_GAMEPLAY
    }

    pub fn depth_ordinal(&self) -> u64 {
        u64::from(self.world) * LEVELS_PER_WORLD + u64::from(self.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        let mut ram = vec![0u8; WORK_RAM_LEN];
        ram[addr::OPER_MODE] = OPER_MODE_GAMEPLAY;
        ram[addr::WORLD_NUMBER] = 3;
        ram[addr::LEVEL_NUMBER] = 2;
        ram[addr::PLAYER_PAGE_LOC] = 5;
        ram[addr::PLAYER_X_POSITION] = 0x42;
        ram[addr::PLAYER_STATUS] = 2;
        ram[addr::NUMBER_OF_LIVES] = 4;
        ram[addr::COIN_TALLY] = 37;
        ram
    }

    #[test]
    fn decodes_every_register_from_its_verified_address() {
        let s = decode(&fixture()).unwrap();
        assert_eq!(
            s,
            SmbState {
                game_mode: OPER_MODE_GAMEPLAY,
                world: 3,
                level: 2,
                x_abs: 5 * 256 + 0x42,
                powerup: 2,
                lives: 4,
                coins: 37,
            }
        );
        assert!(s.in_gameplay());
        assert_eq!(s.depth_ordinal(), 3 * 4 + 2);
    }

    #[test]
    fn zeroed_ram_decodes_to_title_screen_world_one() {
        let s = decode(&vec![0u8; WORK_RAM_LEN]).unwrap();
        assert_eq!(s.game_mode, OPER_MODE_TITLE);
        assert!(!s.in_gameplay());
        assert_eq!(s.depth_ordinal(), 0);
        assert_eq!(s.x_abs, 0);
    }

    #[test]
    fn x_abs_carries_the_page() {
        let mut ram = fixture();
        ram[addr::PLAYER_PAGE_LOC] = 0xFF;
        ram[addr::PLAYER_X_POSITION] = 0xFF;
        let s = decode(&ram).unwrap();
        assert_eq!(s.x_abs, 0xFF * 256 + 0xFF);
    }

    #[test]
    fn rejects_short_buffers_without_panicking() {
        for n in [0usize, 1, addr::OPER_MODE, WORK_RAM_LEN - 1] {
            assert_eq!(decode(&vec![0u8; n]), Err(RamError::TooShort { got: n }));
        }
    }

    #[test]
    fn depth_ordinal_is_monotone_in_world_then_level() {
        let mut prev = None;
        for world in 0..8u8 {
            for level in 0..4u8 {
                let s = SmbState {
                    game_mode: OPER_MODE_GAMEPLAY,
                    world,
                    level,
                    x_abs: 0,
                    powerup: 0,
                    lives: 2,
                    coins: 0,
                };
                let d = s.depth_ordinal();
                if let Some(p) = prev {
                    assert!(d > p);
                }
                prev = Some(d);
            }
        }
    }
}
