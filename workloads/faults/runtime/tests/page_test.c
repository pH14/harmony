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
#define ARMED HARMONY_FAULT_EVENT_PARK_STATUS_ARMED

struct node {
    pid_t pid;
    int control;
    int report;
    int go;
    int done;
    int json;
    char fired[160];
};

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

static void set_fd(const char *name, int fd)
{
    char value[16];

    assert(snprintf(value, sizeof(value), "%d", fd) > 0);
    assert(setenv(name, value, 1) == 0);
}

static void exec_as(char *mode, int go, int done)
{
    char name[] = "page-test";
    char *argv[] = {name, mode, NULL};

    set_fd("PAGE_TEST_GO_FD", go);
    set_fd("PAGE_TEST_DONE_FD", done);
    set_fd("PAGE_TEST_JSON_FD", json_fd);
    (void)execv("/proc/self/exe", argv);
    _exit(3);
}

static void visit(int go, int done)
{
    assert(write(done, "g", 1) == 1);
    assert(next_byte(go) == 'v');
    harmony_instrumentation_event(TARGET);
    _exit(0);
}

static void visit_in_child(int go, int done, int execs)
{
    pid_t child = fork();
    int status;

    assert(child >= 0);
    if (child == 0) {
        if (execs)
            exec_as("visit", go, done);
        visit(go, done);
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
        else if (byte == 'x')
            exec_as("node", go, done);
        else
            visit_in_child(go, done, byte == 'e');
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

static void publish_target(void)
{
    publish(1, 1000, TARGET, TARGET + 1);
}

static void send_request(int fd, uint64_t kind, uint64_t first, uint64_t second)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};

    put_u64(request, kind);
    put_u64(request + 8, first);
    put_u64(request + 16, second);
    assert(write_all(fd, request, sizeof(request)) == 0);
}

static void read_response(int fd, uint64_t kind, unsigned char *response)
{
    assert(read_all(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) == 0);
    assert(get_u64(response) == kind);
}

static void park_status(const struct node *node, uint64_t fires, uint64_t flags,
                        uint64_t generation)
{
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    send_request(node->control, HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0);
    read_response(node->control, HARMONY_FAULT_EVENT_CMD_PARK_STATUS, response);
    assert(get_u64(response + 8) == fires);
    assert(get_u64(response + 16) == flags);
    assert(get_u64(response + 24) == generation);
}

