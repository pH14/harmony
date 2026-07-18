/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * oracles.h — analytical oracle payloads for the AMD work-clock spike (AE-1/AE-2).
 *
 * docs/AMD-EPYC.md §Evidence integrity #5: count-exactness is judged ONLY against
 * payloads whose retired-taken-branch counts are known BY CONSTRUCTION, never by
 * PMU-vs-PMU comparison (which is circular). Every payload here is a self-contained
 * x86-64 asm loop with the entire control flow written in assembly, so the count is
 * an arithmetic function of the caller's iteration count `n` and nothing the compiler
 * can perturb.
 *
 * The work event is `ex_ret_brn_tkn` — retired *taken* branches (Zen PMCx0C4). The
 * oracle for each class is stated as a per-iteration taken-branch count; the harness
 * uses the DIFFERENTIAL method (run at scales n1 and n2, check
 * `count(n2) - count(n1) == per_iter * (n2 - n1)` exactly) so the constant setup
 * offset cancels and only the per-iteration exactness is under test. A variable
 * differential is a mismatch, not a calibration (doc §AE-1(a)).
 *
 * ISA note (docs/AMD-EPYC.md §Topology): these are pure x86-64 payloads, identical to
 * the Intel det-corpus classes — AMD is the same Arch. The hammer is run first with the
 * Intel event (0x1c4) as an apparatus self-test (hm-8v4), then with 0xc4 on Zen.
 *
 * What counts as a "taken branch" on Zen ExRetBrnTkn (APM Vol 2 §Perf; PPR 17h PMCx0C4):
 * conditional Jcc that jumps, unconditional JMP, CALL, RET, and the loop backedge all
 * redirect control flow and retire as taken. Fall-through Jcc does NOT. A LOCK-prefixed
 * memory op is NOT a branch — that is the whole point of the SpecLockMap probe (class
 * `locked`): its oracle is backedge-only, and any excess is the erratum, not a branch.
 */
#ifndef AMD_EPYC_ORACLES_H
#define AMD_EPYC_ORACLES_H

#include <stdint.h>
#include <string.h>

/* A payload runs `n` iterations of a fixed body and returns a sink value (so the
 * optimizer cannot elide the work). `taken_per_iter` is the analytical oracle: the
 * exact number of retired taken branches contributed by one iteration. `const_taken`
 * is the fixed prologue/epilogue contribution (cancels in the differential; recorded
 * for completeness). Total oracle taken = taken_per_iter*n + const_taken - 1, where the
 * "-1" is the loop's final non-taken fall-through (present in every class); the harness
 * only ever asserts on the DIFFERENTIAL, where this too cancels. */
typedef uint64_t (*payload_fn)(uint64_t n);

typedef struct {
    const char *name;
    payload_fn  fn;
    uint64_t    taken_per_iter;   /* analytical oracle: taken branches per iteration */
} oracle_payload;

/* -------- class: loop_backedge (baseline exactness) --------------------------
 * Body: one non-branch add, then the conditional backedge. Only branch is the
 * backedge. Oracle taken/iter = 1. */
static uint64_t pl_loop_backedge(uint64_t n) {
    uint64_t sink = 0;
    if (!n) return 0;
    __asm__ __volatile__(
        "1:\n\t"
        "addq $1, %[s]\n\t"
        "decq %[n]\n\t"
        "jnz 1b\n\t"
        : [s] "+r"(sink), [n] "+r"(n)
        :
        : "cc");
    return sink;
}

/* -------- class: branch_dense --------------------------------------------------
 * Body: 8 unconditional forward jumps (each always taken) then the backedge.
 * Oracle taken/iter = 8 (jmps) + 1 (backedge) = 9. Every jmp target is the next
 * instruction, so the stream is straight-line in effect but every transfer retires
 * taken — this stresses per-instruction taken attribution. */
static uint64_t pl_branch_dense(uint64_t n) {
    uint64_t sink = 0;
    if (!n) return 0;
    __asm__ __volatile__(
        "1:\n\t"
        "jmp 11f\n11:\t jmp 12f\n12:\t jmp 13f\n13:\t jmp 14f\n"
        "14:\t jmp 15f\n15:\t jmp 16f\n16:\t jmp 17f\n17:\t jmp 18f\n18:\t\n\t"
        "addq $1, %[s]\n\t"
        "decq %[n]\n\t"
        "jnz 1b\n\t"
        : [s] "+r"(sink), [n] "+r"(n)
        :
        : "cc");
    return sink;
}

