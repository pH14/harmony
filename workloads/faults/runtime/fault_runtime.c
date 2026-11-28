// SPDX-License-Identifier: AGPL-3.0-or-later

#include "fault_runtime.h"

#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#if defined(__linux__)
#include <link.h>
#endif

#if defined(__linux__) && (defined(__aarch64__) || defined(__x86_64__))
#define HARMONY_FAULT_WATCH_SUPPORTED 1
#include <sys/uio.h>
#include <ucontext.h>
#else
#define HARMONY_FAULT_WATCH_SUPPORTED 0
#endif

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
#ifndef HARMONY_JSON
void fuzz_json_data(const char *data, size_t size);
#define HARMONY_JSON(data, size) fuzz_json_data((data), (size))
#endif
#ifndef HARMONY_SITE_JSON
#define HARMONY_SITE_JSON(data, size) HARMONY_JSON((data), (size))
#endif
#ifndef HARMONY_SITES_AVAILABLE
#define HARMONY_SITES_AVAILABLE() (access("/dev/harmony", W_OK) == 0)
#endif

#define HARMONY_FAULT_MODULE_LIMIT 4096
#define HARMONY_FAULT_PARK_WEIGHT_SHIFT 20
#define HARMONY_FAULT_WATCH_EDGES UINT64_C(50)
#define HARMONY_FAULT_WATCH_RANGE_LIMIT 16
#define HARMONY_FAULT_WATCH_COPY_LIMIT ((size_t)1 << 20)
#define HARMONY_FAULT_WATCH_PAGE_LIMIT 256
#define HARMONY_FAULT_WATCH_PENDING_LIMIT 4
#define HARMONY_FAULT_NEW_SITE_LIMIT ((size_t)1 << 16)
#define HARMONY_FAULT_SITE_LINE_BYTES 2048
#define HARMONY_FAULT_WATCH_UNKNOWN_WIDTH 16
#define HARMONY_FAULT_WATCH_CHUNK ((size_t)4096)

static void harmony_fault_after_fork(void);

struct harmony_fault_module {
    uint64_t start;
    uint64_t end;
    uint64_t base;
};

struct harmony_fault_crossing {
    uint64_t site;
    uint8_t bucket;
};

struct harmony_fault_event_state {
    pthread_mutex_t lock;
    int control_fd;
    int report_fd;
    uint8_t kill_rarity;
    uint32_t kill_armed;
    uint32_t park_armed;
    uint32_t park_from_arm;
    uint64_t park_edges;
    uint64_t park_weight_left;
    uint64_t park_hold_nanos;
    uint64_t park_target_start;
    uint64_t park_target_end;
    uint64_t park_fires;
    uint64_t park_inflight;
    uint32_t park_paged;
    const uint64_t *page;
    int page_fd;
    uint64_t page_seen;
    uint64_t page_armed;
    uint32_t page_armed_fired;
    uint32_t initialized;
    uint64_t coverage_crossings;
    uint64_t coverage_digest;
    uint64_t coverage_callbacks;
    struct harmony_fault_crossing *deferred;
    size_t deferred_count;
    size_t deferred_capacity;
    uint64_t *new_sites;
    size_t new_site_count;
    size_t new_site_capacity;
    uint32_t new_sites_off;
    uint64_t site_visits[HARMONY_FAULT_EVENT_SITE_TABLE_SIZE];
    uint8_t site_bucket[HARMONY_FAULT_EVENT_SITE_TABLE_SIZE];
};

static struct harmony_fault_event_state harmony_fault_events = {
    .lock = PTHREAD_MUTEX_INITIALIZER,
    .control_fd = -1,
    .report_fd = -1,
    .page_fd = -1
};
static pthread_once_t harmony_fault_event_once = PTHREAD_ONCE_INIT;
static _Thread_local volatile sig_atomic_t harmony_fault_held;

static int harmony_fault_lock(void)
{
    harmony_fault_held++;
    if (pthread_mutex_lock(&harmony_fault_events.lock) != 0) {
        harmony_fault_held--;
        return -1;
    }
    return 0;
}

static void harmony_fault_unlock(void)
{
    (void)pthread_mutex_unlock(&harmony_fault_events.lock);
    harmony_fault_held--;
}

static struct harmony_fault_module
    harmony_fault_modules[HARMONY_FAULT_MODULE_LIMIT];
static size_t harmony_fault_module_count;

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

static int harmony_fault_park_spend(uint64_t before)
{
    uint64_t visits = before == UINT64_MAX ? before : before + 1;
    uint64_t weight = harmony_fault_events.park_from_arm != 0
                          ? UINT64_C(1) << HARMONY_FAULT_PARK_WEIGHT_SHIFT
                          : (UINT64_C(1) << HARMONY_FAULT_PARK_WEIGHT_SHIFT) / visits;

    if (weight == 0)
        weight = 1;
    if (harmony_fault_events.park_weight_left <= weight) {
        harmony_fault_events.park_weight_left = 0;
        return 1;
    }
    harmony_fault_events.park_weight_left -= weight;
    return 0;
}

static uint64_t harmony_fault_event_mix(uint64_t value)
{
    value ^= value >> 30;
    value *= UINT64_C(0xbf58476d1ce4e5b9);
    value ^= value >> 27;
    value *= UINT64_C(0x94d049bb133111eb);
    value ^= value >> 31;
    return value;
}

static size_t harmony_fault_event_site_start(uint64_t site)
{
    return (size_t)(harmony_fault_event_mix(site) &
                    (HARMONY_FAULT_EVENT_SITE_TABLE_SIZE - 1));
}

static uint8_t harmony_fault_event_bucket(uint64_t visits)
{
    if (visits <= 3)
        return (uint8_t)visits;
    if (visits <= 7)
        return 4;
    if (visits <= 15)
        return 5;
    if (visits <= 31)
        return 6;
    if (visits <= 127)
        return 7;
    return 8;
}

static const struct harmony_fault_module *harmony_fault_module_find(
    uint64_t site)
{
    size_t index =
        __atomic_load_n(&harmony_fault_module_count, __ATOMIC_ACQUIRE);

    while (index > 0) {
        const struct harmony_fault_module *module =
            &harmony_fault_modules[--index];
        if (site >= module->start && site < module->end)
            return module;
    }
    return NULL;
}

