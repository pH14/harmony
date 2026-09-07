// SPDX-License-Identifier: AGPL-3.0-or-later
//! Assemble the ROM-specific overlay for the generic NES guest image.
//!
//! The base image is deliberately ROM-free and carries only the static
//! QuickNES play-agent plus the small BusyBox command surface it needs. This
//! function appends a deterministic `newc` archive whose `/game.nes` and
//! `/init` entries specialize that base for one caller-provided ROM.

use guest_image::Writer;
use std::{error::Error, fmt};

/// The initramfs entrypoint written by [`prepare`].
const INIT_SCRIPT: &[u8] = br##"#!/bin/sh
# The base image supplies BusyBox and the static QuickNES play-agent. This
# overlay supplies only the ROM and the entrypoint that starts the generic
# payload protocol.
BB=/bin/busybox

$BB mount -t proc proc /proc
$BB mount -t sysfs sysfs /sys
$BB mount -t devtmpfs dev /dev 2>/dev/null
$BB mount -t tmpfs tmpfs /tmp
$BB chmod 1777 /tmp
$BB chmod 0666 /dev/console 2>/dev/null

# The billboard is one physically contiguous hugepage mapping. Reserve two so
# a fragmented first reservation cannot starve the agent's allocation.
echo 2 >/proc/sys/vm/nr_hugepages

if [ ! -f /game.nes ]; then
    echo "NES_GUEST_FAIL: /game.nes is missing"
    exec $BB reboot -f
fi
if [ ! -x /opt/harmony/play-agent ]; then
    echo "NES_GUEST_FAIL: static QuickNES play-agent is missing"
    exec $BB reboot -f
fi

echo "NES_READY: launching payload agent"
/opt/harmony/play-agent --nes-payload --rom /game.nes
rc=$?
echo "NES_EXIT: play-agent exited rc=$rc"
if [ "$rc" != "0" ]; then
    exec $BB reboot -f
fi
exec $BB halt -f
"##;

/// Errors returned before any overlay bytes are produced.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PrepareError {
    /// A payload without a ROM cannot initialize the libretro core.
    EmptyRom,
    /// A prepared overlay without a base image is not a runnable guest image.
    EmptyBaseInitramfs,
}

impl fmt::Display for PrepareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyRom => f.write_str("NES ROM is empty"),
            Self::EmptyBaseInitramfs => f.write_str("base NES initramfs is empty"),
        }
    }
}

impl Error for PrepareError {}

