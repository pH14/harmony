// SPDX-License-Identifier: AGPL-3.0-or-later
// **The first real `/dev/harmony` transaction** (bead hm-i8kc, PR #133 finding
// F2). Until this probe existed, nothing anywhere executed the guest bridge:
// `libvoidstar/tests/abi_test.c` macro-mocks `open`/`read`/`write` and compiles
// the library against the mocks, and the Linux box gate only greps the serial
// for `GUEST_READY`. So the driver's ioctl-free read/write ABI, the host's
// Entropy/Event doorbell services, and `libvoidstar.so` had never met.
//
// Two legs, deliberately:
//
//   1. **Raw ABI leg** — this file opens `/dev/harmony` itself and checks every
//      return value. `libvoidstar`'s public ABI is fire-and-forget by design
//      (`fuzz_json_data` returns `void`, `fuzz_get_random` returns 0 both for
//      "the host said 0" and for "the transaction failed"), so a probe built
//      *only* on the library cannot tell a live bridge from a dead one — it
//      would be a green-on-fail gate of exactly the shape the tasks/157 lane
//      already had to fix once (PR161-F1). The raw leg is what makes failure
//      loud: it prints the errno.
//   2. **libvoidstar leg** — `dlopen`s `/usr/lib/libvoidstar.so` (the path and
//      loading pattern real Antithesis SDKs use, and the one `play-agent`
//      already follows for its libretro core) and drives the same device
//      through the shipped library, proving the artifact guests actually link
//      is live, not just the ABI underneath it.
//
// Every line it prints is a gate assertion made by the host side; the exit code
// is the summary. Determinism note: the entropy words come from the host's
// seeded stream, so at a fixed boot seed they are fixed values — the box gate
// compares two same-seed runs for equality and two different-seed runs for
// inequality rather than hard-coding a constant here.

#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#define DEV "/dev/harmony"
#define LIBVOIDSTAR "/usr/lib/libvoidstar.so"

/* The JSON the raw leg emits. The driver splices a `harmony_attribution`
 * object (rip/pid/comm_hex) into every emission, so the host sees this object
 * plus that attribution — an event id of 0 (`CATALOG_EVENT_ID`) is stamped by
 * the driver, which is finding F9's whole point. */
static const char RAW_JSON[] =
    "{\"antithesis_assert\":{\"id\":\"harmony_bridge_probe_raw\",\"condition\":true}}";
static const char LIB_JSON[] =
    "{\"antithesis_assert\":{\"id\":\"harmony_bridge_probe_libvoidstar\",\"condition\":true}}";

static int fail(const char *what)
{
    printf("BRIDGE_FAIL: %s: %s\n", what, strerror(errno));
    fflush(stdout);
    return 1;
}

/* One raw transaction on a SINGLE fd: the driver keeps the fetched entropy in
 * per-fd state (`struct harmony_file`), so a write on one fd and a read on
 * another would return -EAGAIN. */
static int raw_leg(uint64_t *entropy_out)
{
    unsigned char zero = 0;
    unsigned char bytes[8];
    ssize_t n;
    int fd = open(DEV, O_RDWR | O_CLOEXEC);

    if (fd < 0)
        return fail("open " DEV);

    n = write(fd, RAW_JSON, sizeof(RAW_JSON) - 1);
    if (n != (ssize_t)(sizeof(RAW_JSON) - 1)) {
        close(fd);
        return fail("write(json)");
    }
    printf("BRIDGE_JSON_RAW: wrote %zd bytes\n", n);

    n = write(fd, &zero, 1);
    if (n != 1) {
        close(fd);
        return fail("write(entropy request)");
    }
    n = read(fd, bytes, sizeof(bytes));
    if (n != (ssize_t)sizeof(bytes)) {
        close(fd);
        return fail("read(entropy)");
    }
    close(fd);

    *entropy_out = 0;
    for (size_t i = 0; i < sizeof(bytes); i++)
        *entropy_out |= (uint64_t)bytes[i] << (i * 8);
    printf("BRIDGE_ENTROPY_RAW: %016llx\n", (unsigned long long)*entropy_out);
    return 0;
}

static int libvoidstar_leg(uint64_t *a, uint64_t *b)
{
    void (*json)(const char *, size_t);
    uint64_t (*rnd)(void);
    void *h = dlopen(LIBVOIDSTAR, RTLD_NOW | RTLD_LOCAL);

    if (h == NULL) {
        printf("BRIDGE_FAIL: dlopen " LIBVOIDSTAR ": %s\n", dlerror());
        return 1;
    }
    /* The library exports C symbols; the object-pointer cast is the documented
     * POSIX dlsym idiom. */
    *(void **)(&json) = dlsym(h, "fuzz_json_data");
    *(void **)(&rnd) = dlsym(h, "fuzz_get_random");
    if (json == NULL || rnd == NULL) {
        printf("BRIDGE_FAIL: dlsym: %s\n", dlerror());
        dlclose(h);
        return 1;
    }
    json(LIB_JSON, sizeof(LIB_JSON) - 1);
    printf("BRIDGE_JSON_LIB: emitted %zu bytes\n", sizeof(LIB_JSON) - 1);
    *a = rnd();
    *b = rnd();
    printf("BRIDGE_ENTROPY_LIB: %016llx %016llx\n",
           (unsigned long long)*a, (unsigned long long)*b);
    dlclose(h);
    return 0;
}

int main(void)
{
    uint64_t raw = 0, lib_a = 0, lib_b = 0;

    setvbuf(stdout, NULL, _IOLBF, 0);
    printf("BRIDGE_DEV: %s\n", access(DEV, R_OK | W_OK) == 0 ? "present" : "ABSENT");
    printf("BRIDGE_LIB: %s\n", access(LIBVOIDSTAR, R_OK) == 0 ? "present" : "ABSENT");

    if (raw_leg(&raw) != 0)
        return 2;
    if (libvoidstar_leg(&lib_a, &lib_b) != 0)
        return 3;

    /* Three draws off one seeded stream must not repeat: an all-equal (or
     * all-zero) result would mean the host answered from a stuck source, or
     * that the library swallowed a failure and returned its 0 sentinel. */
    if (raw == 0 && lib_a == 0 && lib_b == 0) {
        printf("BRIDGE_FAIL: every entropy draw was 0 — a swallowed failure\n");
        return 4;
    }
    if (lib_a == lib_b) {
        printf("BRIDGE_FAIL: two draws returned the same word %016llx — the stream did not "
               "advance\n", (unsigned long long)lib_a);
        return 5;
    }
    printf("BRIDGE_PROBE_RC=0\n");
    return 0;
}
