// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stddef.h>
#include <stdint.h>
#include <signal.h>
#include <stdbool.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/types.h>
#include <sys/wait.h>

static unsigned char captured[128];
static size_t captured_len;
static int entropy_requested;
static unsigned char coverage_request[17];
static size_t coverage_requests;
static int coverage_requested;
static int invalid_coverage_response;
static int reenter_during_exchange;

bool notify_coverage(size_t edge);

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
    if (coverage_requested && reenter_during_exchange) {
        reenter_during_exchange = 0;
        assert(notify_coverage(5));
        assert(coverage_requested);
    }
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
        if (!invalid_coverage_response)
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

static void *worker(void *argument)
{
    (void)argument;
    assert(harmony_coverage.counter == 0);
    assert(harmony_coverage.thread == 0);
    assert(notify_coverage(1));
    assert(harmony_coverage.thread != 0);
    assert(harmony_coverage.counter == 1);
    assert(notify_coverage(1));
    return NULL;
}

int main(void)
{
    static const char event[] = "{\"antithesis_assert\":{}}";
    uint32_t guards[3] = {1, 2, 3};
    pthread_t thread;

    fuzz_json_data(event, sizeof(event) - 1);
    assert(captured_len == sizeof(event) - 1);
    assert(memcmp(captured, event, captured_len) == 0);
    assert(fuzz_get_random() == UINT64_C(0x0102030405060708));
    fuzz_flush();
    assert(init_coverage_module(3, "first.sym.tsv") == 0);
    assert(init_coverage_module(4, "second.sym.tsv") == 4);
    assert(notify_coverage(1));
    assert(notify_coverage(2));
    assert(coverage_requests == 2);
    assert(harmony_coverage.thread != 0);
    assert(harmony_coverage_configure(7, 3) == 0);
    notify_coverage(1);
    assert(coverage_requests == 3);
    assert(harmony_coverage_selected() == 0);
    notify_coverage(2);
    assert(coverage_requests == 4);
    assert(harmony_coverage_selected() == 2);
    __sanitizer_cov_trace_pc_guard_init(guards, guards + 3);
    assert(guards[0] == 1 && guards[1] == 2 && guards[2] == 3);
    __sanitizer_cov_trace_pc_guard_internal(&guards[0], 4);
    __sanitizer_cov_trace_pc_guard(&guards[0]);
    assert(coverage_requests == 6);
    assert(instrumentation_request_abi_version(1) == 1);
    assert(instrumentation_request_abi_version(2) == 2);
    assert(instrumentation_request_abi_version(3) == 2);
    assert(*coverage_lease_generation_addr() == 1);
    assert(notify_coverage_v2(4, 0) == (UINT64_C(1) << 40));
    assert(notify_coverage_v2(4, 0) == (UINT64_C(1) << 40));
    assert(coverage_requests == 8);
    __sanitizer_cov_trace_pc();
    assert(coverage_requests == 9);
    reenter_during_exchange = 1;
    assert(notify_coverage(5));
    assert(coverage_requests == 10);
    assert(harmony_coverage.counter == harmony_coverage.threshold);
    assert(notify_coverage(5));
    assert(coverage_requests == 11);
    assert(harmony_coverage_configure(1, 0) == -1);
    assert(pthread_create(&thread, NULL, worker, NULL) == 0);
    assert(pthread_join(thread, NULL) == 0);
    assert(coverage_requests == 13);
    assert(harmony_coverage.counter == 10);
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        struct rlimit limit = {0, 0};
        assert(setrlimit(RLIMIT_CORE, &limit) == 0);
        invalid_coverage_response = 1;
        (void)notify_coverage(1);
        _exit(1);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
    return 0;
}
