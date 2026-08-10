/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae4-freeze.c — AE-4 AuthenticAMD contract freeze demo (docs/AMD-EPYC.md AE-4(a)).
 *
 * Demonstrates the headline AE-4 claim on the STOCK kernel (CPUID freeze is stock KVM
 * functionality — not blocked by AE-3's build): a guest sees a FROZEN CPUID model,
 * including a feature bit cleared BELOW host capability, and the frozen AuthenticAMD
 * vendor string. Method:
 *   1. KVM_GET_SUPPORTED_CPUID from /dev/kvm (the host-capable model).
 *   2. Pick a feature bit SET in leaf 1 EDX on this host, CLEAR it in the frozen model
 *      (a below-host freeze — the exact thing the doc requires demonstrated).
 *   3. KVM_SET_CPUID2 the frozen model on the vCPU.
 *   4. Run a real-mode probe guest that executes CPUID leaf 1 + leaf 0 and stores the
 *      results to guest memory, then HLT.
 *   5. Read guest memory: assert the guest saw the CLEARED bit (frozen below host) and
 *      the frozen vendor string — proving the VMCB CPUID intercept enforces the model.
 *
 * Evidence integrity: the guest is attested to reach KVM_EXIT_HLT (#4); the below-host
 * bit is chosen from the host's own reported CPUID at runtime, so the oracle is the
 * host's real capability (#5, not a hardcoded assumption); the result is emitted as
 * stable JSON for the floor record.
 */
#define _GNU_SOURCE
#include <cpuid.h>
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
#define STORE_GPA  0x2000
#define MEM_SIZE   0x10000

/* real-mode probe: CPUID leaf1 -> [0x2000]=ecx,[+4]=edx; leaf0 -> [+8]=ebx,[+12]=ecx,[+16]=edx; hlt */
static void emit_cpuid_probe(uint8_t *mem) {
    uint8_t code[] = {
        0x66,0xB8,0x01,0x00,0x00,0x00,  /* mov eax,1 */
        0x0F,0xA2,                      /* cpuid */
        0xBF,0x00,0x20,                 /* mov di,0x2000 */
        0x66,0x89,0x0D,                 /* mov [di],ecx */
        0x66,0x89,0x55,0x04,            /* mov [di+4],edx */
        0x66,0x31,0xC0,                 /* xor eax,eax */
        0x0F,0xA2,                      /* cpuid */
        0x66,0x89,0x5D,0x08,            /* mov [di+8],ebx */
        0x66,0x89,0x4D,0x0C,            /* mov [di+12],ecx */
        0x66,0x89,0x55,0x10,            /* mov [di+16],edx */
        0xF4,                           /* hlt */
    };
    memcpy(mem + GUEST_PHYS, code, sizeof(code));
}

int main(int argc, char **argv) {
    const char *out = 0;
    for (int i = 1; i < argc; i++)
        if (!strcmp(argv[i], "--out") && i+1<argc) out = argv[++i];

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }

    /* 1. host-capable CPUID model */
    size_t cap = 256;
    struct kvm_cpuid2 *cp = calloc(1, sizeof(*cp) + cap*sizeof(struct kvm_cpuid_entry2));
    cp->nent = cap;
    if (ioctl(kvm, KVM_GET_SUPPORTED_CPUID, cp) < 0) { perror("GET_SUPPORTED_CPUID"); return 2; }

    /* 2. clear a set leaf-1 EDX bit -> below-host freeze; record host EDX + the bit */
    uint32_t host_leaf1_edx = 0; int cleared_bit = -1;
    for (uint32_t i = 0; i < cp->nent; i++) {
        if (cp->entries[i].function == 1 && cp->entries[i].index == 0) {
            host_leaf1_edx = cp->entries[i].edx;
            /* pick bit 4 (TSC) if set, else the lowest set bit >=4 */
            for (int b = 4; b < 32; b++) if (host_leaf1_edx & (1u<<b)) { cleared_bit = b; break; }
            if (cleared_bit >= 0) cp->entries[i].edx &= ~(1u << cleared_bit);
        }
    }
    if (cleared_bit < 0) { fprintf(stderr, "no set leaf1 EDX bit to clear\n"); return 2; }

    /* 3. build VM + set the FROZEN model on the vCPU */
    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot=0, .guest_phys_addr=0, .memory_size=MEM_SIZE, .userspace_addr=(uint64_t)mem };
    if (ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) { perror("SET_MEM"); return 2; }
    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    if (ioctl(vcpu, KVM_SET_CPUID2, cp) < 0) { perror("SET_CPUID2"); return 2; }
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ|PROT_WRITE, MAP_SHARED, vcpu, 0);

    /* 4. run the probe guest */
    emit_cpuid_probe(mem);
    struct kvm_sregs s; ioctl(vcpu, KVM_GET_SREGS, &s); s.cs.base=0; s.cs.selector=0;
    ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r; memset(&r,0,sizeof(r)); r.rip=GUEST_PHYS; r.rflags=0x2;
    ioctl(vcpu, KVM_SET_REGS, &r);
    int hlt = 0;
    for (;;) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; perror("KVM_RUN"); return 2; }
        if (run->exit_reason == KVM_EXIT_HLT) { hlt = 1; break; }
        fprintf(stderr, "unexpected exit %u\n", run->exit_reason); return 2;
    }

    /* 5. read what the guest saw */
    uint32_t g_l1_ecx, g_l1_edx, g_l0_ebx, g_l0_ecx, g_l0_edx;
    memcpy(&g_l1_ecx, mem+STORE_GPA,    4);
    memcpy(&g_l1_edx, mem+STORE_GPA+4,  4);
    memcpy(&g_l0_ebx, mem+STORE_GPA+8,  4);
    memcpy(&g_l0_ecx, mem+STORE_GPA+12, 4);
    memcpy(&g_l0_edx, mem+STORE_GPA+16, 4);
    char vendor[13]; memcpy(vendor,&g_l0_ebx,4); memcpy(vendor+4,&g_l0_edx,4); memcpy(vendor+8,&g_l0_ecx,4); vendor[12]=0;

    int guest_sees_bit = (g_l1_edx >> cleared_bit) & 1;
    int host_has_bit = (host_leaf1_edx >> cleared_bit) & 1;
    int below_host_freeze = host_has_bit && !guest_sees_bit;   /* the headline demonstration */
    int vendor_frozen = (strcmp(vendor, "AuthenticAMD") == 0);

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o,
      "{\"schema\":\"amd-epyc-ae4-freeze-v1\",\"hlt_ok\":%d,\"guest_vendor\":\"%s\","
      "\"vendor_frozen\":%d,\"cleared_edx_bit\":%d,\"host_has_bit\":%d,"
      "\"guest_sees_bit\":%d,\"below_host_freeze_demonstrated\":%d,"
      "\"host_leaf1_edx\":\"0x%08x\",\"guest_leaf1_edx\":\"0x%08x\"}\n",
      hlt, vendor, vendor_frozen, cleared_bit, host_has_bit, guest_sees_bit,
      below_host_freeze, host_leaf1_edx, g_l1_edx);
    if (out) fclose(o);
    return (hlt && vendor_frozen && below_host_freeze) ? 0 : 1;
}
