/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae9-l1probe.c - the three things the work clock needs, measured wherever it runs.
 *
 * Run it on bare metal and inside a guest and compare.  It answers, in order:
 *
 *  1. Counting.  Does the retired-conditional-branch event count exactly?  The
 *     oracle is a loop whose branch count is known by analysis, run at two sizes
 *     so the fixed startup cost cancels in the difference.
 *
 *  2. The speculative lock map.  rr's probe: a `lock add` loop must leave the
 *     speculative-lock-map-commit counter at zero, because with the workaround in
 *     effect the lock is not speculatively mapped.  The branch counter runs on the
 *     same loop and must read nonzero, so a zero that means "nothing counted at
 *     all" cannot pass for a zero that means "no speculative lock map".  This is
 *     the only way to establish the workaround from inside a guest, where the
 *     control MSR cannot be read.
 *
 *  3. Overflow delivery.  Arm the counter to overflow every `period` branches over
 *     a known amount of work and count the records that come back.  Landing a
 *     guest on an exact count needs the overflow interrupt, so a hypervisor that
 *     counts correctly but never interrupts is no use.
 */
#define _GNU_SOURCE
#include <asm/unistd.h>
#include <linux/perf_event.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define RAW_BR_COND   0x5100d1ULL   /* ex_ret_cond, the work clock */
#define RAW_LOCK_SPEC 0x510825ULL   /* retired lock instrs, SpecLockMapCommit umask */

static int perf_open(uint64_t config, uint64_t period, int sampling)
{
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.type = PERF_TYPE_RAW;
    a.size = sizeof(a);
    a.config = config;
    a.disabled = 1;
    a.pinned = 1;
    a.exclude_kernel = 1;
    a.exclude_hv = 1;
    if (sampling) {
        a.sample_period = period;
        a.sample_type = PERF_SAMPLE_IP;
        a.wakeup_events = 1;
    }
    return (int)syscall(__NR_perf_event_open, &a, 0, -1, -1, 0);
}

/* Retires exactly `n` conditional branches. */
static void branch_loop(unsigned long n)
{
    asm volatile("1:\n\t dec %0\n\t jnz 1b" : "+r"(n) :: "cc");
}

/* Retires `n` locked adds and `n` conditional branches. */
static void lock_loop(unsigned long n, volatile long *slot)
{
    while (n--)
        asm volatile("lock addq $1, %0" : "+m"(*slot) :: "cc", "memory");
}

static long long read_count(int fd)
{
    long long v = -1;
    if (read(fd, &v, sizeof(v)) != sizeof(v)) return -1;
    return v;
}

static long long counted_branch_loop(unsigned long n)
{
    int fd = perf_open(RAW_BR_COND, 0, 0);
    if (fd < 0) return -1;
    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    branch_loop(n);
    ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);
    long long v = read_count(fd);
    close(fd);
    return v;
}

int main(void)
{
    /* 1. counting exactness, by difference */
    long long c1 = counted_branch_loop(10000000UL);
    long long c2 = counted_branch_loop(20000000UL);
    long long c3 = counted_branch_loop(30000000UL);
    long long d1 = c2 - c1, d2 = c3 - c2;
    int counting_exact = (c1 > 0) && (d1 == 10000000LL) && (d2 == 10000000LL);

    /* 2. the speculative lock map, by rr's probe */
    volatile long slot = 0;
    int lock_fd = perf_open(RAW_LOCK_SPEC, 0, 0);
    int br_fd = perf_open(RAW_BR_COND, 0, 0);
    long long spec = -1, spec_branches = -1;
    int lock_event_opened = (lock_fd >= 0) && (br_fd >= 0);
    if (lock_event_opened) {
        ioctl(lock_fd, PERF_EVENT_IOC_RESET, 0);
        ioctl(br_fd, PERF_EVENT_IOC_RESET, 0);
        ioctl(lock_fd, PERF_EVENT_IOC_ENABLE, 0);
        ioctl(br_fd, PERF_EVENT_IOC_ENABLE, 0);
        lock_loop(2000000UL, &slot);
        ioctl(lock_fd, PERF_EVENT_IOC_DISABLE, 0);
        ioctl(br_fd, PERF_EVENT_IOC_DISABLE, 0);
        spec = read_count(lock_fd);
        spec_branches = read_count(br_fd);
    }
    if (lock_fd >= 0) close(lock_fd);
    if (br_fd >= 0) close(br_fd);
    /* workaround in effect: no speculative lock map commits, and the run was
     * genuinely counted */
    int spec_lock_map_off = lock_event_opened && (spec == 0) && (spec_branches > 0);

    /* 3. overflow delivery */
    const unsigned long work = 4000000UL;
    const uint64_t period = 100000ULL;
    int sfd = perf_open(RAW_BR_COND, period, 1);
    long long samples = 0, lost = 0, throttle = 0;
    int sampling_opened = (sfd >= 0);
    if (sampling_opened) {
        long pg = sysconf(_SC_PAGESIZE);
        size_t len = (size_t)pg * 9;
        struct perf_event_mmap_page *mp =
            mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, sfd, 0);
        if (mp == MAP_FAILED) {
            sampling_opened = 0;
        } else {
            ioctl(sfd, PERF_EVENT_IOC_RESET, 0);
            ioctl(sfd, PERF_EVENT_IOC_ENABLE, 0);
            branch_loop(work);
            ioctl(sfd, PERF_EVENT_IOC_DISABLE, 0);

            uint8_t *base = (uint8_t *)mp + pg;
            uint64_t head = mp->data_head;
            __sync_synchronize();
            uint64_t tail = mp->data_tail;
            uint64_t size = (uint64_t)pg * 8;
            while (tail < head) {
                struct perf_event_header *h =
                    (struct perf_event_header *)(base + (tail % size));
                if (h->size == 0) break;
                if (h->type == PERF_RECORD_SAMPLE) samples++;
                else if (h->type == PERF_RECORD_LOST) lost++;
                else if (h->type == PERF_RECORD_THROTTLE) throttle++;
                tail += h->size;
            }
            munmap(mp, len);
        }
        close(sfd);
    }
    long long expected = (long long)(work / period);
    /* the ring holds a bounded number of records; require that overflows are
     * delivered at all and that none were lost or suppressed */
    int overflow_delivered = sampling_opened && (samples > 0) && (lost == 0) && (throttle == 0);

    int pass = counting_exact && spec_lock_map_off && overflow_delivered;

    printf("{\n"
           "  \"counting\": { \"c1\": %lld, \"c2\": %lld, \"c3\": %lld,"
                          " \"d1\": %lld, \"d2\": %lld, \"expected_delta\": 10000000,"
                          " \"exact\": %d },\n"
           "  \"spec_lock_map\": { \"event_opened\": %d, \"commits\": %lld,"
                               " \"branches_same_run\": %lld, \"workaround_in_effect\": %d },\n"
           "  \"overflow\": { \"opened\": %d, \"samples\": %lld, \"expected_about\": %lld,"
                          " \"lost\": %lld, \"throttle\": %lld, \"delivered\": %d },\n"
           "  \"pass\": %d\n"
           "}\n",
           c1, c2, c3, d1, d2, counting_exact,
           lock_event_opened, spec, spec_branches, spec_lock_map_off,
           sampling_opened, samples, expected, lost, throttle, overflow_delivered,
           pass);
    return pass ? 0 : 1;
}
