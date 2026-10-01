/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <fcntl.h>
#include <linux/kvm.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(void)
{
    int kvm = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (kvm < 0) { perror("open /dev/kvm"); return 1; }
    if (ioctl(kvm, KVM_GET_API_VERSION, 0) != 12) {
        fputs("FAIL: KVM API version\n", stderr); return 1;
    }
    int nested_size = ioctl(kvm, KVM_CHECK_EXTENSION, KVM_CAP_NESTED_STATE);
    if (nested_size < 128) {
        fputs("FAIL: KVM_CAP_NESTED_STATE missing\n", stderr); return 1;
    }
    const unsigned int capacity = 256;
    struct kvm_cpuid2 *cpuid = calloc(1, sizeof(*cpuid) + capacity * sizeof(cpuid->entries[0]));
    if (!cpuid) { perror("allocate KVM CPUID"); return 1; }
    cpuid->nent = capacity;
    if (ioctl(kvm, KVM_GET_SUPPORTED_CPUID, cpuid) < 0) {
        perror("KVM_GET_SUPPORTED_CPUID"); free(cpuid); return 1;
    }
    int vmx = 0, svm = 0, npt = 0;
    for (unsigned int i = 0; i < cpuid->nent && i < capacity; ++i) {
        if (cpuid->entries[i].function == 1 && (cpuid->entries[i].ecx & (1u << 5))) vmx = 1;
        if (cpuid->entries[i].function == 0x80000001 && (cpuid->entries[i].ecx & (1u << 2))) svm = 1;
        if (cpuid->entries[i].function == 0x8000000a && (cpuid->entries[i].edx & 1u) && cpuid->entries[i].ebx >= 2) npt = 1;
    }
    free(cpuid);
    if ((!vmx && !(svm && npt)) || (vmx && svm)) {
        fprintf(stderr, "FAIL: nested-host requires KVM-supported VMX or SVM with NPT (nested_size=%d)\n", nested_size);
        return 1;
    }
    int vm = ioctl(kvm, KVM_CREATE_VM, 0);
    if (vm < 0) { perror("KVM_CREATE_VM"); return 1; }
    printf("NESTED_KVM_OK api=12 vendor=%s vmx=%d svm=%d nested_size=%d creations=1\n", vmx ? "vmx" : "svm", vmx, svm, nested_size);
    close(vm);
    close(kvm);
    return 0;
}
