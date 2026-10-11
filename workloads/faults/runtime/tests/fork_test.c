// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

static void discard_json(const char *data, size_t size)
{
    (void)data;
    (void)size;
}
#define HARMONY_JSON(data, size) discard_json((data), (size))
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

static void put_word(unsigned char *frame, size_t offset, uint64_t value)
{
    size_t index;

    for (index = 0; index < 8; index++)
        frame[offset + index] = (unsigned char)(value >> (index * 8));
}

static uint64_t get_word(const unsigned char *frame, size_t offset)
{
    uint64_t value = 0;
    size_t index;

    for (index = 0; index < 8; index++)
        value |= (uint64_t)frame[offset + index] << (index * 8);
    return value;
}

static void exchange(int fd, uint64_t kind, uint64_t first, uint64_t second,
                     unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE])
{
    unsigned char frame[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    memset(frame, 0, sizeof(frame));
    put_word(frame, 0, kind);
    put_word(frame, 8, first);
    put_word(frame, 16, second);
    assert(write_all(fd, frame, sizeof(frame)) == 0);
    assert(read_all(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) == 0);
}

static uint64_t coverage_crossings(int fd)
{
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    exchange(fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS);
    return get_word(response, 8);
}

static uint64_t park_fires(int fd)
{
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];

    exchange(fd, HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    return get_word(response, 8);
}

static int exit_status(pid_t child)
{
    int status;

    assert(waitpid(child, &status, 0) == child);
    return status;
}

static void control_thread_blocks_every_signal(void)
{
    DIR *tasks = opendir("/proc/self/task");
    struct dirent *entry;
    int blocked = 0;

    if (tasks == NULL)
        return;
    while ((entry = readdir(tasks)) != NULL) {
        char path[300];
        char line[256];
        FILE *status;

        if (entry->d_name[0] == '.' || atoi(entry->d_name) == getpid())
            continue;
        (void)snprintf(path, sizeof(path), "/proc/self/task/%s/status",
                       entry->d_name);
        status = fopen(path, "r");
        if (status == NULL)
            continue;
        while (fgets(line, sizeof(line), status) != NULL) {
            unsigned long long mask;

            if (sscanf(line, "SigBlk: %llx", &mask) == 1 &&
                ((mask | 0x40100ULL) & 0x7fffffffULL) == 0x7fffffffULL)
                blocked = 1;
        }
        (void)fclose(status);
    }
    (void)closedir(tasks);
    assert(blocked);
}

int main(void)
{
    int control[2];
    int report[2];
    char text[32];
    unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
    unsigned char kill_report[HARMONY_FAULT_EVENT_REPORT_SIZE];
    uint64_t before;
    pid_t child;
    int status;

    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    (void)snprintf(text, sizeof(text), "%d", control[1]);
    assert(setenv("HARMONY_EVENT_KILL_FD", text, 1) == 0);
    (void)snprintf(text, sizeof(text), "%d", report[1]);
    assert(setenv("HARMONY_EVENT_REPORT_FD", text, 1) == 0);

    harmony_instrumentation_event(UINT64_C(0x1000));
    assert(read_all(report[0], hello, sizeof(hello)) == 0);
    assert(get_word(hello, 0) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(harmony_fault_events != &harmony_fault_events_private);
    control_thread_blocks_every_signal();

    before = coverage_crossings(control[0]);
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        uint64_t site;

        for (site = UINT64_C(0x2000); site < UINT64_C(0x2010); site++)
            harmony_instrumentation_event(site);
        _exit(0);
    }
    status = exit_status(child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(coverage_crossings(control[0]) == before + 16);

    exchange(control[0], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK);
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        harmony_instrumentation_event(UINT64_C(0x3000));
        _exit(0);
    }
    status = exit_status(child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(park_fires(control[0]) == 1);
    exchange(control[0], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);

    child = fork();
    assert(child >= 0);
    if (child == 0) {
        assert(pthread_mutex_lock(&harmony_fault_events->lock) == 0);
        _exit(0);
    }
    status = exit_status(child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    before = coverage_crossings(control[0]);
    harmony_instrumentation_event(UINT64_C(0x4000));
    assert(coverage_crossings(control[0]) == before + 1);

    exchange(control[0], HARMONY_FAULT_EVENT_CMD_KILL, 63, 1, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_KILL);
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        assert(setpgid(0, 0) == 0);
        harmony_instrumentation_event(UINT64_C(0x5000));
        _exit(1);
    }
    assert(read_all(report[0], kill_report, sizeof(kill_report)) == 0);
    assert(get_word(kill_report, 0) == 63);
    assert(get_word(kill_report, 8) == UINT64_C(0x5000));
    status = exit_status(child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL);
    exchange(control[0], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    assert(harmony_fault_events->kill_armed == 0);

    puts("fork test passed");
    return 0;
}
