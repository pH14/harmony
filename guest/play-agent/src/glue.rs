// SPDX-License-Identifier: AGPL-3.0-or-later
//! The pure logic the binary's FFI edges depend on, hoisted here so **Miri
//! exercises it** (the flow-agent precedent, round-2 P1): Miri cannot execute
//! `dlopen`/`/dev/mem`/`mmap`, so the binary's `unsafe` blocks are kept to
//! thin FFI edges — every *decision* they rely on (libretro callback
//! responses, joypad-bit mapping, work-RAM copy bounds, hugetlb length
//! validation, pagemap entry decode and offset math) lives here, safe and
//! unit-tested under the interpreter. Each `// SAFETY:` block in `main.rs`
//! cites the invariant proven here.

use crate::ram::WORK_RAM_LEN;

// --- libretro callback decisions -------------------------------------------

/// `RETRO_DEVICE_JOYPAD` (libretro.h, stable ABI).
pub const RETRO_DEVICE_JOYPAD: u32 = 1;
/// `RETRO_MEMORY_SYSTEM_RAM` (libretro.h).
pub const RETRO_MEMORY_SYSTEM_RAM: u32 = 2;
/// `RETRO_ENVIRONMENT_GET_CAN_DUPE` (libretro.h).
pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: u32 = 3;
/// `RETRO_ENVIRONMENT_SET_PIXEL_FORMAT` (libretro.h).
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: u32 = 10;

/// What the environment callback should do for a command — the whole decision,
/// so the FFI edge is a bare match with one pointer write.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EnvResponse {
    /// Accept the advertised pixel format (video is discarded; any format is
    /// fine) — reply `true`, touch nothing.
    AcceptPixelFormat,
    /// `GET_CAN_DUPE`: write `true` through the core's `bool*` and reply
    /// `true` (duplicate frames are fine — the guest never reads pixels).
    CanDupe,
    /// Unsupported — reply `false`, touch nothing. A core that hard-requires
    /// more surfaces it at load time on the box (documented bring-up
    /// friction).
    Unsupported,
}

/// Decide the environment-callback response for `cmd`.
pub fn env_response(cmd: u32) -> EnvResponse {
    match cmd {
        RETRO_ENVIRONMENT_SET_PIXEL_FORMAT => EnvResponse::AcceptPixelFormat,
        RETRO_ENVIRONMENT_GET_CAN_DUPE => EnvResponse::CanDupe,
        _ => EnvResponse::Unsupported,
    }
}

/// The billboard joypad byte's bit for a libretro `RETRO_DEVICE_ID_JOYPAD_*`
/// id: the byte is NES hardware shift order (bit 0 = A, 1 = B, 2 = Select,
/// 3 = Start, 4 = Up, 5 = Down, 6 = Left, 7 = Right — the exact mapping
/// `film::core_replay::joypad_pressed` replays), which is NOT the libretro id
/// order. `None` for ids outside the NES pad.
pub fn joypad_bit(id: u32) -> Option<u8> {
    Some(match id {
        8 => 0, // RETRO_DEVICE_ID_JOYPAD_A
        0 => 1, // ..._B
        2 => 2, // ..._SELECT
        3 => 3, // ..._START
        4 => 4, // ..._UP
        5 => 5, // ..._DOWN
        6 => 6, // ..._LEFT
        7 => 7, // ..._RIGHT
        _ => return None,
    })
}

/// The input-state callback's full decision: report the held `joypad` byte's
/// bit for port 0's NES pad, `0` for every other port/device/id.
pub fn input_state_response(joypad: u8, port: u32, device: u32, id: u32) -> i16 {
    if port != 0 || device != RETRO_DEVICE_JOYPAD {
        return 0;
    }
    match joypad_bit(id) {
        Some(bit) => i16::from((joypad >> bit) & 1),
        None => 0,
    }
}

