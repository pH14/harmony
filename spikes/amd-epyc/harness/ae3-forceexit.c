/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae3-forceexit.c — AE-3 deterministic in-kernel force-exit + exact landing
 * (docs/AMD-EPYC.md AE-3). REQUIRES the patched linux-6.18.35 kernel booted with
 * the patched kvm_amd (host/build-6.18-kernel.sh + host/stage-6.18-boot.sh): the
 * KVM_EXIT_PREEMPT UAPI (42), the deterministic-intercepts opt-in
 * (KVM_CAP_X86_DETERMINISTIC_INTERCEPTS=245), the one-shot arm ioctl
 * (KVM_ARM_PREEMPT_EXIT=_IO(KVMIO,0xe4)), and the AMD svm.c nmi_interception hook.
 *
 * MECHANISM (the AE-3 unblock). A guest-only ex_ret_brn_tkn (0xc4) perf counter is
 * armed as a SAMPLING event with sample_period = target - SKID_MARGIN. When it
 * overflows during guest execution the host PMI is delivered as an NMI, #VMEXITs
 * SVM_EXIT_NMI, and the patched nmi_interception() — seeing the determinism opt-in
 * and the one-shot preempt_armed — returns to userspace with KVM_EXIT_PREEMPT
 * BEFORE re-entering the guest. The overflow lands early (work < target, bounded by
 * the AE-1 skid); the AE-2 single-step primitive (KVM_GUESTDBG_SINGLESTEP / TF)
 * then advances instruction-by-instruction, reading the counter after each step,
 * until work == target EXACTLY. This is the run_until_overflow + single_step landing
 * contract the CpuBackend inversion names, exercised on real patched silicon.
 *
 * EVIDENCE INTEGRITY (the PR-98 lesson, binding per docs/AMD-EPYC.md):
 *  #1 gate-RC: exit status is the machine conjunction of every arm landing exactly;
 *     a completed loop is never a success condition.
 *  #4 mechanism attestation: the overflow exit reason MUST be KVM_EXIT_PREEMPT (42).
 *     A silent fallback (HLT, a SIGIO signal-kick, a different exit) cannot pass —
 *     preempt_exit==0 forces the arm to FAIL. This is the exact failure mode PR-98
 *     found: an existential harness silently exercising the stock path. Here the
 *     stock kernel returns 1 from nmi_interception (re-enter, no KVM_EXIT_PREEMPT),
 *     so on a stock/unpatched kvm_amd the guest runs to HLT and EVERY arm FAILS —
 *     the harness is structurally unable to green on the stock mechanism.
 *  #5 analytical oracle: the loop payload retires exactly one taken branch per
 *     iteration (the jnz), so "work" == guest ex_ret_brn_tkn is known by construction.
 *  #6 multiplicity/totality: every arm emits a per-record JSON line; the skid per arm
 *     is recomputed from (period, counter@preempt); overshoot (skid>margin => work>
 *     target) is a recorded FAIL, never a silently-enlarged margin.
 *
 * Replay determinism: the landed state (RIP,RCX,RFLAGS,RAX) is a pure function of the
 * target for the fixed loop payload; --mode replay runs each target twice and asserts
 * bit-identical landed digests.
 *
 * Reuses kvm-guest-hammer.c (perf_open_guest, loop payload, SVM setup) and
 * singlestep-driver.c (KVM_GUESTDBG_SINGLESTEP). Pin to a physical core; idle its SMT
 * sibling (AE-0 core map) — the caller (host/run-ae3.sh) enforces that.
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

/* Patched-6.18.35 UAPI additions (absent from the build host's system kvm.h). */
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
#define MEM_SIZE   0x10000
#define LOOP_N     300000u        /* > max target(100k) + margin: guest never HLTs first */
#define RFLAGS_TF  0x100

