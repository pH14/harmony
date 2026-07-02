// SPDX-License-Identifier: AGPL-3.0-or-later
// campaign-super — a supervised process with a PLANTED, fault-triggerable bug
// (task 60). The added component of the Postgres-campaign workload image; see
// guest/linux/IMPLEMENTATION.md §"The planted bug" and
// dissonance/conductor/IMPLEMENTATION.md §"Task 60".
//
// The bug in one sentence: the process keeps a small "ledger" (a canary + a
// retry budget) in a FIXED-address, mlock'd guest page and runs a bounded,
// deterministic retry loop whose bookkeeping invariant — canary intact, budget
// in [0, BUDGET_MAX) — holds under EVERY nominal execution; a single-event upset
// to that ledger word (a host CorruptMemory fault flipping the budget's sign bit
// or the canary) is the *only* way to reach the "impossible" branch, which the
// supervisor reports through a distinctive serial marker + isa-debug-exit
// (mapped to Crash{Panic} by the task-58 server). No upset ⇒ the loop completes
// and the guest reaches its ordinary forced-reboot terminal (Crash{Shutdown}),
// which the campaign oracle treats as benign.
//
// Why this is a genuine "bug reachable only under injected adversity" and not a
// mere fault detector: the guarded branch encodes an assumption the code relies
// on (the budget word is monotone-bounded) that is true in every nominal run,
// so the branch is dead code nominally. The injected upset makes the assumption
// false, exercising a path that was never meant to run — the planted defect.
//
// Determinism: the ledger lives at a fixed VA (nokaslr + MAP_FIXED + MAP_POPULATE
// + mlock), so its guest-PHYSICAL address is stable across identical boots from
// the same snapshot — a deterministic address the campaign's CorruptMemory fault
// can find by searching. The loop body is pure integer arithmetic (no syscalls,
// no wall-clock, no host entropy), so the run is a pure function of the base
// snapshot and the injected schedule.
//
// Build: static (`cc -static -O2`), like the image's busybox — no shared-lib
// closure to manage, and `ioperm`/`outb` work from a static glibc.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <sys/io.h>

// The fixed virtual address of the ledger page. nokaslr + MAP_FIXED pin it; the
// campaign searches guest-PHYSICAL addresses, which this VA maps to
// deterministically.
#define LEDGER_VADDR 0x20000000UL
// The ledger's intact canary — any single-bit flip breaks it.
#define CANARY 0x5a5a5a5a5a5a5a5aULL
// The nominal retry-budget bound. The loop keeps `budget` in [0, BUDGET_MAX)
// forever; a sign-bit upset drives it negative, tripping the guarded branch.
#define BUDGET_MAX 1000000
// Loop length — long enough that its V-time span brackets a generous fault
// window past the base snapshot (the campaign's --window-* is tuned to it).
#define ITERS 2000000L
// The isa-debug-exit port (vmm-core `ISA_DEBUG_EXIT_PORT`) and the FAIL code the
// supervisor writes to it. A nonzero code → DebugExit{code} → Crash{Panic};
// 0x60 tags "task 60".
#define ISA_DEBUG_EXIT_PORT 0xF4
#define FAIL_CODE 0x60

// The supervisor's bookkeeping ledger, laid out so `canary` is the first word
// (offset 0) and `budget` the second (offset 8) of the fixed page.
struct ledger {
    uint64_t canary;
    int64_t budget;
};

// Report the planted bug on the serial (the SDK-less "assertion rides the serial
// text") and terminate the guest through isa-debug-exit → Crash{Panic}. Never
// returns.
static void report_bug_and_die(const char *which)
{
    printf("CAMPAIGN_BUG: retry-budget invariant violated (%s)\n", which);
    fflush(stdout);
    // The distinctive, non-benign terminal: a byte OUT of a nonzero code to the
    // isa-debug-exit port. Requires I/O-port permission for this one byte.
    if (ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    // Fallback if `ioperm` is unavailable on the box kernel: a nonzero _exit,
    // which /init turns into an early forced reboot the operator can still see on
    // the serial (the crash channel then needs the /dev/port path — see the init
    // script). Loud, never silent.
    _exit(FAIL_CODE);
}

// Bring-up aid (gated on CAMPAIGN_DEBUG so it never perturbs the golden): print
// the ledger's guest-physical address, read via /proc/self/pagemap, so the
// operator can scope the campaign's --gpa-* search tightly. Reading the PFN
// needs CAP_SYS_ADMIN (run as root) on a modern kernel; a zero PFN means the
// capability is absent and the operator should widen the sweep instead.
static void print_ledger_gpa(const void *vaddr)
{
    FILE *f = fopen("/proc/self/pagemap", "rb");
    if (!f) {
        return;
    }
    uint64_t vfn = (uint64_t)(uintptr_t)vaddr / 4096;
    if (fseek(f, (long)(vfn * 8), SEEK_SET) == 0) {
        uint64_t entry = 0;
        if (fread(&entry, sizeof(entry), 1, f) == 1 && (entry & (1ULL << 63))) {
            uint64_t pfn = entry & ((1ULL << 55) - 1);
            uint64_t base = pfn * 4096;
            printf("CAMPAIGN_LEDGER_GPA: canary=0x%llx budget=0x%llx\n",
                   (unsigned long long)base, (unsigned long long)(base + 8));
            fflush(stdout);
        }
    }
    fclose(f);
}

int main(void)
{
    void *p = mmap((void *)LEDGER_VADDR, 4096, PROT_READ | PROT_WRITE,
                   MAP_FIXED | MAP_PRIVATE | MAP_ANONYMOUS | MAP_POPULATE, -1, 0);
    if (p == MAP_FAILED) {
        perror("campaign-super: mmap");
        return 1;
    }
    if (mlock(p, 4096) != 0) {
        // Non-fatal: MAP_POPULATE already faulted the page in; mlock only keeps
        // it from ever being reclaimed (there is no swap here anyway).
        perror("campaign-super: mlock (non-fatal)");
    }
    struct ledger *l = (struct ledger *)p;
    l->canary = CANARY;
    l->budget = 0;

    if (getenv("CAMPAIGN_DEBUG")) {
        print_ledger_gpa(p);
    }

    // The base snapshot is sealed at this marker — mid-workload, post-readiness,
    // right before the fault-sensitive loop (the gate's snapshot point).
    printf("CAMPAIGN_READY\n");
    fflush(stdout);

    // The bounded, deterministic retry loop. The two guards below are the
    // planted invariant: true on every nominal iteration, so the guarded
    // branches are dead code — until a single-event upset makes one fire.
    for (long i = 0; i < ITERS; i++) {
        if (l->canary != CANARY) {
            report_bug_and_die("canary");
        }
        if (l->budget < 0 || l->budget >= BUDGET_MAX) {
            report_bug_and_die("budget");
        }
        l->budget = (l->budget + 1) % (BUDGET_MAX / 2);
    }

    printf("CAMPAIGN_DONE\n");
    fflush(stdout);
    // Return cleanly; /init forces the reboot → Crash{Shutdown} (benign).
    return 0;
}
