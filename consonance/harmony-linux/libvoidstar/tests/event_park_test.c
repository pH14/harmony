// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <time.h>

#include "../voidstar.c"

static uint64_t monotonic_nanos(void)
{
    struct timespec now;

    assert(clock_gettime(CLOCK_MONOTONIC, &now) == 0);
    return (uint64_t)now.tv_sec * UINT64_C(1000000000) + (uint64_t)now.tv_nsec;
}

/// A rarity-0 arm holds the first callback at a site no callback has reached.
/// The visit count comes from `site_visit`, which every callback takes once.
static void holds_at_an_unvisited_site(void)
{
    const uint64_t hold = UINT64_C(40000000);
    uint64_t held = 0;
    uint64_t started;

    park_arm(0, hold);
    started = monotonic_nanos();
    assert(park_claim(site_visit(1000), &held));
    assert(held == hold);
    park_hold(held);
    assert(monotonic_nanos() - started >= hold);
}

/// A site already visited past the arm's ceiling does not hold, and the arm
/// stays in force for a rarer one.
static void skips_a_site_visited_past_the_ceiling(void)
{
    uint64_t held = 0;
    uint64_t common = 2000;
    int index;

    park_disarm();
    for (index = 0; index < 8; ++index)
        assert(!park_claim(site_visit(common), &held));
    park_arm(1, UINT64_C(1000));
    assert(!park_claim(site_visit(common), &held));
    assert(park_claim(site_visit(2001), &held));
    assert(held == UINT64_C(1000));
}

/// One arm holds exactly once however many callbacks qualify.
static void one_arm_holds_one_callback(void)
{
    uint64_t held = 0;

    park_disarm();
    park_arm(0, UINT64_C(1000));
    assert(park_claim(site_visit(3000), &held));
    assert(!park_claim(site_visit(3001), &held));
    assert(!park_claim(site_visit(3002), &held));
}

/// A zero hold is a disarm, so a closed window cannot leave a park behind.
static void a_zero_hold_disarms(void)
{
    uint64_t held = 0;

    park_arm(0, UINT64_C(1000));
    park_arm(0, 0);
    assert(!park_claim(site_visit(4000), &held));
}

/// A hold is counted so the host can tell an arm that fired from one that
/// never reached its site.
static void a_hold_is_counted(void)
{
    uint64_t held = 0;
    uint64_t before = atomic_load(&harmony_park_fires);

    park_disarm();
    park_arm(0, UINT64_C(1000));
    assert(park_claim(site_visit(5000), &held));
    assert(atomic_load(&harmony_park_fires) == before + 1);
    assert(!park_claim(site_visit(5001), &held));
    assert(atomic_load(&harmony_park_fires) == before + 1);
}

int main(void)
{
    holds_at_an_unvisited_site();
    skips_a_site_visited_past_the_ceiling();
    one_arm_holds_one_callback();
    a_zero_hold_disarms();
    a_hold_is_counted();
    return 0;
}
