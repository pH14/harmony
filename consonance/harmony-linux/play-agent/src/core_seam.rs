// SPDX-License-Identifier: AGPL-3.0-or-later
//! The mock-core seam: the agent's decision and decode logic runs against this
//! trait, so the portable tests never cross the libretro FFI, need a ROM, or
//! need an emulator (task 86 §Environment). The binary's `LibretroCore`
//! (Linux-only, dlopen'd) is the only other implementor.

use crate::ram::{WORK_RAM_LEN, addr};

/// The frame-stepped console core, as the agent drives it. A deliberate
/// minimum: serialize (the billboard's savestate region), one frame of
/// emulation under a held joypad byte, and the console work RAM (the
/// billboard's second region and the RAM-map decode source).
pub trait Core {
    /// The core's savestate size in bytes (stable for a run; queried once at
    /// init to freeze the billboard layout).
    fn serialize_size(&mut self) -> usize;

    /// Serialize the core's full savestate into `out` (exactly
    /// [`serialize_size`](Self::serialize_size) bytes). Returns `false` on
    /// core failure — the agent treats that as fatal (a torn billboard is
    /// worse than a dead agent).
    fn serialize(&mut self, out: &mut [u8]) -> bool;

    /// Run exactly one frame with `joypad` held (NES shift order, see
    /// [`crate::chord::joypad`]).
    fn run_frame(&mut self, joypad: u8);

    /// Copy the 2 KiB console work RAM into `out` (at least [`WORK_RAM_LEN`]
    /// bytes). Returns `false` if the core cannot expose it.
    fn read_work_ram(&mut self, out: &mut [u8]) -> bool;
}

/// The fake core for portable tests and the `--smoke` mode: synthetic console
/// RAM the test plants fixtures in, a toy movement model (RIGHT advances the
/// player's absolute X, with page carry), and a deterministic fake savestate
/// derived from the frame counter.
#[derive(Clone, Debug)]
pub struct MockCore {
    ram: [u8; WORK_RAM_LEN],
    frame: u32,
    savestate_len: usize,
    /// Frames left until a latched `START` press finishes "loading" 1-1
    /// (`None` = no press latched) — the title→gameplay model the start
    /// script is tested against.
    start_countdown: Option<u8>,
    /// Last frame's joypad byte, for edge detection (SMB latches button
    /// edges on its per-frame poll — a held `START` registers once).
    prev_joypad: u8,
}

/// The default fake savestate size — NES-shaped (~20–32 KiB real cores; small
/// here so tests stay fast while still exercising multi-region layouts).
pub const MOCK_SAVESTATE_LEN: usize = 96;

/// Frames the mock "loads" 1-1 for after a latched `START` edge.
const MOCK_START_LOAD_FRAMES: u8 = 3;

impl Default for MockCore {
    fn default() -> Self {
        MockCore::new()
    }
}

impl MockCore {
    /// A mock core at the title screen with zeroed RAM.
    pub fn new() -> Self {
        MockCore {
            ram: [0u8; WORK_RAM_LEN],
            frame: 0,
            savestate_len: MOCK_SAVESTATE_LEN,
            start_countdown: None,
            prev_joypad: 0,
        }
    }

    /// A mock core already in gameplay (OperMode 1, World 1-1, X = 40).
    pub fn in_gameplay() -> Self {
        let mut core = MockCore::new();
        core.ram[addr::OPER_MODE] = crate::ram::OPER_MODE_GAMEPLAY;
        core.ram[addr::PLAYER_X_POSITION] = 40;
        core.ram[addr::NUMBER_OF_LIVES] = 2;
        core
    }

    /// Mutable access to the synthetic console RAM, for planting fixtures
    /// (level transitions, powerups) between steps.
    pub fn ram_mut(&mut self) -> &mut [u8; WORK_RAM_LEN] {
        &mut self.ram
    }

    /// The frames run so far.
    pub fn frames_run(&self) -> u32 {
        self.frame
    }
}

impl Core for MockCore {
    fn serialize_size(&mut self) -> usize {
        self.savestate_len
    }

    fn serialize(&mut self, out: &mut [u8]) -> bool {
        // Deterministic fake savestate: a function of the frame counter and
        // the player position, so distinct moments serialize distinct bytes
        // (the render-determinism tests key on this).
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
        // The title→gameplay model (the start-script seam): a `START` *edge*
        // on the title screen latches a short "load", after which the mock
        // enters gameplay at 1-1 — mirroring SMB's edge-latched per-frame
        // poll. Directional input on the title stays ignored.
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
        // The toy movement model, only during gameplay: RIGHT advances 2 px
        // per frame with page carry; LEFT retreats 1 px within the page.
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

    /// A `START` edge on the title latches the load and enters gameplay after
    /// the mock's load frames; a continuously-held `START` latches only once
    /// (edge semantics, like the real game's per-frame poll).
    #[test]
    fn start_edge_enters_gameplay_after_the_load() {
        use crate::chord::joypad::START;
        let mut core = MockCore::new();
        for _ in 0..MOCK_START_LOAD_FRAMES {
            core.run_frame(START); // held: one edge, then the countdown
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
