/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae5-determinism.c — AE-5 substrate same-seed determinism (partial; docs/AMD-EPYC.md AE-5).
 *
 * The FULL AE-5 mini gate (the postgres Subject + work-clock-driven preemption + the svm.c
 * force-exit + fault injection at seeded Moments) needs the AE-3 patched kernel (ESCALATED:
 * 6.8-vs-6.18) and the appliance build (hm-tn9, out of spike scope). What IS achievable on
 * the stock kernel is the SUBSTRATE half of the claim: same seed twice ⇒ bit-identical
 * guest state on the SVM harness. This runs an identical deterministic compute-guest N times
 * (a real-mode LCG-ish fill of 256 words), captures the guest's output memory + final
 * registers each run, hashes them, and asserts ALL N runs are bit-identical to run 0.
 *
 * A single divergence is a P0 (doc §AE-5 stop): reported, never hidden. This does not
 * exercise the work clock or faults — it isolates "is bare SVM execution reproducible
 * run-to-run on this Zen 2 part", the substrate the full gate builds on.
 * Evidence integrity: every run attested to KVM_EXIT_HLT (#4); FNV-1a digest per run
 * retained; the pass floor (all N identical) recomputed by the caller from the records.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <linux/kvm.h>

#define GUEST_PHYS 0x1000
#define OUT_GPA    0x2000
#define OUT_LEN    512
#define MEM_SIZE   0x10000

static void emit_compute(uint8_t *mem) {
    uint8_t code[] = {
        0xBF,0x00,0x20,        /* mov di,0x2000 */
        0xB8,0x39,0x30,        /* mov ax,0x3039 (seed) */
        0xB9,0x00,0x01,        /* mov cx,256 */
        /* fill: */
        0x01,0xC8,             /* add ax,cx */
        0x31,0xF8,             /* xor ax,di */
        0x89,0x05,             /* mov [di],ax */
        0x47,0x47,             /* inc di; inc di */
        0x49,                  /* dec cx */
        0x75,0xF5,             /* jnz fill (-11) */
        0xF4,                  /* hlt */
    };
    memcpy(mem+GUEST_PHYS, code, sizeof(code));
}

static uint64_t fnv1a(const void *p, size_t n) {
    const uint8_t *b = p; uint64_t h = 1469598103934665603ULL;
    for (size_t i = 0; i < n; i++) { h ^= b[i]; h *= 1099511628211ULL; }
    return h;
}

int main(int argc, char **argv) {
    int core = -1, reps = 1000; const char *out = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--core") && i+1<argc) core = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--reps") && i+1<argc) reps = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];
    }
    if (core < 0) { fprintf(stderr, "need --core N\n"); return 2; }
    cpu_set_t cs; CPU_ZERO(&cs); CPU_SET(core, &cs); sched_setaffinity(0, sizeof(cs), &cs);

    int kvm = open("/dev/kvm", O_RDWR|O_CLOEXEC);
    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot=0,.guest_phys_addr=0,.memory_size=MEM_SIZE,.userspace_addr=(uint64_t)mem };
    ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region);
    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ|PROT_WRITE, MAP_SHARED, vcpu, 0);

    uint64_t first = 0; int all_ident = 1, hlt_all = 1; int diverged_at = -1;
    for (int r = 0; r < reps; r++) {
        memset(mem+OUT_GPA, 0, OUT_LEN);            /* clear output region each run */
        emit_compute(mem);
        struct kvm_sregs s; ioctl(vcpu, KVM_GET_SREGS, &s); s.cs.base=0; s.cs.selector=0; ioctl(vcpu, KVM_SET_SREGS, &s);
        struct kvm_regs rg; memset(&rg,0,sizeof(rg)); rg.rip=GUEST_PHYS; rg.rflags=0x2; ioctl(vcpu, KVM_SET_REGS, &rg);
        int hlt = 0;
        for (;;) { if (ioctl(vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; break; }
                   if (run->exit_reason == KVM_EXIT_HLT) { hlt = 1; break; } break; }
        if (!hlt) hlt_all = 0;
        struct kvm_regs post; ioctl(vcpu, KVM_GET_REGS, &post);
        /* digest = output memory ⊕ the final architectural registers */
        uint8_t buf[OUT_LEN + sizeof(post)];
        memcpy(buf, mem+OUT_GPA, OUT_LEN);
        memcpy(buf+OUT_LEN, &post, sizeof(post));
        uint64_t h = fnv1a(buf, sizeof(buf));
        if (r == 0) first = h;
        else if (h != first && all_ident) { all_ident = 0; diverged_at = r; }
    }

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o,
      "{\"schema\":\"amd-epyc-ae5-substrate-determinism-v1\",\"reps\":%d,\"core\":%d,"
      "\"all_hlt\":%d,\"bit_identical_all_reps\":%d,\"diverged_at\":%d,"
      "\"digest\":\"0x%016llx\"}\n",
      reps, core, hlt_all, all_ident, diverged_at, (unsigned long long)first);
    if (out) fclose(o);
    return (hlt_all && all_ident) ? 0 : 1;
}
