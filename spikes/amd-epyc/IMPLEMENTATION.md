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
| **AE-2** | **PROVISIONAL GO** | Ruled **TF** (via `KVM_GUESTDBG_SINGLESTEP`), **not BTF** — refuting the doc's provisional lead on silicon: TF is exact + guest-transparent (`tf_kept=0`); BTF delivers 0 `#DB` through stock KVM; MOV-SS shadow the one recorded hazard. |
| **AE-3** | **GO (mechanism) / PROVISIONAL (exact landing)** | Escalation RESOLVED (Paul 2026-07-17: build+boot 6.18.35 on this box). Patched `kvm_amd` fires **`KVM_EXIT_PREEMPT` on-silicon** every arm; skid ∈ [2581,3039] ≪ margin, never overshoots at overflow. Needed a **2nd svm.c hunk** (advertise the opt-in cap on SVM — 0003 is VMX-only). Exact single-step **landing** shows run-to-run jitter on the non-isolated core (core-isolation dependency); isolating diagnostic cut off by loss of box access. See §AE-3 execution. |
| **AE-4** | **PROVISIONAL GO** | Freeze **demonstrated on-silicon**: guest sees frozen `AuthenticAMD` + TSC bit cleared **below host** (`0x078bfbff`→`ef`); denied MSR (HWCR) RDMSR **traps to the vmm**. `det-zen2-v1` truth table ratified. |
| **AE-5** | **PARTIAL** | Substrate same-seed determinism **demonstrated (1000/1000 bit-identical** on SVM); full mini gate (work-clock preempt + `svm.c` force-exit + fault injection + postgres Subject) gated on **AE-3** + the appliance (`hm-tn9`, out of spike scope per `hm-u1n`). |
| **AE-6** | **GATED** | Nested SVM **confirmed available** (`kvm_amd nested=1`); full nested gate (consonance stack as L1) follows the AE-5 bare-metal GO + appliance. |

No **kill condition** fired: no unexplained count mismatch (the ±1 jitter is accounted host
interrupts, exactly 0 on clean windows), overflow never lost/duplicated/early, a single-step
primitive that lands exactly and is guest-hidden exists (TF), and no un-freezable
guest-visible state was found (CPUID + MSR freeze demonstrated). The one **escalation** (AE-3,
a build-environment version skew, not a mechanism failure) is recorded, not improvised around.

## Definition-of-done items

1. **Dispositions with retained machine-readable evidence** — table above; evidence under
   `results/ae-0/`, `results/ae-1/full/` (floor-checked by `schemas/check-floors.py`,
   `floor-check.txt`), `results/ae-2/`, `contract/`.
2. **Measured-constants pack** — `results/constants-pack.md`: event encoding `0xc4` (legacy
   PMU); per-class count offsets (5–6, cancel in the differential); event density
   0-or-1/instruction; **Zen skid mean 1496 / max 5043, constant across periods** ⇒ candidate
   `skid_margin` ≈ 16384 (**~10× Intel's ~128** — re-parameterize `SimCpu`/`PlannerConfig`,
   never inherit).
3. **Single-step ruling** — `results/ae-2/single-step-ruling.md`, **settled on silicon**:
   the primitive is **`RFLAGS.TF` via `KVM_GUESTDBG_SINGLESTEP`** (exact + guest-transparent),
   **not BTF** (which delivered 0 `#DB` through stock KVM — the doc's provisional lead refuted
   by data); rejected-candidate modes recorded (BTF unavailable via stock KVM; MOV-SS shadow
   coalesces a step). Not "TF and hope" — TF with the on-silicon evidence and the mov-ss hazard
   characterized (`results/ae-2/tf-characterization.json`).
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
   backend; references (never forks) `docs/cpu-msr-contract-amd-draft.toml`. **Two mechanisms
   demonstrated on-silicon (AE-4):** CPUID freeze incl. a below-host bit (`ae4-freeze`) and
   MSR default-deny trapping to the vmm (`ae4-msr`). PMU column pinned to legacy (PerfMonV2
   rows inert on Zen 2; re-confirm on Zen 4 EPYC).
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

## AE-3 execution (2026-07-17) — escalation RESOLVED, force-exit PROVEN on-silicon

Paul ruled (2026-07-17) to resolve the 6.8-vs-6.18 escalation by **building + booting the
patched linux-6.18.35 determinism kernel on this box** (a fresh Scaleway lease is blocked by
`hm-3cp` and buys nothing — no stock distro carries the determinism patches). Executed:

1. **Built a bootable 6.18.35** (`host/build-6.18-kernel.sh`): canonical determinism series
   0001-0005 + the AMD `svm.c` hunk, **both git-apply clean to 6.18.35** (verified on-box —
   the hunk did NOT need re-anchoring). Config based on the running Ubuntu config for
   boot-safety (md1 RAID1 + NVMe root), `KVM_AMD=m`, BTF/DEBUG/SIG-force off, certs cleared,
   `LOCALVERSION=` so `uname -r` is exactly `6.18.35`. `make bindeb-pkg` → 102 MB
   `linux-image-6.18.35` .deb (sha256 `dbe27df5…`), ~42 min.
