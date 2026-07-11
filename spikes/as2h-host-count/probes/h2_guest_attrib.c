// SPDX-License-Identifier: AGPL-3.0-or-later
// AS-2H / H2 — guest-work attribution probe.
//
// Question: do instructions executed by a Hypervisor.framework guest count
// toward the host vCPU thread's always-on fixed instruction counter
// (thread_selfcounts), and is the count EXACT?
//
// Method: plain EL1 guest (no vEL2) running a calibrated loop
//     loop: subs x0, x0, #1 ; b.ne loop ; hvc #0
// = exactly 2N+1 retired guest instructions for x0=N, then an HVC doorbell
// back to the host. thread_selfcounts is read immediately before and after
// the hv_vcpu_run loop; no syscalls sit inside the window.
//
// Controls:
//   - N=0 (PC set directly at the HVC): pure enter/exit overhead.
//   - multiple N: guest-work slope must be exactly 2 instructions/iteration.
//   - per-rep JSONL so floor/mode/tail structure is analyzable offline.
//
// Interpretation:
//   delta(N) - delta(0) == 2N exactly on the floor  => guest work IS counted
//   delta(N) ~= delta(0) regardless of N            => guest work NOT counted
//   floor unstable                                  => counted but inexact

#include <Hypervisor/Hypervisor.h>
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

extern int thread_selfcounts(int type, void *buf, size_t nbytes);

#define GUEST_IPA 0x10000000ull
#define GUEST_SIZE 0x4000ull // one 16K page

static uint64_t tsc_instrs(void) {
    uint64_t buf[2] = {0, 0};
    if (thread_selfcounts(1, buf, sizeof buf) != 0) {
        fprintf(stderr, "thread_selfcounts failed: %s\n", strerror(errno));
        exit(2);
    }
    return buf[0]; // field 0 validated as instructions by the H1 probe
}

static void die(const char *what, hv_return_t rc) {
    fprintf(stderr, "%s failed: 0x%x\n", what, (unsigned)rc);
    exit(2);
}

int main(int argc, char **argv) {
    uint64_t iters = argc > 1 ? strtoull(argv[1], NULL, 0) : 1000000;
    int reps = argc > 2 ? atoi(argv[2]) : 200;
    pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

    hv_return_t rc = hv_vm_create(NULL);
    if (rc != HV_SUCCESS) die("hv_vm_create", rc);

    void *ram = mmap(NULL, GUEST_SIZE, PROT_READ | PROT_WRITE,
                     MAP_ANON | MAP_PRIVATE, -1, 0);
    if (ram == MAP_FAILED) die("mmap", 0);
    uint32_t *code = (uint32_t *)ram;
    code[0] = 0xF1000400; // subs x0, x0, #1
    code[1] = 0x54FFFFE1; // b.ne  .-4
    code[2] = 0xD4000002; // hvc  #0
    rc = hv_vm_map(ram, GUEST_IPA, GUEST_SIZE,
                   HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC);
    if (rc != HV_SUCCESS) die("hv_vm_map", rc);

    hv_vcpu_t vcpu;
    hv_vcpu_exit_t *exit_info = NULL;
    rc = hv_vcpu_create(&vcpu, &exit_info, NULL);
    if (rc != HV_SUCCESS) die("hv_vcpu_create", rc);

    printf("{\"probe\":\"h2_guest_attrib\",\"iters\":%" PRIu64
           ",\"reps\":%d}\n", iters, reps);

    for (int r = 0; r < reps; r++) {
        // entry PC: loop head for N>0, straight at BRK for the N=0 control
        uint64_t pc = iters ? GUEST_IPA : GUEST_IPA + 8;
        if (hv_vcpu_set_reg(vcpu, HV_REG_PC, pc) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_X0, iters) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_CPSR, 0x3c5) != HV_SUCCESS)
            die("hv_vcpu_set_reg", 0);

        uint64_t other_exits = 0, syndrome = 0;
        uint64_t t0 = tsc_instrs();
        for (;;) {
            rc = hv_vcpu_run(vcpu);
            if (rc != HV_SUCCESS) die("hv_vcpu_run", rc);
            if (exit_info->reason == HV_EXIT_REASON_EXCEPTION) {
                syndrome = exit_info->exception.syndrome;
                uint64_t ec = (syndrome >> 26) & 0x3f;
                if (ec == 0x16) break; // HVC64 doorbell — payload done
                other_exits++;
                if (other_exits > 64) {
                    fprintf(stderr, "storm: last ec=0x%llx syndrome=0x%llx\n",
                            (unsigned long long)ec, (unsigned long long)syndrome);
                    die("unexpected exception storm", 0);
                }
            } else {
                other_exits++; // VTIMER etc.: re-enter
                if (other_exits > 64) die("unexpected exit storm", 0);
            }
        }
        uint64_t t1 = tsc_instrs();

        uint64_t x0 = ~0ull;
        hv_vcpu_get_reg(vcpu, HV_REG_X0, &x0);
        printf("{\"rep\":%d,\"d_instrs\":%" PRIu64 ",\"other_exits\":%" PRIu64
               ",\"x0_final\":%" PRIu64 ",\"ec\":%" PRIu64 "}\n",
               r, t1 - t0, other_exits, x0, (syndrome >> 26) & 0x3f);
    }

    hv_vcpu_destroy(vcpu);
    hv_vm_unmap(GUEST_IPA, GUEST_SIZE);
    hv_vm_destroy();
    return 0;
}
