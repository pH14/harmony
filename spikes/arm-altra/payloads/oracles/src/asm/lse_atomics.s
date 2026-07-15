// SPDX-License-Identifier: AGPL-3.0-or-later
/* lse-atomics: the same increment with an LSE atomic — AA-4's (b) payload, the
 * answer.
 *
 * `LDADD` performs the read-modify-write as one instruction. There is no monitor
 * to clear, no retry, and therefore no branch whose taken-count depends on
 * anything but the trip count. Semantically identical to llsc-atomics; count-wise
 * it is deterministic by construction. The a/b pair *is* AA-4's argument: run both
 * under the identical injection schedule, show (a) diverges and (b) does not.
 *
 * N1 is Armv8.2 and LSE is mandatory from Armv8.1, so the instruction is present
 * on the target silicon — a fact `ident` reports from `ID_AA64ISAR0_EL1.Atomic`
 * rather than assuming.
 *
 * Taken branches in the window: trips - 1.  Ambiguity terms: none.
 *
 *   x0 = PL011 base   x1 = trips   x2 = &counter   ->  x0 = the last value read
 */

    .arch armv8.1-a

    .section .text, "ax"
    .global oracle_lse_atomics
    .type oracle_lse_atomics, @function

oracle_lse_atomics:
    str     xzr, [x2]                      /* counter := 0 */

    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_lse_atomics_start:

    mov     x3, #1
2:  ldadd   x3, x4, [x2]                   /* atomic; x4 = the pre-add value */
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_lse_atomics_end:
    strb    w10, [x0]

    mov     x0, x4
    ret
    .size oracle_lse_atomics, . - oracle_lse_atomics
