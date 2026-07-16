// SPDX-License-Identifier: AGPL-3.0-or-later
//! The billboard writer — the guest-side producer of task 86's always-on,
//! per-frame core-state export.
//!
//! Local mirror of the billboard wire contract in
//! `dissonance/film/src/billboard.rs` (conventions rule 2: `guest/` crates
//! never depend on `dissonance/`; film defined the layout locally against the
//! same spec, and the integrator reconciles the two onto this one byte layout).
//! The golden test below pins this writer's bytes to film's canonical
//! `encode_billboard` output so the two definitions cannot drift silently.
//!
//! ## The byte layout (v1) — all little-endian
//!
//! | Offset | Size | Field |
//! |---|---|---|
//! | 0 | 4 | magic `b"HBBD"` |
//! | 4 | 2 | layout version (1) |
//! | 6 | 2 | flags (reserved, 0) |
//! | 8 | 4 | frame counter (must equal the `REG_FRAME` value) |
//! | 12 | 1 | the frame's joypad byte |
//! | 13 | 3 | reserved padding (0) |
//! | 16 | 4 | savestate offset (= 32) |
//! | 20 | 4 | savestate length |
//! | 24 | 4 | work-RAM offset (= 32 + savestate length) |
//! | 28 | 4 | work-RAM length (= 2048) |
//!
//! The regions are contiguous: savestate at [`HEADER_LEN`], work RAM
//! immediately after. The layout (and so the buffer's total length) is fixed
//! once at init from the core's `retro_serialize_size` — the buffer's
//! guest-physical address and length are published once via state registers,
//! so the length can never change mid-run.

use std::fmt;

use crate::ram::WORK_RAM_LEN;

/// The billboard magic: ASCII `HBBD` (mirrors `film::billboard::BILLBOARD_MAGIC`).
pub const BILLBOARD_MAGIC: [u8; 4] = *b"HBBD";

/// The layout version this writer stamps (mirrors film's reader pin).
pub const BILLBOARD_LAYOUT_VERSION: u16 = 1;

/// The fixed header size in bytes (mirrors `film::billboard::HEADER_LEN`).
pub const HEADER_LEN: usize = 32;

/// The fixed region layout for one run: header, then the savestate, then the
/// 2 KiB console work RAM, contiguous. Frozen at init from the core's
/// serialize size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BillboardLayout {
    savestate_len: u32,
}

/// Why a billboard write failed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BillboardError {
    /// The destination buffer is smaller than the layout's total length.
    BufferTooSmall {
        /// The buffer length received.
        got: usize,
        /// The layout's required total length.
        need: usize,
    },
    /// The savestate length overflows the u32 region field.
    SavestateTooLarge {
        /// The savestate length requested.
        got: usize,
    },
}

impl fmt::Display for BillboardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BillboardError::BufferTooSmall { got, need } => {
                write!(f, "billboard buffer is {got} bytes, layout needs {need}")
            }
            BillboardError::SavestateTooLarge { got } => {
                write!(f, "savestate of {got} bytes overflows the u32 region field")
            }
        }
    }
}

impl std::error::Error for BillboardError {}

impl BillboardLayout {
    /// Freeze the layout for a run from the core's serialize size.
    pub fn new(savestate_len: usize) -> Result<Self, BillboardError> {
        // The total length must also stay addressable in the u32 length
        // registers; a savestate anywhere near this bound is a broken core.
        let len32 = u32::try_from(savestate_len)
            .ok()
            .filter(|l| (*l as u64) + (HEADER_LEN + WORK_RAM_LEN) as u64 <= u64::from(u32::MAX))
            .ok_or(BillboardError::SavestateTooLarge { got: savestate_len })?;
        Ok(BillboardLayout {
            savestate_len: len32,
        })
    }

    /// The savestate region length.
    pub fn savestate_len(&self) -> usize {
        self.savestate_len as usize
    }

    /// The buffer's total length: header + savestate + work RAM.
    pub fn total_len(&self) -> usize {
        HEADER_LEN + self.savestate_len as usize + WORK_RAM_LEN
    }

    /// Write the 32-byte header for `frame` with `joypad` into `buf`. The
    /// region table is the layout's fixed one; flags are zero.
    pub fn write_header(
        &self,
        buf: &mut [u8],
        frame: u32,
        joypad: u8,
    ) -> Result<(), BillboardError> {
        if buf.len() < self.total_len() {
            return Err(BillboardError::BufferTooSmall {
                got: buf.len(),
                need: self.total_len(),
            });
        }
        let savestate_off = HEADER_LEN as u32;
        let workram_off = savestate_off + self.savestate_len;
        buf[0..4].copy_from_slice(&BILLBOARD_MAGIC);
        buf[4..6].copy_from_slice(&BILLBOARD_LAYOUT_VERSION.to_le_bytes());
        buf[6..8].copy_from_slice(&0u16.to_le_bytes()); // flags
        buf[8..12].copy_from_slice(&frame.to_le_bytes());
        buf[12] = joypad;
        buf[13..16].copy_from_slice(&[0u8; 3]); // reserved padding
        buf[16..20].copy_from_slice(&savestate_off.to_le_bytes());
        buf[20..24].copy_from_slice(&self.savestate_len.to_le_bytes());
        buf[24..28].copy_from_slice(&workram_off.to_le_bytes());
        buf[28..32].copy_from_slice(&(WORK_RAM_LEN as u32).to_le_bytes());
        Ok(())
    }