static int harmony_fault_site_lookup(uint64_t site, uint64_t *offset)
{
    const struct harmony_fault_module *module = harmony_fault_module_find(site);

    if (module == NULL)
        return 0;
    *offset = site - module->base;
    return 1;
}

static uint64_t harmony_fault_site_offset(uint64_t site)
{
    uint64_t offset;

    return harmony_fault_site_lookup(site, &offset) != 0 ? offset : site;
}

static int harmony_fault_park_counts(uint64_t site)
{
    uint64_t offset;

    if (harmony_fault_events.park_target_end == 0)
        return 1;
    offset = harmony_fault_site_offset(site);
    return offset >= harmony_fault_events.park_target_start &&
           offset < harmony_fault_events.park_target_end;
}

#if defined(__linux__)
static void harmony_fault_module_add(uint64_t start, uint64_t size,
                                     uint64_t base)
{
    size_t count =
        __atomic_load_n(&harmony_fault_module_count, __ATOMIC_RELAXED);
    size_t index = count;

    if (size == 0)
        return;
    while (index > 0) {
        const struct harmony_fault_module *module =
            &harmony_fault_modules[--index];
        if (module->start < start + size && start < module->end) {
            if (module->start == start && module->end == start + size &&
                module->base == base)
                return;
            break;
        }
    }
    if (count == HARMONY_FAULT_MODULE_LIMIT)
        return;
    harmony_fault_modules[count].start = start;
    harmony_fault_modules[count].end = start + size;
    harmony_fault_modules[count].base = base;
    __atomic_store_n(&harmony_fault_module_count, count + 1, __ATOMIC_RELEASE);
}

static int harmony_fault_modules_generation(struct dl_phdr_info *info,
                                            size_t size, void *data)
{
    (void)size;
    *(uint64_t *)data = (uint64_t)info->dlpi_adds + (uint64_t)info->dlpi_subs;
    return 1;
}

static int harmony_fault_modules_segments(struct dl_phdr_info *info,
                                          size_t size, void *data)
{
    size_t index;

    (void)size;
    (void)data;
    for (index = 0; index < info->dlpi_phnum; index++) {
        const ElfW(Phdr) *header = &info->dlpi_phdr[index];
        if (header->p_type == PT_LOAD)
            harmony_fault_module_add(
                (uint64_t)(info->dlpi_addr + header->p_vaddr),
                (uint64_t)header->p_memsz, (uint64_t)info->dlpi_addr);
    }
    return 0;
}
#endif

static void harmony_fault_modules_refresh(void)
{
#if defined(__linux__)
    static uint64_t harmony_fault_module_generation;
    uint64_t generation = 0;

    (void)dl_iterate_phdr(harmony_fault_modules_generation, &generation);
    generation++;
    if (generation == harmony_fault_module_generation)
        return;
    harmony_fault_module_generation = generation;
    (void)dl_iterate_phdr(harmony_fault_modules_segments, NULL);
#endif
}

static uint64_t harmony_fault_event_site_before(uint64_t site,
                                                uint8_t *crossed)
{
    size_t slot = harmony_fault_event_site_start(site);
    uint64_t before = harmony_fault_events.site_visits[slot];
    uint8_t bucket;

    *crossed = 0;
    if (before == UINT64_MAX)
        return before;
    harmony_fault_events.site_visits[slot]++;
    bucket = harmony_fault_event_bucket(before + 1);
    if (bucket > harmony_fault_events.site_bucket[slot]) {
        harmony_fault_events.site_bucket[slot] = bucket;
        *crossed = bucket;
    }
    return before;
}

static uint64_t harmony_fault_crossing_hash(uint64_t offset, uint8_t bucket)
{
    return harmony_fault_event_mix((offset << 4) | bucket);
}

static int harmony_fault_crossing_defer(uint64_t site, uint8_t bucket)
{
    if (harmony_fault_events.deferred_count ==
        harmony_fault_events.deferred_capacity) {
        size_t capacity = harmony_fault_events.deferred_capacity == 0
                              ? 64
                              : harmony_fault_events.deferred_capacity * 2;
        struct harmony_fault_crossing *grown =
            realloc(harmony_fault_events.deferred,
                    capacity * sizeof(*grown));
        if (grown == NULL)
            return -1;
        harmony_fault_events.deferred = grown;
        harmony_fault_events.deferred_capacity = capacity;
    }
    harmony_fault_events.deferred[harmony_fault_events.deferred_count].site =
        site;
    harmony_fault_events.deferred[harmony_fault_events.deferred_count].bucket =
        bucket;
    harmony_fault_events.deferred_count++;
    return 0;
}

static void harmony_fault_new_site(uint64_t offset)
{
    if (harmony_fault_events.new_sites_off != 0 ||
        harmony_fault_events.new_site_count == HARMONY_FAULT_NEW_SITE_LIMIT)
        return;
    if (harmony_fault_events.new_site_count ==
        harmony_fault_events.new_site_capacity) {
        size_t capacity = harmony_fault_events.new_site_capacity == 0
                              ? 256
                              : harmony_fault_events.new_site_capacity * 2;
        uint64_t *grown = realloc(harmony_fault_events.new_sites,
                                  capacity * sizeof(*grown));
        if (grown == NULL)
            return;
        harmony_fault_events.new_sites = grown;
        harmony_fault_events.new_site_capacity = capacity;
    }
    harmony_fault_events.new_sites[harmony_fault_events.new_site_count++] =
        offset;
}

static void harmony_fault_crossings_resolve(void)
{
    size_t index;

    for (index = 0; index < harmony_fault_events.deferred_count; index++) {
        const struct harmony_fault_crossing *crossing =
            &harmony_fault_events.deferred[index];
        uint64_t offset = harmony_fault_site_offset(crossing->site);

        harmony_fault_events.coverage_digest +=
            harmony_fault_crossing_hash(offset, crossing->bucket);
        if (crossing->bucket == 1)
            harmony_fault_new_site(offset);
    }
    harmony_fault_events.deferred_count = 0;
}