static void await_hello(const struct node *node)
{
    unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];

    assert(read_all(node->report, hello, sizeof(hello)) == 0);
    assert(get_u64(hello) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(get_u64(hello + 8) == HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
}

static void step(const struct node *node, unsigned char byte)
{
    assert(write(node->go, &byte, 1) == 1);
    assert(next_byte(node->done) == 'd');
}

static void stop(const struct node *node)
{
    int status;

    assert(kill(node->pid, SIGSTOP) == 0);
    assert(waitpid(node->pid, &status, WUNTRACED) == node->pid);
    assert(WIFSTOPPED(status));
}

static void reports(const struct node *node, const char *expected)
{
    char buffer[512];
    size_t used = 0;

    for (;;) {
        ssize_t got = read(node->json, buffer + used, sizeof(buffer) - 1 - used);

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

static void start(struct node *node, int published)
{
    char path[] = "/tmp/harmony-page-test-XXXXXX";
    int page_fd = mkstemp(path);
    int control[2];
    int report[2];
    int go[2];
    int done[2];
    int json[2];
    void *mapped;

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
    if (published)
        publish_target();
    node->pid = fork();
    assert(node->pid >= 0);
    if (node->pid == 0) {
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
    assert(close(page_fd) == 0);
    assert(close(control[0]) == 0);
    assert(close(report[0]) == 0);
    assert(close(go[0]) == 0);
    assert(close(done[1]) == 0);
    assert(close(json[1]) == 0);
    node->control = control[1];
    node->report = report[1];
    node->go = go[1];
    node->done = done[0];
    node->json = json[0];
    assert(snprintf(node->fired, sizeof(node->fired),
                    "%ld {\"harmony_park\":{\"site\":%llu,\"edges\":1}}\n",
                    (long)node->pid, (unsigned long long)TARGET) > 0);
    await_hello(node);
}

static void finish(struct node *node)
{
    int status;

    assert(close(node->go) == 0);
    assert(waitpid(node->pid, &status, 0) == node->pid);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(close(node->control) == 0);
    assert(close(node->report) == 0);
    assert(close(node->done) == 0);
    assert(close(node->json) == 0);
    assert(munmap(page, (size_t)HARMONY_FAULT_PAGE_BYTES) == 0);
    page = NULL;
}

static void a_record_published_before_the_runtime_starts_is_adopted(void)
{
    struct node node;

    start(&node, 1);
    step(&node, 'v');
    park_status(&node, 1, ARMED, 2);
    reports(&node, node.fired);
    finish(&node);
}

static void a_clearing_record_disarms_the_park(void)
{
    struct node node;

    start(&node, 0);
    publish_target();
    step(&node, 'v');
    park_status(&node, 1, ARMED, 2);
    reports(&node, node.fired);
    publish(0, 0, 0, 0);
    step(&node, 'v');
    park_status(&node, 1, 0, 4);
    reports(&node, "");
    finish(&node);
}

static void a_disarm_queued_while_stopped_leaves_the_next_park(void)
{
    struct node node;
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    start(&node, 0);
    publish_target();
    park_status(&node, 0, ARMED, 2);
    stop(&node);
    send_request(node.control, HARMONY_FAULT_EVENT_CMD_PARK, 0, 0);
    publish(0, 0, 0, 0);
    publish_target();
    assert(write(node.go, "v", 1) == 1);
    assert(kill(node.pid, SIGCONT) == 0);
    assert(next_byte(node.done) == 'd');
    read_response(node.control, HARMONY_FAULT_EVENT_CMD_PARK, response);
    park_status(&node, 1, ARMED, 6);
    reports(&node, node.fired);
    finish(&node);
}

static void a_forked_child_neither_fires_nor_adopts_its_parents_park(void)
{
    struct node node;

    start(&node, 0);
    publish_target();
    park_status(&node, 0, ARMED, 2);
    assert(write(node.go, "f", 1) == 1);
    assert(next_byte(node.done) == 'g');
    publish_target();
    step(&node, 'v');
    reports(&node, "");
    park_status(&node, 0, ARMED, 4);
    step(&node, 'v');
    park_status(&node, 1, ARMED, 4);
    reports(&node, node.fired);
    finish(&node);
}

static void an_execed_image_maps_the_page_and_adopts_its_record(void)
{
    struct node node;

    start(&node, 0);
    publish_target();
    assert(write(node.go, "x", 1) == 1);
    await_hello(&node);
    step(&node, 'v');
    park_status(&node, 1, ARMED, 2);
    reports(&node, node.fired);
    finish(&node);
}

static void a_forked_child_that_execs_leaves_the_page_behind(void)
{
    struct node node;

    start(&node, 0);
    publish_target();
    park_status(&node, 0, ARMED, 2);
    assert(write(node.go, "e", 1) == 1);
    assert(next_byte(node.done) == 'g');
    step(&node, 'v');
    reports(&node, "");
    park_status(&node, 0, ARMED, 2);
    step(&node, 'v');
    park_status(&node, 1, ARMED, 2);
    reports(&node, node.fired);
    finish(&node);
}

static const struct {
    const char *name;
    void (*run)(void);
} scenarios[] = {
    {"startup", a_record_published_before_the_runtime_starts_is_adopted},
    {"clear", a_clearing_record_disarms_the_park},
    {"stale-disarm", a_disarm_queued_while_stopped_leaves_the_next_park},
    {"fork", a_forked_child_neither_fires_nor_adopts_its_parents_park},
    {"exec", an_execed_image_maps_the_page_and_adopts_its_record},
    {"fork-exec", a_forked_child_that_execs_leaves_the_page_behind},
};

int main(int argc, char **argv)
{
    size_t index;
    int ran = 0;

    if (argc == 2 && (strcmp(argv[1], "node") == 0 || strcmp(argv[1], "visit") == 0)) {
        int go = harmony_fault_parse_fd("PAGE_TEST_GO_FD");
        int done = harmony_fault_parse_fd("PAGE_TEST_DONE_FD");

        json_fd = harmony_fault_parse_fd("PAGE_TEST_JSON_FD");
        if (strcmp(argv[1], "node") == 0)
            run_node(go, done);
        visit(go, done);
    }
    alarm(10);
    for (index = 0; index < sizeof(scenarios) / sizeof(scenarios[0]); index++) {
        if (argc == 2 && strcmp(argv[1], scenarios[index].name) != 0)
            continue;
        scenarios[index].run();
        ran++;
    }
    assert(ran > 0);
    return 0;
}
