# AS-2H — host-side work counting after the AS-2 nested-vPMU NO-GO

<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->

Scout results, 2026-07-11. Probes and raw JSONL evidence live beside this file
(`probes/`, `results/`). Scope: Mac16,10 / Apple M4 / macOS 26.5 (`rentamac`)
plus J316c / M1 Max / macOS 26.4.1 (local cross-check). This is a scout, not a
retained acceptance campaign; AS-program evidence discipline applies to any
follow-up campaign, not to these files.

## Context

AS-2 (nested vPMU) is a scoped NO-GO: `PMCR.N=0`, no programmable counters
inside the VM. AS-2H asks whether the **host-side** PMU surface can supply the
deterministic work counter instead. The M4's real PMU documents exactly the
needed events (kpep `as4.plist`): `INST_BRANCH` (33, retired branches),
`INST_BRANCH_TAKEN` (2192), `INST_ALL` (8); 3 fixed + 8 configurable counters.

## H1 — no-root per-thread instruction counting is floor-exact

`thread_selfcounts(1, …)` (libsystem_kernel, **no root, no entitlement**)
returns per-thread retired instructions from the always-on fixed counter;
`proc_pid_rusage` `ri_instructions` agrees exactly. Calibrated `subs/b.ne`
loop, 1000 reps per config:

| host | N | floor == mode | mode frac | slope check |
|---|---|---|---|---|
| M1 Max | 100K / 1M | 207,088 / 2,007,088 | 0.72 / 0.42 | Δfloor = 1,800,000 = 2·ΔN exactly |
| M4 | 100K / 1M | 207,070* / 2,007,020 | 0.56 / 0.77 | *100K min 207,019; ±51 kernel-path jitter in the read window |

Floor is exact and reproducible; the upward tail is async kernel events
landing in the measurement window (the read window itself contains a
`proc_pid_rusage` syscall).

## H2 — guest instructions are counted on the host thread, exactly (the headline)

Minimal EL1 guest under Hypervisor.framework (no vEL2): calibrated loop then
`HVC #0` doorbell; `thread_selfcounts` read immediately around the
`hv_vcpu_run` loop; zero syscalls inside the window; zero unexpected exits in
every run; every doorbell EC=0x16; `x0` drained to 0.

Floors, with N=0 control (PC set directly at the HVC):

| host | floor(0) | floor(100K)−floor(0) | floor(1M)−floor(0) | floor(10M)−floor(0) |
|---|---|---|---|---|
| M1 Max | 5,589 | 200,000 = 2N | 2,000,000 = 2N | 20,000,000 = 2N |
| M4 | 5,779 | 200,000 = 2N | 2,000,073 (+73) | 20,000,000 = 2N |

- **Guest EL1 execution is attributed 1:1 to the host thread's fixed
  instruction counter** — the existential premise of host-side counting holds.
- Slope is exactly 2 instructions/iteration across a 100× range on both
  machines; mode fractions 48–99.9%, floor==mode in 7 of 8 configs.
- **Open anomaly (M4 only):** a quantized **+73/74-instruction** contribution
  dominates some window sizes (mode at 100K, floor at 1M, absent at 10M).
  Consistent with a kernel-internal guest re-entry path (e.g. host IRQ
  handled without a userspace exit) landing on the thread counter. It is
  quantized and infrequent, not noise — it must be **excluded** (EL
  filtering), not modeled, because host IRQ arrival is nondeterministic.

## What this changes

AS-2's NO-GO killed "counter visible to the EL2 monitor". H2 shows the
counter exists **outside** the VM with exact guest attribution. The remaining
existential questions move to the kpc layer (root required):

1. **H3 — EL-filtered branch counting.** Configure `INST_BRANCH` on a
   configurable counter via kpc with per-EL enables such that guest EL1/EL0
   count and host-kernel does not (macOS kernel runs at EL2 under VHE, guest
   kernel at EL1 — the EL split is exactly the guest/host split). Kills the
   +74 quantum by construction if the filter bits behave. Risk: XNU's kpc may
   pin PMCR1 EL enables to host user+kernel, leaving guest EL1 invisible to
   configurable counters — the mirror image of H2. Empirical question.
2. **H4 — overflow/PMI delivery.** `kpc_set_period` + kperf action: is there
   a bounded-skid interrupt near a counter threshold usable to stop the vCPU
   (kperf sample → helper → `hv_vcpus_exit`)? Fallback: conservative-margin
   chunked stopping (wall-clock hint is allowed to be nondeterministic when
   landing is exact) + AS-4-style single-step final approach.
3. **Contract question for Paul:** the no-root fixed counter is
   *instructions*, not branches. `INST_BRANCH` needs kpc (root). If H3 shows
   EL-filtered `INST_BRANCH` works thread-scoped, the retired-branch contract
   survives unchanged; if only `INST_ALL` filtering works, adopting an
   instructions-retired V-time on this backend is an explicit ruling, never a
   silent substitution.

## Access needed

kpc configuration is root-gated; `rentamac` sudo requires a password we don't
hold. H3/H4 need either a NOPASSWD sudoers entry for the probe binaries or a
root-run helper window.