static void harmony_fault_event_note_crossing(uint64_t site, uint8_t bucket)
{
    uint64_t offset;
    int resolved = harmony_fault_site_lookup(site, &offset);

    if (harmony_fault_lock() != 0)
        return;
    harmony_fault_events.coverage_crossings++;
    if (resolved != 0) {
        harmony_fault_events.coverage_digest +=
            harmony_fault_crossing_hash(offset, bucket);
        if (bucket == 1)
            harmony_fault_new_site(offset);
    } else if (harmony_fault_crossing_defer(site, bucket) != 0) {
        harmony_fault_events.coverage_digest +=
            harmony_fault_crossing_hash(site, bucket);
        if (bucket == 1)
            harmony_fault_new_site(site);
    }
    harmony_fault_unlock();
}

static void harmony_fault_new_sites_flush(void)
{
    static const char prefix[] = "{\"harmony_sites\":[";
    static const char suffix[] = "]}\n";
    char line[HARMONY_FAULT_SITE_LINE_BYTES];
    uint64_t *sites;
    size_t count;
    size_t index;
    size_t length = 0;

    if (harmony_fault_lock() != 0)
        return;
    sites = harmony_fault_events.new_sites;
    count = harmony_fault_events.new_site_count;
    harmony_fault_events.new_sites = NULL;
    harmony_fault_events.new_site_count = 0;
    harmony_fault_events.new_site_capacity = 0;
    harmony_fault_unlock();
    for (index = 0; index < count; index++) {
        char number[24];
        int width = snprintf(number, sizeof(number), "%llu",
                             (unsigned long long)sites[index]);

        if (width <= 0)
            continue;
        if (length != 0 && length + 1 + (size_t)width + sizeof(suffix) >
                               sizeof(line)) {
            memcpy(line + length, suffix, sizeof(suffix) - 1);
            HARMONY_SITE_JSON(line, length + sizeof(suffix) - 1);
            length = 0;
        }
        if (length == 0) {
            memcpy(line, prefix, sizeof(prefix) - 1);
            length = sizeof(prefix) - 1;
        } else {
            line[length++] = ',';
        }
        memcpy(line + length, number, (size_t)width);
        length += (size_t)width;
    }
    if (length != 0) {
        memcpy(line + length, suffix, sizeof(suffix) - 1);
        HARMONY_SITE_JSON(line, length + sizeof(suffix) - 1);
    }
    free(sites);
}

static void harmony_fault_page_park_clear(void)
{
    if (harmony_fault_events.park_paged == 0)
        return;
    harmony_fault_events.park_armed = 0;
    harmony_fault_events.park_hold_nanos = 0;
    harmony_fault_events.park_paged = 0;
}

static int harmony_fault_page_adopt(void)
{
    const uint64_t *page = harmony_fault_events.page;
    uint64_t generation;
    uint64_t edges;
    uint64_t hold_nanos;
    uint64_t target_start;
    uint64_t target_end;
    uint64_t node_pid;

    if (page == NULL)
        return 0;
    generation = __atomic_load_n(&page[HARMONY_FAULT_PAGE_GENERATION],
                                 __ATOMIC_ACQUIRE);
    if (generation == harmony_fault_events.page_seen)
        return 0;
    if ((generation & 1u) != 0)
        return -1;
    edges = __atomic_load_n(&page[HARMONY_FAULT_PAGE_EDGES], __ATOMIC_RELAXED);
    hold_nanos =
        __atomic_load_n(&page[HARMONY_FAULT_PAGE_HOLD_NANOS], __ATOMIC_RELAXED);
    target_start =
        __atomic_load_n(&page[HARMONY_FAULT_PAGE_TARGET_START], __ATOMIC_RELAXED);
    target_end =
        __atomic_load_n(&page[HARMONY_FAULT_PAGE_TARGET_END], __ATOMIC_RELAXED);
    node_pid = __atomic_load_n(&page[HARMONY_FAULT_PAGE_NODE_PID], __ATOMIC_RELAXED);
    __atomic_thread_fence(__ATOMIC_ACQUIRE);
    if (__atomic_load_n(&page[HARMONY_FAULT_PAGE_GENERATION], __ATOMIC_RELAXED) !=
        generation)
        return -1;
    harmony_fault_events.page_seen = generation;
    if (node_pid != (uint64_t)getpid())
        return 0;
    if (edges == 0) {
        harmony_fault_page_park_clear();
        return 0;
    }
    if (edges > HARMONY_FAULT_EVENT_PARK_EDGE_LIMIT || hold_nanos == 0 ||
        (target_end == 0 ? target_start != 0 : target_start >= target_end))
        return 0;
    harmony_fault_events.park_edges = edges;
    harmony_fault_events.park_weight_left = edges << HARMONY_FAULT_PARK_WEIGHT_SHIFT;
    harmony_fault_events.park_hold_nanos = hold_nanos;
    harmony_fault_events.park_from_arm = 1;
    harmony_fault_events.park_target_start = target_start;
    harmony_fault_events.park_target_end = target_end;
    harmony_fault_events.park_paged = 1;
    harmony_fault_events.park_armed = 1;
    harmony_fault_events.page_armed = generation;
    harmony_fault_events.page_armed_fired = 0;
    return 0;
}

static int harmony_fault_page_map(const uint64_t **page, int *page_fd)
{
    int fd;
    void *mapped;

    *page = NULL;
    *page_fd = -1;
    if (getenv("HARMONY_EVENT_PAGE_FD") == NULL)
        return 0;
    fd = harmony_fault_parse_fd("HARMONY_EVENT_PAGE_FD");
    if (fd < 0)
        return -1;
    mapped = mmap(NULL, (size_t)HARMONY_FAULT_PAGE_BYTES, PROT_READ, MAP_SHARED,
                  fd, 0);
    if (mapped == MAP_FAILED)
        return -1;
    *page = mapped;
    *page_fd = fd;
    return 0;
}

