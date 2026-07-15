// SPDX-License-Identifier: AGPL-3.0-or-later
//
// G3's busy-wait: spin on the harmony pvclock page until N virtual nanoseconds
// have elapsed, reading the page DIRECTLY and making **no syscalls and no raw
// counter reads inside the loop** (docs/PARAVIRT-CLOCK.md §6 G3; cross-model r5).
//
// WHY NOT A SHELL LOOP. The obvious G3 guest — `while [ $(date +%s) -lt N ]` —
// is not a test of the forced refresh at all. Every `date` is a syscall, this
// kernel randomizes the kernel stack offset on syscall entry, and that
// randomization reads the TSC: `do_syscall_64` carries an `rdtsc` (it is in the
// reviewed allowlist). An RDTSC is a **V-time intercept**, so every syscall
// exits to the VMM, advances the anchor, and refreshes the page — the loop then
// terminates whether or not the Δ staleness bound does anything, and the
// intercepts can even keep the pvclock deadline from ever landing, so the
// attribution count reads zero. The gate would be vacuous in both directions.
//
// So the spin below touches nothing but memory: it maps the clock page once
// (before the measured window) and then reads it in a pure user-space loop. The
// ONLY thing that can make this loop's clock advance is the host refreshing the
// page from outside — which is precisely §2.4's staleness-bound forced refresh.
// Freeze it and this program hangs; that is the gate.
//
// The page's physical address is the one the guest kernel published to the host
// at registration, printed on the console ("work-derived clock page registered
// at 0x..."); the harness passes it in argv. /dev/mem is how this guest already
// reaches the reserved hypercall doorbell pages (CONFIG_DEVMEM=y, STRICT_DEVMEM
// off — the page is reserved kernel .bss, not anonymous RAM).
//
// Usage: pvclock-spin <page-pa-hex> <spin-nanoseconds>
//   prints: PVSPIN_DONE <iters> <vns0> <vns1>   (or PVSPIN_FAIL <reason>)

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

// docs/PARAVIRT-CLOCK.md §1, ABI v1 — byte offsets into the page.
#define PVCLOCK_PAGE_LEN 4096u
#define ABI_VERSION_OFF 0x00
#define SEQ_OFF 0x04
#define VNS_OFF 0x08

#define PVCLOCK_ABI_VERSION 1u

// The seqlock read (§1): sample an even sequence, read the value, re-read the
// sequence; retry while it moved or was odd. `volatile` keeps the compiler from
// hoisting the loads out of the spin — the whole point is that the host mutates
// these bytes underneath us.
static uint64_t read_vns(volatile const unsigned char *page) {
    for (;;) {
        uint32_t s0 = *(volatile const uint32_t *)(page + SEQ_OFF);
        if (s0 & 1u) {
            continue; // update in progress
        }
        uint64_t vns = *(volatile const uint64_t *)(page + VNS_OFF);
        uint32_t s1 = *(volatile const uint32_t *)(page + SEQ_OFF);
        if (s0 == s1) {
            return vns;
        }
    }
}

int main(int argc, char **argv) {
    if (argc != 3) {
        printf("PVSPIN_FAIL usage\n");
        return 2;
    }
    unsigned long long pa = strtoull(argv[1], NULL, 0);
    unsigned long long span_ns = strtoull(argv[2], NULL, 0);

    int fd = open("/dev/mem", O_RDONLY | O_SYNC);
    if (fd < 0) {
        printf("PVSPIN_FAIL open_devmem\n");
        return 3;
    }
    void *m = mmap(NULL, PVCLOCK_PAGE_LEN, PROT_READ, MAP_SHARED, fd, (off_t)pa);
    if (m == MAP_FAILED) {
        printf("PVSPIN_FAIL mmap\n");
        return 4;
    }
    volatile const unsigned char *page = (volatile const unsigned char *)m;

    uint32_t abi = *(volatile const uint32_t *)(page + ABI_VERSION_OFF);
    if (abi != PVCLOCK_ABI_VERSION) {
        // Never spin against a page we cannot read: that would hang, and a hang
        // is indistinguishable from the frozen-page failure this gate hunts.
        printf("PVSPIN_FAIL abi %u\n", abi);
        return 5;
    }

    // ---- the measured window: no syscalls, no rdtsc, only page loads --------
    uint64_t vns0 = read_vns(page);
    uint64_t vns1 = vns0;
    uint64_t iters = 0;
    while (vns1 - vns0 < span_ns) {
        vns1 = read_vns(page);
        iters++;
        // The page clock must be MONOTONE. If it moved backward, unsigned
        // `vns1 - vns0` would wrap to a near-`UINT64_MAX` value, the loop
        // condition would read false, and the spin would exit as a false
        // PVSPIN_DONE — a determinism/ABA bug masquerading as liveness. Detect
        // the backward step explicitly and fail (r15 P2); the host G3 gate keys
        // on PVSPIN_DONE, so this failure is surfaced, never mistaken for a pass.
        if (vns1 < vns0) {
            printf("PVSPIN_FAIL backward %llu %llu\n", (unsigned long long)vns0,
                   (unsigned long long)vns1);
            return 6;
        }
    }
    // ---- end of the measured window -----------------------------------------

    printf("PVSPIN_DONE %llu %llu %llu\n", (unsigned long long)iters,
           (unsigned long long)vns0, (unsigned long long)vns1);
    return 0;
}
