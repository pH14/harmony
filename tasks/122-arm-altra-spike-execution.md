# Task 122 — Execute the ARM/Altra vendor spike (hm-idb): AA-0..AA-6 on the live box

Claim `hm-idb` first (`bd update hm-idb --claim`).

## Goal

Execute the ruled ARM vendor spike program — `docs/ARM-ALTRA.md`, stages AA-0 through
AA-6 in the doc's risk-ordered sequence with its decision ladder — on the LIVE Ampere
Altra box (`ssh harmony-arm`; Neoverse N1, 80 cores, SMT off, /dev/kvm present, Ubuntu
6.8 kernel). The doc is the execution packet and is binding: the six evidence-integrity
countermeasures apply per stage (the PR-98 lesson; the tasks/102 re-cert is the
reference implementation). The paravirt work-derived-clock design (AA-5's centerpiece)
is `docs/PARAVIRT-CLOCK.md`.

## Standing disciplines

- **The box is PAID HOURLY and provisioned as a scratch machine — maximize useful
  utilization, never leave it idle while the spike is incomplete.** Batch long
  measurement runs; overlap builds with runs where independence allows.
- **Smoke-fire-once before campaign spend**: every stage's riskiest live assumption gets
  a minutes-long fire-once probe, reported, before that stage's full measurement budget.
- **CPU pinning**: every measurement workload pinned with `taskset -c <core>` to a
  dedicated core; keep cores 0-3 for housekeeping, measure on isolated high cores. No
  SMT exists on Altra — no sibling discipline needed. Record the core map you use.
- The apparatus lives at `spikes/arm-altra/` (CI-green on hosted runners); build it ON
  the box (native aarch64). The merged arm64 backend (PR #117, TCG-smoked) gets its
  first real-KVM contact through the stages that exercise it.
- Report per-stage: the doc's per-stage evidence artifacts + GO/NO-GO inputs, committed
  under `spikes/arm-altra/` per the doc's artifact conventions. PR when the ladder
  completes or a kill condition fires — either way the evidence lands.
- Escalate (do not improvise) on: kill-condition fire, box unreachable, kvm access
  failure, or any doc-vs-hardware contradiction needing a ruling.

## Environment

`ssh harmony-arm` (BatchMode works, no password). All pure-logic work stays local-Mac;
box time is for measurement and real-KVM validation only. Nimbus discipline: this box
was provisioned and authorized directly by Paul (2026-07-17, urgent bead hm-x9f) —
use ssh directly; place NO credentials or box identifiers beyond `harmony-arm` in
committed artifacts.

## Definition of done

AA-0..AA-6 evidence artifacts committed, decision-ladder outcomes recorded per stage,
`hm-idb` updated with the ladder verdicts; PR opened with the review-grounding
description. `hm-idb` closes on merge.
