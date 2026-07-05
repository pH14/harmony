// SPDX-License-Identifier: AGPL-3.0-or-later
// uuid-super — benchmark bug (iii): a RARE-ENTROPY-VALUE bug (task 69). The third
// planted bug of the seeded-bug benchmark, beside campaign-super.c (bug i) and
// order-super.c (bug ii). See dissonance/benchmark (BugClass::RareEntropy) and
// guest/linux/IMPLEMENTATION.md §"The benchmark bugs".
//
// The bug in one sentence: the process draws a value from the guest's seeded
// entropy source (the deterministic `gen_random_uuid()`-style draw the VMM
// controls via the run seed — task 42), and a branch taken ONLY when the draw's
// top PREFIX_BITS match a fixed target prefix poisons a pointer and dereferences
// it, crashing. Nominally the prefix does not match (probability 2^-PREFIX_BITS),
// so the poisoning branch is dead code; the campaign must find the rare seed.
//
// Trigger (tunable — matches dissonance/benchmark manifest BugId(3)):
//   * the seed-derived draw's top PREFIX_BITS bits == TARGET_PREFIX's top bits.
//   * PREFIX_BITS dials the expected time-to-find: 8 bits ⇒ ~256 branches.
//
// Determinism: the draw is a FIXED integer hash of the run seed (splitmix64,
// identical to dissonance/benchmark's `entropy_draw`), NOT host randomness — so a
// fixed seed draws identically every replay and the crash reproduces N/N. On the
// box the seed is the campaign's per-branch seed, read here from the VMM-seeded
// source (a hypercall/`/dev/hwrng`-style channel the image wires to the run
// seed); this portable model reads it from the SEED env the init exports.
// Build: static (`cc -static -O2`), like campaign-super.c.

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/io.h>

// The target prefix and its width (the manifest dial). BugId(3): 8-bit prefix
// 0xA5 in the top byte ⇒ ~1/256 of seeds fire.
#define TARGET_PREFIX 0xA500000000000000ULL
#define PREFIX_BITS 8
// isa-debug-exit terminal. FAIL_CODE 0x63 tags "benchmark bug 3".
#define ISA_DEBUG_EXIT_PORT 0xF4
#define FAIL_CODE 0x63

// splitmix64 — the exact fixed hash dissonance/benchmark::trigger::entropy_draw
// uses, so the guest's ground truth and the offline manifest agree bit-for-bit.
static uint64_t entropy_draw(uint64_t seed)
{
    uint64_t z = seed + 0x9E3779B97F4A7C15ULL;
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}

static int prefix_matches(uint64_t draw)
{
    if (PREFIX_BITS == 0) {
        return 1;
    }
    unsigned shift = 64u - PREFIX_BITS;
    return (draw >> shift) == (TARGET_PREFIX >> shift);
}

// Announce the planted bug: print the distinctive `UUID_BUG:` serial marker
// (fingerprint attribution) and write the terminal FAIL code to isa-debug-exit.
// **Does not return via `_exit`** — the caller emits the marker BEFORE the
// faulting dereference (the crash-attribution gate must see the marker for the
// bug it identifies, and on this container kernel isa-debug-exit is unreachable
// so the deref is the actual crash mechanism /init reports). On a kernel that
// grants port access, the `outb` terminates the guest here (Crash{Panic}).
static void announce_bug(void)
{
    printf("UUID_BUG: rare-entropy prefix matched; poisoning pointer\n");
    fflush(stdout);
    if (ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    if (iopl(3) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    int fd = open("/dev/port", O_WRONLY);
    if (fd >= 0) {
        unsigned char b = FAIL_CODE;
        ssize_t n = pwrite(fd, &b, 1, ISA_DEBUG_EXIT_PORT);
        close(fd);
        (void)n;
    }
}

// Read the run seed from the VMM-controlled channel. On the box this is the
// per-branch campaign seed the image exposes; portably it is the SEED env the
// init exports. A missing/blank seed is 0 (a definite non-hitting default).
static uint64_t read_seed(void)
{
    const char *s = getenv("SEED");
    if (!s || !*s) {
        return 0;
    }
    return strtoull(s, NULL, 0);
}

int main(void)
{
    if (getenv("UUID_DEBUG")) {
        // The crash-channel self-test only (no seed here — it is not drawn yet).
        printf("UUID_IOPERM: %s\n", ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0 ? "ok" : "FAILED");
        fflush(stdout);
    }

    // The base snapshot is sealed here — BEFORE the seed-dependent draw, so each
    // branch re-runs the draw with ITS OWN seed. (If the draw ran before this
    // marker it would be baked into the sealed base and every branch would
    // inherit the same value, making the seed search a no-op — the milestone-1
    // review's P1.)
    printf("UUID_READY\n");
    fflush(stdout);

    // Draw the seeded entropy value AFTER the snapshot point.
    uint64_t seed = read_seed();
    uint64_t draw = entropy_draw(seed);
    if (getenv("UUID_DEBUG")) {
        printf("UUID_DRAW: seed=0x%llx prefix_bits=%d\n",
               (unsigned long long)seed, PREFIX_BITS);
        fflush(stdout);
    }

    // The rare branch: taken only when the seeded draw matches the target prefix.
    // Nominally dead code; the campaign searches seeds until one hits.
    if (prefix_matches(draw)) {
        // Emit the marker + terminal code BEFORE the faulting access, so the
        // per-bug attribution gate always sees `UUID_BUG:` for this bug.
        announce_bug();
        // The planted defect: poison a pointer and dereference it — the crash
        // mechanism /init's reboot terminal reports on the container kernel (where
        // isa-debug-exit above was unreachable). `volatile` so it is not elided.
        volatile uint64_t *poisoned = (volatile uint64_t *)(uintptr_t)0xdead000000000000ULL;
        (void)*poisoned;
        _exit(FAIL_CODE); // fallback if the deref somehow did not fault
    }

    printf("UUID_DONE\n");
    fflush(stdout);
    return 0;
}
