# GLOSSARY — the vocabulary ruling

Binding on all new code and docs immediately. Existing code keeps its legacy names until a
rename rides its natural work; a legacy name in merged code is debt, not precedent. This
document is the naming authority — work that mints a new term must add it here (or use one
already here) in the same PR.

The original ruling also covered the v1 dissonance tree; its kill/rename slates are in git
history. The dissonance rebuild uses LibAFL's own vocabulary and mints nothing
(`dissonance/CLAUDE.md`).

## Why this exists

Names were minted per-task with no central authority, which produced three colliding
registers — musical branding, borrowed research jargon, and plain-descriptive — plus genuine
type-name collisions. The rules below prevent recurrence.

## The three governing rules

1. **One register per layer.** The family register is **harmony theory** — pitch
   relationships: *harmony, consonance, dissonance, unison, counterpoint (reserved),
   resolution*. Orchestra-role terms (conductor, ensemble, maestro) fail the register test.
   Harmony names live at the family/product layer only (top-level crate families, the future
   product surface). The **mechanism layer** (types, traits, modules) uses research-standard
   terms under rule 2, or plain-descriptive names. **Sub-crates get boring role names** — a
   crate name answers "what does this do" cold.
2. **Citation discipline.** A paper's word may be used only for that paper's mechanism.
   Diverge from the paper → drop the word or drop the citation. Never import two papers'
   meanings into one identifier.
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

## Adopted vocabulary

| Word | Names | Notes |
|---|---|---|
| **reproducer** | the replayable artifact — the recorded coordinates that reconstitute a run | `environment::EnvSpec` stays as the decoded form ("the specification of the environment") |
| **rollout** | one branch → run → terminal | A rollout *produces* a timeline |
| **timeline** | one execution history | Composes with the axis: a timeline is a sequence of `Moment`s; a reproducer replays a timeline; a bug's address is `(timeline, Moment)`. **`multiverse` is rejected** — Antithesis branding |
| **`Span`** | a duration on the `Moment` axis | |
| **`SdkEvent`** | one immutable, typed, timestamped record emitted by a cooperating guest | Data plane only: an `SdkEvent` reports what happened and never asks the guest or VMM to do anything. A round-trip operation such as `buggify` uses the control plane for its request/answer and records the outcome separately as an `SdkEvent` |
| **film** | the visible replay of a clip — `(reproducer, Moment) → what the screen showed` | A pure **observation** query over the one timeline, never an in-guest re-render: read the billboard at each frame `Moment`, then re-render host-side by loading the savestate into the same commit-pinned core (**1:1 by construction**). Verb: "film the clip" |

## Keeps (the defense, one line each)

- **`environment`** (crate and trait): the DST term of art — the environment answers
  everything the guest cannot answer for itself. Rule: *environment = the live answering
  surface; reproducer = the recorded artifact that reconstitutes it.* `SeededEnv` /
  `RecordedEnv` / `AdapterEnv` stay — backings *of the environment*.
- **campaign**: citation-grounded (Klees et al. and STADS use it exactly this way);
  alternatives are all taken or worse. Kept **with its definition pinned**: a campaign is
  a pure function of `(campaign_seed, machine)` — one seed, bit-reproducible, one workload,
  one budget.
- **session** — a control-transport lifetime (server ↔ client). Never a synonym for campaign.
- **sweep** — the determinism-gate protocol. **Fenced as gate-only vocabulary**;
  a sweep is not a campaign and never appears in product-facing language.
- **`Moment`**, **seal**, **`SnapId`**, and the family names.

## The containment hierarchy

```
Moment    — a point on the axis
rollout   — one branch → run → terminal        (produces a timeline)
campaign  — a seeded, budgeted sequence of rollouts against one workload
```

`session` is orthogonal (transport); `sweep` is fenced (gates).

---

# Consonance addendum

The same review, run over consonance. Same discipline: binding on new code immediately;
renames ride their scheduled windows; no big-bang. Consonance needed a much smaller slate
than dissonance — its verb spine (`branch`/`replay`/`snapshot`/`drop`/`hash`/`run`,
`seal`/`quiescent`, `work`) is already bit-consistent from `snapshot-store` through the
`ControlServer` to the control-client boundary. What it had instead was a handful of
cross-family collisions.

## A fourth governing rule — prefixes are earned by pairs

