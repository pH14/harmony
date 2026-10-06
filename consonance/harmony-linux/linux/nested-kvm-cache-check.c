/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <fcntl.h>
#include <linux/kvm.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

static void checked(int result, const char *operation)
{
    if (result < 0) { perror(operation); exit(1); }
}

int main(void)
{
    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    checked(kvm, "open /dev/kvm");
    int vm = ioctl(kvm, KVM_CREATE_VM, 0);
    checked(vm, "KVM_CREATE_VM");
    unsigned char *ram = mmap(NULL, 65536, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (ram == MAP_FAILED) { perror("mmap RAM"); return 1; }
    const unsigned char program[] = {
        0x8c, 0xdb, 0x43, 0x8e, 0xdb, 0x8c, 0xd8, 0xe6, 0x80, 0xeb, 0xf5,
    };
    memcpy(ram + 0x1000, program, sizeof(program));
    struct kvm_userspace_memory_region region = {
        .slot = 0, .guest_phys_addr = 0, .memory_size = 65536,
        .userspace_addr = (unsigned long)ram,
    };
    checked(ioctl(vm, KVM_SET_USER_MEMORY_REGION, &region), "KVM_SET_USER_MEMORY_REGION");
    int vcpu = ioctl(vm, KVM_CREATE_VCPU, 0);
    checked(vcpu, "KVM_CREATE_VCPU");
    int run_size = ioctl(kvm, KVM_GET_VCPU_MMAP_SIZE, 0);
    checked(run_size, "KVM_GET_VCPU_MMAP_SIZE");
    struct kvm_run *run = mmap(NULL, run_size, PROT_READ | PROT_WRITE,
                               MAP_SHARED, vcpu, 0);
    if (run == MAP_FAILED) { perror("mmap run"); return 1; }
    struct kvm_sregs sregs;
    checked(ioctl(vcpu, KVM_GET_SREGS, &sregs), "KVM_GET_SREGS");
    sregs.cs.base = 0;
    sregs.cs.selector = 0;
    checked(ioctl(vcpu, KVM_SET_SREGS, &sregs), "KVM_SET_SREGS");
    struct kvm_regs regs = { .rip = 0x1000, .rsp = 0x7000, .rflags = 2 };
    checked(ioctl(vcpu, KVM_SET_REGS, &regs), "KVM_SET_REGS");
    for (unsigned int step = 1; step <= 12; ++step) {
        checked(ioctl(vcpu, KVM_RUN, 0), "KVM_RUN");
        if (run->exit_reason != KVM_EXIT_IO || run->io.direction != KVM_EXIT_IO_OUT ||
            run->io.port != 0x80 || run->io.size != 1 || run->io.count != 1 ||
            *((unsigned char *)run + run->io.data_offset) != step) {
            fprintf(stderr, "FAIL: cache check step=%u exit=%u\n", step, run->exit_reason);
            return 1;
        }
        printf("NESTED_CACHE_STEP=%u\n", step);
        fflush(stdout);
    }
    close(vcpu);
    close(vm);
    close(kvm);
    return 0;
}
