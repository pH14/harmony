/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "duplicate_request.c"
#define CASE_ASSERTION "each request is applied at most once"
#include "interleaving.h"

#define KEYS (1U << 22)
#define ARRIVAL_NS UINT64_C(2000000)
struct requests { uint64_t epoch_ns; uint32_t applied[KEYS]; };

static uint64_t arrived(struct requests *requests)
{
    return (now_ns() - LOAD(&requests->epoch_ns)) / ARRIVAL_NS;
}

static __attribute__((noinline)) uint32_t lookup(struct requests *requests, unsigned key)
{
    return LOAD(&requests->applied[key]);
}

static __attribute__((noinline)) void apply(struct requests *requests, unsigned key)
{
    __atomic_fetch_add(&requests->applied[key], 1, __ATOMIC_SEQ_CST);
}

static int run(const char *path)
{
    struct requests *requests = shared(path, sizeof *requests, 0);
    struct settings settings = configure("duplicate_request");
    started();
    for (uint64_t request = arrived(requests);; request++) {
        while (request >= arrived(requests))
            pace();
        unsigned key = (unsigned)(request % KEYS);
        distract(settings.noise);
        if (!lookup(requests, key)) {
            (void)getpid();
            if (settings.correct) {
                uint32_t missing = 0;
                (void)CAS(&requests->applied[key], &missing, 1);
            } else {
                apply(requests, key);
            }
        }
        expect(lookup(requests, key) <= 1);
        pace();
    }
}

static int init(const char *path)
{
    struct requests *requests = shared(path, sizeof *requests, 1);
    STORE(&requests->epoch_ns, now_ns());
    return 0;
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc == 3 && !strcmp(argv[1], "init"))
        return init(argv[2]);
    if (argc == 3 && !strcmp(argv[1], "handler"))
        return run(argv[2]);
    return 2;
}
