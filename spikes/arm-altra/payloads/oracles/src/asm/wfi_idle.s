// SPDX-License-Identifier: AGPL-3.0-or-later
/* wfi-idle: deterministic idle — mask, make an interrupt pending, WFI, unmask.
 *
 * # Why the wake source is a self-directed SGI and not the virtual timer
 *
 * The obvious idle payload arms the generic timer and sleeps. It is wrong here,
 * and the reason is worth recording because it looks like a downgrade and is not.
 *
 * `WFI` is architecturally permitted to complete **for any reason** — a spurious
 * wake is legal. A timer-woken loop must therefore re-check whether the timer
 * actually fired and sleep again if it did not, and that re-check is a *taken
 * branch whose count depends on wall clock*. Inside a counting window that
 * destroys the oracle: the payload's count would no longer be a function of the
 * payload. Since the whole purpose of this payload is to make `BR_RETIRED`'s
 * treatment of WFI and of interrupt entry/return known by construction, a
 * wall-clock-dependent count is not a blemish, it is a contradiction.
 *
 * A self-directed SGI removes the wait without removing the mechanism: the
 * interrupt is made pending, `WFI` executes (and under KVM still traps to the
 * hypervisor — `HCR_EL2.TWI`), the interrupt is delivered through the GIC, and the
 * exception is entered and returned from. Exactly one interrupt per trip, taken at
 * an instruction fixed by construction (the `ISB` after the unmask), whatever the
 * wall clock did.
 *
 * What this payload therefore does NOT test: that the vCPU really blocks and is
 * really woken by a timer — a *liveness* property, not a counting one. It belongs
 * in an uncounted probe, and AA-5(c)'s Linux-guest boot exercises it for real.
 *
 * Taken branches in the window: trips - 1.
 * Ambiguity terms: trips exception-entries, trips ERETs, trips WFIs.
 *
 *   x0 = PL011 base   x1 = trips   ->  x0 = 0
 */

    .section .text, "ax"
    .global oracle_wfi_idle
    .type oracle_wfi_idle, @function

oracle_wfi_idle:
    mrs     x3, vbar_el1
    adrp    x4, wfi_vectors
    add     x4, x4, :lo12:wfi_vectors
    msr     vbar_el1, x4
    isb

    /* ICC_SGI1R_EL1: INTID 1 in bits [27:24], TargetList bit 0 = this PE,
       Aff1/2/3 = 0 (single-vCPU). */
    movz    x5, #0x0100, lsl #16
    orr     x5, x5, #1

    add     x9, x0, #0x18
1:  ldr     w10, [x9]
    tbnz    w10, #3, 1b

    mov     w10, #0x02
    strb    w10, [x0]
__win_wfi_idle_start:

2:  msr     daifset, #2                    /* mask IRQ */
    msr     icc_sgi1r_el1, x5              /* the interrupt is now pending */
    wfi                                    /* a pending IRQ wakes WFI even masked */
    msr     daifclr, #2                    /* unmask ... */
    isb                                    /* ... and take it HERE, deterministically */
    subs    x1, x1, #1
    b.ne    2b

    mov     w10, #0x03
__win_wfi_idle_end:
    strb    w10, [x0]

    msr     vbar_el1, x3
    isb
    mov     x0, #0
    ret
    .size oracle_wfi_idle, . - oracle_wfi_idle

    .section .text.vectors, "ax"
    .balign 2048
wfi_vectors:
    .balign 128                            /* 0x000 sync, Current EL, SP0 */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x080 irq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x100 fiq */
    b       runtime_unexpected_exception
    .balign 128                            /* 0x180 serror */
    b       runtime_unexpected_exception

    .balign 128                            /* 0x200 sync, Current EL, SPx */
    b       runtime_unexpected_exception

    /* 0x280 irq, Current EL, SPx <- the SGI.
     *
     * Acknowledge and end-of-interrupt. Branch-free; clobbers only x7, which the
     * window body never uses. With ICC_CTLR_EL1.EOImode = 0 (the reset value), the
     * EOIR write both drops priority and deactivates.
     */
    .balign 128
__vec_wfi_start:
    mrs     x7, icc_iar1_el1
    msr     icc_eoir1_el1, x7
    eret
__vec_wfi_end:

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
