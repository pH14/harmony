// SPDX-License-Identifier: AGPL-3.0-or-later
// LSE-only definitions for the two GCC outline-atomic ABI helpers referenced
// by libgcc_eh's aarch64 frame-registration lock.

    .arch armv8.1-a+lse
    .text
    .p2align 2

    .global __aarch64_cas8_acq_rel
    .hidden __aarch64_cas8_acq_rel
    .type __aarch64_cas8_acq_rel, %function
__aarch64_cas8_acq_rel:
    casal x0, x1, [x2]
    ret
    .size __aarch64_cas8_acq_rel, .-__aarch64_cas8_acq_rel

    .global __aarch64_swp8_acq_rel
    .hidden __aarch64_swp8_acq_rel
    .type __aarch64_swp8_acq_rel, %function
__aarch64_swp8_acq_rel:
    swpal x0, x0, [x1]
    ret
    .size __aarch64_swp8_acq_rel, .-__aarch64_swp8_acq_rel

    .section .note.GNU-stack,"",%progbits
