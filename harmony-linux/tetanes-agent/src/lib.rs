// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deterministic TetaNES guest-agent core for prescriptive V-time M2.
//!
//! One [`Agent::run_chord`] consumes one exact two-byte payload entry
//! (`buttons`, `hold_frames`), clocks the same TetaNES configuration as the
//! in-process `machine::nes::NesMachine`, mirrors all 2 KiB of WRAM, and emits
//! the cumulative frame count. Input is consumed once per chord; the yield is
//! emitted only after the whole hold, creating the pre-next-fetch snapshot
//! boundary required by the control client.

use std::io::Cursor;

use tetanes_core::{
    control_deck::{Config, ControlDeck, HeadlessMode},
    input::{JoypadBtnState, Player},
    memory::RamState,
};

/// Size of the NES CPU work RAM mirrored into the host-readable guest page.
pub const WRAM_SIZE: usize = 2 * 1024;
/// Largest accepted controller hold, identical to `machine::nes`.
pub const MAX_HOLD_FRAMES: u8 = 120;
/// One ordinary Linux page; the WRAM mirror fits wholly within it.
pub const GUEST_PAGE_SIZE: usize = 4096;

/// SDK operations needed by the emulator loop.
pub trait Channel {
    /// Channel-specific failure.
    type Error;

    /// Consume one exact-length ordered payload entry.
    fn payload_fetch(&mut self, out: &mut [u8]) -> Result<(), Self::Error>;
    /// Report the cumulative emulated-frame count and create a lifecycle yield.
    fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error>;
}

/// Failure to load or advance the guest emulator.
#[derive(Debug, Eq, PartialEq)]
pub enum AgentError<E> {
    /// The SDK/hypercall channel failed.
    Channel(E),
    /// TetaNES rejected the ROM or could not clock a frame.
    Emulator(String),
    /// The caller supplied a mirror other than exactly 2 KiB.
    BadMirrorLength(usize),
}

/// Headless deterministic TetaNES payload state.
#[derive(Debug)]
pub struct Agent {
    deck: ControlDeck,
    frame_count: u64,
}

impl Agent {
    /// Load a ROM with the same power-on RAM and rendering configuration as
    /// the in-process search machine.
    ///
    /// # Errors
    ///
    /// Returns an emulator error when TetaNES rejects the ROM.
    pub fn from_rom_bytes(rom: &[u8]) -> Result<Self, AgentError<core::convert::Infallible>> {
        let mut deck = ControlDeck::with_config(Config {
            ram_state: RamState::AllZeros,
            headless_mode: HeadlessMode::NO_AUDIO | HeadlessMode::NO_VIDEO,
            sram_dir: None,
            run_ahead: 0,
            ..Config::default()
        });
        deck.load_rom("campaign.nes", &mut Cursor::new(rom))
            .map_err(|error| AgentError::Emulator(error.to_string()))?;
        Ok(Self {
            deck,
            frame_count: 0,
        })
    }

    /// Cumulative number of frames clocked since ROM power-on.
    #[must_use]
    pub fn frame_count(&self) -> u64 {
        self.frame_count
    }

    /// Copy the complete WRAM window into the pinned host-readable mirror.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::BadMirrorLength`] unless `mirror` is exactly 2 KiB.
    pub fn mirror_wram<E>(&self, mirror: &mut [u8]) -> Result<(), AgentError<E>> {
        if mirror.len() != WRAM_SIZE {
            return Err(AgentError::BadMirrorLength(mirror.len()));
        }
        mirror.copy_from_slice(self.deck.wram());
        Ok(())
    }

    /// Fetch and execute one complete chord, publish WRAM, then report its
    /// cumulative ending frame. The next payload fetch does not begin until
    /// this call returns.
    ///
    /// # Errors
    ///
    /// Returns a channel error, an emulator error, or a bad mirror length.
    pub fn run_chord<C: Channel>(
        &mut self,
        channel: &mut C,
        mirror: &mut [u8],
    ) -> Result<u64, AgentError<C::Error>> {
        let mut payload = [0_u8; 2];
        channel
            .payload_fetch(&mut payload)
            .map_err(AgentError::Channel)?;
        let hold = payload[1].clamp(1, MAX_HOLD_FRAMES);
        self.deck.joypad_mut(Player::One).buttons =
            JoypadBtnState::from_bits_truncate(u16::from(payload[0]));
        for _ in 0..hold {
            self.deck
                .clock_frame()
                .map(|_| ())
                .map_err(|error| AgentError::Emulator(error.to_string()))?;
            if self.deck.cpu_corrupted() {
                return Err(AgentError::Emulator(
                    "cpu executed an invalid opcode".to_owned(),
                ));
            }
            self.frame_count = self.frame_count.saturating_add(1);
        }
        self.deck.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
        self.mirror_wram(mirror)?;
        channel
            .frame_complete(self.frame_count)
            .map_err(AgentError::Channel)?;
        Ok(self.frame_count)
    }
}

/// Byte offset of a virtual address's entry in `/proc/self/pagemap`.
#[must_use]
pub const fn pagemap_offset(vaddr: u64) -> u64 {
    (vaddr / GUEST_PAGE_SIZE as u64) * 8
}

