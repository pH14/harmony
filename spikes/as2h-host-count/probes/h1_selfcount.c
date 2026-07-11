// SPDX-License-Identifier: AGPL-3.0-or-later
// AS-2H / H1 — no-root exactness probe for host-side per-thread work counters.
//
// Question: do macOS's always-on per-thread fixed counters (instructions,
// cycles), read via public-ish interfaces, count a calibrated userspace
// payload EXACTLY and REPEATABLY?
//
// Interfaces probed (neither needs root or entitlements):
//   1. proc_pid_rusage(RUSAGE_INFO_V4): ri_instructions / ri_cycles (task-wide)
//   2. thread_selfcounts(1, ...): current-thread {instructions, cycles}
//
// Payload: `subs/b.ne` loop = exactly 2 instructions and 1 retired branch per
// iteration. For iteration count N the architectural user-instruction delta of
// work(N) is exactly 2N+2 (loop + ret; entry bl is attributed to the caller
// read window).
//
// Output: one JSON line per repetition + a summary line. Exactness = the
// work-window delta distribution collapses to a single value (or a tiny set
// of discrete modes), identical across runs, hosts, and P/E-core placement.

#include <errno.h>
#include <inttypes.h>
#include <libproc.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <unistd.h>

extern int thread_selfcounts(int type, void *buf, size_t nbytes);

struct counts {
    uint64_t ru_instrs;
    uint64_t ru_cycles;
    uint64_t tsc_a; // thread_selfcounts field 0 (expected: instructions)
    uint64_t tsc_b; // thread_selfcounts field 1 (expected: cycles)
    int tsc_err;
};

static int g_pid;

static void read_counts(struct counts *c) {
    struct rusage_info_v4 ri;
    if (proc_pid_rusage(g_pid, RUSAGE_INFO_V4, (rusage_info_t *)&ri) != 0) {
        fprintf(stderr, "proc_pid_rusage failed: %s\n", strerror(errno));
        exit(2);
    }
    c->ru_instrs = ri.ri_instructions;
    c->ru_cycles = ri.ri_cycles;
    uint64_t buf[2] = {0, 0};
    c->tsc_err = 0;
    if (thread_selfcounts(1, buf, sizeof buf) != 0) c->tsc_err = errno;
    c->tsc_a = buf[0];
    c->tsc_b = buf[1];
}

__attribute__((noinline)) static void work(uint64_t iters) {
    if (iters == 0) return;
    __asm__ volatile("1: subs %[i], %[i], #1\n\t"
                     "b.ne 1b\n\t"
                     : [i] "+r"(iters)::"cc");
}

int main(int argc, char **argv) {
    uint64_t iters = argc > 1 ? strtoull(argv[1], NULL, 0) : 1000000;
    int reps = argc > 2 ? atoi(argv[2]) : 1000;
    g_pid = getpid();

    // Reduce (not eliminate) migration noise; architectural counts should be
    // placement-independent — that is part of what we are testing.
    pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

    struct counts a, b;
    // warm-up + interface availability report
    read_counts(&a);
    printf("{\"probe\":\"h1_selfcount\",\"iters\":%" PRIu64 ",\"reps\":%d,"
           "\"tsc_errno\":%d,\"ru_instrs_now\":%" PRIu64 "}\n",
           iters, reps, a.tsc_err, a.ru_instrs);

    for (int r = 0; r < reps; r++) {
        read_counts(&a);
        work(iters);
        read_counts(&b);
        printf("{\"rep\":%d,\"d_ru_instrs\":%" PRIu64 ",\"d_ru_cycles\":%" PRIu64
               ",\"d_tsc_a\":%" PRIu64 ",\"d_tsc_b\":%" PRIu64 "}\n",
               r, b.ru_instrs - a.ru_instrs, b.ru_cycles - a.ru_cycles,
               b.tsc_a - a.tsc_a, b.tsc_b - a.tsc_b);
    }
    return 0;
}
