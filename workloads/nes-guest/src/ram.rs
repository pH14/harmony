// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SMB RAM-map decode: console work RAM → the game-progress registers.
//!
//! Every address below is verified against the doppelganger SMB disassembly
//! (SMBDIS.ASM — the RAM-map ground truth task 86 names), quoting the exact
//! label lines:
//!
//! ```text
//! OperMode              = $0770
//! PlayerStatus          = $0756
//! NumberofLives         = $075a ;used by current player
//! LevelNumber           = $075c ;the actual dash number
//! CoinTally             = $075e
//! WorldNumber           = $075f
//! Player_PageLoc        = $6d
//! Player_X_Position     = $86
//! ```
//!
//! `LevelNumber` (`$075C`, "the actual dash number" — the on-screen `World-N`)
//! is deliberately used rather than `AreaNumber` (`$0760`, the internal area
//! index): pipe rooms and bonus areas change the area, not the dash number, so
//! the `(world, level)` cell key stays stable across sub-areas of one level.
//! Absolute X is `Player_PageLoc * 256 + Player_X_Position` — monotone within a
//! level (SMB scrolls one way), the clean progress signal the x-bucket rides.
//!
//! The decode is total over any buffer of at least [`WORK_RAM_LEN`] bytes and
//! never panics on hostile bytes (rule 4) — every field is a plain byte read.

use std::fmt;

/// The NES console work RAM size: 2 KiB (`$0000..$07FF`).
pub const WORK_RAM_LEN: usize = 0x800;

/// The verified SMB RAM addresses (see the module doc for the SMBDIS.ASM
/// citations).
pub mod addr {
    /// `OperMode = $0770` — 0 title, 1 gameplay, 2 victory, 3 game over.
    pub const OPER_MODE: usize = 0x0770;
    /// `PlayerStatus = $0756` — 0 small, 1 big, 2 fiery.
    pub const PLAYER_STATUS: usize = 0x0756;
    /// `NumberofLives = $075a`.
    pub const NUMBER_OF_LIVES: usize = 0x075A;
    /// `LevelNumber = $075c` — "the actual dash number" (0-indexed).
    pub const LEVEL_NUMBER: usize = 0x075C;
    /// `CoinTally = $075e`.
    pub const COIN_TALLY: usize = 0x075E;
    /// `WorldNumber = $075f` — 0-indexed (0 = World 1).
    pub const WORLD_NUMBER: usize = 0x075F;
    /// `Player_PageLoc = $6d` — the 256-px page of the player's absolute X.
    pub const PLAYER_PAGE_LOC: usize = 0x6D;
    /// `Player_X_Position = $86` — the within-page X.
    pub const PLAYER_X_POSITION: usize = 0x86;
}

/// `OperMode` value for the title screen.
pub const OPER_MODE_TITLE: u8 = 0;
/// `OperMode` value for gameplay.
pub const OPER_MODE_GAMEPLAY: u8 = 1;
/// `OperMode` value for the victory (castle) sequence.
pub const OPER_MODE_VICTORY: u8 = 2;
/// `OperMode` value for game over.
pub const OPER_MODE_GAME_OVER: u8 = 3;

/// Levels per world in SMB (1-1 … 1-4): the depth-ordinal radix.
pub const LEVELS_PER_WORLD: u64 = 4;

/// The decoded SMB game state — the raw material for the state registers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SmbState {
    /// `OperMode` (see the `OPER_MODE_*` constants).
    pub game_mode: u8,
    /// 0-indexed world number (0 = World 1).
    pub world: u8,
    /// 0-indexed level ("dash") number (0 = x-1).
    pub level: u8,
    /// Absolute X: `Player_PageLoc * 256 + Player_X_Position`.
    pub x_abs: u32,
    /// `PlayerStatus` (0 small, 1 big, 2 fiery).
    pub powerup: u8,
    /// `NumberofLives`.
    pub lives: u8,
    /// `CoinTally`.
    pub coins: u8,
}

/// Why a work-RAM buffer failed to decode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RamError {
    /// The buffer is shorter than the 2 KiB console work RAM.
    TooShort {
        /// The buffer length received.
        got: usize,
    },
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

/// Decode the SMB state out of a console work-RAM image. Total and panic-free
/// for any buffer of at least [`WORK_RAM_LEN`] bytes.
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
    /// Is the game in the gameplay `OperMode`? Registers, depth, and markers are
    /// only meaningful during gameplay (title-screen bytes are menu state).
    pub fn in_gameplay(&self) -> bool {
        self.game_mode == OPER_MODE_GAMEPLAY
    }

    /// The `(world, level)` depth ordinal: `world * 4 + level` — the furthest-
    /// progress metric (`REG_DEPTH`). Warp zones make it jump, which is
    /// legitimate discovered progress (task 86 §play-agent). Monotone tracking
    /// is the host's (`state_max`) — this is the instantaneous ordinal.
    pub fn depth_ordinal(&self) -> u64 {
        u64::from(self.world) * LEVELS_PER_WORLD + u64::from(self.level)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A synthetic work-RAM fixture with every decoded register planted at its
    /// verified address.
    fn fixture() -> Vec<u8> {
        let mut ram = vec![0u8; WORK_RAM_LEN];
        ram[addr::OPER_MODE] = OPER_MODE_GAMEPLAY;
        ram[addr::WORLD_NUMBER] = 3; // World 4
        ram[addr::LEVEL_NUMBER] = 2; // x-3
        ram[addr::PLAYER_PAGE_LOC] = 5;
        ram[addr::PLAYER_X_POSITION] = 0x42;
        ram[addr::PLAYER_STATUS] = 2; // fiery
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
        assert_eq!(s.x_abs, 0xFF * 256 + 0xFF); // max fits u32 comfortably
    }

    #[test]
    fn rejects_short_buffers_without_panicking() {
        for n in [0usize, 1, addr::OPER_MODE, WORK_RAM_LEN - 1] {
            assert_eq!(decode(&vec![0u8; n]), Err(RamError::TooShort { got: n }));
        }
    }

    #[test]
    fn depth_ordinal_is_monotone_in_world_then_level() {
        // 1-1 < 1-2 < ... < 2-1 < ... < 8-4: the ordinal orders (world, level)
        // lexicographically, the property the depth metric relies on.
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
