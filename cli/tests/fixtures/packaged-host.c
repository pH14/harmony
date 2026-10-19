// SPDX-License-Identifier: AGPL-3.0-or-later
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

int main(int argc, char **argv)
{
    uint64_t hash = UINT64_C(0xcbf29ce484222325);
    for (uint64_t i = 0; i < 4096; ++i)
        hash = (hash ^ (unsigned char)(i * 17 + 31)) * UINT64_C(0x100000001b3);
    printf("HARMONY_PACKAGED_HOST_READBACK=%016lx\n", (unsigned long)hash);
    fflush(stdout);
    if (argc == 2 && strcmp(argv[1], "complete") == 0)
        return 0;
    if (argc == 2 && strcmp(argv[1], "node") == 0) {
        const struct timespec delay = { .tv_sec = 0, .tv_nsec = 1000000 };
        for (;;) {
            if (nanosleep(&delay, NULL))
                return 2;
        }
    }
    return 3;
}
