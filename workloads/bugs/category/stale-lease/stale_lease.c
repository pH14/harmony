/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "stale_lease.c"
#define CASE_ASSERTION "store writes never go backwards in fencing tokens"
#include "interleaving.h"

static pthread_mutex_t lease_mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_mutex_t store_mutex = PTHREAD_MUTEX_INITIALIZER;
static struct settings settings;
static uint64_t token, expires, highest_write;
static unsigned owner;

static __attribute__((noinline)) uint64_t valid_lease(unsigned client)
{
    pthread_mutex_lock(&lease_mutex);
    uint64_t now = now_ns();
    if (now >= expires) {
        owner = client;
        token++;
        expires = now + UINT64_C(20000000);
    }
    uint64_t checked = owner == client ? token : 0;
    pthread_mutex_unlock(&lease_mutex);
    return checked;
}

static __attribute__((noinline)) void write_store(uint64_t checked)
{
    pthread_mutex_lock(&store_mutex);
    int holds = 1;
    if (!settings.correct || checked >= highest_write) {
        holds = checked >= highest_write;
        highest_write = checked;
    }
    pthread_mutex_unlock(&store_mutex);
    expect(holds);
}

static void *client(void *argument)
{
    unsigned id = *(unsigned *)argument;
    for (;;) {
        uint64_t checked = valid_lease(id);
        if (checked)
            write_store(checked);
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
    settings = configure("stale_lease");
    started();
    unsigned first = 1, second = 2;
    pthread_t other;
    launch(&other, client, &second);
    client(&first);
    return 2;
}