/* -------- class: call_ret ------------------------------------------------------
 * Body: call a local subroutine that immediately returns, then the backedge.
 * CALL (taken) + RET (taken) + backedge (taken) => oracle taken/iter = 3. The
 * subroutine sits after the loop and is reached only via CALL. */
static uint64_t pl_call_ret(uint64_t n) {
    uint64_t sink = 0;
    if (!n) return 0;
    __asm__ __volatile__(
        "jmp 2f\n\t"
        "3:\n\t"               /* subroutine: just returns */
        "ret\n\t"
        "2:\n\t"
        "1:\n\t"
        "call 3b\n\t"
        "addq $1, %[s]\n\t"
        "decq %[n]\n\t"
        "jnz 1b\n\t"
        : [s] "+r"(sink), [n] "+r"(n)
        :
        : "cc", "memory");
    return sink;
}

/* -------- class: locked (SpecLockMap probe, AE-1(c)) ---------------------------
 * Body: a LOCK-prefixed atomic add to a stack local, then the backedge. A LOCK op
 * is NOT a branch, so the oracle taken/iter = 1 (backedge only). On Zen with
 * speculative lock mapping ENABLED (LS_CFG bit clear, the baseline), the retired-
 * taken-branch event OVERCOUNTS in the presence of locked ops (the erratum rr works
 * around via LS_CFG 0xC0011020). With the workaround applied the differential must
 * be exactly (n2-n1); with it off it exceeds that and varies run-to-run. That gap IS
 * the AE-1(c) evidence. */
static uint64_t pl_locked(uint64_t n) {
    volatile uint64_t mem = 0;
    if (!n) return 0;
    __asm__ __volatile__(
        "1:\n\t"
        "lock addq $1, %[m]\n\t"
        "decq %[n]\n\t"
        "jnz 1b\n\t"
        : [m] "+m"(mem), [n] "+r"(n)
        :
        : "cc", "memory");
    return mem;
}

/* -------- class: straight_line (body-size invariance) --------------------------
 * Body: a run of 16 non-branch ALU ops then the backedge. Oracle taken/iter = 1,
 * identical to loop_backedge — the point is the cross-check that adding 16
 * non-branch instructions changes the taken count by 0 (straight-line contributes
 * nothing), which the harness asserts by differencing straight_line against
 * loop_backedge at equal n. */
static uint64_t pl_straight_line(uint64_t n) {
    uint64_t sink = 0;
    if (!n) return 0;
    __asm__ __volatile__(
        "1:\n\t"
        "addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t"
        "addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t"
        "addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t"
        "addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t addq $1,%[s]\n\t"
        "decq %[n]\n\t"
        "jnz 1b\n\t"
        : [s] "+r"(sink), [n] "+r"(n)
        :
        : "cc");
    return sink;
}

/* The registry the harness iterates. Order is stable (evidence determinism). */
static const oracle_payload ORACLE_PAYLOADS[] = {
    { "loop_backedge", pl_loop_backedge, 1 },   /* single conditional backedge */
    { "branch_dense",  pl_branch_dense,  9 },   /* 8 taken jmps + backedge */
    { "call_ret",      pl_call_ret,      3 },   /* call + ret + backedge */
    { "straight_line", pl_straight_line, 1 },   /* 16 ALU ops + backedge (body-size invariance) */
    { "locked",        pl_locked,        1 },   /* LOCK add + backedge (SpecLockMap probe) */
};
enum { ORACLE_N = sizeof(ORACLE_PAYLOADS) / sizeof(ORACLE_PAYLOADS[0]) };

static inline const oracle_payload *oracle_by_name(const char *name) {
    for (int i = 0; i < ORACLE_N; i++)
        if (strcmp(ORACLE_PAYLOADS[i].name, name) == 0)
            return &ORACLE_PAYLOADS[i];
    return 0;
}

#endif /* AMD_EPYC_ORACLES_H */
