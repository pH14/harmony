// SPDX-License-Identifier: AGPL-3.0-or-later
// uuid-super — benchmark bug (iii): a RARE-ENTROPY-VALUE bug. The third
// planted bug of the seeded-bug benchmark, beside campaign-super.c (bug i).
// See the benchmark manifest (BugClass::RareEntropy).
//
// The bug in one sentence: the process draws a value from the guest's seeded
// entropy source (the deterministic `gen_random_uuid()`-style draw the VMM
// controls via the run seed), and a branch taken ONLY when the draw's
// top PREFIX_BITS match a fixed target prefix poisons a pointer and dereferences
// it, crashing. Nominally the prefix does not match (probability 2^-PREFIX_BITS),
// so the poisoning branch is dead code; the campaign must find the rare seed.
//
// Trigger (tunable — matches the benchmark manifest BugId(3)):
//   * the seed-derived draw's top PREFIX_BITS bits == TARGET_PREFIX's top bits.
//   * PREFIX_BITS dials the expected time-to-find: 8 bits ⇒ ~256 branches.
//
// Determinism: the run value comes from the seeded /dev/harmony entropy
// transaction after the base snapshot. The first eight-byte response is the
// branch's first SeededEntropy word, without an extra hash. The device reports
// errors explicitly; native RDRAND would instead read unreplayable host entropy.
// Build: static (`cc -static -O2`), like campaign-super.c.

#include <errno.h>
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
#ifndef HARMONY_DEVICE_PATH
#define HARMONY_DEVICE_PATH "/dev/harmony"
#endif
// Post-READY operational-loop length + work cycle. On a NON-firing branch the
// process runs this loop emitting the same bug-agnostic operational logs the
// other supers do, giving the log-template signal a workload to read
// until the campaign deadline cuts it off. Mirrors campaign-super.c's
// ITERS/`BUDGET_MAX/2` so all three supers share the SAME log cadence — the
// apples-to-apples signal workload. A FIRING branch never reaches
// the loop: it crashes at the prefix match, emitting the UUID_BUG marker well
// before the deadline (marker-based certification, terminal-agnostic).
#ifndef ITERS
#define ITERS 200000000L
#endif
#define WORK_CYCLE 500000L
// Pre-draw stabilization span (box calibration 2026-07-07). A
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
// faulting dereference (the crash-attribution check must see the marker for the
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

// Draw after the base snapshot so a branch's EnvSpec seed can select its own
// value. The one-byte request and eight-byte response are the /dev/harmony
// entropy ABI. A failed transaction cannot be interpreted as a non-hit.
static int draw_campaign_entropy(uint64_t *draw)
{
    unsigned char request = 0;
    unsigned char bytes[8];
    int fd;
    do {
        fd = open(HARMONY_DEVICE_PATH, O_RDWR | O_CLOEXEC);
    } while (fd < 0 && errno == EINTR);
    if (fd < 0) {
        return -1;
    }
    ssize_t n;
    do {
        n = write(fd, &request, sizeof(request));
    } while (n < 0 && errno == EINTR);
    if (n != (ssize_t)sizeof(request)) {
        close(fd);
        return -1;
    }
    do {
        n = read(fd, bytes, sizeof(bytes));
    } while (n < 0 && errno == EINTR);
    close(fd);
    if (n != (ssize_t)sizeof(bytes)) {
        return -1;
    }
    uint64_t value = 0;
    for (size_t i = 0; i < sizeof(bytes); i++) {
        value |= (uint64_t)bytes[i] << (i * 8);
    }
    *draw = value;
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
    // `seal_base` needs to land a base BEFORE the draw. A silent busy loop has
    // no deliberate boundary before the entropy device exchange, so the seal
    // could capture a completed draw. Each `fflush` is a console-write exit
    // the seal can land on. The draw is taken ONCE, at
    // iteration STABILIZE_ITERS — by then many checkpoint writes have given the
    // seal a home earlier in the loop, so the draw executes POST-seal, per-branch.
    // STABILIZE_ITERS is tuned (box smoke probe) so the post-seal draw lands within
    // the campaign deadline; a non-matching draw keeps the loop logging to the
    // deadline exactly like bugs 1/2 (the rare branch is dead code otherwise).
    // No entropy transaction runs in the loop except the single draw.
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
        // the seal a landing point before here), from the seeded device —
        // per-branch, NOT baked into the base. The response IS the seeded draw
        // (first word of `SeededEntropy::new(EnvSpec.seed)`); the offline model
        // `trigger::entropy_draw` replicates it. On a prefix match the rare branch
        // poisons a pointer and crashes (marker emitted FIRST for attribution).
        if (i == STABILIZE_ITERS) {
            uint64_t draw;
            if (draw_campaign_entropy(&draw) != 0) {
                printf("UUID_ENTROPY_FAIL: seeded entropy unavailable\n");
                fflush(stdout);
                return 1;
            }
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
