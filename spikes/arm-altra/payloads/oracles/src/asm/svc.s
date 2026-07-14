// SPDX-License-Identifier: AGPL-3.0-or-later
/* svc: one `SVC #0` per trip into a one-instruction handler.
 *
 * The payload installs its OWN vector table around the window, with the handler
 * placed inline in the vector slot. That is the whole trick: `SVC` sets ELR_EL1 to
 * the instruction after it, so a bare `ERET` is the entire handler and the
 * exception path contributes **exactly zero branch instructions**. A shared,
 * compiler-generated dispatcher would contribute however many branches it felt
 * like, and the count would stop being known by construction.
 *
 * Taken branches in the window: trips - 1.
 * Ambiguity terms: trips exception-entries, trips ERETs, trips SVCs — each of
 * unknown per-occurrence weight, which is exactly what AA-1 measures. Differencing
 * this payload against exception-abort (same entry/ERET pair, no SVC) at equal
 * trips isolates the SVC term.
 *
 *   x0 = PL011 base   x1 = trips   ->  x0 = 0
 */

    .section .text, "ax"
    .global oracle_svc
    .type oracle_svc, @function

oracle_svc:
    /* Swap in the payload's vector table for the duration of the window. */
    mrs     x3, vbar_el1
    adrp    x4, svc_vectors
    add     x4, x4, :lo12:svc_vectors
    msr     vbar_el1, x4
    isb

    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_svc_start:

2:  svc     #0
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_svc_end:
    strb    w10, [x0]

    msr     vbar_el1, x3                   /* restore the runtime's table */
    isb
    mov     x0, #0
    ret
    .size oracle_svc, . - oracle_svc

/* The payload's vector table. Placed in .text.vectors, which linker.ld aligns to
 * 2 KiB (VBAR_EL1's low 11 bits are RES0).
 *
 * We run at EL1 with SPSel=1, so exceptions taken from EL1 use the "Current EL
 * with SP_ELx" quadrant at offsets 0x200..0x380. Everything else is a bug and
 * funnels to the runtime's loud handler.
 */
    .section .text.vectors, "ax"
    .balign 2048
svc_vectors:
    .balign 128                            /* 0x000 sync, Current EL, SP0 */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x080 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x100 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x180 serror */
    b       runtime_unexpected_exception

    .balign 128                            /* 0x200 sync, Current EL, SPx  <- SVC */
__vec_svc_start:
    eret                                   /* ELR_EL1 already points past the SVC */
__vec_svc_end:

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
