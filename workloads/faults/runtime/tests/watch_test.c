// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <setjmp.h>
#include <stddef.h>
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
#if defined(__linux__)
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/uio.h>
#endif

static volatile unsigned char *shared;
static volatile unsigned char *hold_target;
static size_t hold_write_at;
static unsigned char hold_write_value;
static void (*hold_action)(void);

static int writing_sleep(const struct timespec *request, struct timespec *remaining);
#define HARMONY_NANOSLEEP(request, remaining) writing_sleep((request), (remaining))

static char json_report[128];
static size_t json_reports;
static void record_json(const char *data, size_t size);
#define HARMONY_JSON(data, size) record_json((data), (size))
#define HARMONY_SITE_JSON(data, size) ((void)(data), (void)(size))
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
    (hold_target != NULL ? hold_target : shared)[hold_write_at] = hold_write_value;
    if (hold_action != NULL)
        hold_action();
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

static size_t page_bytes;
static unsigned char *gone;
static unsigned char *replaced;
static unsigned char *shrunk;
static unsigned char *truncated;
static int truncated_fd;

static int recorded(const volatile void *start)
{
    size_t index;

    for (index = 0; index < harmony_fault_watch.range_count; index++)
        if (harmony_fault_watch.ranges[index].start == (uintptr_t)start)
            return 1;
    return 0;
}

static int watched(const volatile void *page)
{
    size_t index;

    for (index = 0; index < harmony_fault_watch.page_count; index++)
        if (harmony_fault_watch.pages[index] == (uintptr_t)page)
            return 1;
    return 0;
}

static unsigned char *shared_pages(size_t count)
{
    unsigned char *pages = mmap(NULL, count * page_bytes, PROT_READ | PROT_WRITE,
                                MAP_SHARED | MAP_ANONYMOUS, -1, 0);

    assert(pages != MAP_FAILED);
    memset(pages, 1, count * page_bytes);
    return pages;
}

static void change_mappings(void)
{
    assert(recorded(gone) && recorded(replaced) && recorded(shrunk) && recorded(truncated));
    gone[0] = 2;
    replaced[0] = 2;
    shrunk[0] = 2;
    shrunk[page_bytes] = 2;
    truncated[0] = 2;
    assert(munmap(gone, 2 * page_bytes) == 0);
    assert(mmap(replaced, page_bytes, PROT_READ | PROT_WRITE,
                MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0) == replaced);
    assert(munmap(replaced + page_bytes, page_bytes) == 0);
    assert(munmap(shrunk + page_bytes, page_bytes) == 0);
    assert(ftruncate(truncated_fd, 0) == 0);
}

static void hold_over_changed_mappings(size_t offset, unsigned char value)
{
    uint64_t site;

    gone = shared_pages(2);
    replaced = shared_pages(2);
    shrunk = shared_pages(2);
    truncated_fd = memfd_create("watch-test", MFD_CLOEXEC);
    assert(truncated_fd >= 0);
    assert(ftruncate(truncated_fd, (off_t)(2 * page_bytes)) == 0);
    truncated = mmap(NULL, 2 * page_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, truncated_fd, 0);
    assert(truncated != MAP_FAILED);
    memset(truncated, 1, 2 * page_bytes);
    hold_action = change_mappings;
    site = land(offset, value);
    hold_action = NULL;
    assert(!watched(gone) && !watched(replaced) && !watched(truncated));
    assert(watched(shrunk) && !watched(shrunk + page_bytes));
    assert(watched(shared + (offset & ~(page_bytes - 1))));
    assert(shared[offset] == value);
    edges(1);
    assert_read_report(site, 1);
    assert(shrunk[0] == 2);
    assert(munmap(replaced, page_bytes) == 0);
    assert(munmap(shrunk, page_bytes) == 0);
    assert(munmap(truncated, 2 * page_bytes) == 0);
    assert(close(truncated_fd) == 0);
}

static unsigned char *neighbour;
static int neighbour_fd;

static void write_neighbour(void)
{
    neighbour[page_bytes] = 3;
}

