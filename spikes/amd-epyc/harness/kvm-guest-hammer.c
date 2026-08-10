/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * kvm-guest-hammer.c — AE-1(b) guest-mode work-clock exactness (docs/AMD-EPYC.md AE-1(b)).
 *
 * The minimal SVM KVM harness: one vCPU, pinned, runs an analytical-oracle guest payload
 * (a real-mode loop of exactly N taken branches then HLT) and counts GUEST-only
 * ex_ret_brn_tkn (perf_event_open exclude_host=1) across KVM_RUN. Equal guest streams ->
 * equal counts, judged by the DIFFERENTIAL against the by-construction oracle: the loop
 * retires exactly N-1 taken branches, so count(N2)-count(N1) == N2-N1 exactly iff guest-
 * mode counting is bit-exact. This is the guest-mode twin of amd-hammer's host-side (a).
 *
 * Reuses the established minimal-KVM setup (consonance/vmm-backend/src/kvm_sys.rs
 * KvmBackend::build sequence) and the analytical-oracle idea from
 * consonance/vmm-backend/tests/n2_nested_hammer.rs (SPIN_CODE). Guest-only attribution is
 * exclude_host=1 on the counter attached to the KVM_RUN thread — the vendor difference
 * vs the Intel harness is ONLY the perf event (0xc4 vs 0x1c4); the KVM userspace ioctl
 * surface is arch-agnostic (kvm_amd/SVM instead of kvm_intel/VMX).
 *
 * Evidence integrity: RC is the conjunction of every clean-window exactness check (#1);
 * per-sample JSON records (#6); analytical oracle (#5); the exit reason is asserted to be
 * KVM_EXIT_HLT (the guest ran to its HLT, not a fault) — a silent guest fault cannot
 * masquerade as a completed run. Interrupt contamination is accounted as in amd-hammer:
 * a window whose guest count exceeds the oracle by the host-IRQ delta is not clean.
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

#define GUEST_PHYS 0x1000
#define MEM_SIZE   0x10000        /* 64 KiB guest RAM */

static long perf_open_guest(uint64_t raw_event, int core) {
    struct perf_event_attr pe;
    memset(&pe, 0, sizeof(pe));
    pe.type = PERF_TYPE_RAW;
    pe.size = sizeof(pe);
    pe.config = raw_event;
    pe.disabled = 1;
    pe.pinned = 1;
    pe.exclude_host = 1;          /* count GUEST-mode events only */
    pe.exclude_hv = 1;
    pe.read_format = PERF_FORMAT_TOTAL_TIME_ENABLED | PERF_FORMAT_TOTAL_TIME_RUNNING;
    /* pid=0 (this thread runs KVM_RUN), cpu=core */
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

/* Emit a real-mode payload at GUEST_PHYS that loops `n` times (n-1 taken jnz) then HLTs.
 *   66 B9 <n:32>     mov ecx, n
 * 1:66 49            dec ecx
 *   75 FC            jnz 1b        ; rel8 = -4
 *   F4               hlt
 * Oracle taken branches = n-1 (the jnz), + 0 others. Returns the payload length. */
static int emit_loop_payload(uint8_t *mem, uint32_t n) {
    uint8_t *p = mem + GUEST_PHYS;
    int i = 0;
    p[i++] = 0x66; p[i++] = 0xB9;                                 /* mov ecx, imm32 */
    p[i++] = n & 0xff; p[i++] = (n >> 8) & 0xff;
    p[i++] = (n >> 16) & 0xff; p[i++] = (n >> 24) & 0xff;
    p[i++] = 0x66; p[i++] = 0x49;                                 /* dec ecx */
    p[i++] = 0x75; p[i++] = 0xFC;                                 /* jnz -4 */
    p[i++] = 0xF4;                                                /* hlt */
    return i;
}

struct vm {
    int kvm, vmfd, vcpu;
    struct kvm_run *run;
    uint8_t *mem;
};

static int vm_setup(struct vm *v) {
    v->kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (v->kvm < 0) { perror("open /dev/kvm"); return -1; }
    v->vmfd = ioctl(v->kvm, KVM_CREATE_VM, 0);
    if (v->vmfd < 0) { perror("KVM_CREATE_VM"); return -1; }
    v->mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    if (v->mem == MAP_FAILED) { perror("mmap guest mem"); return -1; }
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0, .memory_size = MEM_SIZE,
        .userspace_addr = (uint64_t)v->mem,
    };
    if (ioctl(v->vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) { perror("SET_MEM"); return -1; }
    v->vcpu = ioctl(v->vmfd, KVM_CREATE_VCPU, 0);
    if (v->vcpu < 0) { perror("KVM_CREATE_VCPU"); return -1; }
    int msize = ioctl(v->kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    v->run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, v->vcpu, 0);
    if (v->run == MAP_FAILED) { perror("mmap kvm_run"); return -1; }
    return 0;
}

/* set real-mode CS:IP so execution starts at GUEST_PHYS (CS=0, IP=GUEST_PHYS) */
static int vm_reset_regs(struct vm *v) {
    struct kvm_sregs s;
    if (ioctl(v->vcpu, KVM_GET_SREGS, &s) < 0) return -1;
    s.cs.base = 0; s.cs.selector = 0;
    if (ioctl(v->vcpu, KVM_SET_SREGS, &s) < 0) return -1;
    struct kvm_regs r; memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS; r.rflags = 0x2;
    return ioctl(v->vcpu, KVM_SET_REGS, &r);
}

/* run to HLT; returns 0 on KVM_EXIT_HLT, -1 otherwise (a fault must not pass) */
static int vm_run_to_hlt(struct vm *v) {
    for (;;) {
        if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno == EINTR) continue; perror("KVM_RUN"); return -1; }
        if (v->run->exit_reason == KVM_EXIT_HLT) return 0;
        /* any other exit (IO, MMIO, fail_entry, shutdown) is a payload/harness fault */
        return -(int)v->run->exit_reason;
    }
}

struct read_rec { uint64_t value, en, run; };

int main(int argc, char **argv) {
    uint64_t raw_event = 0xc4, n1 = 100000, n2 = 200000;
    int reps = 40, core = -1; const char *out = 0;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--event") && i+1<argc) raw_event = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--n1") && i+1<argc) n1 = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--n2") && i+1<argc) n2 = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--reps") && i+1<argc) reps = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--core") && i+1<argc) core = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];
        else { fprintf(stderr, "usage: %s --core N [--event 0xHEX --n1 --n2 --reps --out]\n", argv[0]); return 2; }
    }
    if (core < 0) { fprintf(stderr, "need --core N\n"); return 2; }
    cpu_set_t cs; CPU_ZERO(&cs); CPU_SET(core, &cs);
    if (sched_setaffinity(0, sizeof(cs), &cs) != 0 || sched_getcpu() != core) {
        fprintf(stderr, "pin to core %d failed\n", core); return 2; }

    struct vm v;
    if (vm_setup(&v) != 0) return 2;
    long fd = perf_open_guest(raw_event, core);
    if (fd < 0) { fprintf(stderr, "perf_open_guest failed: %s\n", strerror(errno)); return 2; }

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o, "[\n");
    int all_ok = 1;
    for (int r = 0; r < reps; r++) {
        struct read_rec rr[2]; uint64_t ns[2] = { n1, n2 }; unsigned long irqs[2] = {0,0};
        int hlt_ok = 1, mux = 0;
        for (int j = 0; j < 2; j++) {
            emit_loop_payload(v.mem, (uint32_t)ns[j]);
            if (vm_reset_regs(&v) < 0) { fprintf(stderr, "reset regs failed\n"); return 2; }
            unsigned long i0 = cpu_irq_count(core);
            ioctl(fd, PERF_EVENT_IOC_RESET, 0);
            ioctl(fd, PERF_EVENT_IOC_ENABLE, 0);
            int e = vm_run_to_hlt(&v);
            ioctl(fd, PERF_EVENT_IOC_DISABLE, 0);
            irqs[j] = cpu_irq_count(core) - i0;
            if (e != 0) hlt_ok = 0;
            if (read(fd, &rr[j], sizeof(rr[j])) != (ssize_t)sizeof(rr[j])) return 2;
            if (rr[j].en != rr[j].run) mux = 1;
        }
        uint64_t delta = rr[1].value - rr[0].value;
        uint64_t oracle = n2 - n1;                 /* n-1 taken/loop -> differential = n2-n1 */
        int clean = (irqs[0] == 0 && irqs[1] == 0);
        int exact = (delta == oracle) && !mux && hlt_ok;
        if (clean && !exact) all_ok = 0;
        fprintf(o, "  {\"kind\":\"guest_exactness\",\"event\":%llu,\"rep\":%d,"
                   "\"n1\":%llu,\"n2\":%llu,\"count_n1\":%llu,\"count_n2\":%llu,"
                   "\"delta\":%llu,\"oracle_delta\":%llu,\"taken_per_iter\":1,"
                   "\"irqs_n1\":%lu,\"irqs_n2\":%lu,\"clean\":%d,\"hlt_ok\":%d,"
                   "\"multiplexed\":%d,\"core\":%d,\"exact\":%d},\n",
                (unsigned long long)raw_event, r, (unsigned long long)n1, (unsigned long long)n2,
                (unsigned long long)rr[0].value, (unsigned long long)rr[1].value,
                (unsigned long long)delta, (unsigned long long)oracle,
                irqs[0], irqs[1], clean, hlt_ok, mux, core, exact);
    }
    fprintf(o, "  {\"kind\":\"end\",\"rc\":%d}\n]\n", all_ok ? 0 : 1);
    if (out) fclose(o);
    return all_ok ? 0 : 1;
}
