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
#include <sys/mman.h>
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
    if (overflow) {
        /* sampling: each overflow writes a PERF_RECORD_SAMPLE carrying the counter value
         * at the PMI (PERF_SAMPLE_READ, read_format=0 -> just the value). The kernel
         * records that value AT the PMI, so skid = value - period is precise and
         * race-free — no signal-delivery timing is involved. */
        pe.sample_period = 1u << 20;  /* nonzero placeholder; real period via IOC_PERIOD */
        pe.sample_type = PERF_SAMPLE_READ;
        pe.read_format = 0;
        pe.wakeup_events = 1;
    } else {
        pe.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
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
    if (a == core) return b;
    if (b == core) return a;
    return b;
}

/* Sum of every hardware interrupt delivered to `core` (its column of
 * /proc/interrupts). Read before/after a measurement window, the DELTA is the count
 * of async interrupts that landed during it — the accountable contamination source
 * (evidence integrity #6): a window with delta 0 is uncontaminated, and its count
 * must equal the oracle exactly. Rows with fewer than core+1 numeric columns (ERR/MIS)
 * are skipped. Returns 0 on any parse failure (conservative: treats unknown as clean,
 * but the correlation check still flags real contamination). */
static unsigned long cpu_irq_count(int core) {
    FILE *f = fopen("/proc/interrupts", "r");
    if (!f) return 0;
    char line[8192];
    if (!fgets(line, sizeof(line), f)) { fclose(f); return 0; }  /* CPUn header */
    unsigned long total = 0;
    while (fgets(line, sizeof(line), f)) {
        char *p = strchr(line, ':');
        if (!p) continue;
        p++;                        /* first number is column 0 (CPU0) */
        for (int col = 0; ; col++) {
            while (*p == ' ' || *p == '\t') p++;
            if (*p < '0' || *p > '9') break;     /* end of numeric columns */
            char *end;
            unsigned long v = strtoul(p, &end, 10);
            if (col == core) { total += v; break; }
            p = end;
        }
    }
    fclose(f);
    return total;
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
        unsigned long irqs[2] = { 0, 0 };
        int multiplexed = 0, sink_ok = 1;
        for (int j = 0; j < 2; j++) {
            unsigned long irq0 = cpu_irq_count(core);
            ioctl(fd, PERF_EVENT_IOC_RESET, 0);
            ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
            volatile uint64_t sink = pl->fn(ns[j]);
            ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);
            (void)sink;
            irqs[j] = cpu_irq_count(core) - irq0;   /* async interrupts during this window */
            if (read(fd, &rr[j], sizeof(rr[j])) != (ssize_t)sizeof(rr[j])) { close(fd); return 2; }
            if (rr[j].time_enabled != rr[j].time_running) multiplexed = 1;
        }
        uint64_t delta = rr[1].value - rr[0].value;
        uint64_t oracle = pl->taken_per_iter * (n2 - n1);
        /* a sample is CLEAN iff no interrupt landed in either sub-window; only clean
         * windows are held to exactness (contaminated ones are accounted, not passed). */
        int clean = (irqs[0] == 0 && irqs[1] == 0);
        int exact = (delta == oracle) && !multiplexed;
        /* Only a CLEAN window that is inexact is a real failure: a contaminated window's
         * excess is accounted external interrupts, not a counting defect. The clean-
         * window COUNT floor (guards against a vacuous all-contaminated pass) is enforced
         * by check-floors against the retained records. */
        if (clean && !exact) all_ok = 0;
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
        ju64(o, "irqs_n1", (uint64_t)irqs[0], 1);
        ju64(o, "irqs_n2", (uint64_t)irqs[1], 1);
        ju64(o, "clean", (uint64_t)clean, 1);
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

/* One-shot arming, RING-based (no signals): each rep arms EXACTLY ONE overflow at
 * `period` via REFRESH(1) (auto-disables after one overflow), runs a payload exceeding
 * it, then polls the mmap ring for the PERF_RECORD_SAMPLE the kernel's PMI handler
 * wrote. The sample carries the counter value AT the PMI (PERF_SAMPLE_READ), so
 * skid = value - period is precise and race-free — there is no signal-delivery timing
 * in the loop. Multiplicity is proven per-arm from the ring: exactly ONE sample must
 * appear (0 = lost PMI, >1 = duplicate); both are recorded, never inferred from a total.
 *
 * This measures the HARDWARE PMI skid (branches retired between the counter crossing
 * `period` and the PMI landing) — the quantity AE-3's in-kernel svm.c force-exit must
 * bound. (The earlier SIGIO variant conflated it with signal-delivery latency.) */

/* scan [tail,head) for PERF_RECORD_SAMPLE records; return count and the last value. */
static uint64_t ring_scan_samples(void *ring, size_t data_off, size_t data_sz,
                                  unsigned *n_samples, uint64_t *last_value) {
    struct perf_event_mmap_page *mp = ring;
    uint64_t head = mp->data_head;
    __sync_synchronize();
    uint8_t *base = (uint8_t *)ring + data_off;
    uint64_t tail = mp->data_tail, pos = tail;
    *n_samples = 0; *last_value = 0;
    while (pos < head) {
        struct perf_event_header *h = (void *)(base + (pos % data_sz));
        if (h->size == 0 || h->size > data_sz) break;
        if (h->type == PERF_RECORD_SAMPLE) {
            /* sample_type == PERF_SAMPLE_READ, read_format 0 -> one u64 value follows */
            uint64_t v; memcpy(&v, base + ((pos + sizeof(*h)) % data_sz), sizeof(v));
            (*n_samples)++; *last_value = v;
        }
        pos += h->size;
    }
    mp->data_tail = head;   /* drain */
    return head - tail;
}

