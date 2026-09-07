// SPDX-License-Identifier: AGPL-3.0-or-later
// A pause that lands on an instruction rather than on a kernel entry.
//
// The fault agent pauses a process with SIGSTOP, which stops it where the
// scheduler last took the CPU from it. In the guest that is always on the
// way out of a system call, so a race whose window is plain user code is
// out of a pause's reach. A workload compiled with
// -fsanitize-coverage=trace-pc calls __sanitizer_cov_trace_pc at every
// control-flow edge instead, and this file counts those calls and sleeps at
// the one named by FAULTLAB_EDGE, for FAULTLAB_EDGE_SLEEP_US microseconds.
// The sleep is the pause: the other processes run while this one stands
// still inside whatever it was doing. Edge counts are a function of the
// program's input and schedule, so the same number names the same place on
// every run of the same schedule.
//
// The fault agent arms randomized preemption for a window by writing a
// file named by FAULTLAB_JITTER_FILE ("seed every hold_ns"): while it
// exists, this process pauses for hold_ns about every `every` edges, at
// edges drawn from the seed. The file is polled every 65536 edges, so a
// window opens and closes within a millisecond of the agent's tick.
//
// Every pause yields the processor until its hold has elapsed instead of
// sleeping: the guest timer rounds a sleep up to its tick, ten milliseconds,
// and a pause shorter than that is what lets another process reach a race
// window without stalling the workload.
//
// This file is compiled without coverage instrumentation, so the callback
// does not call itself.
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sched.h>
#include <time.h>
#include <unistd.h>

#include "faultlab-edge.h"

static uint64_t edges;
static uint64_t target;
static unsigned sleep_us = 50000;
// A pause may also be placed at the k-th passage of a named reach marker,
// which is how a search aims at a state the markers already name.
static const char *pause_at;
static unsigned long pause_k;
// With FAULTLAB_REACH_ALL set, every passage of every marker is logged with
// the process id, which shows what the other process did during a pause.
static int reach_all;
// The passages of the FAULTLAB_PAUSE_AT marker and the edge of the latest
// one, so a random pause can report how far past that marker it landed.
static unsigned long passages;
static uint64_t passage_edge;
// With FAULTLAB_JITTER_LOG set, every random pause is logged with its edge
// and its distance past the latest marker passage.
static int jitter_log;

static const char *jitter_file;
static int jitter_armed;
static char jitter_seen[96];
static uint64_t jitter_state;
static uint32_t jitter_every;
static uint64_t jitter_hold_ns;
static uint64_t jitter_next;
static unsigned long jitter_pauses;
static uint64_t jitter_slept_ns;

static uint64_t jitter_draw(void) {
    uint64_t z = (jitter_state += 0x9e3779b97f4a7c15ull);
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ull;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebull;
    return z ^ (z >> 31);
}

static void jitter_schedule(uint64_t now) {
    jitter_next = now + 1 + jitter_draw() % (2ull * jitter_every);
}

// Yields until `ns` nanoseconds have passed; returns the time actually held.
static uint64_t hold_ns(uint64_t ns);

static uint64_t now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_MONOTONIC, &t);
    return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

// The file is small and lives on tmpfs, so reading it whole at every poll
// costs less than a system call's worth of edges, and a rewrite with the
// same length is never missed.
static void jitter_refresh(uint64_t now) {
    char line[96] = "";
    FILE *f = fopen(jitter_file, "r");
    if (f) {
        if (!fgets(line, sizeof(line), f)) line[0] = 0;
        fclose(f);
    }
    if (line[0] == 0) {
        if (jitter_armed) {
            fprintf(stderr, "FLJITTER %d off pauses=%lu slept_us=%llu\n", (int)getpid(), jitter_pauses,
                    (unsigned long long)(jitter_slept_ns / 1000));
        }
        jitter_armed = 0;
        jitter_next = 0;
        jitter_seen[0] = 0;
        return;
    }
    if (jitter_armed && strcmp(line, jitter_seen) == 0) return;
    unsigned long long seed = 0, every = 0, hold = 0;
    if (sscanf(line, "%llu %llu %llu", &seed, &every, &hold) != 3 || every == 0) return;
    strcpy(jitter_seen, line);
    jitter_state = seed;
    jitter_every = (uint32_t)every;
    jitter_hold_ns = hold;
    jitter_pauses = 0;
    jitter_slept_ns = 0;
    jitter_armed = 1;
    jitter_schedule(now);
    fprintf(stderr, "FLJITTER %d on seed=%llu every=%llu hold_us=%llu at edge %llu\n", (int)getpid(), seed,
            every, hold / 1000, (unsigned long long)now);
}

static uint64_t hold_ns(uint64_t ns) {
    uint64_t before = now_ns();
    while (now_ns() - before < ns) sched_yield();
    return now_ns() - before;
}

static void jitter_pause(uint64_t now) {
    jitter_slept_ns += hold_ns(jitter_hold_ns);
    jitter_pauses++;
    if (jitter_log || reach_all)
        fprintf(stderr, "FLJITTER %d pause at edge %llu after #%lu +%llu\n", (int)getpid(),
                (unsigned long long)now, passages, (unsigned long long)(now - passage_edge));
    jitter_schedule(now);
}

void faultlab_edge_init(void) {
    const char *t = getenv("FAULTLAB_EDGE");
    if (t) target = strtoull(t, NULL, 10);
    const char *s = getenv("FAULTLAB_EDGE_SLEEP_US");
    if (s) sleep_us = (unsigned)strtoul(s, NULL, 10);
    pause_at = getenv("FAULTLAB_PAUSE_AT");
    const char *k = getenv("FAULTLAB_PAUSE_K");
    if (k) pause_k = strtoul(k, NULL, 10);
    reach_all = getenv("FAULTLAB_REACH_ALL") != NULL;
    jitter_log = getenv("FAULTLAB_JITTER_LOG") != NULL;
    jitter_file = getenv("FAULTLAB_JITTER_FILE");
}

void __sanitizer_cov_trace_pc(void) {
    uint64_t n = ++edges;
    if (jitter_file) {
        if ((n & 0xffff) == 0) jitter_refresh(n);
        if (n == jitter_next) jitter_pause(n);
    }
    if (n == target) {
        fprintf(stderr, "FLEDGE pause at %llu\n", (unsigned long long)n);
        hold_ns(sleep_us * 1000ull);
    }
    // A coarse progress mark, so a run's log shows how far the count runs.
    if ((n & ((1u << 24) - 1)) == 0) fprintf(stderr, "FLEDGE %llu\n", (unsigned long long)n);
}

void faultlab_reach(const char *name) {
    static const char *seen[32];
    static int count;
    if (reach_all) fprintf(stderr, "REACH* %d %s at edge %llu\n", (int)getpid(), name, (unsigned long long)edges);
    if (pause_at && strcmp(name, pause_at) == 0) {
        passages++;
        passage_edge = edges;
        if (passages == pause_k) {
            fprintf(stderr, "FLPAUSE %d %s #%lu at edge %llu\n", (int)getpid(), name, passages,
                    (unsigned long long)edges);
            hold_ns(sleep_us * 1000ull);
            if (reach_all) fprintf(stderr, "FLRESUME %d\n", (int)getpid());
        }
    }
    for (int i = 0; i < count; i++)
        if (seen[i] == name || strcmp(seen[i], name) == 0) return;
    if (count < 32) seen[count++] = name;
    fprintf(stderr, "REACH %s at edge %llu\n", name, (unsigned long long)edges);
}
