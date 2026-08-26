/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae12-attrib.c - attribute the guest-only counter's events by sampled IP.
 *
 * Opens the same exclude_host=1 retired-conditional-branch counter as
 * ae10/ae11, but in sampling mode with PERF_SAMPLE_IP.  Runs one guest
 * window (same payload emitter as ae11) and dumps every sample record's
 * cpumode and instruction pointer.  The guest payload lives at physical
 * 0x1000-0x10000, so a sample either falls in the guest (cpumode
 * GUEST_KERNEL, ip < 0x10000) or somewhere in the measuring kernel or
 * process, which is the attribution the surplus question needs.
 *
 *   iters=N period=P exits=K mid=M reps=R
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
#define RING_PAGES  256   /* data pages; plus one metadata page */

static void branch_loop(unsigned long n)
{
    if (!n) return;
    asm volatile("1:\n\t dec %0\n\t jnz 1b" : "+r"(n) :: "cc");
}

static size_t emit_payload(uint8_t *code, uint64_t iters, unsigned exits)
{
    size_t off = 0;
    unsigned blocks = exits + 1;
    uint64_t base = iters / blocks, rem = iters % blocks;
    for (unsigned b = 0; b < blocks; b++) {
        uint64_t n = base + (b < rem ? 1 : 0);
        if (n > 0) {
            uint32_t imm = (uint32_t)n;
            code[off++] = 0x66; code[off++] = 0xB9;
            memcpy(code + off, &imm, 4); off += 4;
            code[off++] = 0x66; code[off++] = 0x49;
            code[off++] = 0x75; code[off++] = 0xFC;
        }
        if (b != blocks - 1) { code[off++] = 0xE6; code[off++] = 0x01; }
    }
    code[off++] = 0xF4;
    return off;
}

int main(int argc, char **argv)
{
    uint64_t iters = 200000, period = 500;
    unsigned exits = 0;
    unsigned long mid = 0;
    int reps = 1;
    for (int i = 1; i < argc; i++) {
        if (!strncmp(argv[i], "iters=", 6)) iters = strtoull(argv[i] + 6, 0, 0);
        else if (!strncmp(argv[i], "period=", 7)) period = strtoull(argv[i] + 7, 0, 0);
        else if (!strncmp(argv[i], "exits=", 6)) exits = (unsigned)strtoul(argv[i] + 6, 0, 0);
        else if (!strncmp(argv[i], "mid=", 4)) mid = strtoul(argv[i] + 4, 0, 0);
        else if (!strncmp(argv[i], "reps=", 5)) reps = atoi(argv[i] + 5);
        else { fprintf(stderr, "bad arg %s\n", argv[i]); return 2; }
    }

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }

    struct perf_event_attr a;
    memset(&a, 0, sizeof(a));
    a.type = PERF_TYPE_RAW;
    a.size = sizeof(a);
    a.config = RAW_BR_COND;
    a.disabled = 1;
    a.pinned = 1;
    a.exclude_host = 1;
    a.sample_period = period;
    a.sample_type = PERF_SAMPLE_IP;
    int pf = (int)syscall(__NR_perf_event_open, &a, 0, -1, -1, 0);
    if (pf < 0) { perror("perf_event_open"); return 2; }

    long pg = sysconf(_SC_PAGESIZE);
    size_t maplen = (size_t)pg * (RING_PAGES + 1);
    struct perf_event_mmap_page *mp =
        mmap(NULL, maplen, PROT_READ | PROT_WRITE, MAP_SHARED, pf, 0);
    if (mp == MAP_FAILED) { perror("mmap ring"); return 2; }

    for (int rep = 0; rep < reps; rep++) {
        int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
        uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ | PROT_WRITE,
                            MAP_SHARED | MAP_ANONYMOUS, -1, 0);
        struct kvm_userspace_memory_region region = {
            .slot = 0, .guest_phys_addr = 0,
            .memory_size = MEM_SIZE, .userspace_addr = (uint64_t)mem };
        ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region);
        int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
        int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
        struct kvm_run *run = mmap(0, msize, PROT_READ | PROT_WRITE, MAP_SHARED, vcpu, 0);

        emit_payload(mem + GUEST_PHYS, iters, exits);
        struct kvm_sregs s;
        ioctl(vcpu, KVM_GET_SREGS, &s);
        s.cs.base = 0; s.cs.selector = 0;
        ioctl(vcpu, KVM_SET_SREGS, &s);
        struct kvm_regs r;
        memset(&r, 0, sizeof(r));
        r.rip = GUEST_PHYS; r.rflags = 0x2;
        ioctl(vcpu, KVM_SET_REGS, &r);

        /* drain any leftover records, reset tail to head */
        mp->data_tail = mp->data_head;

        ioctl(pf, PERF_EVENT_IOC_RESET, 0);
        ioctl(pf, PERF_EVENT_IOC_ENABLE, 0);
        long io = 0; int hlt = 0;
        for (long guard = 0; guard < 1000000; guard++) {
            if (ioctl(vcpu, KVM_RUN, 0) < 0) {
                if (errno == EINTR) continue;
                break;
            }
            if (run->exit_reason == KVM_EXIT_IO) { io++; branch_loop(mid); continue; }
            if (run->exit_reason == KVM_EXIT_HLT) { hlt = 1; break; }
            break;
        }
        ioctl(pf, PERF_EVENT_IOC_DISABLE, 0);
        long long count = -1;
        if (read(pf, &count, sizeof(count)) != sizeof(count)) count = -1;

        uint8_t *base = (uint8_t *)mp + pg;
        uint64_t head = mp->data_head;
        __sync_synchronize();
        uint64_t tail = mp->data_tail;
        uint64_t size = (uint64_t)pg * RING_PAGES;
        long nsample = 0, nlost = 0, nother = 0;
        while (tail < head) {
            struct perf_event_header *h =
                (struct perf_event_header *)(base + (tail % size));
            if (h->size == 0) break;
            if (h->type == PERF_RECORD_SAMPLE) {
                uint64_t ip = *(uint64_t *)(base + ((tail + sizeof(*h)) % size));
                printf("S rep=%d misc=0x%x cpumode=%u ip=0x%llx\n",
                       rep, h->misc, h->misc & PERF_RECORD_MISC_CPUMODE_MASK,
                       (unsigned long long)ip);
                nsample++;
            } else if (h->type == PERF_RECORD_LOST) {
                nlost++;
            } else {
                nother++;
            }
            tail += h->size;
        }
        mp->data_tail = tail;
        printf("R rep=%d iters=%llu period=%llu exits_seen=%ld hlt=%d count=%lld"
               " samples=%ld lost=%ld other=%ld\n",
               rep, (unsigned long long)iters, (unsigned long long)period,
               io, hlt, count, nsample, nlost, nother);

        munmap(run, msize); munmap(mem, MEM_SIZE);
        close(vcpu); close(vmfd);
    }
    return 0;
}
