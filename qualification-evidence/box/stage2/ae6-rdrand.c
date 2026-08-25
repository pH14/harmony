/* SPDX-License-Identifier: AGPL-3.0-or-later
 *
 * ae6-rdrand.c - what a frozen CPUID model does and does not enforce on SVM.
 *
 * SVM's intercept vector (arch/x86/include/asm/svm.h) has no RDRAND or RDSEED
 * control; VMX has SECONDARY_EXEC_RDRAND_EXITING and SECONDARY_EXEC_RDSEED_EXITING.
 * So on this chip a hypervisor cannot trap a guest RDRAND at all, and the only lever
 * it has is the CPUID model it hands the guest. This measures whether that lever
 * enforces anything: clear leaf 1 ECX bit 30 in the frozen model, then have the guest
 * execute RDRAND anyway and record what came back.
 *
 * The guest installs a real-mode #UD handler first, so "the instruction faulted" and
 * "the instruction ran" are distinguishable rather than one of them being a hang.
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
#define UD_HANDLER 0x1500
#define STORE_GPA  0x2000
#define MEM_SIZE   0x10000

static void emit(uint8_t *mem) {
    uint8_t code[] = {
        0x66,0xB8,0x01,0x00,0x00,0x00,  /* mov eax,1        */
        0x0F,0xA2,                      /* cpuid            */
        0xBF,0x00,0x20,                 /* mov di,0x2000    */
        0x66,0x89,0x0D,                 /* mov [di],ecx     */
        0x66,0x0F,0xC7,0xF0,            /* rdrand eax       */
        0x66,0x89,0x45,0x04,            /* mov [di+4],eax   */
        0xBB,0x00,0x00,                 /* mov bx,0         */
        0x0F,0x92,0xC3,                 /* setc bl          */
        0x89,0x5D,0x08,                 /* mov [di+8],bx    */
        0x66,0x0F,0xC7,0xF0,            /* rdrand eax       */
        0x66,0x89,0x45,0x0A,            /* mov [di+10],eax  */
        0xBB,0x00,0x00,                 /* mov bx,0         */
        0x0F,0x92,0xC3,                 /* setc bl          */
        0x89,0x5D,0x0E,                 /* mov [di+14],bx   */
        0xF4,                           /* hlt              */
    };
    uint8_t ud[] = {
        0xBF,0x00,0x20,                 /* mov di,0x2000        */
        0xC7,0x45,0x10,0xAD,0xDE,       /* mov word [di+16],0xDEAD */
        0xF4,                           /* hlt                  */
    };
    memcpy(mem + GUEST_PHYS, code, sizeof(code));
    memcpy(mem + UD_HANDLER, ud, sizeof(ud));
    /* real-mode interrupt vector 6 (#UD) -> 0000:1500 */
    uint16_t off = UD_HANDLER, seg = 0;
    memcpy(mem + 6*4,     &off, 2);
    memcpy(mem + 6*4 + 2, &seg, 2);
}

