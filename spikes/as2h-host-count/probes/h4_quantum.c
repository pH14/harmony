// SPDX-License-Identifier: AGPL-3.0-or-later
// AS-2H / H4a — characterize the quantized kernel contribution ("+74") that
// lands on the fixed instruction counter during guest windows.
//
// Same guest as H2. Per rep captures: fixed-counter instruction delta,
// cycle delta, wall-clock ns, stray exits. Optional inter-rep sleep tests
// whether the quantum correlates with pacing rather than window length.
//
// Usage: h4_quantum <iters> <reps> <sleep_us>

#include <Hypervisor/Hypervisor.h>
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

extern int thread_selfcounts(int type, void *buf, size_t nbytes);

#define GUEST_IPA 0x10000000ull
#define GUEST_SIZE 0x4000ull

static void die(const char *what, long rc) {
    fprintf(stderr, "%s failed: %ld errno=%d\n", what, rc, errno);
    exit(2);
}

static void tsc_read(uint64_t out[2]) {
    if (thread_selfcounts(1, out, 2 * sizeof(uint64_t)) != 0)
        die("thread_selfcounts", -1);
}

int main(int argc, char **argv) {
    uint64_t iters = argc > 1 ? strtoull(argv[1], NULL, 0) : 1000000;
    int reps = argc > 2 ? atoi(argv[2]) : 300;
    useconds_t sleep_us = argc > 3 ? (useconds_t)strtoul(argv[3], NULL, 0) : 0;
    pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

    if (hv_vm_create(NULL) != HV_SUCCESS) die("hv_vm_create", 0);
    void *ram = mmap(NULL, GUEST_SIZE, PROT_READ | PROT_WRITE,
                     MAP_ANON | MAP_PRIVATE, -1, 0);
    if (ram == MAP_FAILED) die("mmap", 0);
    uint32_t *code = (uint32_t *)ram;
    code[0] = 0xF1000400; // subs x0, x0, #1
    code[1] = 0x54FFFFE1; // b.ne  .-4
    code[2] = 0xD4000002; // hvc  #0
    if (hv_vm_map(ram, GUEST_IPA, GUEST_SIZE,
                  HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC) !=
        HV_SUCCESS)
        die("hv_vm_map", 0);
    hv_vcpu_t vcpu;
    hv_vcpu_exit_t *exit_info = NULL;
    if (hv_vcpu_create(&vcpu, &exit_info, NULL) != HV_SUCCESS)
        die("hv_vcpu_create", 0);

    printf("{\"probe\":\"h4_quantum\",\"iters\":%" PRIu64
           ",\"reps\":%d,\"sleep_us\":%u}\n", iters, reps, (unsigned)sleep_us);

    for (int r = 0; r < reps; r++) {
        if (sleep_us) usleep(sleep_us);
        uint64_t pc = iters ? GUEST_IPA : GUEST_IPA + 8;
        if (hv_vcpu_set_reg(vcpu, HV_REG_PC, pc) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_X0, iters) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_CPSR, 0x3c5) != HV_SUCCESS)
            die("hv_vcpu_set_reg", 0);

        uint64_t other_exits = 0, syndrome = 0, c0[2], c1[2];
        uint64_t w0 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);
        tsc_read(c0);
        for (;;) {
            if (hv_vcpu_run(vcpu) != HV_SUCCESS) die("hv_vcpu_run", 0);
            if (exit_info->reason == HV_EXIT_REASON_EXCEPTION) {
                syndrome = exit_info->exception.syndrome;
                if (((syndrome >> 26) & 0x3f) == 0x16) break;
                if (++other_exits > 64) die("exception storm", (long)syndrome);
            } else if (++other_exits > 64) {
                die("exit storm", exit_info->reason);
            }
        }
        tsc_read(c1);
        uint64_t w1 = clock_gettime_nsec_np(CLOCK_UPTIME_RAW);

        printf("{\"rep\":%d,\"d_instrs\":%" PRIu64 ",\"d_cycles\":%" PRIu64
               ",\"d_wall_ns\":%" PRIu64 ",\"other_exits\":%" PRIu64 "}\n",
               r, c1[0] - c0[0], c1[1] - c0[1], w1 - w0, other_exits);
    }
    return 0;
}
