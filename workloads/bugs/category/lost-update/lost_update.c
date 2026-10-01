/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

void fuzz_json_data(const char *data, size_t size);

#define WRITERS 2
#define NOISE_MAX 32

struct counter {
    uint64_t value;
    uint64_t done[WRITERS];
};

enum variant { RACY, ATOMIC };

static const char STARTED[] = "writer started";
static const char COUNTED[] = "the counter holds every finished increment";

static void emit(const char *type, const char *id, int must_hit, int condition)
{
    char line[512];
    int size = snprintf(
        line, sizeof line,
        "{\"antithesis_assert\":{\"hit\":true,\"must_hit\":%s,\"assert_type\":\"%s\","
        "\"display_type\":\"%s\",\"message\":\"%s\",\"condition\":%s,\"id\":\"%s\","
        "\"location\":{\"class\":\"\",\"function\":\"writer\",\"file\":\"lost_update.c\","
        "\"begin_line\":0,\"begin_column\":0},\"details\":null}}",
        must_hit ? "true" : "false", type, type, id, condition ? "true" : "false", id);
    if (size > 0 && (size_t)size < sizeof line)
        fuzz_json_data(line, (size_t)size);
}

static long knob(const char *key, long fallback)
{
    char text[4096];
    size_t length = strlen(key);
    long value = fallback;
    int fd = open("/proc/cmdline", O_RDONLY | O_CLOEXEC);
    if (fd < 0)
        return fallback;
    ssize_t size = read(fd, text, sizeof text - 1);
    close(fd);
    if (size <= 0)
        return fallback;
    text[size] = '\0';
    for (char *word = strtok(text, " \n"); word; word = strtok(NULL, " \n"))
        if (strncmp(word, key, length) == 0 && word[length] == '=')
            value = strtol(word + length + 1, NULL, 10);
    return value;
}

static struct counter *map_counter(const char *path, int create)
{
    int fd = open(path, O_RDWR | O_CLOEXEC | (create ? O_CREAT | O_TRUNC : 0), 0644);
    if (fd < 0) {
        perror(path);
        return NULL;
    }
    if (create && ftruncate(fd, sizeof(struct counter)) != 0) {
        perror("ftruncate");
        close(fd);
        return NULL;
    }
    void *memory = mmap(NULL, sizeof(struct counter), PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    close(fd);
    if (memory == MAP_FAILED) {
        perror("mmap");
        return NULL;
    }
    return memory;
}

#define NOISE(n)                                                                                   \
    static __attribute__((noinline)) uint64_t noise_##n(uint64_t x)                                \
    {                                                                                              \
        return x * 6364136223846793005ULL + (n);                                                   \
    }
NOISE(0) NOISE(1) NOISE(2) NOISE(3) NOISE(4) NOISE(5) NOISE(6) NOISE(7)
NOISE(8) NOISE(9) NOISE(10) NOISE(11) NOISE(12) NOISE(13) NOISE(14) NOISE(15)
NOISE(16) NOISE(17) NOISE(18) NOISE(19) NOISE(20) NOISE(21) NOISE(22) NOISE(23)
NOISE(24) NOISE(25) NOISE(26) NOISE(27) NOISE(28) NOISE(29) NOISE(30) NOISE(31)

static uint64_t (*const noise[NOISE_MAX])(uint64_t) = {
    noise_0,  noise_1,  noise_2,  noise_3,  noise_4,  noise_5,  noise_6,  noise_7,
    noise_8,  noise_9,  noise_10, noise_11, noise_12, noise_13, noise_14, noise_15,
    noise_16, noise_17, noise_18, noise_19, noise_20, noise_21, noise_22, noise_23,
    noise_24, noise_25, noise_26, noise_27, noise_28, noise_29, noise_30, noise_31,
};

static __attribute__((noinline)) uint64_t read_value(struct counter *counter)
{
    return __atomic_load_n(&counter->value, __ATOMIC_ACQUIRE);
}

static __attribute__((noinline)) void write_value(struct counter *counter, uint64_t value)
{
    __atomic_store_n(&counter->value, value, __ATOMIC_RELEASE);
}

static __attribute__((noinline)) void increment(struct counter *counter, enum variant variant)
{
    if (variant == ATOMIC) {
        __atomic_fetch_add(&counter->value, 1, __ATOMIC_ACQ_REL);
        return;
    }
    uint64_t value = read_value(counter);
    write_value(counter, value + 1);
}

static __attribute__((noinline)) int counted(struct counter *counter)
{
    uint64_t finished = 0;
    for (int writer = 0; writer < WRITERS; writer++)
        finished += __atomic_load_n(&counter->done[writer], __ATOMIC_ACQUIRE);
    return read_value(counter) >= finished;
}

static int writer(const char *path, int id)
{
    if (id < 0 || id >= WRITERS)
        return 2;
    struct counter *counter = map_counter(path, 0);
    if (!counter)
        return 1;
    enum variant variant = knob("lost_update.atomic", 0) ? ATOMIC : RACY;
    long noise_sites = knob("lost_update.noise", 0);
    if (noise_sites < 0)
        noise_sites = 0;
    if (noise_sites > NOISE_MAX)
        noise_sites = NOISE_MAX;
    emit("reachability", STARTED, 1, 1);
    int reported[2] = {0, 0};
    volatile uint64_t sink = 0;
    const struct timespec pace = {.tv_sec = 0, .tv_nsec = 1000000};
    for (;;) {
        increment(counter, variant);
        __atomic_fetch_add(&counter->done[id], 1, __ATOMIC_RELEASE);
        for (long site = 0; site < noise_sites; site++)
            sink = noise[site](sink);
        int holds = counted(counter);
        if (!reported[holds]) {
            emit("always", COUNTED, 1, holds);
            reported[holds] = 1;
        }
        nanosleep(&pace, NULL);
    }
}

int main(int argc, char **argv)
{
    if (argc == 3 && strcmp(argv[1], "init") == 0)
        return map_counter(argv[2], 1) ? 0 : 1;
    if (argc == 4 && strcmp(argv[1], "writer") == 0)
        return writer(argv[2], atoi(argv[3]));
    fprintf(stderr, "usage: %s init PATH | writer PATH ID\n", argv[0]);
    return 2;
}