**The directory provides the family grouping** (`consonance/` is the namespace; a blanket
`vm-` prefix would restate the path). **A name-prefix is reserved for crates that are two
halves of one boundary or one artifact.** Consonance previously carried three accidental
prefix families (`vmm-`, `vm-`, `v`) that encoded nothing. After this slate, every prefix
names a real pair, and the crate list teaches the architecture:

```
vmm-backend, vmm-core                 the machine (below / above the Backend trait)
vtime, lapic                          time & interrupt fabric (engine + device model)
snapshot-store, snapshot-state        the snapshot artifact (memory / everything else)
hypercall-proto, hypercall-doorbell   the guest channel (frames / transport)
unison, acceptance-suite              the determinism gates (instrument / gate)
telemetry                             the operator tap
```

Corollary: device-model crates are named for the hardware they model (`lapic`, `gicv3`) —
the hardware name *is* the group marker; no prefix.

## Kills

| Legacy | Replacement | Why |
|---|---|---|
| "corpus GC" (the snapshot-**pool** sense) | **"pool GC"** | "Corpus" gets exactly one meaning: the acceptance workload suite (payloads + manifest) |
| "Hypervizor VMM" | "the deterministic VMM" | Pre-Harmony project-name leftover |

## Renames — remaining slate

| Legacy | New | Why |
|---|---|---|
| `vm-state` (crate) | **`snapshot-state`** | Completes the snapshot pair: a snapshot is the memory pages (`snapshot-store`) plus this blob — the crate's own first sentence. Also kills the `vm-`/`vmm-` near-collision, which implied a kinship with `vmm-core` that doesn't exist. The `VmState` type and the `vm_state` blob name **stay** — they name the content; the crate names the role |
| `vmm_backend::Event` | **`Injection`** | Three `Event`s in consonance alone (this injectable interrupt/NMI, `telemetry::Event`, hypercall `ServiceId::Event`), all flowing through vmm-core's loop. The backend's is the thing you *inject*; the other two keep the word |
| `Vtime` (`vmm_backend::types`), `VTime` (control wire) | **`Moment`** / **`Span`** | One axis, two roles. Audits ride each crate's next touch |

## Pins — adopted vocabulary, no rename

- **`SnapshotId` vs `SnapId`** — keep both; the pair is the point. `SnapshotId` is the
  **store-local** resource handle; `SnapId` is the **pool-wide wire** handle the
  `ControlServer` mints and maps. The raw handle is `SnapshotId`; `SnapId` is its wire alias.
- **The two canonical digests + the wire verb** — `state_hash` (all architectural state,
  latent included) and `observable_digest` (guest-observable output only — O3 is unsound
  without the distinction). `hash` is the *wire verb*, scoped by `HashScope`. Three names,
  three roles; do not unify.
- **"V-time" survives as the mechanism's name.** V-time names the exit-count-derived clock
  itself; `Moment`/`Span` name positions and durations **on** it. The `vtime` crate and its
  prose stand.
