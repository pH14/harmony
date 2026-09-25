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
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

#if defined(__linux__)
#include <link.h>
#endif

#if defined(__linux__) && (defined(__aarch64__) || defined(__x86_64__))
#define HARMONY_FAULT_WATCH_SUPPORTED 1
#include <sys/mman.h>
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

#define HARMONY_FAULT_MODULE_LIMIT 4096
#define HARMONY_FAULT_PARK_WEIGHT_SHIFT 20
#define HARMONY_FAULT_WATCH_EDGES UINT64_C(50)
#define HARMONY_FAULT_WATCH_RANGE_LIMIT 16
#define HARMONY_FAULT_WATCH_COPY_LIMIT ((size_t)1 << 20)
#define HARMONY_FAULT_WATCH_PAGE_LIMIT 256
#define HARMONY_FAULT_WATCH_PENDING_LIMIT 4
#define HARMONY_FAULT_WATCH_MAPS_LIMIT ((size_t)1 << 16)
#define HARMONY_FAULT_WATCH_UNKNOWN_WIDTH 16

#if HARMONY_FAULT_WATCH_SUPPORTED
static void harmony_fault_watch_after_fork(void);
#endif

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
    uint64_t park_edges;
    uint64_t park_weight_left;
    uint64_t park_hold_nanos;
    uint64_t park_fires;
    uint64_t park_inflight;
    uint32_t initialized;
    uint64_t coverage_crossings;
    uint64_t coverage_digest;
    struct harmony_fault_crossing *deferred;
    size_t deferred_count;
    size_t deferred_capacity;
    uint64_t site_visits[HARMONY_FAULT_EVENT_SITE_TABLE_SIZE];
    uint8_t site_bucket[HARMONY_FAULT_EVENT_SITE_TABLE_SIZE];
};

static struct harmony_fault_event_state harmony_fault_events = {
    .lock = PTHREAD_MUTEX_INITIALIZER,
    .control_fd = -1,
    .report_fd = -1
};
static pthread_once_t harmony_fault_event_once = PTHREAD_ONCE_INIT;
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
    uint64_t weight = (UINT64_C(1) << HARMONY_FAULT_PARK_WEIGHT_SHIFT) / visits;

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

static void harmony_fault_crossings_resolve(void)
{
    size_t index;

    for (index = 0; index < harmony_fault_events.deferred_count; index++) {
        const struct harmony_fault_crossing *crossing =
            &harmony_fault_events.deferred[index];
        harmony_fault_events.coverage_digest += harmony_fault_crossing_hash(
            harmony_fault_site_offset(crossing->site), crossing->bucket);
    }
    harmony_fault_events.deferred_count = 0;
}