static void *harmony_fault_event_control(void *arg)
{
    int fd = *(const int *)arg;

    harmony_fault_modules_refresh();
    for (;;) {
        unsigned char request[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
        unsigned char response[HARMONY_FAULT_EVENT_CONTROL_FRAME_SIZE];
        uint64_t kind;
        uint64_t first;
        uint64_t second;
        uint64_t target_start;
        uint64_t target_end;
        int valid = 1;

        if (read_all(fd, request, sizeof(request)) != 0)
            break;
        harmony_fault_modules_refresh();
        kind = get_u64(request);
        first = get_u64(request + 8);
        second = get_u64(request + 16);
        target_start = get_u64(request + 24);
        target_end = get_u64(request + 32);
        memcpy(response, request, sizeof(response));
        if (harmony_fault_lock() != 0)
            break;
        harmony_fault_crossings_resolve();
        (void)harmony_fault_page_adopt();
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
            if (second != 0 &&
                (first == 0 || first > HARMONY_FAULT_EVENT_PARK_EDGE_LIMIT ||
                 (target_end == 0 ? target_start != 0
                                  : target_start >= target_end))) {
                valid = 0;
            } else if (second == 0) {
                if (harmony_fault_events.park_paged == 0) {
                    harmony_fault_events.park_armed = 0;
                    harmony_fault_events.park_hold_nanos = 0;
                }
            } else {
                harmony_fault_events.park_edges = first;
                harmony_fault_events.park_weight_left =
                    first << HARMONY_FAULT_PARK_WEIGHT_SHIFT;
                harmony_fault_events.park_hold_nanos = second;
                harmony_fault_events.park_from_arm = 0;
                harmony_fault_events.park_paged = 0;
                harmony_fault_events.park_target_start = target_start;
                harmony_fault_events.park_target_end = target_end;
                harmony_fault_events.park_armed = 1;
            }
        } else if (kind == HARMONY_FAULT_EVENT_CMD_PARK_STATUS) {
            memset(response, 0, sizeof(response));
            put_u64(response, HARMONY_FAULT_EVENT_CMD_PARK_STATUS);
            put_u64(response + 8, harmony_fault_events.park_fires);
            put_u64(response + 16,
                    (harmony_fault_events.park_armed != 0
                         ? HARMONY_FAULT_EVENT_PARK_STATUS_ARMED
                         : 0) |
                        (harmony_fault_events.park_inflight != 0
                             ? HARMONY_FAULT_EVENT_PARK_STATUS_HELD
                             : 0) |
                        (harmony_fault_events.page_armed_fired != 0
                             ? HARMONY_FAULT_EVENT_PARK_STATUS_PAGE_FIRED
                             : 0));
            put_u64(response + 24, harmony_fault_events.page_seen);
            put_u64(response + 32, harmony_fault_events.page_armed);
        } else if (kind == HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS) {
            memset(response, 0, sizeof(response));
            put_u64(response, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS);
            put_u64(response + 8, harmony_fault_events.coverage_crossings);
            put_u64(response + 16, harmony_fault_events.coverage_digest);
            put_u64(response + 24, harmony_fault_events.coverage_callbacks);
        } else {
            valid = 0;
        }
        if (!valid || write_all(fd, response, sizeof(response)) != 0) {
            harmony_fault_events.kill_armed = 0;
            harmony_fault_events.park_armed = 0;
            harmony_fault_events.initialized = 0;
            harmony_fault_unlock();
            break;
        }
        harmony_fault_unlock();
        harmony_fault_new_sites_flush();
    }
    if (harmony_fault_lock() == 0) {
        harmony_fault_events.kill_armed = 0;
        harmony_fault_events.park_armed = 0;
        harmony_fault_events.initialized = 0;
        harmony_fault_unlock();
    }
    return NULL;
}

static void harmony_fault_event_start(void)
{
    int control_fd = harmony_fault_parse_fd("HARMONY_EVENT_KILL_FD");
    int report_fd = harmony_fault_parse_fd("HARMONY_EVENT_REPORT_FD");
    const uint64_t *page;
    int page_fd;
    pthread_t thread;

    if (control_fd < 0 || report_fd < 0)
        return;
    if (harmony_fault_page_map(&page, &page_fd) != 0)
        return;
    if (harmony_fault_lock() != 0)
        return;
    harmony_fault_events.control_fd = control_fd;
    harmony_fault_events.report_fd = report_fd;
    harmony_fault_events.page = page;
    harmony_fault_events.page_fd = page_fd;
    harmony_fault_events.page_seen = 0;
    harmony_fault_events.new_sites_off = HARMONY_SITES_AVAILABLE() ? 0 : 1;
    if (pthread_create(&thread, NULL, harmony_fault_event_control,
                       &harmony_fault_events.control_fd) != 0) {
        harmony_fault_events.control_fd = -1;
        harmony_fault_events.report_fd = -1;
        harmony_fault_events.page = NULL;
        harmony_fault_unlock();
        return;
    }
    (void)pthread_detach(thread);
    harmony_fault_unlock();
    (void)pthread_atfork(NULL, NULL, harmony_fault_after_fork);

    {
        unsigned char hello[HARMONY_FAULT_EVENT_REPORT_SIZE];
        put_u64(hello, HARMONY_FAULT_EVENT_REPORT_HELLO);
        put_u64(hello + 8, HARMONY_FAULT_EVENT_PROTOCOL_VERSION);
        if (harmony_fault_event_report_write(report_fd, hello,
                                             sizeof(hello)) != 0) {
            if (harmony_fault_lock() == 0) {
                harmony_fault_events.initialized = 0;
                harmony_fault_events.kill_armed = 0;
                harmony_fault_events.park_armed = 0;
                harmony_fault_unlock();
            }
            return;
        }
        if (harmony_fault_lock() == 0) {
            harmony_fault_events.initialized = 1;
            harmony_fault_unlock();
        }
    }
}

static void harmony_fault_event_init(void)
{
    harmony_fault_held++;
    harmony_fault_event_start();
    harmony_fault_held--;
}

static void harmony_fault_park_report(uint64_t site, uint64_t edges)
{
    char json[96];
    int length;

    site = harmony_fault_site_offset(site);
    length = snprintf(json, sizeof(json),
                          "{\"harmony_park\":{\"site\":%llu,\"edges\":%llu}}\n",
                          (unsigned long long)site, (unsigned long long)edges);

    if (length > 0 && (size_t)length < sizeof(json))
        HARMONY_JSON(json, (size_t)length);
}

#if HARMONY_FAULT_WATCH_SUPPORTED
static void harmony_fault_park_read_report(uint64_t site, uint64_t edges)
{
    char json[96];
    int length;

    site = harmony_fault_site_offset(site);
    length = snprintf(json, sizeof(json),
                      "{\"harmony_park_read\":{\"site\":%llu,\"edges\":%llu}}\n",
                      (unsigned long long)site, (unsigned long long)edges);

    if (length > 0 && (size_t)length < sizeof(json))
        HARMONY_JSON(json, (size_t)length);
}

