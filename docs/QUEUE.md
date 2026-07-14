# QUEUE — what's moving, what's ready, what's blocked, and why

> Foreman-maintained dashboard, regenerated each loop iteration from the beads tracker
> (`bd ready` / `bd list`; issue IDs are `hm-*`). Descriptive names first, numbers as
> anchors. If this file disagrees with `bd list`, the tracker wins and this file is stale.
> Adopted 2026-07-09 (Paul: "worth a try") to replace prose-trigger sprawl across GitHub
> issues, task-spec headers, and memory notes.

_Dissonance strategy/dependency section reconciled with Beads: 2026-07-12. Foreman rows
reconciled 2026-07-12 (loop iteration under the reach-matrix ruling); pre-build queue
recorded and started 2026-07-13 eve (Paul's build-first ruling — `docs/ARCH-BOUNDARY.md`
§Pre-build ruling); the tracker remains authoritative._

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

**Pre-build ruling (Paul, 2026-07-13)**: build-first — box-wait converts into worker
throughput; the vendor spikes gate *trust* (measured constants, the trait freeze, the cell
fill), not construction. The ruled 5-lane queue and its risk acceptance live in
`docs/ARCH-BOUNDARY.md` §Pre-build ruling.

## In flight (1 active worker; 2 PRs in foreman review)

