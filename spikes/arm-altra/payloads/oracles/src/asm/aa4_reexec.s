// SPDX-License-Identifier: AGPL-3.0-or-later
/* AA-4 notifier-replacement proof target.
 *
 * A single clean instruction (`mov x0, #1; ret`) occupying its own complete
 * 4 KiB executable page. The payload calls it, emits a console marker, and calls
 * it again. Under the execute guard the first call scans+approves the page; a
 * memslot update the harness performs at the marker fires KVM's mmu notifier and
 * clears that approval, so the second call must re-scan at a fresh generation.
 * The page is never modified, so a second scan is caused ONLY by the notifier.
 */

    .arch armv8-a

    .section .text.aa4_reexec_target, "ax"
    .balign 4096
    .global aa4_reexec_target
    .type aa4_reexec_target, @function
aa4_reexec_target:
    mov     x0, #1
    ret
    .size aa4_reexec_target, . - aa4_reexec_target
    .balign 4096
