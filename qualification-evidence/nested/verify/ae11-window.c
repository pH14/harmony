/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae11-window.c - parameterized guest-window branch-count measurement.
 *
 * Same arrangement as ae10-guestwindow.c: a counter with exclude_host=1 is
 * opened by the measuring process and read around a KVM guest whose retired
 * conditional branches are known by analysis.  Knobs:
 *
 *   iters=N   total conditional branches the guest retires (0 = payload is
 *             just hlt, zero branches)
 *   exits=K   number of OUT instructions inserted between branch blocks;
 *             each causes one KVM_EXIT_IO round trip to the measuring process
 *   pre=M     conditional branches the measuring process runs in user space
 *             after PERF_EVENT_IOC_ENABLE, before the first KVM_RUN
 *   mid=M     conditional branches the measuring process runs in user space
 *             at every IO exit, before re-entering the guest
 *   post=M    conditional branches the measuring process runs in user space
 *             after the guest halts, before PERF_EVENT_IOC_DISABLE
 *   reps=R    repetitions (fresh VM each repetition, same counter fd)
 *   dual=1    also open an exclude_guest=1 counter and an unrestricted
 *             counter on the same event, enabled/disabled around the window
 *
 * Output: one JSON object; per-repetition guest-only count (g), and with
 * dual=1 also host-only (h) and unrestricted (a), plus IO exits seen and
 * whether the guest reached hlt.
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
#define MAX_REPS    200

static int perf_open_flags(int exclude_host, int exclude_guest)
{
    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.type = PERF_TYPE_RAW;
    a.size = sizeof(a);
    a.config = RAW_BR_COND;
    a.disabled = 1;
    a.pinned = 1;
    a.exclude_host = exclude_host;
    a.exclude_guest = exclude_guest;
    return (int)syscall(__NR_perf_event_open, &a, 0, -1, -1, 0);
}

/* Retires exactly `n` conditional branches in the measuring process. */
static void branch_loop(unsigned long n)
{
    if (!n) return;
    asm volatile("1:\n\t dec %0\n\t jnz 1b" : "+r"(n) :: "cc");
}

/* 16-bit real mode.  Each block: mov ecx,imm32; dec ecx; jnz -4 -> retires
 * exactly n conditional branches.  Between blocks: out 0x01,al -> one
 * KVM_EXIT_IO.  Terminates with hlt. */
static size_t emit_payload(uint8_t *code, uint64_t iters, unsigned exits)
{
    size_t off = 0;
    unsigned blocks = exits + 1;
    uint64_t base = iters / blocks, rem = iters % blocks;
    for (unsigned b = 0; b < blocks; b++) {
        uint64_t n = base + (b < rem ? 1 : 0);
        if (n > 0) {
            uint32_t imm = (uint32_t)n;
            code[off++] = 0x66; code[off++] = 0xB9;      /* mov ecx, imm32 */
            memcpy(code + off, &imm, 4); off += 4;
            code[off++] = 0x66; code[off++] = 0x49;      /* dec ecx */
            code[off++] = 0x75; code[off++] = 0xFC;      /* jnz -4 */
        }
        if (b != blocks - 1) { code[off++] = 0xE6; code[off++] = 0x01; }
    }
    code[off++] = 0xF4;                                  /* hlt */
    return off;
}

struct repres {
    long long g, h, a;
    long io;
    int hlt;
};

static int window(int kvm, int gfd, int hfd, int afd,
                  uint64_t iters, unsigned exits,
                  unsigned long pre, unsigned long mid, unsigned long post,
                  struct repres *out)
{
    out->g = out->h = out->a = -1;
    out->io = 0; out->hlt = 0;

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

    size_t len = emit_payload(mem + GUEST_PHYS, iters, exits);
    if (GUEST_PHYS + len >= MEM_SIZE) { fprintf(stderr, "payload too big\n"); exit(2); }

    struct kvm_sregs s;
    ioctl(vcpu, KVM_GET_SREGS, &s);
    s.cs.base = 0; s.cs.selector = 0;
    ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r;
    memset(&r, 0, sizeof(r));
    r.rip = GUEST_PHYS; r.rflags = 0x2;
    ioctl(vcpu, KVM_SET_REGS, &r);