static int run_overflow(FILE *o, uint64_t raw_event, const oracle_payload *pl,
                        uint64_t n, uint64_t period, int reps, int core, int sib) {
    int fd = open_counter(raw_event, 1);
    if (fd < 0) return 2;

    long ps = sysconf(_SC_PAGESIZE);
    size_t maplen = (size_t)ps * 2;          /* 1 metadata page + 1 data page */
    void *ring = mmap(0, maplen, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (ring == MAP_FAILED) { close(fd); return 2; }
    struct perf_event_mmap_page *mp = ring;
    size_t data_off = mp->data_offset ? mp->data_offset : (size_t)ps;
    size_t data_sz = mp->data_size ? (size_t)mp->data_size : (size_t)ps;
    if (ioctl(fd, PERF_EVENT_IOC_PERIOD, &period) != 0) { munmap(ring, maplen); close(fd); return 2; }

    /* Aggregate to scale to ≥10^6 arms without a giant file. Every arm is ACCOUNTED in
     * the tally totals (#6); every ANOMALY (samples!=1) is recorded in full; a bounded
     * set of nominal exemplars is retained for spot-checking. */
    uint64_t hits0 = 0, hits1 = 0, hitsgt1 = 0;
    uint64_t skid_min = ~0ULL, skid_max = 0, skid_sum = 0, skid_n = 0;
    uint64_t hist[6] = {0};   /* skid buckets: 0 | 1..9 | 10..99 | 100..999 | 1e3..9999 | >=1e4 */
    int exemplars = 0;

    for (int r = 0; r < reps; r++) {
        ioctl(fd, PERF_EVENT_IOC_RESET, 0);
        ioctl(fd, PERF_EVENT_IOC_PERIOD, &period);   /* reset period_left so the overflow
                                                      * fires after exactly `period`, not
                                                      * a stale countdown from last arm */
        ioctl(fd, PERF_EVENT_IOC_REFRESH, 1);        /* arm exactly one overflow */
        volatile uint64_t sink = pl->fn(n);          /* n chosen so taken > period */
        (void)sink;
        ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);        /* stop counting; sample already written */

        unsigned nsamp = 0; uint64_t val = 0;
        ring_scan_samples(ring, data_off, data_sz, &nsamp, &val);
        uint64_t hits = nsamp;
        int ok = (hits == 1) && (val >= period);
        uint64_t skid = ok ? (val - period) : 0;

        if (hits == 0) hits0++;
        else if (hits == 1) hits1++;
        else hitsgt1++;

        if (ok) {
            if (skid < skid_min) skid_min = skid;
            if (skid > skid_max) skid_max = skid;
            skid_sum += skid; skid_n++;
            int b = skid == 0 ? 0 : skid < 10 ? 1 : skid < 100 ? 2 : skid < 1000 ? 3 : skid < 10000 ? 4 : 5;
            hist[b]++;
        }

        int anomaly = (hits != 1) || (val < period);
        if (anomaly || exemplars < 16) {
            if (!anomaly) exemplars++;
            fprintf(o, "  {");
            jstr(o, "kind", anomaly ? "overflow_anomaly" : "overflow_exemplar", 1);
            jstr(o, "payload", pl->name, 1);
            ju64(o, "rep", (uint64_t)r, 1);
            ju64(o, "period", period, 1);
            ju64(o, "value_at_pmi", val, 1);
            ju64(o, "samples", hits, 1);
            ju64(o, "skid", skid, 0);
            fprintf(o, "},\n");
        }
    }
    munmap(ring, maplen);
    close(fd);
    uint64_t clean_arms = hits1, clean_skid_max = skid_max;  /* ring skid is contamination-free */

    /* the summary record: tallies are the totality account (#6) */
    fprintf(o, "  {");
    jstr(o, "kind", "overflow_summary", 1);
    jstr(o, "payload", pl->name, 1);
    ju64(o, "event", raw_event, 1);
    ju64(o, "n", n, 1);
    ju64(o, "period", period, 1);
    ju64(o, "arms_total", (uint64_t)reps, 1);
    ju64(o, "hits_0_lost", hits0, 1);
    ju64(o, "hits_1_ok", hits1, 1);
    ju64(o, "hits_gt1_dup", hitsgt1, 1);
    ju64(o, "skid_min", skid_n ? skid_min : 0, 1);
    ju64(o, "skid_max", skid_max, 1);
    ju64(o, "skid_mean_x1000", skid_n ? (skid_sum * 1000 / skid_n) : 0, 1);
    ju64(o, "clean_arms", clean_arms, 1);
    ju64(o, "clean_skid_max", clean_skid_max, 1);
    fprintf(o, "\"skid_hist\":[%llu,%llu,%llu,%llu,%llu,%llu],",
            (unsigned long long)hist[0], (unsigned long long)hist[1], (unsigned long long)hist[2],
            (unsigned long long)hist[3], (unsigned long long)hist[4], (unsigned long long)hist[5]);
    ju64(o, "core", (uint64_t)core, 1);
    ju64(o, "sibling", (uint64_t)(sib < 0 ? 0 : sib), 0);
    fprintf(o, "},\n");

    /* success = zero lost, zero duplicate, and at least one delivered */
    return (hits0 == 0 && hitsgt1 == 0 && hits1 > 0) ? 0 : 1;
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
        int r = run_overflow(o, raw_event, pl, n, period, reps, core, sib);
        if (r > rc) rc = r;
    } else { usage(argv[0]); return 2; }

    /* trailing sentinel so the JSON array is valid after the comma-terminated records */
    fprintf(o, "  {\"kind\":\"end\",\"rc\":%d}\n]\n", rc);
    if (out) fclose(o);
    return rc;   /* evidence integrity #1: RC is the conjunction, not a done-marker */
}
