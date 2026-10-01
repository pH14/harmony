/* SPDX-License-Identifier: AGPL-3.0-or-later */
#ifndef INTERLEAVING_H
#define INTERLEAVING_H
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

void fuzz_json_data(const char *data, size_t size);

struct settings { int correct; unsigned noise; };

static void die(const char *operation)
{
    perror(operation);
    exit(2);
}

static long knob(const char *key)
{
    char text[4096];
    int fd = open("/proc/cmdline", O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        die("cmdline");
    ssize_t size = read(fd, text, sizeof text - 1);
    close(fd);
    if (size <= 0)
        die("read cmdline");
    text[size] = 0;
    size_t length = strlen(key);
    long result = 0;
    for (char *word = strtok(text, " \n"); word; word = strtok(NULL, " \n"))
        if (!strncmp(word, key, length) && word[length] == '=')
            result = strtol(word + length + 1, NULL, 10);
    return result;
}

static struct settings configure(const char *prefix)
{
    char key[128];
    snprintf(key, sizeof key, "%s.correct", prefix);
    int correct = knob(key) != 0;
    snprintf(key, sizeof key, "%s.noise", prefix);
    long noise = knob(key);
    return (struct settings){correct, (unsigned)(noise < 0 ? 0 : noise > 32 ? 32 : noise)};
}

static void emit(const char *type, const char *id, int condition)
{
    char line[1024];
    int size = snprintf(line, sizeof line,
        "{\"antithesis_assert\":{\"hit\":true,\"must_hit\":true,\"assert_type\":\"%s\","
        "\"display_type\":\"%s\",\"message\":\"%s\",\"condition\":%s,\"id\":\"%s\","
        "\"location\":{\"class\":\"\",\"function\":\"run\",\"file\":\"%s\","
        "\"begin_line\":0,\"begin_column\":0},\"details\":null}}",
        type, type, id, condition ? "true" : "false", id, CASE_FILE);
    if (size <= 0 || (size_t)size >= sizeof line)
        die("assertion size");
    fuzz_json_data(line, (size_t)size);
}

static void expect(int condition)
{
    static unsigned reported;
    unsigned bit = condition ? 1U : 2U;
    if (!(__atomic_fetch_or(&reported, bit, __ATOMIC_SEQ_CST) & bit))
        emit("always", CASE_ASSERTION, condition);
}

static void started(void)
{
    emit("reachability", "interleaving workload started", 1);
}

static void pace(void)
{
    struct timespec request = {.tv_nsec = 1000000};
    while (nanosleep(&request, &request) && errno == EINTR) {}
}

static __attribute__((unused)) uint64_t now_ns(void)
{
    struct timespec now;
    if (clock_gettime(CLOCK_MONOTONIC, &now))
        die("clock_gettime");
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

static __attribute__((unused)) void *shared(const char *path, size_t size, int create)
{
    int fd = open(path, O_RDWR | O_CLOEXEC | (create ? O_CREAT | O_TRUNC : 0), 0644);
    if (fd < 0 || (create && ftruncate(fd, (off_t)size)))
        die("shared file");
    void *memory = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (memory == MAP_FAILED)
        die("mmap");
    return memory;
}

static __attribute__((unused)) void launch(pthread_t *thread, void *(*function)(void *), void *argument)
{
    int error = pthread_create(thread, NULL, function, argument);
    if (error) {
        errno = error;
        die("pthread_create");
    }
}

#define NOISE(n) \
    static __attribute__((noinline)) uint64_t noise_##n(uint64_t x) \
    { return x * UINT64_C(6364136223846793005) + (n); }
NOISE(0) NOISE(1) NOISE(2) NOISE(3) NOISE(4) NOISE(5) NOISE(6) NOISE(7)
NOISE(8) NOISE(9) NOISE(10) NOISE(11) NOISE(12) NOISE(13) NOISE(14) NOISE(15)
NOISE(16) NOISE(17) NOISE(18) NOISE(19) NOISE(20) NOISE(21) NOISE(22) NOISE(23)
NOISE(24) NOISE(25) NOISE(26) NOISE(27) NOISE(28) NOISE(29) NOISE(30) NOISE(31)
#undef NOISE

static uint64_t (*const noise_sites[32])(uint64_t) = {
    noise_0, noise_1, noise_2, noise_3, noise_4, noise_5, noise_6, noise_7,
    noise_8, noise_9, noise_10, noise_11, noise_12, noise_13, noise_14, noise_15,
    noise_16, noise_17, noise_18, noise_19, noise_20, noise_21, noise_22, noise_23,
    noise_24, noise_25, noise_26, noise_27, noise_28, noise_29, noise_30, noise_31,
};

static void distract(unsigned count)
{
    volatile uint64_t sink = 0;
    for (unsigned site = 0; site < count; site++)
        sink = noise_sites[site](sink);
}

#define LOAD(pointer) __atomic_load_n((pointer), __ATOMIC_SEQ_CST)
#define STORE(pointer, value) __atomic_store_n((pointer), (value), __ATOMIC_SEQ_CST)
#define CAS(pointer, expected, value) \
    __atomic_compare_exchange_n((pointer), (expected), (value), 0, __ATOMIC_SEQ_CST, __ATOMIC_SEQ_CST)
#endif