static long perf_open_sampling(uint64_t raw_event, int core, uint64_t period) {
    struct perf_event_attr pe;
    memset(&pe, 0, sizeof(pe));
    pe.type = PERF_TYPE_RAW;
    pe.size = sizeof(pe);
    pe.config = raw_event;
    pe.sample_period = period;         /* overflow after `period` guest taken branches */
    pe.disabled = 1;
    pe.pinned = 1;
    pe.exclude_host = 1;               /* guest-mode events only */
    pe.exclude_hv = 1;
    pe.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    return syscall(__NR_perf_event_open, &pe, 0, core, -1, 0);
}

static unsigned long cpu_irq_count(int core) {
    FILE *f = fopen("/proc/interrupts", "r");
    if (!f) return 0;
    char line[8192]; if (!fgets(line, sizeof(line), f)) { fclose(f); return 0; }
    unsigned long total = 0;
    while (fgets(line, sizeof(line), f)) {
        char *p = strchr(line, ':'); if (!p) continue; p++;
        for (int col = 0; ; col++) {
            while (*p == ' ' || *p == '\t') p++;
            if (*p < '0' || *p > '9') break;
            char *end; unsigned long v = strtoul(p, &end, 10);
            if (col == core) { total += v; break; }
            p = end;
        }
    }
    fclose(f);
    return total;
}

struct read_rec { uint64_t value, en, run; };
static uint64_t perf_read(long fd) {
    struct read_rec rr; if (read(fd, &rr, sizeof(rr)) != (ssize_t)sizeof(rr)) return (uint64_t)-1;
    return rr.value;
}

/* real-mode loop: mov ecx,LOOP_N ; 1: dec ecx ; jnz 1b ; hlt. One taken branch/iter. */
static void emit_loop(uint8_t *mem, uint32_t n) {
    uint8_t *p = mem + GUEST_PHYS; int i = 0;
    p[i++] = 0x66; p[i++] = 0xB9;
    p[i++] = n & 0xff; p[i++] = (n>>8)&0xff; p[i++] = (n>>16)&0xff; p[i++] = (n>>24)&0xff;
    p[i++] = 0x66; p[i++] = 0x49;      /* dec ecx */
    p[i++] = 0x75; p[i++] = 0xFC;      /* jnz -4  */
    p[i++] = 0xF4;                     /* hlt     */
}

struct vm { int kvm, vmfd, vcpu; struct kvm_run *run; uint8_t *mem; };

static int vm_setup(struct vm *v) {
    v->kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (v->kvm < 0) { perror("open /dev/kvm"); return -1; }
    v->vmfd = ioctl(v->kvm, KVM_CREATE_VM, 0);
    if (v->vmfd < 0) { perror("KVM_CREATE_VM"); return -1; }
    /* opt into deterministic intercepts (arms preempt_armed's gate). On a stock
     * kernel this cap is unknown -> ENABLE_CAP fails -> we STOP (unsupported is a
     * result), so the harness cannot silently proceed on the stock mechanism. */
    struct kvm_enable_cap cap; memset(&cap, 0, sizeof(cap));
    cap.cap = KVM_CAP_X86_DETERMINISTIC_INTERCEPTS;
    cap.args[0] = 1;                  /* enable (the handler stores args[0]&1) */
    if (ioctl(v->vmfd, KVM_ENABLE_CAP, &cap) < 0) {
        fprintf(stderr, "ENABLE_CAP DETERMINISTIC_INTERCEPTS failed: %s "
                        "(stock/unpatched kvm_amd? AE-3 needs the patched 6.18.35)\n",
                strerror(errno));
        return -2;
    }
    v->mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (v->mem == MAP_FAILED) { perror("mmap"); return -1; }
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0, .memory_size = MEM_SIZE,
        .userspace_addr = (uint64_t)v->mem };
    if (ioctl(v->vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) { perror("SET_MEM"); return -1; }
    v->vcpu = ioctl(v->vmfd, KVM_CREATE_VCPU, 0);
    if (v->vcpu < 0) { perror("CREATE_VCPU"); return -1; }
    int msize = ioctl(v->kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    v->run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, v->vcpu, 0);
    if (v->run == MAP_FAILED) { perror("mmap run"); return -1; }
    return 0;
}

