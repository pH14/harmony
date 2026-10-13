/* SPDX-License-Identifier: AGPL-3.0-or-later */
/* Shared pieces of the general-discovery drivers: the searchable choice
 * stream, the think-time pause, and Antithesis fallback SDK records. See
 * README.md beside this file for the contract. */
#ifndef HARMONY_GENERAL_H
#define HARMONY_GENERAL_H

#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/random.h>
#include <time.h>
#include <unistd.h>

#define GEN_IOC_EXCHANGE 0xc0204801UL
#define GEN_FRAME_MAGIC 0x31504348u
#define GEN_SERVICE_SDK 6
#define GEN_SDK_SERVICE_REQUEST 3
#define GEN_CHOICE_NAMESPACE 11
#define GEN_HEADER_LEN 24

struct gen_exchange {
    uint64_t request;
    uint64_t response;
    uint32_t request_len;
    uint32_t response_capacity;
    uint32_t response_len;
    uint32_t reserved;
};

struct gen_rng {
    uint64_t state;
    uint64_t last_choice;
    int have_choice;
    int device;
    uint32_t seq;
    unsigned long choices_folded;
};

static inline void gen_put16(unsigned char *at, uint16_t value) {
    at[0] = (unsigned char)value;
    at[1] = (unsigned char)(value >> 8);
}

static inline void gen_put32(unsigned char *at, uint32_t value) {
    for (int i = 0; i < 4; i++) at[i] = (unsigned char)(value >> (8 * i));
}

static inline uint32_t gen_get32(const unsigned char *at) {
    uint32_t value = 0;
    for (int i = 0; i < 4; i++) value |= (uint32_t)at[i] << (8 * i);
    return value;
}

static inline uint64_t gen_get64(const unsigned char *at) {
    uint64_t value = 0;
    for (int i = 0; i < 8; i++) value |= (uint64_t)at[i] << (8 * i);
    return value;
}

static inline uint64_t gen_mix64(uint64_t z) {
    z = (z ^ (z >> 30)) * 0xbf58476d1ce4e5b9ULL;
    z = (z ^ (z >> 27)) * 0x94d049bb133111ebULL;
    return z ^ (z >> 31);
}

static inline uint64_t gen_splitmix64(uint64_t *state) {
    *state += 0x9e3779b97f4a7c15ULL;
    return gen_mix64(*state);
}

/* Returns 1 with the current action choice, 0 when no host answers. */
static int gen_choice_query(struct gen_rng *rng, uint64_t *choice) {
    unsigned char request[GEN_HEADER_LEN + 10];
    unsigned char response[GEN_HEADER_LEN + 16];
    struct gen_exchange exchange;
    if (rng->device < 0) return 0;
    memset(request, 0, sizeof request);
    gen_put32(request, GEN_FRAME_MAGIC);
    gen_put16(request + 4, 1);
    gen_put16(request + 6, GEN_SERVICE_SDK);
    gen_put16(request + 8, GEN_SDK_SERVICE_REQUEST);
    gen_put32(request + 12, ++rng->seq);
    gen_put32(request + 16, 10);
    gen_put16(request + GEN_HEADER_LEN, GEN_CHOICE_NAMESPACE);
    memset(&exchange, 0, sizeof exchange);
    exchange.request = (uint64_t)(uintptr_t)request;
    exchange.response = (uint64_t)(uintptr_t)response;
    exchange.request_len = sizeof request;
    exchange.response_capacity = sizeof response;
    if (ioctl(rng->device, GEN_IOC_EXCHANGE, &exchange) != 0) return 0;
    if (exchange.response_len < GEN_HEADER_LEN) return 0;
    if (gen_get32(response) != GEN_FRAME_MAGIC) return 0;
    if (response[10] != 0 || response[11] != 0) return 0;
    if (gen_get32(response + 16) != 9 || exchange.response_len != GEN_HEADER_LEN + 9) return 0;
    if (response[GEN_HEADER_LEN] != 1) return 0;
    *choice = gen_get64(response + GEN_HEADER_LEN + 1);
    return 1;
}

static void gen_rng_init(struct gen_rng *rng) {
    memset(rng, 0, sizeof *rng);
    if (getrandom(&rng->state, sizeof rng->state, 0) != (ssize_t)sizeof rng->state)
        rng->state = (uint64_t)getpid() * 0x9e3779b97f4a7c15ULL;
    rng->device = open("/dev/harmony", O_RDWR | O_CLOEXEC);
}

/* Fold the current action's choice into the stream before an operation. A
 * restored snapshot restores this state; a new action's choice then makes the
 * continuation's operations differ from its siblings'. */
static void gen_rng_sync(struct gen_rng *rng) {
    uint64_t choice;
    if (!gen_choice_query(rng, &choice)) return;
    if (rng->have_choice && choice == rng->last_choice) return;
    rng->state ^= gen_mix64(choice ^ 0x6a09e667f3bcc909ULL);
    rng->last_choice = choice;
    rng->have_choice = 1;
    rng->choices_folded++;
}

