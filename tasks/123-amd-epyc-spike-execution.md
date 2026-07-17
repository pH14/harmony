# Task 123 — Execute the AMD vendor spike (hm-u1n): AE-0..AE-6 on the live box

Claim `hm-u1n` first (`bd update hm-u1n --claim`).

## Goal

Execute the ruled AMD vendor spike program — `docs/AMD-EPYC.md`, stages AE-0 through
AE-6 in the doc's risk-ordered sequence with its decision ladder — on the LIVE box
(`ssh harmony-amd`; `/dev/kvm` present). The doc is the execution packet and is binding:
the six evidence-integrity countermeasures apply per stage (the PR-98 lesson; the
tasks/102 re-cert is the reference implementation). The work clock is `ex_ret_brn_tkn`
(AE-1, encoding pinned per-Zen-generation at AE-0); the SVM-side `svm.c` force-exit
0004-analogue is the real kernel-patch stage (AE-3). `docs/ARCH-BOUNDARY.md` deferred the
vendor trait freeze pending AE-3/AE-4 data — this spike produces it.

## HARDWARE FLAG (binding context, foreman 2026-07-17)

The provisioned box is an **AMD Ryzen 5 PRO 3600 — Zen 2 "Matisse", NOT an EPYC**
(6c/12t, SMT ACTIVE, Scaleway). Core-level Zen-2 facts (PMU event encodings incl.
`ex_ret_brn_tkn`, count exactness, SpecLockMap/`LS_CFG`, single-step, `svm.c` force-exit)
**transfer — this IS the Zen 2 core**, so AE-0..AE-3 core-mechanism evidence is
first-class. Platform-level facts (server RAS, SMM cadence, EPYC topology, AVIC-at-scale)
do **NOT** transfer. Per-stage discipline: record every result as "Zen 2 core (Ryzen
3600)"; **flag any measurement the doc scopes to EPYC-the-platform as PROVISIONAL** and
list it in the stage's evidence for re-confirmation on a real EPYC. If a stage is
meaningless off-EPYC, say so and skip forward rather than manufacturing evidence — a
recorded "not-answerable-on-this-part" is a valid ladder input.

## Standing disciplines

- **The box is PAID HOURLY and provisioned as a scratch machine — maximize useful
  utilization, never leave it idle while the spike is incomplete.** Batch long
  measurement runs in the background; overlap builds with runs where independence allows;
  think over logs while the next run fires.
- **Smoke-fire-once before campaign spend**: every stage's riskiest live assumption gets
  a minutes-long fire-once probe, reported, before that stage's full measurement budget.
- **Record-then-modify — provisioning IS stage AE-0.** This box is new and un-provisioned.
  Before the first change, capture the baseline manifest to
  `spikes/amd-epyc/results/box-baseline-manifest.json` per the doc's Box-discipline section
  (CPU family/model/stepping + Zen generation, microcode, kernel, `kvm`/`kvm_amd` identity
  stock-vs-patched, cmdline, governor, **AVIC posture**, **`LS_CFG`/SpecLockMap state**, SMT
  posture, core topology). That day-one baseline **is** the restore target; return the box
  to a recorded state whenever the lock is yielded and at spike end, and verify the match.
- **CPU pinning + the SMT caveat (DIFFERENT from the ARM box).** Zen 2 cores are **SMT-2** —
  the sibling-hyperthread confound is **present here**, unlike the single-threaded Altra N1.
  Pin every measurement to a dedicated physical core with `taskset -c <core>` **and idle or
  offline its SMT sibling**; record the pinned core, its sibling's state, governor, and
  frequency posture in every run's evidence. `LS_CFG` (the SpecLockMap workaround) is
  load-bearing and attested per run; the one sanctioned deviation is AE-1's bounded
  workaround-**off** probe that deliberately reproduces the overcount. AE-0 records the
  standing core-assignment table (housekeeping / measurement / guest cores + sibling map).
- **Pre-build fold-in (`hm-8v4`).** The parameterized exactness-hammer (work-clock event,
  PMI/overflow accounting, skid measurement, reusing the x86 det-corpus oracle payloads —
  same ISA) and the `svm.c` force-exit 0004-analogue DRAFT are `hm-8v4`'s scope; they fold
  into AE-1 (hammer self-test against Intel `0x1c4` first, then the `ex_ret_brn_tkn`
  encoding swap-in) and AE-3 (`svm.c` hook compiling against the pinned kernel). Claim/land
  that work through the same evidence channel; do not re-derive it.
- The apparatus lives at `spikes/amd-epyc/` — build it ON the box (native x86_64). The known-
  good x86 guest images (the pr44 postgres pair) carry over unchanged (same ISA, same
  Subject); pin every bootable artifact by sha256 and verify immediately before every boot
  (reuse `vmm-core/tests/live_dirty_remap.rs` `guest_images()`/`verify_pin`).
- Report per-stage: the doc's per-stage evidence artifacts + GO/NO-GO ladder inputs,
  committed under `spikes/amd-epyc/` per the doc's artifact conventions. PR when the ladder
  completes or a kill condition fires — either way the evidence lands.
- **pkill/pgrep landmine.** `pgrep -f`/`pkill -f` self-match wrapper argv (harness suicide +
  waiter deadlock, observed on the Intel box). Separate write and launch ssh calls, redirect
  stdin (`</dev/null`), launch long runs detached (`setsid`/`nohup`), use state-based waits
  (poll for a file/socket/pidfile), never `pkill -f` interrogation of your own command lines.
- Escalate (do not improvise) on: kill-condition fire, box unreachable, kvm access failure,
  or any doc-vs-hardware contradiction needing a ruling.

## Environment

`ssh harmony-amd` (BatchMode works, no password). Test `ssh harmony-amd true` before every
session — reachability fluctuates on every box we run; if unreachable, stop and report,
never simulate results. All pure-logic work (hammer harness logic, `svm.c` reading, oracle
payloads, contract deltas) stays local-Mac; box time is for measurement and real-KVM
validation only. Nimbus discipline: this box was provisioned and authorized directly by
Paul (2026-07-17) — use ssh directly; place **NO** credentials or box identifiers beyond
`harmony-amd` in committed artifacts (the repo hard-codes no host — extend the
`docs/BOX-PINNING.md` `DET_BOX_SSH` convention with an `AMD_BOX_SSH` variable).

## Definition of done

AE-0..AE-6 evidence artifacts committed under `spikes/amd-epyc/`, decision-ladder outcomes
recorded per stage (with EPYC-platform items flagged PROVISIONAL per the hardware flag),
`hm-u1n` updated with the ladder verdicts; PR opened with the review-grounding description.
`hm-u1n` closes on merge.
