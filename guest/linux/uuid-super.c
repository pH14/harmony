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
// Post-READY operational-loop length + work cycle. On a NON-firing branch the
// process runs this loop emitting the same bug-agnostic operational logs the
// other supers do, giving the log-template signal (task 67) a workload to read
// until the campaign deadline cuts it off. Mirrors campaign-super.c's
// ITERS/`BUDGET_MAX/2` so all three supers share the SAME log cadence — the
// apples-to-apples signal workload (task 69 M2). A FIRING branch never reaches
// the loop: it crashes at the prefix match, emitting the UUID_BUG marker well
// before the deadline (marker-based certification, terminal-agnostic).
#define ITERS 200000000L
#define WORK_CYCLE 500000L
// Pre-draw stabilization span (task 69 M2 fix — box calibration 2026-07-07). A
// short bounded busy loop runs after UUID_READY and BEFORE the entropy draw so
// `seal_base`'s snapshot-retry (advancing `snapshot_retry_step` = 10_000 ns each
// try) lands a snapshottable base INSIDE this loop — before the draw — the way
// campaign-super's long loop gives its seal a landing point. WITHOUT it uuid-super
// draws+decides+crashes within a few instructions of UUID_READY, faster than one
// retry step, so the seal OVERSHOOTS the draw and bakes it into the base — every
// branch then inherits the same (baked) draw and the rare-entropy search is a
// no-op (0 fires on the box, confirmed 2026-07-07). The span must exceed the
// 10_000 ns retry step yet stay well under the campaign deadline (50_000 ns) so
// the post-seal draw still lands in time; ~250k iters ≈ tens of µs of V-time.
// Tuned on the box (smoke-fire probe 2026-07-07): the draw must land AFTER the
// seal (which lands at one of the early checkpoint writes, i=0 or i=4096) yet
// within the campaign deadline (~8.9 ns/iter with checkpoints every 4096 ⇒ ~50k
// ns budget ≈ a few thousand iters). i=8192 puts the draw just past the i=4096
// checkpoint. `volatile`/writes below defeat -O2 elision.
#define STABILIZE_ITERS 8192L

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

    // The base snapshot is sealed just after here — inside the stabilization loop
    // below, BEFORE the seed-dependent draw, so each branch re-runs the draw with
    // ITS OWN seed. (If the draw ran before the seal it would be baked into the
    // sealed base and every branch would inherit the same value, making the seed
    // search a no-op — the milestone-1 review's P1, which the box seal *also*
    // re-introduces unless the seal is given a pre-draw landing point; see below.)
    printf("UUID_READY\n");
    fflush(stdout);

    // Pre-draw stabilization + operational logging, unified in ONE bounded loop.
    // The periodic console writes below are BOTH the bug-agnostic signal workload
    // (identical idiom to bugs 1/2) AND — crucially — the snapshottable boundaries
    // `seal_base` needs to land a base BEFORE the draw. A SILENT busy loop has no
    // such boundary, so the seal's only landing point is the RDRAND intercept
    // itself and it bakes the draw into the base (0 fires on the box; both the
    // silent-loop and no-loop variants confirmed this 2026-07-07). Each `fflush`
    // is a console-write exit the seal can land on. The draw is taken ONCE, at
    // iteration STABILIZE_ITERS — by then many checkpoint writes have given the
    // seal a home earlier in the loop, so the draw executes POST-seal, per-branch.
    // STABILIZE_ITERS is tuned (box smoke probe) so the post-seal draw lands within
    // the campaign deadline; a non-matching draw keeps the loop logging to the
    // deadline exactly like bugs 1/2 (the rare branch is dead code otherwise).
    // No RDRAND runs in the loop except the single draw, so the seeded-entropy
    // stream the model replicates is not advanced.
    long last_phase = -1;
    for (long i = 0; i < ITERS; i++) {
        long work = i % WORK_CYCLE;
        long phase = (work * 3) / WORK_CYCLE; // {0,1,2} across the normal cycle
        if (phase != last_phase) {
            const char *name = phase <= 0 ? "warmup" : (phase == 1 ? "steady" : "drain");
            printf("supervisor: lifecycle phase %s\n", name);
            if (phase >= 2) {
                printf("supervisor: backpressure engaged, shedding retries\n");
            }
            fflush(stdout);
            last_phase = phase;
        }
        if (i % 4096 == 0) {
            printf("supervisor: checkpoint committed, batch complete\n");
            fflush(stdout);
        }

        // Draw the seeded entropy ONCE, post-seal (earlier checkpoint writes gave
        // the seal a landing point before here), from the VMM's RDRAND intercept —
        // per-branch, NOT baked into the base. The RDRAND word IS the seeded draw
        // (first word of `SeededEntropy::new(EnvSpec.seed)`); the offline model
        // `trigger::entropy_draw` replicates it. On a prefix match the rare branch
        // poisons a pointer and crashes (marker emitted FIRST for attribution).
        if (i == STABILIZE_ITERS) {
            uint64_t draw = draw_campaign_entropy();
            if (getenv("UUID_DEBUG")) {
                printf("UUID_DRAW: draw=0x%llx prefix_bits=%d\n",
                       (unsigned long long)draw, PREFIX_BITS);
                fflush(stdout);
            }
            if (prefix_matches(draw)) {
                announce_bug();
                volatile uint64_t *poisoned =
                    (volatile uint64_t *)(uintptr_t)0xdead000000000000ULL;
                (void)*poisoned;
                _exit(FAIL_CODE); // fallback if the deref somehow did not fault
            }
        }
    }

    printf("UUID_DONE\n");
    fflush(stdout);
    return 0;
}
