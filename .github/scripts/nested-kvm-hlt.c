// SPDX-License-Identifier: AGPL-3.0-or-later
/*
 * Minimal Linux x86 KVM-in-KVM preflight.
 *
 * This probe answers only whether a nested guest can enter KVM and retire one
 * HLT.  It is intentionally independent of Harmony's VMM, snapshots, and
 * device model.  The surrounding workflow owns the disposable stock-kernel
 * VM and its timeout; this process bounds EINTR retries as well.
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <errno.h>
#include <fcntl.h>
#include <inttypes.h>
#include <linux/kvm.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <unistd.h>

#ifndef KVM_API_VERSION
#error "Linux KVM headers are required"
#endif

_Static_assert(KVM_API_VERSION == 12, "the probe requires KVM API version 12");

static const char SUCCESS_LINE[] = "INNER_KVM_HLT=pass\n";

enum {
    GUEST_RAM_SIZE = 4096,
    MAX_EINTR_RETRIES = 16,
};

struct probe {
    int kvm_fd;
    int vm_fd;
    int vcpu_fd;
    void *guest_ram;
    void *run_mapping;
    size_t run_size;
};

static void report_errno(const char *operation)
{
    int saved_errno = errno;

    (void)fprintf(stderr, "nested-kvm-hlt: %s: %s\n", operation,
                  strerror(saved_errno));
}

static void report_text(const char *message)
{
    (void)fprintf(stderr, "nested-kvm-hlt: %s\n", message);
}

static int close_checked(int *fd, const char *operation)
{
    int result = 0;

    if (*fd < 0)
        return 0;
    if (close(*fd) < 0) {
        report_errno(operation);
        result = -1;
    }
    *fd = -1;
    return result;
}

static int unmap_checked(void *mapping, size_t length, const char *operation)
{
    if (mapping == MAP_FAILED)
        return 0;
    if (munmap(mapping, length) < 0) {
        report_errno(operation);
        return -1;
    }
    return 0;
}

static int run_guest_hlt(int vcpu_fd)
{
    for (unsigned int attempt = 0; attempt < MAX_EINTR_RETRIES; ++attempt) {
        if (ioctl(vcpu_fd, KVM_RUN, 0UL) == 0)
            return 0;
        if (errno != EINTR) {
            report_errno("KVM_RUN");
            return -1;
        }
    }

    report_text("KVM_RUN was interrupted too many times");
    return -1;
}

static int write_success_line(void)
{
    const char *cursor = SUCCESS_LINE;
    size_t remaining = sizeof(SUCCESS_LINE) - 1;
    unsigned int eintr_retries = 0;

    while (remaining != 0) {
        ssize_t written = write(STDOUT_FILENO, cursor, remaining);

        if (written > 0) {
            cursor += (size_t)written;
            remaining -= (size_t)written;
            continue;
        }
        if (written < 0 && errno == EINTR &&
            eintr_retries++ < MAX_EINTR_RETRIES) {
            continue;
        }
        if (written < 0)
            report_errno("write success line");
        else
            report_text("write success line returned zero");
        return -1;
    }
    return 0;
}

int main(void)
{
    struct probe probe = {
        .kvm_fd = -1,
        .vm_fd = -1,
        .vcpu_fd = -1,
        .guest_ram = MAP_FAILED,
        .run_mapping = MAP_FAILED,
        .run_size = 0,
    };
    struct kvm_userspace_memory_region memory_region = {0};
    struct kvm_sregs sregs;
    struct kvm_regs regs = {0};
    struct kvm_run *run;
    int api_version;
    int mmap_size;
    int status = EXIT_FAILURE;

    probe.kvm_fd = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (probe.kvm_fd < 0) {
        report_errno("open /dev/kvm");
        goto cleanup;
    }

    api_version = ioctl(probe.kvm_fd, KVM_GET_API_VERSION, 0UL);
    if (api_version < 0) {
        report_errno("KVM_GET_API_VERSION");
        goto cleanup;
    }
    if (api_version != KVM_API_VERSION) {
        (void)fprintf(stderr,
                      "nested-kvm-hlt: expected KVM API %d, got %d\n",
                      KVM_API_VERSION, api_version);
        goto cleanup;
    }

    probe.vm_fd = ioctl(probe.kvm_fd, KVM_CREATE_VM, 0UL);
    if (probe.vm_fd < 0) {
        report_errno("KVM_CREATE_VM");
        goto cleanup;
    }

    probe.guest_ram = mmap(NULL, GUEST_RAM_SIZE, PROT_READ | PROT_WRITE,
                           MAP_ANONYMOUS | MAP_SHARED, -1, 0);
    if (probe.guest_ram == MAP_FAILED) {
        report_errno("mmap guest RAM");
        goto cleanup;
    }
    ((uint8_t *)probe.guest_ram)[0] = 0xf4; /* HLT at guest physical address 0. */

    memory_region.slot = 0;
    memory_region.guest_phys_addr = 0;
    memory_region.memory_size = GUEST_RAM_SIZE;
    memory_region.userspace_addr = (uint64_t)(uintptr_t)probe.guest_ram;
    if (ioctl(probe.vm_fd, KVM_SET_USER_MEMORY_REGION, &memory_region) < 0) {
        report_errno("KVM_SET_USER_MEMORY_REGION");
        goto cleanup;
    }

    probe.vcpu_fd = ioctl(probe.vm_fd, KVM_CREATE_VCPU, 0UL);
    if (probe.vcpu_fd < 0) {
        report_errno("KVM_CREATE_VCPU");
        goto cleanup;
    }

    mmap_size = ioctl(probe.kvm_fd, KVM_GET_VCPU_MMAP_SIZE, 0UL);
    if (mmap_size < 0) {
        report_errno("KVM_GET_VCPU_MMAP_SIZE");
        goto cleanup;
    }
    if (mmap_size < (int)sizeof(struct kvm_run)) {
        (void)fprintf(stderr,
                      "nested-kvm-hlt: KVM vCPU mapping size %d is smaller than "
                      "struct kvm_run (%zu)\n",
                      mmap_size, sizeof(struct kvm_run));
        goto cleanup;
    }
    probe.run_size = (size_t)mmap_size;
    probe.run_mapping = mmap(NULL, probe.run_size, PROT_READ | PROT_WRITE,
                             MAP_SHARED, probe.vcpu_fd, 0);
    if (probe.run_mapping == MAP_FAILED) {
        report_errno("mmap KVM vCPU run page");
        goto cleanup;
    }

    if (ioctl(probe.vcpu_fd, KVM_GET_SREGS, &sregs) < 0) {
        report_errno("KVM_GET_SREGS");
        goto cleanup;
    }
    sregs.cs.base = 0;
    sregs.cs.selector = 0;
    if (ioctl(probe.vcpu_fd, KVM_SET_SREGS, &sregs) < 0) {
        report_errno("KVM_SET_SREGS");
        goto cleanup;
    }

    regs.rip = 0;
    regs.rflags = 2;
    if (ioctl(probe.vcpu_fd, KVM_SET_REGS, &regs) < 0) {
        report_errno("KVM_SET_REGS");
        goto cleanup;
    }

    if (run_guest_hlt(probe.vcpu_fd) < 0)
        goto cleanup;

    run = (struct kvm_run *)probe.run_mapping;
    if (run->exit_reason != KVM_EXIT_HLT) {
        (void)fprintf(stderr, "nested-kvm-hlt: expected KVM_EXIT_HLT (%u), got %u\n",
                      KVM_EXIT_HLT, run->exit_reason);
        goto cleanup;
    }

    status = EXIT_SUCCESS;

cleanup:
    if (unmap_checked(probe.run_mapping, probe.run_size,
                      "munmap KVM vCPU run page") < 0)
        status = EXIT_FAILURE;
    if (close_checked(&probe.vcpu_fd, "close vCPU") < 0)
        status = EXIT_FAILURE;
    if (close_checked(&probe.vm_fd, "close VM") < 0)
        status = EXIT_FAILURE;
    if (close_checked(&probe.kvm_fd, "close /dev/kvm") < 0)
        status = EXIT_FAILURE;
    if (unmap_checked(probe.guest_ram, GUEST_RAM_SIZE,
                      "munmap guest RAM") < 0)
        status = EXIT_FAILURE;

    if (status == EXIT_SUCCESS && write_success_line() < 0)
        status = EXIT_FAILURE;
    return status;
}
