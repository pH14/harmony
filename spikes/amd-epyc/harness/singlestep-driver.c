/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * singlestep-driver.c — AE-2 single-step characterization without MTF (docs/AMD-EPYC.md §1).
 *
 * SVM has no Monitor Trap Flag, so the patch-0005 analogue must build single-step from
 * AMD debug facilities. This driver characterizes both candidates against analytical
 * oracles under SVM guest context, per boundary class, and feeds the ranked ruling
 * (results/ae-2/single-step-ruling.md):
 *
 *   --mode tf  : KVM_GUESTDBG_SINGLESTEP (KVM forces RFLAGS.TF) — INSTRUCTION granularity.
 *                Count KVM_EXIT_DEBUG per single-stepped instruction; oracle = instr count.
 *   --mode btf : guest DebugCtl.BTF=1 + RFLAGS.TF + KVM_GUESTDBG_ENABLE (#DB intercepted) —
 *                TAKEN-BRANCH granularity (== the V-time ex_ret_brn_tkn event). Count
 *                KVM_EXIT_DEBUG per taken branch; oracle = taken-branch count.
 *
 * Each payload's instruction and taken-branch counts are known BY CONSTRUCTION (analytical
 * oracle, evidence integrity #5). Mechanism attestation (#4): the driver asserts the guest
 * reached KVM_EXIT_HLT (ran to completion, no silent fault), records which primitive was
 * armed, and refuses to report a class it could not step exactly. A silent fall-through
 * (e.g. TF masquerading as BTF) is caught because the #DB count is compared to the
 * primitive's OWN oracle (instr vs taken), which differ.
 *
 * Reuses the minimal SVM KVM setup of kvm-guest-hammer.c. Real-mode payloads.
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
#define MEM_SIZE   0x10000
#define MSR_DEBUGCTL 0x000001d9
#define DEBUGCTL_BTF 0x2
#define RFLAGS_TF    0x100
#define RFLAGS_BASE  0x2

struct vm { int kvm, vmfd, vcpu; struct kvm_run *run; uint8_t *mem; };

static int vm_setup(struct vm *v) {
    v->kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (v->kvm < 0) { perror("open /dev/kvm"); return -1; }
    v->vmfd = ioctl(v->kvm, KVM_CREATE_VM, 0);
    if (v->vmfd < 0) { perror("KVM_CREATE_VM"); return -1; }
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

/* guest_tf: arm the guest's own RFLAGS.TF (modes tfg/btf). set_btf: also set DebugCtl.BTF
 * (mode btf). mode tf uses KVM_GUESTDBG_SINGLESTEP instead (KVM owns TF), so both 0 here. */
static int vm_set_start(struct vm *v, int guest_tf, int set_btf) {
    struct kvm_sregs s;
    if (ioctl(v->vcpu, KVM_GET_SREGS, &s) < 0) return -1;
    s.cs.base = 0; s.cs.selector = 0;
    if (ioctl(v->vcpu, KVM_SET_SREGS, &s) < 0) return -1;
    struct kvm_regs r; memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS;
    r.rflags = RFLAGS_BASE | (guest_tf ? RFLAGS_TF : 0);
    if (ioctl(v->vcpu, KVM_SET_REGS, &r) < 0) return -1;
    if (set_btf) {
        struct { struct kvm_msrs h; struct kvm_msr_entry e; } m;
        memset(&m, 0, sizeof(m));
        m.h.nmsrs = 1; m.e.index = MSR_DEBUGCTL; m.e.data = DEBUGCTL_BTF;
        if (ioctl(v->vcpu, KVM_SET_MSRS, &m) < 1) { /* attested via the run result */ }
    }
    /* read back RFLAGS.TF actually in force (attest whether KVM kept the guest's TF) */
    struct kvm_regs rb; ioctl(v->vcpu, KVM_GET_REGS, &rb);
    return (int)((rb.rflags & RFLAGS_TF) ? 1 : 0);
}

/* payload emitters — return {instr_count_before_hlt, taken_branch_count} via out params */
static int emit_nop_sled(uint8_t *mem, uint32_t k, uint64_t *instr, uint64_t *taken) {
    uint8_t *p = mem + GUEST_PHYS; uint32_t i = 0;
    for (uint32_t j = 0; j < k; j++) p[i++] = 0x90;   /* nop */
    p[i++] = 0xF4;                                     /* hlt */
    *instr = k; *taken = 0; return i;
}
static int emit_loop(uint8_t *mem, uint32_t n, uint64_t *instr, uint64_t *taken) {
    uint8_t *p = mem + GUEST_PHYS; uint32_t i = 0;
    p[i++] = 0x66; p[i++] = 0xB9;                      /* mov ecx, imm32 */
    p[i++] = n & 0xff; p[i++] = (n>>8)&0xff; p[i++] = (n>>16)&0xff; p[i++] = (n>>24)&0xff;
    p[i++] = 0x66; p[i++] = 0x49;                      /* dec ecx */
    p[i++] = 0x75; p[i++] = 0xFC;                      /* jnz -4 */
    p[i++] = 0xF4;                                     /* hlt */
    *instr = 1 + 2ull*n;                               /* mov + n*(dec+jnz) */
    *taken = (n ? n-1 : 0);                            /* jnz taken n-1 times */
    return i;
}
/* branch-dense: k unconditional short jmps (each +0, always taken) then hlt */
static int emit_jmp_chain(uint8_t *mem, uint32_t k, uint64_t *instr, uint64_t *taken) {
    uint8_t *p = mem + GUEST_PHYS; uint32_t i = 0;
    for (uint32_t j = 0; j < k; j++) { p[i++] = 0xEB; p[i++] = 0x00; }  /* jmp +0 (to next) */
    p[i++] = 0xF4;
    *instr = k; *taken = k; return i;                  /* each jmp is one instr AND one taken branch */
}
/* interrupt-shadow (STI): sti creates a one-instruction shadow. TF's #DB must still step
 * sti and the shadowed instruction as two distinct steps (the MTF-papered hazard, doc §1B).
 *   FB (sti) 90 (nop) 90 (nop) F4 (hlt)  -> instr before hlt = 3, taken = 0 */
static int emit_sti_shadow(uint8_t *mem, uint32_t k, uint64_t *instr, uint64_t *taken) {
    (void)k; uint8_t *p = mem + GUEST_PHYS; uint32_t i = 0;
    p[i++] = 0xFB; p[i++] = 0x90; p[i++] = 0x90; p[i++] = 0xF4;
    *instr = 3; *taken = 0; return i;
}
/* MOV SS shadow: mov ss,ax shadows the next instruction against #DB (doc §1B).
 *   B8 00 00 (mov ax,0) 8E D0 (mov ss,ax) 90 (nop) F4 (hlt) -> instr before hlt = 4 */
static int emit_movss_shadow(uint8_t *mem, uint32_t k, uint64_t *instr, uint64_t *taken) {
    (void)k; uint8_t *p = mem + GUEST_PHYS; uint32_t i = 0;
    p[i++] = 0xB8; p[i++] = 0x00; p[i++] = 0x00;   /* mov ax, 0   (instr 1) */
    p[i++] = 0x8E; p[i++] = 0xD0;                  /* mov ss, ax  (instr 2, shadows next) */
    p[i++] = 0x90;                                 /* nop         (instr 3, shadowed) */
    p[i++] = 0xF4;                                 /* hlt */
    /* oracle = 3 instructions before hlt; MOV SS blocks the #DB for the shadowed
     * instruction, so an exact-per-instruction TF stepper UNDER-COUNTS here (the §1B
     * hazard) — recorded, not a defect in the harness. */
    *instr = 3; *taken = 0; return i;
}

/* run under single-step, counting KVM_EXIT_DEBUG until KVM_EXIT_HLT.
 * returns #DB count, or -1 on unexpected exit; *hlt set on clean HLT. */
static int64_t run_stepped(struct vm *v, int *hlt, uint64_t max_steps) {
    int64_t dbg = 0; *hlt = 0;
    for (uint64_t s = 0; s < max_steps; s++) {
        if (ioctl(v->vcpu, KVM_RUN, 0) < 0) { if (errno == EINTR) continue; perror("KVM_RUN"); return -1; }
        switch (v->run->exit_reason) {
            case KVM_EXIT_DEBUG: dbg++; break;
            case KVM_EXIT_HLT: *hlt = 1; return dbg;
            default: fprintf(stderr, "unexpected exit %u after %lld #DB\n",
                             v->run->exit_reason, (long long)dbg); return -1;
        }
    }
    fprintf(stderr, "step budget exhausted\n"); return -1;
}

int main(int argc, char **argv) {
    const char *mode = "tf", *out = 0; int core = -1;
    uint64_t k = 16;
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i], "--mode") && i+1<argc) mode = argv[++i];
        else if (!strcmp(argv[i], "--k") && i+1<argc) k = strtoull(argv[++i], 0, 0);
        else if (!strcmp(argv[i], "--core") && i+1<argc) core = atoi(argv[++i]);
        else if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];
        else { fprintf(stderr, "usage: %s --core N [--mode tf|btf] [--k K] [--out f]\n", argv[0]); return 2; }
    }
    if (core < 0) { fprintf(stderr, "need --core N\n"); return 2; }
    cpu_set_t cs; CPU_ZERO(&cs); CPU_SET(core, &cs);
    if (sched_setaffinity(0, sizeof(cs), &cs) != 0) { perror("pin"); return 2; }
    /* mode tf : KVM_GUESTDBG_SINGLESTEP (KVM owns TF), instruction granularity
     * mode tfg: guest RFLAGS.TF + KVM_GUESTDBG_ENABLE, no BTF (does KVM keep guest TF?)
     * mode btf: guest RFLAGS.TF + DebugCtl.BTF + ENABLE, taken-branch granularity */
    int singlestep = !strcmp(mode, "tf");
    int guest_tf = !strcmp(mode, "tfg") || !strcmp(mode, "btf");
    int set_btf = !strcmp(mode, "btf");

    struct vm v; if (vm_setup(&v) != 0) return 2;
    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o, "[\n");

    struct { const char *name; int (*emit)(uint8_t*, uint32_t, uint64_t*, uint64_t*); } P[] = {
        { "nop_sled",     emit_nop_sled },
        { "loop",         emit_loop },
        { "jmp_chain",    emit_jmp_chain },
        { "sti_shadow",   emit_sti_shadow },
        { "movss_shadow", emit_movss_shadow },
    };
    int all_ok = 1;
    for (unsigned pi = 0; pi < sizeof(P)/sizeof(P[0]); pi++) {
        uint64_t instr = 0, taken = 0;
        P[pi].emit(v.mem, (uint32_t)k, &instr, &taken);
        int tf_kept = vm_set_start(&v, guest_tf, set_btf);
        if (tf_kept < 0) { fprintf(stderr, "set_start failed\n"); return 2; }
        struct kvm_guest_debug dbg; memset(&dbg, 0, sizeof(dbg));
        dbg.control = KVM_GUESTDBG_ENABLE | (singlestep ? KVM_GUESTDBG_SINGLESTEP : 0);
        if (ioctl(v.vcpu, KVM_SET_GUEST_DEBUG, &dbg) < 0) { perror("SET_GUEST_DEBUG"); return 2; }

        int hlt = 0;
        int64_t got = run_stepped(&v, &hlt, 20ull * (instr + taken + 16));
        /* oracle: TF-instruction modes vs BTF-taken-branch mode */
        uint64_t oracle = set_btf ? taken : instr;
        int exact = (got >= 0) && hlt && ((uint64_t)got == oracle);
        if (!exact) all_ok = 0;
        fprintf(o, "  {\"kind\":\"singlestep\",\"mode\":\"%s\",\"payload\":\"%s\",\"k\":%llu,"
                   "\"db_exits\":%lld,\"oracle_instr\":%llu,\"oracle_taken\":%llu,"
                   "\"oracle_used\":%llu,\"guest_tf_kept\":%d,\"hlt_ok\":%d,\"exact\":%d},\n",
                mode, P[pi].name, (unsigned long long)k, (long long)got,
                (unsigned long long)instr, (unsigned long long)taken,
                (unsigned long long)oracle, tf_kept, hlt, exact);
    }
    fprintf(o, "  {\"kind\":\"end\",\"mode\":\"%s\",\"rc\":%d}\n]\n", mode, all_ok ? 0 : 1);
    if (out) fclose(o);
    return all_ok ? 0 : 1;
}
