// SPDX-License-Identifier: AGPL-3.0-or-later
// Capability inventory only; no VM is created or run.
#include <fcntl.h>
#include <linux/kvm.h>
#include <stdio.h>
#include <sys/ioctl.h>
#include <unistd.h>

int main(void) {
    int fd = open("/dev/kvm", O_RDWR | O_CLOEXEC);
    if (fd < 0) { perror("open /dev/kvm"); return 1; }
    int api = ioctl(fd, KVM_GET_API_VERSION, 0);
    int nested = ioctl(fd, KVM_CHECK_EXTENSION, KVM_CAP_NESTED_STATE);
    printf("KVM_API_VERSION=%d\nKVM_CAP_NESTED_STATE=%d\n", api, nested);
    close(fd);
    return api == KVM_API_VERSION && nested >= 0 ? 0 : 1;
}
