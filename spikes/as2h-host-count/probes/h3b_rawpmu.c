// SPDX-License-Identifier: AGPL-3.0-or-later
// AS-2H / H3b — is KPC_CLASS_RAWPMU (or any other kpc class) a usable path
// to EL-filter or guest-scope a counter on this kernel?
//
// Pure enumeration + benign write-back test; no guest. Run as root.

#include <dlfcn.h>
#include <errno.h>
#include <inttypes.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static uint32_t (*kpc_get_counter_count)(uint32_t);
static uint32_t (*kpc_get_config_count)(uint32_t);
static int (*kpc_get_config)(uint32_t, uint64_t *);
static int (*kpc_set_config)(uint32_t, uint64_t *);
static int (*kpc_force_all_ctrs_set)(int);
static int (*kpc_force_all_ctrs_get)(int *);
static int (*kpc_pmu_version)(void);

int main(void) {
    void *kperf = dlopen(
        "/System/Library/PrivateFrameworks/kperf.framework/kperf", RTLD_LAZY);
    if (!kperf) { fprintf(stderr, "dlopen failed\n"); return 2; }
#define SYM(n) *(void **)&n = dlsym(kperf, #n)
    SYM(kpc_get_counter_count);
    SYM(kpc_get_config_count);
    SYM(kpc_get_config);
    SYM(kpc_set_config);
    SYM(kpc_force_all_ctrs_set);
    SYM(kpc_force_all_ctrs_get);
    SYM(kpc_pmu_version);
#undef SYM
    if (!kpc_get_counter_count || !kpc_get_config_count) {
        fprintf(stderr, "core symbols missing\n");
        return 3;
    }
    if (kpc_pmu_version)
        printf("{\"pmu_version\":%d}\n", kpc_pmu_version());

    static const struct { uint32_t mask; const char *name; } classes[] = {
        {0x1, "fixed"}, {0x2, "configurable"}, {0x4, "power"}, {0x8, "rawpmu"},
    };
    for (unsigned i = 0; i < 4; i++) {
        uint32_t cc = kpc_get_counter_count(classes[i].mask);
        uint32_t fc = kpc_get_config_count(classes[i].mask);
        printf("{\"class\":\"%s\",\"mask\":%u,\"counter_count\":%u,"
               "\"config_count\":%u}\n",
               classes[i].name, classes[i].mask, cc, fc);
    }

    // benign write-back on any class with configs beyond `configurable`
    for (unsigned i = 0; i < 4; i++) {
        uint32_t mask = classes[i].mask;
        if (mask == 0x2) continue;
        uint32_t fc = kpc_get_config_count(mask);
        if (fc == 0 || fc > 64 || !kpc_get_config || !kpc_set_config) continue;
        int forced = 0;
        if (kpc_force_all_ctrs_set && kpc_force_all_ctrs_set(1) == 0) forced = 1;
        uint64_t *cfg = calloc(fc, sizeof(uint64_t));
        int grc = kpc_get_config(mask, cfg);
        printf("{\"class\":\"%s\",\"get_config_rc\":%d,\"get_errno\":%d,"
               "\"values\":[", classes[i].name, grc, grc ? errno : 0);
        for (uint32_t j = 0; j < fc; j++)
            printf("%s\"0x%" PRIx64 "\"", j ? "," : "", cfg[j]);
        printf("]}\n");
        if (grc == 0) {
            errno = 0;
            int src = kpc_set_config(mask, cfg); // write back unchanged
            printf("{\"class\":\"%s\",\"set_config_writeback_rc\":%d,"
                   "\"set_errno\":%d}\n", classes[i].name, src, errno);
        }
        free(cfg);
        if (forced) kpc_force_all_ctrs_set(0);
    }
    return 0;
}
