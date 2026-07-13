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
Intel×virtualized proven by the nested-x86 spike (ALL GO, nested==metal, 1.01–1.08×). A
cell is filled when one documented command builds the pinned stack, boots on that host,
and passes the same-seed determinism gate. "Vendor" replaces "personality" (GLOSSARY
ratification rides the docs-landing PR `hm-2uw`). ARM = Linux/KVM on an incoming Ampere
Altra (Apple-silicon route dead); AMD = incoming Epyc; ARM > AMD, parallelize the docs.

## In flight (3 workers, at cap)

- **NES game-workload bring-up** (task 86 M0, PR #93) — worker respawned 2026-07-12 on
  Fable 5 with a rebase-first brief (PR is CONFLICTING with main); round-8 fixes +
  box-smoke fix 3 already on the branch; ROM delivered; scope per the historical fence in
  task 86 (no LinkSensor/cell-quality/selector claim). Codex pass deferred until the
  rebased head exists · `hm-ahb`
- **vmm-core Miri gate closure** (tasks/98, P1 bug) — worker agent-miri-gate (Opus 4.8)
  spawned 2026-07-12: make the exact nightly.yml command pass without losing unsafe
  coverage · `hm-4yj`
- **SpecEnvCodec fallible decode** (tasks/99, P1 bug) — worker agent-specenvcodec-fallible
  (Opus 4.8) spawned 2026-07-12: typed errors on hostile reproducer blobs, control errors
  never guest findings · `hm-5d9`
- **E-fails re-key harness** (tasks/97, PR #94) — worker done (`hm-b3h` closed); blind
  GPT-5.6 Sol pass RELAUNCHED 2026-07-12 (the 2026-07-09 runs died unreported); foreman
  primary read = next iteration's heavy op. Review context: the Differential rewrite
  supersedes its CellFnV1 ratification path but retains the corpus/evidence value — the
  read judges the code as delivered, the merge-vs-park call gets flagged to Paul if the
  supersession makes it ambiguous

## Ready (unblocked, waiting for a worker slot or Paul)

Reach-matrix lane (foreman-owned or spawnable next):

- **Strategy-docs landing PR** (`hm-2uw`, P1, foreman docs work) — the uncommitted
  2026-07-10..12 strategy docs onto a docs branch + handoff PR: reach-matrix ROADMAP,
  vendor rename + GLOSSARY ratification, APPLE-SILICON demotion, NESTED-INTEGRATION
  parked-not-ratified header. Next foreman docs slot.
- **Land the nested-x86 spike branch** (`hm-l2g`, P1) — push `spike/nested-x86`
  (b6b2a5d), handoff PR, review, merge. Unblocks the appliance build + preflight CLI.
- **ARM vendor spike doc: Linux/KVM on Ampere Altra** (`hm-x8g`, P1) ∥ **AMD vendor spike
  doc: SVM on Epyc** (`hm-wv8`, P2) — both pure doc tasks, spawnable as slots free;
  hardware-arrival day should be experiment day.
- **Paravirt work-derived clock spec** (`hm-8h8`, P2) — ARM correctness (no FEAT_ECV
  anywhere reachable) + x86 RDTSC-exit removal, one design.
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
