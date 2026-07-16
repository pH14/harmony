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
Intel×virtualized: **RE-CERTIFIED 2026-07-16** (PR #98 merged, squash 4c4eb3fb) — the
2026-07-12 voiding was re-earned under tasks/102: armed-PMI floor 1,101,006 ≥ 1M from
perf records only, N-3 8/8 reference-pair pinned, floors machine-checked ALL PASS.
A cell is filled when one documented command builds the pinned stack, boots on that host,
and passes the same-seed determinism gate. "Vendor" replaces "personality" (GLOSSARY
ratified via PR #103). ARM = Linux/KVM on an incoming Ampere
Altra (Apple-silicon route dead); AMD = incoming Epyc; ARM > AMD, parallelize the docs.

**Pre-build ruling (Paul, 2026-07-13)**: build-first — box-wait converts into worker
throughput; the vendor spikes gate *trust* (measured constants, the trait freeze, the cell
fill), not construction. The ruled 5-lane queue and its risk acceptance live in
`docs/ARCH-BOUNDARY.md` §Pre-build ruling.

## In flight (1 active worker)

- **ARM backend skeleton — IMPLEMENTATION, converging** (tasks/112, `hm-cbt`, PR #117,
  spec merged as PR #111; agent-arm-backend-skeleton, Opus 4.8 xhigh): M0–M4 delivered,
  TCG-smoked; **13 review rounds done 2026-07-16**. The recurring vGIC P1 refuted twice
  against tasks/112 M2's AA-6 deferral (on the PR record). Real fixes landed across
  rounds 5–13: MMIO range/alignment fail-closed validation; GIC + doorbell components in
  `state_components()`; FDT parser bounded by `size_dt_struct`; `wrap_image` code0
  branch-over-header (+ TCG boot through the production helper); PSCI 0.2 vcpu feature;
  GIC timer-latch restore validation; **the ARM transport GPA model** (dedicated ABI
  pages at the absolute-GPA ABI addresses; `DOOR` state-hash chunk + component digest;
  control-plane + stage-time fault validation routed through the GPA resolver — x86 held
  byte-identical throughout). Latest pass (r14, head `8df9681`): zero P1s, one P2
  (pvclock helpers are the last GPA-family member) — dispatched. **Merge = clean
  re-pass + the M0 x86-neutrality box gate green on the final head** (worker holds the
  box window; it correctly refused to fabricate a run pointer when the box was busy).

Landed since the midday refresh: **conductor full-suite Miri restoration MERGED**
(tasks/104, PR #105 — 12× cut to ~11.5 min, foreman-confirmed, triple vacuity guard;
`hm-d4y` residue = the box confirmation dispatch) and the **vocabulary rename sweep
MERGED** (tasks/105, PR #106 — the GLOSSARY slate is code: campaign-runner, sdk-events,
Reproducer, Moment/Span, Subject; wire bytes golden-proven; zero findings across both
reviewers; Exemplar→Entry structural merge deferred as `hm-74w`).

**CI runner toolchain REPAIRED 2026-07-15 eve** (`hm-ph7`): reinstall done; the rerun now
*measures* again (all 1791 tests pass; line coverage 93.66%). NOT caused by the drift-gate
merge (tests/files aren't counted — verified from the lcov artifact). Drivers: a stale
`work_perf.rs` exclusion broken by the keystone move (**repaired**, beb14c6) + under-tested
code that landed while CI was fail-before-measuring (film replay bin 9.5% / core_replay
55.6%, benchcampaign 82.6%, telemetry bin 23.5%, bringup 70.2%). **⚖️ RULED — `hm-42y`
(Paul, 2026-07-16, commit babb6be):** coverage region floor **94.5 → 93.5** (accept the
dip); the measured 93.66 now passes, the ratchet doctrine is unchanged, and the driver
test-up stays an organic follow-up. Quality on main is green again.

## Ready (unblocked, waiting for a worker slot or Paul)

Reach-matrix lane (foreman-owned or spawnable next):

- **Hardware-arrival lane** — Altra arrival blocker `hm-7pb` (P1) → ARM spike execution
  `hm-idb`; Epyc arrival blocker `hm-9wt` (P2) → AMD spike execution `hm-u1n`. Paul
  closes an arrival bead when its box is racked; the execution surfaces dispatch-ready.
  Arrival day now lands on pre-built tooling: the preflight truth-table probes (`hm-69y`
  rider) and the harness lanes (`hm-8v4` / `hm-2kj`).

- **macOS-backend design exploration** (`hm-dj0`, P2, background-session filed).

General ready (foreman spawns as slots free):

- **Appliance as first-class repo build** (`hm-tn9`, unblocked by the spike merge) and
  **host-qualification preflight CLI** (`hm-69y`, same; carries the pre-build rider:
  absorb the AA-0/AE-0 capability truth tables as machine-readable GO/refuse checks).
  **harmonyd `hm-9od` stays DEFERRED** (Paul 2026-07-12: no resident daemon until a live
  consumer exists — do not auto-spawn).
- **AMD hammer variants + `svm.c` draft** (`hm-8v4`, unblocked by the spike merge — the
  hammer source is on main and the Intel box frees after the ARM M0 window).
- **W^X + rescan-on-exec** (`hm-rfz`, P2): third rung of the PARAVIRT-CLOCK §3.3
  enforcement ladder; should land before any non-fully-owned ARM guest. Substantive
  contract work — hold for a free slot after the ARM skeleton.
- **Nested-x86 apparatus hardening residue** (`hm-w9s`, P3, filed under the scope
  ruling): the 3 non-evidence-class r11 P1s (metal-restore failure corners, probe
  mandatory-key presence); opportunistic — AE-*/AA-* rebuild this apparatus anyway.

Dissonance lane (held — Paul: background reprioritization in progress, foreman does not
spawn these until that lane re-opens):

- **Dissonance document/naming convergence** — must finish before the Differential children;
  reserves counterpoint, rules `campaign-runner`, and reconciles ordering/retention/SDK contracts ·
  `hm-7zx`
- **Campaign-runner remap-factory opt-in** (task 95 M2 follow-up; the crate is
  `campaign-runner` — the `conductor` rename landed in tasks/105, PR #106) · `hm-lld`
- **Deterministic-preemption soundness gap** — oldest open debt; needs a decision, not
  deferral · `hm-5ee`
- Dormant tier (deliberately unscheduled, revisit at planning): live net-fault enforcement
  (task 61b) `hm-wvh` · HLT idle-wake
  arbitration (task 77) `hm-k37` · multi-CPU characterization (task 92, do-not-auto-spawn)
  `hm-c2b` · guest-SDK follow-ups `hm-1by` · branch
  pruning `hm-069` · inadmissible-proposal retry `hm-f30`
  (ARM OCI window `hm-e3o` CLOSED 2026-07-12: superseded by the incoming Altra box)

## Blocked (dependency edges enforce these — they surface via `bd ready` when cleared)

- **Pre-build queue** (`docs/ARCH-BOUNDARY.md` §Pre-build ruling) — largely CLEARED:
  keystone `hm-54m` (2026-07-14), paravirt clock `hm-rk5` (PR #110), contract vendor
  column `hm-0nf` (PR #116, 2026-07-16), spike-branch merge `hm-l2g` (PR #98,
  2026-07-16) all done; ARM skeleton `hm-cbt` is the one still IN FLIGHT (see above).
  Its unblocks (`hm-tn9`/`hm-69y`/`hm-8v4`) moved to Ready.
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

- **Nested-x86 re-certification MERGED — Intel×virtualized re-earned** (tasks/102,
  `hm-60k` + `hm-l2g`, PR #98 squash 4c4eb3fb, 2026-07-16 midday, ⚖️ Paul's scope
  ruling: evidence-class findings gate the merge, apparatus residue → `hm-w9s`): the
  voided ALL-GO fully re-earned — armed-PMI floor **1,101,006 ≥ 1,000,000 from perf
  records only** (Option-A top-up, fire-once smoke first), N-3 8/8 runsets at floors
  with the reference **pair** pinned, N-3 dose audit proven from retained artifacts
  (2.11× fair-share signature), honest 2^32 oracle scope correction on the record,
  dispositions from recert/top-up evidence only, invalid runsets stay marked. 12 fix
  rounds / 11 blind cross-model passes; the certification core survived every pass;
  floors machine-checked ALL PASS, re-run independently by the foreman at every head.
  Unblocks the appliance build (`hm-tn9`), preflight CLI (`hm-69y`), AMD hammer lane
  (`hm-8v4`).
- **CPU/MSR contract vendor axis MERGED** (tasks/117, `hm-0nf`, PR #116 squash 187153dc,
  2026-07-16 early): the AMD draft column on the one frozen contract — Intel canonical
  form + hash byte-identical (zero-drift rule, golden-pinned), AMD column drafted from
  the APM with every enforcement cell `verified:on-silicon-pending-AE4`, §6 markdown now
  the complete normative grammar for the AMD hashed record forms (verified:/applies-when:
  suffixes, transfer records, msr-shared explicit allowlist, committed hash + byte-exact
  golden). Six review rounds; final cross-model pass CLEAN; contract suite 52/52 on the
  merge head. AE-4 silicon verification lands with the Epyc box (`hm-9wt`).
- **Postgres-image drift gate RESTORED AND MERGED** (tasks/115, `hm-xdp`, PR #115
  squash 058ece4, 2026-07-15 late eve): live_materialization now pins its guest images
  by content hash (fail-closed on drift, negative-proven on the box in 2.15 s pre-boot)
  and defaults to the PR-44-proven `HOPS=4`. Worker finding accepted on the record: the
  Jul-9 image drift was NOT what broke `REQUIRE_DRAWS` — the stale `HOPS=3` default was;
  the drift was a separate silent hazard the pin closes. Full box gates green at default
  knobs (depth 4524 ppm vs 15463; round-trip + reproducer bit-identical); clean blind
  cross-model pass (incl. the 84-test Miri floor). `hm-2nt` (promote the Jul-9 image)
  stays open/deferred — now cosmetic-only since defaults are green. Spawn→merge ~2.5 h.
- **Stable-clippy lint sweep MERGED** (tasks/116, PR #114 squash db6549b, 2026-07-15):
  the CI-green restoration for the newer stable clippy — the two sites the tasks/115
  worker independently flagged from the box (`byte_char_slices` in control-proto codec,
  `manual_checked_ops` in campaign-runner) were verified fixed on main by this sweep.
- **THE PRE-BUILD TRIPLE MERGED 2026-07-15 eve (Paul)**: the paravirt work-derived clock
  (tasks/110, PR #110 — 21 rounds, box all-green, 24.93x workload RDTSC-exit reduction;
  `hm-rk5` closed, `hm-rfz` W^X follow-up unblocked); the ARM pre-build apparatus
  (tasks/109, PR #108 — 25 rounds, pre-silicon bar, arrival-day residue `hm-f99`;
  `hm-2kj` closed); and the ARM backend skeleton spec (tasks/112, PR #111 — Fable
  implementation spawned from it same hour).
- **SIGSTOP-cycling wedge FIXED AND MERGED same evening** (tasks/114, `hm-440`, PR #113,
  2026-07-15): the observed wedge was a single-step LIVELOCK (72% CPU, work never
  advancing after a suspend-lost work-clock completion) — now step-budget-bounded and
  fail-closed with a typed error at the run_until seam; deterministic + neutrality-tested;
  PlannerConfig stays frozen-shape. One codex P1 refuted with the process-state evidence;
  the unobserved blocked-ioctl sibling filed as `hm-efc` (P3). Spawn→merge ~1.5 h.
- **Stale insn-cpuid golden FIXED AND MERGED same evening** (tasks/113, `hm-zc2`, PR #112,
  2026-07-15): root cause = never re-blessed after the v3→v4 ARAT contract correction
  (PR #36) — NOT microcode, NOT the hm-xdp image family; refreshed via DETCORPUS_BLESS
  on the patched backend, O2 gate re-run green, cross-reboot invariance proven by the
  spike leg + this bless straddling the 07-10 reboot; full payload-hash provenance
  committed. Spawn→diagnose→bless→review→merge in ~2.5 h.
- **Campaign-runner full-suite Miri box confirmation CLOSED** (`hm-d4y`, 2026-07-15):
  foreman-dispatched nightly run 29444065376 fully GREEN on the box — campaign-runner
  step success inside the unsafe-crates Miri job (63.4 min vs the 155 ceiling), vmm-core
  Miri 45.8 min. The pinned-nightly jobs dodge the hm-ph7 stable-toolchain corruption.
- **Mac nested-KVM dev-loop probe CLOSED same hour it spawned** (tasks/111, `hm-8l3`,
  2026-07-15): honest **REFUSE** — the Mac's host stack lacks nested virtualization, so
  no local /dev/kvm dev loop; **QEMU TCG is the local oracle for the ARM ioctl/boot
  path until the Altra racks** (consequence recorded on `hm-cbt`). Zero installs, zero
  box time.

- **The ARCH-BOUNDARY restructure MERGED** (tasks/108, PR #109 squash, 2026-07-14 midday):
  the keystone Arch trait + two-level Exit + engine/vendor split + vm-state v2 arch tag;
  portable approved (2 rounds) + box gates green-for-PR (6/8, 2 failures proven
  pre-existing via main-baseline differential). `hm-54m` closed; unblocks the vmm-core
  churn lanes `hm-rk5` (paravirt clock) / `hm-cbt` (ARM backend skeleton) / `hm-0nf`
  (vendor axis) — these now surface in `bd ready`; paravirt clock needs an implementation
  task spec drafted before spawn.
- **Cloud-vendor CLI moved out-of-band** (Paul, 2026-07-14): `hm-6ge` closed — the
  budget-gated machine-lease CLI is Paul's personal toolchain outside this repo's task
  queue; this repo just consumes it. Spec committed to main for reference
  (tasks/106-cloud-vendor-cli.md, b565d58).
- ~~**Nested-x86 N-2 re-run CLOSED** (`hm-dbh`, 2026-07-14): 1,052,000/1,052,000 accounted
  at the ≥1M floor~~ — **SUPERSEDED SAME DAY**: the armed count was inflated (MTF-only
  deadlines counted); true armed PMIs 588,923 of the ≥1M floor. `hm-dbh` REOPENED,
  escalated to Paul (top-up vs criterion revision) — see the In-flight row for PR #98.
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
