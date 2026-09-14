// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

static int kill_calls;

static int mock_kill(pid_t pid, int signal_number);
static int controlled_sleep(const struct timespec *request, struct timespec *remaining);
#define HARMONY_NANOSLEEP(request, remaining) controlled_sleep((request), (remaining))

static ssize_t acknowledged_write(int fd, const void *data, size_t length);
#define HARMONY_WRITE(fd, data, length) acknowledged_write((fd), (data), (length))
#define HARMONY_KILL(pid, signal_number) mock_kill((pid), (signal_number))
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

static pthread_mutex_t sleep_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t sleep_changed = PTHREAD_COND_INITIALIZER;
static int sleep_entered;
static int sleep_released;

static int mock_kill(pid_t pid, int signal_number)
{
    assert(pid == 0);
    assert(signal_number == SIGKILL);
    assert(pthread_mutex_trylock(&harmony_fault_events.lock) == EBUSY);
    kill_calls++;
    return 0;
}

static int controlled_sleep(const struct timespec *request, struct timespec *remaining)
{
    if (request->tv_nsec != 1234567)
        return nanosleep(request, remaining);
    assert(pthread_mutex_lock(&sleep_lock) == 0);
    sleep_entered = 1;
    assert(pthread_cond_broadcast(&sleep_changed) == 0);
    while (!sleep_released)
        assert(pthread_cond_wait(&sleep_changed, &sleep_lock) == 0);
    assert(pthread_mutex_unlock(&sleep_lock) == 0);
    return 0;
}

static void *park_callback(void *unused)
{
    (void)unused;
    harmony_instrumentation_event(6);
    return NULL;
}

static ssize_t acknowledged_write(int fd, const void *data, size_t length)
{
    if (length == HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) {
        const unsigned char *frame = data;
        assert(pthread_mutex_trylock(&harmony_fault_events.lock) == EBUSY);
        if (get_u64(frame) == HARMONY_FAULT_EVENT_CMD_PARK && get_u64(frame + 16) == 0)
            assert(harmony_fault_events.park_inflight == 0);
    }
    return write(fd, data, length);
}

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
                     unsigned char *response)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};

    put_word(request, 0, kind);
    put_word(request, 8, first);
    put_word(request, 16, second);
    assert(write(fd, request, sizeof(request)) == (ssize_t)sizeof(request));
    assert(read_all(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) == 0);
}

int main(void)
{
    int control[2];
    int report[2];
    char control_name[16];
    char report_name[16];
    unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
    unsigned char report_frame[HARMONY_FAULT_EVENT_REPORT_SIZE];

    alarm(10);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, control) == 0);
    assert(socketpair(AF_UNIX, SOCK_STREAM, 0, report) == 0);
    assert(snprintf(control_name, sizeof(control_name), "%d", control[0]) > 0);
    assert(snprintf(report_name, sizeof(report_name), "%d", report[0]) > 0);
    assert(setenv("HARMONY_EVENT_KILL_FD", control_name, 1) == 0);
    assert(setenv("HARMONY_EVENT_REPORT_FD", report_name, 1) == 0);

    harmony_instrumentation_event(1);
    assert(read_all(report[1], report_frame, sizeof(report_frame)) == 0);
    assert(get_word(report_frame, 0) == HARMONY_FAULT_EVENT_REPORT_HELLO);
    assert(get_word(report_frame, 8) == HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_KILL);
    assert(get_word(response, 8) == 0);
    assert(get_word(response, 16) == 1);
    harmony_instrumentation_event(1);
    assert(recv(report[1], report_frame, sizeof(report_frame), MSG_DONTWAIT) < 0);
    assert(errno == EAGAIN || errno == EWOULDBLOCK);
    harmony_instrumentation_event(2);
    assert(read_all(report[1], report_frame, sizeof(report_frame)) == 0);
    assert(get_word(report_frame, 0) == 0);
    assert(get_word(report_frame, 8) == 2);
    assert(kill_calls == 1);

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 1, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    harmony_instrumentation_event(3);
    assert(read_all(report[1], report_frame, sizeof(report_frame)) == 0);
    assert(get_word(report_frame, 0) == 0);
    assert(get_word(report_frame, 8) == 3);
    assert(kill_calls == 2);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    assert(get_word(response, 8) == 0);
    assert(get_word(response, 16) == 1);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);

    assert(close(report[1]) == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    harmony_instrumentation_event(4);
    assert(kill_calls == 2);

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 1, response);
    harmony_instrumentation_event(5);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    assert(get_word(response, 8) == 1);
    assert(get_word(response, 16) == 0);

    {
        pthread_t callback;
        unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 1234567, response);
        assert(pthread_create(&callback, NULL, park_callback, NULL) == 0);
        assert(pthread_mutex_lock(&sleep_lock) == 0);
        while (!sleep_entered)
            assert(pthread_cond_wait(&sleep_changed, &sleep_lock) == 0);
        assert(pthread_mutex_unlock(&sleep_lock) == 0);
        harmony_instrumentation_event(7);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 8) == 2);
        assert(get_word(response, 16) == 1);
        put_word(request, 0, HARMONY_FAULT_EVENT_CMD_PARK);
        assert(write(control[1], request, sizeof(request)) == (ssize_t)sizeof(request));
        assert(pthread_mutex_lock(&sleep_lock) == 0);
        sleep_released = 1;
        assert(pthread_cond_broadcast(&sleep_changed) == 0);
        assert(pthread_mutex_unlock(&sleep_lock) == 0);
        assert(read_all(control[1], response, sizeof(response)) == 0);
        assert(pthread_join(callback, NULL) == 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 16) == 0);
    }

    assert(close(control[1]) == 0);
    assert(close(report[0]) == 0);
    return 0;
}
