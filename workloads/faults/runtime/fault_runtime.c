// SPDX-License-Identifier: AGPL-3.0-or-later

#include "fault_runtime.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#ifndef HARMONY_READ
#define HARMONY_READ(fd, buf, len) read((fd), (buf), (len))
#endif
#ifndef HARMONY_WRITE
#define HARMONY_WRITE(fd, buf, len) write((fd), (buf), (len))
#endif
#ifndef HARMONY_NANOSLEEP
#define HARMONY_NANOSLEEP(request, remaining) nanosleep((request), (remaining))
#endif
#ifndef HARMONY_KILL
#define HARMONY_KILL(pid, signal_number) kill((pid), (signal_number))
#endif

struct harmony_fault_event_state {
    pthread_mutex_t lock;
    int control_fd;
    int report_fd;
    uint8_t kill_rarity;
    uint8_t park_rarity;
    uint32_t kill_armed;
    uint32_t park_armed;
    uint64_t park_hold_nanos;
    uint64_t park_fires;
    uint64_t park_inflight;
    uint32_t initialized;
    uint64_t site_visits[HARMONY_FAULT_EVENT_SITE_TABLE_SIZE];
};

static struct harmony_fault_event_state harmony_fault_events = {
    .lock = PTHREAD_MUTEX_INITIALIZER,
    .control_fd = -1,
    .report_fd = -1
};
static pthread_cond_t harmony_fault_park_done = PTHREAD_COND_INITIALIZER;
static pthread_once_t harmony_fault_event_once = PTHREAD_ONCE_INIT;

static void put_u64(unsigned char *out, uint64_t value)
{
    size_t index;

    for (index = 0; index < 8; index++)
        out[index] = (unsigned char)(value >> (index * CHAR_BIT));
}

static uint64_t get_u64(const unsigned char *in)
{
    uint64_t value = 0;
    size_t index;

    for (index = 0; index < 8; index++)
        value |= (uint64_t)in[index] << (index * CHAR_BIT);
    return value;
}

static int write_all(int fd, const unsigned char *data, size_t size)
{
    size_t written = 0;

    while (written < size) {
        ssize_t result = HARMONY_WRITE(fd, data + written, size - written);
        if (result < 0 && errno == EINTR)
            continue;
        if (result <= 0)
            return -1;
        written += (size_t)result;
    }
    return 0;
}

static int read_all(int fd, unsigned char *data, size_t size)
{
    size_t consumed = 0;

    while (consumed < size) {
        ssize_t result = HARMONY_READ(fd, data + consumed, size - consumed);
        if (result < 0 && errno == EINTR)
            continue;
        if (result <= 0)
            return -1;
        consumed += (size_t)result;
    }
    return 0;
}

static int harmony_fault_event_report_write(int fd, const unsigned char *data,
                                            size_t size)
{
    size_t written = 0;

    while (written < size) {
        ssize_t result;
#ifdef MSG_NOSIGNAL
        result = send(fd, data + written, size - written, MSG_NOSIGNAL);
        if (result < 0 && errno == ENOTSOCK)
            result = HARMONY_WRITE(fd, data + written, size - written);
#else
        result = HARMONY_WRITE(fd, data + written, size - written);
#endif
        if (result < 0 && errno == EINTR)
            continue;
        if (result <= 0)
            return -1;
        written += (size_t)result;
    }
    return 0;
}

static int harmony_fault_parse_fd(const char *name)
{
    const char *value = getenv(name);
    char *end = NULL;
    long parsed;

    if (value == NULL || *value == '\0')
        return -1;
    errno = 0;
    parsed = strtol(value, &end, 10);
    if (errno != 0 || end == value || *end != '\0' || parsed < 0 ||
        parsed > INT_MAX)
        return -1;
    return (int)parsed;
}

static int harmony_fault_event_rarity_allows(uint64_t before, uint8_t rarity)
{
    return before < (UINT64_C(1) << rarity);
}

