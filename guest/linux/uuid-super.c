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
// Determinism: the run value is read from **RDRAND**, which the determinism
// hypervisor intercepts and answers with the first word of
// `SeededEntropy::new(EnvSpec.seed)` (the reference xorshift64* stream in
// consonance/hypercall-proto) — NOT host randomness. That word IS the draw (no
// extra hashing); dissonance/benchmark's `entropy_draw` replicates the identical
// function, so the guest and the offline model agree bit-for-bit on which seeds
// fire. A fixed branch draws identically every replay (the crash reproduces N/N)
// while different branches (different EnvSpec seeds) draw different values, which
// is what makes the rare-entropy search work. The RDRAND draw happens AFTER the
// snapshot so the sealed base does not capture a fixed value. Build: static
// (`cc -static -O2`), like campaign-super.c.

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

// Draw the run's entropy from the VMM seeded-entropy service via **RDRAND**,
// which the determinism hypervisor intercepts and answers with the per-branch
// campaign seed (task 42's `gen_random_uuid()` path). This MUST be drawn *after*
// the snapshot (see `main`): a value baked into the process before the base seal
// — e.g. `getenv("SEED")`, which an earlier draft used — is captured by the
// snapshot, so branching with different EnvSpec seeds could never vary it and the
// rare-entropy search was a no-op (the milestone-1 round-2 review's P1). RDRAND is
// re-executed on every branch's run, so each branch's EnvSpec seed actually varies
// the draw. Retries per Intel's RDRAND guidance; falls back to 0 (a definite
// non-hit) only if the instruction never succeeds.
static uint64_t draw_campaign_entropy(void)
{
    for (int i = 0; i < 10; i++) {
        uint64_t v = 0;
        unsigned char ok = 0;
        // Inline asm (no -mrdrnd needed): rdrand into v, carry flag → ok.
        __asm__ volatile("rdrand %0; setc %1" : "=r"(v), "=qm"(ok)::"cc");
        if (ok) {
            return v;
        }
    }
    return 0;
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

    // Draw the seeded entropy AFTER the snapshot point, from the VMM's RDRAND
    // intercept (per-branch, campaign-controlled) — NOT a pre-snapshot env var.
    // The RDRAND word IS the seeded-entropy draw (the first word of
    // `SeededEntropy::new(EnvSpec.seed)`), so it is checked DIRECTLY — no extra
    // hashing. `dissonance/benchmark::trigger::entropy_draw` replicates the exact
    // same value, so the guest and the offline model agree on which seeds fire
    // (the round-3 stream-matching fix — an earlier draft re-hashed with a
    // splitmix64 the model did not share, so they disagreed).
    uint64_t draw = draw_campaign_entropy();
    if (getenv("UUID_DEBUG")) {
        printf("UUID_DRAW: draw=0x%llx prefix_bits=%d\n",
               (unsigned long long)draw, PREFIX_BITS);
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
