/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae0-probe.c — AE-0 Zen-generation capability truth table (docs/AMD-EPYC.md AE-0).
 *
 * Records, from real silicon, the machine-readable truth table AE-0 demands: CPU
 * identity, the SVM VMCB feature surface, the PMU model (legacy per-counter vs
 * PerfMonV2), the single-step facility surface (AE-2's candidate hardware), the
 * intercept-controllable instruction surface, and — the load-bearing row — that the
 * pinned `ex_ret_brn_tkn` encoding is openable as a pinned, non-multiplexed
 * perf_event_open with a trivial overflow actually delivering a sample.
 *
 * Complements host/capture-baseline.sh (which captures the mutable host posture: kernel,
 * kvm_amd identity, LS_CFG/AVIC/governor/SMT). This binary captures the immutable
 * capability surface + the live perf openability test. Stable JSON on stdout.
 *
 * Every "expect" the doc names is emitted as a decoded row so AE-0 acceptance
 * (every expect confirmed or recorded as a deviation with a disposition) is checkable
 * against the record, not asserted in prose. HARDWARE FLAG: platform-scoped rows
 * (topology width, AVIC-at-scale) are Zen-2-core-first-class but EPYC-PROVISIONAL —
 * the caller tags them; this probe reports the raw silicon truth.
 */
#define _GNU_SOURCE
#include <cpuid.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>
#include <linux/perf_event.h>

static void jbool(const char *k, int v) { printf("  \"%s\": %s,\n", k, v ? "true" : "false"); }
static void ju32(const char *k, unsigned v) { printf("  \"%s\": %u,\n", k, v); }
static void jhex(const char *k, unsigned v) { printf("  \"%s\": \"0x%08x\",\n", k, v); }

/* exactly n retired conditional/taken branches (the analytical loop) */
static void __attribute__((noinline)) asm_loop(uint64_t n) {
    __asm__ volatile("1: dec %0\n\tjnz 1b" : "+r"(n)::"cc");
}

static long perf_open(uint64_t config, uint64_t period) {
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.size = sizeof(a);
    a.type = PERF_TYPE_RAW;
    a.config = config;
    a.disabled = 1;
    a.exclude_kernel = 1;
    a.exclude_hv = 1;
    a.pinned = 1;
    a.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    if (period) { a.sample_period = period; a.sample_type = PERF_SAMPLE_IP; a.wakeup_events = 1; }
    return syscall(SYS_perf_event_open, &a, 0, -1, -1, 0);
}

/* Confirm the event opens pinned+non-multiplexed and counts > 0. */
static void probe_event_openable(uint64_t event) {
    long fd = perf_open(event, 0);
    printf("  \"ex_ret_brn_tkn_openable\": {\n");
    printf("    \"event\": \"0x%llx\",\n", (unsigned long long)event);
    if (fd < 0) {
        printf("    \"opened\": false, \"errno\": %d, \"count\": 0, \"non_multiplexed\": false\n  },\n", errno);
        return;
    }
    struct { uint64_t v, en, run; } rr = {0, 0, 0};
    ioctl((int)fd, PERF_EVENT_IOC_RESET, 0);
    ioctl((int)fd, PERF_EVENT_IOC_ENABLE, 0);
    asm_loop(1000000);
    ioctl((int)fd, PERF_EVENT_IOC_DISABLE, 0);
    int rd = read((int)fd, &rr, sizeof(rr)) == (ssize_t)sizeof(rr);
    printf("    \"opened\": true, \"count\": %llu, \"non_multiplexed\": %s, \"read_ok\": %s\n  },\n",
           (unsigned long long)rr.v, (rd && rr.en == rr.run) ? "true" : "false", rd ? "true" : "false");
    close((int)fd);
}

/* Trivial overflow: arm a small period, confirm at least one PERF_RECORD_SAMPLE lands. */
static void probe_overflow_delivers(uint64_t event) {
    long fd = perf_open(event, 100000);
    printf("  \"trivial_overflow_delivers_sample\": ");
    if (fd < 0) { printf("{\"opened\": false, \"errno\": %d},\n", errno); return; }
    long ps = sysconf(_SC_PAGESIZE);
    size_t maplen = (size_t)ps * 9;
    struct perf_event_mmap_page *mp = mmap(0, maplen, PROT_READ | PROT_WRITE, MAP_SHARED, (int)fd, 0);
    if (mp == MAP_FAILED) { printf("{\"opened\": true, \"mmap\": false, \"errno\": %d},\n", errno); close((int)fd); return; }
    ioctl((int)fd, PERF_EVENT_IOC_RESET, 0);
    ioctl((int)fd, PERF_EVENT_IOC_ENABLE, 0);
    asm_loop(2000000);   /* ~expect >= 20 overflows at period 1e5 vs ~2e6 branches */
    ioctl((int)fd, PERF_EVENT_IOC_DISABLE, 0);
    uint64_t head = mp->data_head; __sync_synchronize();
    uint8_t *data = (uint8_t *)mp + (mp->data_offset ? mp->data_offset : (uint64_t)ps);
    size_t dsize = mp->data_size ? (size_t)mp->data_size : (size_t)ps * 8;
    unsigned samples = 0; uint64_t pos = 0;
    while (pos < head && pos + sizeof(struct perf_event_header) <= dsize) {
        struct perf_event_header *h = (void *)(data + pos);
        if (!h->size) break;
        if (h->type == PERF_RECORD_SAMPLE) samples++;
        pos += h->size;
    }
    printf("{\"opened\": true, \"mmap\": true, \"samples\": %u, \"delivered\": %s},\n",
           samples, samples > 0 ? "true" : "false");
    munmap(mp, maplen); close((int)fd);
}

int main(void) {
    cpu_set_t set; CPU_ZERO(&set); CPU_SET(0, &set); sched_setaffinity(0, sizeof(set), &set);
    unsigned a = 0, b = 0, c = 0, d = 0; char vendor[13] = {0};

    printf("{\n");
    printf("  \"schema\": \"amd-epyc-ae0-capability-v1\",\n");

    __get_cpuid(0, &a, &b, &c, &d);
    memcpy(vendor, &b, 4); memcpy(vendor + 4, &d, 4); memcpy(vendor + 8, &c, 4);
    printf("  \"cpuid_vendor\": \"%s\",\n", vendor);
    unsigned maxext; __cpuid(0x80000000, maxext, b, c, d);
    jhex("max_extended_leaf", maxext);

    __get_cpuid(1, &a, &b, &c, &d);
    jhex("cpuid1_eax_signature", a);
    ju32("family_base", (a >> 8) & 0xf);
    ju32("family_ext", (a >> 20) & 0xff);
    ju32("model_base", (a >> 4) & 0xf);
    ju32("model_ext", (a >> 16) & 0xf);
    ju32("stepping", a & 0xf);
    jbool("rdrand", (c >> 30) & 1);
    __get_cpuid_count(7, 0, &a, &b, &c, &d);
    jbool("rdseed", (b >> 18) & 1);

    __cpuid(0x80000001, a, b, c, d);
    jbool("svm_supported", (c >> 2) & 1);
    jbool("rdtscp", (d >> 27) & 1);

    __cpuid(0x80000007, a, b, c, d);
    jbool("invariant_tsc", (d >> 8) & 1);

    /* SVM feature surface (CPUID 0x8000000A EDX) — the VMCB capability rows AE-0 wants */
    __cpuid(0x8000000A, a, b, c, d);
    printf("  \"svm\": {\n");
    printf("    \"revision\": %u, \"nasid\": %u, \"features_edx\": \"0x%08x\",\n", a & 0xff, b, d);
    printf("    \"nested_paging\": %s,\n", (d >> 0) & 1 ? "true" : "false");
    printf("    \"lbr_virt\": %s,\n", (d >> 1) & 1 ? "true" : "false");
    printf("    \"svm_lock\": %s,\n", (d >> 2) & 1 ? "true" : "false");
    printf("    \"nrip_save\": %s,\n", (d >> 3) & 1 ? "true" : "false");
    printf("    \"tsc_rate_msr\": %s,\n", (d >> 4) & 1 ? "true" : "false");
    printf("    \"vmcb_clean\": %s,\n", (d >> 5) & 1 ? "true" : "false");
    printf("    \"flush_by_asid\": %s,\n", (d >> 6) & 1 ? "true" : "false");
    printf("    \"decode_assists\": %s,\n", (d >> 7) & 1 ? "true" : "false");
    printf("    \"pause_filter\": %s,\n", (d >> 10) & 1 ? "true" : "false");
    printf("    \"pause_filter_threshold\": %s,\n", (d >> 12) & 1 ? "true" : "false");
    printf("    \"avic\": %s,\n", (d >> 13) & 1 ? "true" : "false");
    printf("    \"v_vmsave_vmload\": %s,\n", (d >> 15) & 1 ? "true" : "false");
    printf("    \"vgif\": %s\n  },\n", (d >> 16) & 1 ? "true" : "false");

    /* PMU model: PerfMonV2 (Zen 4+) adds global control/status; legacy is per-counter */
    printf("  \"pmu\": {\n");
    if (maxext >= 0x80000022) {
        __cpuid(0x80000022, a, b, c, d);
        printf("    \"leaf_present\": true, \"perfmon_v2\": %s, \"num_core_pmc\": %u, \"num_lbr_stack\": %u\n",
               (a & 1) ? "true" : "false", b & 0xf, (b >> 4) & 0x3f);
    } else {
        printf("    \"leaf_present\": false, \"perfmon_v2\": false, \"model\": \"legacy-per-counter-PERF_CTL/CTR\"\n");
    }
    printf("  },\n");

    /* single-step facility surface (AE-2 candidate hardware). BTF is architectural via
     * DebugCtl bit 1 on AMD (APM Vol 2 ch.13); DR0-DR3 = 4 hw breakpoints; TF is
     * RFLAGS.8 (always present). These are architectural, not CPUID-gated — recorded
     * so AE-2's candidate ranking has the surface pinned. */
    printf("  \"single_step_surface\": {\n");
    printf("    \"rflags_tf\": true, \"debugctl_btf\": true, \"dr_breakpoints\": 4,\n");
    printf("    \"note\": \"BTF/TF/DR architectural on AMD64; #DB-under-SVM behavior is the AE-2 empirical question\"\n  },\n");

    /* live perf openability + overflow-delivery (the load-bearing AE-0 row) */
    probe_event_openable(0xc4);
    probe_overflow_delivers(0xc4);

    printf("  \"probe\": \"done\"\n}\n");
    return 0;
}