- **The mirror-type pattern is deliberate.** Same-name local mirrors
  (`telemetry::ExitCounts` mirrors `vmm_backend::ExitCounts`; `snapshot-state`'s
  `VcpuRegs`/`Segment`/… mirror `vmm-backend`'s) are **not** collisions — the marker is
  the "local mirror of X" doc comment. Naming reviews prosecute unmarked duplicates only.

## Reserved — consonance

- **The `vmm-core` split names.** `vmm-core` is a grab-bag ("core" answers "what does this
  do" with "everything"), but that is a packaging problem `docs/ARCH-BOUNDARY.md` already
  owns: engine/vendor module split now, **crate split only when the window arrives**. The
  role names are minted at that window — candidates: `engine` (the arch-neutral half;
  family-consistent with "consonance, the deterministic engine"), the vendor crates per
  ARCH-BOUNDARY's own vocabulary, and possibly `control-server` peeling off. Reserved so
  the split does not improvise; do not rename `vmm-core` before it.

## Sequencing — consonance

1. **Binding on new code immediately.**
2. **Cheap, anytime**: `vm-state` → `snapshot-state` (one reverse dep, no wire/golden
   impact).
3. **Rides next touch**: `vmm_backend::Event` → `Injection`; the `Vtime`/`VTime` audits;
   the doc-comment kills (pool GC, Hypervizor).
4. **No big-bang** — merged, gated, golden-pinned code is not churned for vocabulary alone.

---

# Testing addendum

The same review, run over the testing architecture. Same discipline: binding on new code and
new docs immediately; renames ride their scheduled work; no big-bang. The testing ladder these
words describe is `docs/TESTING.md`; the wire planes are `docs/PROTOCOL.md`.

## Kills — testing

| Legacy | Replacement | Why |
|---|---|---|
| "deterministic-twice" | **identity test** | Named the mechanism by its shape ("we ran it twice") and never said what it established. The claim is *identity*: one address, two runs, indistinguishable state |
| "conformance" (of a trait implementation) | **contract tests** | The trait's doc comments *are* the contract; the exam makes them executable. "Conformance" also collides head-on with the acceptance suite's **O2 conformance-to-spec** oracle, which is a different thing (a workload's observable output against a committed golden) |
| "chip census" | **CPU qualification** | A census counts; this decides. The check issues a per-chip verdict — the chip either is or is not a lawful determinism substrate |
| **"seam"** (in new text) | **boundary**, or the concrete trait/contract by name | An unfalsifiable word: every interface is a "seam", so the term carries no information and hides *which* line is meant. Write "boundary" for an architectural line, or name the thing (`the Backend trait`, `the interrupt delivery contract`, `the ISA boundary`). **Banned in new text only** — existing occurrences are debt cleared by their own scheduled pass |
| `harvest_dirty_gfns` | **`drain_dirty_pages`** | "Harvest" is inherited KVM jargon; "drain" is the crate's own existing word for retrieve-and-reset, and the method's semantics are exactly a drain |

## Adopted vocabulary — testing

| Word | Names | Notes |
|---|---|---|
| **identity test** | run the same address twice; require the full state hashes to be bit-identical | An identity test compares `state_hash` (all state, latent included) — not `observable_digest`, which would be blind to a latent divergence |
| **contract tests** | the one shared exam every implementor of a trait must pass, written once and run against each | Plural by nature: it is a *suite* keyed to a trait, not a single test. The exam is generic over the trait and driven through a fixture the implementor supplies |
| **CPU qualification** | the per-chip check that qualifies new hardware as a determinism substrate | Classifies every advertised instruction/feature as *deterministically pure* / *must-trap* / *forbidden*, plus per-chip exactness and save/restore fixpoint |
| **plane** | a group of control-protocol verbs sharing **one** obligation | Five today: session, state algebra, observation, intervention, provenance (`docs/PROTOCOL.md`). A new verb inherits its plane's obligation; a verb that fits no plane's obligation is a design error |

## The backend contract categories — exactly three

A `Backend` obligation is **ordering**, **exactness**, or **fixpoint**. There is no fourth
category, and in particular:

- **"capability honesty" is not a category.** A capability flag creates no obligation of its
  own — it **selects which exactness exams apply**. A backend advertising a deterministic
  clock is bound by the clock exams; one that does not advertise it is bound to decline
  loudly rather than behave as if it had the capability. Both are exactness.

| Category | Names |
|---|---|
| **ordering** | operations happen in the contract's order; an out-of-order one fails closed rather than silently mis-servicing the guest |
| **exactness** | quantities the engine treats as exact really are exact — deadlines, dirty-page sets, repeated runs |
| **fixpoint** | round trips are round trips — `save → restore → save` is the identity, and a malformed blob is an error, never a panic |

## Pins — testing

- **The three digests keep their three roles** (restating the consonance addendum's pin,
  because the acceptance oracles are where it matters): `state_hash` = all state incl.
  latent; `observable_digest` = guest-emitted output only, and **the only sound basis for
  O3**; `Hash { scope }` = the wire verb. Three names, three roles; never unify.
- **O1 / O2 / O3 keep their numbers and their meanings**: O1 **identity**, O2
  **conformance-to-spec** (observable digest against a committed golden), O3
  **seed-sensitivity**. The word "conformance" is reserved to O2 and is not available for
  trait contract tests.
- **`sweep` stays fenced as gate-only vocabulary** (see the top-level Keeps). A
  CPU-qualification sweep is a gate; it is not a campaign and never appears in
  product-facing language.

## Sequencing — testing

1. **Binding on new code and new docs immediately.**
2. **New text only** for the "seam" ban: a repo-wide purge of existing occurrences is its
   own pass, deliberately not folded into unrelated work.
