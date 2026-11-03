/* SPDX-License-Identifier: AGPL-3.0-or-later */
#ifndef HARMONY_GUIDANCE_H
#define HARMONY_GUIDANCE_H

#include <inttypes.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

void fuzz_json_data(const char *data, size_t size);

static size_t harmony_guidance_escape(char *out, const char *text)
{
    static const char hex[] = "0123456789abcdef";
    const unsigned char *p = (const unsigned char *)(text ? text : "");
    size_t used = 0;

    for (; *p; p++) {
        if (*p == '"' || *p == '\\') {
            out[used++] = '\\';
            out[used++] = (char)*p;
        } else if (*p < 0x20) {
            out[used++] = '\\';
            out[used++] = 'u';
            out[used++] = '0';
            out[used++] = '0';
            out[used++] = hex[*p >> 4];
            out[used++] = hex[*p & 15];
        } else {
            out[used++] = (char)*p;
        }
    }
    out[used] = '\0';
    return used;
}

static char *harmony_guidance_quoted(const char *text)
{
    size_t length = text ? strlen(text) : 0;
    char *out;

    if (length > (SIZE_MAX - 1) / 6)
        abort();
    out = malloc(length * 6 + 1);
    if (out == NULL)
        abort();
    (void)harmony_guidance_escape(out, text);
    return out;
}

static __attribute__((unused)) void harmony_guidance_emit(
    const char *id, int hit, int64_t value, int maximize, const char *file, int line,
    const char *function)
{
    char *quoted_id = harmony_guidance_quoted(id);
    char *quoted_file = harmony_guidance_quoted(file);
    char *quoted_function = harmony_guidance_quoted(function);
    char data[64] = "";
    char *record;
    size_t room;
    int size;

    if (quoted_id[0] == '\0')
        abort();
    if (hit)
        (void)snprintf(data, sizeof data,
                       "\"guidance_data\":{\"left\":%" PRId64 ",\"right\":0},", value);
    room = 2 * strlen(quoted_id) + strlen(quoted_file) + strlen(quoted_function) + 256;
    record = malloc(room);
    if (record == NULL)
        abort();
    size = snprintf(record, room,
                    "{\"antithesis_guidance\":{%s\"location\":{\"class\":\"\","
                    "\"function\":\"%s\",\"file\":\"%s\",\"begin_line\":%d,"
                    "\"begin_column\":0},\"guidance_type\":\"numeric\",\"message\":\"%s\","
                    "\"id\":\"%s\",\"maximize\":%s,\"hit\":%s}}",
                    data, quoted_function, quoted_file, line, quoted_id, quoted_id,
                    maximize ? "true" : "false", hit ? "true" : "false");
    if (size <= 0 || (size_t)size >= room)
        abort();
    fuzz_json_data(record, (size_t)size);
    free(record);
    free(quoted_function);
    free(quoted_file);
    free(quoted_id);
}

#define harmony_guide(id, value, maximize) \
    harmony_guidance_emit((id), 1, (int64_t)(value), (maximize), __FILE__, __LINE__, __func__)
#define harmony_declare_guide(id, maximize) \
    harmony_guidance_emit((id), 0, 0, (maximize), __FILE__, __LINE__, __func__)

#endif