/// Append the deterministic ROM and entrypoint overlay to a ROM-free base.
///
/// The returned bytes retain `base_initramfs` byte-for-byte as their prefix,
/// add zero padding to put the overlay on a four-byte boundary, and append one
/// deterministic `newc` archive. Linux's initramfs parser requires raw cpio
/// magic after a compressed member to be four-byte aligned; without this
/// padding the kernel falls back to old-style initrd handling before `/init`
/// runs. Later entries override the base's `/init` entry. The overlay contains
/// no host paths, timestamps, or input-dependent metadata beyond the ROM bytes.
///
/// # Errors
///
/// Returns an error when either input is empty.
pub fn prepare(rom: &[u8], base_initramfs: &[u8]) -> Result<Vec<u8>, PrepareError> {
    if rom.is_empty() {
        return Err(PrepareError::EmptyRom);
    }
    if base_initramfs.is_empty() {
        return Err(PrepareError::EmptyBaseInitramfs);
    }

    let mut overlay = Writer::new();
    overlay.file("game.nes", 0o644, rom);
    overlay.file("init", 0o755, INIT_SCRIPT);
    let overlay = overlay.finish();
    let padding = (4 - (base_initramfs.len() % 4)) % 4;

    let mut prepared = Vec::with_capacity(base_initramfs.len() + padding + overlay.len());
    prepared.extend_from_slice(base_initramfs);
    prepared.resize(prepared.len() + padding, 0);
    prepared.extend_from_slice(&overlay);
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::{INIT_SCRIPT, PrepareError, prepare};

    #[derive(Debug, Eq, PartialEq)]
    struct Entry {
        name: String,
        mode: u32,
        data: Vec<u8>,
    }

    fn hex_field(bytes: &[u8], offset: usize) -> u32 {
        u32::from_str_radix(
            std::str::from_utf8(&bytes[offset..offset + 8]).expect("newc field is utf-8"),
            16,
        )
        .expect("newc field is hexadecimal")
    }

    fn parse_overlay(bytes: &[u8]) -> Vec<Entry> {
        let mut entries = Vec::new();
        let mut offset = 0;
        loop {
            assert_eq!(&bytes[offset..offset + 6], b"070701");
            let mode = hex_field(bytes, offset + 14);
            let size = usize::try_from(hex_field(bytes, offset + 54)).expect("size fits usize");
            let namesize =
                usize::try_from(hex_field(bytes, offset + 94)).expect("name size fits usize");
            let name_start = offset + 110;
            let name_end = name_start + namesize - 1;
            let name = std::str::from_utf8(&bytes[name_start..name_end])
                .expect("newc name is utf-8")
                .to_owned();
            let data_start = (name_start + namesize).next_multiple_of(4);
            let data_end = data_start + size;
            let next = data_end.next_multiple_of(4);
            if name == "TRAILER!!!" {
                assert_eq!(size, 0);
                assert_eq!(next, bytes.len());
                break;
            }
            entries.push(Entry {
                name,
                mode,
                data: bytes[data_start..data_end].to_vec(),
            });
            offset = next;
        }
        entries
    }

    #[test]
    fn preserves_the_base_and_appends_a_deterministic_two_file_overlay() {
        let rom = b"test-rom";
        let base = b"gzip-base";
        let first = prepare(rom, base).expect("prepare image");
        let second = prepare(rom, base).expect("prepare image again");
        assert_eq!(first, second);
        assert_eq!(&first[..base.len()], base);

        let overlay_start = base.len().next_multiple_of(4);
        assert_eq!(overlay_start % 4, 0);
        assert!(
            first[base.len()..overlay_start]
                .iter()
                .all(|&byte| byte == 0)
        );
        let entries = parse_overlay(&first[overlay_start..]);
        assert_eq!(
            entries,
            [
                Entry {
                    name: "game.nes".into(),
                    mode: 0o100644,
                    data: rom.to_vec(),
                },
                Entry {
                    name: "init".into(),
                    mode: 0o100755,
                    data: INIT_SCRIPT.to_vec(),
                },
            ]
        );
    }

    #[test]
    fn aligns_raw_overlay_after_each_gzip_prefix_length() {
        // A valid empty gzip member keeps this fixture independent of host
        // compression tools. The suffixes exercise all four possible prefix
        // alignment classes while leaving the gzip member itself unchanged.
        const EMPTY_GZIP_MEMBER: &[u8] = &[
            0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0xff, 0x03, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];

        for suffix_len in 0..4 {
            let mut base = EMPTY_GZIP_MEMBER.to_vec();
            base.extend(vec![0; suffix_len]);
            let prepared = prepare(b"rom", &base).expect("prepare image");
            let overlay_start = base.len().next_multiple_of(4);
            assert_eq!(&prepared[..base.len()], base.as_slice());
            assert!(
                prepared[base.len()..overlay_start]
                    .iter()
                    .all(|&byte| byte == 0)
            );
            let entries = parse_overlay(&prepared[overlay_start..]);
            assert_eq!(entries[0].name, "game.nes");
            assert_eq!(entries[1].name, "init");
        }
    }

    #[test]
    fn init_runs_only_the_generic_payload_command() {
        let script = std::str::from_utf8(INIT_SCRIPT).expect("script is utf-8");
        assert!(script.contains("/opt/harmony/play-agent --nes-payload --rom /game.nes"));
        assert!(!script.contains("HARMONY_SMB_ROM"));
        assert!(!script.contains("/opt/harmony/smb.nes"));
    }

    #[test]
    fn rejects_missing_rom_or_base_before_allocating_an_overlay() {
        assert_eq!(prepare(&[], b"base"), Err(PrepareError::EmptyRom));
        assert_eq!(prepare(b"rom", &[]), Err(PrepareError::EmptyBaseInitramfs));
    }
}
