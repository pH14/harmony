// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <errno.h>
#include <signal.h>
#include <pthread.h>
#include <sched.h>
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

static char json_report[128];
static size_t json_reports;
static void record_json(const char *data, size_t size);
#define HARMONY_JSON(data, size) record_json((data), (size))
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

static void record_json(const char *data, size_t size)
{
    assert(size < sizeof(json_report));
    memcpy(json_report, data, size);
    json_report[size] = '\0';
    json_reports++;
}

static pthread_mutex_t sleep_lock = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t sleep_changed = PTHREAD_COND_INITIALIZER;
static int sleep_entered[2];
static int sleep_released[2];

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
    int slot;

    if (request->tv_nsec == 1234567)
        slot = 0;
    else if (request->tv_nsec == 2345678)
        slot = 1;
    else
        return nanosleep(request, remaining);
    assert(pthread_mutex_lock(&sleep_lock) == 0);
    sleep_entered[slot] = 1;
    assert(pthread_cond_broadcast(&sleep_changed) == 0);
    while (!sleep_released[slot])
        assert(pthread_cond_wait(&sleep_changed, &sleep_lock) == 0);
    assert(pthread_mutex_unlock(&sleep_lock) == 0);
    return 0;
}

static void await_sleep(int slot)
{
    assert(pthread_mutex_lock(&sleep_lock) == 0);
    while (!sleep_entered[slot])
        assert(pthread_cond_wait(&sleep_changed, &sleep_lock) == 0);
    assert(pthread_mutex_unlock(&sleep_lock) == 0);
}

static void release_sleep(int slot)
{
    assert(pthread_mutex_lock(&sleep_lock) == 0);
    sleep_released[slot] = 1;
    assert(pthread_cond_broadcast(&sleep_changed) == 0);
    assert(pthread_mutex_unlock(&sleep_lock) == 0);
}

static void *park_callback(void *site)
{
    harmony_instrumentation_event((uint64_t)(uintptr_t)site);
    return NULL;
}

static ssize_t acknowledged_write(int fd, const void *data, size_t length)
{
    if (length == HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE)
        assert(pthread_mutex_trylock(&harmony_fault_events.lock) == EBUSY);
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

static void send_frame(int fd, uint64_t kind, uint64_t first, uint64_t second,
                       uint64_t target_start, uint64_t target_end)
{
    unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE] = {0};

    put_word(request, 0, kind);
    put_word(request, 8, first);
    put_word(request, 16, second);
    put_word(request, 24, target_start);
    put_word(request, 32, target_end);
    assert(write(fd, request, sizeof(request)) == (ssize_t)sizeof(request));
}

static void exchange_target(int fd, uint64_t kind, uint64_t first, uint64_t second,
                            uint64_t target_start, uint64_t target_end,
                            unsigned char *response)
{
    send_frame(fd, kind, first, second, target_start, target_end);
    assert(read_all(fd, response, HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) == 0);
}

static void exchange(int fd, uint64_t kind, uint64_t first, uint64_t second,
                     unsigned char *response)
{
    exchange_target(fd, kind, first, second, 0, 0, response);
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
    harmony_fault_events.park_weight_left = UINT64_C(2) << HARMONY_FAULT_PARK_WEIGHT_SHIFT;
    assert(harmony_fault_park_spend(0) == 0);
    assert(harmony_fault_park_spend(1) == 0);
    assert(harmony_fault_park_spend(2) == 0);
    assert(harmony_fault_park_spend(3) == 1);
    harmony_fault_events.park_weight_left = 2;
    assert(harmony_fault_park_spend(UINT64_C(1) << 40) == 0);
    assert(harmony_fault_events.park_weight_left == 1);
    assert(harmony_fault_park_spend(UINT64_MAX) == 1);
    assert(harmony_fault_events.park_weight_left == 0);

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

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    harmony_instrumentation_event(3);
    assert(read_all(report[1], report_frame, sizeof(report_frame)) == 0);
    assert(get_word(report_frame, 0) == 0);
    assert(get_word(report_frame, 8) == 3);
    assert(kill_calls == 2);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    assert(get_word(response, 8) == 0);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);

    assert(close(report[1]) == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_KILL, 0, 1, response);
    harmony_instrumentation_event(4);
    assert(kill_calls == 2);

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 3, 1, response);
    harmony_instrumentation_event(5);
    harmony_instrumentation_event(5);
    harmony_instrumentation_event(5);
    harmony_instrumentation_event(10);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 0);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(json_reports == 0);
    harmony_instrumentation_event(9);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
    assert(get_word(response, 8) == 1);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(json_reports == 1);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":9,\"edges\":3}}\n") == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 2, 1, response);
    harmony_instrumentation_event(11);
    harmony_instrumentation_event(11);
    harmony_instrumentation_event(11);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(json_reports == 1);
    harmony_instrumentation_event(11);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 2);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(json_reports == 2);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":11,\"edges\":2}}\n") == 0);
    harmony_instrumentation_event(12);
    harmony_instrumentation_event(12);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 2);
    assert(json_reports == 2);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 3);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(json_reports == 3);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":12,\"edges\":2}}\n") == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    harmony_instrumentation_event(13);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 3);
    assert(get_word(response, 16) == 0);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 20, 22, response);
    assert(get_word(response, 24) == 20);
    assert(get_word(response, 32) == 22);
    harmony_instrumentation_event(30);
    harmony_instrumentation_event(19);
    harmony_instrumentation_event(22);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 3);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    harmony_instrumentation_event(21);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 4);
    assert(json_reports == 4);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":21,\"edges\":1}}\n") == 0);
    harmony_instrumentation_event(31);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 4);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, response);
    harmony_instrumentation_event(32);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 5);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":32,\"edges\":1}}\n") == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS);
    assert(get_word(response, 8) == harmony_fault_events.coverage_crossings);
    assert(get_word(response, 8) != 0);
    assert(get_word(response, 16) == harmony_fault_events.coverage_digest);

    {
        pthread_t first;
        pthread_t second;
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1234567, response);
        assert(pthread_create(&first, NULL, park_callback, (void *)(uintptr_t)6) == 0);
        await_sleep(0);
        harmony_instrumentation_event(7);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 8) == 6);
        assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_HELD);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
        assert(get_word(response, 0) == HARMONY_FAULT_EVENT_CMD_PARK);
        assert(get_word(response, 16) == 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_HELD);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 2345678, response);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 16) ==
               (HARMONY_FAULT_EVENT_PARK_STATUS_ARMED | HARMONY_FAULT_EVENT_PARK_STATUS_HELD));
        assert(pthread_create(&second, NULL, park_callback, (void *)(uintptr_t)8) == 0);
        await_sleep(1);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 8) == 7);
        assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_HELD);
        release_sleep(1);
        assert(pthread_join(second, NULL) == 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_HELD);
        release_sleep(0);
        assert(pthread_join(first, NULL) == 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
        assert(get_word(response, 8) == 7);
        assert(get_word(response, 16) == 0);
    }

    send_frame(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 22, 22);
    for (;;) {
        int initialized;

        assert(pthread_mutex_lock(&harmony_fault_events.lock) == 0);
        initialized = harmony_fault_events.initialized != 0;
        assert(pthread_mutex_unlock(&harmony_fault_events.lock) == 0);
        if (!initialized)
            break;
        assert(sched_yield() == 0);
    }
    assert(harmony_fault_events.park_armed == 0);

    assert(close(control[1]) == 0);
    assert(close(report[0]) == 0);
    return 0;
}