static int vm_reset_regs(struct vm *v) {
    struct kvm_sregs s;
    if (ioctl(v->vcpu, KVM_GET_SREGS, &s) < 0) return -1;
    s.cs.base = 0; s.cs.selector = 0;
    if (ioctl(v->vcpu, KVM_SET_SREGS, &s) < 0) return -1;
    struct kvm_regs r; memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS; r.rflags = 0x2;
    return ioctl(v->vcpu, KVM_SET_REGS, &r);
}

static int set_singlestep(struct vm *v, int on) {
    struct kvm_guest_debug dbg; memset(&dbg, 0, sizeof(dbg));
    dbg.control = on ? (KVM_GUESTDBG_ENABLE | KVM_GUESTDBG_SINGLESTEP) : 0;
    return ioctl(v->vcpu, KVM_SET_GUEST_DEBUG, &dbg);
}

/* FNV-1a of the landed architectural state — a pure function of `target` for the
 * fixed loop payload, so replay with the same target must reproduce it bit-for-bit. */
static uint64_t landed_digest(struct vm *v) {
    struct kvm_regs r; if (ioctl(v->vcpu, KVM_GET_REGS, &r) < 0) return 0;
    /* RIP + RCX are the meaningful landed state of the loop payload; RFLAGS carries
     * KVM's single-step TF/RF debug bits (harness state, not guest state) and RAX is
     * always 0 here, so both are excluded from the landing identity. */
    uint64_t fields[2] = { r.rip, r.rcx };
    uint64_t h = 1469598103934665603ull;
    unsigned char *b = (unsigned char *)fields;
    for (size_t i = 0; i < sizeof(fields); i++) { h ^= b[i]; h *= 1099511628211ull; }
    return h;
}

struct arm_result {
    uint64_t target, period, work_at_preempt, work_landed, skid, digest;
    int preempt_exit, landed_exact, overshoot, irq_dirty, rearmed;
};

