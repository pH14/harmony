<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# Measured-constants pack (Zen 2 core, Ryzen 3600) — `docs/AMD-EPYC.md` DoD #2

The `SimCpu`/`PlannerConfig` re-parameterization inputs, measured on real silicon, never
inherited from Intel (doc §2). **HARDWARE FLAG:** this is a Zen 2 *core* (Ryzen 3600), so
these are first-class Zen-2-core constants; they re-confirm on a Zen-2 EPYC unchanged, but
a Zen 3/4/5 part re-measures (the event encoding and PMU model are per-generation facts).

## Pinned work-event encoding (AE-0)

| field | value | evidence |
|---|---|---|
| event | `ex_ret_brn_tkn` (retired **taken** branches) | `results/ae-0/capability-truth-table.json` |
| raw encoding | `event=0xc4` (Zen `0xC4`-family, umask 0) | AE-0 openable + exact + overflow-delivers |
| PMU model | **legacy per-counter** `PERF_CTL`/`PERF_CTR` (`0xC001_020x`); **no PerfMonV2** | AE-0 (leaf `0x8000_0022` absent) |

Never assumed — pinned per Zen generation at AE-0 and verified openable as a pinned,
non-multiplexed `perf_event_open` whose trivial overflow delivers a sample (20/20).

## Count exactness + per-class offsets (AE-1(a), ~5000 clean windows, zero mismatches)

The count for a payload class is `taken_per_iter * n + const_offset`; the differential
cancels `const_offset`, and every clean (interrupt-free) window's differential equals the
analytical oracle **exactly** (0 mismatches across all classes at 3000 reps each).

| class | taken/iter (oracle) | const_offset | clean windows | clean mismatches |
|---|---|---|---|---|
| `loop_backedge` | 1 | 5 | 1044 | 0 |
| `straight_line` (16 ALU ops/iter) | 1 | 5 | 1053 | 0 |
| `branch_dense` (8 jmp + backedge) | 9 | 5 | 876 | 0 |
| `call_ret` (call+ret+backedge) | 3 | 6 | 1057 | 0 |
| `locked` (LOCK add + backedge) | 1 | 5 | 1056 | 0 |

**Event density = 0-or-1 taken branch per instruction, confirmed:** `straight_line`
(`loop_backedge` + 15 extra non-branch ALU ops) has the **same** taken/iter and offset —
16 non-branch instructions contribute exactly 0 to the count. The `CpuBackend` contract
(monotonic, 0-or-1-per-instruction `u64`) holds on Zen 2 for this event.

**Guest-mode exactness (AE-1(b), `kvm-guest-hammer`):** the minimal single-vCPU SVM
harness runs a real-mode loop of known taken-branch count under `KVM_RUN` and counts
guest-only `ex_ret_brn_tkn` (`exclude_host=1`). Over 1000 runs (all attested to reach
`KVM_EXIT_HLT`, so no silent fault passed), **355/355 interrupt-free windows are exact**
(differential == oracle), matching the host-side result — guest-mode counting is
bit-exact on Zen 2 SVM. (Contaminated windows carry the same ~1-count-per-interrupt host
perturbation as host-side; the VM-entry/exit boundary adds no extra jitter on clean
windows.)

**Contamination model (accounted, not a defect):** each async interrupt leaks ~1 taken
branch into the CPL3 count; the differential jitter scales with window length at constant
branch structure. On interrupt-free windows the count is bit-exact — so at-scale exactness
requires core isolation (the deterministic backend runs the vCPU on an isolated core), a
recorded standing condition, the AMD analogue of the box's interrupt-steering discipline.

## Zen skid_margin (AE-1(d), 10^6 armed overflows)

The hardware PMI-delivery skid — retired taken branches between the counter crossing the
armed period and the PMI sample being recorded — measured **constant across periods**
(5k/10k/50k/100k all ≈ 1480 mean), so it is the HW skid, not a period artifact:

| statistic | value (10^6 arms) |
|---|---|
| skid mean | **1496** |
| skid min | 0 |
| skid max | **5043** |
| distribution | 99.95% in [1000,9999); tail max 5043; none ≥10⁴ |
| overflow multiplicity | 1,000,000 armed / 1,000,000 delivered / **0 lost / 0 duplicate** |

**Candidate Zen `skid_margin` ≥ 2 × max = ~10,100** (round up to **16384** for headroom),
following the Intel discipline (`run_until.rs SKID_MARGIN = 256 = 2 × 128`). This is
**~10× the Intel skid** (~128) — a first-class Zen-vs-Intel difference (doc §2): the
planner must arm this much earlier and single-step the residual. The single-step primitive
that finishes the landing is AE-2's ruling.

## Single-step semantics

Deferred to AE-2 (`results/ae-2/single-step-ruling.md`): provisional lead `DebugCtl.BTF`
(branch granularity == this event's granularity) + `RFLAGS.TF` residual; ranked ruling
pending the on-silicon `#DB`-under-SVM characterization.

## Reproduction

```sh
ssh harmony-amd 'bash ~/amd-epyc-spike/host/run-ae1.sh --core 2 --event 0xc4 \
    --reps 3000 --overflow-reps 1000000 --runset full'
# floors recomputed from retained records by schemas/check-floors.py (see floor-check.txt)
```
