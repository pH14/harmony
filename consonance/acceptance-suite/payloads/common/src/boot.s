/* Multiboot v1 boot shim: 32-bit protected-mode entry -> x86-64 long mode.
 *
 * The header uses the Multiboot "address override" fields (flag bit 16) so
 * the loader never parses the ELF container — QEMU's multiboot ELF path
 * rejects ELF64 images ("Cannot load x86-64 image, give a 32bit one").
 * linker.ld keeps the whole image in one PT_LOAD segment so the file really
 * is the flat image the override fields describe.
 */

.set MB_MAGIC,    0x1BADB002
.set MB_FLAGS,    0x00010000          /* bit 16: address fields valid */
.set MB_CHECKSUM, -(MB_MAGIC + MB_FLAGS)

.section .multiboot_header, "a"
.balign 4
mb_header:
    .long MB_MAGIC
    .long MB_FLAGS
    .long MB_CHECKSUM
    .long mb_header                   /* header_addr   */
    .long __load_start                /* load_addr     */
    .long __load_end                  /* load_end_addr */
    .long __bss_end                   /* bss_end_addr  */
    .long _start                      /* entry_addr    */

.section .text.boot, "ax"
.code32
.global _start
_start:
    /* Multiboot guarantees: protected mode, paging off, A20 on, IF=0, flat
       segments. Assume nothing else. */
    cli
    cld
    movl $__boot_stack_top, %esp

    /* Zero the four boot page-table pages (loaders zero bss, but do not
       depend on it). */
    movl $boot_pml4, %edi
    xorl %eax, %eax
    movl $(4 * 4096 / 4), %ecx
    rep stosl

    /* PML4[0] -> PDPT; PDPT[0] -> PD (low GiB), PDPT[3] -> PD_hi (4th GiB)
       (present | writable). PDPT[3] is the 0xC0000000-0xFFFFFFFF GiB, which
       holds the xAPIC MMIO page at 0xFEE00000 the `irq-landing` payload drives;
       the low-GiB map alone never reaches it. */
    movl $boot_pdpt, %eax
    orl  $0x3, %eax
    movl %eax, boot_pml4
    movl $boot_pd, %eax
    orl  $0x3, %eax
    movl %eax, boot_pdpt
    movl $boot_pd_hi, %eax
    orl  $0x3, %eax
    movl %eax, boot_pdpt + 24            /* PDPT[3] */

    /* PD[0..512]: identity-map the first 1 GiB with 2 MiB pages
       (present | writable | page-size). */
    movl $boot_pd, %edi
    movl $0x83, %eax
    movl $512, %ecx
0:
    movl %eax, (%edi)
    movl $0, 4(%edi)
    addl $0x200000, %eax
    addl $8, %edi
    loop 0b

    /* PD_hi[0..512]: identity-map the 4th GiB (0xC0000000+) with 2 MiB pages,
       so xAPIC MMIO (0xFEE00000) and the rest of the high MMIO hole resolve. */
    movl $boot_pd_hi, %edi
    movl $0xC0000083, %eax               /* 0xC0000000 | present|write|PS */
    movl $512, %ecx
1:
    movl %eax, (%edi)
    movl $0, 4(%edi)
    addl $0x200000, %eax
    addl $8, %edi
    loop 1b

    /* CR4.PAE = 1 — and nothing else; CR4.PCE stays 0, which the rdpmc #GP
       probe in the `features` payload relies on. */
    movl %cr4, %eax
    orl  $(1 << 5), %eax
    movl %eax, %cr4

    movl $boot_pml4, %eax
    movl %eax, %cr3

    /* EFER.LME = 1 */
    movl $0xC0000080, %ecx
    rdmsr
    orl  $(1 << 8), %eax
    wrmsr

    /* CR0.PG = 1 (PE is already set per Multiboot). */
    movl %cr0, %eax
    orl  $0x80000001, %eax
    movl %eax, %cr0

    lgdtl boot_gdt_descr
    ljmpl $0x08, $long_mode

.code64
long_mode:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw %ax, %fs
    movw %ax, %gs
    movl $__boot_stack_top, %esp      /* zero-extends into RSP */
    xorl %ebp, %ebp
    call payload_main

    /* payload_main is `-> !`; if it somehow returns, exit(2). */
    movw $0xF4, %dx
    movb $2, %al
    outb %al, %dx
1:  hlt
    jmp 1b

.section .rodata
.balign 8
boot_gdt:
    .quad 0                           /* null */
    .quad 0x00209A0000000000          /* 0x08: 64-bit code (L=1, P, S, X) */
    .quad 0x0000920000000000          /* 0x10: data (P, S, W) */
boot_gdt_end:
boot_gdt_descr:
    .word boot_gdt_end - boot_gdt - 1
    .long boot_gdt                    /* 32-bit base; loaded before long mode */

.section .bss
.balign 4096
boot_pml4:   .skip 4096
boot_pdpt:   .skip 4096
boot_pd:     .skip 4096
boot_pd_hi:  .skip 4096
.balign 16
boot_stack_bottom:
    .skip 64 * 1024
.global __boot_stack_top
__boot_stack_top:
