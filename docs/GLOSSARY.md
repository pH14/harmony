# GLOSSARY — the vocabulary ruling

> **Status: RULED (Paul, 2026-07-06).** Binding on all new code, docs, and task specs
> immediately. Existing code keeps its legacy names until the rename slate below rides its
> scheduled work (see "Sequencing"); a legacy name in merged code is debt, not precedent.
> This document is the naming authority — a task spec that mints a new term must add it
> here (or use one already here) in the same PR.

## Why this exists

Names were minted per-task with no central authority (212 commits of task-by-task
delivery), which produced three colliding registers — musical branding, borrowed research
jargon, and plain-descriptive — plus a set of genuine collisions:

- `environment::Environment` (the trait, the decide seam) vs `explorer::Environment` (the
  struct, the opaque reproducer blob) — two load-bearing types, one name, sibling crates.
- `Moment` vs `VTime` — two names for one axis, with `VTime` used as both a point
  (`StopReason::Deadline { vtime }`) and a duration (fault delays); the unit ruling was
  escalated per task 65 (`docs/EXPLORATION.md`) and is settled here.
- Six words for "a state you can get back to": `SnapId`, `Exemplar`, `VirtualExemplar`,
  `ExemplarRef`, `FrontierEntry`, seal.
- Three sibling "kind" enums: `DecisionClass` (environment), `Role` (matcher), `PointKind`
  (link) — the latter two classify the same thing.
- The `runtrace` crate shadowing the `RunTrace` struct (different things).
- Cross-paper collisions in the borrowed jargon itself: *cell* means
  observation-discretization in Go-Explore but model-state in ModelFuzz; exact PCT needs
  two passes, which the single-pass `Tactic` invariant structurally forbids; Coyote's
  portfolio lesson implies a future "which arm" chooser whose natural name collides with
  `Selector`.

## The three governing rules

1. **One register per layer.** The family register is **harmony theory** — pitch
   relationships: *harmony, consonance, dissonance, unison, counterpoint, resolution*.
   Orchestra-role terms (conductor, ensemble, maestro) fail the register test. Harmony
   names live at the family/product layer only (top-level crate families, the future
   product surface). The **mechanism layer** (types, traits, modules) uses
   research-standard terms under rule 2, or plain-descriptive names. **Sub-crates get
   boring role names** — a crate name answers "what does this do" cold.
2. **Citation discipline.** A paper's word may be used only for that paper's mechanism.
   `CellFn` is Go-Explore's cell (a pure discretization of observed features), *not*
   ModelFuzz's (a co-executed formal model's state) — cite ModelFuzz only when a model-state
   `CellFn` exists. A `Tactic` (single-pass, online, no lookahead) can never host exact
   PCT (two passes) — task 72 names its mechanism something else. Diverge from the paper →
   drop the word or drop the citation. Never import two papers' meanings into one
   identifier.
3. **Name the currencies first.** The system has two nouns and one axis at its core:
   a captured **state** (expensive, transient, a resource), a replayable **reproducer**
   (cheap, portable, the artifact), and the **`Moment`** axis — with the identity
   *state = replay(reproducer)*. Every other noun is a role played by one of these three;
   a name that obscures which currency it wraps is wrong.

## The family register

*harmony* (the whole) · *consonance* (the deterministic engine — things agreeing) ·
*dissonance* (the adversary — the bug finder) · *unison* (the determinism checker — two
runs identical) · *counterpoint* (the composition root — the discipline of setting the
voices against each other under consonance/dissonance rules) · *resolution* (the judgment
layer — dissonance resolving). Counterpoint → resolution completes the theory: in
counterpoint, dissonance must resolve.

## Kills

| Legacy name | Replacement | Why |
|---|---|---|
| `Modulation` | **rollout** | Decorative music word at the mechanism layer; "rollout" is RL-standard for exactly this (branch → run → terminal) |
| `Progression` / `progression_step` | the **search loop** / **`step`** | Carries zero information; the method name does the work |
| `conductor` (crate) | **`counterpoint`** | Orchestra-role term — register violation. Counterpoint names the crate's structural essence: it is the only crate importing both `consonance` and `dissonance`, and counterpoint is the discipline of combining those voices. (Runner-up `cadence` rejected: in engineering vocabulary it primes "release/meeting rhythm" — misleading.) |
| `CampaignOracle` | delete, or **`CrashOracle`** | Its `judge` delegates verbatim to `TerminalOracle` — it is named for its call site, not its verdict rule. Oracles are named by what they judge, never where they are used |