/* One armed deadline: overflow-to-near-target, then single-step to work==target. */
static int do_arm(struct vm *v, long fd, uint64_t target, uint64_t margin, int core,
                  struct arm_result *out) {
    memset(out, 0, sizeof(*out));
    out->target = target;
    emit_loop(v->mem, LOOP_N);
    if (vm_reset_regs(v) < 0) return -1;
    if (set_singlestep(v, 0) < 0) return -1;

    ioctl(fd, PERF_EVENT_IOC_RESET, 0);
    unsigned long irq0 = cpu_irq_count(core);

    uint64_t use_overflow = (target > margin);
    uint64_t period = use_overflow ? (target - margin) : 0;
    out->period = period;

    if (use_overflow) {
        /* Arm the sampling period + the in-kernel one-shot exit, run to the overflow.
         * A spurious NMI (e.g. nmi_watchdog=1) can fire KVM_EXIT_PREEMPT BEFORE the
         * overflow — work_at_preempt < period, a negative "skid". Rather than land a
         * bogus arm, re-prime the guest+counter and re-arm (bounded); if it still comes
         * in premature after the budget, keep it (the checker hard-fails negative skid). */
        for (int attempt = 0; ; attempt++) {
            ioctl(fd, PERF_EVENT_IOC_PERIOD, &period);
            ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
            if (ioctl(v->vcpu, KVM_ARM_PREEMPT_EXIT, 0) < 0) {
                fprintf(stderr, "KVM_ARM_PREEMPT_EXIT failed: %s\n", strerror(errno));
                return -1;
            }
            for (;;) {
                if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno == EINTR) continue; perror("KVM_RUN"); return -1; }
                break;
            }
            out->preempt_exit = (v->run->exit_reason == KVM_EXIT_PREEMPT);
            out->work_at_preempt = perf_read(fd);
            if (!out->preempt_exit) {
                /* Mechanism attestation failure: overflow did not force the in-kernel
                 * exit (stock path, or a different exit). Recorded FAIL; do not land. */
                return 0;
            }
            if ((int64_t)(out->work_at_preempt - period) < 0 && attempt < 8) {
                out->rearmed = 1;                 /* premature: re-prime + re-arm */
                emit_loop(v->mem, LOOP_N);
                if (vm_reset_regs(v) < 0) return -1;
                ioctl(fd, PERF_EVENT_IOC_RESET, 0);
                continue;
            }
            break;
        }
        /* Push the next overflow far away so the sampling PMI machinery cannot fire
         * during the single-step landing (which otherwise occasionally lets a step
         * retire 2 taken branches, overshooting target by 1). The accumulated count is
         * preserved by PERF_EVENT_IOC_PERIOD; only the overflow point moves. */
        uint64_t huge = 1ULL << 40;
        ioctl(fd, PERF_EVENT_IOC_PERIOD, &huge);
    } else {
        ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
        out->work_at_preempt = 0;   /* stepping from the start */
    }

    out->skid = use_overflow ? (out->work_at_preempt - period) : 0;   /* work - (target-margin) */
    out->overshoot = (out->work_at_preempt > target);

    /* Single-step to the exact target. Each step retires <=1 taken branch, so work
     * climbs to target without skipping it. Budget covers margin+skid worst case. */
    if (set_singlestep(v, 1) < 0) return -1;
    uint64_t budget = 4 * (margin + 64), steps = 0, work = out->work_at_preempt;
    int landed = 0;
    while (work < target && steps < budget) {
        if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno == EINTR) continue; perror("KVM_RUN step"); return -1; }
        if (v->run->exit_reason != KVM_EXIT_DEBUG) {
            fprintf(stderr, "unexpected exit %u during landing (work=%llu target=%llu)\n",
                    v->run->exit_reason, (unsigned long long)work, (unsigned long long)target);
            break;
        }
        work = perf_read(fd);
        steps++;
    }
    out->work_landed = work;
    landed = (work == target);
    out->landed_exact = landed;
    out->digest = landed_digest(v);   /* always: lets us see if an over-read landed the
                                         guest at the right RIP with only the counter +1 */
    out->irq_dirty = (cpu_irq_count(core) - irq0) ? 1 : 0;
    return 0;
}

