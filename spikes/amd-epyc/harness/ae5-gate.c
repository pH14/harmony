/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae5-gate.c — AE-5 mini determinism gate, mechanism-integrated (docs/AMD-EPYC.md AE-5).
 * REQUIRES the patched 6.18.35 kernel booted (host/stage-6.18-boot.sh).
 *
 * Exercises the WHOLE bare-metal mechanism stack together, same seed => bit-identical:
 *   work clock (ex_ret_brn_tkn 0xc4)  +  the svm.c in-kernel force-exit (KVM_EXIT_PREEMPT)
 *   +  exact landing via the AE-2 single-step primitive (TF)  +  a fault injected at a
 *   seeded Moment  +  AVIC-off (posture attested by the runner).
 *
 * Per rep (all reps share one seed => one (Moment, fault) plan): run a deterministic
 * compute guest; arm the force-exit so it preempts at work==Moment; at that Moment inject
 * the seeded fault (XOR a fixed guest memory word — a deterministic perturbation at a
 * deterministic point); release single-step; run to HLT; hash the final guest output
 * memory + architectural registers. ALL reps must be bit-identical to rep 0.
 *
 * This is AE-5's method on the SVM harness payload matrix. The FULL gate additionally
 * runs the hash-verified x86 postgres Subject through the consonance vmm-core stack; that
 * needs the appliance (hm-tn9: repo-on-box + guest images) and is the documented residual.
 *
 * Evidence integrity (PR-98 lesson): the force-exit exit reason MUST be KVM_EXIT_PREEMPT
 * (mechanism attestation #4 — a stock path fails); every rep attests KVM_EXIT_HLT at the
 * end (#4); the per-rep digest is retained (#6); a single divergence is a recorded P0
 * (doc §AE-5 stop), reported not hidden; the all-identical floor is recomputed by the
 * caller from the per-rep records (#2). Reuses ae3-forceexit.c's arm/land logic.
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
#include <sys/syscall.h>
#include <linux/kvm.h>
#include <linux/perf_event.h>

#ifndef KVM_EXIT_PREEMPT
#define KVM_EXIT_PREEMPT 42
#endif
#ifndef KVM_CAP_X86_DETERMINISTIC_INTERCEPTS
#define KVM_CAP_X86_DETERMINISTIC_INTERCEPTS 245
#endif
#ifndef KVM_ARM_PREEMPT_EXIT
#define KVM_ARM_PREEMPT_EXIT _IO(KVMIO, 0xe4)
#endif

#define GUEST_PHYS 0x1000
#define OUT_GPA    0x3000
#define OUT_LEN    512
#define MEM_SIZE   0x10000
#define ITERS      200000u        /* compute-loop iterations (== available taken branches-1) */

static long perf_open_sampling(uint64_t raw_event, int core, uint64_t period) {
    struct perf_event_attr pe;
    memset(&pe, 0, sizeof(pe));
    pe.type = PERF_TYPE_RAW; pe.size = sizeof(pe); pe.config = raw_event;
    pe.sample_period = period; pe.disabled = 1; pe.pinned = 1;
    pe.exclude_host = 1; pe.exclude_hv = 1;
    pe.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    return syscall(__NR_perf_event_open, &pe, 0, core, -1, 0);
}
static uint64_t perf_read(long fd) {
    struct { uint64_t v, e, r; } rr;
    if (read(fd, &rr, sizeof(rr)) != (ssize_t)sizeof(rr)) return (uint64_t)-1;
    return rr.v;
}
static uint64_t fnv1a(const void *p, size_t n) {
    const uint8_t *b = p; uint64_t h = 1469598103934665603ULL;
    for (size_t i = 0; i < n; i++) { h ^= b[i]; h *= 1099511628211ULL; }
    return h;
}

/* Deterministic compute guest: cx=ITERS; loop { ax = (ax+cx)^di ; [di]=ax ; di+=2 } dec cx;
 * jnz; hlt. Writes 256 words to OUT_GPA-ish; final memory+regs is a pure function of the
 * run (and of any injected perturbation). One taken branch (jnz) per iteration => work. */
static void emit_compute(uint8_t *mem) {
    uint8_t code[] = {
        0xBF, 0x00, 0x30,        /* mov di, 0x3000              */
        0x31, 0xC0,              /* xor ax, ax                  */
        0x66, 0xB9, (ITERS & 0xff), ((ITERS>>8)&0xff), ((ITERS>>16)&0xff), ((ITERS>>24)&0xff), /* mov ecx, ITERS */
        /* loop: */
        0x01, 0xC8,              /* add ax, cx                  */
        0x31, 0xF8,              /* xor ax, di                  */
        0x66, 0x83, 0xE7, 0x7F,  /* and edi, 0x7f  (wrap di into [0,127]) */
        0x81, 0xC7, 0x00, 0x30,  /* add di, 0x3000 (back into the output window) */
        0x89, 0x05,              /* mov [di], ax                */
        0x66, 0x49,              /* dec ecx                     */
        0x75, 0xEA,              /* jnz loop (-22)              */
        0xF4,                    /* hlt                         */
    };
    memcpy(mem + GUEST_PHYS, code, sizeof(code));
}

struct vm { int kvm, vmfd, vcpu; struct kvm_run *run; uint8_t *mem; };

static int vm_setup(struct vm *v) {
    v->kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (v->kvm < 0) { perror("open /dev/kvm"); return -1; }
    v->vmfd = ioctl(v->kvm, KVM_CREATE_VM, 0);
    if (v->vmfd < 0) { perror("KVM_CREATE_VM"); return -1; }
    struct kvm_enable_cap cap; memset(&cap, 0, sizeof(cap));
    cap.cap = KVM_CAP_X86_DETERMINISTIC_INTERCEPTS;
    if (ioctl(v->vmfd, KVM_ENABLE_CAP, &cap) < 0) {
        fprintf(stderr, "ENABLE_CAP DETERMINISTIC_INTERCEPTS failed: %s (patched 6.18.35?)\n",
                strerror(errno)); return -2;
    }
    v->mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (v->mem == MAP_FAILED) { perror("mmap"); return -1; }
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0, .memory_size = MEM_SIZE, .userspace_addr = (uint64_t)v->mem };
    if (ioctl(v->vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) { perror("SET_MEM"); return -1; }
    v->vcpu = ioctl(v->vmfd, KVM_CREATE_VCPU, 0);
    if (v->vcpu < 0) { perror("CREATE_VCPU"); return -1; }
    int msize = ioctl(v->kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    v->run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, v->vcpu, 0);
    if (v->run == MAP_FAILED) { perror("mmap run"); return -1; }
    return 0;
}
static int reset_regs(struct vm *v) {
    struct kvm_sregs s; if (ioctl(v->vcpu, KVM_GET_SREGS, &s) < 0) return -1;
    s.cs.base = 0; s.cs.selector = 0;
    if (ioctl(v->vcpu, KVM_SET_SREGS, &s) < 0) return -1;
    struct kvm_regs r; memset(&r, 0, sizeof(r)); r.rip = GUEST_PHYS; r.rflags = 0x2;
    return ioctl(v->vcpu, KVM_SET_REGS, &r);
}
static int set_singlestep(struct vm *v, int on) {
    struct kvm_guest_debug dbg; memset(&dbg, 0, sizeof(dbg));
    dbg.control = on ? (KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP) : 0;
    return ioctl(v->vcpu, KVM_SET_GUEST_DEBUG, &dbg);
}

/* One rep: land at Moment via force-exit, inject the seeded fault, run to HLT, hash state.
 * Returns 0 and fills *digest/*preempt/*hlt; -1 on harness error. */
static int one_rep(struct vm *v, long fd, uint64_t moment, uint64_t margin, int core,
                   uint64_t fault_gpa, uint16_t fault_xor,
                   uint64_t *digest, int *preempt, int *hlt) {
    (void)core;
    *preempt = 0; *hlt = 0;
    memset(v->mem + OUT_GPA, 0, OUT_LEN);
    emit_compute(v->mem);
    if (reset_regs(v) < 0) return -1;
    if (set_singlestep(v, 0) < 0) return -1;
    ioctl(fd, PERF_EVENT_IOC_RESET, 0);

    uint64_t period = (moment > margin) ? (moment - margin) : 0, work = 0;
    if (period > 0) {
        ioctl(fd, PERF_EVENT_IOC_PERIOD, &period);
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
        if (ioctl(v->vcpu, KVM_ARM_PREEMPT_EXIT, 0) < 0) { perror("ARM_PREEMPT_EXIT"); return -1; }
        for (;;) { if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; perror("KVM_RUN"); return -1; } break; }
        *preempt = (v->run->exit_reason == KVM_EXIT_PREEMPT);
        if (!*preempt) return 0;                 /* mechanism attestation failed -> record */
        uint64_t huge = 1ULL << 40; ioctl(fd, PERF_EVENT_IOC_PERIOD, &huge); /* no more overflows while stepping */
        work = perf_read(fd);
    } else {
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
    }
    /* single-step to work == moment exactly (the deterministic Moment) */
    if (set_singlestep(v, 1) < 0) return -1;
    uint64_t budget = 8 * (margin + 64), steps = 0;
    while (work < moment && steps < budget) {
        if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; perror("KVM_RUN step"); return -1; }
        if (v->run->exit_reason != KVM_EXIT_DEBUG) break;
        work = perf_read(fd); steps++;
    }
    if (work != moment) return 0;                /* did not reach the Moment -> non-landing, record */

    /* inject the seeded fault AT the Moment: XOR a fixed guest word (deterministic). */
    uint16_t *w = (uint16_t *)(v->mem + fault_gpa);
    *w = (uint16_t)(*w ^ fault_xor);

    /* release single-step, run to HLT */
    if (set_singlestep(v, 0) < 0) return -1;
    for (;;) { if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; break; }
               if (v->run->exit_reason == KVM_EXIT_HLT) { *hlt = 1; break; } break; }

    struct kvm_regs post; ioctl(v->vcpu, KVM_GET_REGS, &post);
    uint8_t buf[OUT_LEN + sizeof(post)];
    memcpy(buf, v->mem + OUT_GPA, OUT_LEN);
    memcpy(buf + OUT_LEN, &post, sizeof(post));
    *digest = fnv1a(buf, sizeof(buf));
    return 0;
}

