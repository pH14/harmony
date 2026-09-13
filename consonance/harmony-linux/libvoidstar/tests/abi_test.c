// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stddef.h>
#include <stdint.h>
#include <stdatomic.h>
#include <string.h>
#include <sys/types.h>

static unsigned char captured[128];
static size_t captured_len;
static int entropy_requested;
static unsigned char coverage_request[17];
static size_t coverage_requests;
static int coverage_requested;
static uint32_t last_coverage_thread;
static uint64_t last_coverage_observed;
static pthread_mutex_t read_gate_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t read_gate_changed = PTHREAD_COND_INITIALIZER;
static int hold_coverage_read;
static int coverage_read_entered;
static int release_coverage_read;
static int waiter_started;
static _Atomic int waiter_finished;

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

        assert(pthread_mutex_lock(&read_gate_lock) == 0);
        if (hold_coverage_read) {
            coverage_read_entered = 1;
            assert(pthread_cond_broadcast(&read_gate_changed) == 0);
            while (!release_coverage_read)
                assert(pthread_cond_wait(&read_gate_changed, &read_gate_lock) == 0);
        }
        assert(pthread_mutex_unlock(&read_gate_lock) == 0);
        assert(size == 12);
        for (index = 0; index < 4; index++) {
            thread |= (uint32_t)coverage_request[1 + index] << (index * 8);
            ready |= (uint32_t)coverage_request[13 + index] << (index * 8);
        }
        for (index = 0; index < 8; index++)
            observed |= (uint64_t)coverage_request[5 + index] << (index * 8);
        assert(ready != 0);
        last_coverage_thread = thread;
        last_coverage_observed = observed;
        selected = (uint32_t)(((uint64_t)thread ^ observed) % ready);
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

static void *drive_automatic_exchange(void *unused)
{
    size_t index;

    (void)unused;
    for (index = 0; index < 64; index++)
        assert(!notify_coverage(0));
    return NULL;
}

static void *drive_waiting_callback(void *unused)
{
    (void)unused;
    assert(pthread_mutex_lock(&read_gate_lock) == 0);
    waiter_started = 1;
    assert(pthread_cond_broadcast(&read_gate_changed) == 0);
    assert(pthread_mutex_unlock(&read_gate_lock) == 0);
    assert(!notify_coverage(0));
    atomic_store_explicit(&waiter_finished, 1, memory_order_release);
    return NULL;
}

int main(void)
{
    static const char event[] = "{\"antithesis_assert\":{}}";
    uint32_t guards[3] = {1, 2, 3};
    pthread_t owner;
    pthread_t waiter;

    fuzz_json_data(event, sizeof(event) - 1);
    assert(captured_len == sizeof(event) - 1);
    assert(memcmp(captured, event, captured_len) == 0);
    assert(fuzz_get_random() == UINT64_C(0x0102030405060708));
    fuzz_flush();
    assert(init_coverage_module(3, "first.sym.tsv") == 0);
    assert(init_coverage_module(5, "second.sym.tsv") == 3);
    for (size_t index = 0; index < 64; index++)
        assert(!notify_coverage(0));
    assert(coverage_requests == 1);
    assert(last_coverage_observed == 1);
    assert((last_coverage_thread & UINT32_C(0x80000000)) != 0);
    assert(pthread_mutex_lock(&read_gate_lock) == 0);
    hold_coverage_read = 1;
    assert(pthread_mutex_unlock(&read_gate_lock) == 0);
    assert(pthread_create(&owner, NULL, drive_automatic_exchange, NULL) == 0);
    assert(pthread_mutex_lock(&read_gate_lock) == 0);
    while (!coverage_read_entered)
        assert(pthread_cond_wait(&read_gate_changed, &read_gate_lock) == 0);
    assert(pthread_mutex_unlock(&read_gate_lock) == 0);
    assert(pthread_create(&waiter, NULL, drive_waiting_callback, NULL) == 0);
    assert(pthread_mutex_lock(&read_gate_lock) == 0);
    while (!waiter_started)
        assert(pthread_cond_wait(&read_gate_changed, &read_gate_lock) == 0);
    assert(pthread_mutex_unlock(&read_gate_lock) == 0);
    assert(pthread_mutex_trylock(&harmony_automatic_coverage_lock) == EBUSY);
    assert(!atomic_load_explicit(&waiter_finished, memory_order_acquire));
    assert(pthread_mutex_lock(&read_gate_lock) == 0);
    release_coverage_read = 1;
    assert(pthread_cond_broadcast(&read_gate_changed) == 0);
    assert(pthread_mutex_unlock(&read_gate_lock) == 0);
    assert(pthread_join(owner, NULL) == 0);
    assert(pthread_join(waiter, NULL) == 0);
    assert(atomic_load_explicit(&waiter_finished, memory_order_acquire));
    assert(coverage_requests == 2);
    assert(harmony_coverage_configure(7, 3) == 0);
    assert(!notify_coverage(1));
    assert(coverage_requests == 3);
    assert(harmony_coverage_selected() == 0);
    assert(!notify_coverage(2));
    assert(coverage_requests == 4);
    assert(harmony_coverage_selected() == 2);
    __sanitizer_cov_trace_pc_guard_init(guards, guards + 3);
    assert(guards[0] == 1 && guards[1] == 2 && guards[2] == 3);
    __sanitizer_cov_trace_pc_guard_internal(&guards[0], 4);
    __sanitizer_cov_trace_pc_guard(&guards[0]);
    assert(coverage_requests == 6);
    assert(harmony_coverage_configure(1, 0) == -1);
    assert(harmony_coverage_configure(UINT32_C(0x80000000), 1) == -1);
    return 0;
}
