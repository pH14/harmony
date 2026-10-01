/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "missed_wakeup.c"
#define CASE_ASSERTION "a queued item never loses its wakeup"
#include "interleaving.h"

static pthread_mutex_t mutex = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t changed = PTHREAD_COND_INITIALIZER;
static struct settings settings;
static int queued, waiting;
static uint64_t signals, wait_signal, queued_since;

static void *producer(void *unused)
{
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&mutex);
        if (!queued) {
            queued = 1;
            queued_since = now_ns();
            signals++;
            pthread_cond_signal(&changed);
        }
        pthread_mutex_unlock(&mutex);
        distract(settings.noise);
        pace();
    }
    return NULL;
}

static void *consumer(void *unused)
{
    (void)unused;
    for (;;) {
        pthread_mutex_lock(&mutex);
        if (queued) {
            queued = 0;
            pthread_mutex_unlock(&mutex);
            pace();
            continue;
        }
        if (!settings.correct) {
            pthread_mutex_unlock(&mutex);
            distract(settings.noise);
            pthread_mutex_lock(&mutex);
        }
        if (!settings.correct || !queued) {
            waiting = 1;
            wait_signal = signals;
            do {
                pthread_cond_wait(&changed, &mutex);
            } while (settings.correct && !queued);
            waiting = 0;
        }
        queued = 0;
        pthread_mutex_unlock(&mutex);
        pace();
    }
    return NULL;
}

static int run(void)
{
    settings = configure("missed_wakeup");
    started();
    pthread_t produce, consume;
    launch(&produce, producer, NULL);
    launch(&consume, consumer, NULL);
    for (;;) {
        pthread_mutex_lock(&mutex);
        int lost = queued && waiting && wait_signal == signals
            && now_ns() - queued_since >= UINT64_C(15000000000);
        pthread_mutex_unlock(&mutex);
        expect(!lost);
        pace();
    }
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc == 2 && !strcmp(argv[1], "run"))
        return run();
    return 2;
}
