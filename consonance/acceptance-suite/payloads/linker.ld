/* Linked at 1 MiB physical. The payload is loaded via the Multiboot v1
 * "address override" path (header flag bit 16): the loader treats the file as
 * flat and copies it according to the header's load_addr/load_end_addr —
 * hence the single PT_LOAD segment, which keeps file offsets linear in the
 * virtual addresses. (QEMU's ELF multiboot path rejects ELF64 images, so the
 * ELF container is never parsed.)
 */
ENTRY(_start)

PHDRS {
    load PT_LOAD FLAGS(7); /* one RWX segment for everything */
}

SECTIONS {
    . = 1M;
    __load_start = .;

    .boot : {
        KEEP(*(.multiboot_header))
    } :load

    .text : { *(.text .text.*) } :load
    .rodata : { *(.rodata .rodata.*) *(.eh_frame) *(.eh_frame_hdr) } :load
    .data : { *(.data .data.*) *(.got .got.*) } :load

    . = ALIGN(8);
    __load_end = .;

    .bss (NOLOAD) : { *(.bss .bss.*) *(COMMON) } :load
    . = ALIGN(8);
    __bss_end = .;

    /DISCARD/ : { *(.note .note.*) *(.comment) }
}
