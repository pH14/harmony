/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae8-guestpmu.c - can a guest on this chip count retired conditional branches
 * exactly?
 *
 * This is the first question nested operation turns on.  Running the determinism
 * machinery inside a VM means the work clock is read by a guest, through the
 * hypervisor's emulated performance counters, rather than by the host from real
 * hardware.  If that count is not exact the whole arrangement is dead, and no
 * amount of later work recovers it.
 *
 * The guest programs AMD core counter 0 for event 0xd1 (retired conditional
 * branches, the event rr uses on Zen), runs a loop whose branch count is known by
 * analysis, and reads the counter back.  The oracle is the analysis.
 *
 * Absolute counts carry a fixed offset for the instructions between enabling the
 * counter and reading it, so the test is on differences: doubling the loop count
 * must move the counter by exactly the number of branches added.  A constant
 * offset cancels; a proportional error does not.
 */
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <linux/kvm.h>

#define GUEST_PHYS 0x1000
#define UD_HANDLER 0x1500
#define STORE_GPA  0x2000
#define MEM_SIZE   0x10000

#define PERF_CTL0  0xC0010200u
#define PERF_CTR0  0xC0010201u
/* event 0xd1 (ex_ret_cond), umask 0, count in both user and supervisor, enabled */
#define CTL_VALUE  0x004300D1u

/* Build the payload for `iters` loop iterations.  Returns the byte length. */
static size_t emit(uint8_t *mem, uint16_t iters)
{
    uint8_t c[128];
    size_t n = 0;
    /* stop and zero counter 0 */
    c[n++] = 0x66; c[n++] = 0xB9;                                   /* mov ecx, */
    c[n++] = PERF_CTL0 & 0xff; c[n++] = (PERF_CTL0 >> 8) & 0xff;
    c[n++] = (PERF_CTL0 >> 16) & 0xff; c[n++] = (PERF_CTL0 >> 24) & 0xff;
    c[n++] = 0x66; c[n++] = 0x31; c[n++] = 0xC0;                    /* xor eax,eax */
    c[n++] = 0x66; c[n++] = 0x31; c[n++] = 0xD2;                    /* xor edx,edx */
    c[n++] = 0x0F; c[n++] = 0x30;                                   /* wrmsr */

    c[n++] = 0x66; c[n++] = 0xB9;                                   /* mov ecx, */
    c[n++] = PERF_CTR0 & 0xff; c[n++] = (PERF_CTR0 >> 8) & 0xff;
    c[n++] = (PERF_CTR0 >> 16) & 0xff; c[n++] = (PERF_CTR0 >> 24) & 0xff;
    c[n++] = 0x66; c[n++] = 0x31; c[n++] = 0xC0;                    /* xor eax,eax */
    c[n++] = 0x66; c[n++] = 0x31; c[n++] = 0xD2;                    /* xor edx,edx */
    c[n++] = 0x0F; c[n++] = 0x30;                                   /* wrmsr */

    /* enable */
    c[n++] = 0x66; c[n++] = 0xB9;                                   /* mov ecx, */
    c[n++] = PERF_CTL0 & 0xff; c[n++] = (PERF_CTL0 >> 8) & 0xff;
    c[n++] = (PERF_CTL0 >> 16) & 0xff; c[n++] = (PERF_CTL0 >> 24) & 0xff;
    c[n++] = 0x66; c[n++] = 0xB8;                                   /* mov eax, */
    c[n++] = CTL_VALUE & 0xff; c[n++] = (CTL_VALUE >> 8) & 0xff;
    c[n++] = (CTL_VALUE >> 16) & 0xff; c[n++] = (CTL_VALUE >> 24) & 0xff;
    c[n++] = 0x66; c[n++] = 0x31; c[n++] = 0xD2;                    /* xor edx,edx */
    c[n++] = 0x0F; c[n++] = 0x30;                                   /* wrmsr */

    /* the counted loop: one retired conditional branch per iteration */
    c[n++] = 0xBE; c[n++] = iters & 0xff; c[n++] = (iters >> 8) & 0xff; /* mov si,n */
    c[n++] = 0x4E;                                                  /* dec si     */
    c[n++] = 0x75; c[n++] = 0xFD;                                   /* jnz -3     */

    /* read counter 0 */
    c[n++] = 0x66; c[n++] = 0xB9;                                   /* mov ecx, */
    c[n++] = PERF_CTR0 & 0xff; c[n++] = (PERF_CTR0 >> 8) & 0xff;
    c[n++] = (PERF_CTR0 >> 16) & 0xff; c[n++] = (PERF_CTR0 >> 24) & 0xff;
    c[n++] = 0x0F; c[n++] = 0x32;                                   /* rdmsr */
    c[n++] = 0xBF; c[n++] = 0x00; c[n++] = 0x20;                    /* mov di,0x2000 */
    c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x05;                    /* mov [di],eax  */
    c[n++] = 0x66; c[n++] = 0x89; c[n++] = 0x55; c[n++] = 0x04;     /* mov [di+4],edx */
    c[n++] = 0xF4;                                                  /* hlt */

    memcpy(mem + GUEST_PHYS, c, n);

    /* #UD lands here if the hypervisor refuses the counter MSRs */
    uint8_t ud[] = {
        0xBF, 0x00, 0x20,                    /* mov di,0x2000           */
        0xC7, 0x45, 0x08, 0xAD, 0xDE,        /* mov word [di+8],0xDEAD  */
        0xF4,                                /* hlt                     */
    };
    memcpy(mem + UD_HANDLER, ud, sizeof(ud));
    uint16_t off = UD_HANDLER, seg = 0;
    memcpy(mem + 6 * 4,     &off, 2);
    memcpy(mem + 6 * 4 + 2, &seg, 2);
    /* #GP (vector 13) too: a denied MSR write faults rather than hanging */
    memcpy(mem + 13 * 4,     &off, 2);
    memcpy(mem + 13 * 4 + 2, &seg, 2);
    return n;
}

