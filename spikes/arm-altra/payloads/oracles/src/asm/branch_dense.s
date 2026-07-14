/* branch-dense: seven data-dependent branches per trip, four encodings.
 *
 * The branch targets are decided by a xorshift64* stream seeded from the params
 * page, so the taken count is a pure function of (seed, trips) — and the model
 * recomputes that exact function (oracle-model::branch_dense_trip_taken).
 *
 * The `add` on each branch's NOT-taken path accumulates a distinct weight, so the
 * returned accumulator is the exact complement of the taken count: if the
 * accumulator matches the model, every one of the seven predicates matched. That
 * makes the accumulator a real end-to-end check of the predicate model — one the
 * TCG smoke can run, and does, even though TCG can say nothing about counters.
 *
 * Taken branches in the window: (trips - 1) + sum of the per-trip predicate.
 * Ambiguity terms: none.
 *
 *   x0 = PL011 base   x1 = trips   x2 = seed   ->  x0 = the accumulator
 */

    .section .text, "ax"
    .global oracle_branch_dense
    .type oracle_branch_dense, @function

oracle_branch_dense:
    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_branch_dense_start:

    mov     x3, x2                         /* xorshift64* state */
    mov     x11, #0xDD1D                   /* XORSHIFT_MUL = 0x2545F4914F6CDD1D */
    movk    x11, #0x4F6C, lsl #16
    movk    x11, #0xF491, lsl #32
    movk    x11, #0x2545, lsl #48
    mov     x12, #0                        /* accumulator */

2:  eor     x3, x3, x3, lsr #12
    eor     x3, x3, x3, lsl #25
    eor     x3, x3, x3, lsr #27
    mul     x4, x3, x11                    /* the scrambled output; branch on it */

    tbz     x4, #0, 3f                     /* taken when bit 0 is CLEAR */
    add     x12, x12, #1
3:  tbz     x4, #1, 4f
    add     x12, x12, #2
4:  tbz     x4, #2, 5f
    add     x12, x12, #3
5:  tbz     x4, #3, 6f
    add     x12, x12, #4
6:  tbnz    x4, #4, 7f                     /* taken when bit 4 is SET */
    add     x12, x12, #5
7:  tbnz    x4, #5, 8f
    add     x12, x12, #6
8:  and     x13, x4, #0xff
    cbz     x13, 9f                        /* taken when the low byte is zero */
    add     x12, x12, #7
9:  subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_branch_dense_end:
    strb    w10, [x0]

    mov     x0, x12
    ret
    .size oracle_branch_dense, . - oracle_branch_dense
