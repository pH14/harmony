// SPDX-License-Identifier: AGPL-3.0-or-later
//! The **billboard** wire contract — task 86's always-on, per-frame core-state
//! export, as `film` reads it.
//!
//! Task 86's play-agent writes one pinned guest buffer every vblank: a
//! self-describing [`BillboardHeader`] (magic, layout version, the frame
//! counter, the frame's joypad byte, and the offset/length of each region)
//! followed by the core's full savestate (`retro_serialize`) and the console
//! work RAM. `film` never renders from a reconstruction — it re-runs the exact
//! frame through the *same* commit-pinned core (`core_replay`) — so the only
//! thing it interprets from the buffer is this header: it locates the savestate
//! to hand back to the core, and it **verifies** that the frame the guest
//! stamped is the frame the frame-clock `Moment` addressed (a mismatch is a hard
//! error, never a silently misaligned frame — task 87 §projector).
//!
//! ## Why this lives here (conventions rule 2)
//!
//! Task 86 (the play-agent + the billboard producer) is a sibling spec, unmerged
//! on this branch, so this crate **defines the header layout locally** — exactly
//! as its spec fixes it (magic, version, frame counter, joypad byte, region
//! table) — and codes against it, the same way `resolution` models the unmerged
//! `read`/`regs` verbs. When 86 lands, the integrator reconciles the two against
//! this one byte layout (see `IMPLEMENTATION.md`); nothing in `film`'s observable
//! behaviour depends on which side owns the constant.
//!
//! ## The byte layout (v1) — all little-endian, no padding interpretation
//!
//! | Offset | Size | Field | Notes |
//! |---|---|---|---|
//! | 0 | 4 | `magic` | [`BILLBOARD_MAGIC`] (`b"HBBD"`) |
//! | 4 | 2 | `version` | [`BILLBOARD_LAYOUT_VERSION`]; additive-evolution, a skew is rejected loudly |
//! | 6 | 2 | `flags` | reserved, ignored on read |
//! | 8 | 4 | `frame` | the frame counter — must equal the frame-clock `REG_FRAME` value |
//! | 12 | 1 | `joypad` | the frame's joypad byte (the input `retro_run` will see) |
//! | 13 | 3 | — | reserved padding |
//! | 16 | 4 | `savestate_off` | region offset from the billboard base |
//! | 20 | 4 | `savestate_len` | region length |
//! | 24 | 4 | `workram_off` | region offset |
//! | 28 | 4 | `workram_len` | region length |
//!
//! The header is a fixed [`HEADER_LEN`] bytes; the two regions follow at their
//! declared offsets. [`BillboardHeader::parse`] validates that every declared
//! region lies wholly inside the bytes it was handed, so an accessor
//! ([`BillboardHeader::savestate`] / [`BillboardHeader::work_ram`]) can never
//! index out of range — no panic on a truncated or hostile buffer (rule 4).

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The billboard magic: ASCII `HBBD` (Harmony BillBoarD). A stable format
/// marker independent of [`BILLBOARD_LAYOUT_VERSION`], so a buffer that is not a
/// billboard at all is rejected before any version handling.
pub const BILLBOARD_MAGIC: [u8; 4] = *b"HBBD";

/// The billboard layout version this crate reads. Additive-evolution, like the
/// task-80 [`RegsView`](resolution::RegsView): a bump adds fields; a reader that
/// pins this version rejects a skewed producer loudly
/// ([`HeaderError::VersionSkew`]) rather than mis-parsing a moved field.
pub const BILLBOARD_LAYOUT_VERSION: u16 = 1;

/// The fixed size of the [`BillboardHeader`] prefix, in bytes. The savestate and
/// work-RAM regions follow at their declared offsets.
pub const HEADER_LEN: usize = 32;

/// One region inside the billboard buffer: an offset (from the billboard base)
/// and a length, both in bytes. Kept as a plain pair so the header round-trips
/// byte-for-byte and a consumer can range-check it without floating point
/// (rule 4).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Region {
    /// The region's offset from the billboard base.
    pub offset: u32,
    /// The region's length in bytes.
    pub len: u32,
}

