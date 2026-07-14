// SPDX-License-Identifier: AGPL-3.0-or-later
/* nested-x86 N-0 capability probe.
 *
 * Runs as PID-adjacent process inside a minimal L1 guest (initramfs). Reports,
 * as line-oriented JSON between sentinel markers:
 *   - CPUID identity + hypervisor leaf (what L1 knows about being virtualized)
 *   - CPUID leaf 0xA (arch perfmon version / counters / widths / event mask)
 *   - raw IA32_VMX_* capability MSRs as virtualized by L0 KVM for L1,
 *     with decodes of the six controls the determinism stack requires
 *   - IA32_PERF_CAPABILITIES (full-width writes)
 *   - a count-exactness sniff: raw event 0x1c4 (BR_INST_RETIRED.CONDITIONAL,
 *     user-only, pinned) against an analytically exact `dec/jnz` loop.
 *     The loop retires exactly N conditional branches; fixed harness overhead
 *     is eliminated differentially (count(2N) - count(N) == N exactly iff the
 *     vPMU is exact for this event at one virtualization layer).
 *   - a PMI overflow-delivery sniff: the same event opened in sampling mode
 *     (sample_period=P) with an mmap ring + O_ASYNC/SIGIO. Every
 *     PERF_RECORD_SAMPLE is written by the L1 kernel's PMI handler, so
 *     ring_samples == floor(total_count / P) with zero throttle records
 *     iff every overflow PMI was delivered inside L1 exactly once.
 *
 * Static build: gcc -O2 -static -o probe probe.c
 */
#define _GNU_SOURCE
#include <cpuid.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/perf_event.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

static int msr_fd = -1;

static int rdmsr(uint32_t msr, uint64_t *val) {
    if (msr_fd < 0) return -ENODEV;
    ssize_t r = pread(msr_fd, val, 8, msr);
    return r == 8 ? 0 : -errno;
}

static void emit_msr(const char *name, uint32_t msr) {
    uint64_t v = 0;
    int rc = rdmsr(msr, &v);
    if (rc == 0)
        printf("  \"%s\": \"0x%016llx\",\n", name, (unsigned long long)v);
    else
        printf("  \"%s\": \"UNREADABLE(errno=%d)\",\n", name, -rc);
}

/* allowed-1 settings of a VMX control MSR live in bits 63:32 */
static int allowed1(uint32_t msr, int bit) {
    uint64_t v = 0;
    if (rdmsr(msr, &v)) return -1;
    return (int)((v >> (32 + bit)) & 1);
}

static void emit_bit(const char *name, int v) {
    if (v < 0) printf("  \"%s\": \"UNKNOWN\",\n", name);
    else       printf("  \"%s\": %s,\n", name, v ? "true" : "false");
}

/* exactly n retired conditional branches (n-1 taken + 1 not-taken) */
static void __attribute__((noinline)) asm_loop(uint64_t n) {
    __asm__ volatile("1: dec %0\n\tjnz 1b" : "+r"(n)::"cc");
}

static long perf_open(uint64_t type, uint64_t config) {
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.size = sizeof(a);
    a.type = (uint32_t)type;
    a.config = config;
    a.disabled = 1;
    a.exclude_kernel = 1;
    a.exclude_hv = 1;
    a.pinned = 1;
    return syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
}

static void count_sniff(const char *label, uint64_t type, uint64_t config) {
    long fd = perf_open(type, config);
    if (fd < 0) {
        printf("  \"%s\": \"perf_event_open failed errno=%d\",\n", label, errno);
        return;
    }
    static const uint64_t ns[] = {1000000ULL, 10000000ULL, 100000000ULL};
    printf("  \"%s\": {\n", label);
    for (unsigned i = 0; i < 3; i++) {
        printf("    \"n_%llu\": [", (unsigned long long)ns[i]);
        for (int rep = 0; rep < 5; rep++) {
            uint64_t count = 0;
            ioctl((int)fd, PERF_EVENT_IOC_RESET, 0);
            ioctl((int)fd, PERF_EVENT_IOC_ENABLE, 0);
            asm_loop(ns[i]);
            ioctl((int)fd, PERF_EVENT_IOC_DISABLE, 0);
            if (read((int)fd, &count, 8) != 8) count = (uint64_t)-1;
            printf("%s%llu", rep ? ", " : "", (unsigned long long)count);
        }
        printf("]%s\n", i < 2 ? "," : "");
    }
    printf("  },\n");
    close((int)fd);
}

/* ---- PMI overflow-delivery sniff ---- */

#define RING_PAGES 8

static volatile sig_atomic_t sigio_hits;
static void on_sigio(int sig) { (void)sig; sigio_hits++; }