/// Copy the core's system RAM into the billboard's work-RAM region: bounded by
/// both lengths, zero-filling the tail when the core exposes less than 2 KiB
/// (the region is always fully written — never stale bytes). Returns `false`
/// when `out` cannot hold a full work-RAM region or the source is empty.
pub fn copy_work_ram(core_ram: &[u8], out: &mut [u8]) -> bool {
    if out.len() < WORK_RAM_LEN || core_ram.is_empty() {
        return false;
    }
    let n = core_ram.len().min(WORK_RAM_LEN);
    out[..n].copy_from_slice(&core_ram[..n]);
    out[n..WORK_RAM_LEN].fill(0);
    true
}

// --- billboard pinning: hugetlb length + pagemap decode ---------------------

/// One hugetlb page: 2 MiB on x86-64 (the guest kernel's default hugepage
/// size; `game-init.sh` reserves it via `nr_hugepages`).
pub const HUGE_PAGE: usize = 2 << 20;

/// Validate the billboard length against the single-hugepage mapping the
/// binary allocates — the bound `slice::from_raw_parts_mut(ptr, len)` relies
/// on (`len` proven `<=` the mapping's size before the slice exists).
pub fn validate_billboard_len(len: usize) -> Result<(), String> {
    if len == 0 || len > HUGE_PAGE {
        return Err(format!("billboard len {len} must be in 1..={HUGE_PAGE}"));
    }
    Ok(())
}

/// The byte offset of `vaddr`'s entry in `/proc/self/pagemap` (8 bytes per
/// 4 KiB page).
pub fn pagemap_offset(vaddr: u64) -> u64 {
    (vaddr / 4096) * 8
}

