/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "mini_wal_reset.c"
#define CASE_ASSERTION "a checkpoint contains one complete log generation"
#include "interleaving.h"

#define ENTRIES 16U
static pthread_rwlock_t lock = PTHREAD_RWLOCK_INITIALIZER;
static struct settings settings;
static uint64_t header, records[ENTRIES];

static __attribute__((noinline)) uint64_t read_header(void)
{
    return LOAD(&header);
}

static __attribute__((noinline)) uint64_t read_entry(unsigned index)
{
    return LOAD(&records[index]);
}

static void *checkpointer(void *unused)
{
    (void)unused;
    for (;;) {
        if (settings.correct)
            pthread_rwlock_rdlock(&lock);
        uint64_t observed = read_header();
        unsigned count = (uint32_t)observed;
        uint64_t generation = observed >> 32;
        int holds = 1;
        for (unsigned index = 0; index < count; index++)
            holds &= read_entry(index) == ((generation << 32) | index);
        if (settings.correct)
            pthread_rwlock_unlock(&lock);
        if (count)
            expect(holds);
        distract(settings.noise);
        pace();
    }
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc != 2 || strcmp(argv[1], "run"))
        return 2;
    settings = configure("mini_wal_reset");
    started();
    pthread_t other;
    launch(&other, checkpointer, NULL);
    for (uint64_t generation = 1;; generation++) {
        unsigned count = generation & 1 ? ENTRIES : ENTRIES / 2;
        pthread_rwlock_wrlock(&lock);
        STORE(&header, (generation - 1) << 32);
        for (unsigned index = 0; index < count; index++)
            STORE(&records[index], (generation << 32) | index);
        STORE(&header, (generation << 32) | count);
        pthread_rwlock_unlock(&lock);
        distract(settings.noise);
        pace();
    }
}
