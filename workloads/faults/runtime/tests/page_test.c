// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static int json_fd = -1;
static void forward_json(const char *data, size_t size);
#define HARMONY_JSON(data, size) forward_json((data), (size))
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

#define TARGET UINT64_C(0x40)

static uint64_t *page;

static void forward_json(const char *data, size_t size)
{
    char line[160];
    int length = snprintf(line, sizeof(line), "%ld %.*s", (long)getpid(), (int)size, data);

    assert(length > 0 && (size_t)length < sizeof(line));
    assert(write(json_fd, line, (size_t)length) == (ssize_t)length);
}

static unsigned char next_byte(int fd)
{
    for (;;) {
        unsigned char byte = 0;
        ssize_t got = read(fd, &byte, 1);

        if (got < 0 && errno == EINTR)
            continue;
        return got == 1 ? byte : 0;
    }
}

static void visit_in_child(int go, int done)
{
    pid_t child = fork();
    int status;

    assert(child >= 0);
    if (child == 0) {
        assert(write(done, "g", 1) == 1);
        assert(next_byte(go) == 'v');
        harmony_instrumentation_event(TARGET);
        _exit(0);
    }
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static void run_node(int go, int done)
{
    harmony_instrumentation_event(1);
    for (;;) {
        unsigned char byte = next_byte(go);

        if (byte == 0)
            _exit(0);
        if (byte == 'v')
            harmony_instrumentation_event(TARGET);
        else
            visit_in_child(go, done);
        assert(write(done, "d", 1) == 1);
    }
}

static void publish(uint64_t edges, uint64_t hold_nanos, uint64_t start, uint64_t end)
{
    uint64_t generation =
        __atomic_load_n(&page[HARMONY_FAULT_PAGE_GENERATION], __ATOMIC_RELAXED);

    __atomic_store_n(&page[HARMONY_FAULT_PAGE_GENERATION], generation + 1,
                     __ATOMIC_RELAXED);
    __atomic_thread_fence(__ATOMIC_RELEASE);
    __atomic_store_n(&page[HARMONY_FAULT_PAGE_EDGES], edges, __ATOMIC_RELAXED);
    __atomic_store_n(&page[HARMONY_FAULT_PAGE_HOLD_NANOS], hold_nanos, __ATOMIC_RELAXED);
    __atomic_store_n(&page[HARMONY_FAULT_PAGE_TARGET_START], start, __ATOMIC_RELAXED);
    __atomic_store_n(&page[HARMONY_FAULT_PAGE_TARGET_END], end, __ATOMIC_RELAXED);
    __atomic_store_n(&page[HARMONY_FAULT_PAGE_GENERATION], generation + 2,
                     __ATOMIC_RELEASE);
}

static void exchange(int fd, uint64_t kind, uint64_t first, uint64_t second,
                     unsigned char *response)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};

    put_u64(request, kind);
    put_u64(request + 8, first);
    put_u64(request + 16, second);
    assert(write_all(fd, request, sizeof(request)) == 0);
    assert(read_all(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) == 0);
}

