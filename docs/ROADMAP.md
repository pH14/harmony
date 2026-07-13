# Roadmap — where the project actually is

This is a **current-state** page, not a sequenced backlog — that history lives in git (the
old "Sequenced backlog" table tracked waves 1–2 and is superseded many times over; see
`git log -- docs/ROADMAP.md` if you need it). For the reasoning behind the state below, see
`docs/REVIEW-2026-07.md` (the 2026-07 strategic review); this page is REVIEW-2026-07's
conclusions kept current as Wave 4 lands.

## Wave 3 — closed

Deterministic real-Linux boot, then a full stack-height escalation, each deterministic-twice
(bit-identical serial + `state_hash`): bare payloads → real Linux boot (task 34) → bare Postgres
(task 37) → runc OCI (task 48) → single-node k3s with an intra-guest CNI (task 49), including
`CONFIG_SMP=y`-built kernels (task 56; see the single-vCPU ruling below). **The determinism
thesis is proven at full stack height; the existential risk the project was founded on is
retired.**

## The 47–57 arc — merged and box-verified

The V-time / preemption / SMP work that made Wave 3 possible, all merged:

- **Deterministic preemption timer** (task 47, `InjectionPlanner`) and **idle-HLT resume**
  (task 52, `IdlePlanner`/`resume_idle`) — execute-to-event and jump-over-event duals over one
  `VClock`, integer-only, Kani-proven.
- **Deterministic vPIT tick** (53) and **LAPIC V-time tick** (54) — the timer devices riding
  V-time instead of host time.
- **Deterministic force-exit preemption** (55) and **SMP-cpuset k3s bring-up** (56) — the SMP
  idle-HLT root cause (MADT/ARAT) fixed, k3s runs deterministic-twice on an SMP-built kernel.
- **Canonical determinism kernel port** (task 57) — the linux-6.18.35 canonical port of the
  determinism patches (0004/0005) that closed harmony#34's known hole (disclosed inline,
  gate-caught, not silently patched over).

## Wave 4 — "close the loop" (current frontier)

**The product loop has never run once.** Wave 3 proved the substrate; dissonance's four crates
(`environment`, `control-proto`, `explorer`, `flow`) are built to high quality against an
in-crate toy, but zero lines of `consonance/` depend on any `dissonance/` crate — no control
server, no explorer→VM adapter, no fault enforcement on any real path, no coverage producer. The
organizing principle for Wave 4: **the smallest end-to-end bug-finding loop on real software**,
with everything not serving it explicitly deferred.