impl Region {
    /// The exclusive end offset (`offset + len`), or `None` on overflow — the
    /// range-check primitive [`BillboardHeader::parse`] uses, so a hostile
    /// `offset`/`len` can never wrap to an in-bounds-looking end.
    fn end(&self) -> Option<u64> {
        u64::from(self.offset).checked_add(u64::from(self.len))
    }

    /// Borrow this region out of `buf`. Only ever called after
    /// [`BillboardHeader::parse`] has proven the region lies inside `buf`, so the
    /// slice indices are statically in range for a buffer of the parsed length;
    /// a caller passing a *shorter* buffer than was parsed gets a clamped empty
    /// slice rather than a panic.
    fn slice<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        let start = self.offset as usize;
        let end = start.saturating_add(self.len as usize);
        buf.get(start..end).unwrap_or(&[])
    }
}

/// The parsed, validated billboard header. A *view* over the buffer's first
/// [`HEADER_LEN`] bytes: it carries the frame identity and the region table, and
/// its accessors borrow the region bytes back out of the same buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct BillboardHeader {
    /// The layout version the producer stamped (always equals
    /// [`BILLBOARD_LAYOUT_VERSION`] once [`parse`](Self::parse) accepts it).
    pub version: u16,
    /// Reserved flag bits, carried verbatim (ignored by v1).
    pub flags: u16,
    /// The frame counter — the `REG_FRAME` value for the frame this billboard
    /// snapshots. [`verify`](Self::verify) asserts it against the frame the
    /// frame-clock `Moment` addressed.
    pub frame: u32,
    /// The joypad byte the guest will present to `retro_run` for this frame — the
    /// input `core_replay` replays. Opaque to `film` (the renderer maps its bits
    /// to libretro input); carried so the render is a pure function of the
    /// capture.
    pub joypad: u8,
    /// The core savestate region (`retro_serialize` output).
    pub savestate: Region,
    /// The console work-RAM region (a stable window for ad-hoc RAM inspection).
    pub work_ram: Region,
}

impl BillboardHeader {
    /// Parse and validate a billboard buffer's header. Total and panic-free on
    /// any input (rule 4): a buffer shorter than [`HEADER_LEN`], a wrong
    /// [`BILLBOARD_MAGIC`], a version skew, or a region running past `buf` is a
    /// distinct [`HeaderError`], never a partial success or an out-of-range
    /// index.
    ///
    /// On success every declared region is proven to lie wholly inside `buf`, so
    /// [`savestate`](Self::savestate) / [`work_ram`](Self::work_ram) return the
    /// real bytes without re-checking.
    pub fn parse(buf: &[u8]) -> Result<BillboardHeader, HeaderError> {
        if buf.len() < HEADER_LEN {
            return Err(HeaderError::TooShort {
                got: buf.len(),
                need: HEADER_LEN,
            });
        }
        let magic: [u8; 4] = buf[0..4].try_into().expect("slice of exactly 4 bytes");
        if magic != BILLBOARD_MAGIC {
            return Err(HeaderError::BadMagic { got: magic });
        }
        let version = u16_le(buf, 4);
        if version != BILLBOARD_LAYOUT_VERSION {
            return Err(HeaderError::VersionSkew {
                got: version,
                want: BILLBOARD_LAYOUT_VERSION,
            });
        }
        let flags = u16_le(buf, 6);
        let frame = u32_le(buf, 8);
        let joypad = buf[12];
        let savestate = Region {
            offset: u32_le(buf, 16),
            len: u32_le(buf, 20),
        };
        let work_ram = Region {
            offset: u32_le(buf, 24),
            len: u32_le(buf, 28),
        };
        let buf_len = buf.len() as u64;
        for (region, which) in [(savestate, "savestate"), (work_ram, "work_ram")] {
            // A region must not overlap the header and must lie wholly inside the
            // buffer the projector actually read. Overflow-safe end (rule 4).
            let end = region.end().ok_or(HeaderError::RegionOverflow { which })?;
            if u64::from(region.offset) < HEADER_LEN as u64 || end > buf_len {
                return Err(HeaderError::RegionOutOfBounds {
                    which,
                    offset: region.offset,
                    len: region.len,
                    buf_len: buf.len(),
                });
            }
        }
        Ok(BillboardHeader {
            version,
            flags,
            frame,
            joypad,
            savestate,
            work_ram,
        })
    }

