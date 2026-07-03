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

#include <errno.h>
#include <fcntl.h>
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

// Emit the distinctive terminal: a byte OUT of a nonzero code to the isa-debug-exit
// port (`DebugExit{FAIL_CODE}` → Crash{Panic}). A successful write TERMINATES the
// guest (the VMM exits on the port write), so a channel that reaches the port does
// not return. Tries every host-visible route in turn — the kernel may configure
// any one of them — so the crash channel does not silently depend on a single
// `CONFIG_*`. Returns nonzero only if **no** channel reached the port.
static int emit_debug_exit(void)
{
    // 1. ioperm + outb (CONFIG_X86_IOPL_IOPERM + CAP_SYS_RAWIO).
    if (ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    // 2. iopl(3) + outb (the older all-ports route).
    if (iopl(3) == 0) {
        outb(FAIL_CODE, ISA_DEBUG_EXIT_PORT);
    }
    // 3. /dev/port write at the port offset (CONFIG_DEVPORT) — no ioperm needed.
    int fd = open("/dev/port", O_WRONLY);
    if (fd >= 0) {
        unsigned char b = FAIL_CODE;
        ssize_t n = pwrite(fd, &b, 1, ISA_DEBUG_EXIT_PORT);
        close(fd);
        (void)n;
    }
    return 1; // none of the channels terminated the guest
}

// Report the planted bug on the serial (the SDK-less "assertion rides the serial
// text") and terminate the guest through isa-debug-exit → Crash{Panic}. Never
// returns.
static void report_bug_and_die(const char *which)
{
    printf("CAMPAIGN_BUG: retry-budget invariant violated (%s)\n", which);
    fflush(stdout);
    emit_debug_exit();
    // Fallback only if NO channel reached the port: a nonzero _exit, which /init
    // turns into an early forced reboot the operator still sees on the serial
    // (the CAMPAIGN_IOPERM/CAMPAIGN_DEVPORT self-test below says which channels
    // exist). Loud, never silent.
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
    // VOLATILE is load-bearing: the ledger is mutated from OUTSIDE this
    // translation unit (a host-side `CorruptMemory` fault flips `canary`/`budget`
    // while the loop runs), so nothing the compiler can see writes them after
    // init. Without `volatile`, `-O2` may hoist `l->canary != CANARY` to a
    // constant and keep `budget` in a register — silently deleting the planted
    // bug's guards (the whole milestone mechanism). `volatile` forces a real
    // memory load on every access, so the injected flip is observed.
    volatile struct ledger *l = (volatile struct ledger *)p;
    l->canary = CANARY;
    l->budget = 0;

    if (getenv("CAMPAIGN_DEBUG")) {
        print_ledger_gpa(p);
        // Crash-channel self-test (does not write FAIL — only probes availability),
        // so the boot serial says which route emit_debug_exit will use. Granting
        // ioperm here also arms the branched runs (the permission is captured in
        // the base snapshot).
        printf("CAMPAIGN_IOPERM: %s\n",
               ioperm(ISA_DEBUG_EXIT_PORT, 1, 1) == 0 ? "ok" : "FAILED");
        printf("CAMPAIGN_IOPL: %s\n", iopl(3) == 0 ? "ok" : "FAILED");
        int probe = open("/dev/port", O_WRONLY);
        printf("CAMPAIGN_DEVPORT: %s\n", probe >= 0 ? "ok" : "FAILED");
        if (probe >= 0) {
            close(probe);
        }
        fflush(stdout);
    }

    // The base snapshot is sealed at this marker — mid-workload, post-readiness,
    // right before the fault-sensitive loop (the gate's snapshot point).
    printf("CAMPAIGN_READY\n");
    fflush(stdout);

    // The bounded, deterministic retry loop. The two guards below are the
    // planted invariant: true on every nominal iteration, so the guarded
    // branches are dead code — until a single-event upset makes one fire. Every
    // `l->` access is a volatile load/store (above), so the guards are never
    // optimized out and the host's flip is always seen.
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
