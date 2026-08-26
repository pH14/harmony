/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae10-guestwindow.c - the measurement consonance actually makes, run wherever.
 *
 * consonance does not have its guest read a counter.  It opens a counter that
 * counts only guest execution, runs the guest, and reads the counter itself.
 * That is the arrangement this measures: `exclude_host`, attached to the thread
 * that enters the guest.
 *
 * The payload is a loop whose retired conditional branches are known by analysis.
 * Absolute counts carry a fixed cost for the guest's entry and exit, so the test
 * is on differences across loop sizes, repeated so that a drifting offset shows up
 * as disagreement between repetitions rather than hiding in one number.
 *
 * Run on bare metal and inside a guest and compare.
 */
#define _GNU_SOURCE
#include <asm/unistd.h>
#include <errno.h>
#include <fcntl.h>
#include <linux/kvm.h>
#include <linux/perf_event.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#define RAW_BR_COND 0x5100d1ULL
#define GUEST_PHYS  0x1000
#define MEM_SIZE    0x10000
#define REPS        20

static int perf_open_guest_only(void)
{
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.type = PERF_TYPE_RAW;
    a.size = sizeof(a);
    a.config = RAW_BR_COND;
    a.disabled = 1;
    a.pinned = 1;
    a.exclude_host = 1;   /* count the guest, not us */
    return (int)syscall(__NR_perf_event_open, &a, 0, -1, -1, 0);
}

/* `mov si,n; dec si; jnz` retires exactly n conditional branches, then halts. */
static void emit(uint8_t *mem, uint16_t iters)
{
    uint8_t c[] = {
        0xBE, (uint8_t)(iters & 0xff), (uint8_t)(iters >> 8),  /* mov si,n */
        0x4E,                                                   /* dec si   */
        0x75, 0xFD,                                             /* jnz -3   */
        0xF4,                                                   /* hlt      */
    };
    memcpy(mem + GUEST_PHYS, c, sizeof(c));
}

/* Run one guest window; returns the guest-only branch count, or -1. */
static long long window(int kvm, int perf_fd, uint16_t iters, int *hlt)
{
    *hlt = 0;
    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    if (vmfd < 0) return -1;
    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE,
                        MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0,
        .memory_size = MEM_SIZE, .userspace_addr = (uint64_t)mem };
    if (ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) return -1;
    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    if (vcpu < 0) return -1;
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, vcpu, 0);

    emit(mem, iters);
    struct kvm_sregs s;
    ioctl(vcpu, KVM_GET_SREGS, &s);
    s.cs.base = 0; s.cs.selector = 0;
    ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r;
    memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS; r.rflags = 0x2;
    ioctl(vcpu, KVM_SET_REGS, &r);

    ioctl(perf_fd, PERF_EVENT_IOC_RESET, 0);
    ioctl(perf_fd, PERF_EVENT_IOC_ENABLE, 0);
    for (int guard = 0; guard < 4096; guard++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (run->exit_reason == KVM_EXIT_HLT) { *hlt = 1; break; }
        break;
    }
    ioctl(perf_fd, PERF_EVENT_IOC_DISABLE, 0);
    long long v = -1;
    if (read(perf_fd, &v, sizeof(v)) != sizeof(v)) v = -1;

    munmap(run, msize); munmap(mem, MEM_SIZE);
    close(vcpu); close(vmfd);
    return v;
}

int main(void)
{
    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }
    int pf = perf_open_guest_only();
    if (pf < 0) {
        printf("{ \"guest_only_counter_opened\": 0, \"pass\": 0 }\n");
        return 1;
    }

    const uint16_t sizes[3] = { 10000, 20000, 30000 };
    long long got[3][REPS];
    int all_hlt = 1;
    for (int s = 0; s < 3; s++)
        for (int r = 0; r < REPS; r++) {
            int hlt = 0;
            got[s][r] = window(kvm, pf, sizes[s], &hlt);
            if (!hlt) all_hlt = 0;
        }

    /* Every repetition of one size must agree exactly, and each step of 10000
     * loop iterations must move the count by exactly 10000. */
    int stable = 1;
    for (int s = 0; s < 3; s++)
        for (int r = 1; r < REPS; r++)
            if (got[s][r] != got[s][0]) stable = 0;

    long long d1 = got[1][0] - got[0][0];
    long long d2 = got[2][0] - got[1][0];
    int deltas_exact = (d1 == 10000) && (d2 == 10000);
    long long offset = got[0][0] - 10000;
    int pass = stable && deltas_exact && all_hlt && (got[0][0] > 0);

    printf("{\n  \"guest_only_counter_opened\": 1,\n");
    for (int s = 0; s < 3; s++) {
        printf("  \"iters_%u\": [", sizes[s]);
        for (int r = 0; r < REPS; r++)
            printf("%s%lld", r ? ", " : "", got[s][r]);
        printf("],\n");
    }
    printf("  \"repetitions_agree\": %d,\n"
           "  \"deltas\": [ %lld, %lld ],\n"
           "  \"expected_delta\": 10000,\n"
           "  \"deltas_exact\": %d,\n"
           "  \"fixed_offset\": %lld,\n"
           "  \"all_halted\": %d,\n"
           "  \"pass\": %d\n}\n",
           stable, d1, d2, deltas_exact, offset, all_hlt, pass);
    close(pf); close(kvm);
    return pass ? 0 : 1;
}
