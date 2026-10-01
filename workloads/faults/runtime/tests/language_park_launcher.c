// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <errno.h>
#include <inttypes.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include "../fault_runtime.h"

static void transfer(int fd, void *buffer, size_t length, int writing)
{
    unsigned char *bytes = buffer;
    while (length != 0) {
        ssize_t count = writing ? write(fd, bytes, length) : read(fd, bytes, length);
        if (count < 0 && errno == EINTR)
            continue;
        assert(count > 0);
        bytes += count;
        length -= (size_t)count;
    }
}

static void put_word(unsigned char *out, uint64_t value)
{
    for (unsigned int index = 0; index < 8; index++)
        out[index] = (unsigned char)(value >> (index * 8));
}

static uint64_t word(const unsigned char *in)
{
    uint64_t value = 0;
    for (unsigned int index = 0; index < 8; index++)
        value |= (uint64_t)in[index] << (index * 8);
    return value;
}

static void exchange_target(int fd, uint64_t command, uint64_t edges, uint64_t hold,
                            uint64_t start, uint64_t end, unsigned char *response)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};
    put_word(request, command);
    put_word(request + 8, edges);
    put_word(request + 16, hold);
    put_word(request + 24, start);
    put_word(request + 32, end);
    transfer(fd, request, sizeof(request), 1);
    transfer(fd, response, sizeof(request), 0);
    assert(word(response) == command);
}

static void exchange(int fd, uint64_t command, uint64_t edges, uint64_t hold,
                     unsigned char *response)
{
    exchange_target(fd, command, edges, hold, 0, 0, response);
}

static void delay(void)
{
    struct timespec remaining = {0, 1000000};
    while (nanosleep(&remaining, &remaining) != 0)
        assert(errno == EINTR);
}

int main(int argc, char **argv)
{
    int control[2], report[2], output[2], start[2];
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
    unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
    char descriptor[32];
    pid_t child;
    int status;
    uint64_t before, fires, target_start = 0, target_end = 0;
    int progress = 0;
    assert(argc >= 2);
    alarm(120);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    assert(pipe(output) == 0);
    assert(pipe(start) == 0);
    child = fork();
    assert(child >= 0);
    if (child == 0) {
        assert(close(control[1]) == 0);
        assert(close(report[1]) == 0);
        assert(close(output[0]) == 0);
        assert(close(start[1]) == 0);
        assert(dup2(output[1], STDOUT_FILENO) == STDOUT_FILENO);
        assert(close(output[1]) == 0);
        assert(snprintf(descriptor, sizeof(descriptor), "%d", control[0]) > 0);
        assert(setenv("HARMONY_EVENT_KILL_FD", descriptor, 1) == 0);
        assert(snprintf(descriptor, sizeof(descriptor), "%d", report[0]) > 0);
        assert(setenv("HARMONY_EVENT_REPORT_FD", descriptor, 1) == 0);
        assert(setenv("HARMONY_LANGUAGE_PARK_SELECTION", "1", 1) == 0);
        assert(snprintf(descriptor, sizeof(descriptor), "%d", start[0]) > 0);
        assert(setenv("HARMONY_LANGUAGE_PARK_START_FD", descriptor, 1) == 0);
        execvp(argv[1], argv + 1);
        return 1;
    }
    assert(close(control[0]) == 0);
    assert(close(report[0]) == 0);
    assert(close(output[1]) == 0);
    assert(close(start[0]) == 0);
    transfer(report[1], hello, sizeof(hello), 0);
    assert(word(hello) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(word(hello + 8) == HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
    FILE *child_output = fdopen(output[0], "r");
    char *line = NULL;
    size_t capacity = 0;
    assert(child_output != NULL);
    do {
        assert(getline(&line, &capacity, child_output) > 0);
        fputs(line, stdout);
        fflush(stdout);
        if (strncmp(line, "HARMONY_LANGUAGE_PARK_RANGE ", 28) == 0) {
            char extra;
            assert(sscanf(line, "HARMONY_LANGUAGE_PARK_RANGE %" SCNu64 " %" SCNu64 " %c",
                          &target_start, &target_end, &extra) == 2);
            assert(target_start < target_end);
        }
    } while (strcmp(line, "HARMONY_LANGUAGE_READY\n") != 0);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1000000000,
                    target_start, target_end, response);
    if (target_end != 0) {
        unsigned char token = 1;
        transfer(start[1], &token, 1, 1);
    }
    do {
        delay();
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    } while ((word(response + 16) & HARMONY_FAULT_EVENT_PARK_STATUS_HELD) == 0);
    fires = word(response + 8);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    before = word(response + 24);
    if (target_end != 0) {
        unsigned char token = 2;
        transfer(start[1], &token, 1, 1);
        do {
            assert(getline(&line, &capacity, child_output) > 0);
            fputs(line, stdout);
            fflush(stdout);
        } while (strncmp(line, "HARMONY_LANGUAGE_MARKER ", 24) != 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(word(response + 8) == fires);
        assert((word(response + 16) & HARMONY_FAULT_EVENT_PARK_STATUS_HELD) != 0);
        progress = 1;
    }
    assert(close(start[1]) == 0);
    for (unsigned int tick = 0; !progress && tick < 10; tick++) {
        delay();
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
        uint64_t after = word(response + 24);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        if (word(response + 8) == fires
            && (word(response + 16) & HARMONY_FAULT_EVENT_PARK_STATUS_HELD) != 0
            && after > before) {
            progress = 1;
            break;
        }
    }
    assert(progress);
    puts("HARMONY_LANGUAGE_PARK_PROGRESS");
    fflush(stdout);
    while (getline(&line, &capacity, child_output) > 0) {
        fputs(line, stdout);
        fflush(stdout);
    }
    free(line);
    assert(fclose(child_output) == 0);
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    return 0;
}
