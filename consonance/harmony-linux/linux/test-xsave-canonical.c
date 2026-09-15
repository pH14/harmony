// SPDX-License-Identifier: AGPL-3.0-or-later
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>
typedef uint8_t u8;
typedef uint32_t u32;
typedef uint64_t u64;
#define __user
#ifndef __always_inline
#define __always_inline inline __attribute__((always_inline))
#endif
static u8 *access_base;
static size_t accesses, fail_at = SIZE_MAX;
static int checked_get(u8 *out, u8 *p)
{
    assert(p >= access_base && p < access_base + 832);
    if (accesses++ == fail_at) return -EFAULT;
    *out = *p;
    return 0;
}
static int checked_put(u8 value, u8 *p)
{
    assert(p >= access_base && p < access_base + 832);
    if (accesses++ == fail_at) return -EFAULT;
    *p = value;
    return 0;
}
#define __get_user(value, p) checked_get(&(value), (p))
#define __put_user(value, p) checked_put((value), (p))
#include "harmony_xstate.h"

static void expected(u8 *image, u8 active, u32 mxcsr)
{
    memset(image, 0, 832);
    image[0] = 0x7f;
    image[1] = 3;
    memcpy(image + 24, &mxcsr, 4);
    image[28] = image[29] = 0xff;
    image[512] = active;
    if (active & 1) image[32] = 0x55;
    if (active & 2) image[160] = 0x66;
    if (active & 4) image[576] = 0x77;
}

static void seed(u8 *image, u8 present, u8 active)
{
    u8 init[832];
    expected(init, active, 0x1f80);
    memset(image, 0xa5, 832);
    image[512] = present;
    if (present & 1) {
        memcpy(image, init, 24);
        memcpy(image + 32, init + 32, 128);
        image[5] = 0xa5;
        image[7] = 0xf8;
        for (unsigned i = 0; i < 8; i++) memset(image + 42 + 16 * i, 0xa5, 6);
    }
    if (present & 2) memcpy(image + 160, init + 160, 256);
    if (present & 4) memcpy(image + 576, init + 576, 256);
}

int main(void)
{
    u8 guarded[834], want[832];
    u8 *image = guarded + 1;
    access_base = image;
    assert(harmony_xstate_layout_valid(7, 832, false));
    assert(!harmony_xstate_layout_valid(7, 831, false));
    assert(!harmony_xstate_layout_valid(3, 832, false));
    assert(!harmony_xstate_layout_valid(15, 832, false));
    assert(!harmony_xstate_layout_valid(7, 832, true));
    size_t cases = 0;
    for (unsigned user = 0; user < 2; user++) {
        for (u8 present = 0; present < 8; present++) {
            for (u8 active = 0; active < 8; active++) {
                if (active & ~present) continue;
                for (unsigned nondefault = 0; nondefault < 2; nondefault++) {
                    u32 mxcsr = nondefault ? 0x3f80 : 0x1f80;
                    memset(guarded, 0xc3, sizeof(guarded));
                    seed(image, present, active);
                    expected(want, active, mxcsr);
                    assert(harmony_xstate_canonicalize(image, user, mxcsr) == 0);
                    assert(memcmp(image, want, 832) == 0);
                    assert(guarded[0] == 0xc3 && guarded[833] == 0xc3);
                    assert(harmony_xstate_canonicalize(image, user, mxcsr) == 0);
                    assert(memcmp(image, want, 832) == 0);
                    cases++;
                }
            }
        }
    }
    size_t count = 0;
    for (u8 present = 0; present < 8; present++) {
        for (u8 active = 0; active < 8; active++) {
            if (active & ~present) continue;
            seed(image, present, active);
            accesses = 0;
            assert(harmony_xstate_canonicalize(image, true, 0x3f80) == 0);
            size_t positions = accesses;
            count += positions;
            for (size_t i = 0; i < positions; i++) {
                seed(image, present, active);
                accesses = 0;
                fail_at = i;
                assert(harmony_xstate_canonicalize(image, true, 0x3f80) == -EFAULT);
                assert(guarded[0] == 0xc3 && guarded[833] == 0xc3);
                memset(image, 0, 832);
                seed(image, present, active);
                fail_at = SIZE_MAX;
                assert(harmony_xstate_canonicalize(image, true, 0x3f80) == 0);
                expected(want, active, 0x3f80);
                assert(memcmp(image, want, 832) == 0);
            }
        }
    }
    u8 before[832];
    memcpy(before, image, 832);
    assert(harmony_xstate_canonicalize(image, true, 0x80001f80) == -EINVAL);
    assert(memcmp(before, image, 832) == 0);
    printf("G1_MODEL cases=%zu fault_positions=%zu full_buffer=true masks=true bounds=true mxcsr=true empty_tag_payload=true\n", cases, count);
    return 0;
}
