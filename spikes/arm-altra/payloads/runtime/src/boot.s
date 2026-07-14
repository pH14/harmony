// SPDX-License-Identifier: AGPL-3.0-or-later
/* Boot shim + MMU bring-up for the arm64 oracle payloads.
 *
 * Entry state we assume (and nothing more): EL1 (QEMU `virt` without
 * virtualization=on, and a KVM guest, both start there), MMU off, caches off,
 * interrupts unmasked-or-not. A defensive EL2 -> EL1 drop is included because
 * some loaders enter at EL2; it is never taken on the two environments this
 * spike targets.
 *
 * The MMU is brought up because the atomics payloads REQUIRE it: LDXR/STXR and
 * the LSE atomics are only architecturally defined on Normal memory, and with
 * the MMU off every access is Device-nGnRnE. An MMU-off spike could not test
 * AA-4's hazard at all.
 */

.section .text.boot, "ax"
.global _start
_start:
    msr     daifset, #0xf                  /* mask D, A, I, F while we bring up */

    /* Defensive: if a loader entered at EL2, drop to EL1h. */
    mrs     x0, CurrentEL
    lsr     x0, x0, #2
    cmp     x0, #2
    b.ne    1f
    mov     x0, #(1 << 31)                 /* HCR_EL2.RW: EL1 is AArch64 */
    msr     hcr_el2, x0
    mov     x0, #0x3c5                     /* EL1h, DAIF masked */
    msr     spsr_el2, x0
    adr     x0, 1f
    msr     elr_el2, x0
    eret

1:
    /* Boot stack. */
    adrp    x0, __stack_top
    add     x0, x0, :lo12:__stack_top
    mov     sp, x0

    /* Zero .bss. The loader is not trusted to have done it. */
    adrp    x0, __bss_start
    add     x0, x0, :lo12:__bss_start
    adrp    x1, __bss_end
    add     x1, x1, :lo12:__bss_end
2:
    cmp     x0, x1
    b.hs    3f
    str     xzr, [x0], #8
    b       2b
3:
    bl      mmu_enable
    bl      runtime_init                   /* Rust: vectors, UART, GIC */
    bl      payload_main                   /* Rust: the payload; `-> !` */

    /* payload_main never returns. If it somehow does, park rather than run off
       the end of .text into whatever follows. */
4:
    wfi
    b       4b

/* Identity-map the low 2 GiB with two 1 GiB block descriptors and enable the
 * MMU, D-cache and I-cache.
 *
 *   L1[0] = 0x0000_0000  Device-nGnRnE, PXN|UXN  (GICv3 at 0x0800_0000,
 *                                                 PL011 at 0x0900_0000)
 *   L1[1] = 0x4000_0000  Normal WB inner-shareable, executable  (RAM: the
 *                                                 shared pages, the image, the
 *                                                 atomics' scratch word)
 *   L1[2..] invalid      -> the exception-abort payload's translation fault
 *                           target (a load from 0x8000_0000 faults by
 *                           construction, not by luck).
 */
mmu_enable:
    adrp    x0, __l1_table
    add     x0, x0, :lo12:__l1_table

    /* L1[0]: Device block. UXN(54) | PXN(53) | AF(10) | AttrIdx=0 | block(01) */
    mov     x1, #0x0401
    movk    x1, #0x0060, lsl #48
    str     x1, [x0]

    /* L1[1]: Normal block at 0x4000_0000.
       AF(10) | SH=inner(3<<8) | AttrIdx=1(1<<2) | block(01) = 0x4000_0705 */
    mov     x1, #0x0705
    movk    x1, #0x4000, lsl #16
    str     x1, [x0, #8]

    /* MAIR: attr0 = 0x00 Device-nGnRnE, attr1 = 0xFF Normal WB RW-allocate. */
    mov     x1, #0xff00
    msr     mair_el1, x1

    /* TCR_EL1 = 0x0000_0002_0080_3519:
         T0SZ=25 (39-bit VA)   IRGN0=WBWA   ORGN0=WBWA   SH0=inner
         TG0=4K                EPD1=1 (TTBR1 unused)     IPS=40-bit          */
    mov     x1, #0x3519
    movk    x1, #0x0080, lsl #16
    movk    x1, #0x0002, lsl #32
    msr     tcr_el1, x1

    msr     ttbr0_el1, x0
    dsb     ish
    isb
    tlbi    vmalle1
    dsb     ish
    isb

    /* SCTLR_EL1: M | C | I. Read-modify-write: the RES1 bits must survive. */
    mrs     x1, sctlr_el1
    orr     x1, x1, #(1 << 0)              /* M: MMU enable   */
    orr     x1, x1, #(1 << 2)              /* C: D-cache      */
    orr     x1, x1, #(1 << 12)             /* I: I-cache      */
    msr     sctlr_el1, x1
    dsb     sy
    isb
    ret

.section .bss
.balign 4096
__l1_table:
    .skip 4096
