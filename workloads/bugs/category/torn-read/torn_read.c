/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "torn_read.c"
#define CASE_ASSERTION "a copied record has matching fields"
#include "interleaving.h"

struct record { uint64_t sequence, first, second; };

static __attribute__((noinline)) uint64_t sequence(struct record *record)
{
    return LOAD(&record->sequence);
}

static __attribute__((noinline)) uint64_t first(struct record *record)
{
    return LOAD(&record->first);
}

static __attribute__((noinline)) uint64_t second(struct record *record)
{
    return LOAD(&record->second);
}

static int run(const char *path, int writer)
{
    struct record *record = shared(path, sizeof *record, 0);
    struct settings settings = configure("torn_read");
    started();
    for (;;) {
        if (writer) {
            uint64_t next = (sequence(record) | 1) + 1;
            STORE(&record->sequence, next - 1);
            STORE(&record->first, next);
            STORE(&record->second, next);
            STORE(&record->sequence, next);
        } else {
            uint64_t before = sequence(record);
            if (!(before & 1)) {
                uint64_t a = first(record);
                uint64_t b = second(record);
                if (!settings.correct || before == sequence(record))
                    expect(a == b);
            }
        }
        distract(settings.noise);
        pace();
    }
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc == 3 && !strcmp(argv[1], "init"))
        return shared(argv[2], sizeof(struct record), 1) ? 0 : 2;
    if (argc == 3 && !strcmp(argv[1], "writer"))
        return run(argv[2], 1);
    if (argc == 3 && !strcmp(argv[1], "reader"))
        return run(argv[2], 0);
    return 2;
}
