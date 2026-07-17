<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
# AMD vendor spike (tasks/123, `hm-u1n`) — execution write-up

Execution of `docs/AMD-EPYC.md` (AE-0..AE-6) on the **live box** `ssh harmony-amd`. This is
the review record and the ladder-verdict register. The apparatus map and commands are in
`README.md`; the measured constants in `results/constants-pack.md`.

## HARDWARE FLAG (binding, tasks/123)

The provisioned box is an **AMD Ryzen 5 PRO 3600 — Zen 2 "Matisse", NOT an EPYC** (6c/12t,
SMT active, Scaleway; microcode `0x8701034`, kernel `6.8.0-88-generic`). **This IS the Zen 2
core**, so AE-0..AE-3 core-mechanism evidence (event encoding, count exactness, SpecLockMap,
single-step surface, `svm.c` force-exit) is **first-class** and transfers to a Zen-2 EPYC.
Platform-scoped facts (EPYC topology, server RAS, AVIC-at-scale, SMM cadence) do **not**
transfer and are flagged **PROVISIONAL — re-confirm on a real EPYC** where they appear below.

## Decision-ladder verdicts

| Stage | Verdict | One line |
|---|---|---|
| **AE-0** | **GO** | Zen 2 part exposes every assumption: SVM full surface, `ex_ret_brn_tkn`=0xc4 openable/exact/overflow-delivers, legacy PMU (no PerfMonV2), single-step surface present. |
| **AE-1** | **PROVISIONAL GO** | The existential trio holds: host-side + guest-mode counting bit-exact; 10⁶ overflows exactly-once; skid bounded (max 5043); SpecLockMap overcount **not reproduced** (null). |
| **AE-2** | **REDESIGN-pending-characterization** | `DebugCtl.BTF` lead (branch granularity == the work event) + TF residual; ranked ruling awaits the on-silicon `#DB`-under-SVM data (driver apparatus pending). |
| **AE-3** | **draft authored (build/measure pending)** | The `svm.c` `KVM_EXIT_PREEMPT` force-exit analogue is only the `svm.c` hunk (reuses all shared 0004 plumbing); build + on-silicon attest is the remaining box step. |
| **AE-4** | **PROVISIONAL (skeleton done)** | `det-zen2-v1` enforcement truth table complete; PerfMonV2 rows inert on Zen 2; on-silicon freeze demo (shares the KVM harness) is the remaining box step. |
| **AE-5** | **gated** | Bare-metal mini determinism gate needs the appliance build (`hm-tn9`, out of spike scope per `hm-u1n`) + the AE-2/AE-3/AE-4 box steps. |
| **AE-6** | **gated** | Nested SVM follows the AE-5 bare-metal GO. |

No **kill condition** fired: no unexplained count mismatch (the ±1 jitter is accounted host
interrupts, exactly 0 on clean windows), overflow never lost/duplicated/early, both
single-step primitives present, no un-freezable guest-visible state found.

## Definition-of-done items

1. **Dispositions with retained machine-readable evidence** — table above; evidence under
   `results/ae-0/`, `results/ae-1/full/` (floor-checked by `schemas/check-floors.py`,
   `floor-check.txt`), `results/ae-2/`, `contract/`.
2. **Measured-constants pack** — `results/constants-pack.md`: event encoding `0xc4` (legacy
   PMU); per-class count offsets (5–6, cancel in the differential); event density
   0-or-1/instruction; **Zen skid mean 1496 / max 5043, constant across periods** ⇒ candidate
   `skid_margin` ≈ 16384 (**~10× Intel's ~128** — re-parameterize `SimCpu`/`PlannerConfig`,
   never inherit).
3. **Single-step ruling** — `results/ae-2/single-step-ruling.md`: ranked candidates
   (A BTF lead / B TF residual / C DR7 aid / D PMC-step last-resort) with rejected-mode
   analysis; final ruling gated on the `#DB`-under-SVM box step (not "TF and hope").
