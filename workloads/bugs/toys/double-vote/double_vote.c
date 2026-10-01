/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "double_vote.c"
#define CASE_ASSERTION "one term never elects two leaders"
#include "interleaving.h"

static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_barrier_t round_begin, round_end;
static struct settings settings;
static unsigned voted_for, leaders;

static __attribute__((noinline)) int unvoted(void)
{
    return LOAD(&voted_for) == 0;
}

static __attribute__((noinline)) void vote(unsigned candidate)
{
    if (settings.correct)
        pthread_mutex_lock(&mutex);
    if (unvoted()) {
        STORE(&voted_for, candidate);
        __atomic_fetch_or(&leaders, 1U << candidate, __ATOMIC_SEQ_CST);
    }
    if (settings.correct)
        pthread_mutex_unlock(&mutex);
}

static void *candidate(void *unused)
{
    (void)unused;
    for (;;) {
        pthread_barrier_wait(&round_begin);
        vote(2);
        distract(settings.noise);
        pthread_barrier_wait(&round_end);
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
    settings = configure("double_vote");
    started();
    if (pthread_barrier_init(&round_begin, NULL, 2) || pthread_barrier_init(&round_end, NULL, 2))
        die("barrier init");
    pthread_t other;
    launch(&other, candidate, NULL);
    for (;;) {
        STORE(&voted_for, 0);
        STORE(&leaders, 0);
        pthread_barrier_wait(&round_begin);
        vote(1);
        pthread_barrier_wait(&round_end);
        unsigned elected = LOAD(&leaders);
        expect(!(elected && (elected & (elected - 1))));
        distract(settings.noise);
        pace();
    }
}