## Renames — types and terms

| Legacy | New | Why |
|---|---|---|
| `explorer::Environment` (struct) | **`Reproducer`** | Kills the worst collision (the `environment::Environment` trait keeps its name). "Reproducer" is already the word every doc comment uses *and* the fuzzing literature's term of art — register-2 and register-3 simultaneously. (`Recipe` was considered and rejected: a new mixed metaphor.) |
| `VTime` | **`Moment`** (point) / **`Span`** (duration) | One axis, two roles. Settles the escalated task-65 unit ruling. Every existing `VTime` use is audited into one or the other when its crate is next touched |
| `Exemplar` / `VirtualExemplar` / `ExemplarRef` / `FrontierEntry` | **`Entry`** + **`EntryRef`** | Six state-words → four. `SnapId` (raw resource handle) and **seal** (cached materialization of an entry) stay — genuinely distinct layers of the state currency |

## Renames — crates

| Legacy | New | Why |
|---|---|---|
| `dissonance/conductor` | **`counterpoint`** | See Kills. No reverse dependencies — the cheapest rename on the slate |
| `dissonance/runtrace` | **`journal`** | It is journal/store/scrape; unshadows the `RunTrace` struct |
| `dissonance/link` | **`sdk-link`** | Ungreppable, collides with linkers. The tier vocabulary (scrape / link / instrument) stays in the docs |
| `dissonance/logtmpl` | **`log-templates`** | Double-clipped abbreviation; spell it out |
| `dissonance/matcher` | **`signals`** | The product concept is *declared signals* (`SignalSet`/`SignalDecl`/`Role`); matching is the mechanism inside |
| `dissonance/tactics-regime` | **`tactics`** | Named for one implementation strategy, not the role; future portfolio arms land in this crate |
| `dissonance/flow` | **keep** | The deliberate exception: it anchors an already-consistent cluster (`DecisionClass::NetFlow`, `FlowPolicy`, `FlowEvent`, `guest/flow-agent`) |

## Merges

- `PointKind` (link) + `Role` (matcher) → one spine **`Role`**. Explicitly do **not**
  fold in `DecisionClass`: it is wire-versioned catalog vocabulary with stable
  discriminants; coupling the wire format to the config DSL is worse than a second enum.

## Adopted vocabulary

| Word | Names | Notes |
|---|---|---|
| **`Reproducer`** | the reproducer artifact — the opaque currency the explorer ferries | `environment::EnvSpec` stays as the decoded form ("the specification of the environment") — do **not** also name it Reproducer, or the original collision is rebuilt one level down |
| **rollout** | one branch → run → terminal | A rollout *produces* a timeline |
| **`step`** | one search-loop iteration | pick base → mint reproducer → rollout → admit → judge |
| **timeline** | one execution history — the data-noun the codebase lacked | Composes with the axis: a timeline is a sequence of `Moment`s; a reproducer replays a timeline; a bug's address is `(timeline, Moment)`. The user-facing word for the resolution layer. **`multiverse` is rejected** — Antithesis branding. NB: pre-task-94 explorer code used "Timeline" for the *inner loop* (`timeline()`/`multiverse_step()`); that sense is dead — any surviving loop-sense use is legacy (see `docs/LAYERS.md`) |
| **`Span`** | a duration on the `Moment` axis | |

## Keeps (the defense, one line each)

