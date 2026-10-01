/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "stale_cache.c"
#define CASE_ASSERTION "a filled cache never predates its invalidation"
#include "interleaving.h"

static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static struct settings settings;
static uint64_t store_version, cache_version;
static int cached;

static __attribute__((noinline)) void fill(uint64_t read_version)
{
    pthread_mutex_lock(&mutex);
    int holds = 1;
    if (!settings.correct || read_version == store_version) {
        cache_version = read_version;
        cached = 1;
        holds = cache_version == store_version;
    }
    pthread_mutex_unlock(&mutex);
    expect(holds);
}

static void *reader(void *unused)
{
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&mutex);
        int missing = !cached;
        uint64_t value = store_version;
        pthread_mutex_unlock(&mutex);
        if (missing)
            fill(value);
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
    settings = configure("stale_cache");
    started();
    pthread_t other;
    launch(&other, reader, NULL);
    for (;;) {
        pthread_mutex_lock(&mutex);
        store_version++;
        cached = 0;
        pthread_mutex_unlock(&mutex);
        distract(settings.noise);
        pace();
    }
}
