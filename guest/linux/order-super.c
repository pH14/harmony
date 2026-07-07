// SPDX-License-Identifier: AGPL-3.0-or-later
// order-super — benchmark bug (ii): an ORDERING / INTERRUPT-TIMING bug (task 69).
// The second planted bug of the seeded-bug benchmark, beside task 60's
// campaign-super.c (bug i). See dissonance/benchmark (BugClass::OrderingInterrupt)
// and guest/linux/IMPLEMENTATION.md §"The benchmark bugs".
//
// The bug in one sentence: a supervised process maintains a two-word invariant
// `mirror == ~primary` that it updates NON-ATOMICALLY inside a small, fixed
// window each iteration; if an injected external interrupt is SERVICED by the
// guest kernel while the process is mid-update — i.e. the interrupt's Moment
// landed in the window — the handler ran between the two half-updates, an
// ordering the code assumes never happens, corrupting shared bookkeeping, which
// the process detects (a jump in the kernel's serviced-interrupt count) and
// aborts with a distinctive serial marker. Outside the window the same interrupt
// is harmless (the pair is settled), so the ordering assumption ("no interrupt is
// serviced mid-update") holds on every nominal run and the guarded branch is dead
// code.
//
// WHY A COUNTER, NOT PREEMPTION (milestone-2 box calibration, 2026-07-07): an
// earlier draft detected the interrupt via an INVOLUNTARY context switch
// (`rusage.ru_nivcsw`), assuming the injected vector drives a reschedule. On THIS
// guest that can NEVER fire — the supervisor is the only runnable userspace task
// (postgres is stopped, /init is blocked in `wait`) and there is no clock-event
// device, so an injected interrupt runs its IDT handler and returns to the SAME
// task; `ru_nivcsw` stays 0 (0/512 fires on the box, confirmed). The reachable,
// single-task-observable effect of a delivered interrupt is that the kernel
// SERVICES it: the total hardware-interrupt count — the first number of
// /proc/stat's `intr` line — increments. The process samples that count across
// the update window; a change means an interrupt was serviced INSIDE it, i.e. the
// injected vector at the vulnerable Moment. No preemption, reschedule, or signal
// plumbing required. (KVM still delivers the task-59 `InjectInterrupt` to the
// guest kernel IDT, not as a userspace signal — the milestone-1 SIGUSR1 draft was
// wrong — but we now observe the *delivery* via the counter, not via a handler.)
// This is only sound because the detection loop is interrupt-QUIET nominally: the
// guest has no timer tick and the serial console is polled, so `intr` does not
// move except when the campaign injects one (verified by the nominal-never-fires
// smoke).
//
// Trigger (tunable — matches dissonance/benchmark manifest BugId(2)):
//   * fault kind:  InjectInterrupt { vector = INTERRUPT_VECTOR } (any vector the
//                  guest kernel SERVICES + counts in /proc/stat `intr`; the exact
//                  vector is calibrated on the box).
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

// The interrupt vector the campaign injects (BugId(2) uses 0x81). Documented
// here for the operator; the value is a manifest dial — any vector the guest
// kernel services + counts in /proc/stat `intr` works (calibrated on the box, see
// the header note). The mint also spreads {vector^0..15}, so the campaign covers a
// small vector neighbourhood around this value.
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
// The normal-work cycle length that drives the bug-agnostic operational logging
// (see the loop). Mirrors campaign-super.c's `BUDGET_MAX/2` so all three supers
// emit the SAME log cadence — the apples-to-apples signal workload (task 69 M2).
#define WORK_CYCLE 500000L

// The shared bookkeeping. `primary` and `mirror` must always satisfy
// `mirror == ~primary`; `updating` is 1 exactly during the non-atomic window.
static volatile uint64_t primary;
static volatile uint64_t mirror;

