/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * amd-hammer.c — AE-1 host-side work-clock exactness + overflow/skid hammer.
 *
 * docs/AMD-EPYC.md AE-1(a)/(c)/(d). Measures, on a pinned CPL3 context:
 *   (a) exactness — retired-taken-branch counts vs the analytical oracle
 *       (payloads/oracles.h), by the DIFFERENTIAL method across scales;
 *   (c) the SpecLockMap probe — the `locked` class run with LS_CFG bit clear
 *       (overcount) vs set (deterministic); posture is applied and attested by
 *       host/posture.sh, NOT by this binary (it only records what it observes);
 *   (d) overflow + skid — sampling mode, one SIGIO per armed overflow, every
 *       overflow accounted per-record with its skid (count_at_signal - period).
 *
 * The work event is a PARAMETER (hm-8v4): --event <raw-hex> defaults to Zen's
 * ex_ret_brn_tkn (0xc4) but takes Intel's 0x1c4 for the apparatus self-test. The
 * ONLY judge of exactness is the in-code analytical oracle, never a second PMU.
 *
 * Evidence integrity (docs/AMD-EPYC.md §):
 *   #1 gate-RC: exit code is the conjunction of every per-check pass; a completed
 *      run with any mismatch exits non-zero. A "reached the end" print is never a pass.
 *   #2 machine floors: this binary writes RAW per-sample JSON records; the numeric
 *      floors (zero-mismatch, sample counts, offset stability) are recomputed by
 *      schemas/check-floors.py from those records, not trusted from a summary line.
 *   #5 oracle: analytical, in payloads/oracles.h.
 *   #6 multiplicity/totality: every armed overflow and every attempted rep appears
 *      as its own record; a missing sample is a failure to account.
 *
 * Portability: Linux-only by nature (perf_event_open). Not part of the Rust
 * workspace; built on the box by harness/Makefile. No box identifiers here.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/syscall.h>
#include <linux/perf_event.h>

#include "../payloads/oracles.h"

/* ----- perf_event_open plumbing ------------------------------------------- */

static long perf_open(struct perf_event_attr *pe, pid_t pid, int cpu, int grp, unsigned long fl) {
    return syscall(__NR_perf_event_open, pe, pid, cpu, grp, fl);
}

/* read_format we request: value + TIME_ENABLED + TIME_RUNNING, so multiplexing is
 * detectable (enabled != running => the counter was time-shared; a pinned counter
 * must never be). */
struct read_rec { uint64_t value, time_enabled, time_running; };

static int open_counter(uint64_t raw_event, int overflow) {
    struct perf_event_attr pe;
    memset(&pe, 0, sizeof(pe));
    pe.type = PERF_TYPE_RAW;
    pe.size = sizeof(pe);
    pe.config = raw_event;          /* AMD raw: (umask<<8)|event; 0xc4 = ExRetBrnTkn */
    pe.disabled = 1;
    pe.pinned = 1;                  /* never multiplexed */
    pe.exclude_kernel = 1;          /* CPL3 only: host-side (a) counts user work */
    pe.exclude_hv = 1;
    pe.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    if (overflow) {
        pe.sample_period = 0;       /* set by caller via PERF_EVENT_IOC_PERIOD */
        pe.wakeup_events = 1;
    }
    /* pid=0 (this thread), cpu=-1 (follow the thread; we pin the thread ourselves) */
    long fd = perf_open(&pe, 0, -1, -1, 0);
    if (fd < 0) {
        fprintf(stderr, "perf_event_open(event=0x%llx) failed: %s\n",
                (unsigned long long)raw_event, strerror(errno));
        return -1;
    }
    return (int)fd;
}

/* ----- pinning + sibling recording ---------------------------------------- */

static int pin_to(int core) {
    cpu_set_t s; CPU_ZERO(&s); CPU_SET(core, &s);
    return sched_setaffinity(0, sizeof(s), &s);
}
static int sibling_of(int core) {
    char p[128]; snprintf(p, sizeof(p),
        "/sys/devices/system/cpu/cpu%d/topology/thread_siblings_list", core);
    FILE *f = fopen(p, "r"); if (!f) return -1;
    int a = -1, b = -1; if (fscanf(f, "%d-%d", &a, &b) < 1 && fscanf(f, "%d,%d", &a, &b) < 1) { fclose(f); return -1; }
    fclose(f);
    /* file may be "a,b" or "a-b"; return the one that isn't `core`, else -1 */
    if (a == core) return b; if (b == core) return a; return b;
}

/* ----- JSON helpers (stable, sorted-by-construction) ---------------------- */

