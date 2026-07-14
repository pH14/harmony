/* clock-page: seqlock reads of the work-derived clock page — AA-5's payload.
 *
 * The guest reads a **materialized** V-time (`docs/PARAVIRT-CLOCK.md` §0): the
 * value is already finished, and the read path contains no arithmetic against any
 * live hardware counter — no `CNTVCT`, no `CNTPCT`, nothing. That is what makes
 * deterministic time possible on silicon whose counter cannot be trapped, which is
 * every reachable ARM server part (no FEAT_ECV).
 *
 * # Why a retry can never happen inside the window — and why it is counted anyway
 *
 * A seqlock retry would add a taken branch no oracle could predict. It cannot
 * occur here, and the argument is structural rather than statistical: the harness
 * can only write the page while the vCPU has **exited**, and a counting window
 * contains no exits (its only MMIO accesses are the two mark stores that delimit
 * it). The page is quiescent for the window's whole duration.
 *
 * The payload nevertheless counts every retry — branch-free, with `CINC` — and
 * reports the total, which AA-5's acceptance requires to be zero. If the argument
 * above is ever wrong, this fails loudly instead of silently perturbing the
 * oracle. An assumption that checks itself is worth more than an assumption.
 *
 * Taken branches in the window: (trips - 1) + retries[reported, must be 0].
 * Ambiguity terms: none.
 *
 *   x0 = PL011 base   x1 = trips   x2 = clock-page GPA   ->  x0 = retries
 */

    .section .text, "ax"
    .global oracle_clock_page
    .type oracle_clock_page, @function

oracle_clock_page:
    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_clock_page_start:

    add     x3, x2, #4                     /* &seq          */
    add     x4, x2, #8                     /* &vns          */
    add     x5, x2, #16                    /* &guest_clock  */
    mov     x6, #0                         /* retries       */
    mov     x7, #0                         /* accumulator   */

2:  ldar    w8, [x3]                       /* seq (acquire) */
    tst     w8, #1
    cinc    x6, x6, ne                     /* count "odd seq", branch-free */
    tbnz    w8, #0, 2b                     /* odd => update in progress */

    ldr     x11, [x4]                      /* materialized vns          */
    ldr     x12, [x5]                      /* materialized guest_clock  */
    dmb     ishld

    ldar    w13, [x3]                      /* seq again */
    cmp     w8, w13
    cinc    x6, x6, ne                     /* count "seq moved", branch-free */
    b.ne    2b                             /* torn => retry */

    add     x7, x7, x11                    /* keep the read live */
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_clock_page_end:
    strb    w10, [x0]

    mov     x0, x6
    ret
    .size oracle_clock_page, . - oracle_clock_page
