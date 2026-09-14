// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include "../fault_runtime.h"

extern void notify_coverage(uint64_t edge);

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

static void read_exact(int fd, unsigned char *data, size_t size)
{
    size_t consumed = 0;

    while (consumed < size) {
        ssize_t result = read(fd, data + consumed, size - consumed);
        assert(result > 0);
        consumed += (size_t)result;
    }
}

static void write_exact(int fd, const unsigned char *data, size_t size)
{
    size_t written = 0;

    while (written < size) {
        ssize_t result = write(fd, data + written, size - written);
        assert(result > 0);
        written += (size_t)result;
    }
}

static void exchange(int fd, uint64_t kind, uint64_t first, uint64_t second,
                     unsigned char *response)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};

    put_word(request, 0, kind);
    put_word(request, 8, first);
    put_word(request, 16, second);
    write_exact(fd, request, sizeof(request));
    read_exact(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE);
}

static void exercise_composed_runtime(int report_connected)
{
    int control[2];
    int report[2];
    int proceed[2];
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
    unsigned char report_frame[HARMONY_FAULT_EVENT_REPORT_SIZE];
    unsigned char go = 1;
    pid_t child;
    int status;

    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    assert(pipe(proceed) == 0);
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        char control_name[16];
        char report_name[16];

        assert(setsid() == getpid());
        assert(signal(SIGPIPE, SIG_IGN) != SIG_ERR);
        alarm(10);
        assert(close(control[1]) == 0);
        assert(close(report[1]) == 0);
        assert(close(proceed[1]) == 0);
        assert(snprintf(control_name, sizeof(control_name), "%d", control[0]) > 0);
        assert(snprintf(report_name, sizeof(report_name), "%d", report[0]) > 0);
        assert(setenv("HARMONY_EVENT_KILL_FD", control_name, 1) == 0);
        assert(setenv("HARMONY_EVENT_REPORT_FD", report_name, 1) == 0);
        notify_coverage(1);
        read_exact(proceed[0], &go, sizeof(go));
        notify_coverage(2);
        _exit(0);
    }
    assert(close(control[0]) == 0);
    assert(close(report[0]) == 0);
    assert(close(proceed[0]) == 0);
    read_exact(report[1], report_frame, sizeof(report_frame));
    assert(get_word(report_frame, 0) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(get_word(report_frame, 8) == HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_KILL);
    assert(get_word(response, 8) == 0);
    assert(get_word(response, 16) == 1);
    if (!report_connected)
        assert(close(report[1]) == 0);
    write_exact(proceed[1], &go, sizeof(go));
    if (report_connected) {
        read_exact(report[1], report_frame, sizeof(report_frame));
        assert(get_word(report_frame, 0) == 0);
        assert(get_word(report_frame, 8) == 2);
        assert(close(report[1]) == 0);
    }
    assert(waitpid(child, &status, 0) == child);
    if (report_connected) {
        assert(WIFSIGNALED(status));
        assert(WTERMSIG(status) == SIGKILL);
    } else {
        assert(WIFEXITED(status));
        assert(WEXITSTATUS(status) == 0);
    }
    assert(close(control[1]) == 0);
    assert(close(proceed[1]) == 0);
}

int main(void)
{
    alarm(30);
    exercise_composed_runtime(1);
    exercise_composed_runtime(0);
    return 0;
}
