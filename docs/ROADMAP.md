# Roadmap — where the project actually is

This is a **current-state** page, not a sequenced backlog — that history lives in git (the
old "Sequenced backlog" table tracked waves 1–2 and is superseded many times over; see
`git log -- docs/ROADMAP.md` if you need it). For the reasoning behind the state below, see
`docs/REVIEW-2026-07.md` (the 2026-07 strategic review); the dissonance direction is ruled in
`docs/DISSONANCE-STRATEGY.md` and the vocabulary in `docs/GLOSSARY.md`. **Current as of
2026-07-16**; the live tracker (`bd list`, dashboard `docs/QUEUE.md`) wins on anything
finer-grained.

**Where the frontier actually is now.** Wave 3 proved the substrate; Wave 4 **closed the product
loop** (the dissonance stack is wired into `consonance/vmm-core` and campaigns run on real KVM).
The two live frontiers are: (1) the **multi-vendor reach matrix** — Intel×metal ships,
Intel×virtualized re-certified (PR #98), the CPU/MSR contract's AMD column merged (PR #116), the
ARM/Altra backend skeleton near-merge (PR #117), and the GHA-hosted-vs-self-hosted CI benchmark in
flight (PR #118, `hm-w9s`); and (2) the **Differential observation/materialization migration**
(`hm-bbx`) that `docs/DISSONANCE-STRATEGY.md` rules — the SDK-cooperative testing product.

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

## Wave 4 — "close the loop" (CLOSED)

**The product loop is closed and has run on real KVM.** Wave 3 proved the substrate; Wave 4 wired
dissonance's crates (`environment`, `control-proto`, `explorer`, `flow`) into `consonance/vmm-core`:
a control-transport server, the socket-backed `Machine`/`SocketMachine` adapter, host-plane fault
enforcement, the first planted-bug campaign (found + replayed N/N), and the first true guest-plane
path (the net vertical). The organizing principle held — **the smallest end-to-end bug-finding loop
on real software** — and it now exists end to end.

| # | Task | What it proves | Status |
|---|---|---|---|
| 58 | control server in vmm-core + socket-backed `Machine` adapter, seed-driven only | the explorer drives a real VM: snapshot → branch(seed′) → run → hash → replay | **MERGED** (PR #44; box gate green) |
| 59 | host-plane enforcement (`perturb`: CorruptMemory + InjectInterrupt at a `Moment`) | first real fault vocabulary, zero guest cooperation | **MERGED** (PR #51; box record→replay closure passed) |
| 60 | first campaign: planted bug, crash oracle, `Recorded` env, replay N/N | harmony finds and reproduces its first bug | **MERGED** (PR #55) |
| 61 | net vertical: `net_decide` service + in-guest flow agent + `flow` shell | first true guest-plane path; hypercall stack gets a consumer | **MERGED** (PR #67); live net-fault *enforcement* half split to task 61b (`hm-wvh`, open) |
| 62 | doc-debt sweep + the SMP/single-vCPU + task-90 rulings | docs stop lying to newcomers and to the foreman | **done** (its earlier pass); the current-state reconcile against `docs/DISSONANCE-STRATEGY.md` is `hm-7zx` |
| 77 | unify HLT idle-wake arbitration with IRQ service (`vmm-core`) | the wake-source set stops being duplicated with different membership | queued, unimplemented (`hm-k37`, P3) |
| 93 | resolve compose-vs-genesis-only | the reproducer model is real, not toy-tested | **resolved — keep `compose`, genesis-only rejected** (PR #39; see `docs/DISSONANCE.md` §"Ruling (task 93)") |
| 94 | rename: Variation→Modulation, Theme→Progression (docs/specs/code) | one vocabulary across the project | **superseded + landed**: `docs/GLOSSARY.md` (2026-07-06) retired Modulation/Progression again to **rollout**/**`step`**, and the tasks/105 sweep (PR #106) put the GLOSSARY slate into code (`campaign-runner`, `sdk-events`, `Reproducer`, `Moment`/`Span`, `Subject`) |

Task 93 (reproducer model) was un-deferred and resolved as part of the Wave-4 push so task 58
did not bake in the wrong seam — see `docs/DISSONANCE.md`'s reproducer section for the ruling.

## Wave 5 — exploration (design ruled 2026-07-01, PR #43; production path now re-ruled)

`docs/EXPLORATION.md` rules the search-and-scoring architecture behind the search loop's three
seams (the live/replay-plane split, Sensor → Cell → Archive scoring, parent-rooted virtual
exemplars + lazy materialization composing via the task-93 `compose` contract, the Tactic/Selector
decomposition of `Strategy`, and two GO/NO-GO gates). **What landed:** the spine and its supporting
crates — 63 seal-validation (`[GO #1]`) · 64 spine (the keystone contract) · 65 `RunTrace` · 66
matcher DSL · 67 logtmpl/CellFn v1 · 68 lazy materialization · 73 guest SDK · plus 87 film and 86 M0
(the game workload). **GO/NO-GO #2 closed NO-GO** (task 69, PR #90): the sensor was behavior-neutral
but weak; the ¾-exploit budget was the entire deficit.

**The production observation/archive path is re-ruled by `docs/DISSONANCE-STRATEGY.md`.** The
`Sensor → CellFnV1 → Archive::admit` seam and the `link`-tier `GuestEvent` stream are compatibility
code; the target is normalized `SdkSchema`/`SdkEvent` (`dissonance/sdk-events`) plus a Differential
observation/materialization plane — the **`hm-bbx`** epic (SDK normalization, lineage/evidence-cut
spike + explicit GO ratification, deterministic Revision coordination, atomic seal-cut capture across
the VM control seam, generic-Explorer integration, and retention/finalization policy) → the
cooperative maze gate `hm-cs5` → held-out SMB `hm-2su` → software-system transfer `hm-ebe`. The
downstream selector/PCT/OTel/oracle/triage tasks (70/72/74/75/76) are **re-scoped, not queued as
specced**: obsolete PCT and triage bundles are held under `hm-dgi` (prune-or-quarantine) pending that
disposition, and a count-based Entry selector (`hm-bfr`) waits behind the cooperative baseline and
the two strategy GO decisions (mechanism `hm-yjf`, software-transfer `hm-zlx`). **Don't build past a
GO/NO-GO without passing it.**

## Resolution — the epoch loop (design ruled 2026-07-02; affirmed by the strategy)

`docs/RESOLUTION.md` rules the loop above the search loop: **resolution**, the judgment layer of
dissonance — an agent (or a human through one) that investigates findings at exact `Moment`s and
re-instruments the search between campaigns. `docs/DISSONANCE-STRATEGY.md` **affirms** the placement
and boundary (its "Resolution's two interfaces"): resolution stays inside dissonance, reads frozen
campaign artifacts, and drives `MomentRef` counterfactuals without reaching into a live Explorer. Key
rulings: part of dissonance, an outer loop around the explorer (peer only at the socket); an
intervention is search-surface iff recorded as a `Reproducer`/`Environment` `Action`; `exec`-in-fork
is an **improvisation** (off the record, taint-guarded, exempt from the determinism discipline);
instrumentation mutation is first-class and epoch-grained; humans steer the LLM, not the VM. Tasks
**80–83** (numbers 63–76 are the Wave-5 exploration queue; 77–79 left as a gap): 80 inspection verbs +
the moment address, 81 improvisations, 82 the `dissonance/resolution` client/REPL/transcript (the
Moment-addressed investigation instrument — built), 83 the findings diff (finalized Differential
rendering now tracked as `hm-m78`). The between-campaign advisor loop (artifact review → model/human
judgment → new `CampaignConfig`) is future work (strategy staged direction 6). Agent harness,
rehearsal-mark UI, and `donate` are deliberately unspecced until the core lands.

## Scoring — the seam's interior (ruled 2026-07-07; PARTIALLY SUPERSEDED 2026-07-12)

`docs/SCORING.md` was the EXPLORATION.md companion for the Scoring seam's interior. Its **surviving
contract** rides the strategy: genesis-rooted **recompute cells** (the plain operation — distinct
from `EnvCodec::compose`'s `Moment` re-keying, which `docs/GLOSSARY.md` split from the overloaded
"re-key"), explicit retention with distinct raw-evidence / bounded-working-set / committed-Entry /
finalized boundaries, deterministic best-per-cell **quality** domination, and human-ratified
configuration changes at sealed campaign boundaries. Its `CellFnV1`/`fold_k` channel menu, mutable
`Archive::admit`, exact two-channel `Reward`, selector-bandit coupling, and per-subtree STADS are
**historical** — the production observation/archive path is `docs/DISSONANCE-STRATEGY.md`'s
Differential plane (`hm-bbx`), not the task-70 selector. The literature and the **E-fails playbook**
(the GO-fail procedure) remain as the decision record.

## The deferred register (recorded so it isn't re-derived)

- **D1 — host-side, snapshot-store-backed RAM block device.** The only way to hunt
  durability/crash-consistency bugs (tear/reorder/drop un-synced writes at a seed-driven crash
  point). Until it exists, task 60's campaign hunts the concurrency/scheduling bug class only.
- **D5 — snapshot performance** (dirty-log harvest + memslot-remap restore). **MERGED** (task 95
  M2, PR #95): O(dirty) capture + memslot-remap restore, box gates green on hash-pinned images.
  The campaign-runner opt-in into the remap-restore factory is the remaining follow-up (`hm-lld`).
- **SDK / coverage-guided epoch — now the ruled cooperative direction.** The highest-leverage
  dissonance component long-term is now specified: `docs/DISSONANCE-STRATEGY.md`'s SDK-cooperative
  testing over normalized `SdkSchema`/`SdkEvent` and a Differential observation plane, with an
  archive-guided simple selector before any advanced selection. The `hm-bbx` epic builds it (→ the
  cooperative maze gate `hm-cs5`, held-out SMB `hm-2su`, software transfer `hm-ebe`); the explorer's
  current AFL-shaped `Sensor`/`Archive::admit` corpus is compatibility code, not the target.
- **ARM vendor: Linux/KVM on Ampere Altra** (`docs/ARM-ALTRA.md`, ruled 2026-07-12) — a
  reach-matrix cell (vendor × form; the matrix lives in `docs/QUEUE.md` §"The Consonance north
  star"); *spike execution* gated on the
  Altra box arriving (bead `hm-7pb` → execution `hm-idb`). Altra has no FEAT_ECV, so the
  clock question runs through the paravirt work-derived clock — **built and merged**
  (`docs/PARAVIRT-CLOCK.md`, PR #110, `hm-rk5` closed). The Apple-silicon route is DEAD
  (`docs/APPLE-SILICON.md`, archaeology only). The **ISA seam is ruled and restructured**
  (`docs/ARCH-BOUNDARY.md`; the engine/vendor split + two-level `Exit` + vm-state v2 arch tag
  merged as PR #109); per the **pre-build ruling (Paul 2026-07-13, `docs/ARCH-BOUNDARY.md`
  §Pre-build ruling)**, ARM-side building did not wait for spike GO — the spike gates trust
  (measured constants, the trait freeze, the cell fill). The pre-build queue has largely
  landed (appliance/preflight apparatus PR #108, paravirt clock PR #110); the **ARM backend
  skeleton is at near-merge** (`hm-cbt`, PR #117).
- **AMD vendor: SVM on Epyc** (`docs/AMD-EPYC.md`, ruled 2026-07-13) — the second
  reach-matrix vendor cell, gated on the Epyc box (bead `hm-9wt` → execution `hm-u1n`).
  ARM > AMD if bandwidth forces a choice; parallelize when both boxes are up.
- **Task 92 — multi-CPU/backend characterization registry** (probe → select → validate),
  fixing the one-box bus factor on the single destructive `det-cfl-v1` baseline. Deferred behind
  Wave 4.
- **Task 43 — `guest/` → `harmony-linux/` tier.** Complete as `hm-ciz`: the R-L4 carve-out,
  glossary renames, `/dev/harmony`, and compatibility library landed together. The workload
  audit defers loader extraction to task 44 and its remaining adapter moves to `hm-hza`.
  `harmony-kubernetes` (a higher guest layer) can now build on this landing.
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

Task 90 (the harmony/consonance/dissonance/unison rename) left `hypervizor` only in hashed
build inputs, under a document-as-deliberately-stale ruling until a rebaseline was otherwise
required. Task 43 adds a kernel driver and image library, so it folds that cleanup into its
mandatory twice-reproducible manifest rebaseline rather than creating an extra hash event.

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
- **Dissonance design:** `docs/DISSONANCE-STRATEGY.md` — the ruled strategy (SDK-cooperative
  testing, the Differential observation plane, Resolution's two interfaces); `docs/DISSONANCE.md`
  — the two-plane fault surface + one-reproducer permutation model; `docs/EXPLORATION.md` and
  `docs/SCORING.md` — the search/scoring seam (design record; production path re-ruled by the
  strategy); `docs/RESOLUTION.md` — the judgment loop; `docs/GLOSSARY.md` — the naming authority;
  `docs/LAYERS.md` — the exploration → perturbation → judgment capability layering.
- **Code quality:** `docs/CODE-QUALITY.md` — coverage, mutation testing, determinism lints,
  wire-decoder fuzzing, Kani proofs; independent of the feature backlog above.
- **Task constitution:** `tasks/00-CONVENTIONS.md` — now includes the frontier-task class
  (box-only, spec-named surface list, box + portable-logic gates) that ~2/3 of tasks 41–57
  already followed in practice.