- **Nested-x86 re-certification** (PR #98, worker agent-pr98, Fable 5) — **⛔ MERGE HALTED
  AT THE LAST GATE, ESCALATED TO PAUL (2026-07-14 ~10:20)**: N-3 is fully green (six
  conditions 1000/1000 bit-identical, live-migration green on destination, metal reference
  at floor strength, nested==metal equal at both seeds — foreman re-ran the machine
  floor-check independently) and the box is restore-verified + FREE. But the fresh
  cross-model pass caught, and the foreman confirmed **from the retained perf records**,
  that N-2's `armed` summary counted MTF-only (no-PMI) deadlines: true armed PMIs =
  **588,923 of the ≥1,000,000 floor** (55%; `hm-dbh` REOPENED — my earlier close cited the
  inflated count). Mechanism evidence stands (all 588,923 exactly-once, exact,
  oracle-agreed); the floor as written is unmet. Per the program's own stop rule this is
  **Paul's ruling: top-up run (~750k more deadlines, hours of box time) vs criterion
  revision** — see PR #98 comment 4970278590. Worker meanwhile fixing the instrument
  (armed-PMI recompute from records, disposition walk-back, fmt/SPDX/stressor/migration/
  n5-demo/pmu-cursor findings). `hm-60k` blocked on the ruling.
- **The ARCH-BOUNDARY restructure, steps 1–4** (tasks/108; PR #109, gate `hm-54m`) —
  **PORTABLE SIDE APPROVED 2026-07-14 morning** (head 72468d5, two review rounds): round-1's
  3 cross-model P1s fixed + foreman-verified (aarch64 seam check clean and CI-gated; vendor-
  boundary interrupt admissibility; envcodec mask + 2000-seed test) plus both structural
  moves (`vendor::x86::bringup`, vendor-neutral `Vmm` error — engine names no vendor, swept);
  round-2's snapshot-state P1/P2 **ruled acknowledged-and-deferred** per the pre-build ruling
  and documented where it binds (trait hooks + ARCH-BOUNDARY §D; flagged for Paul's veto).
  Gates: 1692/1692, clippy ×3 targets, Miri ×4 crates. **BOX GATES GREEN-FOR-PR
  (2026-07-14 ~10:45)**: 6/8 outright; the 2 failures proven PRE-EXISTING by a controlled
  main-baseline differential (hm-xdp postgres-image family) — the only on-box tree
  difference was the predicted +2-byte vm-state v2 header, all hashes bit-identical.
  **MERGE-READY, awaiting Paul** (session classifier requires human merge sign-off);
  merge unblocks `hm-rk5` / `hm-cbt` / `hm-0nf`.
- **ARM pre-build apparatus** (tasks/109, `hm-2kj`) — **round-1 review POSTED** (PR #108,
  2026-07-14: 8 verified blocking threads — wrong perf_event_attr bits, probe-RC discards,
  the missing KVM_RUN loop vs spec deliverable 2, floor-checker stage-blindness incl. a
  vacuous state_digest rep floor, an elf.rs overflow panic + stripped-ELF vacuous scan,
  truth-table schema −3 mandatory rows, deny/CI/Miri gates); fix worker respawned and
  dispatched same iteration. Architecture itself is strong; one round expected.

Landed since the midday refresh: **conductor full-suite Miri restoration MERGED**
(tasks/104, PR #105 — 12× cut to ~11.5 min, foreman-confirmed, triple vacuity guard;
`hm-d4y` residue = the box confirmation dispatch) and the **vocabulary rename sweep
MERGED** (tasks/105, PR #106 — the GLOSSARY slate is code: campaign-runner, sdk-events,
Reproducer, Moment/Span, Subject; wire bytes golden-proven; zero findings across both
reviewers; Exemplar→Entry structural merge deferred as `hm-74w`).

## Ready (unblocked, waiting for a worker slot or Paul)

Reach-matrix lane (foreman-owned or spawnable next):

- **Mac nested-KVM dev-loop probe** (`hm-8l3`, P3, ~an hour) — can an aarch64 Linux VM on
  this Mac expose /dev/kvm for the ARM backend's ioctl dev loop? GO/refuse note recorded on
  `hm-cbt`; TCG stays the fallback oracle either way.
- **Hardware-arrival lane** — Altra arrival blocker `hm-7pb` (P1) → ARM spike execution
  `hm-idb`; Epyc arrival blocker `hm-9wt` (P2) → AMD spike execution `hm-u1n`. Paul
  closes an arrival bead when its box is racked; the execution surfaces dispatch-ready.
  Arrival day now lands on pre-built tooling: the preflight truth-table probes (`hm-69y`
  rider) and the harness lanes (`hm-8v4` / `hm-2kj`).
- **Campaign-runner Miri box confirmation** (`hm-d4y` residue) — one green box-dispatched
  nightly once the re-cert window frees the box (~13 min expected vs the 155 ceiling).
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
  **host-qualification preflight CLI** (`hm-69y`) ← same — now carrying the pre-build
  rider: absorb the AA-0/AE-0 capability truth tables as machine-readable GO/refuse
  checks (comment on the bead).
  **harmonyd `hm-9od` is DEFERRED** (Paul 2026-07-12: no resident daemon until a live
  consumer exists; appliance ships gate mode only — do not auto-spawn).
- **Pre-build queue, gated lanes** (`docs/ARCH-BOUNDARY.md` §Pre-build ruling): the
  paravirt work-derived clock x86-first `hm-rk5`, the ARM backend skeleton (D-list)
  `hm-cbt`, and the contract vendor column (AE-4's shape) `hm-0nf` — all three now ←
  **PR #109 merge gate `hm-54m`** (the keystone branch is done but unmerged; the
  merge-window rule holds until it lands); AMD hammer variants + `svm.c` draft `hm-8v4`
  ← spike-branch merge `hm-l2g` (the hammer source and the Intel box both free then).
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

- **Cloud-vendor CLI moved out-of-band** (Paul, 2026-07-14): `hm-6ge` closed — the
  budget-gated machine-lease CLI is Paul's personal toolchain outside this repo's task
  queue; this repo just consumes it. Spec committed to main for reference
  (tasks/106-cloud-vendor-cli.md, b565d58).
- **Nested-x86 N-2 re-run CLOSED** (`hm-dbh`, 2026-07-14): 1,052,000/1,052,000 accounted
  at the ≥1M floor on the patched mechanism, with the review-demanded instruments
  (PatchedKvmBackend hammer, per-record PMI overflow accounting, independent guest work
  oracle). Evidence on `spike/nested-x86` results/.
- **vmm-core Miri gate CLOSED** (tasks/98, PR #99, 2026-07-13, Paul ruled merge-now over
  a re-litigated codex finding): own nightly job box-demonstrated twice (~48-50 min vs a
  120-min contention-derived ceiling); both `map_memory` seams Miri-run (the new
  `Mapping::anonymous` seam + a pointer-retention backend double, foreman-executed);
  conductor's unsafe slice restored with a filter-rot guard (1.3s); full-suite debt →
  `hm-d4y` · `hm-4yj` closed
- **Box-gate CLI vacuous-pass hardening MERGED** (tasks/103, PR #104, 2026-07-13): three
  review rounds converging 2→1→0 codex P1s (pre-execution frame marker →
  `smb_completed_frames` transitions; `--tail-delta`/`--hop-delta` zero-budget holes;
  billboard-below-film's-header → `BILLBOARD_MIN_LEN` drift-pinned to film) · `hm-9wa`
  closed
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