struct run_out { uint64_t count; uint16_t fault; int hlt; int other_exit; };

static int one_run(int kvm, struct kvm_cpuid2 *cp, uint16_t iters, struct run_out *o)
{
    memset(o, 0, sizeof(*o));
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
    if (cp && ioctl(vcpu, KVM_SET_CPUID2, cp) < 0) return -1;
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

    for (int guard = 0; guard < 4096; guard++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) {
            if (errno == EINTR) continue;
            return -1;
        }
        if (run->exit_reason == KVM_EXIT_HLT) { o->hlt = 1; break; }
        o->other_exit = run->exit_reason;
        break;
    }
    uint32_t lo, hi;
    memcpy(&lo, mem + STORE_GPA, 4);
    memcpy(&hi, mem + STORE_GPA + 4, 4);
    memcpy(&o->fault, mem + STORE_GPA + 8, 2);
    o->count = ((uint64_t)hi << 32) | lo;

    munmap(run, msize); munmap(mem, MEM_SIZE);
    close(vcpu); close(vmfd);
    return 0;
}

int main(int argc, char **argv)
{
    const char *out_path = 0;
    for (int i = 1; i < argc; i++)
        if (!strcmp(argv[i], "--out") && i + 1 < argc) out_path = argv[++i];

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }

    size_t cap = 256;
    struct kvm_cpuid2 *cp = calloc(1, sizeof(*cp) + cap * sizeof(struct kvm_cpuid_entry2));
    cp->nent = cap;
    if (ioctl(kvm, KVM_GET_SUPPORTED_CPUID, cp) < 0) { free(cp); cp = 0; }

    const uint16_t iters[4] = { 10000, 20000, 30000, 40000 };
    struct run_out o[4];
    for (int i = 0; i < 4; i++) {
        if (one_run(kvm, cp, iters[i], &o[i]) < 0) {
            fprintf(stderr, "run %d failed: %s\n", i, strerror(errno));
            return 2;
        }
    }

    /* Each extra 10000 iterations retires exactly 10000 more conditional
     * branches.  A fixed offset in the absolute count cancels in the delta. */
    int deltas_exact = 1;
    long long d[3];
    for (int i = 0; i < 3; i++) {
        d[i] = (long long)o[i + 1].count - (long long)o[i].count;
        if (d[i] != 10000) deltas_exact = 0;
    }
    int no_fault = 1, all_hlt = 1;
    for (int i = 0; i < 4; i++) {
        if (o[i].fault) no_fault = 0;
        if (!o[i].hlt) all_hlt = 0;
    }
    int counted = (o[0].count > 0);
    int pass = deltas_exact && no_fault && all_hlt && counted;

    char buf[1400];
    int n = snprintf(buf, sizeof(buf),
        "{\n"
        "  \"counter_msrs_accepted\": %d,\n"
        "  \"counter_moved\": %d,\n"
        "  \"runs\": [\n"
        "    { \"iterations\": %u, \"count\": %llu, \"hlt\": %d, \"other_exit\": %d },\n"
        "    { \"iterations\": %u, \"count\": %llu, \"hlt\": %d, \"other_exit\": %d },\n"
        "    { \"iterations\": %u, \"count\": %llu, \"hlt\": %d, \"other_exit\": %d },\n"
        "    { \"iterations\": %u, \"count\": %llu, \"hlt\": %d, \"other_exit\": %d }\n"
        "  ],\n"
        "  \"deltas\": [ %lld, %lld, %lld ],\n"
        "  \"expected_delta\": 10000,\n"
        "  \"deltas_exact\": %d,\n"
        "  \"pass\": %d\n"
        "}\n",
        no_fault, counted,
        iters[0], (unsigned long long)o[0].count, o[0].hlt, o[0].other_exit,
        iters[1], (unsigned long long)o[1].count, o[1].hlt, o[1].other_exit,
        iters[2], (unsigned long long)o[2].count, o[2].hlt, o[2].other_exit,
        iters[3], (unsigned long long)o[3].count, o[3].hlt, o[3].other_exit,
        d[0], d[1], d[2], deltas_exact, pass);

    fwrite(buf, 1, n, stdout);
    if (out_path) { FILE *f = fopen(out_path, "w"); if (f) { fwrite(buf, 1, n, f); fclose(f); } }
    return pass ? 0 : 1;
}
