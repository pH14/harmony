// SPDX-License-Identifier: AGPL-3.0-or-later
// AS-2H / H3 — EL-filtered retired-branch counting of a guest via kpc.
//
// Question: can a real configurable PMU counter, programmed host-side via
// kpc (kperf.framework private API, root), count the guest's retired
// branches EXACTLY while excluding host-kernel work via per-EL enables?
//
// Same guest as H2: `loop: subs x0,x0,#1 ; b.ne loop ; hvc #0` at EL1.
// Retired-branch truth for x0=N is exactly N (the b.ne, taken N-1 times,
// not-taken once; subs and hvc are not branches).
//
// EL-enable bits in the kpc config word (widely used public values; their
// hardware meaning on VHE/EL2 hosts is exactly what this probe measures):
//   0x10000 EL0 A32   0x20000 EL0 A64   0x40000 "EL1" A64   0x80000 EL3 A64
//
// Interpretation of the matrix:
//   elmask=EL1 only, delta==N exactly       => guest EL1 counted, host EL2
//                                              kernel excluded — retired-
//                                              branch contract SURVIVES
//   elmask=EL1 only, delta==0               => configurable counters blind
//                                              to guest — H3 NO-GO
//   delta==N + kernel-shaped noise          => "EL1" bit aliases host kernel
//                                              (VHE) — filtering insufficient
//
// Usage: h3_kpc_branch <event> <elmask-hex> <iters> <reps>   (run as root)

#include <Hypervisor/Hypervisor.h>
#include <dlfcn.h>
#include <errno.h>
#include <inttypes.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>

extern int thread_selfcounts(int type, void *buf, size_t nbytes);

#define KPC_CLASS_CONFIGURABLE_MASK 0x2u

static int (*kpc_force_all_ctrs_set)(int);
static int (*kpc_force_all_ctrs_get)(int *);
static int (*kpc_set_counting)(uint32_t);
static int (*kpc_set_thread_counting)(uint32_t);
static uint32_t (*kpc_get_counter_count)(uint32_t);
static uint32_t (*kpc_get_config_count)(uint32_t);
static int (*kpc_set_config)(uint32_t, uint64_t *);
static int (*kpc_get_thread_counters)(uint32_t, uint32_t, uint64_t *);

static void cleanup(void) {
    if (kpc_set_thread_counting) kpc_set_thread_counting(0);
    if (kpc_set_counting) kpc_set_counting(0);
    if (kpc_force_all_ctrs_set) kpc_force_all_ctrs_set(0);
}

static void die(const char *what, long rc) {
    fprintf(stderr, "%s failed: rc=%ld errno=%d (%s)\n", what, rc, errno,
            strerror(errno));
    cleanup();
    exit(2);
}

#define GUEST_IPA 0x10000000ull
#define GUEST_SIZE 0x4000ull

static uint64_t tsc_instrs(void) {
    uint64_t buf[2] = {0, 0};
    if (thread_selfcounts(1, buf, sizeof buf) != 0) die("thread_selfcounts", -1);
    return buf[0];
}

int main(int argc, char **argv) {
    if (argc < 5) {
        fprintf(stderr, "usage: %s <event> <elmask-hex> <iters> <reps>\n", argv[0]);
        return 1;
    }
    uint64_t event = strtoull(argv[1], NULL, 0);
    uint64_t elmask = strtoull(argv[2], NULL, 16);
    uint64_t iters = strtoull(argv[3], NULL, 0);
    int reps = atoi(argv[4]);
    pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);

    void *kperf = dlopen(
        "/System/Library/PrivateFrameworks/kperf.framework/kperf", RTLD_LAZY);
    if (!kperf) die("dlopen kperf", 0);
#define SYM(n)                                                                 \
    do {                                                                       \
        *(void **)&n = dlsym(kperf, #n);                                       \
        if (!n) { fprintf(stderr, "missing symbol: %s\n", #n); exit(3); }      \
    } while (0)
    SYM(kpc_force_all_ctrs_set);
    SYM(kpc_force_all_ctrs_get);
    SYM(kpc_set_counting);
    SYM(kpc_set_thread_counting);
    SYM(kpc_get_counter_count);
    SYM(kpc_get_config_count);
    SYM(kpc_set_config);
    SYM(kpc_get_thread_counters);
#undef SYM

    uint32_t ncfg = kpc_get_config_count(KPC_CLASS_CONFIGURABLE_MASK);
    uint32_t nctr = kpc_get_counter_count(KPC_CLASS_CONFIGURABLE_MASK);
    if (ncfg == 0 || nctr == 0 || ncfg > 64 || nctr > 64)
        die("kpc_get_config/counter_count", (long)ncfg);

    int rc = kpc_force_all_ctrs_set(1);
    if (rc) die("kpc_force_all_ctrs_set(1)", rc);
    atexit(cleanup);

    uint64_t *config = calloc(ncfg, sizeof(uint64_t));
    config[0] = event | elmask;
    rc = kpc_set_config(KPC_CLASS_CONFIGURABLE_MASK, config);
    if (rc) die("kpc_set_config", rc);
    rc = kpc_set_counting(KPC_CLASS_CONFIGURABLE_MASK);
    if (rc) die("kpc_set_counting", rc);
    rc = kpc_set_thread_counting(KPC_CLASS_CONFIGURABLE_MASK);
    if (rc) die("kpc_set_thread_counting", rc);

    // --- guest setup (identical shape to H2) ---
    hv_return_t hrc = hv_vm_create(NULL);
    if (hrc != HV_SUCCESS) die("hv_vm_create", hrc);
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

    uint64_t *ctrs0 = calloc(nctr, sizeof(uint64_t));
    uint64_t *ctrs1 = calloc(nctr, sizeof(uint64_t));

    printf("{\"probe\":\"h3_kpc_branch\",\"event\":%" PRIu64
           ",\"elmask\":\"0x%" PRIx64 "\",\"iters\":%" PRIu64
           ",\"reps\":%d,\"ncfg\":%u,\"nctr\":%u}\n",
           event, elmask, iters, reps, ncfg, nctr);

    for (int r = 0; r < reps; r++) {
        uint64_t pc = iters ? GUEST_IPA : GUEST_IPA + 8;
        if (hv_vcpu_set_reg(vcpu, HV_REG_PC, pc) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_X0, iters) != HV_SUCCESS ||
            hv_vcpu_set_reg(vcpu, HV_REG_CPSR, 0x3c5) != HV_SUCCESS)
            die("hv_vcpu_set_reg", 0);

        uint64_t other_exits = 0, syndrome = 0;
        if (kpc_get_thread_counters(0, nctr, ctrs0)) die("thread_counters t0", 0);
        uint64_t i0 = tsc_instrs();
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
        uint64_t i1 = tsc_instrs();
        if (kpc_get_thread_counters(0, nctr, ctrs1)) die("thread_counters t1", 0);

        uint64_t x0 = ~0ull;
        hv_vcpu_get_reg(vcpu, HV_REG_X0, &x0);
        printf("{\"rep\":%d,\"d_cfg0\":%" PRIu64 ",\"d_instrs\":%" PRIu64
               ",\"other_exits\":%" PRIu64 ",\"x0_final\":%" PRIu64
               ",\"ec\":%" PRIu64 "}\n",
               r, ctrs1[0] - ctrs0[0], i1 - i0, other_exits, x0,
               (syndrome >> 26) & 0x3f);
    }

    hv_vcpu_destroy(vcpu);
    hv_vm_unmap(GUEST_IPA, GUEST_SIZE);
    hv_vm_destroy();
    cleanup();
    return 0;
}
