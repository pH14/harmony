// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <pthread.h>
#include <setjmp.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static volatile unsigned char *shared;
static size_t hold_write_at;
static unsigned char hold_write_value;

static int writing_sleep(const struct timespec *request, struct timespec *remaining);
#define HARMONY_NANOSLEEP(request, remaining) writing_sleep((request), (remaining))

static char json_report[128];
static size_t json_reports;
static void record_json(const char *data, size_t size);
#define HARMONY_JSON(data, size) record_json((data), (size))
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

#if HARMONY_FAULT_WATCH_SUPPORTED
#define HOLD_NANOS 1111

static int control_fd;
static uint64_t next_site = 0x1000;
static sigjmp_buf foreign_return;
static volatile sig_atomic_t foreign_faults;

static void record_json(const char *data, size_t size)
{
    assert(size < sizeof(json_report));
    memcpy(json_report, data, size);
    json_report[size] = '\0';
    json_reports++;
}

static int writing_sleep(const struct timespec *request, struct timespec *remaining)
{
    if (request->tv_sec != 0 || request->tv_nsec != HOLD_NANOS)
        return nanosleep(request, remaining);
    shared[hold_write_at] = hold_write_value;
    return 0;
}

static void put_word(unsigned char *frame, size_t offset, uint64_t value)
{
    size_t index;

    for (index = 0; index < 8; index++)
        frame[offset + index] = (unsigned char)(value >> (index * 8));
}

static void exchange(uint64_t kind, uint64_t first, uint64_t second)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    put_word(request, 0, kind);
    put_word(request, 8, first);
    put_word(request, 16, second);
    assert(write(control_fd, request, sizeof(request)) == (ssize_t)sizeof(request));
    assert(read_all(control_fd, response, sizeof(response)) == 0);
}

static uint64_t land(size_t offset, unsigned char value)
{
    uint64_t site = next_site++;
    size_t before = json_reports;

    hold_write_at = offset;
    hold_write_value = value;
    exchange(HARMONY_FAULT_EVENT_CMD_PARK, 1, HOLD_NANOS);
    harmony_instrumentation_event(site);
    exchange(HARMONY_FAULT_EVENT_CMD_PARK, 0, 0);
    assert(json_reports == before + 1);
    assert(strstr(json_report, "{\"harmony_park\":") == json_report);
    return site;
}

static void edges(size_t count)
{
    while (count-- > 0)
        harmony_instrumentation_event(next_site++);
}

static void assert_read_report(uint64_t site, uint64_t edge)
{
    char expected[128];

    assert(snprintf(expected, sizeof(expected),
                    "{\"harmony_park_read\":{\"site\":%llu,\"edges\":%llu}}\n",
                    (unsigned long long)site, (unsigned long long)edge) > 0);
    assert(strcmp(json_report, expected) == 0);
}

static void *read_from_another_thread(void *offset)
{
    return (void *)(uintptr_t)shared[(size_t)(uintptr_t)offset];
}

static void foreign_fault(int signo, siginfo_t *info, void *context)
{
    (void)signo;
    (void)info;
    (void)context;
    foreign_faults++;
    siglongjmp(foreign_return, 1);
}

int main(void)
{
    int control[2];
    int report[2];
    char control_name[16];
    char report_name[16];
    unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t reports;
    uint64_t site;
    void *thread_value;
    pthread_t reader;
    pid_t child;
    int status;
    void *hole;
    struct sigaction catcher;
    struct sigaction before;

    alarm(10);
    shared = mmap(NULL, 2 * page, PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    assert(shared != MAP_FAILED);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    assert(snprintf(control_name, sizeof(control_name), "%d", control[0]) > 0);
    assert(snprintf(report_name, sizeof(report_name), "%d", report[0]) > 0);
    assert(setenv("HARMONY_EVENT_KILL_FD", control_name, 1) == 0);
    assert(setenv("HARMONY_EVENT_REPORT_FD", report_name, 1) == 0);
    control_fd = control[1];
    harmony_instrumentation_event(1);
    assert(read_all(report[1], hello, sizeof(hello)) == 0);

    site = land(64, 7);
    assert(shared[64] == 7);
    edges(1);
    assert_read_report(site, 1);
    shared[64] = 0;

    site = land(page + 32, 3);
    edges(2);
    assert(shared[page + 32] == 3);
    edges(1);
    assert_read_report(site, 3);

    land(128, 9);
    reports = json_reports;
    assert(shared[1024] == 0);
    edges(HARMONY_FAULT_WATCH_EDGES);
    assert(json_reports == reports);

    land(200, 5);
    reports = json_reports;
    shared[200] = 4;
    edges(HARMONY_FAULT_WATCH_EDGES);
    assert(json_reports == reports);

    land(300, 6);
    reports = json_reports;
    edges(HARMONY_FAULT_WATCH_EDGES);
    assert(shared[300] == 6);
    edges(1);
    assert(json_reports == reports);

    land(400, 8);
    reports = json_reports;
    assert(pthread_create(&reader, NULL, read_from_another_thread, (void *)(uintptr_t)400) == 0);
    assert(pthread_join(reader, &thread_value) == 0);
    assert((uintptr_t)thread_value == 8);
    assert(shared[400] == 8);
    edges(HARMONY_FAULT_WATCH_EDGES);
    assert(json_reports == reports);

    site = land(500, 2);
    child = fork();
    assert(child >= 0);
    if (child == 0)
        _exit(shared[500] == 2 ? 0 : 1);
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(shared[500] == 2);
    edges(1);
    assert_read_report(site, 1);

    hole = mmap(NULL, page, PROT_NONE, MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    assert(hole != MAP_FAILED);
    memset(&catcher, 0, sizeof(catcher));
    catcher.sa_sigaction = foreign_fault;
    catcher.sa_flags = SA_SIGINFO;
    assert(sigemptyset(&catcher.sa_mask) == 0);
    assert(sigaction(SIGSEGV, &catcher, &before) == 0);
    site = land(600, 1);
    if (sigsetjmp(foreign_return, 1) == 0)
        (void)*(volatile unsigned char *)hole;
    assert(foreign_faults == 1);
    assert(shared[600] == 1);
    edges(1);
    assert_read_report(site, 1);
    if (sigsetjmp(foreign_return, 1) == 0)
        (void)*(volatile unsigned char *)hole;
    assert(foreign_faults == 2);
    assert(sigaction(SIGSEGV, &before, NULL) == 0);
    return 0;
}
#else
static void record_json(const char *data, size_t size)
{
    (void)data;
    (void)size;
    json_reports++;
}

static int writing_sleep(const struct timespec *request, struct timespec *remaining)
{
    return nanosleep(request, remaining);
}

int main(void)
{
    (void)json_report;
    (void)shared;
    (void)hold_write_at;
    (void)hold_write_value;
    return 0;
}
#endif
