// SPDX-License-Identifier: AGPL-3.0-or-later
// order-super — benchmark bug (ii): an ORDERING / INTERRUPT-TIMING bug (task 69).
// The second planted bug of the seeded-bug benchmark, beside task 60's
// campaign-super.c (bug i). See dissonance/benchmark (BugClass::OrderingInterrupt)
// and guest/linux/IMPLEMENTATION.md §"The benchmark bugs".
//
// The bug in one sentence: a supervised process maintains a two-word invariant
// `mirror == ~primary` that it updates NON-ATOMICALLY inside a small, fixed
// window each iteration; a handler for the injected interrupt vector reads that
// pair and, if it observes the pair mid-update (the ordering assumption the code
// relies on — that the pair is never observed inconsistent — is false only then),
// corrupts shared bookkeeping and aborts with a distinctive serial marker. The
// campaign lands an `InjectInterrupt` (task-59 host-fault vocabulary) at a Moment
// inside the vulnerable window; nominally (no interrupt, or an interrupt outside
// the window) the invariant always holds and the branch is dead code.
//
// Trigger (tunable — matches dissonance/benchmark manifest BugId(2)):
//   * fault kind:  InjectInterrupt { vector = INTERRUPT_VECTOR }
//   * Moment:      inside [WINDOW_LO, WINDOW_HI) offsets past the base snapshot
//   * window width HI-LO is the dial on expected time-to-find (~256 branches).
//
// Determinism: pure integer arithmetic, fixed-address bookkeeping, no wall-clock
// / host entropy; the run is a pure function of (base snapshot, injected
// schedule). Build: static (`cc -static -O2`), like campaign-super.c.

#include <fcntl.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/io.h>
#include <sys/mman.h>

// The interrupt the guest kernel routes to SIGUSR1 for this process. On the box
// the campaign's `InjectInterrupt { vector }` is delivered to the vCPU and the
// guest's IDT/signal plumbing surfaces it here; the vector value is the manifest
// dial (BugId(2) uses 0x81). The self-delivery path below models it portably.
#define INTERRUPT_SIGNAL SIGUSR1
// Loop length — long enough (in V-time) that the base snapshot (sealed at
// ORDER_READY) lands well inside it, leaving a wide window for the injected
// interrupt (same reasoning as campaign-super.c's ITERS).
#define ITERS 200000000L
// isa-debug-exit terminal (same convention as campaign-super.c). FAIL_CODE 0x62
// tags "benchmark bug 2".
#define ISA_DEBUG_EXIT_PORT 0xF4
#define FAIL_CODE 0x62

// The shared bookkeeping the handler and the loop both touch. `primary` and
// `mirror` must always satisfy `mirror == ~primary`; `updating` is 1 exactly
// during the non-atomic window the ordering bug lives in.
struct book {
    volatile uint64_t primary;
    volatile uint64_t mirror;
    volatile int updating;
    volatile int violated;
};
static struct book book;

// Report the planted bug and terminate through isa-debug-exit (Crash{Panic}), or
// fall back to a nonzero exit /init turns into a forced reboot (Crash{Shutdown}).
static void report_and_die(void)
{
    printf("ORDER_BUG: interrupt-ordering invariant violated (mirror != ~primary)\n");
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

// The injected-interrupt handler: reads the (primary, mirror) pair. If it lands
// while `updating` is set — i.e. the interrupt arrived inside the non-atomic
// window (the campaign's Moment fell in [WINDOW_LO, WINDOW_HI)) — it observes the
// pair inconsistent and trips the bug. Outside the window `updating` is 0 and the
// pair is always consistent, so the same interrupt is inert (the ordering
// assumption holds).
static void on_interrupt(int sig)
{
    (void)sig;
    if (book.updating && book.mirror != ~book.primary) {
        book.violated = 1;
    }
}

int main(void)
{
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_interrupt;
    sigaction(INTERRUPT_SIGNAL, &sa, NULL);

    book.primary = 0;
    book.mirror = ~0ULL;
    book.updating = 0;
    book.violated = 0;

    if (getenv("ORDER_DEBUG")) {
        // Crash-channel self-test (does not write FAIL) + the interrupt vector
        // this process expects, so the boot serial documents the wiring.
        printf("ORDER_IOPERM: %s\n", ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0 ? "ok" : "FAILED");
        printf("ORDER_VECTOR: signal=%d\n", INTERRUPT_SIGNAL);
        fflush(stdout);
    }

    // The base snapshot is sealed here — mid-workload, right before the
    // ordering-sensitive loop (the campaign's snapshot point).
    printf("ORDER_READY\n");
    fflush(stdout);

    for (long i = 0; i < ITERS; i++) {
        // The non-atomic update window: for a few instructions `mirror` is stale
        // w.r.t. `primary`. A well-ordered execution never observes this; an
        // interrupt delivered HERE does. `updating` brackets the window so the
        // handler can tell "mid-update" from "settled".
        book.updating = 1;
        book.primary = book.primary + 1;   // (1) primary advances
        // --- vulnerable window: mirror not yet restored ---
        book.mirror = ~book.primary;       // (2) mirror catches up
        book.updating = 0;

        if (book.violated) {
            report_and_die();
        }
    }

    printf("ORDER_DONE\n");
    fflush(stdout);
    return 0;
}