static void *land_on_pair(unsigned char *pair)
{
    hold_target = pair;
    neighbour = pair;
    hold_action = write_neighbour;
    (void)land(8, 3);
    hold_action = NULL;
    hold_target = NULL;
    assert(watched(pair) && watched(pair + page_bytes));
    return pair;
}

static void touch_after_neighbour_changed(unsigned char *pair)
{
    sig_atomic_t faults = foreign_faults;
    size_t reports = json_reports;

    assert(((volatile unsigned char *)pair)[8] == 3);
    assert(foreign_faults == faults);
    edges(1);
    assert(json_reports == reports + 1);
    assert(strstr(json_report, "{\"harmony_park_read\":") == json_report);
}

static void change_neighbour_during_page_watch(void)
{
    unsigned char *pair = land_on_pair(shared_pages(2));
    unsigned char *grown;

    assert(munmap(pair + page_bytes, page_bytes) == 0);
    touch_after_neighbour_changed(pair);
    assert(munmap(pair, page_bytes) == 0);

    neighbour_fd = memfd_create("watch-neighbour", MFD_CLOEXEC);
    assert(neighbour_fd >= 0);
    assert(ftruncate(neighbour_fd, (off_t)(3 * page_bytes)) == 0);
    grown = mmap(NULL, 3 * page_bytes, PROT_READ | PROT_WRITE, MAP_SHARED, neighbour_fd, 0);
    assert(grown != MAP_FAILED);
    assert(munmap(grown + 2 * page_bytes, page_bytes) == 0);
    memset(grown, 1, 2 * page_bytes);
    (void)land_on_pair(grown);
    assert(mmap(grown + 2 * page_bytes, page_bytes, PROT_READ | PROT_WRITE,
                MAP_SHARED | MAP_FIXED, neighbour_fd, (off_t)(2 * page_bytes)) ==
           grown + 2 * page_bytes);
    touch_after_neighbour_changed(grown);
    assert(grown[page_bytes] == 3);
    assert(munmap(grown, 3 * page_bytes) == 0);
    assert(close(neighbour_fd) == 0);
}

static void unmap_during_page_watch(void)
{
    unsigned char *watched_page = shared_pages(1);
    sig_atomic_t faults = foreign_faults;

    hold_target = watched_page;
    (void)land(8, 3);
    hold_target = NULL;
    assert(munmap(watched_page, page_bytes) == 0);
    if (sigsetjmp(foreign_return, 1) == 0)
        (void)*(volatile unsigned char *)watched_page;
    assert(foreign_faults == faults + 1);
    edges(HARMONY_FAULT_WATCH_EDGES);
}

static void replace_during_page_watch(void)
{
    unsigned char *watched_page = shared_pages(1);
    sig_atomic_t faults = foreign_faults;

    hold_target = watched_page;
    (void)land(8, 3);
    hold_target = NULL;
    assert(((volatile unsigned char *)watched_page)[100] == 1);
    assert(mmap(watched_page, page_bytes, PROT_READ, MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED,
                -1, 0) == watched_page);
    edges(1);
    assert(harmony_fault_watch.active == 1);
    assert(*(volatile unsigned char *)watched_page == 0);
    assert(foreign_faults == faults);
    if (sigsetjmp(foreign_return, 1) == 0)
        *(volatile unsigned char *)watched_page = 1;
    assert(foreign_faults == faults + 1);
    assert(harmony_fault_watch.active == 1);
    edges(HARMONY_FAULT_WATCH_EDGES);
    assert(munmap(watched_page, page_bytes) == 0);
}

static void restricted_page_during_page_watch(void)
{
    unsigned char *watched_page = shared_pages(1);
    sig_atomic_t faults = foreign_faults;

    hold_target = watched_page;
    (void)land(8, 3);
    hold_target = NULL;
    assert(mprotect(watched_page, page_bytes, PROT_READ) == 0);
    if (sigsetjmp(foreign_return, 1) == 0)
        *(volatile unsigned char *)watched_page = 1;
    assert(foreign_faults == faults + 1);
    edges(HARMONY_FAULT_WATCH_EDGES);
    if (sigsetjmp(foreign_return, 1) == 0)
        *(volatile unsigned char *)watched_page = 1;
    assert(foreign_faults == faults + 2);
    assert(munmap(watched_page, page_bytes) == 0);
}

