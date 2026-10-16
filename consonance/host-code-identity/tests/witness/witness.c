/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define _GNU_SOURCE
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <unistd.h>

#ifndef WITNESS_FLIP
#define WITNESS_FLIP 0
#endif

#ifndef WITNESS_CLEAR_PRELOAD
#define WITNESS_CLEAR_PRELOAD 0
#endif

static unsigned long entropy_responses;
static unsigned long flips;
static void *anonymous_code;

static uint16_t u16le(const unsigned char *p) {
    return (uint16_t)p[0] | ((uint16_t)p[1] << 8);
}

static uint32_t u32le(const unsigned char *p) {
    return (uint32_t)p[0] | ((uint32_t)p[1] << 8)
         | ((uint32_t)p[2] << 16) | ((uint32_t)p[3] << 24);
}

unsigned witness_anchor(void) {
    return 0x688u;
}

__attribute__((constructor))
static void map_anonymous_code(void) {
    if (getenv("HARMONY_LOADED_CODE_ANON_EXEC") == NULL) {
        return;
    }
    anonymous_code = mmap(NULL, 4096, PROT_READ | PROT_WRITE,
                          MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (anonymous_code == MAP_FAILED || mprotect(anonymous_code, 4096, PROT_READ | PROT_EXEC) != 0) {
        perror("anonymous executable witness");
        abort();
    }
    fprintf(stderr, "anonymous executable witness mapped\n");
}

ssize_t send(int fd, const void *buffer, size_t length, int flags) {
    const unsigned char *p = buffer;
    if (length == 32 && memcmp(p, "HCP1", 4) == 0
        && u16le(p + 4) == 2 && u16le(p + 6) == 2
        && u16le(p + 8) == 1 && u16le(p + 10) == 0
        && u32le(p + 16) == 8) {
        __atomic_add_fetch(&entropy_responses, 1, __ATOMIC_RELAXED);
#if WITNESS_FLIP
        unsigned char changed[32];
        memcpy(changed, p, sizeof(changed));
        changed[24] ^= 1;
        __atomic_add_fetch(&flips, 1, __ATOMIC_RELAXED);
        return syscall(SYS_sendto, fd, changed, length, flags, NULL, 0);
#endif
    }
    return syscall(SYS_sendto, fd, buffer, length, flags, NULL, 0);
}

#if WITNESS_CLEAR_PRELOAD
__attribute__((constructor))
static void clear_selector(void) {
    unsetenv("LD_PRELOAD");
    fprintf(stderr, "preload constructor cleared LD_PRELOAD\n");
}
#endif

__attribute__((destructor))
static void report(void) {
    fprintf(stderr, "witness flip_mode=%d entropy_responses=%lu flips=%lu\n",
            WITNESS_FLIP,
            __atomic_load_n(&entropy_responses, __ATOMIC_RELAXED),
            __atomic_load_n(&flips, __ATOMIC_RELAXED));
}
