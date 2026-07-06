// SPDX-License-Identifier: AGPL-3.0-or-later
// order-super — benchmark bug (ii): an ORDERING / INTERRUPT-TIMING bug (task 69).
// The second planted bug of the seeded-bug benchmark, beside task 60's
// campaign-super.c (bug i). See dissonance/benchmark (BugClass::OrderingInterrupt)
// and guest/linux/IMPLEMENTATION.md §"The benchmark bugs".
//
// The bug in one sentence: a supervised process maintains a two-word invariant
// `mirror == ~primary` that it updates NON-ATOMICALLY inside a small, fixed
// window each iteration; if the guest kernel PREEMPTS the process (an involuntary
// context switch) while it is mid-update — i.e. the injected interrupt landed in
// the window and drove the scheduler — the invariant is observably torn while the
// process is descheduled, corrupting shared bookkeeping, which the process
// detects on resume and aborts with a distinctive serial marker. Outside the
// window the same preemption is harmless (the pair is settled), so the ordering
// assumption ("the pair is never observed inconsistent") holds on every nominal
// run and the guarded branch is dead code.
//
// WHY NOT A POSIX SIGNAL (the milestone-1 review's P1): KVM delivers a task-59
// `InjectInterrupt { vector }` to the guest **kernel IDT**, NOT as a userspace
// signal to this process — an earlier draft that installed a SIGUSR1 handler was
// wrong, the handler would never run. The reachable, userspace-observable effect
// of an injected external interrupt on this single-vCPU guest is a **kernel
// reschedule**: the interrupt (a timer/reschedule-class vector the kernel acts
// on — the manifest's vector is wired to one at box bring-up) makes the kernel
// preempt the running process, an **involuntary context switch** the kernel
// counts in `rusage.ru_nivcsw`. The process samples that counter across the
// update window; a change means a preemption landed inside it. This is the
// mechanism the injected vector actually reaches, observed without any IDT/signal
// plumbing in userspace.
//
// Trigger (tunable — matches dissonance/benchmark manifest BugId(2)):
//   * fault kind:  InjectInterrupt { vector = INTERRUPT_VECTOR } (a reschedule-
//                  class vector the guest kernel acts on; wired at box bring-up).
//   * Moment:      inside [WINDOW_LO, WINDOW_HI) offsets past the base snapshot,
//                  landing in a per-iteration non-atomic update window.
//   * window width is the dial on expected time-to-find (~256 branches).
//
// Determinism: pure integer bookkeeping; scheduling is deterministic under the
// harness, so a fixed (base snapshot, injected schedule) reproduces the torn
// preemption — and thus the crash — every replay. Build: static (`cc -static
// -O2`), like campaign-super.c.

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/io.h>
#include <sys/resource.h>

// The interrupt vector the campaign injects (BugId(2) uses 0x81). Documented
// here for the operator; the value is a manifest dial and is wired to a
// reschedule-class vector at box bring-up (see the header note).
#define INTERRUPT_VECTOR 0x81
// Loop length — long enough (in V-time) that the base snapshot (sealed at
// ORDER_READY) lands well inside it, so the injected interrupt's Moment falls in
// the loop. Box bring-up (milestone 2) tunes this so the base seals inside, the
// same way campaign-super.c's ITERS was tuned.
#define ITERS 20000000L
// isa-debug-exit terminal (same convention as campaign-super.c). FAIL_CODE 0x62
// tags "benchmark bug 2".
#define ISA_DEBUG_EXIT_PORT 0xF4
#define FAIL_CODE 0x62

// The shared bookkeeping. `primary` and `mirror` must always satisfy
// `mirror == ~primary`; `updating` is 1 exactly during the non-atomic window.
static volatile uint64_t primary;
static volatile uint64_t mirror;

// Involuntary context switches so far — the observable a preemption increments.
static uint64_t involuntary_ctxsw(void)
{
    struct rusage r;
    if (getrusage(RUSAGE_SELF, &r) != 0) {
        return 0;
    }
    return (uint64_t)r.ru_nivcsw;
}

// Announce the planted bug: print the distinctive `ORDER_BUG:` marker and write
// the terminal FAIL code to isa-debug-exit (Crash{Panic} where the kernel allows
// it; on the container kernel the routes fail and /init maps the non-zero exit to
// a reboot → Crash{Shutdown}). Never returns.
static void report_and_die(void)
{
    printf("ORDER_BUG: interrupt-ordering invariant violated (torn mid-update preemption)\n");
    fflush(stdout);
    if (ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    if (iopl(3) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    int fd = open("/dev/port", O_WRONLY);
    if (fd >= 0) {
        unsigned char b = FAIL_CODE;
        ssize_t n = pwrite(fd, &b, 1, ISA_DEBUG_EXIT_PORT);
        close(fd);
        (void)n;
    }
    _exit(FAIL_CODE);
}

int main(void)
{
    primary = 0;
    mirror = ~0ULL;

    if (getenv("ORDER_DEBUG")) {
        printf("ORDER_IOPERM: %s\n", ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0 ? "ok" : "FAILED");
        printf("ORDER_VECTOR: injected=0x%x (reschedule-class)\n", INTERRUPT_VECTOR);
        fflush(stdout);
    }

    // The base snapshot is sealed here — mid-workload, right before the
    // ordering-sensitive loop (the campaign's snapshot point).
    printf("ORDER_READY\n");
    fflush(stdout);

    for (long i = 0; i < ITERS; i++) {
        uint64_t sw_before = involuntary_ctxsw();

        // The non-atomic update window: for a few instructions `mirror` is stale
        // w.r.t. `primary`. A preemption HERE leaves the pair observably torn
        // while the process is descheduled.
        primary = primary + 1;   // (1) primary advances — mirror now stale
        // --- vulnerable window: mirror not yet restored ---
        mirror = ~primary;       // (2) mirror catches up — window closed
        // Sample AFTER the window fully closes so the [sw_before, sw_after]
        // interval brackets the ENTIRE torn window (round-7 P2). Sampling before
        // the `mirror = ~primary` store (the earlier draft) left the last sliver
        // of the window — a preemption landing between the sample and the store,
        // still torn — uncounted, so a valid trigger-window interrupt could be
        // missed and the crash would not fire on a genuinely-triggering schedule.
        uint64_t sw_after = involuntary_ctxsw();

        // A change in the involuntary-context-switch count across the window
        // means the kernel preempted us *inside* it — the injected interrupt drove
        // a reschedule at the vulnerable Moment, so the pair was left observably
        // torn while this process was descheduled. That is the ordering violation
        // the planted bug encodes; nominally (no injected interrupt) a
        // deterministic single-vCPU run takes no involuntary preemption here, so
        // the branch is dead code.
        if (sw_after != sw_before) {
            report_and_die();
        }
    }

    printf("ORDER_DONE\n");
    fflush(stdout);
    return 0;
}
