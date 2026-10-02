/* SPDX-License-Identifier: AGPL-3.0-or-later */
#define CASE_FILE "aba_reuse.c"
#define CASE_ASSERTION "the stack never resurrects an owned node"
#include "interleaving.h"

#define NIL UINT32_MAX
static struct settings settings;
static uint64_t head;
static uint32_t next[3];
static unsigned stage, done;
static uint32_t retired;

static uint64_t changed(uint64_t previous, uint32_t index)
{
    uint64_t tag = settings.correct ? ((previous >> 32) + 1) << 32 : 0;
    return tag | index;
}

static __attribute__((noinline)) uint32_t pop(void)
{
    for (;;) {
        uint64_t previous = LOAD(&head);
        uint32_t index = (uint32_t)previous;
        if (index == NIL)
            return NIL;
        uint32_t successor = LOAD(&next[index]);
        if (CAS(&head, &previous, changed(previous, successor)))
            return index;
    }
}

static __attribute__((noinline)) void push(uint32_t index)
{
    for (;;) {
        uint64_t previous = LOAD(&head);
        STORE(&next[index], (uint32_t)previous);
        if (CAS(&head, &previous, changed(previous, index)))
            return;
    }
}

static __attribute__((noinline)) int replace_head(uint64_t observed, uint32_t successor)
{
    return CAS(&head, &observed, changed(observed, successor));
}

static void *recycler(void *unused)
{
    (void)unused;
    for (;;) {
        while (LOAD(&stage) == 0)
            pace();
        if (LOAD(&stage) == 1) {
            uint32_t first = pop();
            uint32_t second = pop();
            STORE(&retired, second);
            if (first != NIL)
                push(first);
        }
        STORE(&done, 1);
        while (LOAD(&stage) != 0)
            pace();
        STORE(&done, 0);
    }
    return NULL;
}

int main(int argc, char **argv)
{
    if (argc == 2 && !strcmp(argv[1], "ready"))
        return 0;
    if (argc != 2 || strcmp(argv[1], "run"))
        return 2;
    settings = configure("aba_reuse");
    started();
    pthread_t other;
    launch(&other, recycler, NULL);
    for (;;) {
        STORE(&next[0], 1);
        STORE(&next[1], 2);
        STORE(&next[2], NIL);
        STORE(&retired, NIL);
        uint64_t initial = changed(LOAD(&head), 0);
        STORE(&head, initial);
        uint64_t observed = LOAD(&head);
        uint32_t successor = LOAD(&next[(uint32_t)observed]);
        STORE(&stage, 1);
        int succeeded = replace_head(observed, successor);
        STORE(&stage, 2);
        while (!LOAD(&done))
            pace();
        expect(!succeeded || successor != LOAD(&retired));
        distract(settings.noise);
        STORE(&stage, 0);
        while (LOAD(&done))
            pace();
        pace();
    }
}
