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
static size_t coverage_requests;
static size_t coverage_opens;
static int coverage_stale_fd;
static int invalid_coverage_response;
static int reenter_during_exchange;
static int reenter_during_entropy;
static int entropy_failure;
static int zero_entropy_response;
static int json_failure;

bool notify_coverage(size_t edge);

static int mock_open(const char *path, int flags)
{
    assert(strcmp(path, "/dev/harmony") == 0);
    assert((flags & O_CLOEXEC) != 0);
    if (entropy_failure == 1 && (flags & O_ACCMODE) == O_RDWR) {
        errno = EIO;
        return -1;
    }
    if (json_failure == 1 && (flags & O_ACCMODE) == O_WRONLY) {
        errno = EIO;
        return -1;
    }
    if ((flags & O_ACCMODE) == O_RDWR)
        coverage_opens++;
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
        if (entropy_failure == 2) {
            errno = EIO;
            return -1;
        }
        if (entropy_failure == 4)
            return 0;
        entropy_requested = 1;
        return 1;
    }
    if (json_failure == 2) {
        errno = EIO;
        return -1;
    }
    if (json_failure == 3)
        return 0;
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
    assert(entropy_requested);
    assert(size == sizeof(entropy));
    if (entropy_failure == 3) {
        errno = EIO;
        return -1;
    }
    if (entropy_failure == 5)
        return 0;
    if (reenter_during_entropy) {
        size_t before = coverage_requests;
        reenter_during_entropy = 0;
        assert(notify_coverage(6));
        assert(coverage_requests == before);
    }
    if (zero_entropy_response)
        memset(data, 0, sizeof(entropy));
    else
        memcpy(data, entropy, sizeof(entropy));
    return (ssize_t)sizeof(entropy);
}

struct harmony_ioc_coverage;

static int mock_ioctl(int fd, unsigned long command, struct harmony_ioc_coverage *arg)
{
    unsigned char *request = (unsigned char *)arg;
    unsigned char *out = request + 16;
    uint64_t observed = 0;
    uint32_t thread = 0;
    uint32_t ready = 0;
    uint32_t selected;
    size_t index;

    assert(fd == 7);
    assert(command == 0xC0204803UL);
    if (coverage_stale_fd) {
        coverage_stale_fd = 0;
        errno = EBADF;
        return -1;
    }
    if (reenter_during_exchange) {
        reenter_during_exchange = 0;
        assert(notify_coverage(5));
    }
    for (index = 0; index < 4; index++)
        assert(request[28 + index] == 0);
    for (index = 0; index < 4; index++) {
        thread |= (uint32_t)request[index] << (index * 8);
        ready |= (uint32_t)request[12 + index] << (index * 8);
    }
    for (index = 0; index < 8; index++)
        observed |= (uint64_t)request[4 + index] << (index * 8);
    assert(ready != 0);
    selected = (uint32_t)(((uint64_t)thread ^ observed) % ready);
    if (!invalid_coverage_response)
        observed++;
    for (index = 0; index < 8; index++)
        out[index] = (unsigned char)(observed >> (index * 8));
    for (index = 0; index < 4; index++)
        out[8 + index] = (unsigned char)(selected >> (index * 8));
    coverage_requests++;
    return 0;
}

#define HARMONY_OPEN(path, flags) mock_open((path), (flags))
#define HARMONY_CLOSE(fd) mock_close((fd))
#define HARMONY_READ(fd, buf, len) mock_read((fd), (buf), (len))
#define HARMONY_WRITE(fd, buf, len) mock_write((fd), (buf), (len))
#define HARMONY_IOCTL(fd, command, arg) mock_ioctl((fd), (command), (arg))
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

static void assert_entropy_failure_aborts(int failure)
{
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        struct rlimit limit = {0, 0};
        assert(setrlimit(RLIMIT_CORE, &limit) == 0);
        entropy_failure = failure;
        (void)fuzz_get_random();
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
}

static void assert_json_failure_aborts(int failure)
{
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        static const char event[] = "{\"antithesis_assert\":{}}";
        struct rlimit limit = {0, 0};
        assert(setrlimit(RLIMIT_CORE, &limit) == 0);
        json_failure = failure;
        fuzz_json_data(event, sizeof(event) - 1);
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
}

static void assert_invalid_json_aborts(bool null_data)
{
    pid_t child = fork();
    assert(child >= 0);
    if (child == 0) {
        static const char event[] = "{\"antithesis_assert\":{}}";
        struct rlimit limit = {0, 0};
        assert(setrlimit(RLIMIT_CORE, &limit) == 0);
        if (null_data)
            fuzz_json_data(NULL, 1);
        else
            fuzz_json_data(event, SIZE_MAX);
        _exit(0);
    }
    int status;
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGABRT);
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
    zero_entropy_response = 1;
    assert(fuzz_get_random() == 0);
    zero_entropy_response = 0;
    fuzz_flush();
    size_t opens = coverage_opens;
    assert(init_coverage_module(3, "first.sym.tsv") == 0);
    assert(init_coverage_module(4, "second.sym.tsv") == 4);
    assert(notify_coverage(1));
    assert(notify_coverage(2));
    assert(coverage_requests == 2);
    assert(coverage_opens == opens + 1);
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
    reenter_during_entropy = 1;
    assert(fuzz_get_random() == UINT64_C(0x0102030405060708));
    assert(coverage_requests == 11);
    assert(harmony_coverage.counter >= harmony_coverage.threshold);
    assert(notify_coverage(6));
    assert(coverage_requests == 12);
    assert(harmony_coverage_configure(1, 0) == -1);
    assert(pthread_create(&thread, NULL, worker, NULL) == 0);
    assert(pthread_join(thread, NULL) == 0);
    assert(coverage_requests == 14);
    assert(harmony_coverage.counter == 12);
    assert(harmony_coverage.threshold == 11);
    assert(harmony_coverage_add(0) == 1);
    assert(coverage_requests == 15);
    assert(harmony_coverage.threshold == 12);
    assert(harmony_coverage_add(3) == 1);
    assert(coverage_requests == 16);
    assert(harmony_coverage.counter == 15);
    harmony_coverage.threshold = 20;
    assert(harmony_coverage_add(2) == 3);
    assert(coverage_requests == 16);
    assert(harmony_coverage_add(UINT64_MAX) == 1);
    assert(harmony_coverage.counter == UINT64_MAX);
    assert(coverage_requests == 17);
    opens = coverage_opens;
    coverage_stale_fd = 1;
    assert(harmony_coverage_add(UINT64_MAX) == 1);
    assert(coverage_requests == 18);
    assert(coverage_opens == opens + 1);
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
    assert_entropy_failure_aborts(1);
    assert_entropy_failure_aborts(2);
    assert_entropy_failure_aborts(3);
    assert_entropy_failure_aborts(4);
    assert_entropy_failure_aborts(5);
    assert_json_failure_aborts(1);
    assert_json_failure_aborts(2);
    assert_json_failure_aborts(3);
    assert_invalid_json_aborts(true);
    assert_invalid_json_aborts(false);
    return 0;
}