static size_t harmony_fault_event_site_start(uint64_t site)
{
    site ^= site >> 30;
    site *= UINT64_C(0xbf58476d1ce4e5b9);
    site ^= site >> 27;
    site *= UINT64_C(0x94d049bb133111eb);
    site ^= site >> 31;
    return (size_t)(site & (HARMONY_FAULT_EVENT_SITE_TABLE_SIZE - 1));
}

static uint64_t harmony_fault_event_site_before(uint64_t site)
{
    size_t slot = harmony_fault_event_site_start(site);
    uint64_t before = harmony_fault_events.site_visits[slot];

    if (before != UINT64_MAX)
        harmony_fault_events.site_visits[slot]++;
    return before;
}

static void *harmony_fault_event_control(void *arg)
{
    int fd = *(const int *)arg;

    for (;;) {
        unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
        unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
        uint64_t kind;
        uint64_t first;
        uint64_t second;
        int valid = 1;

        if (read_all(fd, request, sizeof(request)) != 0)
            break;
        kind = get_u64(request);
        first = get_u64(request + 8);
        second = get_u64(request + 16);
        memcpy(response, request, sizeof(response));
        if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
            break;
        if (kind == HARMONY_FAULT_EVENT_CMD_KILL) {
            if (first >= HARMONY_FAULT_EVENT_RARITY_LIMIT ||
                (second != 0 && second != 1)) {
                valid = 0;
            } else if (second == 0) {
                harmony_fault_events.kill_armed = 0;
            } else {
                harmony_fault_events.kill_rarity = (uint8_t)first;
                harmony_fault_events.kill_armed = 1;
            }
        } else if (kind == HARMONY_FAULT_EVENT_CMD_PARK) {
            if (first >= HARMONY_FAULT_EVENT_RARITY_LIMIT) {
                valid = 0;
            } else if (second == 0) {
                harmony_fault_events.park_armed = 0;
                harmony_fault_events.park_hold_nanos = 0;
                while (harmony_fault_events.park_inflight != 0) {
                    if (pthread_cond_wait(&harmony_fault_park_done,
                                          &harmony_fault_events.lock) != 0) {
                        valid = 0;
                        break;
                    }
                }
            } else {
                harmony_fault_events.park_rarity = (uint8_t)first;
                harmony_fault_events.park_hold_nanos = second;
                harmony_fault_events.park_armed = 1;
            }
        } else if (kind == HARMONY_FAULT_EVENT_CMD_PARK_STATUS) {
            memset(response, 0, sizeof(response));
            put_u64(response, HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
            put_u64(response + 8, harmony_fault_events.park_fires);
            put_u64(response + 16,
                    (harmony_fault_events.park_armed != 0 ||
                     harmony_fault_events.park_inflight != 0) ? 1 : 0);
        } else {
            valid = 0;
        }
        if (!valid || write_all(fd, response, sizeof(response)) != 0) {
            harmony_fault_events.kill_armed = 0;
            harmony_fault_events.park_armed = 0;
            harmony_fault_events.initialized = 0;
            (void)pthread_mutex_unlock(&harmony_fault_events.lock);
            break;
        }
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
    }
    if (pthread_mutex_lock(&harmony_fault_events.lock) == 0) {
        harmony_fault_events.kill_armed = 0;
        harmony_fault_events.park_armed = 0;
        harmony_fault_events.initialized = 0;
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
    }
    return NULL;
}

static void harmony_fault_event_init(void)
{
    int control_fd = harmony_fault_parse_fd("HARMONY_EVENT_KILL_FD");
    int report_fd = harmony_fault_parse_fd("HARMONY_EVENT_REPORT_FD");
    pthread_t thread;

    if (control_fd < 0 || report_fd < 0)
        return;
    if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
        return;
    harmony_fault_events.control_fd = control_fd;
    harmony_fault_events.report_fd = report_fd;
    if (pthread_create(&thread, NULL, harmony_fault_event_control,
                       &harmony_fault_events.control_fd) != 0) {
        harmony_fault_events.control_fd = -1;
        harmony_fault_events.report_fd = -1;
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        return;
    }
    (void)pthread_detach(thread);
    (void)pthread_mutex_unlock(&harmony_fault_events.lock);

    {
        unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
        put_u64(hello, HARMONY_FAULT_EVENT_REPORT_HELLO);
        put_u64(hello + 8, HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
        if (harmony_fault_event_report_write(report_fd, hello,
                                             sizeof(hello)) != 0) {
            if (pthread_mutex_lock(&harmony_fault_events.lock) == 0) {
                harmony_fault_events.initialized = 0;
                harmony_fault_events.kill_armed = 0;
                harmony_fault_events.park_armed = 0;
                (void)pthread_mutex_unlock(&harmony_fault_events.lock);
            }
            return;
        }
        if (pthread_mutex_lock(&harmony_fault_events.lock) == 0) {
            harmony_fault_events.initialized = 1;
            (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        }
    }
}

static void harmony_fault_event_sleep(uint64_t hold_nanos)
{
    const uint64_t nanos_per_second = UINT64_C(1000000000);
    struct timespec request;

    request.tv_sec = (time_t)(hold_nanos / nanos_per_second);
    request.tv_nsec = (long)(hold_nanos % nanos_per_second);
    while (HARMONY_NANOSLEEP(&request, &request) != 0 && errno == EINTR) {
    }
}

void harmony_fault_runtime_event(uint64_t site)
{
    uint64_t before;
    uint8_t kill_rarity = 0;
    uint64_t kill_site = 0;
    int kill_report_fd = -1;
    uint64_t park_hold_nanos = 0;
    uint32_t kill_claimed = 0;
    uint32_t park_claimed = 0;

    if (pthread_once(&harmony_fault_event_once, harmony_fault_event_init) != 0)
        return;
    if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
        return;
    if (harmony_fault_events.initialized == 0) {
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        return;
    }
    before = harmony_fault_event_site_before(site);
    if (harmony_fault_events.kill_armed != 0 &&
        harmony_fault_event_rarity_allows(before,
                                          harmony_fault_events.kill_rarity)) {
        harmony_fault_events.kill_armed = 0;
        kill_claimed = 1;
        kill_rarity = harmony_fault_events.kill_rarity;
        kill_site = site;
        kill_report_fd = harmony_fault_events.report_fd;
    } else if (harmony_fault_events.park_armed != 0 &&
             harmony_fault_event_rarity_allows(
                 before, harmony_fault_events.park_rarity)) {
        harmony_fault_events.park_armed = 0;
        if (harmony_fault_events.park_fires != UINT64_MAX)
            harmony_fault_events.park_fires++;
        park_claimed = 1;
        harmony_fault_events.park_inflight++;
        park_hold_nanos = harmony_fault_events.park_hold_nanos;
    }
    if (kill_claimed != 0) {
        unsigned char report[HARMONY_FAULT_EVENT_REPORT_SIZE];
        put_u64(report, (uint64_t)kill_rarity);
        put_u64(report + 8, kill_site);
        if (kill_report_fd < 0 ||
            harmony_fault_event_report_write(kill_report_fd, report,
                                             sizeof(report)) != 0) {
            (void)pthread_mutex_unlock(&harmony_fault_events.lock);
            return;
        }
        (void)HARMONY_KILL(0, SIGKILL);
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        return;
    }
    (void)pthread_mutex_unlock(&harmony_fault_events.lock);
    if (park_claimed != 0) {
        harmony_fault_event_sleep(park_hold_nanos);
        if (pthread_mutex_lock(&harmony_fault_events.lock) == 0) {
            harmony_fault_events.park_inflight--;
            (void)pthread_cond_broadcast(&harmony_fault_park_done);
            (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        }
    }
}