4. **Trait-freeze memo (preliminary, `docs/ARCH-BOUNDARY.md`'s deferred decision)** —
   AE-1(d) shows every armed overflow stops **at or after** the armed count (skid ∈ [0, 5043],
   `skid_min = 0`, **0 early / 0 lost / 0 duplicate** over 10⁶ arms). So
   `run_until_overflow`'s **late-only-stop contract holds on SVM PMI delivery** — the `Arch`/
   `CpuBackend` trait needs **no structural change**, only the re-parameterized Zen
   `skid_margin` (DoD #2). The *final* memo is owed after AE-3 moves the exit in-kernel (the
   host-side path already exhibits the late-only property; the in-kernel path should preserve
   it with a smaller skid). No trait absorption required on the evidence so far.
5. **Contract vendor-column skeleton + enforcement truth table** —
   `contract/enforcement-truth-table.md`: `det-zen2-v1`, each row → its SVM enforcement
   backend (CPUID intercept / MSR-permission-bitmap / RDTSC/RDPMC/RDRAND intercepts), all
   available per AE-0; references (never forks) `docs/cpu-msr-contract-amd-draft.toml`. PMU
   column pinned to legacy (PerfMonV2 rows inert on Zen 2; re-confirm on Zen 4 EPYC).
6. **One-command AE-5 demo** — gated on the appliance (`hm-tn9`) and the AE-2/3/4 box steps;
   `host/build-kvm-amd.sh` is the content-pinned patched-stack build recipe it will drive.
7. **Box baseline** — `results/box-baseline-manifest.json` is the day-one restore target;
   `LS_CFG`/AVIC/SMT/governor postures recorded; box **returned to and verified at baseline**
   after every run and at spike end (`capture-baseline.sh --restore-view` diff clean).

## What was actually measured (the trio, in one place)

- **AE-1(a) host-side exactness** (`amd-hammer --mode exactness`, `results/ae-1/full/ae1a.json.gz`):
  5 analytical-oracle payload classes × 3000 reps; **0 mismatches over ~5000 interrupt-free
  windows**, offsets stable, no multiplexing. Async interrupts leak ~1 count each (accounted,
  scales with window length) — the AMD analogue of the core-isolation discipline.
- **AE-1(b) guest-mode exactness** (`kvm-guest-hammer`, `ae1b.json.gz`): minimal single-vCPU
  SVM harness; 1000 runs all attested to `KVM_EXIT_HLT`; **355/355 clean windows exact** —
  guest-mode counting is bit-exact, matching host-side.
- **AE-1(c) SpecLockMap** (`ae1c-{off,on}.json.gz`): the `locked` class with `LS_CFG` bit 54
  OFF vs ON — **both exactly 20000** over ~1050 clean windows each. **NULL result: no overcount
  on this Zen 2 for `ex_ret_brn_tkn`.** The workaround is kept as a harmless precaution (exact
  either way) but is **not evidenced as load-bearing on this part**; flagged for re-confirm on
  other Zen generations and under lock contention.
- **AE-1(d) overflow + skid** (`ae1d.json`): 1,000,000 one-shot arms, ring-based (race-free);
  **1,000,000 delivered, 0 lost, 0 duplicate**; HW-PMI skid mean 1496 / max 5043 / constant
  across periods.

## Production-crate diffs

**None.** The entire deliverable lives under `spikes/amd-epyc/`. The `WorkSource` event-pin
was exercised by a standalone host-side hammer and a standalone C KVM harness (event as a
`--event` parameter), not by editing the `vmm-core` stack, so **no `SPIKE(amd-epyc):` marked
production edit was required.** The `svm.c` patch draft (`host/patches/`) is spike-local and
reuses the *existing* Intel `kvm-patches/patches/0001,0002,0004` verbatim.

## Residual risks & remaining box steps (for the foreman / next iteration)

- **AE-2 empirical single-step** — build `harness/singlestep-driver.c` (guest-debug BTF/TF
  `#DB` across straight-line/branch-dense/syscall/exception/`iret`/interrupt-shadow/injected
  boundaries) → ratify the BTF ruling. Apparatus vehicle exists (`kvm-guest-hammer`).
- **AE-3 build** — `build-kvm-amd.sh` needs a kernel tree matching the shared Intel patches
  (0001/0002/0004 target a newer tree than `linux-source-6.8`); resolve the version delta or
  rebase the shared hunks, then build + attest the patched `kvm_amd` (avic=0) via the harness.
- **AE-4 enforcement demo** — extend `kvm-guest-hammer` with `KVM_SET_CPUID2` +
  `KVM_X86_SET_MSR_FILTER` and a CPUID/MSR-probe guest to demonstrate the freeze (incl.
  below-host bits) and default-deny.
- **EPYC re-confirmation** — every platform-scoped row (AVIC-at-scale, EPYC topology,
  PerfMonV2 on Zen 4) re-measures on a real EPYC; the core-mechanism constants do not.
- **SpecLockMap second data point** — the null overcount wants a contended-lock / second-Zen-
  generation cross-check before concluding the workaround is universally unnecessary.

## Judgment calls

- **`bd` vs the doc's "No Beads":** the spike doc forbids `bd` during execution, but tasks/123
  re-homes the spike into the task workflow (claim `hm-u1n`, PR, close-on-merge). Reconciled by
  using `bd` only for the task lifecycle handshake (claim + final verdict) and keeping all
  durable state in the evidence dirs, per the doc.
- **No isolation reboot:** CONFIG_HZ=1000 makes >1 ms windows always tick-contaminated. Rather
  than risk a remote-box reboot with `nohz_full`/`isolcpus`, exactness is proven on sub-ms
  interrupt-free windows with the contamination accounted — a lower-risk path to the same
  bit-exact conclusion the deterministic backend reaches via core isolation.
- **Overflow harness rewrite:** the first SIGIO-based skid measurement was race-polluted
  (coalescing signals mis-timed the counter read); rewritten ring-based (kernel records the
  value at the PMI) for a precise, race-free skid — the earlier variant is not in the evidence.
