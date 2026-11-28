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
static void record_sites(const char *data, size_t size);
#define HARMONY_SITE_JSON(data, size) record_sites((data), (size))
#define HARMONY_SITES_AVAILABLE() 1
#include "../fault_runtime.c"
#include "../fault_runtime_shim.c"

static void record_json(const char *data, size_t size)
{
    assert(size < sizeof(json_report));
    memcpy(json_report, data, size);
    json_report[size] = '\0';
    json_reports++;
}

static void exchange(int fd, uint64_t kind, uint64_t first, uint64_t second,
                     unsigned char *response);

static uint64_t *site_reports;
static size_t site_report_count;
static size_t site_report_capacity;
static size_t site_lines;

static void record_sites(const char *data, size_t size)
{
    static const char prefix[] = "{\"harmony_sites\":[";
    static const char suffix[] = "]}\n";
    char line[HARMONY_FAULT_SITE_LINE_BYTES + 1];
    char *cursor;

    assert(size <= HARMONY_FAULT_SITE_LINE_BYTES);
    memcpy(line, data, size);
    line[size] = '\0';
    assert(strncmp(line, prefix, sizeof(prefix) - 1) == 0);
    assert(strcmp(line + size - (sizeof(suffix) - 1), suffix) == 0);
    cursor = line + sizeof(prefix) - 1;
    for (;;) {
        char *end;
        uint64_t site = strtoull(cursor, &end, 10);

        assert(end != cursor);
        if (site_report_count == site_report_capacity) {
            site_report_capacity = site_report_capacity == 0 ? 1024 : site_report_capacity * 2;
            site_reports = realloc(site_reports, site_report_capacity * sizeof(*site_reports));
            assert(site_reports != NULL);
        }
        site_reports[site_report_count++] = site;
        if (*end == ']')
            break;
        assert(*end == ',');
        cursor = end + 1;
    }
    site_lines++;
}

static size_t sites_reported(uint64_t site)
{
    size_t count = 0;
    size_t index;

    for (index = 0; index < site_report_count; index++)
        count += site_reports[index] == site;
    return count;
}

static int compare_sites(const void *left, const void *right)
{
    uint64_t a = *(const uint64_t *)left;
    uint64_t b = *(const uint64_t *)right;

    return (a > b) - (a < b);
}

