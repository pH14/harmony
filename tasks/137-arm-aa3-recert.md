# Task 137 — ARM AA-3 on-silicon re-certification (mechanical re-run)

**Bead:** hm-idb (the ARM spike execution; AA-3 re-cert rides it). **Binding:** `docs/ARM-ALTRA.md` §AA-3. This is a **MECHANICAL RE-RUN**, not new design or re-scoping — the AA-3 mechanism (patched work-counter overflow → deterministic in-kernel exit) and its harness already exist and are merged; this task re-verifies the AA-3 acceptance **on silicon** on the currently-loaded aa3preempt kernel, because the prior GO was **VOIDED (Paul 2026-07-18)** pending on-silicon re-cert.

## Why this exists
AA-3's physics were re-verified but its GO certification was VOIDED (nested-x86 PR-98 precedent: "results retained; certification pending; mechanism presumed sound"). The prior run did ~1.01M armed landings with solo==co-tenant MATCH. This task re-runs that verification on-silicon to un-void the cert.

## Scope — RE-RUN ONLY
Use the **existing** AA-3 landing harness (`spikes/arm-altra/harness/...` `arm_spike` / the AA-3 stage runner — the one merged with the spike). Re-run it on the box. Do **NOT** modify the mechanism, the harness logic, the comparators, or the acceptance criteria. If the harness needs a trivial invocation-path fix to run, that's fine; anything beyond that is out of scope → PARK + escalate.

## Acceptance (docs/ARM-ALTRA.md §AA-3, verbatim)
- **≥10⁶ armed deadlines cumulative** with `work == target` on **every** landing (deltas 1..100k; MTF-analogue-edge / skid-bracket / pure-overflow classes interleaved; payload classes incl. targets adjacent to counted+uncounted instructions and on both sides of exceptions).
- **Never overshoot**; **replay-identical** landed-state digests; **skid never exceeds the AA-1 margin**.
- **solo == co-tenant**: the solo and co-tenant runs must produce identical state_hash per the standing ruling (co-running is a determinism STRESS-TEST — divergence is a P0 STOP, never serialize-to-hide). Pin per `docs/BOX-PINNING.md`; the memory rule: parallelize across leased cores, solo==co-tenant MUST hold.
- **mechanism-attestation**: every landing carries the claimed patched Preempt exit + patched-module identity (the PR-98 lesson: the harness must be structurally unable to fall back to stock and still pass).
- **Recorded rulings that bind the disposition**: BR_RETIRED = ALL retired branches, PER-ARCH; use the exact-Moment / grid dispositions as previously ruled. Do not re-open these — apply them.

## Disposition
Record the AA-3 re-cert disposition in `docs/ARM-ALTRA.md` + the evidence dir: **GO** (acceptance met on-silicon → un-voids the cert), **PROVISIONAL GO** (clean but bounded, name it), or **STOP/NO-GO** (PMI-to-exit non-deterministic, or landing overshoots irreducibly, or solo≠co-tenant → **P0, PARK + escalate to Paul immediately, do NOT rush a determinism-core decision unsupervised**).

## Environment
Box: `ssh harmony-arm` (Ampere Altra / N1). **It is ALREADY on the patched `6.18.35-aa3preempt` kernel** (loaded from tonight's AA-6 window; no reboot needed — verify `uname -r` = aa3preempt before running; if it somehow reverted, grub-reboot boot-once into aa3preempt, stock stays default). Pin every run with `taskset` per `docs/BOX-PINNING.md`, SMT sibling idle; parallelize solo+co-tenant across distinct leased cores. Bundle-transfer code (git push to box is classifier-blocked). **Commit+push evidence promptly** (box was account-wiped once 2026-07-20). Smoke-fire-once (a small landing batch) before the full ≥10⁶ run. When done, leave the box on aa3preempt (do NOT revert) and report. **You are on Opus 4.8** deliberately (low-level virt GO-cert class).
