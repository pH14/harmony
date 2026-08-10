// SPDX-License-Identifier: AGPL-3.0-or-later
/* exception-abort: one translation fault per trip.
 *
 * The faulting address (0x8000_0000) is unmapped **by construction**, not by luck:
 * the boot shim's L1 table maps exactly two 1 GiB blocks (0x0 device, 0x4000_0000
 * normal) and leaves L1[2] invalid. A load from 0x8000_0000 therefore takes a
 * level-1 translation fault, EC 0x25 (data abort from the same EL).
 *
 * Same shape as `svc` but a *different exception class* and no SVC instruction.
 * That is the point: it carries the same entry/ERET pair with no SVC term, so
 * differencing the two at equal trips isolates the SVC weight — and agreeing about
 * the entry/ERET pair across two exception classes is itself evidence that the
 * entry cost is class-independent.
 *
 * Taken branches in the window: trips - 1.
 * Ambiguity terms: trips exception-entries, trips ERETs.
 *
 *   x0 = PL011 base   x1 = trips   ->  x0 = 0
 */

    .section .text, "ax"
    .global oracle_exception_abort
    .type oracle_exception_abort, @function

oracle_exception_abort:
    mrs     x3, vbar_el1
    adrp    x4, abort_vectors
    add     x4, x4, :lo12:abort_vectors
    msr     vbar_el1, x4
    isb

    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_exception_abort_start:

    movz    x5, #0x8000, lsl #16           /* 0x8000_0000: no L1 entry -> fault */
2:  ldr     x6, [x5]                       /* faults, every trip */
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_exception_abort_end:
    strb    w10, [x0]

    msr     vbar_el1, x3
    isb
    mov     x0, #0
    ret
    .size oracle_exception_abort, . - oracle_exception_abort

    .section .text.vectors, "ax"
    .balign 2048
abort_vectors:
    .balign 128                            /* 0x000 sync, Current EL, SP0 */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x080 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x100 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x180 serror */
    b       runtime_unexpected_exception

    /* 0x200 sync, Current EL, SPx <- the data abort.
     *
     * Skip the faulting load and return. Branch-free by construction; it clobbers
     * only x7, which the window body never uses. NZCV is not touched here, and
     * would survive anyway: exception entry saves PSTATE into SPSR_EL1 and ERET
     * restores it, so an abort landing between `subs` and `b.ne` cannot corrupt
     * the loop condition.
     */
    .balign 128
__vec_abort_start:
    mrs     x7, elr_el1
    add     x7, x7, #4
    msr     elr_el1, x7
    eret
__vec_abort_end:

    .balign 128                            /* 0x280 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x300 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x380 serror */
    b       runtime_unexpected_exception

    .balign 128                            /* 0x400 sync, Lower EL, AArch64 */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x480 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x500 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x580 serror */
    b       runtime_unexpected_exception

    .balign 128                            /* 0x600 sync, Lower EL, AArch32 */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x680 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x700 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x780 serror */
    b       runtime_unexpected_exception
