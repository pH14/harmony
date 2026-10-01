/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "lost_update.c"
#define CASE_ASSERTION "the counter holds every finished increment"
#include "interleaving.h"

#define WRITERS 2

struct counter {
    uint64_t value;
    uint64_t done[WRITERS];
};

static __attribute__((noinline)) uint64_t read_value(struct counter *counter)
{
    return LOAD(&counter->value);
}

static __attribute__((noinline)) void write_value(struct counter *counter, uint64_t value)
{
    STORE(&counter->value, value);
}

static __attribute__((noinline)) void increment(struct counter *counter, int correct)
{
    if (correct) {
        __atomic_fetch_add(&counter->value, 1, __ATOMIC_SEQ_CST);
        return;
    }
    uint64_t value = read_value(counter);
    write_value(counter, value + 1);
}

static __attribute__((noinline)) int counted(struct counter *counter)
{
    uint64_t finished = 0;
    for (int writer = 0; writer < WRITERS; writer++)
        finished += LOAD(&counter->done[writer]);
    return read_value(counter) >= finished;
}

static int writer(const char *path, int id)
{
    if (id < 0 || id >= WRITERS)
        return 2;
    struct counter *counter = shared(path, sizeof *counter, 0);
    struct settings settings = configure("lost_update");
    started();
    for (;;) {
        increment(counter, settings.correct);
        __atomic_fetch_add(&counter->done[id], 1, __ATOMIC_SEQ_CST);
        distract(settings.noise);
        expect(counted(counter));
        pace();
    }
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc == 3 && !strcmp(argv[1], "init"))
        return shared(argv[2], sizeof(struct counter), 1) ? 0 : 2;
    if (argc == 4 && !strcmp(argv[1], "writer"))
        return writer(argv[2], atoi(argv[3]));
    return 2;
}
