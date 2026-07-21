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
- Three sibling "kind" enums: `DecisionClass` (environment), `Role` (matcher), and `PointKind`
  on the current binary guest wire and its SDK-event reader mirror. The proposed merge of the
  latter two is legacy; `PointKind` itself remains current binary-wire vocabulary until that
  versioned format is replaced.
- The `runtrace` crate shadowing the `RunTrace` struct (different things).
- Cross-paper collisions in the borrowed jargon itself: *cell* means
  observation-discretization in Go-Explore but model-state in ModelFuzz; exact PCT needs
  two passes, which the single-pass `Tactic` invariant structurally forbids; Coyote's
  portfolio lesson implies a future "which arm" chooser whose natural name collides with
  `Selector`.

## The three governing rules

1. **One register per layer.** The family register is **harmony theory** — pitch
   relationships: *harmony, consonance, dissonance, unison, counterpoint (reserved), resolution*.
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
runs identical) · *resolution* (the judgment layer inside dissonance — dissonance resolving).
**Counterpoint is reserved and names no system today.** It may be assigned only if a genuine
family/product-level role emerges; importing both consonance and dissonance does not entitle a
mechanism crate to a musical name.

## Kills

| Legacy name | Replacement | Why |
|---|---|---|
| `Modulation` | **rollout** | Decorative music word at the mechanism layer; "rollout" is RL-standard for exactly this (branch → run → terminal) |
| `Progression` / `progression_step` | the **search loop** / **`step`** | Carries zero information; the method name does the work |
| `conductor` (crate) | **`campaign-runner`** | Orchestra-role term — register violation. The crate composes, executes, records, and reports campaigns; use the boring role name. `counterpoint` is reserved at the family/product layer |
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
| `dissonance/conductor` | **`campaign-runner`** | See Kills. Describes the composition root without spending a family-level music term |
| `dissonance/runtrace` | **`journal`** | It is journal/store/scrape; unshadows the `RunTrace` struct |
| `dissonance/link` | **`sdk-events`** | The host-side SDK evidence reader: Antithesis JSON or the internal binary wire → validated normalized `SdkSchema` + typed, timestamped `SdkEvent`s. `link` is opaque and `sdk-link` preserves the opacity. Temporal reduction, cells, and archive policy do not live here |
| `dissonance/logtmpl` | **`log-templates`** | Double-clipped abbreviation; spell it out |
| `dissonance/matcher` | **`signals`** | The product concept is *declared signals* (`SignalSet`/`SignalDecl`/`Role`); matching is the mechanism inside |
| `dissonance/tactics-regime` | **`tactics`** | Named for one implementation strategy, not the role; future portfolio arms land in this crate |
| `dissonance/flow` | **keep** | The deliberate exception: it anchors an already-consistent cluster (`DecisionClass::NetFlow`, `FlowPolicy`, `FlowEvent`, `harmony-linux/flow-agent`) |

## Required separations

- SDK schema declarations and matcher/campaign `Role` do **not** merge. Normalized `SdkSchema`
  describes stable raw evidence identity, value shape, and base update semantics; a campaign role
  or Differential derivation describes how evidence is used after that base temporal meaning is
  reconstructed. The former is data, the latter is a query/projection. `DecisionClass` also remains
  separate wire-versioned control vocabulary.
- Base update semantics belong to the versioned source declaration or ingress normalization:
  `SdkSchema` for SDK evidence and the corresponding source schema for scrape/instrument evidence.
  A campaign role never changes `set` into `max`/`min`, or vice versa. Matcher `Role::StateMax` is
  legacy conflation to split during the observation-plane migration, not the owner of SDK update
  behavior.
- A legacy declaration may preserve identity while leaving state semantics unresolved. It remains
  valid for schema coverage and explicit expectations but cannot enter temporal state reduction
  until a versioned source or workload instrumentation declaration supplies the missing contract.