/// Decode a present pagemap entry into its guest-physical address.
///
/// # Errors
///
/// Rejects an absent page, a hidden/zero PFN, or overflowing GPA arithmetic.
pub fn decode_pagemap_entry(entry: u64, vaddr: u64) -> Result<u64, String> {
    if entry & (1_u64 << 63) == 0 {
        return Err("WRAM mirror page not present after touch".to_owned());
    }
    let pfn = entry & ((1_u64 << 55) - 1);
    if pfn == 0 {
        return Err("pagemap PFN is zero (need root/CAP_SYS_ADMIN)".to_owned());
    }
    pfn.checked_mul(GUEST_PAGE_SIZE as u64)
        .and_then(|base| base.checked_add(vaddr % GUEST_PAGE_SIZE as u64))
        .ok_or_else(|| "pagemap PFN overflows a u64 GPA".to_owned())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    fn synthetic_nrom() -> Vec<u8> {
        let mut rom = vec![0_u8; 16 + (16 * 1024) + (8 * 1024)];
        rom[..16].copy_from_slice(&[b'N', b'E', b'S', 0x1a, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        let prg = &mut rom[16..16 + (16 * 1024)];
        prg.fill(0xea);
        prg[..3].copy_from_slice(&[0x4c, 0x00, 0x80]);
        for vector in [0x3ffa, 0x3ffc, 0x3ffe] {
            prg[vector..vector + 2].copy_from_slice(&0x8000_u16.to_le_bytes());
        }
        rom
    }

    #[derive(Default)]
    struct Tape {
        payloads: VecDeque<[u8; 2]>,
        reports: Vec<u64>,
    }

    impl Channel for Tape {
        type Error = &'static str;

        fn payload_fetch(&mut self, out: &mut [u8]) -> Result<(), Self::Error> {
            let payload = self.payloads.pop_front().ok_or("exhausted")?;
            out.copy_from_slice(&payload);
            Ok(())
        }

        fn frame_complete(&mut self, frame_count: u64) -> Result<(), Self::Error> {
            self.reports.push(frame_count);
            Ok(())
        }
    }

    #[test]
    fn one_fetch_and_one_yield_bound_each_complete_chord() {
        let mut channel = Tape {
            payloads: [[0x81, 0], [0, 4], [0x02, u8::MAX]].into(),
            ..Tape::default()
        };
        let mut agent = Agent::from_rom_bytes(&synthetic_nrom()).unwrap();
        let mut mirror = [0_u8; WRAM_SIZE];
        assert_eq!(agent.run_chord(&mut channel, &mut mirror), Ok(1));
        assert_eq!(agent.run_chord(&mut channel, &mut mirror), Ok(5));
        assert_eq!(agent.run_chord(&mut channel, &mut mirror), Ok(125));
        assert_eq!(channel.reports, [1, 5, 125]);
        assert_eq!(agent.frame_count(), 125);
    }

    #[test]
    fn guest_loop_matches_an_independent_tetanes_deck_at_every_chord_boundary() {
        let rom = synthetic_nrom();
        let actions = [[0x01, 3], [0x80, 2], [0, 7], [0x42, 4]];
        let mut channel = Tape {
            payloads: actions.into(),
            ..Tape::default()
        };
        let mut agent = Agent::from_rom_bytes(&rom).unwrap();
        let mut mirror = [0_u8; WRAM_SIZE];

        let mut reference = ControlDeck::with_config(Config {
            ram_state: RamState::AllZeros,
            headless_mode: HeadlessMode::NO_AUDIO | HeadlessMode::NO_VIDEO,
            sram_dir: None,
            run_ahead: 0,
            ..Config::default()
        });
        reference
            .load_rom("campaign.nes", &mut Cursor::new(&rom))
            .unwrap();
        let mut frames = 0_u64;
        for [buttons, hold] in actions {
            reference.joypad_mut(Player::One).buttons =
                JoypadBtnState::from_bits_truncate(u16::from(buttons));
            for _ in 0..hold.clamp(1, MAX_HOLD_FRAMES) {
                let _ = reference.clock_frame().unwrap();
                frames += 1;
            }
            reference.joypad_mut(Player::One).buttons = JoypadBtnState::empty();
            assert_eq!(agent.run_chord(&mut channel, &mut mirror), Ok(frames));
            assert_eq!(mirror.as_slice(), reference.wram());
        }
        assert_eq!(channel.reports, [3, 5, 12, 16]);
    }

    #[test]
    fn pagemap_decode_rejects_every_untrusted_failure_shape() {
        let present = 1_u64 << 63;
        assert_eq!(pagemap_offset(0x2000_1234), (0x2000_1234 / 4096) * 8);
        assert!(decode_pagemap_entry(7, 0).is_err());
        assert!(decode_pagemap_entry(present, 0).is_err());
        assert_eq!(
            decode_pagemap_entry(present | 0x1234, 0x7000_0123),
            Ok(0x0123_4123)
        );
        let too_large = present | ((1_u64 << 55) - 1);
        assert!(decode_pagemap_entry(too_large, 0xfff).is_err());
    }
}