    /// Verify the frame identity: the frame the guest stamped
    /// ([`frame`](Self::frame)) must equal `expected` — the `REG_FRAME` value of
    /// the frame-clock `Moment` this billboard was read at. A mismatch is a hard
    /// [`HeaderError::FrameMismatch`] (task 87 §projector: never a silently
    /// misaligned frame). Magic and version were already checked by
    /// [`parse`](Self::parse); this is the alignment invariant the projector
    /// asserts per frame.
    pub fn verify(&self, expected: u32) -> Result<(), HeaderError> {
        if self.frame != expected {
            return Err(HeaderError::FrameMismatch {
                got: self.frame,
                want: expected,
            });
        }
        Ok(())
    }

    /// The core savestate bytes, borrowed from the buffer this header was parsed
    /// from. Pass the **same** buffer given to [`parse`](Self::parse); a shorter
    /// buffer yields a clamped empty slice rather than a panic.
    pub fn savestate<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.savestate.slice(buf)
    }

    /// The console work-RAM bytes, borrowed from the buffer this header was
    /// parsed from (same discipline as [`savestate`](Self::savestate)).
    pub fn work_ram<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        self.work_ram.slice(buf)
    }
}

/// Encode a billboard buffer: the [`HEADER_LEN`]-byte header followed by the
/// savestate and work-RAM regions laid out contiguously after it. The single
/// canonical producer for tests and the in-crate mock server — it is the byte
/// form [`BillboardHeader::parse`] round-trips, so the two can never drift.
///
/// The savestate lands at [`HEADER_LEN`]; the work RAM immediately after it. The
/// returned buffer's length is exactly `HEADER_LEN + savestate.len() +
/// work_ram.len()`.
pub fn encode_billboard(frame: u32, joypad: u8, savestate: &[u8], work_ram: &[u8]) -> Vec<u8> {
    let savestate_off = HEADER_LEN as u32;
    let savestate_len = savestate.len() as u32;
    let workram_off = savestate_off + savestate_len;
    let workram_len = work_ram.len() as u32;
    let mut buf = Vec::with_capacity(HEADER_LEN + savestate.len() + work_ram.len());
    buf.extend_from_slice(&BILLBOARD_MAGIC);
    buf.extend_from_slice(&BILLBOARD_LAYOUT_VERSION.to_le_bytes());
    buf.extend_from_slice(&0u16.to_le_bytes()); // flags
    buf.extend_from_slice(&frame.to_le_bytes());
    buf.push(joypad);
    buf.extend_from_slice(&[0u8; 3]); // reserved padding
    buf.extend_from_slice(&savestate_off.to_le_bytes());
    buf.extend_from_slice(&savestate_len.to_le_bytes());
    buf.extend_from_slice(&workram_off.to_le_bytes());
    buf.extend_from_slice(&workram_len.to_le_bytes());
    debug_assert_eq!(buf.len(), HEADER_LEN);
    buf.extend_from_slice(savestate);
    buf.extend_from_slice(work_ram);
    buf
}

/// Read a little-endian `u16` at `off`. The caller has already checked
/// `buf.len() >= HEADER_LEN` and `off + 2 <= HEADER_LEN`, so the slice is
/// statically in range.
fn u16_le(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes(buf[off..off + 2].try_into().expect("2-byte slice"))
}

/// Read a little-endian `u32` at `off` (same in-range invariant as
/// [`u16_le`]).
fn u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().expect("4-byte slice"))
}

