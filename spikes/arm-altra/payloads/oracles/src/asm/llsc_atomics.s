/* llsc-atomics: an LDXR/STXR increment loop — AA-4's (a) payload, the hazard.
 *
 * This is the ONE payload whose taken-branch count is deliberately **not** known
 * by construction, and that is its entire scientific point. An event landing
 * between the load-exclusive and the store-exclusive clears the monitor, `STXR`
 * fails, the loop retries, and the retry's `CBNZ` is a taken branch that no
 * analytical oracle can predict — run-to-run count divergence, exactly the
 * minefield `docs/ARM-PORT.md` and `docs/ARM-ALTRA.md` §4 describe. (The
 * architecture also permits *spurious* `STXR` failure, which is why rr refuses to
 * record LL/SC at all.)
 *
 * So the payload counts its own retries — **branch-free**, by adding the `STXR`
 * status register itself, which is 0 on success and 1 on failure — and reports the
 * total. The count is then `(trips - 1) + retries`, with `retries` a *reported*
 * term rather than a derived one. On a quiescent single-vCPU run with no injection
 * the retries must be zero; under AA-4(a)'s injection schedule they will not be,
 * and quantifying that divergence is the deliverable.
 *
 * Taken branches in the window: (trips - 1) + retries[reported].
 * Ambiguity terms: none.
 *
 * The counter word lives in Normal (cacheable, inner-shareable) memory — which is
 * why this runtime brings the MMU up at all. Exclusives on Device memory are not
 * architecturally defined, so an MMU-off spike could not test this hazard.
 *
 *   x0 = PL011 base   x1 = trips   x2 = &counter   ->  x0 = STXR retries
 */

    .section .text, "ax"
    .global oracle_llsc_atomics
    .type oracle_llsc_atomics, @function

oracle_llsc_atomics:
    str     xzr, [x2]                      /* counter := 0 */

    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_llsc_atomics_start:

    mov     x3, #0                         /* retry counter */
2:  ldxr    x4, [x2]
    add     x4, x4, #1
    stxr    w5, x4, [x2]
    add     x3, x3, x5                     /* w5 is 0 (stored) or 1 (failed) */
    cbnz    w5, 2b                         /* taken exactly once per retry */
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_llsc_atomics_end:
    strb    w10, [x0]

    mov     x0, x3
    ret
    .size oracle_llsc_atomics, . - oracle_llsc_atomics
