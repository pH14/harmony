// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdatomic.h>
#include <stdint.h>

#include "../voidstar.c"

enum {
    TEST_READERS = 4,
    TEST_ARMS = 4096
};

static _Atomic bool stop;
static _Atomic uint64_t claims;

static void *reader(void *unused)
{
    uint64_t target;

    (void)unused;
    while (!atomic_load_explicit(&stop, memory_order_acquire)) {
        if (event_claim(&target) != 0)
            (void)atomic_fetch_add_explicit(&claims, 1, memory_order_relaxed);
        else
            (void)sched_yield();
    }
    return NULL;
}

int main(void)
{
    pthread_t readers[TEST_READERS];
    uint64_t arm;
    size_t index;

    for (index = 0; index < TEST_READERS; index++)
        assert(pthread_create(&readers[index], NULL, reader, NULL) == 0);
    atomic_store_explicit(&harmony_event_target, UINT64_MAX, memory_order_release);
    atomic_store_explicit(&harmony_event_remaining, UINT64_MAX, memory_order_release);
    while (atomic_load_explicit(&claims, memory_order_acquire) == 0)
        (void)sched_yield();

    /* Replacing an arm is one bounded atomic operation. It never waits for a
     * callback that may be blocked, preempted, or gone. */
    for (arm = 1; arm <= TEST_ARMS; arm++) {
        (void)atomic_exchange_explicit(
            &harmony_event_remaining, 0, memory_order_acq_rel);
        atomic_store_explicit(&harmony_event_target, arm, memory_order_release);
        (void)atomic_exchange_explicit(
            &harmony_event_remaining, arm, memory_order_acq_rel);
    }
    (void)atomic_exchange_explicit(&harmony_event_remaining, 0, memory_order_acq_rel);
    atomic_store_explicit(&stop, true, memory_order_release);
    for (index = 0; index < TEST_READERS; index++)
        assert(pthread_join(readers[index], NULL) == 0);
    atomic_store_explicit(&harmony_event_target, UINT64_MAX, memory_order_release);
    atomic_store_explicit(&harmony_event_remaining, UINT64_MAX, memory_order_release);
    assert(event_claim(NULL) == UINT64_MAX);
    assert(atomic_load_explicit(&harmony_event_remaining, memory_order_acquire) ==
           UINT64_MAX - 1);
    return 0;
}
