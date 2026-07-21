/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * Flat load for aa4-mislabel-evasion, matching the payload guest memory map
 * (RAM base 0x4000_0000; image at RAM + 512 KiB, clear of the params/pvclock
 * pages). .rodata is page-aligned onto its own page so the guard scans a page
 * that holds ONLY the mislabeled hazard — the W^X page layout the guard enforces.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;
    __image_start = .;

    .text : ALIGN(4) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    /* Data-flagged, page-aligned: the planted exclusives live here. Section-aware
     * static scanning excludes this; the runtime guard rescans it on execute. */
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    __image_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.ARM.exidx*)
        *(.ARM.attributes)
    }
}