// The total number of hardware interrupts the kernel has serviced — the first
// number of /proc/stat's `intr` line. A delivered external interrupt increments
// it; sampling before/after the update window detects an interrupt serviced
// INSIDE the window (the injected vector at the vulnerable Moment) — a single-task
// observable that needs no preemption (see the header note). On a single-vCPU
// guest the `intr` line sits near the top of /proc/stat (right after `cpu`/`cpu0`),
// so one pread of the head reaches it; the fd is kept open and `pread` re-snapshots
// the file on each call. Returns 0 on any error (a stuck 0 simply never fires,
// never a false positive).
static uint64_t interrupts_serviced(void)
{
    static int fd = -1;
    if (fd < 0) {
        fd = open("/proc/stat", O_RDONLY);
        if (fd < 0) {
            return 0;
        }
    }
    char buf[1024];
    ssize_t n = pread(fd, buf, sizeof(buf) - 1, 0);
    if (n <= 0) {
        return 0;
    }
    buf[n] = '\0';
    const char *p = strstr(buf, "\nintr ");
    if (!p) {
        return 0;
    }
    return strtoull(p + 6, NULL, 10);
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
        printf("ORDER_VECTOR: injected=0x%x (serviced-interrupt observable)\n", INTERRUPT_VECTOR);
        // Baseline serviced-interrupt count — confirms /proc/stat `intr` is
        // readable + parseable (pre-READY diagnostic; not part of any branch).
        printf("ORDER_INTR0: %llu\n", (unsigned long long)interrupts_serviced());
        fflush(stdout);
    }

    // The base snapshot is sealed here — mid-workload, right before the
    // ordering-sensitive loop (the campaign's snapshot point).
    printf("ORDER_READY\n");
    fflush(stdout);

    // `last_phase` makes the operational lifecycle line (below) fire on
    // transitions only, not every iteration — a periodic health log, not a
    // per-tick flood.
    long last_phase = -1;
    for (long i = 0; i < ITERS; i++) {
        uint64_t intr_before = interrupts_serviced();

        // The non-atomic update window: for a few instructions `mirror` is stale
        // w.r.t. `primary`. An interrupt SERVICED here ran the kernel handler
        // between the two half-updates, leaving the pair observably torn.
        primary = primary + 1;   // (1) primary advances — mirror now stale
        // --- vulnerable window: mirror not yet restored ---
        mirror = ~primary;       // (2) mirror catches up — window closed
        // Sample AFTER the window fully closes so the [intr_before, intr_after]
        // interval brackets the ENTIRE torn window (round-7 P2). Sampling before
        // the `mirror = ~primary` store (the earlier draft) left the last sliver
        // of the window — an interrupt landing between the sample and the store,
        // still torn — uncounted, so a valid trigger-window interrupt could be
        // missed and the crash would not fire on a genuinely-triggering schedule.
        uint64_t intr_after = interrupts_serviced();

        // A jump in the serviced-interrupt count across the window means the kernel
        // serviced an interrupt *inside* it — the injected vector at the vulnerable
        // Moment, whose handler ran mid-update. That is the ordering violation the
        // planted bug encodes; nominally (no injected interrupt, no timer tick,
        // polled serial console) `intr` does not move here, so the branch is dead
        // code.
        if (intr_after != intr_before) {
            report_and_die();
        }

        // Realistic operational logging (task 69 M2 — see IMPLEMENTATION
        // §"guest logging"): a supervised worker emits the periodic
        // health/progress lines a real service would, so the log-template signal
        // (task 67) has a workload to read. Every line is **bug-agnostic by
        // construction** — its content is a function of the worker's NORMAL work
        // counter `i`, chosen WITHOUT reference to the planted trigger (the
        // involuntary-preemption check above); none encodes proximity to the
        // vulnerable interrupt window. Emitted at the loop BOTTOM, OUTSIDE the
        // `[sw_before, sw_after]` measurement window, so its (voluntary) console
        // writes never fall inside the torn window they would otherwise perturb —
        // a write() yields as a *voluntary* switch (`ru_nvcsw`), never the
        // *involuntary* `ru_nivcsw` the trigger keys on, and any injected
        // interrupt landing here (not in the window) is correctly a non-trigger.
        // Identical idiom + messages to campaign-super.c so the signal workload is
        // the SAME across bugs (fairness — do NOT enrich per-bug logging to help
        // the signal).
        long work = i % WORK_CYCLE;
        long phase = (work * 3) / WORK_CYCLE; // {0,1,2} across the normal cycle
        if (phase != last_phase) {
            const char *name = phase <= 0 ? "warmup" : (phase == 1 ? "steady" : "drain");
            printf("supervisor: lifecycle phase %s\n", name);
            if (phase >= 2) {
                printf("supervisor: backpressure engaged, shedding retries\n");
            }
            fflush(stdout);
            last_phase = phase;
        }
        if (i % 4096 == 0) {
            printf("supervisor: checkpoint committed, batch complete\n");
            fflush(stdout);
        }
    }

    printf("ORDER_DONE\n");
    fflush(stdout);
    return 0;
}