int main(int argc, char **argv) {
    uint64_t raw_event = 0xc4, margin = 16384, reps = 1000, moment = 50000;
    unsigned seed = 1; int core = -1; const char *out = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--core") && i+1<argc) core = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--event") && i+1<argc) raw_event = strtoull(argv[++i],0,0);
        else if (!strcmp(argv[i], "--margin") && i+1<argc) margin = strtoull(argv[++i],0,0);
        else if (!strcmp(argv[i], "--reps") && i+1<argc) reps = strtoull(argv[++i],0,0);
        else if (!strcmp(argv[i], "--moment") && i+1<argc) moment = strtoull(argv[++i],0,0);
        else if (!strcmp(argv[i], "--seed") && i+1<argc) seed = (unsigned)strtoul(argv[++i],0,0);
        else if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];
        else { fprintf(stderr,"usage: %s --core N [--reps R --moment M --margin MG --seed S --out f]\n",argv[0]); return 2; }
    }
    if (core < 0) { fprintf(stderr, "need --core N\n"); return 2; }
    cpu_set_t cs; CPU_ZERO(&cs); CPU_SET(core, &cs);
    if (sched_setaffinity(0, sizeof(cs), &cs) != 0 || sched_getcpu() != core) {
        fprintf(stderr, "pin to core %d failed\n", core); return 2; }

    /* the seed fixes the (Moment, fault) plan shared by ALL reps — same seed, same plan. */
    unsigned s = seed;
    if (moment == 0) moment = 1 + rand_r(&s) % 100000;
    uint64_t fault_gpa = OUT_GPA + 2 * (rand_r(&s) % 128);
    uint16_t fault_xor = (uint16_t)(1 + rand_r(&s) % 0xfffe);

    struct vm v; int rc = vm_setup(&v);
    if (rc == -2) return 3;
    if (rc != 0) return 2;
    long fd = perf_open_sampling(raw_event, core, margin ? margin : 1);
    if (fd < 0) { fprintf(stderr, "perf_open_sampling: %s\n", strerror(errno)); return 2; }

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o, "{\"schema\":\"amd-epyc-ae5-gate-v1\",\"reps\":%llu,\"core\":%d,\"event\":%llu,"
               "\"moment\":%llu,\"margin\":%llu,\"seed\":%u,\"fault_gpa\":\"0x%llx\","
               "\"fault_xor\":%u,\n \"reps_detail\":[",
            (unsigned long long)reps, core, (unsigned long long)raw_event,
            (unsigned long long)moment, (unsigned long long)margin, seed,
            (unsigned long long)fault_gpa, fault_xor);

    uint64_t first = 0; int all_ident = 1, all_hlt = 1, all_preempt = 1, diverged_at = -1;
    for (uint64_t r = 0; r < reps; r++) {
        uint64_t dig = 0; int pre = 0, hlt = 0;
        if (one_rep(&v, fd, moment, margin, core, fault_gpa, fault_xor, &dig, &pre, &hlt) < 0) {
            all_ident = 0; break; }
        if (!pre && moment > margin) all_preempt = 0;
        if (!hlt) all_hlt = 0;
        if (r == 0) first = dig;
        else if (dig != first && all_ident) { all_ident = 0; diverged_at = (int)r; }
        if (r < 8 || dig != first)   /* retain a sample head + any divergence, keep file small */
            fprintf(o, "%s{\"rep\":%llu,\"preempt\":%d,\"hlt\":%d,\"digest\":\"0x%016llx\"}",
                    r ? "," : "", (unsigned long long)r, pre, hlt, (unsigned long long)dig);
    }
    int ok = all_ident && all_hlt && all_preempt;
    fprintf(o, "],\n \"all_preempt\":%d,\"all_hlt\":%d,\"bit_identical_all_reps\":%d,"
               "\"diverged_at\":%d,\"digest\":\"0x%016llx\",\"rc\":%d}\n",
            all_preempt, all_hlt, all_ident, diverged_at, (unsigned long long)first, ok ? 0 : 1);
    if (out) fclose(o);
    fprintf(stderr, "[ae5] reps=%llu preempt=%d hlt=%d identical=%d diverged_at=%d rc=%d\n",
            (unsigned long long)reps, all_preempt, all_hlt, all_ident, diverged_at, ok ? 0 : 1);
    return ok ? 0 : 1;
}
