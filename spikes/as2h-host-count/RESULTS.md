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

## H3 — kpc configurable counters are blind to guest execution (NO-GO)

Run as root on the M4. `INST_BRANCH` on a configurable counter, thread-scoped,
across EL masks {EL0A64, EL1, EL0A64|EL1} × N ∈ {0, 1M, 10M}, 500 reps each:
the counter reads **exactly 74 in every configuration** (100% mode, usually
distinct=1) — the constant host-userspace sliver around `hv_vcpu_run` — while
the guest retired 1M–10M branches. `INST_ALL` likewise reads a constant 312.
In the same windows the fixed counter saw the guest's 2M/20M instructions.

Conclusions: XNU disables/swaps **configurable** counters in guest context but
keeps **fixed** counters (cycles, instructions) running; the config-word EL
bits made no observable difference. Host-side retired-branch counting via
stock kpc is dead.

## H3b — no remaining kpc mechanism (root, M4)

`pmu_version=2`; fixed: 2 counters / **0 configs**; configurable: 8/8;
power: 0/0; **rawpmu: 0/0 (class not implemented on ARM64)**. There is no
stock-kernel interface left that could EL-filter or guest-scope a counter.
With AS-2's in-VM NO-GO, the hardware options on stock macOS are **exhausted**.

## H4a — the contamination is a family of state-dependent entry paths

Fixed-counter windows over N ∈ {0.5M..16M} × 300 reps plus paced variants
(1ms / 10ms sleeps between windows):

- The clean floor is globally exact: `floor(N) − 2N` is one of a small
  discrete set of path lengths — **5770 (minimal), 5778 (+8), 5852 (+82)** —
  and the guest-work slope is exactly 2 instructions/iteration across a 32×
  range on top of whichever path fired.
- Which path fires depends on **ambient machine state, not N**: +74-class
  offsets dominated N=1M yesterday (95%) and vanished at N=1M today; after
  1ms idle gaps 247/300 windows ran +8; after 10ms gaps a +165-ish cluster
  appears (deeper idle ⇒ longer wake/entry work charged to the thread).
- Rare large offsets (+5.8K–9.8K) are real async events (timer ticks),
  Poisson-ish, more frequent in longer windows, absent when the box is quiet
  (tickless idle).

## Verdict and design implications for an instructions-retired approach

What exists on stock macOS: an **exact-when-clean, no-root, per-thread
instructions-retired counter with 1:1 guest attribution**, contaminated
additively by (a) a discrete family of guest-entry path costs selected by
ambient host state and (b) rare async kernel events — neither filterable,
both landing on the same counter as guest work.

- **Usable:** as the *hint* in an overshoot-impossible landing design
  (max-retire-rate-bounded chunked stops + AS-4-style single-step final
  approach, where the authoritative position comes from stepping, not the
  counter). Contamination costs performance, never correctness.
- **Not usable as-is:** as the *authoritative record* V-time (assigning
  Moments to observed events). Contamination is state-dependent, so
  dual-run-agreement filtering has correlated failure modes (paced idle
  reproduces the same +8 in both runs), and record/replay/cross-machine runs
  do not share ambient state.
- Any adoption of an instructions clock in place of retired branches on this
  backend is an explicit contract ruling (GLOSSARY/SCORING lineage), never a
  silent substitution.

Scope of all conclusions: Mac16,10 / M4 / macOS 26.5 and J316c / M1 Max /
macOS 26.4.1, stock kernels, public + private-framework interfaces, root
where noted. A future macOS could reopen any of these doors; the probes here
re-answer the question in minutes.