| # | Task | What it proves | Status |
|---|---|---|---|
| 58 | control server in vmm-core + socket-backed `Machine` adapter, seed-driven only | the explorer drives a real VM: snapshot → branch(seed′) → run → hash → replay | frontier, not yet started |
| 59 | host-plane enforcement (`perturb`: CorruptMemory + InjectInterrupt at a `Moment`) | first real fault vocabulary, zero guest cooperation | frontier, not yet started |
| 60 | first campaign: planted bug, crash oracle, `Recorded` env, replay N/N | harmony finds and reproduces its first bug | frontier, not yet started |
| 61 | net vertical: `net_decide` service + in-guest flow agent + `flow` shell | first true guest-plane path; hypercall stack gets a consumer | frontier, not yet started |
| 62 | doc-debt sweep + the SMP/single-vCPU + task-90 rulings | docs stop lying to newcomers and to the foreman | this document |
| 77 | unify HLT idle-wake arbitration with IRQ service (`vmm-core`) | the wake-source set stops being duplicated with different membership; safe before 61 adds device IRQs | queued (integrator feedback 2026-07-02); land before 61 |
| 93 | resolve compose-vs-genesis-only | the reproducer model is real, not toy-tested | **resolved — keep `compose`, genesis-only rejected** (PR #39; see `docs/DISSONANCE.md` §"Ruling (task 93)") |
| 94 | rename: Variation→Modulation, Theme→Progression (docs/specs/code) | one vocabulary across the project | **re-sequenced (foreman, 2026-07-02): right after 58 + 64 merge, before 65+ spawns** — both `docs/EXPLORATION.md` and `docs/RESOLUTION.md` are already post-rename, and 64 refactors the explorer the rename touches (64's spec says coordinate) |

Task 93 (reproducer model) was un-deferred and resolved as part of the Wave-4 push so task 58
doesn't bake in the wrong seam — see `docs/DISSONANCE.md`'s reproducer section for the ruling.

## Wave 5 — exploration (design ruled 2026-07-01, merged as PR #43)

`docs/EXPLORATION.md` rules the search-and-scoring architecture behind the Progression's three
seams: the live/replay-plane split, Sensor → Cell → Archive scoring, parent-rooted virtual
exemplars + lazy materialization (composing via the task-93 `compose` contract), the
Tactic/Selector decomposition of `Strategy`, and a phased plan with two GO/NO-GO gates. Tasks
**63–76** (phases A–J): 63 seal-validation `[GO/NO-GO #1]` · 64 spine (the keystone contract) ·
65 RunTrace · 66 matcher DSL · 67 logtmpl/CellFn · 68 lazy materialization · 69 seeded-bug
benchmark + correlation `[GO/NO-GO #2]` · 70 selector-bandit · 71 regime tactics · 72 exact PCT ·
73 guest SDK · 74 OTel channel · 75 oracles · 76 triage. Dispatch order: 63 ∥ 64 first (63 also
needs 58 merged); 65 after 64+58; 66/67 after 64; 71 after 64; 68 after 63 = GO; 69 after
60+65+67+68; 70/72 only after 69 = GO; 73→74 and 75→76 off-path. **Don't build past a GO/NO-GO
without passing it.**

## Resolution — the epoch loop (design ruled 2026-07-02)

`docs/RESOLUTION.md` rules the third loop above Modulation/Progression: **resolution**, the
judgment layer of dissonance — an agent (or a human through one) that investigates findings at
exact `Moment`s and re-instruments the search between campaigns. Key rulings: part of
dissonance, an outer loop around the explorer (peer only at the socket); an intervention is
search-surface iff recorded as an `Environment` `Action`; `exec`-in-fork is an **improvisation**
(off the record, taint-guarded, exempt from the determinism discipline); instrumentation
mutation is first-class and epoch-grained; humans steer the LLM, not the VM. Tasks **80–83**
(numbers 63–76 are the Wave-5 exploration queue; 77–79 left as a gap): 80 inspection verbs + the moment
address, 81 improvisations, 82 the `dissonance/resolution` client/REPL/transcript, 83 the
findings diff. 80/81 hang off task 58 only; 83 waits on 60 + 64. Agent harness, rehearsal-mark
UI, and `donate` are deliberately unspecced until the core lands.

## Scoring — the seam's interior (design ruled 2026-07-07)

`docs/SCORING.md` is the EXPLORATION.md companion for the Scoring seam's interior: what makes a
state worth keeping (`CellFn`/`Archive`) and worth returning to (`Selector`/retention), and the
**E-fails playbook** both GO/NO-GO gates (69 M2, 84) route FAIL to but had no spec for. Seven
rulings grounded in a four-report primary-source research pass: the re-key-and-rebuild contract
(a `CellFn` change never invalidates a campaign), epoch-wise granularity control, quality as
per-cell domination (never a key dimension), the two-channel `Reward` widening with **cost ruled
out of choice**, Selector economics (cold-start smoothing → hierarchical bandit), Agamotto-shaped
seal-pool retention, and the STADS stop. Ruled by Paul 2026-07-07; task 70 implements
against it.

## The deferred register (recorded so it isn't re-derived)

- **D1 — host-side, snapshot-store-backed RAM block device.** The only way to hunt
  durability/crash-consistency bugs (tear/reorder/drop un-synced writes at a seed-driven crash
  point). Until it exists, task 60's campaign hunts the concurrency/scheduling bug class only.
- **D5 — snapshot performance** (dirty-log harvest + memslot-remap restore). Deferred until
  task 60 makes campaign throughput measurable; today restore is a full guest-RAM memcpy.
- **SDK / coverage-guided epoch.** The highest-leverage dissonance component long-term (cell
  archive over AFL-style novelty; log/event-template + sometimes-assertion feedback channels;
  bursty/regime-based entropy — see the 2026-07 fuzzer research) — strictly after the
  seed-driven loop works. The explorer's current AFL-shaped corpus gets no further investment
  before that redesign.
- **ARM vendor: Linux/KVM on Ampere Altra** (`docs/ARM-ALTRA.md`, ruled 2026-07-12) — a
  reach-matrix cell (see the vendor × form matrix below), gated on the Altra box arriving
  (bead `hm-7pb` → execution `hm-idb`). Altra has no FEAT_ECV, so the clock question runs
  through the paravirt work-derived clock design (`docs/PARAVIRT-CLOCK.md`). The
  Apple-silicon route is DEAD (`docs/APPLE-SILICON.md`, archaeology only). The **ISA seam
  design is ruled** (`docs/ARCH-BOUNDARY.md`): the engine/vendor split's reserved names
  wait for the ARM window; all ARM-side building stays spike-gated per `docs/ARM-ALTRA.md`.
- **AMD vendor: SVM on Epyc** (`docs/AMD-EPYC.md`, ruled 2026-07-13) — the second
  reach-matrix vendor cell, gated on the Epyc box (bead `hm-9wt` → execution `hm-u1n`).
  ARM > AMD if bandwidth forces a choice; parallelize when both boxes are up.
- **Task 92 — multi-CPU/backend characterization registry** (probe → select → validate),
  fixing the one-box bus factor on the single destructive `det-cfl-v1` baseline. Deferred behind
  Wave 4.
- **Task 43 — `harmony-linux` tier / guest SDK crate.** Deferred; `harmony-kubernetes` (a
  higher guest layer) waits on this landing first.
- **Real multi-vCPU** — deferred, not foreclosed; see the single-vCPU ruling below.

## Ruling: single-vCPU is the v1 contract

Task 56 shipped a `CONFIG_SMP=y` guest (`maxcpus=1`) without any doc acknowledging that "one
vCPU, period" — load-bearing in the CPU/MSR contract topology, DISSONANCE.md's one-outstanding-
decision model, and `Moment = InsnCount` — had been quietly cracked. **Integrator ruling
(task 62):** the v1 contract is an **SMP-built kernel with exactly one *online* vCPU**; real
multi-vCPU is out of scope until explicitly re-ruled — deferred, not foreclosed (deterministic
SMP is a potential edge over Antithesis, which is single-core-pinned; see REVIEW-2026-07 gap 5).
Recorded verbatim in `docs/DISSONANCE.md` and as a `docs/CPU-MSR-CONTRACT.md` margin note.

## Ruling: task 90 close-out posture

Task 90 (the harmony/consonance/dissonance/unison rename) is ~95% executed but its task file
still reads as fully pending. Its real stragglers are the `hypervizor` strings in **hashed**
build inputs (`guest/linux/lib-build.sh:48,59,60` — the task-43 landmine: changing them
invalidates `MANIFEST.sha256`). **Integrator ruling (task 62): document-as-deliberately-stale.**
A comment sits at each site plus a close-out note in `tasks/90-rename-harmony.md`; the strings
themselves are untouched by this task.

## Cross-cutting references

- **ARM / AMD vendors:** `docs/ARM-ALTRA.md` (Linux/KVM on Ampere Altra, the ruled ARM
  program) and `docs/AMD-EPYC.md` (SVM on Epyc) own the vendor spike sequences;
  `docs/ARM-PORT.md` keeps the cross-ARM mechanism analysis; `docs/ARCH-BOUNDARY.md` rules
  the ISA seam and supersedes ARM-PORT.md's pre-Wave-4 codebase survey;
  `docs/APPLE-SILICON.md` is a dead route retained as archaeology.
- **Determinism & conformance corpus:** `docs/DETERMINISM-CORPUS.md` — the plan for verifying
  the engine is itself correct (four oracles: determinism, conformance, seed-sensitivity,
  backend-equivalence). Tasks 22/23 (host `BLOCK_WRITE` + crash-consistency) are struck; the
  corpus's "real standard system" entry is Postgres-on-RAM (tasks 36–38), not
  SQLite-with-disk.
- **Dissonance design:** `docs/DISSONANCE.md` — the two-plane, two-loop bug-finder design; the
  crate registry there is kept in sync with what's actually built.
- **Code quality:** `docs/CODE-QUALITY.md` — coverage, mutation testing, determinism lints,
  wire-decoder fuzzing, Kani proofs; independent of the feature backlog above.
- **Task constitution:** `tasks/00-CONVENTIONS.md` — now includes the frontier-task class
  (box-only, spec-named surface list, box + portable-logic gates) that ~2/3 of tasks 41–57
  already followed in practice.
