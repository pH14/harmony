# QUEUE — what's moving, what's ready, what's blocked, and why

> Foreman-maintained dashboard, regenerated each loop iteration from the beads tracker
> (`bd ready` / `bd list`; issue IDs are `hm-*`). Descriptive names first, numbers as
> anchors. If this file disagrees with `bd list`, the tracker wins and this file is stale.
> Adopted 2026-07-09 (Paul: "worth a try") to replace prose-trigger sprawl across GitHub
> issues, task-spec headers, and memory notes.

_Last regenerated: 2026-07-09 ~07:25._

## In flight

- **NES game-workload bring-up** — guest image, emulator core, per-frame billboard,
  boot-determinism gates, campaigns under default search, film's re-homed live gate
  (task 86 M0, Fable worker) · `hm-ahb`
- **Benchmark NO-GO report + explore-only addendum** — final read + merge as one artifact
  (PR #90) · `hm-ias`
- **Snapshot-store speedups, part 1** — bench + three opts; final read + merge (PR #91) · `hm-b3g`

## Ready (unblocked, waiting for a worker slot or Paul)

- **Paul (away, queued for his return):** supply the Super Mario Bros ROM
  (`HARMONY_SMB_ROM`) · `hm-bjc` — apply the foreman skill patch
  (`memory/foreman-2026-07-09.patch`) · `hm-svi` — approve closing the five legacy
  GitHub issues · `hm-fdk`
- **Behavioral diff / findings front page** — run-over-run New/Resolved/Ongoing view;
  was queued behind two long-done tasks and forgotten · `hm-m78`
- **Deterministic-preemption soundness gap** — oldest open debt; needs a decision, not
  deferral (gh #34) · `hm-5ee`
- Dormant tier (deliberately unscheduled, revisit at planning): OTel sensor channel
  (task 74) `hm-qdn` · live net-fault enforcement (task 61b) `hm-wvh` · HLT idle-wake
  arbitration (task 77) `hm-k37` · multi-CPU characterization (task 92, do-not-auto-spawn)
  `hm-c2b` · ARM window next steps `hm-e3o` · guest-SDK follow-ups `hm-1by` · branch
  pruning `hm-069`

## Blocked (dependency edges enforce these — they surface via `bd ready` when cleared)

- **Vocabulary rename sweep** (conductor→counterpoint, Environment→Reproducer,
  VTime→Moment/Span, Machine→Subject) ← the three in-flight merges · `hm-u7q`
- **LAYERS spec reconcile** ← rename sweep · `hm-4o4`
- **Snapshot-store frontier (D5: dirty-log capture + remap restore — seeds in minutes)**
  ← speedups part 1 + stopwatch merges · `hm-b9s`
- **E-fails signal iteration** (offline CellFn re-key over the trace corpus; spec first)
  ← benchmark report merge · `hm-b3h`
- **Selector chain** (post-NO-GO): selector artifact `hm-bfr` ← signal iteration; then
  exact-pct `hm-6rv`, triage suite `hm-4xe`, game-workload selector referendum `hm-2su`,
  exploration-gate implementation `hm-cs5`

## Recently done (this week)

- Campaign timing instrument merged — every box run now self-reports its phase
  decomposition (PR #92, task 96)

- Film / visible replay crate — merged, live gate re-homed into the game workload (PR #87)
- Exec-in-fork + lineage taint guard — merged with box gates green (PR #86, task 81)
- Signal-vs-random benchmark ruled **NO-GO**, confirmed by two independent cross-model
  audits; E-fails doctrine engaged (task 69 M2)
- Snapshot-perf and stopwatch specs merged (tasks 95, 96); both implementations landed
  same day
- Workloads-first directive recorded; game-workload dispatch split M0/M1 (amendment b1e70d3)