int main(int argc, char **argv) {
    uint64_t raw_event = 0xc4, margin = 16384, arms = 1000;
    unsigned seed = 1;
    int core = -1, replay = 0, smoke = 0; const char *out = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--core") && i+1<argc) core = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--event") && i+1<argc) raw_event = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--margin") && i+1<argc) margin = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--arms") && i+1<argc) arms = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--seed") && i+1<argc) seed = (unsigned)strtoul(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--replay")) replay = 1;
        else if (!strcmp(argv[i], "--smoke")) { smoke = 1; arms = 1; }
        else if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];
        else { fprintf(stderr, "usage: %s --core N [--smoke|--replay] [--arms K --margin M --seed S --event 0xHEX --out f]\n", argv[0]); return 2; }
    }
    if (core < 0) { fprintf(stderr, "need --core N\n"); return 2; }
    cpu_set_t cs; CPU_ZERO(&cs); CPU_SET(core, &cs);
    if (sched_setaffinity(0, sizeof(cs), &cs) != 0 || sched_getcpu() != core) {
        fprintf(stderr, "pin to core %d failed\n", core); return 2; }

    struct vm v;
    int rc = vm_setup(&v);
    if (rc == -2) return 3;               /* opt-in cap absent => unsupported (stock kernel) */
    if (rc != 0) return 2;
    long fd = perf_open_sampling(raw_event, core, margin ? margin : 1);
    if (fd < 0) { fprintf(stderr, "perf_open_sampling: %s\n", strerror(errno)); return 2; }

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o, "[\n");
    int all_ok = 1;
    uint64_t n_preempt = 0, n_exact = 0, n_overshoot = 0, skid_max = 0;

    for (uint64_t a = 0; a < arms; a++) {
        /* seeded target in [1,100000] (the doc's delta range). rand_r => reproducible. */
        uint64_t target = smoke ? 50000 : (1 + rand_r(&seed) % 100000);
        struct arm_result r1;
        if (do_arm(&v, fd, target, margin, core, &r1) < 0) { all_ok = 0; break; }
        if (r1.preempt_exit || !(target > margin)) n_preempt++;
        if (r1.landed_exact) n_exact++;
        if (r1.overshoot) n_overshoot++;
        if (r1.skid > skid_max) skid_max = r1.skid;

        uint64_t digest2 = 0; int replay_match = 1;
        if (replay) {
            struct arm_result r2;
            if (do_arm(&v, fd, target, margin, core, &r2) < 0) { all_ok = 0; break; }
            digest2 = r2.digest;
            replay_match = r2.landed_exact && (r2.digest == r1.digest);
        }

        /* irq_dirty (a host timer tick on the pinned core, unavoidable over the >1ms
         * single-step landing at CONFIG_HZ=1000) is ORTHOGONAL to the landing: the work
         * counter is guest-only (exclude_host) and single-step reads the exact guest
         * value, so a host IRQ cannot perturb work==target. Recorded, not a fail
         * condition — unlike AE-1's differential windows where a tick could add a count. */
        int arm_ok = r1.landed_exact && !r1.overshoot &&
                     (r1.period == 0 || r1.preempt_exit) && replay_match;
        if (!arm_ok) all_ok = 0;
        fprintf(o, "  {\"kind\":\"arm\",\"idx\":%llu,\"target\":%llu,\"margin\":%llu,"
                   "\"period\":%llu,\"work_at_preempt\":%llu,\"skid\":%llu,"
                   "\"work_landed\":%llu,\"preempt_exit\":%d,\"landed_exact\":%d,"
                   "\"overshoot\":%d,\"irq_dirty\":%d,\"rearmed\":%d,\"digest\":\"%016llx\","
                   "\"replay\":%d,\"replay_digest\":\"%016llx\",\"replay_match\":%d,"
                   "\"core\":%d,\"ok\":%d},\n",
                (unsigned long long)a, (unsigned long long)target, (unsigned long long)margin,
                (unsigned long long)r1.period, (unsigned long long)r1.work_at_preempt,
                (unsigned long long)r1.skid, (unsigned long long)r1.work_landed,
                r1.preempt_exit, r1.landed_exact, r1.overshoot, r1.irq_dirty, r1.rearmed,
                (unsigned long long)r1.digest, replay, (unsigned long long)digest2,
                replay_match, core, arm_ok);
    }
    fprintf(o, "  {\"kind\":\"end\",\"arms\":%llu,\"n_preempt\":%llu,\"n_exact\":%llu,"
               "\"n_overshoot\":%llu,\"skid_max\":%llu,\"event\":%llu,\"rc\":%d}\n]\n",
            (unsigned long long)arms, (unsigned long long)n_preempt,
            (unsigned long long)n_exact, (unsigned long long)n_overshoot,
            (unsigned long long)skid_max, (unsigned long long)raw_event, all_ok ? 0 : 1);
    if (out) fclose(o);
    fprintf(stderr, "[ae3] arms=%llu preempt=%llu exact=%llu overshoot=%llu skid_max=%llu rc=%d\n",
            (unsigned long long)arms, (unsigned long long)n_preempt, (unsigned long long)n_exact,
            (unsigned long long)n_overshoot, (unsigned long long)skid_max, all_ok ? 0 : 1);
    return all_ok ? 0 : 1;
}