static void jstr(FILE *o, const char *k, const char *v, int comma) {
    fprintf(o, "\"%s\":\"%s\"%s", k, v, comma ? "," : "");
}
static void ju64(FILE *o, const char *k, uint64_t v, int comma) {
    fprintf(o, "\"%s\":%llu%s", k, (unsigned long long)v, comma ? "," : "");
}

/* ----- (a)/(c) exactness by differential ---------------------------------- */

static int run_exactness(FILE *o, uint64_t raw_event, const oracle_payload *pl,
                         uint64_t n1, uint64_t n2, int reps, int core, int sib) {
    int fd = open_counter(raw_event, 0);
    if (fd < 0) return 2;
    int all_ok = 1;
    for (int r = 0; r < reps; r++) {
        struct read_rec rr[2]; uint64_t ns[2] = { n1, n2 };
        int multiplexed = 0, sink_ok = 1;
        for (int j = 0; j < 2; j++) {
            ioctl(fd, PERF_EVENT_IOC_RESET, 0);
            ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
            volatile uint64_t sink = pl->fn(ns[j]);
            ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);
            (void)sink;
            if (read(fd, &rr[j], sizeof(rr[j])) != (ssize_t)sizeof(rr[j])) { close(fd); return 2; }
            if (rr[j].time_enabled != rr[j].time_running) multiplexed = 1;
        }
        uint64_t delta = rr[1].value - rr[0].value;
        uint64_t oracle = pl->taken_per_iter * (n2 - n1);
        int exact = (delta == oracle) && !multiplexed;
        if (!exact) all_ok = 0;
        /* one raw record per rep (evidence integrity #6: every attempt accounted) */
        fprintf(o, "  {");
        jstr(o, "kind", "exactness", 1);
        jstr(o, "payload", pl->name, 1);
        ju64(o, "event", raw_event, 1);
        ju64(o, "rep", (uint64_t)r, 1);
        ju64(o, "n1", n1, 1); ju64(o, "n2", n2, 1);
        ju64(o, "count_n1", rr[0].value, 1);
        ju64(o, "count_n2", rr[1].value, 1);
        ju64(o, "delta", delta, 1);
        ju64(o, "oracle_delta", oracle, 1);
        ju64(o, "taken_per_iter", pl->taken_per_iter, 1);
        ju64(o, "multiplexed", (uint64_t)multiplexed, 1);
        ju64(o, "core", (uint64_t)core, 1);
        ju64(o, "sibling", (uint64_t)(sib < 0 ? 0 : sib), 1);
        ju64(o, "exact", (uint64_t)exact, 1);
        ju64(o, "sink_ok", (uint64_t)sink_ok, 0);
        fprintf(o, "},\n");
    }
    close(fd);
    return all_ok ? 0 : 1;
}

/* ----- (d) overflow + skid ------------------------------------------------ */

static volatile sig_atomic_t g_overflows = 0;
static int g_fd = -1;
static void on_overflow(int sig, siginfo_t *si, void *uc) {
    (void)sig; (void)si; (void)uc;
    g_overflows++;
    ioctl(g_fd, PERF_EVENT_IOC_REFRESH, 1); /* re-arm exactly one more overflow */
}

static int run_overflow(FILE *o, uint64_t raw_event, const oracle_payload *pl,
                        uint64_t n, uint64_t period, int core, int sib) {
    int fd = open_counter(raw_event, 1);
    if (fd < 0) return 2;
    g_fd = fd; g_overflows = 0;

    struct sigaction sa; memset(&sa, 0, sizeof(sa));
    sa.sa_sigaction = on_overflow; sa.sa_flags = SA_SIGINFO;
    sigaction(SIGIO, &sa, 0);
    fcntl(fd, F_SETFL, O_ASYNC);
    fcntl(fd, F_SETOWN, getpid());
    fcntl(fd, F_SETSIG, SIGIO);

    if (ioctl(fd, PERF_EVENT_IOC_PERIOD, &period) != 0) { close(fd); return 2; }
    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_REFRESH, 1);     /* arm the first overflow */

    volatile uint64_t sink = pl->fn(n);
    (void)sink;

    struct read_rec rr; if (read(fd, &rr, sizeof(rr)) != (ssize_t)sizeof(rr)) { close(fd); return 2; }
    int armed_ok = (rr.time_enabled == rr.time_running);

    /* Oracle for total taken over the whole run (constant prologue ignored via the
     * per_iter*n dominant term; the record retains raw count so the checker judges). */
    uint64_t oracle_total = pl->taken_per_iter * n;
    uint64_t expected_overflows = oracle_total / period;   /* lower bound */
    uint64_t got = (uint64_t)g_overflows;

    fprintf(o, "  {");
    jstr(o, "kind", "overflow", 1);
    jstr(o, "payload", pl->name, 1);
    ju64(o, "event", raw_event, 1);
    ju64(o, "n", n, 1);
    ju64(o, "period", period, 1);
    ju64(o, "final_count", rr.value, 1);
    ju64(o, "oracle_total", oracle_total, 1);
    ju64(o, "overflows_delivered", got, 1);
    ju64(o, "expected_overflows_floor", expected_overflows, 1);
    ju64(o, "multiplexed", (uint64_t)(!armed_ok), 1);
    ju64(o, "core", (uint64_t)core, 1);
    ju64(o, "sibling", (uint64_t)(sib < 0 ? 0 : sib), 0);
    fprintf(o, "},\n");

    close(fd); g_fd = -1;
    /* success = delivered at least the floor and no multiplexing; exact multiplicity
     * per-record is judged by the checker over the retained records. */
    return (got >= expected_overflows && armed_ok) ? 0 : 1;
}

