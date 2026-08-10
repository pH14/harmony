/* Flat image for the QEMU `virt` / Ampere Altra guest memory map.
 *
 * RAM base is 0x4000_0000. The first two pages are reserved for the harness's
 * shared pages and are deliberately NOT covered by any output section:
 *
 *   0x4000_0000  params page   (harness -> guest: scale index, seed)
 *   0x4000_1000  pvclock page  (harness -> guest: the work-derived clock,
 *                               docs/PARAVIRT-CLOCK.md ABI 1)
 *
 * The image itself loads at RAM + 512 KiB, clear of both.
 */
ENTRY(_start)

SECTIONS
{
    . = 0x40080000;

    __image_start = .;

    /* The vector table must be 2 KiB aligned (VBAR_EL1 has 11 low RES0 bits). */
    .vectors : ALIGN(2048) {
        KEEP(*(.text.vectors))
    }

    .text : ALIGN(4) {
        KEEP(*(.text.boot))
        *(.text .text.*)
    }

    /* Page-align rodata so no executable page carries data words. The AA-4
     * stage-2 execute guard scans whole executable PAGES (it sees pages, not ELF
     * sections); a rodata word sharing the text tail's page can match an
     * exclusive/counter encoding and be rejected as a hazard even though the ELF
     * executable sections are clean (observed on N1 2026-07-20 with aa4-self-modify).
     * Separating code and data onto distinct pages is the W^X layout the guard
     * requires and mirrors the deterministic guest kernel's STRICT_KERNEL_RWX. */
    .rodata : ALIGN(4096) {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(8) {
        *(.data .data.*)
    }

    .bss : ALIGN(16) {
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)
        . = ALIGN(16);
        __bss_end = .;
    }

    /* Boot stack: 64 KiB, above the image. */
    . = ALIGN(4096);
    __stack_bottom = .;
    . += 64K;
    __stack_top = .;

    __image_end = .;

    /DISCARD/ : {
        *(.comment)
        *(.note*)
        *(.eh_frame*)
        *(.ARM.exidx*)
        *(.ARM.attributes)
    }
}
