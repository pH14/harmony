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

/* A site no callback has reached qualifies under every rarity, so a reader
   offering one takes whichever arm is standing. */
static void *reader(void *unused)
{
    uint64_t target;

    (void)unused;
    while (!atomic_load_explicit(&stop, memory_order_acquire)) {
        if (event_kill_claim(0, &target))
            (void)atomic_fetch_add_explicit(&claims, 1, memory_order_relaxed);
        else
            (void)sched_yield();
    }
    return NULL;
}

static void arm_kill(uint64_t rarity)
{
    atomic_store_explicit(&harmony_event_armed, false, memory_order_release);
    atomic_store_explicit(&harmony_event_target, rarity, memory_order_release);
    atomic_store_explicit(
        &harmony_event_ceiling, site_ceiling(rarity), memory_order_release);
    atomic_store_explicit(&harmony_event_armed, true, memory_order_release);
}

int main(void)
{
    pthread_t readers[TEST_READERS];
    uint64_t arm;
    size_t index;

    for (index = 0; index < TEST_READERS; index++)
        assert(pthread_create(&readers[index], NULL, reader, NULL) == 0);
    arm_kill(40);
    while (atomic_load_explicit(&claims, memory_order_acquire) == 0)
        (void)sched_yield();

    /* Replacing an arm is one bounded atomic operation. It never waits for a
     * callback that may be blocked, preempted, or gone. */
    for (arm = 1; arm <= TEST_ARMS; arm++)
        arm_kill(arm % 41);
    atomic_store_explicit(&harmony_event_armed, false, memory_order_release);
    atomic_store_explicit(&stop, true, memory_order_release);
    for (index = 0; index < TEST_READERS; index++)
        assert(pthread_join(readers[index], NULL) == 0);

    /* A site already past the arm's ceiling leaves the arm standing for a
       rarer one; the rare site takes it, and takes it exactly once. */
    arm_kill(0);
    assert(!event_kill_claim(1, NULL));
    assert(atomic_load_explicit(&harmony_event_armed, memory_order_acquire));
    assert(event_kill_claim(0, NULL));
    assert(!atomic_load_explicit(&harmony_event_armed, memory_order_acquire));
    assert(!event_kill_claim(0, NULL));
    return 0;
}
