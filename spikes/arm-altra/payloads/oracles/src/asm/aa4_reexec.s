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

/* AA-4 two-vCPU race writer, on its own page. Entered by a SECOND vCPU with the
 * MMU off (reset default), so VA == PA == GPA: it stores to the target GPA in x1
 * in a tight loop. The store goes through stage-2, so the execute guard sees it
 * even with stage-1 disabled. w2 carries the store value; both are set by the
 * harness before the writer vCPU is run. */
    .section .text.aa4_reexec_writer, "ax"
    .balign 4096
    .global aa4_reexec_writer
    .type aa4_reexec_writer, @function
aa4_reexec_writer:
    str     w2, [x1]
    b       aa4_reexec_writer
    .size aa4_reexec_writer, . - aa4_reexec_writer
    .balign 4096