    if (gfd >= 0) ioctl(gfd, PERF_EVENT_IOC_RESET, 0);
    if (hfd >= 0) ioctl(hfd, PERF_EVENT_IOC_RESET, 0);
    if (afd >= 0) ioctl(afd, PERF_EVENT_IOC_RESET, 0);
    if (gfd >= 0) ioctl(gfd, PERF_EVENT_IOC_ENABLE, 0);
    if (hfd >= 0) ioctl(hfd, PERF_EVENT_IOC_ENABLE, 0);
    if (afd >= 0) ioctl(afd, PERF_EVENT_IOC_ENABLE, 0);

    branch_loop(pre);
    for (long guard = 0; guard < 1000000; guard++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) {
            if (errno == EINTR) continue;
            break;
        }
        if (run->exit_reason == KVM_EXIT_IO) {
            out->io++;
            branch_loop(mid);
            continue;
        }
        if (run->exit_reason == KVM_EXIT_HLT) { out->hlt = 1; break; }
        break;
    }
    branch_loop(post);

    if (gfd >= 0) ioctl(gfd, PERF_EVENT_IOC_DISABLE, 0);
    if (hfd >= 0) ioctl(hfd, PERF_EVENT_IOC_DISABLE, 0);
    if (afd >= 0) ioctl(afd, PERF_EVENT_IOC_DISABLE, 0);

    if (gfd >= 0 && read(gfd, &out->g, sizeof(out->g)) != sizeof(out->g)) out->g = -1;
    if (hfd >= 0 && read(hfd, &out->h, sizeof(out->h)) != sizeof(out->h)) out->h = -1;
    if (afd >= 0 && read(afd, &out->a, sizeof(out->a)) != sizeof(out->a)) out->a = -1;

    munmap(run, msize); munmap(mem, MEM_SIZE);
    close(vcpu); close(vmfd);
    return 0;
}

int main(int argc, char **argv)
{
    uint64_t iters = 10000;
    unsigned exits = 0;
    unsigned long pre = 0, mid = 0, post = 0;
    int reps = 20, dual = 0;
    for (int i = 1; i < argc; i++) {
        if (!strncmp(argv[i], "iters=", 6)) iters = strtoull(argv[i] + 6, 0, 0);
        else if (!strncmp(argv[i], "exits=", 6)) exits = (unsigned)strtoul(argv[i] + 6, 0, 0);
        else if (!strncmp(argv[i], "pre=", 4)) pre = strtoul(argv[i] + 4, 0, 0);
        else if (!strncmp(argv[i], "mid=", 4)) mid = strtoul(argv[i] + 4, 0, 0);
        else if (!strncmp(argv[i], "post=", 5)) post = strtoul(argv[i] + 5, 0, 0);
        else if (!strncmp(argv[i], "reps=", 5)) reps = atoi(argv[i] + 5);
        else if (!strncmp(argv[i], "dual=", 5)) dual = atoi(argv[i] + 5);
        else { fprintf(stderr, "bad arg %s\n", argv[i]); return 2; }
    }
    if (reps > MAX_REPS) reps = MAX_REPS;

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }
    int gfd = perf_open_flags(1, 0);
    if (gfd < 0) { perror("perf_event_open guest-only"); return 2; }
    int hfd = -1, afd = -1;
    if (dual) {
        hfd = perf_open_flags(0, 1);
        afd = perf_open_flags(0, 0);
        if (hfd < 0 || afd < 0) { perror("perf_event_open dual"); return 2; }
    }

    static struct repres rr[MAX_REPS];
    for (int i = 0; i < reps; i++)
        window(kvm, gfd, hfd, afd, iters, exits, pre, mid, post, &rr[i]);

    printf("{ \"iters\": %llu, \"exits\": %u, \"pre\": %lu, \"mid\": %lu, \"post\": %lu,"
           " \"reps\": %d, \"dual\": %d,\n  \"g\": [",
           (unsigned long long)iters, exits, pre, mid, post, reps, dual);
    for (int i = 0; i < reps; i++) printf("%s%lld", i ? "," : "", rr[i].g);
    printf("],\n");
    if (dual) {
        printf("  \"h\": [");
        for (int i = 0; i < reps; i++) printf("%s%lld", i ? "," : "", rr[i].h);
        printf("],\n  \"a\": [");
        for (int i = 0; i < reps; i++) printf("%s%lld", i ? "," : "", rr[i].a);
        printf("],\n");
    }
    printf("  \"io\": [");
    for (int i = 0; i < reps; i++) printf("%s%ld", i ? "," : "", rr[i].io);
    printf("],\n  \"hlt\": [");
    for (int i = 0; i < reps; i++) printf("%s%d", i ? "," : "", rr[i].hlt);
    printf("] }\n");
    return 0;
}