static long perf_open_sampling(uint64_t config, uint64_t period) {
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.size = sizeof(a);
    a.type = PERF_TYPE_RAW;
    a.config = config;
    a.disabled = 1;
    a.exclude_kernel = 1;
    a.exclude_hv = 1;
    a.pinned = 1;
    a.sample_period = period;
    a.sample_type = PERF_SAMPLE_IP;
    a.wakeup_events = 1;
    return syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
}

struct ring {
    struct perf_event_mmap_page *mp;
    uint8_t *data;
    size_t data_size;
    size_t map_len;
};

static int ring_map(int fd, struct ring *r) {
    long ps = sysconf(_SC_PAGESIZE);
    r->map_len = (size_t)ps * (1 + RING_PAGES);
    void *m = mmap(NULL, r->map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    if (m == MAP_FAILED) return -1;
    r->mp = m;
    r->data = (uint8_t *)m + (r->mp->data_offset ? r->mp->data_offset : (uint64_t)ps);
    r->data_size = r->mp->data_size ? (size_t)r->mp->data_size : (size_t)ps * RING_PAGES;
    return 0;
}

/* few records, ring never wraps; walk headers from offset 0 to data_head */
static void ring_scan(const struct ring *r, unsigned *samples, unsigned *throttles,
                      unsigned *others) {
    uint64_t head = r->mp->data_head;
    __sync_synchronize();
    *samples = *throttles = *others = 0;
    uint64_t pos = 0;
    while (pos < head && pos + sizeof(struct perf_event_header) <= r->data_size) {
        const struct perf_event_header *h = (const void *)(r->data + pos);
        if (h->size == 0) break;
        if (h->type == PERF_RECORD_SAMPLE) (*samples)++;
        else if (h->type == PERF_RECORD_THROTTLE || h->type == PERF_RECORD_UNTHROTTLE)
            (*throttles)++;
        else (*others)++;
        pos += h->size;
    }
}

static void pmi_sniff(void) {
    static const struct { uint64_t n, p; } combos[] = {
        {1000000ULL,  100000ULL},   /* expect 10 PMIs */
        {10000000ULL, 1000000ULL},  /* expect 10 PMIs, 10x the spacing */
        {1000000ULL,  250000ULL},   /* expect 4 PMIs */
    };
    struct sigaction sa;
    memset(&sa, 0, sizeof(sa));
    sa.sa_handler = on_sigio;
    sigaction(SIGIO, &sa, NULL);

    printf("  \"pmi_sniff_raw_0x1c4\": {\n");
    for (unsigned ci = 0; ci < 3; ci++) {
        uint64_t n = combos[ci].n, p = combos[ci].p;
        printf("    \"n_%llu_p_%llu\": {\"expect\": %llu, \"reps\": [\n",
               (unsigned long long)n, (unsigned long long)p,
               (unsigned long long)((n + 2) / p));
        for (int rep = 0; rep < 5; rep++) {
            long fd = perf_open_sampling(0x1c4, p);
            if (fd < 0) {
                printf("      {\"error\": \"perf_event_open errno=%d\"}%s\n",
                       errno, rep < 4 ? "," : "");
                continue;
            }
            struct ring r;
            if (ring_map((int)fd, &r)) {
                printf("      {\"error\": \"mmap errno=%d\"}%s\n",
                       errno, rep < 4 ? "," : "");
                close((int)fd);
                continue;
            }
            fcntl((int)fd, F_SETOWN, getpid());
            fcntl((int)fd, F_SETSIG, SIGIO);
            fcntl((int)fd, F_SETFL, fcntl((int)fd, F_GETFL) | O_ASYNC);
            sigio_hits = 0;
            uint64_t count = 0;
            ioctl((int)fd, PERF_EVENT_IOC_RESET, 0);
            ioctl((int)fd, PERF_EVENT_IOC_ENABLE, 0);
            asm_loop(n);
            ioctl((int)fd, PERF_EVENT_IOC_DISABLE, 0);
            if (read((int)fd, &count, 8) != 8) count = (uint64_t)-1;
            unsigned samples, throttles, others;
            ring_scan(&r, &samples, &throttles, &others);
            printf("      {\"ring_samples\": %u, \"signals\": %d, \"throttles\": %u, "
                   "\"other_records\": %u, \"count\": %llu}%s\n",
                   samples, (int)sigio_hits, throttles, others,
                   (unsigned long long)count, rep < 4 ? "," : "");
            munmap(r.mp, r.map_len);
            close((int)fd);
        }
        printf("    ]}%s\n", ci < 2 ? "," : "");
    }
    printf("  },\n");
}

int main(void) {
    cpu_set_t set;
    CPU_ZERO(&set);
    CPU_SET(0, &set);
    sched_setaffinity(0, sizeof(set), &set);

    msr_fd = open("/dev/cpu/0/msr", O_RDONLY);

    unsigned a, b, c, d;
    char vendor[13] = {0}, hv[13] = {0};

    printf("{\n");

    __get_cpuid(0, &a, &b, &c, &d);
    memcpy(vendor, &b, 4); memcpy(vendor + 4, &d, 4); memcpy(vendor + 8, &c, 4);
    printf("  \"cpuid_vendor\": \"%s\",\n", vendor);

    __get_cpuid(1, &a, &b, &c, &d);
    printf("  \"cpuid1_eax\": \"0x%08x\",\n", a);
    printf("  \"vmx_bit\": %s,\n", (c >> 5) & 1 ? "true" : "false");
    printf("  \"hypervisor_bit\": %s,\n", (c >> 31) & 1 ? "true" : "false");

    /* raw cpuid: __get_cpuid clamps to the basic-leaf range */
    __cpuid(0x40000000, a, b, c, d);
    memcpy(hv, &b, 4); memcpy(hv + 4, &c, 4); memcpy(hv + 8, &d, 4);
    printf("  \"hypervisor_leaf\": \"%s\",\n", hv);

    __get_cpuid_count(0xA, 0, &a, &b, &c, &d);
    printf("  \"perfmon_version\": %u,\n", a & 0xff);
    printf("  \"gp_counters\": %u,\n", (a >> 8) & 0xff);
    printf("  \"gp_counter_width\": %u,\n", (a >> 16) & 0xff);
    printf("  \"event_unavail_mask\": \"0x%02x\",\n", b & 0x7f);
    printf("  \"branch_insn_retired_available\": %s,\n", (b >> 5) & 1 ? "false" : "true");
    printf("  \"fixed_counters\": %u,\n", d & 0x1f);
    printf("  \"fixed_counter_width\": %u,\n", (d >> 5) & 0xff);

    /* raw VMX capability MSRs (as L0 virtualizes them for L1) */
    emit_msr("IA32_VMX_BASIC_0x480", 0x480);
    emit_msr("IA32_VMX_PINBASED_0x481", 0x481);
    emit_msr("IA32_VMX_PROCBASED_0x482", 0x482);
    emit_msr("IA32_VMX_EXIT_0x483", 0x483);
    emit_msr("IA32_VMX_ENTRY_0x484", 0x484);
    emit_msr("IA32_VMX_MISC_0x485", 0x485);
    emit_msr("IA32_VMX_PROCBASED2_0x48B", 0x48B);
    emit_msr("IA32_VMX_EPT_VPID_CAP_0x48C", 0x48C);
    emit_msr("IA32_VMX_TRUE_PINBASED_0x48D", 0x48D);
    emit_msr("IA32_VMX_TRUE_PROCBASED_0x48E", 0x48E);
    emit_msr("IA32_VMX_TRUE_EXIT_0x48F", 0x48F);
    emit_msr("IA32_VMX_TRUE_ENTRY_0x490", 0x490);
    emit_msr("IA32_PERF_CAPABILITIES_0x345", 0x345);

    /* the six controls the determinism stack requires, decoded from TRUE ctls
     * where defined (fall back to non-TRUE for proc2) */
    emit_bit("ctl_rdtsc_exiting", allowed1(0x48E, 12));
    emit_bit("ctl_mtf", allowed1(0x48E, 27));
    emit_bit("ctl_secondary_controls", allowed1(0x48E, 31));
    emit_bit("ctl2_ept", allowed1(0x48B, 1));
    emit_bit("ctl2_unrestricted_guest", allowed1(0x48B, 7));
    emit_bit("ctl2_rdrand_exiting", allowed1(0x48B, 11));
    emit_bit("ctl2_rdseed_exiting", allowed1(0x48B, 16));
    emit_bit("ctl2_pml", allowed1(0x48B, 17));
    emit_bit("exit_load_perf_global_ctrl", allowed1(0x48F, 12));
    emit_bit("entry_load_perf_global_ctrl", allowed1(0x490, 13));

    /* count-exactness sniff at one virtualization layer */
    count_sniff("sniff_raw_0x1c4_br_cond", PERF_TYPE_RAW, 0x1c4);
    count_sniff("sniff_hw_instructions", PERF_TYPE_HARDWARE, PERF_COUNT_HW_INSTRUCTIONS);

    /* PMI overflow-delivery sniff (N-0 method: overflow fires an interrupt in L1) */
    pmi_sniff();

    printf("  \"probe\": \"done\"\n}\n");
    return 0;
}