static void check_new_site_lines(int control_fd, unsigned char *response)
{
    const uint64_t base = UINT64_C(1) << 60;
    const size_t candidates = HARMONY_FAULT_NEW_SITE_LIMIT + HARMONY_FAULT_NEW_SITE_LIMIT / 2;
    unsigned char *taken = calloc(HARMONY_FAULT_EVENT_SITE_TABLE_SIZE, 1);
    size_t fresh = 0;
    int late_fresh;
    size_t before;
    size_t index;

    assert(taken != NULL);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    assert(sites_reported(9) == 1);
    assert(sites_reported(12) == 1);
    assert(sites_reported(21) == 1);
    before = site_report_count;
    for (index = 0; index < candidates; index++) {
        size_t slot = harmony_fault_event_site_start(base + index);

        if (harmony_fault_events.site_bucket[slot] == 0 && !taken[slot]) {
            taken[slot] = 1;
            fresh++;
        }
    }
    assert(fresh > HARMONY_FAULT_NEW_SITE_LIMIT);
    for (index = 0; index < candidates; index++)
        harmony_instrumentation_event(base + index);
    for (index = 0; index < candidates; index++)
        harmony_instrumentation_event(base + index);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    assert(site_report_count - before == HARMONY_FAULT_NEW_SITE_LIMIT);
    assert(site_lines > 1);
    qsort(site_reports + before, site_report_count - before, sizeof(*site_reports),
          compare_sites);
    for (index = before; index < site_report_count; index++) {
        assert(site_reports[index] >= base && site_reports[index] < base + candidates);
        assert(index == before || site_reports[index] != site_reports[index - 1]);
    }
    late_fresh = harmony_fault_events.site_bucket[harmony_fault_event_site_start(
                     base + candidates)] == 0;
    harmony_instrumentation_event(base + candidates);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    exchange(control_fd, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
    assert(sites_reported(base + candidates) == (late_fresh ? 1U : 0U));
    free(taken);
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

static volatile sig_atomic_t signal_during_ack;
static volatile sig_atomic_t signal_callbacks;

static void instrumented_handler(int signal_number)
{
    (void)signal_number;
    harmony_instrumentation_event(40);
    signal_callbacks++;
}

static ssize_t acknowledged_write(int fd, const void *data, size_t length)
{
    if (length == HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE) {
        assert(pthread_mutex_trylock(&harmony_fault_events.lock) == EBUSY);
        if (signal_during_ack)
            assert(pthread_kill(pthread_self(), SIGUSR1) == 0);
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

static uint64_t *test_page;

static int open_page(void)
{
    char path[] = "/tmp/harmony-event-page-XXXXXX";
    int fd = mkstemp(path);
    void *mapped;

    assert(fd >= 0);
    assert(unlink(path) == 0);
    assert(ftruncate(fd, HARMONY_FAULT_PAGE_BYTES) == 0);
    mapped = mmap(NULL, (size_t)HARMONY_FAULT_PAGE_BYTES, PROT_READ | PROT_WRITE,
                  MAP_SHARED, fd, 0);
    assert(mapped != MAP_FAILED);
    test_page = mapped;
    __atomic_store_n(&test_page[HARMONY_FAULT_PAGE_NODE_PID], (uint64_t)getpid(),
                     __ATOMIC_RELAXED);
    return fd;
}

static void store_word(size_t index, uint64_t value, int order)
{
    __atomic_store_n(&test_page[index], value, order);
}

static void publish_page(uint64_t edges, uint64_t hold_nanos, uint64_t target_start,
                         uint64_t target_end)
{
    uint64_t generation =
        __atomic_load_n(&test_page[HARMONY_FAULT_PAGE_GENERATION], __ATOMIC_RELAXED);

    store_word(HARMONY_FAULT_PAGE_GENERATION, generation + 1, __ATOMIC_RELAXED);
    __atomic_thread_fence(__ATOMIC_RELEASE);
    store_word(HARMONY_FAULT_PAGE_EDGES, edges, __ATOMIC_RELAXED);
    store_word(HARMONY_FAULT_PAGE_HOLD_NANOS, hold_nanos, __ATOMIC_RELAXED);
    store_word(HARMONY_FAULT_PAGE_TARGET_START, target_start, __ATOMIC_RELAXED);
    store_word(HARMONY_FAULT_PAGE_TARGET_END, target_end, __ATOMIC_RELAXED);
    store_word(HARMONY_FAULT_PAGE_GENERATION, generation + 2, __ATOMIC_RELEASE);
}

int main(void)
{
    int control[2];
    int report[2];
    char control_name[16];
    char report_name[16];
    char page_name[16];
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
    assert(snprintf(page_name, sizeof(page_name), "%d", open_page()) > 0);
    assert(setenv("HARMONY_EVENT_PAGE_FD", page_name, 1) == 0);

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
    assert(get_word(response, 24) == harmony_fault_events.coverage_callbacks);

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

    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 16) == 0);
    assert(get_word(response, 24) == 0);
    publish_page(3, 1, 0, 0);
    harmony_instrumentation_event(12);
    harmony_instrumentation_event(5);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 7);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(get_word(response, 24) == 2);
    assert(get_word(response, 32) == 2);
    harmony_instrumentation_event(11);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 8);
    assert(get_word(response, 16) ==
           (HARMONY_FAULT_EVENT_PARK_STATUS_ARMED | HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED));
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":11,\"edges\":3}}\n") == 0);
    publish_page(1, 1, 12, 13);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 8);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(get_word(response, 24) == 4);
    assert(get_word(response, 32) == 4);
    harmony_instrumentation_event(11);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 8);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 9);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":12,\"edges\":1}}\n") == 0);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 12, 14, response);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 9);
    assert(get_word(response, 16) ==
           (HARMONY_FAULT_EVENT_PARK_STATUS_ARMED | HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED));
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);

    store_word(HARMONY_FAULT_PAGE_GENERATION, 5, __ATOMIC_RELEASE);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED);
    assert(get_word(response, 24) == 4);
    store_word(HARMONY_FAULT_PAGE_GENERATION, 4, __ATOMIC_RELEASE);
    publish_page(0, 1, 0, 0);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 9);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED);
    assert(get_word(response, 24) == 6);
    assert(get_word(response, 32) == 4);
    publish_page(1, 1, 12, 12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED);
    assert(get_word(response, 24) == 8);
    assert(get_word(response, 32) == 4);
    publish_page(1, 1, 0, 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 9);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(get_word(response, 24) == 10);
    assert(get_word(response, 32) == 10);
    store_word(HARMONY_FAULT_PAGE_GENERATION, 11, __ATOMIC_RELEASE);
    harmony_instrumentation_event(14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 9);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(get_word(response, 24) == 10);
    store_word(HARMONY_FAULT_PAGE_GENERATION, 10, __ATOMIC_RELEASE);
    harmony_instrumentation_event(14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 10);
    assert(get_word(response, 16) ==
           (HARMONY_FAULT_EVENT_PARK_STATUS_ARMED | HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED));
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":14,\"edges\":1}}\n") == 0);
    publish_page(0, 0, 0, 0);
    harmony_instrumentation_event(14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 10);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED);
    assert(get_word(response, 24) == 12);
    assert(get_word(response, 32) == 10);

    publish_page(1, 1, 0, 0);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 50, 51, response);
    publish_page(0, 0, 0, 0);
    harmony_instrumentation_event(14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 10);
    assert(get_word(response, 16) == HARMONY_FAULT_EVENT_PARK_STATUS_ARMED);
    assert(get_word(response, 24) == 16);
    assert(get_word(response, 32) == 14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 16) == 0);

    store_word(HARMONY_FAULT_PAGE_NODE_PID, (uint64_t)getpid() + 1, __ATOMIC_RELAXED);
    publish_page(1, 1, 0, 0);
    harmony_instrumentation_event(14);
    harmony_instrumentation_event(14);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 10);
    assert(get_word(response, 16) == 0);
    assert(get_word(response, 24) == 18);
    assert(get_word(response, 32) == 14);
    store_word(HARMONY_FAULT_PAGE_NODE_PID, (uint64_t)getpid(), __ATOMIC_RELAXED);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 11, 13, response);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 10);
    exchange_target(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 1, 1, 12, 13, response);
    harmony_instrumentation_event(12);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK_STATUS, 0, 0, response);
    assert(get_word(response, 8) == 11);
    assert(strcmp(json_report, "{\"harmony_park\":{\"site\":12,\"edges\":1}}\n") == 0);
    exchange(control[1], HARMONY_FAULT_EVENT_CMD_PARK, 0, 0, response);

    {
        struct sigaction action;
        uint64_t callbacks;

        memset(&action, 0, sizeof(action));
        action.sa_handler = instrumented_handler;
        assert(sigemptyset(&action.sa_mask) == 0);
        assert(sigaction(SIGUSR1, &action, NULL) == 0);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
        callbacks = get_word(response, 24);
        signal_during_ack = 1;
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
        signal_during_ack = 0;
        assert(signal_callbacks == 1);
        assert(get_word(response, 24) == callbacks);
        harmony_instrumentation_event(41);
        exchange(control[1], HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS, 0, 0, response);
        assert(get_word(response, 24) == callbacks + 1);
    }

    check_new_site_lines(control[1], response);

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
