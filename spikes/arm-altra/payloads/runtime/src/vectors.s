/* The runtime's default EL1 vector table.
 *
 * It is deliberately dumb: every one of the 16 entries funnels into a single
 * handler that prints the syndrome and FAILS the payload. Nothing in a payload's
 * *uncounted* code should ever take an exception, so an exception here is a bug
 * and must be loud.
 *
 * The three payloads that take exceptions on purpose (svc, exception-abort,
 * wfi-idle) do NOT use this table. Each installs its own vector table around its
 * counting window, with the handler placed inline in the vector slot. That is
 * what lets the exception path contribute a known number of branch instructions
 * (zero), instead of however many the compiler felt like emitting for a shared
 * dispatcher — the difference between an oracle and a guess.
 */

.section .text.vectors, "ax"
.balign 2048
.global __runtime_vectors
__runtime_vectors:
    .rept 16
    .balign 128
    b       runtime_unexpected_exception
    .endr
