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
| **timeline** | one execution history — the data-noun the codebase lacked | Composes with the axis: a timeline is a sequence of `Moment`s; a reproducer replays a timeline; a bug's address is `(timeline, Moment)`. The user-facing word for the resolution layer. **`multiverse` is rejected** — Antithesis branding |
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
