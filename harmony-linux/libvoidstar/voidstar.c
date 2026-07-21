// SPDX-License-Identifier: AGPL-3.0-or-later
// Clean-room implementation of the public libvoidstar ABI used by Antithesis SDKs.
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <unistd.h>

#ifndef HARMONY_OPEN
#define HARMONY_OPEN(path, flags) open((path), (flags))
#endif
#ifndef HARMONY_CLOSE
#define HARMONY_CLOSE(fd) close((fd))
#endif
#ifndef HARMONY_READ
#define HARMONY_READ(fd, buf, len) read((fd), (buf), (len))
#endif
#ifndef HARMONY_WRITE
#define HARMONY_WRITE(fd, buf, len) write((fd), (buf), (len))
#endif

static pthread_mutex_t harmony_device_lock = PTHREAD_MUTEX_INITIALIZER;

/* The R-L3 transport ruling fixes the device path; it is not configurable. */
static const char harmony_device_path[] = "/dev/harmony";

static int write_all(int fd, const unsigned char *data, size_t size)
{
    size_t written = 0;

    while (written < size) {
        ssize_t result = HARMONY_WRITE(fd, data + written, size - written);
        if (result < 0 && errno == EINTR)
            continue;
        if (result <= 0)
            return -1;
        written += (size_t)result;
    }
    return 0;
}

static int read_all(int fd, unsigned char *data, size_t size)
{
    size_t consumed = 0;

    while (consumed < size) {
        ssize_t result = HARMONY_READ(fd, data + consumed, size - consumed);
        if (result < 0 && errno == EINTR)
            continue;
        if (result <= 0)
            return -1;
        consumed += (size_t)result;
    }
    return 0;
}

void fuzz_json_data(const char *data, size_t size)
{
    int fd;

    if ((data == NULL && size != 0) || size > (size_t)SSIZE_MAX)
        return;
    if (pthread_mutex_lock(&harmony_device_lock) != 0)
        return;
    fd = HARMONY_OPEN(harmony_device_path, O_WRONLY | O_CLOEXEC);
    if (fd >= 0) {
        (void)write_all(fd, (const unsigned char *)data, size);
        (void)HARMONY_CLOSE(fd);
    }
    (void)pthread_mutex_unlock(&harmony_device_lock);
}

uint64_t fuzz_get_random(void)
{
    static const unsigned char request = 0;
    unsigned char bytes[8];
    uint64_t value = 0;
    int fd;
    size_t index;

    if (pthread_mutex_lock(&harmony_device_lock) != 0)
        return 0;
    fd = HARMONY_OPEN(harmony_device_path, O_RDWR | O_CLOEXEC);
    if (fd < 0)
        goto out;
    if (write_all(fd, &request, sizeof(request)) != 0 ||
        read_all(fd, bytes, sizeof(bytes)) != 0) {
        (void)HARMONY_CLOSE(fd);
        goto out;
    }
    (void)HARMONY_CLOSE(fd);
    for (index = 0; index < sizeof(bytes); index++)
        value |= (uint64_t)bytes[index] << (index * CHAR_BIT);
out:
    (void)pthread_mutex_unlock(&harmony_device_lock);
    return value;
}

void fuzz_flush(void)
{
}

/* Coverage is deliberately inert until the Harmony coverage service lands. */
void init_coverage_module(const void *module, size_t size)
{
    (void)module;
    (void)size;
}

void notify_coverage(uint64_t edge)
{
    (void)edge;
}

void __sanitizer_cov_trace_pc_guard_init(uint32_t *start, uint32_t *stop)
{
    while (start != NULL && stop != NULL && start < stop) {
        *start = 0;
        start++;
    }
}

void __sanitizer_cov_trace_pc_guard_internal(uint32_t *guard, uint64_t site)
{
    (void)guard;
    (void)site;
}

void __sanitizer_cov_trace_pc_guard(uint32_t *guard)
{
    (void)guard;
}
