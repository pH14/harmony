// SPDX-License-Identifier: AGPL-3.0-or-later
/* straight-line: 64 unbranched ALU instructions per trip, one back-edge.
 *
 * The lowest branch density in the set. Together with branch-dense (the highest)
 * it pins the counting window's constant offset from two directions — and a
 * *variable* offset between them is a mismatch, not a calibration (AA-1(a)).
 *
 * Taken branches in the window: trips - 1.  Ambiguity terms: none.
 *
 *   x0 = PL011 base   x1 = trips        ->  x0 = the accumulator
 */

    .section .text, "ax"
    .global oracle_straight_line
    .type oracle_straight_line, @function

oracle_straight_line:
    /* Drain the transmitter BEFORE opening the window. This poll's back-edge is
       a taken branch whose count depends on wall clock, so it must land outside
       the window — and it can, because after the drain nothing is written until
       the MARK_BEGIN store below, and the window's closing store therefore needs
       no poll at all. (payloads/README.md §The counting window.) */
    add     x9, x0, #0x18                  /* PL011 FR */
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b                    /* FR.BUSY: still shifting */

    mov     w10, #0x02                     /* MARK_BEGIN */
    strb    w10, [x0]                      /* ---- window opens at this store ---- */
__win_straight_line_start:

    mov     x11, #0
    mov     x12, #1
2:
    .rept 32
    add     x11, x11, x12
    eor     x12, x12, x11
    .endr
    subs    x1, x1, #1
    b.ne    2b                             /* the only branch in the window */

    mov     w10, #0x03                     /* MARK_END */
__win_straight_line_end:
    strb    w10, [x0]                      /* ---- window closes at this store ---- */

    mov     x0, x11
    ret
    .size oracle_straight_line, . - oracle_straight_line
