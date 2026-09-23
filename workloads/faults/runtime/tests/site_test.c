// SPDX-License-Identifier: AGPL-3.0-or-later

#include <assert.h>
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

    {
        uint64_t code = (uint64_t)(uintptr_t)&ignore_json;
        Dl_info info;
        assert(dladdr((const void *)(uintptr_t)code, &info) != 0);
        assert(harmony_fault_site_offset(code) ==
               code - (uint64_t)(uintptr_t)info.dli_fbase);
        assert(harmony_fault_site_offset(9) == 9);
    }

    assert(harmony_fault_event_rarity_allows(0, 0) != 0);
    assert(harmony_fault_event_rarity_allows(1, 0) == 0);
    assert(harmony_fault_event_rarity_allows((UINT64_C(1) << 63) - 1, 63) != 0);
    assert(harmony_fault_event_rarity_allows(UINT64_C(1) << 63, 63) == 0);
    assert(harmony_fault_event_rarity_allows(UINT64_MAX, 63) == 0);

    assert(harmony_fault_event_site_before(saturated_site) == 0);
    assert(harmony_fault_event_site_before(saturated_site) == 1);
    slot = harmony_fault_event_site_start(saturated_site);
    harmony_fault_events.site_visits[slot] = UINT64_MAX - 1;
    assert(harmony_fault_event_site_before(saturated_site) == UINT64_MAX - 1);
    assert(harmony_fault_event_site_before(saturated_site) == UINT64_MAX);
    assert(harmony_fault_event_site_before(saturated_site) == UINT64_MAX);

    memset(harmony_fault_events.site_visits, 0,
           sizeof(harmony_fault_events.site_visits));
    for (index = 0; index < 4096; index++)
        (void)harmony_fault_event_site_before((uint64_t)index);
    for (index = 4096; index < 8192; index++) {
        slot = harmony_fault_event_site_start((uint64_t)index);
        if (harmony_fault_events.site_visits[slot] == 0)
            break;
    }
    assert(index < 8192);
    assert(harmony_fault_event_site_before((uint64_t)index) == 0);
    {
        uint64_t collision = (uint64_t)index + 1;
        while (harmony_fault_event_site_start(collision) != slot)
            collision++;
        assert(harmony_fault_event_site_before(collision) == 1);
        assert(harmony_fault_event_site_before((uint64_t)index) == 2);
        assert(harmony_fault_event_rarity_allows(2, 0) == 0);
    }
    return 0;
}