    /// The mutable savestate region of `buf` (the slice `retro_serialize`
    /// fills). Call only after [`write_header`](Self::write_header) has proven
    /// the buffer long enough for this layout.
    pub fn savestate_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = HEADER_LEN;
        let end = start + self.savestate_len as usize;
        &mut buf[start.min(len)..end.min(len)]
    }

    /// The mutable work-RAM region of `buf` (same discipline as
    /// [`savestate_mut`](Self::savestate_mut)).
    pub fn work_ram_mut<'a>(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
        let len = buf.len();
        let start = HEADER_LEN + self.savestate_len as usize;
        let end = start + WORK_RAM_LEN;
        &mut buf[start.min(len)..end.min(len)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reproduce film's canonical `encode_billboard` byte-for-byte (a test-only
    /// mirror of `dissonance/film/src/billboard.rs::encode_billboard`, quoted
    /// there as "the byte form `BillboardHeader::parse` round-trips").
    fn film_encode_billboard(frame: u32, joypad: u8, savestate: &[u8], work_ram: &[u8]) -> Vec<u8> {
        let savestate_off = HEADER_LEN as u32;
        let savestate_len = savestate.len() as u32;
        let workram_off = savestate_off + savestate_len;
        let workram_len = work_ram.len() as u32;
        let mut buf = Vec::with_capacity(HEADER_LEN + savestate.len() + work_ram.len());
        buf.extend_from_slice(&BILLBOARD_MAGIC);
        buf.extend_from_slice(&BILLBOARD_LAYOUT_VERSION.to_le_bytes());
        buf.extend_from_slice(&0u16.to_le_bytes());
        buf.extend_from_slice(&frame.to_le_bytes());
        buf.push(joypad);
        buf.extend_from_slice(&[0u8; 3]);
        buf.extend_from_slice(&savestate_off.to_le_bytes());
        buf.extend_from_slice(&savestate_len.to_le_bytes());
        buf.extend_from_slice(&workram_off.to_le_bytes());
        buf.extend_from_slice(&workram_len.to_le_bytes());
        buf.extend_from_slice(savestate);
        buf.extend_from_slice(work_ram);
        buf
    }

    /// The writer's bytes must equal film's canonical encoder byte-for-byte —
    /// but with the full 2 KiB work RAM (this producer's fixed region size).
    #[test]
    fn writer_matches_films_canonical_encoding() {
        let savestate: Vec<u8> = (0..40u8).map(|b| b.wrapping_mul(7)).collect();
        let work_ram: Vec<u8> = (0..WORK_RAM_LEN as u32).map(|b| (b % 251) as u8).collect();
        let expected = film_encode_billboard(7, 0b0000_0011, &savestate, &work_ram);

        let layout = BillboardLayout::new(savestate.len()).unwrap();
        let mut buf = vec![0u8; layout.total_len()];
        layout.write_header(&mut buf, 7, 0b0000_0011).unwrap();
        layout.savestate_mut(&mut buf).copy_from_slice(&savestate);
        layout.work_ram_mut(&mut buf).copy_from_slice(&work_ram);
        assert_eq!(buf, expected);
    }

    /// A hard golden pin of the 32 header bytes, so a drift in any constant is
    /// a red test with the exact bytes in the diff (the header does not depend
    /// on region contents).
    #[test]
    fn header_bytes_golden() {
        let layout = BillboardLayout::new(0x40).unwrap();
        let mut buf = vec![0u8; layout.total_len()];
        layout.write_header(&mut buf, 0x0102_0304, 0xA5).unwrap();
        #[rustfmt::skip]
        let expected: [u8; HEADER_LEN] = [
            b'H', b'B', b'B', b'D', // magic
            0x01, 0x00,             // version 1 LE
            0x00, 0x00,             // flags
            0x04, 0x03, 0x02, 0x01, // frame LE
            0xA5,                   // joypad
            0x00, 0x00, 0x00,       // reserved
            0x20, 0x00, 0x00, 0x00, // savestate_off = 32
            0x40, 0x00, 0x00, 0x00, // savestate_len = 0x40
            0x60, 0x00, 0x00, 0x00, // workram_off = 32 + 0x40
            0x00, 0x08, 0x00, 0x00, // workram_len = 2048
        ];
        assert_eq!(&buf[..HEADER_LEN], &expected);
    }

    #[test]
    fn total_len_is_header_plus_regions() {
        let layout = BillboardLayout::new(24_576).unwrap();
        assert_eq!(layout.total_len(), 32 + 24_576 + 2048);
        assert_eq!(layout.savestate_len(), 24_576);
    }

    #[test]
    fn rejects_short_buffers_and_oversized_savestates() {
        let layout = BillboardLayout::new(64).unwrap();
        let mut short = vec![0u8; layout.total_len() - 1];
        assert_eq!(
            layout.write_header(&mut short, 0, 0),
            Err(BillboardError::BufferTooSmall {
                got: layout.total_len() - 1,
                need: layout.total_len(),
            })
        );
        assert!(matches!(
            BillboardLayout::new(usize::MAX),
            Err(BillboardError::SavestateTooLarge { .. })
        ));
        assert!(matches!(
            BillboardLayout::new(u32::MAX as usize),
            Err(BillboardError::SavestateTooLarge { .. })
        ));
    }

    /// Region accessors are clamped, never panicking, even on a wrongly-sized
    /// buffer (rule 4).
    #[test]
    fn region_accessors_clamp_on_short_buffers() {
        let layout = BillboardLayout::new(64).unwrap();
        let mut tiny = vec![0u8; 8];
        assert!(layout.savestate_mut(&mut tiny).is_empty());
        assert!(layout.work_ram_mut(&mut tiny).is_empty());
    }
}
