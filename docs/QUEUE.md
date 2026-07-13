# QUEUE — what's moving, what's ready, what's blocked, and why

> Foreman-maintained dashboard, regenerated each loop iteration from the beads tracker
> (`bd ready` / `bd list`; issue IDs are `hm-*`). Descriptive names first, numbers as
> anchors. If this file disagrees with `bd list`, the tracker wins and this file is stale.
> Adopted 2026-07-09 (Paul: "worth a try") to replace prose-trigger sprawl across GitHub
> issues, task-spec headers, and memory notes.

_Dissonance strategy/dependency section reconciled with Beads: 2026-07-12. Foreman rows
reconciled 2026-07-12 (loop iteration under the reach-matrix ruling); the tracker remains
authoritative._

Decision-gate safety: before dispatching ready work, the foreman inspects any closed decision
blocker and requires a recorded GO; it never dispatches in the same iteration that closes that
decision. NO-GO repairs or supersedes downstream edges before `bd ready` is used for dispatch.

## The Consonance north star (ruled by Paul, 2026-07-12)

**Consonance running in as many places as possible**: the reach matrix of vendors
(Intel / AMD / ARM) × forms (bare metal / virtualized). Intel×metal ships today;
Intel×virtualized: mechanism demonstrated by the nested-x86 spike (boots nested, ABI
round-trips, hash-identical on executed runs) — the ALL-GO **certification was voided
2026-07-12** (evidence-integrity review) and is being re-earned under tasks/102 (PR #98).
A cell is filled when one documented command builds the pinned stack, boots on that host,
and passes the same-seed determinism gate. "Vendor" replaces "personality" (GLOSSARY
ratified via PR #103). ARM = Linux/KVM on an incoming Ampere
Altra (Apple-silicon route dead); AMD = incoming Epyc; ARM > AMD, parallelize the docs.

## In flight (2 workers, 1 slot open)

- **Nested-x86 re-certification** (PR #98, worker agent-pr98, Fable 5) — Paul's 2026-07-12
  ruling executing on `spike/nested-x86`: harness-integrity set DONE (e0a62e2 —
  patched-backend hammer + armed-capability asserts, gate-RC propagation, independent guest
  oracle, per-record PMI accounting, retained-runset audit); N-2 re-run DONE (e492a69 —
  1,052,000/1,052,000 exact on the PATCHED mechanism, ≥1M floor met); N-3 floor matrix
  RUNNING on the box (1,000 reps/condition; solo condition 800/800 identical at the
  2026-07-13 06:15 foreman check) · `hm-dbh` evidence committed, `hm-jpu` running →
  disposition re-record + PR #98 merge = `hm-60k` → unblocks appliance `hm-tn9` +
  preflight CLI `hm-69y`
- **vmm-core Miri gate closure** (tasks/98, PR #99, worker agent-miri-gate, Opus 4.8) —
  fix set + measured-honest budgets on the branch; watching the full-green nightly
  re-dispatch (run 29236400380, ~3.5 h); foreman substantive review on CI-green ·
  `hm-4yj` (the real conductor-step shrink is filed as `hm-d4y`)

## Ready (unblocked, waiting for a worker slot or Paul)

Reach-matrix lane (foreman-owned or spawnable next):

- **Multiarch docs reconcile** (`hm-xi7`, P2, foreman docs work) — bring ROADMAP /
  ARM-PORT / ARCH-BOUNDARY to the ruled reach-matrix state (their 2026-07-09/10
  Apple-promoting drafts were dropped from PR #103), plus the two hm-2uw items the slate
  never contained: APPLE-SILICON.md demotion-status header, NESTED-INTEGRATION
  parked-not-ratified header.
- **Box-gate CLI vacuous-pass hardening** (`hm-9wa`, P1 bug, PR-93 round-9 findings) —
  next crate-code spawn; Mac-side gates immediately, box smoke waits for the nested-posture
  window to close.
- **Conductor Miri CI budget shrink** (`hm-d4y`, P1 bug) — dispatch only AFTER PR #99
  merges (same workflow files).
- **Nested-x86 spike findings** — stale insn-cpuid golden (`hm-zc2`), SIGSTOP-cycling
  wedge (`hm-440`), both P2 bugs on main.
- **macOS-backend design exploration** (`hm-dj0`, P2, background-session filed).

General ready (foreman spawns as slots free):

- **Box image drift on main** — task-78 draw-probe gate broken by the 2026-07-09 image
  rebuild; pin-by-hash ruling recorded, harness retrofit + image-content fix remain
  (`hm-xdp`, image fix `hm-2nt`)

Dissonance lane (held — Paul: background reprioritization in progress, foreman does not
spawn these until that lane re-opens):

- **Dissonance document/naming convergence** — must finish before the Differential children;
  reserves counterpoint, rules `campaign-runner`, and reconciles ordering/retention/SDK contracts ·
  `hm-7zx`
- **Campaign-runner remap-factory opt-in** (task 95 M2 follow-up; current crate still named
  `conductor`) · `hm-lld`
- **Deterministic-preemption soundness gap** — oldest open debt; needs a decision, not
  deferral · `hm-5ee`
- Dormant tier (deliberately unscheduled, revisit at planning): live net-fault enforcement
  (task 61b) `hm-wvh` · HLT idle-wake
  arbitration (task 77) `hm-k37` · multi-CPU characterization (task 92, do-not-auto-spawn)
  `hm-c2b` · guest-SDK follow-ups `hm-1by` · branch
  pruning `hm-069` · inadmissible-proposal retry `hm-f30`
  (ARM OCI window `hm-e3o` CLOSED 2026-07-12: superseded by the incoming Altra box)

## Blocked (dependency edges enforce these — they surface via `bd ready` when cleared)

- **Appliance as first-class repo build** `hm-tn9` ← spike-branch merge `hm-l2g`;
  **host-qualification preflight CLI** (`hm-69y`, renamed from "doctor") ← same.
  **harmonyd `hm-9od` is DEFERRED** (Paul 2026-07-12: no resident daemon until a live
  consumer exists; appliance ships gate mode only — do not auto-spawn).
- **ARCH-BOUNDARY restructure → engine/vendor split** `hm-b5n` ← vocabulary rename sweep
  `hm-u7q` ← game-workload merge `hm-ahb` (the shared post-merge-window slot).
- **Differential migration epic** `hm-bbx`: SDK normalization `hm-bbx.1` and the lineage/evidence-
  cut/retention spike `hm-bbx.2` follow `hm-7zx`; explicit ratification `hm-bbx.5` follows the
  spike and blocks deterministic Revision coordination `hm-bbx.3` plus atomic seal-cut capture
  `hm-bbx.6`; generic Explorer/evidence-ledger/archive integration `hm-bbx.4` follows schema, spike,
  coordinator, and seal-cut capture. A NO-GO blocks or supersedes DD-specific children, but retargets
  backend-independent `hm-bbx.6` under the selected alternative unless actual-seal admission itself
  is explicitly abandoned; no production child may become dispatchable while edges are repaired.
- **Frankenstein reachability inventory** `hm-dgi` ← `hm-7zx`; obsolete PCT and triage bundles
  remain blocked on this disposition rather than silently reviving after selector work.
- **Vocabulary rename sweep** (conductor→campaign-runner, Environment→Reproducer,
  VTime→Moment/Span, Machine→Subject) ← the game-workload merge (open branches would
  conflict with a crate rename) · `hm-u7q`
- **LAYERS spec reconcile** ← rename sweep · `hm-4o4`
- **First cooperative Differential exploration gate** `hm-cs5` ← `hm-bbx.4`, retention `hm-5sv`,
  and the umbrella epic. The direct retention edge prevents a plain epic close from bypassing the
  full-retention profile; the bead also builds the currently nonexistent deterministic maze guest
  and wire-v2 X/Y instrumentation. It uses the simple selector; decision `hm-yjf` must explicitly
  ratify mechanism GO before any transfer work.
- **Software-system transfer gate** `hm-ebe` ← mechanism GO `hm-yjf`; proves the same cooperative
  mechanism on a planted database/distributed-system bug; decision `hm-zlx` must then ratify
  software-transfer GO.
- **Count-based Entry-selector experiment** `hm-bfr` ← software-transfer GO `hm-zlx`.
  The old task-70 bandit/STADS/Portfolio bundle is gone. `hm-6rv` and `hm-4xe` remain blocked on
  `hm-dgi`, which must rewrite or supersede them before it closes.
- **Held-out SMB cooperative evaluation** `hm-2su` ← M0 `hm-ahb`, Differential substrate
  `hm-bbx`, and mechanism GO `hm-yjf`; advanced selector policy is optional.
- **Retention policy, finalized findings diff, and OTel evidence** — `hm-5sv` is now an epic child
  after `hm-bbx.4`; `hm-m78` follows `.4`; `hm-qdn` follows SDK normalization, the prefix spike,
  and `.4` before claiming Explorer integration.

## Recently done (this week)

- **AMD vendor spike program doc MERGED** (docs/AMD-EPYC.md, PR #102, 2026-07-13):
  AE-0..AE-6 with the six PR-98 evidence-integrity countermeasures binding per stage;
  no-MTF single-step ranked-ruling deliverable; one-command demo DoD · `hm-wv8` closed
- **Strategy-docs slate MERGED** (PR #103, 2026-07-13): DISSONANCE-STRATEGY.md (Resolution
  kept, scoped inside dissonance), GLOSSARY counterpoint-reserved + campaign-runner,
  LAYERS/SCORING reconcile, tasks/84+86 amendments, foreman decision guard. Apple-era
  ROADMAP/ARM-PORT/ARCH-BOUNDARY drafts dropped in round 1 → `hm-xi7` · `hm-2uw` closed
- **Nested-x86 re-cert duplicate bead chain closed 2026-07-13** (`hm-ymy`/`hm-wd8`/`hm-2ea`
  were parallel mints of the executing `hm-b5b`→`hm-dbh`/`hm-jpu`→`hm-60k` chain).
- **E-fails re-key harness PARKED by Paul's ruling 2026-07-12** (tasks/97, PR #94 closed
  unmerged after 5 non-converging rounds; Differential rewrite supersedes its consumer;
  corpus + hardening archived at tag `archive/task-97-rekey-harness`).
- **SpecEnvCodec fallible decode MERGED** (tasks/99, PR #97, 2026-07-12): typed errors on
  hostile reproducer blobs, full operand-pair contract property-tested · `hm-5d9` closed
- **NES game-workload M0 MERGED** (task 86, PR #93, 2026-07-12): det 25/25, film's 5
  sub-gates green (visible SMB clip), campaign report committed · `hm-ahb` closed
- **Multi-arch promotion ruled + slate filed 2026-07-12** (Paul, 12 tracker actions):
  reach matrix = the Consonance north star; NESTED-INTEGRATION parked as a product sketch
  (product undecided); Apple-silicon route dead; `hm-e3o` closed superseded; tasks/98+99
  specs pushed to main (c4c9409) making the two P1 quality-review bugs dispatchable.
- **Snapshot-store frontier D5 MERGED** (task 95 M2, PR #95, e7963b2): O(dirty) dirty-log
  capture + memslot-remap restore, box gates green on hash-pinned images.
- **Legacy scoring path retired for the Differential strategy** — `hm-5h7` and `hm-5rt`
  superseded by `hm-cs5`; post-crash marker-filter bead `hm-mcx` absorbed as an actual-seal
  regression in `hm-bbx.4`.

- **Paul-return queue fully cleared 2026-07-09 eve**: foreman skill posture patch applied
  (commit 80f4e50), SMB ROM delivered (`bd memories smb-rom-location`), legacy GitHub
  issues #34/#64/#70/#74/#77 closed with bead pointers
- **Codex cross-model pass now GPT-5.6 Sol** (config + skills + CLI wrapper fix — see
  `bd memories codex-cli-5-6-sol`)
- **Historical E-fails re-key harness delivered** (tasks/97, `hm-b3h`): useful corpus and failure
  evidence retained; its CellFnV1 ratification/live-confirmation path is now superseded.
- **Snapshot-perf M2 delivered** (`hm-b9s`): O(dirty) seal + remap restore, box gates
  green on content-hash-pinned images (the `hm-xdp` discipline, now in-harness)
- PR #96 opened in error (duplicate of merged PR #91) and closed same iteration; stale
  merged branch `task/snapshot-store-performance` remains on origin (deletion
  classifier-blocked)
- **GO/NO-GO #2 formally closed** — benchmark + ablation merged (PR #90): NO-GO, sharpened
  (sensor behavior-neutral but weak; the ¾-exploit budget was the entire deficit)
- Snapshot-store speedups part 1 merged (PR #91); campaign stopwatch merged (PR #92)
- Film / visible replay crate — merged, live gate re-homed into the game workload (PR #87)
