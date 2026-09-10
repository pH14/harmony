// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <pthread.h>
#include <sched.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <stdint.h>

#include "../voidstar.c"

enum {
    TEST_PHASE_ACKED = 0,
    TEST_PHASE_CLOSING = 1,
    TEST_PHASE_PUBLISHED = 2,
    TEST_READER_THREADS = 4,
    TEST_GENERATIONS = 2048
};

struct gate_test_state {
    _Atomic uint64_t generation;
    _Atomic uint64_t acknowledged;
    _Atomic uint64_t remaining;
    _Atomic uint64_t active;
    _Atomic uint64_t callbacks;
    _Atomic uint64_t violations;
    _Atomic uint64_t phase;
    _Atomic uint64_t held_consumed_generation;
    _Atomic bool held_entered;
    _Atomic bool release_held;
    _Atomic bool close_started;
    _Atomic bool close_done;
    _Atomic bool stop;
};

static uint64_t consume_test_event(struct gate_test_state *state)
{
    uint64_t generation = atomic_load_explicit(&state->generation, memory_order_acquire);
    uint64_t remaining = atomic_load_explicit(&state->remaining, memory_order_acquire);

    while (remaining != 0) {
        if (atomic_compare_exchange_weak_explicit(
                &state->remaining, &remaining, remaining - 1,
                memory_order_acq_rel, memory_order_acquire)) {
            uint64_t acknowledged = atomic_load_explicit(
                &state->acknowledged, memory_order_acquire);
            uint64_t phase = atomic_load_explicit(&state->phase, memory_order_acquire);

            /* A callback may finish while the closer is draining the old
             * generation, but it must never observe a newly published
             * generation before its acknowledgement is complete. */
            if (phase == TEST_PHASE_PUBLISHED || generation != acknowledged)
                (void)atomic_fetch_add_explicit(
                    &state->violations, 1, memory_order_relaxed);
            return generation;
        }
    }
    return 0;
}

static void *held_reader(void *argument)
{
    struct gate_test_state *state = argument;

    event_gate_enter();
    atomic_store_explicit(&state->held_entered, true, memory_order_release);
    while (!atomic_load_explicit(&state->release_held, memory_order_acquire))
        (void)sched_yield();
    (void)atomic_fetch_add_explicit(&state->active, 1, memory_order_relaxed);
    atomic_store_explicit(
        &state->held_consumed_generation, consume_test_event(state), memory_order_release);
    (void)atomic_fetch_sub_explicit(&state->active, 1, memory_order_relaxed);
    event_gate_leave();
    return NULL;
}

static void *close_gate(void *argument)
{
    struct gate_test_state *state = argument;

    atomic_store_explicit(&state->close_started, true, memory_order_release);
    event_gate_close();
    atomic_store_explicit(&state->close_done, true, memory_order_release);
    return NULL;
}

static void *reader(void *argument)
{
    struct gate_test_state *state = argument;

    while (!atomic_load_explicit(&state->stop, memory_order_acquire)) {
        event_gate_enter();
        (void)atomic_fetch_add_explicit(&state->active, 1, memory_order_relaxed);
        (void)atomic_fetch_add_explicit(&state->callbacks, 1, memory_order_relaxed);
        (void)consume_test_event(state);
        (void)atomic_fetch_sub_explicit(&state->active, 1, memory_order_relaxed);
        event_gate_leave();
    }
    return NULL;
}

static void wait_until(_Atomic bool *value)
{
    while (!atomic_load_explicit(value, memory_order_acquire))
        (void)sched_yield();
}

int main(void)
{
    struct gate_test_state state = {
        .generation = 1,
        .acknowledged = 1,
        .remaining = 1,
        .phase = TEST_PHASE_ACKED,
    };
    pthread_t held;
    pthread_t closer;
    pthread_t readers[TEST_READER_THREADS];
    uint64_t generation;
    size_t index;

    assert(pthread_create(&held, NULL, held_reader, &state) == 0);
    wait_until(&state.held_entered);
    assert(pthread_create(&closer, NULL, close_gate, &state) == 0);
    wait_until(&state.close_started);
    while ((atomic_load_explicit(&harmony_event_gate, memory_order_acquire) &
            HARMONY_EVENT_GATE_CLOSED) == 0)
        (void)sched_yield();
    assert(!atomic_load_explicit(&state.close_done, memory_order_acquire));
    atomic_store_explicit(&state.release_held, true, memory_order_release);
    assert(pthread_join(held, NULL) == 0);
    assert(pthread_join(closer, NULL) == 0);
    assert(atomic_load_explicit(&state.held_consumed_generation, memory_order_acquire) == 1);
    assert(atomic_load_explicit(&state.remaining, memory_order_acquire) == 0);
    assert(atomic_load_explicit(&state.active, memory_order_acquire) == 0);

    atomic_store_explicit(&state.generation, 2, memory_order_release);
    atomic_store_explicit(&state.remaining, 1, memory_order_release);
    atomic_store_explicit(&state.acknowledged, 2, memory_order_release);
    atomic_store_explicit(&state.phase, TEST_PHASE_ACKED, memory_order_release);
    event_gate_reopen();

    for (index = 0; index < TEST_READER_THREADS; ++index)
        assert(pthread_create(&readers[index], NULL, reader, &state) == 0);
    for (generation = 3; generation < TEST_GENERATIONS; ++generation) {
        atomic_store_explicit(&state.phase, TEST_PHASE_CLOSING, memory_order_release);
        event_gate_close();
        assert(atomic_load_explicit(&state.active, memory_order_acquire) == 0);
        atomic_store_explicit(&state.generation, generation, memory_order_release);
        atomic_store_explicit(&state.remaining, 1, memory_order_release);
        atomic_store_explicit(&state.phase, TEST_PHASE_PUBLISHED, memory_order_release);
        (void)sched_yield();
        atomic_store_explicit(&state.acknowledged, generation, memory_order_release);
        atomic_store_explicit(&state.phase, TEST_PHASE_ACKED, memory_order_release);
        event_gate_reopen();

        /* Exercise the disarm command on every other generation too. */
        if ((generation & 1) == 0) {
            atomic_store_explicit(&state.phase, TEST_PHASE_CLOSING, memory_order_release);
            event_gate_close();
            assert(atomic_load_explicit(&state.active, memory_order_acquire) == 0);
            atomic_store_explicit(&state.remaining, 0, memory_order_release);
            atomic_store_explicit(&state.phase, TEST_PHASE_PUBLISHED, memory_order_release);
            (void)sched_yield();
            atomic_store_explicit(&state.acknowledged, generation, memory_order_release);
            atomic_store_explicit(&state.phase, TEST_PHASE_ACKED, memory_order_release);
            event_gate_reopen();
        }
    }
    atomic_store_explicit(&state.stop, true, memory_order_release);
    event_gate_reopen();
    for (index = 0; index < TEST_READER_THREADS; ++index)
        assert(pthread_join(readers[index], NULL) == 0);

    assert(atomic_load_explicit(&state.callbacks, memory_order_acquire) != 0);
    assert(atomic_load_explicit(&state.violations, memory_order_acquire) == 0);
    return 0;
}