int main(int argc, char **argv) {
    const char *out = 0;
    for (int i = 1; i < argc; i++)
        if (!strcmp(argv[i], "--out") && i+1 < argc) out = argv[++i];

    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 2; }

    size_t cap = 256;
    struct kvm_cpuid2 *cp = calloc(1, sizeof(*cp) + cap*sizeof(struct kvm_cpuid_entry2));
    cp->nent = cap;
    if (ioctl(kvm, KVM_GET_SUPPORTED_CPUID, cp) < 0) { perror("GET_SUPPORTED_CPUID"); return 2; }

    uint32_t host_leaf1_ecx = 0;
    for (uint32_t i = 0; i < cp->nent; i++)
        if (cp->entries[i].function == 1 && cp->entries[i].index == 0) {
            host_leaf1_ecx = cp->entries[i].ecx;
            cp->entries[i].ecx &= ~(1u << 30);      /* RDRAND */
        }
    int host_has_rdrand = (host_leaf1_ecx >> 30) & 1;

    /* what this process's own CPUID says, as a control on the host side */
    unsigned a,b,c,d; __cpuid(1, a,b,c,d);
    int bare_host_rdrand = (c >> 30) & 1;

    int vmfd = ioctl(kvm, KVM_CREATE_VM, 0);
    uint8_t *mem = mmap(0, MEM_SIZE, PROT_READ|PROT_WRITE, MAP_SHARED|MAP_ANONYMOUS, -1, 0);
    struct kvm_userspace_memory_region region = {
        .slot=0, .guest_phys_addr=0, .memory_size=MEM_SIZE, .userspace_addr=(uint64_t)mem };
    if (ioctl(vmfd, KVM_SET_USER_MEMORY_REGION, &region) < 0) { perror("SET_MEM"); return 2; }
    int vcpu = ioctl(vmfd, KVM_CREATE_VCPU, 0);
    if (ioctl(vcpu, KVM_SET_CPUID2, cp) < 0) { perror("SET_CPUID2"); return 2; }
    int msize = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    struct kvm_run *run = mmap(0, msize, PROT_READ|PROT_WRITE, MAP_SHARED, vcpu, 0);

    emit(mem);
    struct kvm_sregs s; ioctl(vcpu, KVM_GET_SREGS, &s); s.cs.base=0; s.cs.selector=0;
    ioctl(vcpu, KVM_SET_SREGS, &s);
    struct kvm_regs r; memset(&r,0,sizeof(r)); r.rip=GUEST_PHYS; r.rflags=0x2;
    ioctl(vcpu, KVM_SET_REGS, &r);

    int hlt = 0, other_exit = 0;
    for (int guard = 0; guard < 64; guard++) {
        if (ioctl(vcpu, KVM_RUN, 0) < 0) { if (errno==EINTR) continue; perror("KVM_RUN"); return 2; }
        if (run->exit_reason == KVM_EXIT_HLT) { hlt = 1; break; }
        other_exit = run->exit_reason; break;
    }

    uint32_t g_ecx, v1, v2; uint16_t cf1, cf2, udmark;
    memcpy(&g_ecx,  mem+STORE_GPA,    4);
    memcpy(&v1,     mem+STORE_GPA+4,  4);
    memcpy(&cf1,    mem+STORE_GPA+8,  2);
    memcpy(&v2,     mem+STORE_GPA+10, 4);
    memcpy(&cf2,    mem+STORE_GPA+14, 2);
    memcpy(&udmark, mem+STORE_GPA+16, 2);

    int guest_sees_rdrand = (g_ecx >> 30) & 1;
    int faulted = (udmark == 0xDEAD);
    int executed = !faulted && (cf1 == 1) && (cf2 == 1);
    int values_differ = executed && (v1 != v2);

    FILE *o = out ? fopen(out, "w") : stdout;
    fprintf(o,
      "{\"schema\":\"amd-epyc-ae6-rdrand-v1\",\"hlt_ok\":%d,\"other_exit\":%d,"
      "\"host_leaf1_ecx\":\"0x%08x\",\"host_has_rdrand\":%d,\"bare_host_rdrand\":%d,"
      "\"guest_leaf1_ecx\":\"0x%08x\",\"guest_sees_rdrand\":%d,"
      "\"ud_faulted\":%d,\"executed\":%d,\"cf1\":%d,\"cf2\":%d,"
      "\"value1\":\"0x%08x\",\"value2\":\"0x%08x\",\"values_differ\":%d,"
      "\"cpuid_mask_enforced_execution\":%d}\n",
      hlt, other_exit, host_leaf1_ecx, host_has_rdrand, bare_host_rdrand,
      g_ecx, guest_sees_rdrand, faulted, executed, cf1, cf2, v1, v2, values_differ,
      faulted ? 1 : 0);
    if (out) fclose(o);
    /* 0 = the demonstration completed and is readable, whatever it found. */
    return (hlt && host_has_rdrand && !guest_sees_rdrand) ? 0 : 1;
}