static void park_status(int fd, uint64_t fires, uint64_t flags, uint64_t generation)
{
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    exchange(fd, HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_u64(response) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    assert(get_u64(response + 8) == fires);
    assert(get_u64(response + 16) == flags);
    assert(get_u64(response + 24) == generation);
}

static void step(int go, int done, unsigned char byte)
{
    assert(write(go, &byte, 1) == 1);
    assert(next_byte(done) == 'd');
}

static void stop(pid_t node)
{
    int status;

    assert(kill(node, SIGSTOP) == 0);
    assert(waitpid(node, &status, WUNTRACED) == node);
    assert(WIFSTOPPED(status));
}

static void reports(int fd, const char *expected)
{
    char buffer[512];
    size_t used = 0;

    for (;;) {
        ssize_t got = read(fd, buffer + used, sizeof(buffer) - 1 - used);

        if (got < 0 && errno == EINTR)
            continue;
        if (got < 0 && (errno == EAGAIN || errno == EWOULDBLOCK))
            break;
        assert(got > 0);
        used += (size_t)got;
        assert(used < sizeof(buffer) - 1);
    }
    buffer[used] = '\0';
    assert(strcmp(buffer, expected) == 0);
}

static void set_fd(const char *name, int fd)
{
    char value[16];

    assert(snprintf(value, sizeof(value), "%d", fd) > 0);
    assert(setenv(name, value, 1) == 0);
}

int main(void)
{
    char path[] = "/tmp/harmony-page-test-XXXXXX";
    char expected[160];
    int page_fd = mkstemp(path);
    int control[2];
    int report[2];
    int go[2];
    int done[2];
    int json[2];
    unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
    void *mapped;
    pid_t node;
    int status;

    alarm(10);
    assert(page_fd >= 0);
    assert(unlink(path) == 0);
    assert(ftruncate(page_fd, HARMONY_FAULT_PAGE_BYTES) == 0);
    mapped = mmap(NULL, (size_t)HARMONY_FAULT_PAGE_BYTES, PROT_READ | PROT_WRITE,
                  MAP_SHARED, page_fd, 0);
    assert(mapped != MAP_FAILED);
    page = mapped;
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    assert(pipe(go) == 0);
    assert(pipe(done) == 0);
    assert(pipe(json) == 0);
    assert(fcntl(json[0], F_SETFL, O_NONBLOCK) == 0);

    publish(1, 1000, TARGET, TARGET + 1);
    node = fork();
    assert(node >= 0);
    if (node == 0) {
        assert(close(control[1]) == 0);
        assert(close(report[1]) == 0);
        assert(close(go[1]) == 0);
        assert(close(done[0]) == 0);
        assert(close(json[0]) == 0);
        assert(munmap(mapped, (size_t)HARMONY_FAULT_PAGE_BYTES) == 0);
        page = NULL;
        json_fd = json[1];
        set_fd("HARMONY_EVENT_KILL_FD", control[0]);
        set_fd("HARMONY_EVENT_REPORT_FD", report[0]);
        set_fd("HARMONY_EVENT_PAGE_FD", page_fd);
        run_node(go[0], done[1]);
    }
    assert(close(control[0]) == 0);
    assert(close(report[0]) == 0);
    assert(close(go[0]) == 0);
    assert(close(done[1]) == 0);
    assert(close(json[1]) == 0);
    assert(read_all(report[1], hello, sizeof(hello)) == 0);
    assert(get_u64(hello) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(get_u64(hello + 8) == HARMONY_FAULT_EVENT_PROTOCOL_VERSION);

    step(go[1], done[0], 'v');
    park_status(control[1], 0, 0, 2);
    reports(json[0], "");

    stop(node);
    publish(1, 1000, TARGET, TARGET + 1);
    assert(write(go[1], "v", 1) == 1);
    assert(kill(node, SIGCONT) == 0);
    assert(next_byte(done[0]) == 'd');
    park_status(control[1], 1, HARMONY_FAULT_EVENT_PARK_STATUS_ARMED, 4);
    assert(snprintf(expected, sizeof(expected),
                    "%ld {\"harmony_park\":{\"site\":%llu,\"edges\":1}}\n", (long)node,
                    (unsigned long long)TARGET) > 0);
    reports(json[0], expected);

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    park_status(control[1], 1, 0, 4);
    assert(write(go[1], "f", 1) == 1);
    assert(next_byte(done[0]) == 'g');
    publish(1, 1000, TARGET, TARGET + 1);
    step(go[1], done[0], 'v');
    reports(json[0], "");
    park_status(control[1], 1, HARMONY_FAULT_EVENT_PARK_STATUS_ARMED, 6);
    step(go[1], done[0], 'v');
    park_status(control[1], 2, HARMONY_FAULT_EVENT_PARK_STATUS_ARMED, 6);
    reports(json[0], expected);

    assert(close(go[1]) == 0);
    assert(waitpid(node, &status, 0) == node);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    return 0;
}
