/* SPDX-License-Identifier: AGPL-3.0-or-later */
#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

#include "../harmony_guidance.h"

static char captured[4][512];
static size_t records;

void fuzz_json_data(const char *data, size_t size)
{
    assert(records < 4);
    assert(size < sizeof captured[0]);
    memcpy(captured[records], data, size);
    captured[records][size] = '\0';
    records++;
}

static int contains(const char *text, const char *part)
{
    return strstr(text, part) != NULL;
}

int main(void)
{
    harmony_declare_guide("follower lag", 1);
    harmony_guide("follower lag", 17, 1);
    harmony_guide("term gap", INT64_MIN, 0);
    harmony_guide("quote \" and \\ and \n", -3, 1);
    assert(records == 4);
    assert(strncmp(captured[0], "{\"antithesis_guidance\":{\"location\":", 35) == 0);
    assert(!contains(captured[0], "guidance_data"));
    assert(contains(captured[0], "\"hit\":false}}"));
    assert(contains(captured[0], "\"message\":\"follower lag\",\"id\":\"follower lag\""));
    assert(contains(captured[0], "\"guidance_type\":\"numeric\""));
    assert(contains(captured[0], "\"function\":\"main\""));
    assert(contains(captured[0], "guidance_test.c"));
    assert(contains(captured[1], "\"guidance_data\":{\"left\":17,\"right\":0}"));
    assert(contains(captured[1], "\"maximize\":true,\"hit\":true}}"));
    assert(contains(captured[2], "\"left\":-9223372036854775808,"));
    assert(contains(captured[2], "\"maximize\":false"));
    assert(contains(captured[3], "\"id\":\"quote \\\" and \\\\ and \\u000a\""));
    return 0;
}