static inline uint64_t gen_next(struct gen_rng *rng) { return gen_splitmix64(&rng->state); }

static inline uint64_t gen_below(struct gen_rng *rng, uint64_t bound) {
    return bound ? gen_next(rng) % bound : 0;
}

static inline int gen_one_in(struct gen_rng *rng, uint64_t n) { return gen_below(rng, n) == 0; }

static void gen_sleep_ms(unsigned ms) {
    struct timespec t = {(time_t)(ms / 1000), (long)(ms % 1000) * 1000000L};
    while (nanosleep(&t, &t) != 0 && errno == EINTR) {
    }
}

static unsigned gen_think_ms(struct gen_rng *rng) { return 1u << gen_below(rng, 8); }

/* Decision sites. The current action's choice holds one byte for each site
 * (site modulo 8). The byte's top two bits say how often the site follows the
 * choice (never, 1 in 2, 3 in 4 or 7 in 8 draws) and its low six bits name the
 * option it prefers, modulo the site's option count. A site the choice leaves
 * alone draws exactly as it did without a choice. The search, not the
 * workload, decides which sites a choice biases and toward what. */
static int gen_bias(struct gen_rng *rng, unsigned site, unsigned n) {
    static const unsigned follow[4] = {0, 4, 6, 7};
    unsigned byte;
    if (!rng->have_choice || n == 0) return -1;
    byte = (unsigned)(rng->last_choice >> (8 * (site % 8))) & 0xffu;
    if (follow[byte >> 6] == 0 || gen_below(rng, 8) >= follow[byte >> 6]) return -1;
    return (int)((byte & 0x3fu) % n);
}

static unsigned gen_pick(struct gen_rng *rng, unsigned site, unsigned n) {
    int preferred = gen_bias(rng, site, n);
    return preferred >= 0 ? (unsigned)preferred : (unsigned)gen_below(rng, n);
}

/* Option 0 is no pause and option k is 2^(k-1) ms; unbiased, it pauses half
 * the time for 1 to 128 ms. */
static void gen_think_at(struct gen_rng *rng, unsigned site) {
    int level = gen_bias(rng, site, 9);
    if (level < 0) {
        if (gen_below(rng, 2) == 0) gen_sleep_ms(gen_think_ms(rng));
    } else if (level > 0) {
        gen_sleep_ms(1u << (level - 1));
    }
}

static void gen_json_string(FILE *out, const char *text) {
    fputc('"', out);
    for (; *text; text++) {
        unsigned char c = (unsigned char)*text;
        if (c == '"' || c == '\\') fprintf(out, "\\%c", c);
        else if (c < 0x20) fprintf(out, "\\u%04x", c);
        else fputc(c, out);
    }
    fputc('"', out);
}

/* One Antithesis fallback SDK record, written with one write call. */
static void gen_assert(const char *type, const char *id, int hit, int condition, const char *detail) {
    const char *dir = getenv("ANTITHESIS_OUTPUT_DIR");
    char *buffer = NULL;
    size_t length = 0;
    FILE *line;
    char path[512];
    int fd;
    if (!dir || !*dir) return;
    line = open_memstream(&buffer, &length);
    if (!line) return;
    fprintf(line, "{\"antithesis_assert\":{\"hit\":%s,\"must_hit\":true,\"assert_type\":\"%s\","
                  "\"display_type\":\"%s\",\"message\":",
            hit ? "true" : "false", type, strcmp(type, "always") == 0 ? "Always" : "Reachable");
    gen_json_string(line, id);
    fprintf(line, ",\"condition\":%s,\"id\":", condition ? "true" : "false");
    gen_json_string(line, id);
    fprintf(line, ",\"location\":{\"class\":\"general\",\"function\":\"\",\"file\":\"\","
                  "\"begin_line\":0,\"begin_column\":0},\"details\":");
    if (detail) {
        fputs("{\"detail\":", line);
        gen_json_string(line, detail);
        fputc('}', line);
    } else {
        fputs("null", line);
    }
    fputs("}}\n", line);
    fclose(line);
    snprintf(path, sizeof path, "%s/sdk.jsonl", dir);
    fd = open(path, O_WRONLY | O_APPEND | O_CREAT | O_CLOEXEC, 0644);
    if (fd >= 0) {
        ssize_t written = write(fd, buffer, length);
        (void)written;
        close(fd);
    }
    free(buffer);
}

static inline void gen_declare_always(const char *id) { gen_assert("always", id, 0, 0, NULL); }
static inline void gen_declare_reachable(const char *id) { gen_assert("reachability", id, 0, 0, NULL); }
static inline void gen_always(const char *id, int condition, const char *detail) {
    gen_assert("always", id, 1, condition, condition ? NULL : detail);
}
static inline void gen_reached(const char *id) { gen_assert("reachability", id, 1, 1, NULL); }

#endif