- Assertion **site identity** and **property identity** remain separate. On the adopted Antithesis
  surface the message identifies the aggregated property; multiple sites may contribute events and
  coverage without creating separate property verdicts.

## Adopted vocabulary

| Word | Names | Notes |
|---|---|---|
| **`Reproducer`** | the reproducer artifact — the opaque currency the explorer ferries | `environment::EnvSpec` stays as the decoded form ("the specification of the environment") — do **not** also name it Reproducer, or the original collision is rebuilt one level down |
| **rollout** | one branch → run → terminal | A rollout *produces* a timeline |
| **`step`** | one search-loop iteration | pick base → mint reproducer → rollout → admit → judge |
| **timeline** | one execution history — the data-noun the codebase lacked | Composes with the axis: a timeline is a sequence of `Moment`s; a reproducer replays a timeline; a bug's address is `(timeline, Moment)`. The user-facing word for the resolution layer. **`multiverse` is rejected** — Antithesis branding. NB: pre-task-94 explorer code used "Timeline" for the *inner loop* (`timeline()`/`multiverse_step()`); that sense is dead — any surviving loop-sense use is legacy (see `docs/LAYERS.md`) |
| **`Span`** | a duration on the `Moment` axis | |
| **`SdkEvent`** | one immutable, typed, timestamped record emitted by a cooperating guest | Data plane only: an `SdkEvent` reports what happened and never asks the guest or VMM to do anything. A round-trip operation such as `buggify` uses the control plane for its request/answer and records the outcome separately as an `SdkEvent` |
| **`SdkSchema`** | the versioned, normalized SDK declarations persisted with its events | Stable site/property identity, occurrence/state classification, value shape, and—when declared—base update semantics. Ordinary Antithesis assertions are occurrence/property evidence; numeric guidance may declare `max`/`min` but is report-only until represented and ordered exactly; binary v1 may leave never-fired state semantics unresolved. Replaces the overnamed SDK “catalog”; it is data, does not declare what a campaign should find interesting, and need not be a new app-facing SDK object |
| **film** | the visible replay of a reproducer clip — `(reproducer, Moment) → what the screen showed` | A pure **observation** query over the one timeline (never an in-guest re-render, which the one-reproducer rule forbids — `docs/LAYERS.md`): read task 86's billboard at each frame `Moment`, then re-render host-side by loading the savestate into the same commit-pinned core (**1:1 by construction**). The resolution layer's showpiece (task 87, `dissonance/film`). Verb: "film the clip"; the intermediate artifact is a *capture bundle*, rendered to a PPM sequence / contact sheet |

## Keeps (the defense, one line each)