- **The spine six** — `Tactic`, `Selector`, `Archive`, `Sensor`, `CellFn`, `Oracle` (with
  `Frontier` as the Archive's exposed data; `Machine`/`EnvCodec` sit below the spine):
  each names one seam with one stated invariant and one traceable citation.
- **`environment`** (crate and trait): the DST term of art — the environment answers
  everything the guest cannot answer for itself. Its second job (naming the artifact)
  retires to `Reproducer`. Resulting rule: *environment = the live answering surface;
  reproducer = the recorded artifact that reconstitutes it.* `SeededEnv` / `RecordedEnv` /
  `AdapterEnv` stay — backings *of the environment*, now unambiguous.
- **campaign**: citation-grounded (Klees et al. and STADS use it exactly this way);
  alternatives are all taken or worse. Kept **with its definition pinned**: a campaign is
  a pure function of `(campaign_seed, machine)` — one seed, bit-reproducible, one workload,
  one budget.
- **session** — a control-transport lifetime (server ↔ client). Orthogonal to the
  hierarchy below; never a synonym for campaign.
- **sweep** — the task-58 determinism-gate protocol. **Fenced as gate-only vocabulary**;
  a sweep is not a campaign and never appears in product-facing language.
- **`Moment`**, **seal**, **`SnapId`**, and the family names.

## The containment hierarchy

```
Moment    — a point on the axis
rollout   — one branch → run → terminal        (produces a timeline)
step      — one search-loop iteration
campaign  — a seeded, budgeted sequence of steps against one workload
```

`session` is orthogonal (transport); `sweep` is fenced (gates).

## Reserved — named now so future tasks do not mint collisions

- **`Portfolio`** — the Coyote-style arm-chooser (tasks 70/72). It must **not** be called
  a Selector; `Selector` already means "which frontier entry to branch from."
- **The PCT two-pass host** (task 72) — deliberately unnamed here, but ruled: it is *not*
  a `Tactic` (single-pass, online, structurally cannot count `k` scheduling points ahead).
  Name it when built; add it here in the same PR.
- **The level above campaign** — continuous, CI-driven testing (campaigns repeated over
  time against a workload). Reserved, deliberately unnamed until it exists.

## Logged follow-ons (naming-adjacent, not renames)

- `CellFnV1` is multi-channel but lives in `logtmpl` (→ `log-templates`) — packaging
  smell; the cell abstraction ("the whole game") may deserve its own crate.
- `tactics-regime` (→ `tactics`) mixes both proposal axes (regime tactic = entropy axis;
  `SeqMutators` = mutation axis) in one crate.
- The task-70 two-loops merge: whichever of `Explorer::explore` / `run_campaign` survives
  carries the campaign vocabulary (`Campaign`, `CampaignConfig`, `campaign_seed`) — no
  third word. `benchcampaign` becomes an internal `bench` module, not vocabulary.

## Sequencing

1. **This document is binding on new code immediately** (it costs nothing).
2. **Eager, standalone**: `explorer::Environment` → `Reproducer` — the collision every
   future cross-crate reader pays for.
3. **`conductor` → `counterpoint`** anytime — zero reverse dependencies.
4. **Everything else rides its natural work**: task 70 rewrites the loop seam (kills
   `Modulation`/`Progression`, merges the loops under campaign vocabulary); crate renames
   batch with each crate's next substantive PR; `VTime` audits ride each crate's next
   touch. No big-bang rename PR — merged, box-gated, golden-pinned code is not churned
   for vocabulary alone.

---

# Consonance addendum

> **Status: RULED (Paul, 2026-07-06).** The same review, run over consonance. Same
> discipline: binding on new code immediately; renames ride their scheduled windows; no
> big-bang. Consonance needed a much smaller slate than dissonance — its verb spine
> (`branch`/`replay`/`snapshot`/`drop`/`hash`/`run`, `seal`/`quiescent`, `work`) is already
> bit-consistent from `snapshot-store` through the `ControlServer` to the explorer seam.
> What it had instead was a handful of cross-family collisions this document's first pass
> missed because it was written looking at dissonance.

## A fourth governing rule — prefixes are earned by pairs

**The directory provides the family grouping** (`consonance/` is the namespace; a blanket
`vm-` prefix would restate the path). **A name-prefix is reserved for crates that are two
halves of one seam or one artifact** — the `sdk-link` precedent, generalized. Consonance
previously carried three accidental prefix families (`vmm-`, `vm-`, `v`) that encoded
nothing. After this slate, every prefix names a real pair, and the crate list teaches the
architecture:

```
vmm-backend, vmm-core                 the machine (below / above the Backend trait)
vtime, lapic                          time & interrupt fabric (engine + device model)
snapshot-store, snapshot-state        the snapshot artifact (memory / everything else)
hypercall-proto, hypercall-doorbell   the guest channel (frames / transport)
unison, acceptance-suite              the determinism gates (instrument / gate)
telemetry                             the operator tap
```

Corollary: device-model crates are named for the hardware they model (`lapic` today, a
future `gic` per `docs/ARCH-BOUNDARY.md`) — the hardware name *is* the group marker; no
prefix.

## Kills

| Legacy | Replacement | Why |
|---|---|---|
| "corpus GC" (the snapshot-**pool** sense — `control.rs` `drop`, `explorer/seam.rs`, `explorer/lib.rs` doc comments) | **"pool GC"** | "Corpus" gets exactly one meaning: the acceptance workload suite (payloads + manifest). The retained-state nouns on the spine side are already `Archive`/`Frontier` |
| "Hypervizor VMM" (`hypercall-proto/src/lib.rs`) | "the deterministic VMM" | Pre-Harmony project-name leftover |

## Renames — crates

| Legacy | New | Why |
|---|---|---|
| `det-corpus` | **`acceptance-suite`** | Kills the double-clipped "det-" (the `logtmpl` smell) and the corpus overload; names the role — the engine's acceptance gate (oracles + manifest + report + runner CLI), the domain layer over `unison`. Explicitly **not** folded into `unison`: the instrument stays domain-free (its own non-goal); the gate is a product artifact (CLI exit-code contract, audited manifest, JSON report), not a test suite of the bisector |
| `vm-state` | **`snapshot-state`** | Completes the snapshot pair: a snapshot is the memory pages (`snapshot-store`) plus this blob — the crate's own first sentence. Also kills the `vm-`/`vmm-` near-collision, which implied a kinship with `vmm-core` that doesn't exist. The `VmState` type and the `vm_state` blob name **stay** — they name the content; the crate names the role |
| `vmcall-transport` | **`hypercall-doorbell`** | Documented misnomer (the mechanism is a port-I/O doorbell, not `VMCALL`; supersedes the spec's deferred `io-transport`). The prefix groups the channel pair with `hypercall-proto` (frames / transport). ⚠️ Guest payloads depend on it → `MANIFEST.sha256` rebaseline; rides task 43's window (task-90 ruling) |

## Renames — types

| Legacy | New | Why |
|---|---|---|
| `unison::Machine` / `MachineFactory` / `MachineError` | **`Subject`** / **`SubjectFactory`** / **`SubjectError`** | The eager one — the `Reproducer` of this addendum. Two load-bearing `Machine` traits (unison's `run_to`/`work`/`state_hash` vs the spine's `branch`/`replay`/`run`), sibling families, and they meet inside `vmm-core` (`corpus.rs` implements one, `control.rs` serves the other). The Keeps line "`Machine`/`EnvCodec` sit below the spine" blessed the **spine's** trait; unison's own doc — "a deterministic machine under test" — is the new name. The spine's `Machine` keeps its name |
| `det_corpus::Oracle` (enum) | **`OracleKind`** | A which-check selector (O1/O2/O3) vs the spine's `trait Oracle` (kept, citation-grounded). Rides the `acceptance-suite` rename — one PR |
| `vmm_backend::Event` | **`Injection`** | Three `Event`s in consonance alone (this injectable interrupt/NMI, `telemetry::Event`, hypercall `ServiceId::Event`), all flowing through vmm-core's loop. The backend's is the thing you *inject*; the other two keep the word |
| `Vtime` (`vmm_backend::types`), `VTime` (control wire) | **`Moment`** / **`Span`** | Already ruled above; noted here because consonance carries *both spellings* of the killed name. Audits ride each crate's next touch — the newest consonance code (`control.rs`, the live gates) already complies |

## Pins — adopted vocabulary, no rename

- **`SnapshotId` vs `SnapId`** — keep both; the pair is the point. `SnapshotId` is the
  **store-local** resource handle; `SnapId` is the **pool-wide wire** handle the
  `ControlServer` mints and maps (`control.rs`: "Wire `SnapId` → store `SnapshotId`").
  This sharpens the Keeps line above ("`SnapId` (raw resource handle)") — the raw handle
  is `SnapshotId`; `SnapId` is its wire alias.
- **The two canonical digests + the wire verb** — `state_hash` (all architectural state,
  latent included) and `observable_digest` (guest-observable output only — O3 is unsound
  without the distinction). `hash` is the *wire verb*, scoped by `HashScope`. Three names,
  three roles; do not unify.
- **"V-time" survives as the mechanism's name.** The kill above retired `VTime` the
  *type*, not the word: V-time names the work-derived clock itself; `Moment`/`Span` name
  positions and durations **on** it. The `vtime` crate and its prose stand.
- **The mirror-type pattern is deliberate.** Same-name local mirrors under conventions
  rule 2 (`telemetry::ExitCounts` mirrors `vmm_backend::ExitCounts`; `snapshot-state`'s
  `VcpuRegs`/`Segment`/… mirror `vmm-backend`'s) are **not** collisions — the marker is
  the "local mirror of X" doc comment. Future naming reviews prosecute unmarked
  duplicates only.

## Reserved — consonance

- **The `vmm-core` split names.** `vmm-core` is a grab-bag ("core" answers "what does this
  do" with "everything"), but that is a packaging problem `docs/ARCH-BOUNDARY.md` §B
  already owns: engine/personality module split now, **crate split only when an ARM
  backend lands**. The role names are minted at that window — candidates: `engine` (the
  arch-neutral half; family-consistent with "consonance, the deterministic engine"), the
  personality crates per ARCH-BOUNDARY's own vocabulary, and possibly `control-server`
  peeling off. Reserved so the split does not improvise; do not rename `vmm-core` before
  it.

## Sequencing — consonance

1. **Binding on new code immediately.**
2. **Eager, standalone**: `unison::Machine` → `Subject` — the cross-family collision every
   future reader pays for.
3. **Cheap, anytime**: `det-corpus` → `acceptance-suite` (+ `OracleKind` folded in — one
   dev-dep reverse edge, CI path globs, doc refs; no goldens, no wire); `vm-state` →
   `snapshot-state` (one reverse dep, no wire/golden impact).
4. **Task 43's window**: `vmcall-transport` → `hypercall-doorbell` (MANIFEST rebaseline),
   alongside the R-L4 payloads/golden move to consonance's test surface.
5. **Rides next touch**: `vmm_backend::Event` → `Injection`; the `Vtime`/`VTime` audits;
   the doc-comment kills (pool GC, Hypervizor).
6. **No big-bang** — merged, box-gated, golden-pinned code is not churned for vocabulary
   alone.

---

# Scoring addendum

> **Status: RULED (Paul, 2026-07-07).** The Scoring-seam ruling (`docs/SCORING.md`) mints
> three terms and reserves one name; per the naming authority rule they land here in the
> same PR. Binding on new code immediately, like the addenda above.

## Adopted vocabulary — scoring

| Word | Names | Notes |
|---|---|---|
| **re-key** | recomputing every retained timeline's cells under a changed `CellFn`, then rebuilding the archive by re-running admission | The AURORA container-rebuild / Go-Explore archive-conversion mechanism; harmony's form is exact and offline (replay retained `RunTrace`s through the pure fold). Verb: "re-key the traces" |
| **re-key epoch** | the interval between re-keys — the cadence of the `SCORING.md` R2 granularity controller | Epoch-wise, never online: `CellFn` knobs are discrete, so the controller adjusts between re-keys, not continuously |
| **energy** | how many rollouts a chosen entry receives before the `Selector` chooses again | AFLFast's power-schedule term of art, used for AFLFast's mechanism (citation discipline holds). Cost-aware; *choice* stays cost-blind (`SCORING.md` R5) |

## Reserved — scoring

- **`quality`** — the second `Reward` channel (`SCORING.md` R4): the per-cell domination
  preference magnitude (R3). Named now so task 70+ does not mint a collision; `Reward`
  channels are meaning-blind integers, and there are exactly two.