/// Decode one `/proc/self/pagemap` entry into the page's physical (inside the
/// VM: guest-physical) address for `vaddr`: bit 63 = present, bits 0..55 =
/// PFN (zero when the reader lacks `CAP_SYS_ADMIN`), gpa = pfn·4096 + the
/// within-page offset. A PFN too large to form a u64 GPA (a 55-bit PFN can
/// exceed it ×4096) is rejected as the corrupt/hostile input it is — library
/// logic never panics on input (rule 4).
pub fn decode_pagemap_entry(entry: u64, vaddr: u64) -> Result<u64, String> {
    if entry & (1 << 63) == 0 {
        return Err("billboard page not present after touch".to_string());
    }
    let pfn = entry & ((1 << 55) - 1);
    if pfn == 0 {
        return Err("pagemap PFN is zero (need root/CAP_SYS_ADMIN to read PFNs)".to_string());
    }
    pfn.checked_mul(4096)
        .and_then(|base| base.checked_add(vaddr % 4096))
        .ok_or_else(|| format!("pagemap PFN {pfn:#x} overflows a u64 GPA — corrupt entry"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::joypad;

    #[test]
    fn env_responses_cover_exactly_the_two_supported_commands() {
        assert_eq!(env_response(10), EnvResponse::AcceptPixelFormat);
        assert_eq!(env_response(3), EnvResponse::CanDupe);
        for cmd in [0u32, 1, 2, 4, 9, 11, 27, 31, 65_581] {
            assert_eq!(env_response(cmd), EnvResponse::Unsupported, "cmd {cmd}");
        }
    }

    /// The id→bit mapping must agree with the chord module's NES shift-order
    /// masks (the billboard byte contract film replays).
    #[test]
    fn joypad_bits_match_the_nes_shift_order_masks() {
        let cases = [
            (8u32, joypad::A),
            (0, joypad::B),
            (2, joypad::SELECT),
            (3, joypad::START),
            (4, joypad::UP),
            (5, joypad::DOWN),
            (6, joypad::LEFT),
            (7, joypad::RIGHT),
        ];
        for (id, mask) in cases {
            let bit = joypad_bit(id).unwrap();
            assert_eq!(1u8 << bit, mask, "libretro id {id}");
        }
        assert_eq!(joypad_bit(1), None); // Y — not on an NES pad
        assert_eq!(joypad_bit(9), None);
    }

    #[test]
    fn input_state_reports_only_port_zero_joypad() {
        let byte = joypad::RIGHT | joypad::A;
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 7), 1); // RIGHT
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 8), 1); // A
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 0), 0); // B not held
        assert_eq!(input_state_response(byte, 1, RETRO_DEVICE_JOYPAD, 7), 0); // port 1
        assert_eq!(input_state_response(byte, 0, 2, 7), 0); // wrong device
        assert_eq!(input_state_response(byte, 0, RETRO_DEVICE_JOYPAD, 42), 0); // bad id
    }

    #[test]
    fn copy_work_ram_clamps_and_zero_fills() {
        // A short core RAM: copied, tail zeroed.
        let src = vec![0xABu8; 100];
        let mut out = vec![0xFFu8; WORK_RAM_LEN + 4];
        assert!(copy_work_ram(&src, &mut out));
        assert!(out[..100].iter().all(|&b| b == 0xAB));
        assert!(out[100..WORK_RAM_LEN].iter().all(|&b| b == 0));
        assert!(
            out[WORK_RAM_LEN..].iter().all(|&b| b == 0xFF),
            "past the region untouched"
        );

        // An oversized core RAM: clamped to the region.
        let src = vec![0x11u8; WORK_RAM_LEN + 999];
        assert!(copy_work_ram(&src, &mut out));
        assert!(out[..WORK_RAM_LEN].iter().all(|&b| b == 0x11));

        // Failure modes: short destination, empty source.
        assert!(!copy_work_ram(&src, &mut vec![0u8; WORK_RAM_LEN - 1]));
        assert!(!copy_work_ram(&[], &mut out));
    }

    #[test]
    fn billboard_len_bounds_are_enforced() {
        assert!(validate_billboard_len(0).is_err());
        assert!(validate_billboard_len(1).is_ok());
        assert!(validate_billboard_len(HUGE_PAGE).is_ok());
        assert!(validate_billboard_len(HUGE_PAGE + 1).is_err());
    }

    #[test]
    fn pagemap_offset_is_eight_bytes_per_page() {
        assert_eq!(pagemap_offset(0), 0);
        assert_eq!(pagemap_offset(4095), 0);
        assert_eq!(pagemap_offset(4096), 8);
        assert_eq!(pagemap_offset(0x2000_1234), (0x2000_1234u64 / 4096) * 8);
    }

    #[test]
    fn pagemap_entries_decode_present_pfn_and_offset() {
        let present = 1u64 << 63;
        // Present, PFN 0x1234, page-aligned vaddr.
        assert_eq!(
            decode_pagemap_entry(present | 0x1234, 0x7000_0000),
            Ok(0x1234 * 4096)
        );
        // The within-page offset rides along.
        assert_eq!(
            decode_pagemap_entry(present | 0x1234, 0x7000_0123),
            Ok(0x1234 * 4096 + 0x123)
        );
        // Not present.
        assert!(decode_pagemap_entry(0x1234, 0x7000_0000).is_err());
        // Present but PFN hidden (no CAP_SYS_ADMIN).
        assert!(decode_pagemap_entry(present, 0x7000_0000).is_err());
        // Bits 55..63 (soft-dirty/exclusive/etc flags) must not leak into the PFN.
        assert_eq!(
            decode_pagemap_entry(present | (1 << 61) | (1 << 55) | 7, 0),
            Ok(7 * 4096)
        );
    }

    /// Round-7 P2: a max-PFN entry (all 55 PFN bits set) cannot form a u64
    /// GPA — an error, never an overflow panic on input-dependent library
    /// logic.
    #[test]
    fn oversized_pfns_are_rejected_not_overflowed() {
        let present = 1u64 << 63;
        let max_pfn = (1u64 << 55) - 1;
        let err = decode_pagemap_entry(present | max_pfn, 0x123).unwrap_err();
        assert!(err.contains("overflows"), "got: {err}");
        // The largest PFN that still fits ×4096 decodes fine (the boundary).
        let largest_ok = u64::MAX / 4096;
        assert_eq!(
            decode_pagemap_entry(present | largest_ok, 0),
            Ok(largest_ok * 4096)
        );
    }
}
