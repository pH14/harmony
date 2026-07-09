# QUEUE — what's moving, what's ready, what's blocked, and why

> Foreman-maintained dashboard, regenerated each loop iteration from the beads tracker
> (`bd ready` / `bd list`; issue IDs are `hm-*`). Descriptive names first, numbers as
> anchors. If this file disagrees with `bd list`, the tracker wins and this file is stale.
> Adopted 2026-07-09 (Paul: "worth a try") to replace prose-trigger sprawl across GitHub
> issues, task-spec headers, and memory notes.

_Last regenerated: 2026-07-09 ~17:15 (foreman loop iteration 1)._

## In flight

- **NES game-workload bring-up** (task 86 M0, PR #93) — round-8 REQUEST_CHANGES posted
  (5 verified P1s, theme: vacuous box-gate paths) and dispatched to the live worker;
  ROM delivered by Paul → smoke-fire-once proceeding on the box, full campaign spend
  held until the billboard-at-seal / crash-terminal / trace-retention fixes land · `hm-ahb`
- **E-fails re-key harness** (tasks/97, PR #94) — worker done (`hm-b3h` closed: rekey
  crate + frozen corpus + REKEY-REPORT.md, all gates green); blind GPT-5.6 Sol pass
  running, foreman primary read next
- **Snapshot-store frontier D5: dirty-log capture + remap restore** (task 95 M2, PR #95)
  — worker done (`hm-b9s` closed, box gates a0/a/b + sweep green on hash-pinned images);
  blind GPT-5.6 Sol pass running, foreman primary read next

## Ready (unblocked, waiting for a worker slot or Paul)

- **PAUL: ratify a CellFn from REKEY-REPORT.md's ranked menu, or decline** — playbook
  step 4 is a human call (draw-top-64 vs v1-shipped vs draw-top-256; the twin-control
  caveat is in the bead) · `hm-5h7`
- **Marker-filter hole** — guest crash lines mint template species; v1's post-genesis
  novelty on bug 3 is the crash itself; land before the next correlation campaign · `hm-mcx`
- **Trace-retention runbook discipline** — every campaign passes `--record` from branch 0
  (bug-1 corpus is permanently un-rekeyable); also cited in PR #93 round 8 · `hm-5sv`
- **Box image drift on main** — task-78 draw-probe gate broken by the 2026-07-09 image
  rebuild; pin-by-hash ruling recorded, harness retrofit + image-content fix remain
  (`hm-xdp`, image fix `hm-2nt`)
- **Explorer sdk_events drain gap** — LinkSensor blind on the composed-engine path · `hm-r1x`
- **Conductor remap-factory opt-in** (task 95 M2 follow-up) · `hm-lld`
- **Deterministic-preemption soundness gap** — oldest open debt; needs a decision, not
  deferral · `hm-5ee`
- **Behavioral diff / findings front page** (task 83) · `hm-m78`
- Dormant tier (deliberately unscheduled, revisit at planning): OTel sensor channel
  (task 74) `hm-qdn` · live net-fault enforcement (task 61b) `hm-wvh` · HLT idle-wake
  arbitration (task 77) `hm-k37` · multi-CPU characterization (task 92, do-not-auto-spawn)
  `hm-c2b` · ARM window next steps `hm-e3o` · guest-SDK follow-ups `hm-1by` · branch
  pruning `hm-069` · inadmissible-proposal retry `hm-f30`

## Blocked (dependency edges enforce these — they surface via `bd ready` when cleared)

- **Bounded box confirmation of a ratified CellFn** ← Paul's ratification ruling · `hm-5rt`
- **Vocabulary rename sweep** (conductor→counterpoint, Environment→Reproducer,
  VTime→Moment/Span, Machine→Subject) ← the game-workload merge (open branches would
  conflict with a crate rename) · `hm-u7q`
- **LAYERS spec reconcile** ← rename sweep · `hm-4o4`
- **Selector chain** (post-NO-GO): selector artifact `hm-bfr` ← signal iteration; then
  exact-pct `hm-6rv`, triage suite `hm-4xe`, game-workload selector referendum `hm-2su`,
  exploration-gate implementation `hm-cs5`

## Recently done (this week)

- **Paul-return queue fully cleared 2026-07-09 eve**: foreman skill posture patch applied
  (commit 80f4e50), SMB ROM delivered (`bd memories smb-rom-location`), legacy GitHub
  issues #34/#64/#70/#74/#77 closed with bead pointers
- **Codex cross-model pass now GPT-5.6 Sol** (config + skills + CLI wrapper fix — see
  `bd memories codex-cli-5-6-sol`)
- **E-fails re-key harness delivered** (tasks/97, `hm-b3h`): the NO-GO's prescribed
  offline iteration loop exists and ran; ratification menu on Paul's desk (`hm-5h7`);
  spun out `hm-mcx`, `hm-5sv`, `hm-5rt`
- **Snapshot-perf M2 delivered** (`hm-b9s`): O(dirty) seal + remap restore, box gates
  green on content-hash-pinned images (the `hm-xdp` discipline, now in-harness)
- PR #96 opened in error (duplicate of merged PR #91) and closed same iteration; stale
  merged branch `task/snapshot-store-performance` remains on origin (deletion
  classifier-blocked)
- **GO/NO-GO #2 formally closed** — benchmark + ablation merged (PR #90): NO-GO, sharpened
  (sensor behavior-neutral but weak; the ¾-exploit budget was the entire deficit)
- Snapshot-store speedups part 1 merged (PR #91); campaign stopwatch merged (PR #92)
- Film / visible replay crate — merged, live gate re-homed into the game workload (PR #87)
