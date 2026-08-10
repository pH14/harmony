/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae4-msr.c — AE-4 MSR default-deny enforcement demo (docs/AMD-EPYC.md AE-4(b)).
 *
 * Demonstrates on the STOCK kernel that a guest read of a denied AMD MSR is TRAPPED and
 * reaches the vmm — the MSR-permission-bitmap enforcement the frozen contract relies on.
 * Method: enable KVM_CAP_X86_USER_SPACE_MSR (report filtered MSRs to userspace), install a
 * KVM_X86_SET_MSR_FILTER that denies reads of a chosen AMD MSR, then run a real-mode guest
 * that RDMSRs it. A correctly-enforced deny surfaces as KVM_EXIT_X86_RDMSR (the trap
 * reached the vmm) instead of the guest reading the value — proving the enforcement path.
 *
 * The control MSR is HWCR (0xC0010015), which docs/cpu-msr-contract-amd-draft.toml lists
 * deny-gp; the demo is agnostic to the value (it proves the TRAP, not a frozen value).
 * Evidence integrity #4: the exit reason (KVM_EXIT_X86_RDMSR vs a silent guest read) is the
 * attestation — a guest that silently read the MSR would NOT produce this exit.
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
#define MEM_SIZE   0x10000
#define TARGET_MSR 0xC0010015u   /* HWCR — deny-gp in the AMD contract draft */

int main(int argc, char **argv) {
    const char *out = 0;
    for (int i = 1; i < argc; i++)
        if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }
    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);

    /* enable reporting filtered MSRs to userspace */
    struct kvm_enable_cap cap = { .cap = KVM_CAP_X86_USER_SPACE_MSR };
    cap.args[0] = KVM_MSR_EXIT_REASON_FILTER;
    int usmsr_ok = (ioctl(vmfd, KVM_ENABLE_CAP, &cap) == 0);

    /* deny reads of TARGET_MSR: default-allow, one READ range with the target's bit clear */
    uint8_t bitmap = 0x00;   /* bit 0 (the single MSR in this range) = 0 => deny read */
    struct kvm_msr_filter filter;
    memset(&filter, 0, sizeof(filter));
    filter.flags = KVM_MSR_FILTER_DEFAULT_ALLOW;
    filter.ranges[0].flags = KVM_MSR_FILTER_READ;
    filter.ranges[0].nmsrs = 1;
    filter.ranges[0].base = TARGET_MSR;
    filter.ranges[0].bitmap = &bitmap;
    int filter_ok = (ioctl(vmfd, KVM_X86_SET_MSR_FILTER, &filter) == 0);

    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot=0, .guest_phys_addr=0, .memory_size=MEM_SIZE, .userspace_addr=(uint64_t)mem };
    ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region);
    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ|PROT_WRITE, MAP_SHARED, vcpu, 0);

    /* guest: mov ecx, TARGET_MSR ; rdmsr ; hlt */
    uint8_t code[] = { 0x66,0xB9, TARGET_MSR&0xff,(TARGET_MSR>>8)&0xff,(TARGET_MSR>>16)&0xff,(TARGET_MSR>>24)&0xff,
                       0x0F,0x32, 0xF4 };
    memcpy(mem+GUEST_PHYS, code, sizeof(code));
    struct kvm_sregs s; ioctl(vcpu, KVM_GET_SREGS, &s); s.cs.base=0; s.cs.selector=0; ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r; memset(&r,0,sizeof(r)); r.rip=GUEST_PHYS; r.rflags=0x2; ioctl(vcpu, KVM_SET_REGS, &r);

    int rdmsr_trapped = 0, hlt = 0, shutdown = 0; unsigned other = 0;
    for (int steps = 0; steps < 16; steps++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; perror("KVM_RUN"); break; }
        if (run->exit_reason == KVM_EXIT_X86_RDMSR) {
            rdmsr_trapped = 1;
            /* the trap reached the vmm; complete it with an injected #GP (deny-gp) so the
             * guest cannot proceed to read a value — then it triple-faults (no IDT) => shutdown */
            run->msr.error = 1;   /* signal #GP for this MSR access */
            continue;
        } else if (run->exit_reason == KVM_EXIT_HLT) { hlt = 1; break; }
        else if (run->exit_reason == KVM_EXIT_SHUTDOWN) { shutdown = 1; break; }
        else { other = run->exit_reason; break; }
    }

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o,
      "{\"schema\":\"amd-epyc-ae4-msr-v1\",\"target_msr\":\"0x%08x\","
      "\"user_space_msr_cap\":%d,\"filter_installed\":%d,"
      "\"rdmsr_trapped_to_vmm\":%d,\"deny_gp_then_shutdown\":%d,\"guest_hlt\":%d,"
      "\"other_exit\":%u}\n",
      TARGET_MSR, usmsr_ok, filter_ok, rdmsr_trapped, shutdown, hlt, other);
    if (out) fclose(o);
    /* success = the denied MSR read TRAPPED to the vmm (the enforcement path fired) */
    return (usmsr_ok && filter_ok && rdmsr_trapped) ? 0 : 1;
}
