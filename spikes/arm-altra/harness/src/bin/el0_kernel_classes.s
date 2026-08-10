// SPDX-License-Identifier: AGPL-3.0-or-later
/* AA-1(a) kernel-mediated EL0 windows (host-only; NOT guest payloads).
 *
 * Each window brackets a loop whose only in-window branch INSTRUCTION is the
 * `subs; b.ne` back-edge (executes `trips` times), plus a per-trip
 * kernel-mediated control transfer whose BR_RETIRED contribution is the unknown
 * under measurement. The handlers and the sigreturn restorer are owned asm with
 * branch counts known by construction (each handler: 0 branches + `ret`; the
 * restorer: 0 branches, never returns). All witness accounting (`cinc`, counter
 * loads/stores) is branch-free.
 *
 * The mark stores mirror the guest windows' `strb` marks for structural parity;
 * at EL0 they are plain stores to a writable buffer.
 *
 *   oracle_el0_syscall  (x0 mark, x1 trips)                -> x0 = count of getpid returns equal to EL0_EXPECT_PID
 *   oracle_el0_signal   (x0 mark, x1 trips, x2 pid)        -> x0 = handler hits
 *   oracle_el0_pagefault(x0 mark, x1 trips, x2 fault_page) -> x0 = handler hits
 */

    .section .text, "ax"

    .global oracle_el0_syscall
    .type oracle_el0_syscall, %function
oracle_el0_syscall:
    mov     x9, x0
    mov     x10, x1
    mov     x11, #0                        /* witness accumulator */
    adrp    x12, EL0_EXPECT_PID
    ldr     x12, [x12, :lo12:EL0_EXPECT_PID]
    mov     w13, #0x02
    strb    w13, [x9]                      /* ---- window opens ---- */
1:  mov     x8, #172                       /* __NR_getpid */
    svc     #0
    cmp     x0, x12
    cinc    x11, x11, eq                   /* branch-free witness */
    subs    x10, x10, #1
    b.ne    1b                             /* the only window branch instruction */
    mov     w13, #0x03
    strb    w13, [x9]                      /* ---- window closes ---- */
    mov     x0, x11
    ret
    .size oracle_el0_syscall, . - oracle_el0_syscall

    .global oracle_el0_signal
    .type oracle_el0_signal, %function
oracle_el0_signal:
    mov     x9, x0
    mov     x10, x1
    mov     x14, x2
    adrp    x12, EL0_HANDLER_HITS
    str     xzr, [x12, :lo12:EL0_HANDLER_HITS]
    mov     w13, #0x02
    strb    w13, [x9]                      /* ---- window opens ---- */
1:  mov     x0, x14
    mov     x1, #10                        /* SIGUSR1 */
    mov     x8, #129                       /* __NR_kill */
    svc     #0                             /* delivery lands on syscall return */
    subs    x10, x10, #1
    b.ne    1b
    mov     w13, #0x03
    strb    w13, [x9]                      /* ---- window closes ---- */
    adrp    x12, EL0_HANDLER_HITS
    ldr     x0, [x12, :lo12:EL0_HANDLER_HITS]
    ret
    .size oracle_el0_signal, . - oracle_el0_signal

    .global oracle_el0_pagefault
    .type oracle_el0_pagefault, %function
oracle_el0_pagefault:
    mov     x9, x0
    mov     x10, x1
    mov     x14, x2
    adrp    x12, EL0_HANDLER_HITS
    str     xzr, [x12, :lo12:EL0_HANDLER_HITS]
    mov     w13, #0x02
    strb    w13, [x9]                      /* ---- window opens ---- */
1:  str     x10, [x14]                     /* faults; the handler skips it (pc += 4) */
    subs    x10, x10, #1
    b.ne    1b
    mov     w13, #0x03
    strb    w13, [x9]                      /* ---- window closes ---- */
    adrp    x12, EL0_HANDLER_HITS
    ldr     x0, [x12, :lo12:EL0_HANDLER_HITS]
    ret
    .size oracle_el0_pagefault, . - oracle_el0_pagefault

/* SIGUSR1 handler: count the hit. 0 branches + ret (the ret enters the
 * restorer, which the kernel placed in lr). Caller-saved registers are free to
 * clobber — the interrupted context is restored wholesale by rt_sigreturn. */
    .global el0_signal_handler
    .type el0_signal_handler, %function
el0_signal_handler:
    adrp    x9, EL0_HANDLER_HITS
    ldr     x10, [x9, :lo12:EL0_HANDLER_HITS]
    add     x10, x10, #1
    str     x10, [x9, :lo12:EL0_HANDLER_HITS]
    ret
    .size el0_signal_handler, . - el0_signal_handler

/* SIGSEGV handler (SA_SIGINFO: x0 sig, x1 siginfo, x2 ucontext): skip the
 * faulting instruction by bumping ucontext's saved PC by 4, and count the hit.
 * The PC slot's byte offset inside ucontext_t is computed in Rust
 * (offset_of-based, target-checked) and published in EL0_PC_SLOT_OFFSET —
 * hardcoding a guessed layout here is exactly the silent-wrong-constant class
 * this apparatus refuses. 0 branches + ret. */
    .global el0_segv_handler
    .type el0_segv_handler, %function
el0_segv_handler:
    adrp    x9, EL0_PC_SLOT_OFFSET
    ldr     x9, [x9, :lo12:EL0_PC_SLOT_OFFSET]
    ldr     x10, [x2, x9]
    add     x10, x10, #4
    str     x10, [x2, x9]
    adrp    x9, EL0_HANDLER_HITS
    ldr     x10, [x9, :lo12:EL0_HANDLER_HITS]
    add     x10, x10, #1
    str     x10, [x9, :lo12:EL0_HANDLER_HITS]
    ret
    .size el0_segv_handler, . - el0_segv_handler

/* The owned rt_sigreturn restorer (SA_RESTORER): 0 branches, never returns.
 * Owning it keeps the whole signal return path's branch count known by
 * construction instead of depending on libc's trampoline. */
    .global el0_sig_restorer
    .type el0_sig_restorer, %function
el0_sig_restorer:
    mov     x8, #139                       /* __NR_rt_sigreturn */
    svc     #0
    .size el0_sig_restorer, . - el0_sig_restorer