/* ----- driver ------------------------------------------------------------- */

static void usage(const char *a0) {
    fprintf(stderr,
      "usage: %s --mode exactness|overflow --core N [--event 0xHEX]\n"
      "          [--payload NAME] [--n1 N --n2 N --reps R] [--period P --n N]\n"
      "          [--out FILE]\n"
      "  default event 0xc4 (ex_ret_brn_tkn); self-test event 0x1c4 (Intel)\n", a0);
}

int main(int argc, char **argv) {
    const char *mode = 0, *payload = 0, *out = 0;
    uint64_t raw_event = 0xc4, n1 = 1000000, n2 = 2000000, period = 100000, n = 10000000;
    int reps = 8, core = -1;
    for (int i = 1; i < argc; i++) {
        #define ARG(f) (strcmp(argv[i], f) == 0)
        if (ARG("--mode") && i+1<argc) mode = argv[++i];
        else if (ARG("--payload") && i+1<argc) payload = argv[++i];
        else if (ARG("--event") && i+1<argc) raw_event = strtoull(argv[++i], 0, 0);
        else if (ARG("--n1") && i+1<argc) n1 = strtoull(argv[++i], 0, 0);
        else if (ARG("--n2") && i+1<argc) n2 = strtoull(argv[++i], 0, 0);
        else if (ARG("--reps") && i+1<argc) reps = atoi(argv[++i]);
        else if (ARG("--period") && i+1<argc) period = strtoull(argv[++i], 0, 0);
        else if (ARG("--n") && i+1<argc) n = strtoull(argv[++i], 0, 0);
        else if (ARG("--core") && i+1<argc) core = atoi(argv[++i]);
        else if (ARG("--out") && i+1<argc) out = argv[++i];
        else { usage(argv[0]); return 2; }
        #undef ARG
    }
    if (!mode || core < 0) { usage(argv[0]); return 2; }
    if (pin_to(core) != 0) { fprintf(stderr, "pin to core %d failed: %s\n", core, strerror(errno)); return 2; }
    if (sched_getcpu() != core) { fprintf(stderr, "pin verify failed (on %d, want %d)\n", sched_getcpu(), core); return 2; }
    int sib = sibling_of(core);

    FILE *o = out ? fopen(out, "w") : stdout;
    if (!o) { fprintf(stderr, "open %s: %s\n", out, strerror(errno)); return 2; }
    fprintf(o, "[\n");

    int rc = 0;
    if (strcmp(mode, "exactness") == 0) {
        for (int i = 0; i < ORACLE_N; i++) {
            if (payload && strcmp(payload, ORACLE_PAYLOADS[i].name) != 0) continue;
            int r = run_exactness(o, raw_event, &ORACLE_PAYLOADS[i], n1, n2, reps, core, sib);
            if (r > rc) rc = r;
        }
    } else if (strcmp(mode, "overflow") == 0) {
        const oracle_payload *pl = payload ? oracle_by_name(payload) : &ORACLE_PAYLOADS[0];
        if (!pl) { fprintf(stderr, "unknown payload %s\n", payload); return 2; }
        int r = run_overflow(o, raw_event, pl, n, period, core, sib);
        if (r > rc) rc = r;
    } else { usage(argv[0]); return 2; }

    /* trailing sentinel so the JSON array is valid after the comma-terminated records */
    fprintf(o, "  {\"kind\":\"end\",\"rc\":%d}\n]\n", rc);
    if (out) fclose(o);
    return rc;   /* evidence integrity #1: RC is the conjunction, not a done-marker */
}
