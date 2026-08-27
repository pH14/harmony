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

enum {
    HARMONY_CMD_ENTROPY = 0,
    HARMONY_CMD_COVERAGE = 1,
    HARMONY_COVERAGE_REQUEST_SIZE = 17,
    HARMONY_COVERAGE_RESPONSE_SIZE = 12
};

struct harmony_coverage_state {
    uint32_t thread;
    uint32_t ready;
    uint32_t selected;
    uint64_t counter;
    uint64_t threshold;
};

static _Thread_local struct harmony_coverage_state harmony_coverage = {
    0, 1, 0, 0, 1
};
static uint32_t harmony_next_guard = 1;

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
    static const unsigned char request = HARMONY_CMD_ENTROPY;
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

static void put_u32(unsigned char *out, uint32_t value)
{
    size_t index;

    for (index = 0; index < 4; index++)
        out[index] = (unsigned char)(value >> (index * CHAR_BIT));
}

static void put_u64(unsigned char *out, uint64_t value)
{
    size_t index;

    for (index = 0; index < 8; index++)
        out[index] = (unsigned char)(value >> (index * CHAR_BIT));
}

static uint32_t get_u32(const unsigned char *in)
{
    uint32_t value = 0;
    size_t index;

    for (index = 0; index < 4; index++)
        value |= (uint32_t)in[index] << (index * CHAR_BIT);
    return value;
}

static uint64_t get_u64(const unsigned char *in)
{
    uint64_t value = 0;
    size_t index;

    for (index = 0; index < 8; index++)
        value |= (uint64_t)in[index] << (index * CHAR_BIT);
    return value;
}

/*
 * Give an instrumented logical thread a stable identity and runnable-set
 * width. The first prescribed threshold is one basic block. Reconfiguration
 * resets only this calling thread's TLS counter.
 */
int harmony_coverage_configure(uint32_t thread, uint32_t ready)
{
    if (ready == 0)
        return -1;
    harmony_coverage.thread = thread;
    harmony_coverage.ready = ready;
    harmony_coverage.selected = 0;
    harmony_coverage.counter = 0;
    harmony_coverage.threshold = 1;
    return 0;
}

uint32_t harmony_coverage_selected(void)
{
    return harmony_coverage.selected;
}

static int coverage_exchange(uint64_t observed)
{
    unsigned char request[HARMONY_COVERAGE_REQUEST_SIZE];
    unsigned char response[HARMONY_COVERAGE_RESPONSE_SIZE];
    uint64_t next;
    uint32_t selected;
    int fd;

    request[0] = HARMONY_CMD_COVERAGE;
    put_u32(request + 1, harmony_coverage.thread);
    put_u64(request + 5, observed);
    put_u32(request + 13, harmony_coverage.ready);
    if (pthread_mutex_lock(&harmony_device_lock) != 0)
        return -1;
    fd = HARMONY_OPEN(harmony_device_path, O_RDWR | O_CLOEXEC);
    if (fd < 0)
        goto fail_locked;
    if (write_all(fd, request, sizeof(request)) != 0 ||
        read_all(fd, response, sizeof(response)) != 0) {
        (void)HARMONY_CLOSE(fd);
        goto fail_locked;
    }
    (void)HARMONY_CLOSE(fd);
    (void)pthread_mutex_unlock(&harmony_device_lock);
    next = get_u64(response);
    selected = get_u32(response + 8);
    if (next <= observed || selected >= harmony_coverage.ready)
        return -1;
    harmony_coverage.threshold = next;
    harmony_coverage.selected = selected;
    return 0;

fail_locked:
    (void)pthread_mutex_unlock(&harmony_device_lock);
    return -1;
}

void init_coverage_module(const void *module, size_t size)
{
    (void)module;
    (void)size;
}

void notify_coverage(uint64_t edge)
{
    (void)edge;
    if (harmony_coverage.counter != UINT64_MAX)
        harmony_coverage.counter++;
    if (harmony_coverage.counter == harmony_coverage.threshold &&
        coverage_exchange(harmony_coverage.counter) != 0)
        harmony_coverage.threshold = UINT64_MAX;
}

void __sanitizer_cov_trace_pc_guard_init(uint32_t *start, uint32_t *stop)
{
    if (pthread_mutex_lock(&harmony_device_lock) != 0)
        return;
    while (start != NULL && stop != NULL && start < stop) {
        if (*start == 0 && harmony_next_guard != 0) {
            *start = harmony_next_guard;
            harmony_next_guard++;
        }
        start++;
    }
    (void)pthread_mutex_unlock(&harmony_device_lock);
}

void __sanitizer_cov_trace_pc_guard_internal(uint32_t *guard, uint64_t site)
{
    if (guard != NULL && *guard != 0)
        notify_coverage(site);
}

void __sanitizer_cov_trace_pc_guard(uint32_t *guard)
{
    if (guard != NULL && *guard != 0)
        notify_coverage((uint64_t)*guard);
}