static void harmony_fault_event_note_crossing(uint64_t site, uint8_t bucket)
{
    uint64_t offset;
    int resolved = harmony_fault_site_lookup(site, &offset);

    if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
        return;
    harmony_fault_events.coverage_crossings++;
    if (resolved != 0)
        harmony_fault_events.coverage_digest +=
            harmony_fault_crossing_hash(offset, bucket);
    else if (harmony_fault_crossing_defer(site, bucket) != 0)
        harmony_fault_events.coverage_digest +=
            harmony_fault_crossing_hash(site, bucket);
    (void)pthread_mutex_unlock(&harmony_fault_events.lock);
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
        int valid = 1;

        if (read_all(fd, request, sizeof(request)) != 0)
            break;
        harmony_fault_modules_refresh();
        kind = get_u64(request);
        first = get_u64(request + 8);
        second = get_u64(request + 16);
        memcpy(response, request, sizeof(response));
        if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
            break;
        harmony_fault_crossings_resolve();
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
                (first == 0 || first > HARMONY_FAULT_EVENT_PARK_EDGE_LIMIT)) {
                valid = 0;
            } else if (second == 0) {
                harmony_fault_events.park_armed = 0;
                harmony_fault_events.park_hold_nanos = 0;
            } else {
                harmony_fault_events.park_edges = first;
                harmony_fault_events.park_weight_left =
                    first << HARMONY_FAULT_PARK_WEIGHT_SHIFT;
                harmony_fault_events.park_hold_nanos = second;
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
                             : 0));
        } else if (kind == HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS) {
            memset(response, 0, sizeof(response));
            put_u64(response, HARMONY_FAULT_EVENT_CMD_COVERAGE_STATUS);
            put_u64(response + 8, harmony_fault_events.coverage_crossings);
            put_u64(response + 16, harmony_fault_events.coverage_digest);
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
#if HARMONY_FAULT_WATCH_SUPPORTED
    (void)pthread_atfork(NULL, NULL, harmony_fault_watch_after_fork);
#endif

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
    struct sigaction previous;
    struct harmony_fault_watch_range ranges[HARMONY_FAULT_WATCH_RANGE_LIMIT];
    uintptr_t pages[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    unsigned char released[HARMONY_FAULT_WATCH_PAGE_LIMIT];
    size_t pending[HARMONY_FAULT_WATCH_PENDING_LIMIT];
    char maps[HARMONY_FAULT_WATCH_MAPS_LIMIT];
    unsigned char copy[HARMONY_FAULT_WATCH_COPY_LIMIT];
    unsigned char changed[HARMONY_FAULT_WATCH_COPY_LIMIT / 8];
} harmony_fault_watch;

static void harmony_fault_watch_protect_all(int protection)
{
    size_t index;

    for (index = 0; index < harmony_fault_watch.page_count; index++)
        (void)mprotect((void *)harmony_fault_watch.pages[index],
                       (size_t)harmony_fault_watch.page_size, protection);
}

static void harmony_fault_watch_stop(void)
{
    if (__atomic_exchange_n(&harmony_fault_watch.active, 0, __ATOMIC_ACQ_REL) != 0)
        harmony_fault_watch_protect_all(PROT_READ | PROT_WRITE);
    if (__atomic_exchange_n(&harmony_fault_watch.installed, 0, __ATOMIC_ACQ_REL) != 0)
        (void)sigaction(SIGSEGV, &harmony_fault_watch.previous, NULL);
    __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
}

static void harmony_fault_watch_after_fork(void)
{
    harmony_fault_watch_stop();
}

static int harmony_fault_watch_owned(void)
{
    return __atomic_load_n(&harmony_fault_watch.active, __ATOMIC_ACQUIRE) != 0 &&
           pthread_equal(harmony_fault_watch.owner, pthread_self()) != 0;
}

static void harmony_fault_watch_add_range(uintptr_t start, uintptr_t end)
{
    struct harmony_fault_watch_range *range;
    size_t size = (size_t)(end - start);

    if (harmony_fault_watch.range_count == HARMONY_FAULT_WATCH_RANGE_LIMIT ||
        size > HARMONY_FAULT_WATCH_COPY_LIMIT - harmony_fault_watch.copied)
        return;
    range = &harmony_fault_watch.ranges[harmony_fault_watch.range_count++];
    range->start = start;
    range->size = size;
    range->copy_at = harmony_fault_watch.copied;
    memcpy(harmony_fault_watch.copy + range->copy_at, (const void *)start, size);
    harmony_fault_watch.copied += size;
}

static int harmony_fault_watch_snapshot(void)
{
    int expected = 0;
    size_t used = 0;
    char *line;
    int fd;

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
    fd = open("/proc/self/maps", O_RDONLY | O_CLOEXEC);
    if (harmony_fault_watch.page_size == 0 || fd < 0) {
        if (fd >= 0)
            (void)close(fd);
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return 0;
    }
    while (used + 1 < sizeof(harmony_fault_watch.maps)) {
        ssize_t got = read(fd, harmony_fault_watch.maps + used,
                           sizeof(harmony_fault_watch.maps) - 1 - used);

        if (got < 0 && errno == EINTR)
            continue;
        if (got <= 0)
            break;
        used += (size_t)got;
    }
    (void)close(fd);
    harmony_fault_watch.maps[used] = '\0';
    line = harmony_fault_watch.maps;
    for (;;) {
        char *next = strchr(line, '\n');
        unsigned long start = 0;
        unsigned long end = 0;
        char perms[5];

        if (next == NULL)
            break;
        *next = '\0';
        if (sscanf(line, "%lx-%lx %4s", &start, &end, perms) == 3 &&
            strcmp(perms, "rw-s") == 0 && end > start)
            harmony_fault_watch_add_range((uintptr_t)start, (uintptr_t)end);
        line = next + 1;
    }
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

    if ((previous.sa_flags & SA_SIGINFO) != 0 && previous.sa_sigaction != NULL) {
        previous.sa_sigaction(signo, info, context);
        return;
    }
    if (previous.sa_handler != SIG_DFL && previous.sa_handler != SIG_IGN &&
        previous.sa_handler != NULL) {
        previous.sa_handler(signo);
        return;
    }
    harmony_fault_watch_protect_all(PROT_READ | PROT_WRITE);
    (void)sigaction(SIGSEGV, &previous, NULL);
}

static void harmony_fault_watch_fault(int signo, siginfo_t *info,
                                      void *context)
{
    uintptr_t address = (uintptr_t)info->si_addr;
    uintptr_t page = address & ~(harmony_fault_watch.page_size - 1);
    size_t index;

    for (index = 0; index < harmony_fault_watch.page_count; index++) {
        if (harmony_fault_watch.pages[index] != page)
            continue;
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
        (void)mprotect((void *)page, (size_t)harmony_fault_watch.page_size,
                       PROT_READ | PROT_WRITE);
        return;
    }
    harmony_fault_watch_forward(signo, info, context);
}

static void harmony_fault_watch_mark_changes(void)
{
    size_t index;

    memset(harmony_fault_watch.changed, 0, (harmony_fault_watch.copied + 7) / 8);
    harmony_fault_watch.page_count = 0;
    for (index = 0; index < harmony_fault_watch.range_count; index++) {
        const struct harmony_fault_watch_range *range =
            &harmony_fault_watch.ranges[index];
        const unsigned char *now = (const unsigned char *)range->start;
        const unsigned char *then = harmony_fault_watch.copy + range->copy_at;
        size_t page;

        for (page = 0; page < range->size; page += harmony_fault_watch.page_size) {
            size_t offset;
            int changed = 0;

            if (memcmp(now + page, then + page, harmony_fault_watch.page_size) == 0)
                continue;
            for (offset = page; offset < page + harmony_fault_watch.page_size;
                 offset++) {
                size_t bit = range->copy_at + offset;

                if (now[offset] == then[offset])
                    continue;
                harmony_fault_watch.changed[bit / 8] = (unsigned char)(
                    harmony_fault_watch.changed[bit / 8] | (1u << (bit % 8)));
                changed = 1;
            }
            if (changed != 0 &&
                harmony_fault_watch.page_count < HARMONY_FAULT_WATCH_PAGE_LIMIT)
                harmony_fault_watch.pages[harmony_fault_watch.page_count++] =
                    range->start + page;
        }
    }
}

static void harmony_fault_watch_start(uint64_t site)
{
    struct sigaction action;

    harmony_fault_watch_mark_changes();
    if (harmony_fault_watch.page_count == 0) {
        __atomic_store_n(&harmony_fault_watch.claimed, 0, __ATOMIC_RELEASE);
        return;
    }
    memset(&action, 0, sizeof(action));
    action.sa_sigaction = harmony_fault_watch_fault;
    action.sa_flags = SA_SIGINFO | SA_NODEFER | SA_RESTART;
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
    harmony_fault_watch_protect_all(PROT_NONE);
}

static void harmony_fault_watch_edge(void)
{
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
    for (index = 0; index < harmony_fault_watch.pending_count; index++) {
        size_t page = harmony_fault_watch.pending[index];

        if (__atomic_load_n(&harmony_fault_watch.released[page],
                            __ATOMIC_ACQUIRE) == 0)
            (void)mprotect((void *)harmony_fault_watch.pages[page],
                           (size_t)harmony_fault_watch.page_size, PROT_NONE);
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
    uint8_t crossed;

    harmony_fault_watch_edge();
    if (pthread_once(&harmony_fault_event_once, harmony_fault_event_init) != 0)
        return;
    if (pthread_mutex_lock(&harmony_fault_events.lock) != 0)
        return;
    if (harmony_fault_events.initialized == 0) {
        (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        return;
    }
    before = harmony_fault_event_site_before(site, &crossed);
    if (harmony_fault_events.kill_armed != 0 &&
        harmony_fault_event_rarity_allows(before,
                                          harmony_fault_events.kill_rarity)) {
        harmony_fault_events.kill_armed = 0;
        kill_claimed = 1;
        kill_rarity = harmony_fault_events.kill_rarity;
        kill_site = site;
        kill_report_fd = harmony_fault_events.report_fd;
    } else if (harmony_fault_events.park_armed != 0 &&
               harmony_fault_park_spend(before)) {
        harmony_fault_events.park_armed = 0;
        park_edges = harmony_fault_events.park_edges;
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
    if (crossed != 0)
        harmony_fault_event_note_crossing(site, crossed);
    if (park_claimed != 0) {
        int watching;

        harmony_fault_park_report(site, park_edges);
        watching = harmony_fault_watch_snapshot();
        harmony_fault_event_sleep(park_hold_nanos);
        if (watching != 0)
            harmony_fault_watch_start(site);
        if (pthread_mutex_lock(&harmony_fault_events.lock) == 0) {
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
            (void)pthread_mutex_unlock(&harmony_fault_events.lock);
        }
    }
}
