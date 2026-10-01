// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <errno.h>
#include <pthread.h>
#include <stdatomic.h>
#include <stdint.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

static atomic_bool done;

static void *spin(void *argument)
{
    volatile uint64_t visits = 0;
    (void)argument;
    while (!atomic_load_explicit(&done, memory_order_relaxed))
        visits++;
    return NULL;
}

int main(int argc, char **argv)
{
    pthread_t worker;
    pid_t child = -1;
    if (argc == 2 && strcmp(argv[1], "processes") == 0) {
        child = fork();
        assert(child >= 0);
        if (child == 0) {
            (void)spin(NULL);
            return 1;
        }
    } else {
        assert(pthread_create(&worker, NULL, spin, NULL) == 0);
    }
    puts("HARMONY_LANGUAGE_READY");
    fflush(stdout);
    for (unsigned int marker = 1; marker <= 20; marker++) {
        struct timespec delay = {0, 10000000};
        while (nanosleep(&delay, &delay) != 0)
            assert(errno == EINTR);
        printf("HARMONY_LANGUAGE_MARKER %02u\n", marker);
        fflush(stdout);
    }
    atomic_store_explicit(&done, 1, memory_order_relaxed);
    if (child > 0) {
        assert(kill(child, SIGTERM) == 0);
        assert(waitpid(child, NULL, 0) == child);
    } else {
        assert(pthread_join(worker, NULL) == 0);
    }
    return 0;
}