- **The surviving Explorer vocabulary** — `Tactic`, `Selector`, `Oracle`, and `CellFn` keep their
  single-pass, entry-choice, completed-trace, and pure-projection meanings. **Archive** remains the
  one retained-corpus concept and selector-facing read model, but the mutable `Archive::admit`
  interface is not the Differential target. `Sensor`, `Feature`, `FeatureSet`, and `ChannelId` are
  compatibility vocabulary scheduled to leave the production path. `Machine`/`EnvCodec` remain
  below the search loop on the control plane. See `docs/DISSONANCE-STRATEGY.md`.
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
- **cut** / **`EvidenceCut`** (task 127, `hm-bbx.6`): a seal's **evidence cut** — the
  server-stamped `(Moment, included SDK-event count)` pair bound to a successful seal,
  half-open by the ordered SDK capture's **prefix length** (positions below the count
  included, at/after excluded — never a `Moment` comparison). Captured with the seal and
  carried verbatim (snapshot reply → metadata → pending fork → persisted lineage); the
  stamp is the sole authority, never reconstructed by a second read. The console scrape
  stays source-local and stop-granular — structurally outside the cut. A later
  seal-relative source gets its **own** declared cursor; independent cursors never imply
  cross-source order. Vocabulary ratified by `docs/DISSONANCE-STRATEGY.md` ("The cut is
  captured with the seal"); this entry records the type name.

## The containment hierarchy

```
Moment    — a point on the axis
rollout   — one branch → run → terminal        (produces a timeline)
step      — one search-loop iteration
campaign  — a seeded, budgeted sequence of steps against one workload
```

`session` is orthogonal (transport); `sweep` is fenced (gates).

## Reserved — named now so future tasks do not mint collisions

- **`Portfolio`** — the future Coyote-style tactic/mutation-arm chooser. It must **not** be called
   a Selector; `Selector` already means "which frontier entry to branch from."
- **The PCT two-pass host**, if a scheduler-control feasibility proof earns it — deliberately
  unnamed here, but ruled: it is *not*
   a `Tactic` (single-pass, online, structurally cannot count `k` scheduling points ahead).
   Name it when built; add it here in the same PR.
- **The level above campaign** — continuous, CI-driven testing (campaigns repeated over
  time against a workload). Reserved, deliberately unnamed until it exists.

## Logged follow-ons (naming-adjacent, not renames)

- `CellFn` survives as the pure projection over a complete observation map. `CellFnV1`'s
  feature/channel algorithm is compatibility code, not a packaging candidate or ratified target.
- `tactics-regime` (→ `tactics`) mixes both proposal axes (regime tactic = entropy axis;
  `SeqMutators` = mutation axis) in one crate.
- The generic `Explorer` is the production search loop and carries campaign vocabulary through its
  composition root (`CampaignConfig`, campaign seed, and report). Bespoke benchmark loops are
  compatibility code or internal `bench` modules, not a second campaign engine.

## Sequencing

> **Landed (tasks/105, PR #106, 2026-07-13).** Items 2 and 3 below are **in code**, alongside the
> `dissonance/link` → `sdk-events` crate rename, `VTime` → `Moment`/`Span`, and consonance's
> `Machine` → `Subject`. Still riding their natural work per item 4: the remaining crate renames
> (`runtrace` → `journal`, `logtmpl` → `log-templates`, `matcher` → `signals`, `tactics-regime` →
> `tactics`) and the `sdk-events` internals — `LinkSensor`, `LINK_ASSERT_CHANNEL`/`LINK_STATE_CHANNEL`,
> `GuestEvent` → `SdkEvent`, and the SDK "catalog" → `SdkSchema` — which the `hm-bbx` Differential
> integration deletes/renames rather than churning ahead of it.

1. **This document is binding on new code immediately** (it costs nothing).
2. **Eager, standalone**: `explorer::Environment` → `Reproducer` — the collision every
   future cross-crate reader pays for.
3. **`conductor` → `campaign-runner`** in its next substantive composition-root change.
4. **Everything else rides its natural work**: the Differential integration rewrites the
   observation/archive seam and establishes the generic Explorer campaign path; crate renames
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
halves of one seam or one artifact** — the guest SDK / `sdk-events` pair, generalized. Consonance
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

> **Status: AMENDED (2026-07-12).** The Differential strategy keeps the plain operation
> **recompute cells**, retains the research term **energy** only if that exact mechanism is built,
> and uses **quality** as archive-domination data. The earlier `re-key`/`re-key epoch` and exact
> two-channel `Reward` rulings are superseded.

## Adopted vocabulary — scoring

| Word | Names | Notes |
|---|---|---|
| **energy** | how many rollouts a chosen entry receives before the `Selector` chooses again | AFLFast's power-schedule term of art. Use it only if Harmony actually implements that repeated-rollout allocation mechanism; it is not a generic budget synonym |
| **recompute cells** | derive cells again from retained/replayed evidence under a different versioned `CellFn` | Plain descriptive operation, not a new API noun and not EnvCodec key shifting. It can create a diagnostic archive view but cannot manufacture a seal |

## Reserved — scoring

- **`quality`** — deterministic per-Entry data used by Differential best-per-cell domination.
  Whether it is scalar, lexicographic, or later exposed through `Reward` remains an implementation
  contract to earn; it is not ruled to be exactly a second `Reward` channel.
