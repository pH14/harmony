// SPDX-License-Identifier: AGPL-3.0-or-later
// Synchronous M3 oracle transport over the modeled PL011 data register.

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <sys/mman.h>
#include <unistd.h>

enum {
    PL011_BASE = 0x09000000,
    PL011_PAGE = 4096,
};

static int write_all(volatile uint8_t *uart, const uint8_t *bytes, size_t len)
{
    for (size_t index = 0; index < len; ++index)
        uart[0] = bytes[index];
    return 0;
}

int main(void)
{
    int fd = open("/dev/mem", O_RDWR | O_SYNC | O_CLOEXEC);
    if (fd < 0)
        return 1;

    void *mapping = mmap(NULL, PL011_PAGE, PROT_READ | PROT_WRITE, MAP_SHARED,
                         fd, PL011_BASE);
    int saved_errno = errno;
    if (close(fd) != 0 && mapping != MAP_FAILED)
        return 1;
    errno = saved_errno;
    if (mapping == MAP_FAILED)
        return 1;

    uint8_t buffer[4096];
    int status = 0;
    for (;;) {
        ssize_t count = read(STDIN_FILENO, buffer, sizeof(buffer));
        if (count == 0)
            break;
        if (count < 0) {
            if (errno == EINTR)
                continue;
            status = 1;
            break;
        }
        write_all((volatile uint8_t *)mapping, buffer, (size_t)count);
    }

    if (munmap(mapping, PL011_PAGE) != 0)
        status = 1;
    return status;
}
