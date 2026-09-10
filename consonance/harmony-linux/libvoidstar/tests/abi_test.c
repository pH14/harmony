// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <sched.h>
#include <stddef.h>
#include <stdatomic.h>
#include <stdint.h>
#include <string.h>
#include <sys/types.h>

static unsigned char captured[128];
static size_t captured_len;
static int entropy_requested;
static unsigned char coverage_request[17];
static size_t coverage_requests;
static int coverage_requested;
static uint32_t last_coverage_thread;
static uint32_t last_coverage_ready;
static uint64_t last_coverage_observed;
static _Atomic int block_automatic_read;
static _Atomic int automatic_read_entered;
static _Atomic int release_automatic_read;

static int mock_open(const char *path, int flags)
{
    assert(strcmp(path, "/dev/harmony") == 0);
    assert((flags & O_CLOEXEC) != 0);
    return 7;
}

static int mock_close(int fd)
{
    assert(fd == 7);
    return 0;
}

static ssize_t mock_write(int fd, const void *data, size_t size)
{
    assert(fd == 7);
    if (size == 1 && *(const unsigned char *)data == 0) {
        entropy_requested = 1;
        return 1;
    }
    if (size == sizeof(coverage_request) && *(const unsigned char *)data == 1) {
        memcpy(coverage_request, data, size);
        coverage_requests++;
        coverage_requested = 1;
        return (ssize_t)size;
    }
    assert(captured_len + size <= sizeof(captured));
    memcpy(captured + captured_len, data, size);
    captured_len += size;
    return (ssize_t)size;
}

static ssize_t mock_read(int fd, void *data, size_t size)
{
    static const unsigned char entropy[8] = {0x08, 0x07, 0x06, 0x05,
                                             0x04, 0x03, 0x02, 0x01};
    assert(fd == 7);
    if (coverage_requested) {
        unsigned char *out = data;
        uint64_t observed = 0;
        uint32_t thread = 0;
        uint32_t ready = 0;
        uint32_t selected;
        size_t index;

        assert(size == 12);
        for (index = 0; index < 4; index++) {
            thread |= (uint32_t)coverage_request[1 + index] << (index * 8);
            ready |= (uint32_t)coverage_request[13 + index] << (index * 8);
        }
        for (index = 0; index < 8; index++)
            observed |= (uint64_t)coverage_request[5 + index] << (index * 8);
        assert(ready != 0);
        selected = (uint32_t)(((uint64_t)thread ^ observed) % ready);
        last_coverage_thread = thread;
        last_coverage_ready = ready;
        last_coverage_observed = observed;
        if ((thread & UINT32_C(0x80000000)) != 0 &&
            atomic_load_explicit(&block_automatic_read, memory_order_acquire)) {
            atomic_store_explicit(
                &automatic_read_entered, 1, memory_order_release);
            while (!atomic_load_explicit(
                &release_automatic_read, memory_order_acquire))
                (void)sched_yield();
        }
        observed++;
        for (index = 0; index < 8; index++)
            out[index] = (unsigned char)(observed >> (index * 8));
        for (index = 0; index < 4; index++)
            out[8 + index] = (unsigned char)(selected >> (index * 8));
        coverage_requested = 0;
        return 12;
    }
    assert(entropy_requested);
    assert(size == sizeof(entropy));
    memcpy(data, entropy, sizeof(entropy));
    return (ssize_t)sizeof(entropy);
}

#define HARMONY_OPEN(path, flags) mock_open((path), (flags))
#define HARMONY_CLOSE(fd) mock_close((fd))
#define HARMONY_READ(fd, buf, len) mock_read((fd), (buf), (len))
#define HARMONY_WRITE(fd, buf, len) mock_write((fd), (buf), (len))
#include "../voidstar.c"

static void *reach_automatic_threshold(void *unused)
{
    (void)unused;
    assert(!notify_coverage(0));
    return NULL;
}

int main(void)
{
    static const char event[] = "{\"antithesis_assert\":{}}";
    uint32_t guards[3] = {1, 2, 3};
    pthread_t threshold_thread;
    size_t index;

    assert(setenv("HARMONY_INSTRUMENTED_PROCESS_ID", "23", 1) == 0);
    assert(process_id_from_environment() == 23);
    assert(setenv("HARMONY_INSTRUMENTED_PROCESS_ID", "0", 1) == 0);
    assert(process_id_from_environment() == 0);
    assert(setenv("HARMONY_INSTRUMENTED_PROCESS_ID", "2147483648", 1) == 0);
    assert(process_id_from_environment() == 0);
    assert(unsetenv("HARMONY_INSTRUMENTED_PROCESS_ID") == 0);
    harmony_automatic_process_id = 23;

    fuzz_json_data(event, sizeof(event) - 1);
    assert(captured_len == sizeof(event) - 1);
    assert(memcmp(captured, event, captured_len) == 0);
    assert(fuzz_get_random() == UINT64_C(0x0102030405060708));
    fuzz_flush();
    assert(init_coverage_module(3, "first.sym.tsv") == 0);
    assert(init_coverage_module(5, "second.sym.tsv") == 3);
    /* Unconfigured instrumentation yields at the fixed process-wide cadence. */
    for (index = 0; index < 63; index++)
        assert(!notify_coverage(0));
    assert(coverage_requests == 0);
    atomic_store_explicit(&block_automatic_read, 1, memory_order_release);
    assert(pthread_create(
        &threshold_thread, NULL, reach_automatic_threshold, NULL) == 0);
    while (!atomic_load_explicit(
        &automatic_read_entered, memory_order_acquire))
        (void)sched_yield();
    /* Callbacks may pass several thresholds while an exchange is in flight. */
    for (index = 0; index < 192; index++)
        assert(!notify_coverage(0));
    atomic_store_explicit(&release_automatic_read, 1, memory_order_release);
    assert(pthread_join(threshold_thread, NULL) == 0);
    assert(coverage_requests == 1);
    for (index = 2; index <= 4; index++) {
        assert(!notify_coverage(0));
        assert(coverage_requests == index);
        assert(last_coverage_observed == index);
    }
    assert((last_coverage_thread & UINT32_C(0x80000000)) != 0);
    assert((last_coverage_thread & UINT32_C(0x7fffffff)) ==
           UINT32_C(23));
    assert(last_coverage_ready == 1);
    assert(harmony_coverage_configure(7, 3) == 0);
    assert(!notify_coverage(1));
    assert(coverage_requests == 5);
    assert(harmony_coverage_selected() == 0);
    notify_coverage(2);
    assert(coverage_requests == 6);
    assert(harmony_coverage_selected() == 2);
    __sanitizer_cov_trace_pc_guard_init(guards, guards + 3);
    assert(guards[0] == 1 && guards[1] == 2 && guards[2] == 3);
    __sanitizer_cov_trace_pc_guard_internal(&guards[0], 4);
    __sanitizer_cov_trace_pc_guard(&guards[0]);
    assert(coverage_requests == 8);
    assert(harmony_coverage_configure(1, 0) == -1);
    assert(harmony_coverage_configure(UINT32_C(0x80000000), 1) == -1);
    return 0;
}