2. **First-ever live boot** of the x86 6.18.35 determinism kernel (task 57 built+verified it
   but never booted it). Self-recovering GRUB one-shot (`host/stage-6.18-boot.sh`):
   `MODULES=most` initrd (verified to carry `raid1`+`nvme` — `MODULES=dep` silently dropped
   them for the software-RAID root, a real boot-safety catch), permanent saved-default = stock
   6.8, one-shot `grub-reboot` into 6.18.35. **NOTE:** `/boot` is on RAID1, so GRUB cannot
   self-clear `next_entry` (the "diskfilter" warning) — the one-shot is sticky and was cleared
   manually from Linux immediately after boot. Box returned in 135 s on `uname -r=6.18.35`,
   root on `/dev/md1`, patched `kvm_amd` (vermagic 6.18.35, `avic=N`, `nested=1`).

3. **Force-exit fires on-silicon (the escalation resolver).** `harness/ae3-forceexit.c` arms
   the `ex_ret_brn_tkn` (0xc4) overflow at `target − margin`; the guest exits with
   **`KVM_EXIT_PREEMPT` (42) on every arm**, `work_at_preempt ≤ target`, **skid ∈ [2581, 3039]
   ≪ margin (8192/16384)**, never overshooting at the overflow. A stock `kvm_amd` cannot green
   this (the harness `ENABLE_CAP`/`ARM_PREEMPT_EXIT` fail on stock) — mechanism attestation is
   structural.

**Finding — the AMD `svm.c` 0004-analogue needed a SECOND hunk.** The committed hunk added
only `nmi_interception`; `ENABLE_CAP(KVM_CAP_X86_DETERMINISTIC_INTERCEPTS)` returned **-EINVAL**
because patch 0003's `kvm_caps.has_deterministic_intercepts = true` is **VMX-only**, so the
opt-in was never advertised on SVM and the force-exit could never arm. Added the one-line
`svm_hardware_setup()` advertisement (the `vmx_hardware_setup` analogue); rebuilt the kvm
modules out-of-tree against the running patched tree and **hot-swapped** them (no reboot) —
`ENABLE_CAP` then succeeded and the mechanism fired. The corrected two-hunk patch is committed
(`host/patches/0004-KVM-SVM-…`, now VALIDATED-ON-SILICON). This is the concrete answer to
`docs/ARCH-BOUNDARY.md`'s deferred trait question for the AMD side: the SVM force-exit needs
**two** small svm.c additions beyond the shared x86 plumbing (the NMI hook + the cap
advertisement), not just the one the draft assumed.

**Residual — exact single-step LANDING jitters on the non-isolated core.** After the overflow
lands early (bounded skid), the AE-2 TF single-step advances toward `work == target`. It
reaches target most of the time, but ~30% of arms **overshoot by exactly 1** (`work_landed =
target+1`), and — more tellingly — the **full-register landed digest varies run-to-run even on
exact (`work==target`) landings** (12-run sample: every digest distinct). This is consistent
with AE-1's "async interrupts leak ~1 count" effect on this `CONFIG_HZ=1000`, non-`nohz_full`
core: the *differential* is exact (AE-1) but the *absolute* landing point has few-branch
run-to-run jitter, so the landed state is not yet bit-reproducible. This points to a
**core-isolation dependency** (`nohz_full`/`isolcpus`) for exact deterministic landing — the
same isolation the production determinism backend runs under and that AE-1 deliberately
deferred ("No isolation reboot"). The isolating diagnostic (RIP+RCX-only digest, to separate
counter jitter from harmless RFLAGS debug-bit variance) was **cut off by loss of box access**
(see below) and must be re-run.

**Box access lost mid-AE-3 (escalated).** During the landing-jitter diagnostic the box
**rebooted and regenerated SSH host keys, then rejected the spike SSH key** (`Permission
denied (publickey)`, consistent over 90 s) — a re-provision or `authorized_keys` reset, not a
transient. Per tasks/123 (escalate on box access failure) this was handed to Paul. All CODE
(build recipe, boot-staging, harnesses, the corrected two-hunk patch) is committed and
reproducible; the on-silicon numbers above are recorded from the live run before access was
lost. Re-running the full AE-3 campaign (10⁶ cumulative arms, replay-identical landing under
core isolation), AE-5, and AE-6 requires restored box access.

## (historical) The pre-ruling escalation

Before Paul's 2026-07-17 ruling, AE-3 was ESCALATED: the `svm.c` hunk was verified against real
6.8 source but the full build was blocked because the shared determinism plumbing targets ~6.18
while the box ran stock 6.8. That version skew is what the build-on-this-box ruling resolved.
The superseded out-of-tree recipe is `host/build-kvm-amd.sh` (kept for provenance);
`host/build-6.18-kernel.sh` is the recipe that was actually executed.

## Remaining box steps (for the foreman / next iteration)

- **AE-5 full mini gate + AE-6 nested** — need the AE-3 patched kernel (above) **and** the
  appliance build (`hm-tn9`, out of spike scope): the postgres Subject + work-clock-driven
  preemption via the `svm.c` force-exit + fault injection at seeded Moments, then the one-command
  demo; then its nested-SVM twin (`kvm_amd nested=1` confirmed ready). The substrate half
  (same-seed determinism, `ae5-determinism` 1000/1000) is done.
- **AE-2 tail** — the full `work == target` landing (single-step + work-counter inversion, the
  AE-3 harness) and the syscall/exception/`iret`/injected-interrupt classes (need the
  IDT-bearing protected/long-mode Subject); plus a direct guest-`PUSHF` confirmation of
  TF-invisibility. The core ruling (TF, mov-ss hazard, BTF-rejected) is settled.
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
