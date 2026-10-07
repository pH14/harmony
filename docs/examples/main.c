/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <dlfcn.h>
#include "lost_update.c"

__attribute__((no_sanitize("coverage")))
void fuzz_json_data(const char *data, size_t size)
{
    static void (*send)(const char *, size_t);
    if (!send) {
        void *library = dlopen("/usr/lib/libvoidstar.so", RTLD_NOW);
        if (!library || !(send = dlsym(library, "fuzz_json_data"))) {
            fputs("cannot load assertion runtime\n", stderr);
            exit(2);
        }
    }
    send(data, size);
}
