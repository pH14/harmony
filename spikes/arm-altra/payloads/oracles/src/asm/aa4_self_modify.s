// SPDX-License-Identifier: AGPL-3.0-or-later
/* AA-4 planted self-modification proof.
 *
 * The target occupies one complete 4 KiB executable page. The first call returns
 * 1. A store from this separate code page replaces that instruction with
 * `mov x0, #2`; cache maintenance makes the new instruction visible; the second
 * call must return 2. Under the execute guard, the store exits before modification
 * and the later call cannot execute until a fresh scan generation is approved.
 */

    .arch armv8-a

    .section .text.aa4_self_modify, "ax"
    .global aa4_self_modify
    .type aa4_self_modify, @function
aa4_self_modify:
    stp     x29, x30, [sp, #-16]!
    mov     x29, sp

    bl      aa4_self_modify_target
    cmp     x0, #1
    b.ne    1f

    adrp    x1, aa4_self_modify_target
    add     x1, x1, :lo12:aa4_self_modify_target
    mov     w2, #0x0040                   /* low half of `mov x0, #2` */
    movk    w2, #0xd280, lsl #16          /* encoding 0xd2800040 */
    str     w2, [x1]

    dc      cvau, x1
    dsb     ish
    ic      ivau, x1
    dsb     ish
    isb

    blr     x1
    cmp     x0, #2
    cset    w0, ne                        /* 0 success, 1 mismatch */
    b       2f
1:
    mov     x0, #1
2:
    ldp     x29, x30, [sp], #16
    ret
    .size aa4_self_modify, . - aa4_self_modify

    .section .text.aa4_self_modify_target, "ax"
    .balign 4096
    .global aa4_self_modify_target
    .type aa4_self_modify_target, @function
aa4_self_modify_target:
    mov     x0, #1                        /* encoding 0xd2800020 */
    ret
    .size aa4_self_modify_target, . - aa4_self_modify_target
    .balign 4096