/// Why a billboard buffer failed [`BillboardHeader::parse`] or
/// [`BillboardHeader::verify`]. Every variant is a distinct, panic-free
/// rejection — the corruption classes task 87's portable gate enumerates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Error)]
pub enum HeaderError {
    /// The buffer is shorter than a header.
    #[error("billboard buffer is {got} bytes, shorter than the {need}-byte header")]
    TooShort {
        /// The buffer length received.
        got: usize,
        /// The required minimum ([`HEADER_LEN`]).
        need: usize,
    },
    /// The magic did not match [`BILLBOARD_MAGIC`] — not a billboard buffer.
    #[error("bad billboard magic {got:02x?} (expected {:02x?})", BILLBOARD_MAGIC)]
    BadMagic {
        /// The four magic bytes read.
        got: [u8; 4],
    },
    /// The layout version did not match [`BILLBOARD_LAYOUT_VERSION`].
    #[error("billboard layout version skew: got v{got}, this reader is v{want}")]
    VersionSkew {
        /// The version the producer stamped.
        got: u16,
        /// The version this reader pins.
        want: u16,
    },
    /// The stamped frame counter did not match the frame-clock `Moment` this
    /// billboard was read at — a misaligned frame (task 87's hard error).
    #[error("billboard frame mismatch: buffer stamped frame {got}, moment addressed frame {want}")]
    FrameMismatch {
        /// The frame counter the buffer carried.
        got: u32,
        /// The frame the frame-clock `Moment` addressed.
        want: u32,
    },
    /// A region's `offset + len` overflowed `u64` — a hostile region table.
    #[error("billboard {which} region offset+len overflows")]
    RegionOverflow {
        /// Which region (`savestate` / `work_ram`).
        which: &'static str,
    },
    /// A region ran past the buffer (or into the header) — the projector did not
    /// read enough bytes, or the table is corrupt.
    #[error(
        "billboard {which} region [{offset}, {offset}+{len}) is out of bounds for a {buf_len}-byte \
         buffer"
    )]
    RegionOutOfBounds {
        /// Which region (`savestate` / `work_ram`).
        which: &'static str,
        /// The region offset.
        offset: u32,
        /// The region length.
        len: u32,
        /// The buffer length that was read.
        buf_len: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        encode_billboard(7, 0b0000_0011, &[0xAA; 40], &[0x55; 16])
    }

    #[test]
    fn round_trips_a_well_formed_billboard() {
        let buf = sample();
        let h = BillboardHeader::parse(&buf).unwrap();
        assert_eq!(h.version, BILLBOARD_LAYOUT_VERSION);
        assert_eq!(h.frame, 7);
        assert_eq!(h.joypad, 0b0000_0011);
        assert_eq!(h.savestate(&buf), &[0xAA; 40]);
        assert_eq!(h.work_ram(&buf), &[0x55; 16]);
        h.verify(7).unwrap();
    }

    #[test]
    fn header_len_is_exactly_thirty_two() {
        // The encoder and the parser share one constant; a producer emitting a
        // different prefix size would fail the region bounds check, not silently
        // misalign.
        assert_eq!(HEADER_LEN, 32);
        assert_eq!(encode_billboard(0, 0, &[], &[]).len(), HEADER_LEN);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut buf = sample();
        buf[0] ^= 0xFF;
        assert!(matches!(
            BillboardHeader::parse(&buf),
            Err(HeaderError::BadMagic { .. })
        ));
    }

    #[test]
    fn rejects_version_skew() {
        let mut buf = sample();
        // Bump the version field (offset 4) past what this reader pins.
        buf[4] = buf[4].wrapping_add(1);
        assert!(matches!(
            BillboardHeader::parse(&buf),
            Err(HeaderError::VersionSkew {
                want: BILLBOARD_LAYOUT_VERSION,
                ..
            })
        ));
    }

    #[test]
    fn verify_rejects_frame_mismatch() {
        let buf = sample();
        let h = BillboardHeader::parse(&buf).unwrap();
        assert_eq!(
            h.verify(8),
            Err(HeaderError::FrameMismatch { got: 7, want: 8 })
        );
    }

    #[test]
    fn rejects_short_buffer_without_panicking() {
        for n in 0..HEADER_LEN {
            assert!(matches!(
                BillboardHeader::parse(&vec![0u8; n]),
                Err(HeaderError::TooShort { .. })
            ));
        }
    }

    #[test]
    fn rejects_region_running_past_the_buffer() {
        let mut buf = sample();
        // Inflate savestate_len (offset 20) so the region runs past the buffer.
        buf[20..24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            BillboardHeader::parse(&buf),
            Err(HeaderError::RegionOutOfBounds { .. } | HeaderError::RegionOverflow { .. })
        ));
    }

    #[test]
    fn rejects_region_overlapping_the_header() {
        let mut buf = sample();
        // Point the savestate offset (offset 16) into the header itself.
        buf[16..20].copy_from_slice(&0u32.to_le_bytes());
        assert!(matches!(
            BillboardHeader::parse(&buf),
            Err(HeaderError::RegionOutOfBounds { .. })
        ));
    }
}