static void sent_signal_ends_process(void)
{
    pid_t child = fork();
    int status;

    assert(child >= 0);
    if (child == 0) {
        struct sigaction action;

        memset(&action, 0, sizeof(action));
        action.sa_sigaction = harmony_fault_watch_fault;
        action.sa_flags = SA_SIGINFO | SA_NODEFER;
        (void)sigemptyset(&action.sa_mask);
        harmony_fault_watch.page_count = 0;
        if (sigaction(SIGSEGV, &action, &harmony_fault_watch.previous) != 0)
            _exit(2);
        (void)kill(getpid(), SIGSEGV);
        _exit(0);
    }
    assert(waitpid(child, &status, 0) == child);
    assert(WIFSIGNALED(status) && WTERMSIG(status) == SIGSEGV);
}

static void fork_during_pipe_read(void)
{
    unsigned char got = 0;
    unsigned char from = 9;
    int read_end;
    pid_t child;
    int status;

    assert(harmony_fault_watch_pipe_read(&got, (uintptr_t)&from, 1) == 0 && got == 9);
    assert(harmony_fault_watch.pipe_open == 1);
    read_end = harmony_fault_watch.pipe_fds[0];
    child = fork();
    assert(child >= 0);
    if (child == 0)
        _exit(harmony_fault_watch.pipe_open == 0 && fcntl(read_end, F_GETFD) == -1 ? 0 : 1);
    assert(waitpid(child, &status, 0) == child);
    assert(WIFEXITED(status) && WEXITSTATUS(status) == 0);
    assert(harmony_fault_watch.pipe_open == 1 && fcntl(read_end, F_GETFD) >= 0);
    harmony_fault_watch_pipe_close();
}

static void read_mapped_and_unmapped(void)
{
    unsigned char got[HARMONY_FAULT_WATCH_CHUNK];
    unsigned char *pair = mmap(NULL, 2 * page_bytes, PROT_READ | PROT_WRITE,
                               MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);

    assert(pair != MAP_FAILED);
    memset(pair, 0x5a, 2 * page_bytes);
    assert(munmap(pair + page_bytes, page_bytes) == 0);
    assert(harmony_fault_watch_read(got, (uintptr_t)pair, sizeof(got)) == 0);
    assert(got[0] == 0x5a && got[sizeof(got) - 1] == 0x5a);
    assert(harmony_fault_watch_read(got, (uintptr_t)(pair + page_bytes), sizeof(got)) != 0);
    assert(harmony_fault_watch_read(got, (uintptr_t)(pair + page_bytes - 16), 32) != 0);
    assert(harmony_fault_watch_read(got, (uintptr_t)pair, sizeof(got)) == 0);
    harmony_fault_watch_pipe_close();
    assert(munmap(pair, page_bytes) == 0);
}

static void refuse_self_reads(void)
{
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_process_vm_readv, 0, 1),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ERRNO | (ENOSYS & SECCOMP_RET_DATA)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {(unsigned short)(sizeof(filter) / sizeof(filter[0])),
                                 filter};
    unsigned char from = 1;
    unsigned char to = 0;
    struct iovec local = {&to, 1};
    struct iovec remote = {&from, 1};

    assert(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0);
    assert(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program) == 0);
    errno = 0;
    assert(process_vm_readv(getpid(), &local, 1, &remote, 1, 0) == -1 && errno == ENOSYS);
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

    page_bytes = page;
    read_mapped_and_unmapped();
    hold_over_changed_mappings(700, 4);
    assert(sigaction(SIGSEGV, &catcher, &before) == 0);
    change_neighbour_during_page_watch();
    unmap_during_page_watch();
    replace_during_page_watch();
    restricted_page_during_page_watch();
    assert(sigaction(SIGSEGV, &before, NULL) == 0);
    sent_signal_ends_process();
    fork_during_pipe_read();
    refuse_self_reads();
    read_mapped_and_unmapped();
    assert(harmony_fault_watch.self_read_refused == 1);
    hold_over_changed_mappings(800, 5);
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
