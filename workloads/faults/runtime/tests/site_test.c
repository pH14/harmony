// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
#include <dlfcn.h>
#include <stddef.h>
#include <stdint.h>

static void ignore_json(const char *data, size_t size)
{
    (void)data;
    (void)size;
}
#define HARMONY_JSON(data, size) ignore_json((data), (size))
#include "../fault_runtime.c"

int main(void)
{
    const uint64_t saturated_site = UINT64_C(0x123456789abcdef0);
    size_t index;
    size_t slot;
    uint8_t crossed;

    {
        uint64_t code = (uint64_t)(uintptr_t)&ignore_json;
        uint64_t expected = code;
#if defined(__linux__)
        Dl_info info;
        assert(dladdr((const void *)(uintptr_t)code, &info) != 0);
        expected = code - (uint64_t)(uintptr_t)info.dli_fbase;
#endif
        assert(harmony_fault_site_offset(code) == code);
        harmony_fault_event_note_crossing(code, 3);
        assert(harmony_fault_events.coverage_crossings == 1);
        assert(harmony_fault_events.coverage_digest == 0);
        assert(harmony_fault_events.deferred_count == 1);
        harmony_fault_modules_refresh();
        assert(harmony_fault_site_offset(code) == expected);
        assert(harmony_fault_site_offset(9) == 9);
        harmony_fault_crossings_resolve();
        assert(harmony_fault_events.deferred_count == 0);
        assert(harmony_fault_events.coverage_digest ==
               harmony_fault_crossing_hash(expected, 3));
        harmony_fault_event_note_crossing(code, 4);
        harmony_fault_crossings_resolve();
        assert(harmony_fault_events.coverage_digest ==
               harmony_fault_crossing_hash(expected, 3) +
                   harmony_fault_crossing_hash(expected, 4));
        harmony_fault_events.coverage_crossings = 0;
        harmony_fault_events.coverage_digest = 0;
    }

#if defined(__linux__)
    {
        size_t count = harmony_fault_module_count;
        harmony_fault_module_add(0x1000, 0x100, 0x1000);
        assert(harmony_fault_site_offset(0x1010) == 0x10);
        harmony_fault_module_add(0x1000, 0x100, 0x1000);
        assert(harmony_fault_module_count == count + 1);
        harmony_fault_module_add(0x1000, 0x100, 0x800);
        assert(harmony_fault_site_offset(0x1010) == 0x810);
        harmony_fault_module_add(0x1000, 0x100, 0x1000);
        assert(harmony_fault_site_offset(0x1010) == 0x10);
        assert(harmony_fault_module_count == count + 3);
        assert(harmony_fault_site_offset(0x1100) == 0x1100);
        harmony_fault_module_add(0x100000, 0x10000, 0x100000);
        harmony_fault_module_add(0x108000, 0x2000, 0x108000);
        assert(harmony_fault_site_offset(0x108010) == 0x10);
        harmony_fault_module_add(0x100000, 0x10000, 0x100000);
        assert(harmony_fault_site_offset(0x108010) == 0x8010);
        assert(harmony_fault_module_count == count + 6);
    }
#endif

    assert(harmony_fault_event_rarity_allows(0, 0) != 0);
    assert(harmony_fault_event_rarity_allows(1, 0) == 0);
    assert(harmony_fault_event_rarity_allows((UINT64_C(1) << 63) - 1, 63) != 0);
    assert(harmony_fault_event_rarity_allows(UINT64_C(1) << 63, 63) == 0);
    assert(harmony_fault_event_rarity_allows(UINT64_MAX, 63) == 0);

    assert(harmony_fault_event_site_before(saturated_site, &crossed) == 0);
    assert(harmony_fault_event_site_before(saturated_site, &crossed) == 1);
    slot = harmony_fault_event_site_start(saturated_site);
    harmony_fault_events.site_visits[slot] = UINT64_MAX - 1;
    assert(harmony_fault_event_site_before(saturated_site, &crossed) == UINT64_MAX - 1);
    assert(harmony_fault_event_site_before(saturated_site, &crossed) == UINT64_MAX);
    assert(harmony_fault_event_site_before(saturated_site, &crossed) == UINT64_MAX);

    memset(harmony_fault_events.site_visits, 0,
           sizeof(harmony_fault_events.site_visits));
    for (index = 0; index < 4096; index++)
        (void)harmony_fault_event_site_before((uint64_t)index, &crossed);
    for (index = 4096; index < 8192; index++) {
        slot = harmony_fault_event_site_start((uint64_t)index);
        if (harmony_fault_events.site_visits[slot] == 0)
            break;
    }
    assert(index < 8192);
    assert(harmony_fault_event_site_before((uint64_t)index, &crossed) == 0);
    {
        uint64_t collision = (uint64_t)index + 1;
        while (harmony_fault_event_site_start(collision) != slot)
            collision++;
        assert(harmony_fault_event_site_before(collision, &crossed) == 1);
        assert(harmony_fault_event_site_before((uint64_t)index, &crossed) == 2);
        assert(harmony_fault_event_rarity_allows(2, 0) == 0);
    }

    {
        const uint8_t buckets[] = {0, 1, 2, 3, 4, 4, 4, 4, 5};
        for (index = 0; index < sizeof(buckets); index++)
            assert(harmony_fault_event_bucket((uint64_t)index) == buckets[index]);
        assert(harmony_fault_event_bucket(15) == 5);
        assert(harmony_fault_event_bucket(16) == 6);
        assert(harmony_fault_event_bucket(31) == 6);
        assert(harmony_fault_event_bucket(32) == 7);
        assert(harmony_fault_event_bucket(127) == 7);
        assert(harmony_fault_event_bucket(128) == 8);
        assert(harmony_fault_event_bucket(UINT64_MAX) == 8);
    }

    {
        const uint64_t site = 77;
        uint64_t crossings = 0;
        uint64_t digest;
        memset(harmony_fault_events.site_visits, 0,
               sizeof(harmony_fault_events.site_visits));
        memset(harmony_fault_events.site_bucket, 0,
               sizeof(harmony_fault_events.site_bucket));
        for (index = 0; index < 200; index++) {
            (void)harmony_fault_event_site_before(site, &crossed);
            if (crossed != 0) {
                crossings++;
                harmony_fault_event_note_crossing(site, crossed);
            }
        }
        assert(crossings == 8);
        assert(crossed == 0);
        assert(harmony_fault_events.coverage_crossings == 8);
        assert(harmony_fault_events.deferred_count == 8);
        harmony_fault_crossings_resolve();
        digest = harmony_fault_events.coverage_digest;
        assert(digest != 0);
        for (index = 0; index < 1000; index++) {
            (void)harmony_fault_event_site_before(site, &crossed);
            assert(crossed == 0);
        }
        (void)harmony_fault_event_site_before(site + 1, &crossed);
        assert(crossed == 1);
        harmony_fault_event_note_crossing(site + 1, crossed);
        assert(harmony_fault_events.coverage_crossings == 9);
        harmony_fault_crossings_resolve();
        assert(harmony_fault_events.coverage_digest ==
               digest + harmony_fault_crossing_hash(site + 1, 1));
    }
    return 0;
}