struct harmony_fault_watch_range {
    uintptr_t start;
    size_t size;
    size_t copy_at;
    unsigned long offset;
    unsigned long inode;
    unsigned long major;
    unsigned long minor;
    char perms[4];
};

struct harmony_fault_watch_maps {
    int fd;
    size_t used;
    size_t at;
    char buffer[256];
};

static struct {
    int claimed;
    int active;
    int installed;
    int read_changed;
    pthread_t owner;
    uint64_t site;
    uint64_t edges;
    uintptr_t page_size;
    size_t range_count;
    size_t copied;
    size_t page_count;
    size_t pending_count;
    int self_read_refused;
    int pipe_open;
    int pipe_fds[2];
    struct sigaction previous;
    struct harmony_fault_watch_range ranges[HARMONY_FAULT_WATCH_RANGE_LIMIT];
    uintptr_t pages[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    unsigned char page_range[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    unsigned char released[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    size_t pending[HARMONY_FAULT_WATCH_PENDING_LIMIT];
    unsigned char copy[HARMONY_FAULT_WATCH_COPY_LIMIT];
    unsigned char changed[HARMONY_FAULT_WATCH_COPY_LIMIT / 8];
    unsigned char now[HARMONY_FAULT_WATCH_CHUNK];
} harmony_fault_watch;

static int harmony_fault_watch_maps_open(struct harmony_fault_watch_maps *maps)
{
    maps->used = 0;
    maps->at = 0;
    maps->fd = open("/proc/self/maps", O_RDONLY | O_CLOEXEC);
    return maps->fd < 0 ? -1 : 0;
}

static int harmony_fault_watch_maps_char(struct harmony_fault_watch_maps *maps)
{
    if (maps->at == maps->used) {
        ssize_t got;

        do
            got = read(maps->fd, maps->buffer, sizeof(maps->buffer));
        while (got < 0 && errno == EINTR);
        if (got <= 0)
            return -1;
        maps->used = (size_t)got;
        maps->at = 0;
    }
    return (unsigned char)maps->buffer[maps->at++];
}

static int harmony_fault_watch_maps_number(struct harmony_fault_watch_maps *maps,
                                           unsigned long base, unsigned long *value,
                                           int *next)
{
    int digits = 0;
    int c = harmony_fault_watch_maps_char(maps);

    *value = 0;
    for (;;) {
        unsigned long digit;

        if (c >= '0' && c <= '9')
            digit = (unsigned long)(c - '0');
        else if (base == 16 && c >= 'a' && c <= 'f')
            digit = (unsigned long)(c - 'a' + 10);
        else
            break;
        *value = *value * base + digit;
        digits++;
        c = harmony_fault_watch_maps_char(maps);
    }
    *next = c;
    return digits > 0;
}

static int harmony_fault_watch_maps_next(struct harmony_fault_watch_maps *maps,
                                         struct harmony_fault_watch_range *found)
{
    for (;;) {
        unsigned long start = 0;
        unsigned long end = 0;
        char perms[4] = {0};
        int c = 0;
        size_t index;
        int ok = harmony_fault_watch_maps_number(maps, 16, &start, &c) && c == '-' &&
                 harmony_fault_watch_maps_number(maps, 16, &end, &c) && c == ' ';

        for (index = 0; ok && index < sizeof(perms); index++) {
            c = harmony_fault_watch_maps_char(maps);
            ok = c >= 0 && c != '\n';
            perms[index] = (char)c;
        }
        ok = ok && (c = harmony_fault_watch_maps_char(maps)) == ' ' &&
             harmony_fault_watch_maps_number(maps, 16, &found->offset, &c) && c == ' ' &&
             harmony_fault_watch_maps_number(maps, 16, &found->major, &c) && c == ':' &&
             harmony_fault_watch_maps_number(maps, 16, &found->minor, &c) && c == ' ' &&
             harmony_fault_watch_maps_number(maps, 10, &found->inode, &c);
        while (c >= 0 && c != '\n')
            c = harmony_fault_watch_maps_char(maps);
        if (ok && end > start) {
            found->start = (uintptr_t)start;
            found->size = (size_t)(end - start);
            memcpy(found->perms, perms, sizeof(perms));
            return 1;
        }
        if (c < 0)
            return 0;
    }
}

static int harmony_fault_watch_holds(const struct harmony_fault_watch_range *line,
                                     size_t index, int watching)
{
    const struct harmony_fault_watch_range *range =
        &harmony_fault_watch.ranges[harmony_fault_watch.page_range[index]];
    uintptr_t page = harmony_fault_watch.pages[index];

    return page >= line->start && page - line->start < line->size &&
           (memcmp(line->perms, "rw-s", sizeof(line->perms)) == 0 ||
            (watching != 0 && memcmp(line->perms, "---s", sizeof(line->perms)) == 0)) &&
           line->inode == range->inode && line->major == range->major &&
           line->minor == range->minor &&
           line->offset + (page - line->start) == range->offset + (page - range->start);
}

static void harmony_fault_watch_check_pages(size_t first, size_t end, int watching,
                                            unsigned char unknown, unsigned char *ours)
{
    struct harmony_fault_watch_maps maps;
    struct harmony_fault_watch_range line;
    size_t index;

    if (harmony_fault_watch_maps_open(&maps) != 0) {
        memset(ours, unknown, end - first);
        return;
    }
    memset(ours, 0, end - first);
    while (harmony_fault_watch_maps_next(&maps, &line))
        for (index = first; index < end; index++)
            if (harmony_fault_watch_holds(&line, index, watching))
                ours[index - first] = 1;
    (void)close(maps.fd);
}

static void harmony_fault_watch_restore(void)
{
    unsigned char ours[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    size_t index;

    harmony_fault_watch_check_pages(0, harmony_fault_watch.page_count, 1, 1, ours);
    for (index = 0; index < harmony_fault_watch.page_count; index++)
        if (__atomic_load_n(&harmony_fault_watch.released[index], __ATOMIC_ACQUIRE) == 0 &&
            ours[index] != 0)
            (void)mprotect((void *)harmony_fault_watch.pages[index],
                           (size_t)harmony_fault_watch.page_size, PROT_READ | PROT_WRITE);
}

static void harmony_fault_watch_stop(void)
{
    if (__atomic_exchange_n(&harmony_fault_watch.active, 0, __ATOMIC_ACQ_REL) != 0)
        harmony_fault_watch_restore();
    if (__atomic_exchange_n(&harmony_fault_watch.installed, 0, __ATOMIC_ACQ_REL) != 0)
        (void)sigaction(SIGSEGV, &harmony_fault_watch.previous, NULL);
    __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
}

static int harmony_fault_watch_owned(void)
{
    return __atomic_load_n(&harmony_fault_watch.active, __ATOMIC_ACQUIRE) != 0 &&
           pthread_equal(harmony_fault_watch.owner, pthread_self()) != 0;
}

static void harmony_fault_watch_pipe_close(void)
{
    if (harmony_fault_watch.pipe_open == 0)
        return;
    (void)close(harmony_fault_watch.pipe_fds[0]);
    (void)close(harmony_fault_watch.pipe_fds[1]);
    harmony_fault_watch.pipe_open = 0;
}

static int harmony_fault_watch_pipe_read(void *to, uintptr_t from, size_t size)
{
    size_t done = 0;

    if (harmony_fault_watch.pipe_open == 0) {
        if (pipe2(harmony_fault_watch.pipe_fds, O_CLOEXEC | O_NONBLOCK) != 0)
            return -1;
        harmony_fault_watch.pipe_open = 1;
    }
    while (done < size) {
        size_t want = size - done < HARMONY_FAULT_WATCH_CHUNK ? size - done
                                                              : HARMONY_FAULT_WATCH_CHUNK;
        ssize_t put = write(harmony_fault_watch.pipe_fds[1], (const void *)(from + done), want);
        size_t got = 0;

        if (put < 0 && errno == EINTR)
            continue;
        while (put > 0 && got < (size_t)put) {
            ssize_t step = read(harmony_fault_watch.pipe_fds[0],
                                (unsigned char *)to + done + got, (size_t)put - got);

            if (step < 0 && errno == EINTR)
                continue;
            if (step <= 0)
                break;
            got += (size_t)step;
        }
        if (put <= 0 || got != (size_t)put || (size_t)put != want) {
            harmony_fault_watch_pipe_close();
            return -1;
        }
        done += want;
    }
    return 0;
}

static int harmony_fault_watch_read(void *to, uintptr_t from, size_t size)
{
    if (__atomic_load_n(&harmony_fault_watch.self_read_refused, __ATOMIC_RELAXED) == 0) {
        struct iovec local = {to, size};
        struct iovec remote = {(void *)from, size};
        ssize_t got = process_vm_readv(getpid(), &local, 1, &remote, 1, 0);

        if (got == (ssize_t)size)
            return 0;
        if (got < 0 && errno != EFAULT)
            __atomic_store_n(&harmony_fault_watch.self_read_refused, 1, __ATOMIC_RELAXED);
    }
    return harmony_fault_watch_pipe_read(to, from, size);
}

static void harmony_fault_watch_add_range(const struct harmony_fault_watch_range *found)
{
    struct harmony_fault_watch_range *range;

    if (memcmp(found->perms, "rw-s", sizeof(found->perms)) != 0 ||
        harmony_fault_watch.range_count == HARMONY_FAULT_WATCH_RANGE_LIMIT ||
        found->size > HARMONY_FAULT_WATCH_COPY_LIMIT - harmony_fault_watch.copied)
        return;
    if (harmony_fault_watch_read(harmony_fault_watch.copy + harmony_fault_watch.copied,
                                 found->start, found->size) != 0)
        return;
    range = &harmony_fault_watch.ranges[harmony_fault_watch.range_count++];
    *range = *found;
    range->copy_at = harmony_fault_watch.copied;
    harmony_fault_watch.copied += found->size;
}

static int harmony_fault_watch_snapshot(void)
{
    int expected = 0;
    struct harmony_fault_watch_maps maps;
    struct harmony_fault_watch_range found;

    if (harmony_fault_watch_owned())
        harmony_fault_watch_stop();
    if (!__atomic_compare_exchange_n(&harmony_fault_watch.claimed, &expected, 1, 0,
                                     __ATOMIC_ACQUIRE, __ATOMIC_RELAXED))
        return 0;
    harmony_fault_watch.range_count = 0;
    harmony_fault_watch.copied = 0;
    if (harmony_fault_watch.page_size == 0) {
        long size = sysconf(_SC_PAGESIZE);

        if (size > 0)
            harmony_fault_watch.page_size = (uintptr_t)size;
    }
    if (harmony_fault_watch.page_size == 0 || harmony_fault_watch_maps_open(&maps) != 0) {
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return 0;
    }
    while (harmony_fault_watch_maps_next(&maps, &found))
        harmony_fault_watch_add_range(&found);
    (void)close(maps.fd);
    harmony_fault_watch_pipe_close();
    if (harmony_fault_watch.range_count == 0) {
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return 0;
    }
    return 1;
}

static int harmony_fault_watch_changed_at(uintptr_t address, size_t width)
{
    size_t index;

    for (index = 0; index < harmony_fault_watch.range_count; index++) {
        const struct harmony_fault_watch_range *range =
            &harmony_fault_watch.ranges[index];
        size_t offset;

        if (address < range->start || address >= range->start + range->size)
            continue;
        for (offset = (size_t)(address - range->start);
             offset < range->size && width > 0; offset++, width--) {
            size_t bit = range->copy_at + offset;

            if ((harmony_fault_watch.changed[bit / 8] >> (bit % 8)) & 1u)
                return 1;
        }
        return 0;
    }
    return 0;
}

static void harmony_fault_watch_access(const void *context, int *write,
                                       size_t *width)
{
    const ucontext_t *ucontext = context;

    *write = 0;
    *width = HARMONY_FAULT_WATCH_UNKNOWN_WIDTH;
#if defined(__aarch64__)
    {
        const uint32_t esr_magic = UINT32_C(0x45535201);
        const unsigned char *cursor =
            (const unsigned char *)&ucontext->uc_mcontext.__reserved;
        const unsigned char *end =
            cursor + sizeof(ucontext->uc_mcontext.__reserved);

        while (cursor + 16 <= end) {
            uint32_t magic;
            uint32_t size;
            uint64_t esr;
            uint64_t class_bits;

            memcpy(&magic, cursor, sizeof(magic));
            memcpy(&size, cursor + 4, sizeof(size));
            if (magic == 0 || size < 8 || size > (size_t)(end - cursor))
                return;
            if (magic != esr_magic) {
                cursor += size;
                continue;
            }
            memcpy(&esr, cursor + 8, sizeof(esr));
            class_bits = (esr >> 26) & UINT64_C(0x3f);
            if (class_bits == UINT64_C(0x24) || class_bits == UINT64_C(0x25)) {
                *write = (int)((esr >> 6) & 1u);
                if (((esr >> 24) & 1u) != 0)
                    *width = (size_t)1 << ((esr >> 22) & 3u);
            }
            return;
        }
    }
#else
    *write = (ucontext->uc_mcontext.gregs[REG_ERR] & 2) != 0;
#endif
}

static void harmony_fault_watch_forward(int signo, siginfo_t *info,
                                        void *context)
{
    struct sigaction previous = harmony_fault_watch.previous;
    sigset_t mask;

    if (previous.sa_handler != SIG_DFL && previous.sa_handler != SIG_IGN) {
        if ((previous.sa_flags & SA_NODEFER) == 0)
            (void)sigaddset(&previous.sa_mask, signo);
        (void)pthread_sigmask(SIG_BLOCK, &previous.sa_mask, &mask);
        if ((previous.sa_flags & SA_SIGINFO) != 0)
            previous.sa_sigaction(signo, info, context);
        else
            previous.sa_handler(signo);
        (void)pthread_sigmask(SIG_SETMASK, &mask, NULL);
        return;
    }
    harmony_fault_watch_restore();
    (void)sigaction(SIGSEGV, &previous, NULL);
    if (info->si_code <= 0)
        (void)raise(signo);
}

static void harmony_fault_watch_fault(int signo, siginfo_t *info,
                                      void *context)
{
    int saved_errno = errno;
    uintptr_t address = (uintptr_t)info->si_addr;
    uintptr_t page = address & ~(harmony_fault_watch.page_size - 1);
    size_t index;

    for (index = 0; index < harmony_fault_watch.page_count; index++) {
        unsigned char ours = 0;

        if (harmony_fault_watch.pages[index] != page)
            continue;
        if (info->si_code == SEGV_ACCERR &&
            __atomic_load_n(&harmony_fault_watch.released[index], __ATOMIC_ACQUIRE) == 0)
            harmony_fault_watch_check_pages(index, index + 1, 1, 1, &ours);
        if (ours == 0 || mprotect((void *)page, (size_t)harmony_fault_watch.page_size,
                                  PROT_READ | PROT_WRITE) != 0) {
            __atomic_store_n(&harmony_fault_watch.released[index], 1, __ATOMIC_RELEASE);
            break;
        }
        if (harmony_fault_watch_owned()) {
            int write;
            size_t width;

            harmony_fault_watch_access(context, &write, &width);
            if (write == 0 && harmony_fault_watch_changed_at(address, width))
                __atomic_store_n(&harmony_fault_watch.read_changed, 1,
                                 __ATOMIC_RELAXED);
            if (harmony_fault_watch.pending_count <
                HARMONY_FAULT_WATCH_PENDING_LIMIT)
                harmony_fault_watch
                    .pending[harmony_fault_watch.pending_count++] = index;
        } else {
            __atomic_store_n(&harmony_fault_watch.released[index], 1,
                             __ATOMIC_RELEASE);
        }
        errno = saved_errno;
        return;
    }
    errno = saved_errno;
    harmony_fault_watch_forward(signo, info, context);
}

static int harmony_fault_watch_mark_page(size_t index)
{
    const struct harmony_fault_watch_range *range =
        &harmony_fault_watch.ranges[harmony_fault_watch.page_range[index]];
    size_t page = (size_t)(harmony_fault_watch.pages[index] - range->start);
    const unsigned char *then = harmony_fault_watch.copy + range->copy_at;
    const unsigned char *now = harmony_fault_watch.now;
    size_t chunk;
    int changed = 0;

    for (chunk = page; chunk < page + harmony_fault_watch.page_size;
         chunk += HARMONY_FAULT_WATCH_CHUNK) {
        size_t offset;

        if (harmony_fault_watch_read(harmony_fault_watch.now, range->start + chunk,
                                     HARMONY_FAULT_WATCH_CHUNK) != 0) {
            memset(harmony_fault_watch.changed + (range->copy_at + page) / 8, 0,
                   harmony_fault_watch.page_size / 8);
            return 0;
        }
        if (memcmp(now, then + chunk, HARMONY_FAULT_WATCH_CHUNK) == 0)
            continue;
        for (offset = 0; offset < HARMONY_FAULT_WATCH_CHUNK; offset++) {
            size_t bit = range->copy_at + chunk + offset;

            if (now[offset] == then[chunk + offset])
                continue;
            harmony_fault_watch.changed[bit / 8] = (unsigned char)(
                harmony_fault_watch.changed[bit / 8] | (1u << (bit % 8)));
            changed = 1;
        }
    }
    return changed;
}

static void harmony_fault_watch_mark_changes(void)
{
    unsigned char ours[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    size_t count = 0;
    size_t index;
    size_t page;

    memset(harmony_fault_watch.changed, 0, (harmony_fault_watch.copied + 7) / 8);
    for (index = 0; index < harmony_fault_watch.range_count; index++)
        for (page = 0; page < harmony_fault_watch.ranges[index].size &&
                       count < HARMONY_FAULT_WATCH_PAGE_LIMIT;
             page += harmony_fault_watch.page_size) {
            harmony_fault_watch.page_range[count] = (unsigned char)index;
            harmony_fault_watch.pages[count++] = harmony_fault_watch.ranges[index].start + page;
        }
    harmony_fault_watch_check_pages(0, count, 0, 0, ours);
    harmony_fault_watch.page_count = 0;
    for (index = 0; index < count; index++) {
        if (ours[index] == 0 || harmony_fault_watch_mark_page(index) == 0)
            continue;
        harmony_fault_watch.page_range[harmony_fault_watch.page_count] =
            harmony_fault_watch.page_range[index];
        harmony_fault_watch.pages[harmony_fault_watch.page_count++] =
            harmony_fault_watch.pages[index];
    }
    harmony_fault_watch_pipe_close();
}

static void harmony_fault_watch_start(uint64_t site)
{
    struct sigaction action;
    size_t index;

    harmony_fault_watch_mark_changes();
    if (harmony_fault_watch.page_count == 0) {
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return;
    }
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = harmony_fault_watch_fault;
    action.sa_flags = SA_SIGINFO | SA_NODEFER | SA_RESTART | SA_ONSTACK;
    (void)sigemptyset(&action.sa_mask);
    if (sigaction(SIGSEGV, &action, &harmony_fault_watch.previous) != 0) {
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return;
    }
    __atomic_store_n(&harmony_fault_watch.installed, 1, __ATOMIC_RELEASE);
    harmony_fault_watch.owner = pthread_self();
    harmony_fault_watch.site = site;
    harmony_fault_watch.edges = 0;
    harmony_fault_watch.pending_count = 0;
    memset(harmony_fault_watch.released, 0, harmony_fault_watch.page_count);
    __atomic_store_n(&harmony_fault_watch.read_changed, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&harmony_fault_watch.active, 1, __ATOMIC_RELEASE);
    for (index = 0; index < harmony_fault_watch.page_count; index++)
        if (mprotect((void *)harmony_fault_watch.pages[index],
                     (size_t)harmony_fault_watch.page_size, PROT_NONE) != 0)
            __atomic_store_n(&harmony_fault_watch.released[index], 1, __ATOMIC_RELEASE);
}

static void harmony_fault_watch_edge(void)
{
    unsigned char ours[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    size_t index;

    if (!harmony_fault_watch_owned())
        return;
    harmony_fault_watch.edges++;
    if (__atomic_load_n(&harmony_fault_watch.read_changed, __ATOMIC_RELAXED) != 0) {
        uint64_t site = harmony_fault_watch.site;
        uint64_t edges = harmony_fault_watch.edges;

        harmony_fault_watch_stop();
        harmony_fault_park_read_report(site, edges);
        return;
    }
    if (harmony_fault_watch.edges >= HARMONY_FAULT_WATCH_EDGES) {
        harmony_fault_watch_stop();
        return;
    }
    if (harmony_fault_watch.pending_count != 0)
        harmony_fault_watch_check_pages(0, harmony_fault_watch.page_count, 1, 0, ours);
    for (index = 0; index < harmony_fault_watch.pending_count; index++) {
        size_t page = harmony_fault_watch.pending[index];

        if (__atomic_load_n(&harmony_fault_watch.released[page], __ATOMIC_ACQUIRE) != 0)
            continue;
        if (ours[page] == 0 ||
            mprotect((void *)harmony_fault_watch.pages[page],
                     (size_t)harmony_fault_watch.page_size, PROT_NONE) != 0)
            __atomic_store_n(&harmony_fault_watch.released[page], 1, __ATOMIC_RELEASE);
    }
    harmony_fault_watch.pending_count = 0;
}
#else
static int harmony_fault_watch_snapshot(void)
{
    return 0;
}

static void harmony_fault_watch_start(uint64_t site)
{
    (void)site;
}

static void harmony_fault_watch_edge(void)
{
}
#endif

static void harmony_fault_after_fork(void)
{
    harmony_fault_events.new_sites = NULL;
    harmony_fault_events.new_site_count = 0;
    harmony_fault_events.new_site_capacity = 0;
    harmony_fault_events.new_sites_off = 1;
    if (harmony_fault_events.page != NULL) {
        harmony_fault_events.page = NULL;
        (void)fcntl(harmony_fault_events.page_fd, F_SETFD, FD_CLOEXEC);
        harmony_fault_page_park_clear();
    }
#if HARMONY_FAULT_WATCH_SUPPORTED
    harmony_fault_watch_stop();
    harmony_fault_watch_pipe_close();
#endif
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
    uint64_t park_edges = 0;
    uint32_t kill_claimed = 0;
    uint32_t park_claimed = 0;
    int page_settled;
    uint8_t crossed;

    if (harmony_fault_held != 0)
        return;
    harmony_fault_watch_edge();
    if (pthread_once(&harmony_fault_event_once, harmony_fault_event_init) != 0)
        return;
    if (harmony_fault_lock() != 0)
        return;
    if (harmony_fault_events.initialized == 0) {
        harmony_fault_unlock();
        return;
    }
    page_settled = harmony_fault_page_adopt() == 0;
    if (harmony_fault_events.coverage_callbacks != UINT64_MAX)
        harmony_fault_events.coverage_callbacks++;
    before = harmony_fault_event_site_before(site, &crossed);
    if (harmony_fault_events.kill_armed != 0 &&
        harmony_fault_event_rarity_allows(before,
                                          harmony_fault_events.kill_rarity)) {
        harmony_fault_events.kill_armed = 0;
        kill_claimed = 1;
        kill_rarity = harmony_fault_events.kill_rarity;
        kill_site = site;
        kill_report_fd = harmony_fault_events.report_fd;
    } else if (page_settled && harmony_fault_events.park_armed != 0 &&
               harmony_fault_park_counts(site) &&
               harmony_fault_park_spend(before)) {
        harmony_fault_events.park_armed = 0;
        park_edges = harmony_fault_events.park_edges;
        if (harmony_fault_events.park_fires != UINT64_MAX)
            harmony_fault_events.park_fires++;
        if (harmony_fault_events.park_paged != 0)
            harmony_fault_events.page_armed_fired = 1;
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
            harmony_fault_unlock();
            return;
        }
        (void)HARMONY_KILL(0, SIGKILL);
        harmony_fault_unlock();
        return;
    }
    harmony_fault_unlock();
    if (crossed != 0)
        harmony_fault_event_note_crossing(site, crossed);
    if (park_claimed != 0) {
        int watching;

        harmony_fault_park_report(site, park_edges);
        watching = harmony_fault_watch_snapshot();
        harmony_fault_event_sleep(park_hold_nanos);
        if (watching != 0)
            harmony_fault_watch_start(site);
        if (harmony_fault_lock() == 0) {
            harmony_fault_events.park_inflight--;
            if (harmony_fault_events.initialized != 0 &&
                harmony_fault_events.park_inflight == 0 &&
                harmony_fault_events.park_armed == 0 &&
                harmony_fault_events.park_hold_nanos != 0) {
                harmony_fault_events.park_weight_left =
                    harmony_fault_events.park_edges
                    << HARMONY_FAULT_PARK_WEIGHT_SHIFT;
                harmony_fault_events.park_armed = 1;
            }
            harmony_fault_unlock();
        }
    }
}
