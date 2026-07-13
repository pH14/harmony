# Dissonance strategy

> **Status: strategic ruling, not a wire or public-API specification.** This document describes the
> intended product and the shortest coherent path toward it. Statements about current behavior are
> checked against the code; proposed behavior is labeled as such. Implementation details remain
> gated by their task specifications and tests. `docs/GLOSSARY.md` remains the naming authority. A
> new public term or type introduced while implementing this direction must be ruled there in the
> same change.

## Naming and placement

### Keep **Resolution**

The name is literal, not decorative. In tonal music, **resolution** is the motion from dissonance
toward consonance or stability; in counterpoint, a prepared dissonance resolves by voice-leading
into a consonant interval. That is precisely the role intended here: search creates useful
dissonance; judgment investigates it and changes the next campaign so that the system moves toward
an explanation, a reproducible finding, or a better instrument. See the Open University's concise
discussion of [consonance, dissonance, and resolution](https://www.open.edu/openlearn/mod/oucontent/view.php?id=102599&section=1.1).

Resolution is therefore **part of dissonance**, not a third peer beside consonance and dissonance.
The name remains accurate only while its scope stays narrow:

- it investigates findings and frontier behavior;
- it revises instruments and campaign configuration between campaigns;
- it does not execute the search loop;
- it does not make decisions inside a rollout;
- it does not become the generic process supervisor for Harmony.

The family-level relationship is:

```text
harmony
├── consonance    deterministic execution mechanism
└── dissonance    adversarial testing policy
    ├── search loop
    └── resolution    judgment between campaigns
```

### Use the ruled mechanism vocabulary

This document uses the vocabulary already fixed by `docs/GLOSSARY.md`:

- `Moment` — a point on the deterministic V-time axis;
- rollout — one branch, run, and terminal result, producing a timeline;
- `step` — one search-loop iteration;
- campaign — a seeded, budgeted sequence of steps against one workload;
- `Reproducer` — the portable artifact that regenerates a timeline;
- `Entry` / `EntryRef` — a retained archive representative;
- the current Explorer spine vocabulary — `Tactic`, `Selector`, `Archive`, `Sensor`, `CellFn`, and
  `Oracle`; the target disposition of those interfaces is ruled below rather than assumed;
- `Portfolio` — the future chooser among tactic or mutation arms; never a synonym for `Selector`;
- `CampaignConfig` and `CampaignReport` — campaign input and result;
- `MomentRef` and session — exact investigation coordinate and control-transport lifetime.

This document deliberately does **not** introduce waypoint, semantic state, behavior descriptor,
micro-epoch, strategy artifact, `SearchProposal`, `CampaignPlan`, or an evidence-bundle type.

- A “waypoint” was ambiguous among a signal, cell, entry, stop condition, oracle, and `MomentRef`.
- “Semantic state” would create another state currency beside captured state and a reproducer.
- `CellFn` already names the Go-Explore state-discretization seam; “behavior descriptor” adds no
  precision.
- A fixed batch revised by Resolution is already a campaign; no smaller named epoch is needed.
- The retired `Strategy` type was a god object. A new object combining base selection, tactics,
  mutation, stopping, judgment, and budget would recreate it.

The words Go-Explore, quality-diversity, parameter fuzzing, causal testing, STADS, bandit, and
program synthesis name research mechanisms or provenance. They do not name Harmony subsystems.

## Executive thesis

The first product should be **SDK-cooperative testing** over a deterministic system under test.
Black-box testing should be the same architecture with a poorer observation and intervention
surface, not a separate product built first.

Harmony should not use an LLM as a low-level fuzzing policy. It should use an LLM, or a human
through one, as Resolution's judgment policy between campaigns. The model contributes where
pretrained semantic knowledge matters:

- understanding source, specifications, logs, and SDK schemas;
- recognizing which observed distinctions may represent meaningful progress;
- proposing parameterized tactics and fault regimes;
- proposing oracles and assertions;
- proposing new SDK signals or state registers;
- forming hypotheses and specifying controlled counterfactual investigations.

The mechanical system contributes where throughput and exactness matter:

- expanding a fixed campaign configuration across many timings and values;
- selecting and restoring retained entries;
- running thousands or millions of deterministic rollouts;
- recording traces and judging them with deterministic oracles;
- reproducing, minimizing, and comparing findings exactly.

The resulting product is best described as a **deterministic, archive-guided experimental testing
system with an outer semantic judgment loop**. It is not, in its first useful form, a reinforcement-
learning system. It has no learned value function, policy-gradient update, neural reward model, or
requirement to robustify a learned policy for deployment.

## One system, several borrowed mechanisms

The research influences coexist because they occupy different existing seams:

| Influence | What Harmony borrows | Where it belongs | What it must not become |
|---|---|---|---|
| **Go-Explore** | remember useful cells; select one; return exactly; explore outward | `CellFn`, `Archive`, `Selector`, entry materialization | a second archive or a claim that the whole system implements both Go-Explore phases |
| **Quality-diversity / MAP-Elites** | preserve diversity while preferring a better occupant within a cell | Differential archive domination using versioned `quality` data and a deterministic configured order | a separate QD service or a learned global scalar objective |
| **Parameter fuzzing** | mutate fault timings, values, schedules, and tactic parameters around a retained base | `EnvCodec`, sequence mutators, parameterized `Tactic`s | a peer search loop with its own corpus |
| **Causal testing** | compare same-prefix executions under explicit interventions | Resolution using `MomentRef::vary`, replay, `RunTrace`, and oracles | ordinary novelty search or an automatic claim of causation from correlation |
| **STADS** | estimate discovery and residual unseen behavior | campaign measurement and stopping evidence | the selector, reward, or proof of completeness |
| **Bandits** | allocate budget among tactic or mutation arms after their yields are measurable | future `Portfolio` | the `Selector`, which only chooses an `Entry` |
| **Program synthesis** | turn a semantic request into bounded predicates and parameterized programs | optional campaign-authoring language above `CampaignConfig` | free-form model output with executable authority |

There is one archive. Go-Explore supplies its navigation pattern; quality-diversity supplies a
possible admission and replacement policy. Parameter fuzzing supplies variations after a base is
chosen. Causal testing reuses the same deterministic executor for explanation rather than broad
discovery. Resolution decides which of these fixed mechanisms the next campaign will use.

Quality-diversity should not be claimed before it exists. The current default archive is
first-admission-wins and has no quality-based replacement. Until versioned `quality` data and a
deterministic domination order are implemented, the honest description is **archive-guided novelty
search with exact return**.

## The sealed campaign is the adaptation boundary

The containment hierarchy is already sufficient:

```text
Moment    point on the deterministic axis
rollout   one branch → run → terminal
step      choose base → mint reproducer → rollout → admit → judge
campaign  one seed, workload, fixed configuration, and budget
```

Every campaign is sealed:

- its configuration is immutable and content-addressed;
- every stochastic choice comes from the campaign's seeded deterministic streams;
- no model call, human input, floating-point embedding, or wall-clock observation changes its
  execution;
- a campaign may produce observation-only host timing and cost measurements, but search never
  reads them while the campaign runs.

Resolution operates only after a campaign has finalized its artifacts. If lower-latency adaptation
is desirable, run shorter campaigns. Do not add model callbacks to workers and do not coin a second
batch unit.

The model invocation itself need not be reproducible. Its entire input and output must be recorded,
and the configuration selected for execution must be immutable. Replaying a campaign or a finding
must never invoke a model.

## The search loop

The target mechanical flow is:

```text
CampaignConfig (including campaign seed) + machine
        │
        ▼
Portfolio chooses an arm, if a portfolio exists
        │
        ▼
Selector chooses genesis or an Entry
        │
        ▼
materialize Entry + EnvCodec/mutator mints a Reproducer
        │
        ▼
rollout under a fixed open-loop Tactic
        │
        ▼
durable normalized evidence + Entry lineage + candidate seals
  ├── immutable completed-run evidence view → Oracle → verdict/outcome evidence
  └── committed evidence batch → Differential Dataflow
        ├── reduced + derived observations at each sealed_at
        ├── CellFn → CellKey assignment
        ├── best Entry per cell by deterministic quality reduction
        └── retained-evidence and campaign-report views
        │
        ▼
campaign reporting and the next-step Selector view
```

The responsibilities remain deliberately separate:

- A `Selector` chooses an entry without learning what its cells mean.
- A `Portfolio`, when built, chooses among tactic or mutation arms; it never chooses an entry.
- A `Tactic` is single-pass and open-loop. It sees only its own state, the surfaced decision, and
  the seeded PRNG. It never reads observation, archive, or oracle feedback during a rollout.
- `EnvCodec` and mutators transform reproducers outside the live decision loop.
- SDK-event and scrape decoders produce immutable evidence from a finished `RunTrace`; they do not
  reduce temporal state.
- Differential derivations are pure functions of persisted evidence and one immutable campaign
  configuration, evaluated only across completed revision frontiers.
- `CellFn` is the last campaign-defined stage before opaque archive policy.
- An `Oracle` judges a completed trace; a live probe runs on a throwaway terminal branch.

Advanced selection must not block proof of this composed substrate. First run the real cooperative
vertical with a simple deterministic selector and fixed tactic. Only add count-based selection,
bandit allocation, or learned proposal policies after the system can measure their incremental
yield against permanent random controls.

## Cooperative observation: preserve evidence before choosing search state

The cooperative product depends on a correct temporal interpretation of SDK output. An SDK state-
register event is often a change to a value that remains current until another update; it is not
merely an instantaneous feature. The search needs the meaningful combination of current values at a
sealable `Moment`, not a set of unrelated event hits.

`SdkSchema` is not an exhaustive declaration of everything that may become interesting. It is
ordinary versioned data persisted with the trace: stable event identities, value shapes, whether an
identity reports occurrences or state, and—when state-bearing—the base update operation needed to
reconstruct values. A declaration whose legacy source cannot supply that operation remains
reportable but is not eligible for state reduction until a versioned source declaration resolves
it. `SdkSchema` does not decide the final cell representation. A retained Entry's cell may contain
any deterministic fact of the execution prefix through its actual `sealed_at`: instantaneous state,
monotone progress, or explicitly derived history.

Base state-bearing observations currently need four reduction semantics:

- **`set`** — replace the register's current value; the latest value at or before the queried
  `Moment` is current;
- **`max`** — retain the greatest value observed for the register;
- **`min`** — retain the least value observed for the register;
- **`accumulate`** — retain the set of distinct values or species observed so far.

A one-shot occurrence, such as an assertion hit, is not automatically persistent state. It says
that something happened at one `Moment`. It may nominate that moment for investigation, contribute
to an oracle verdict, or become an input to a deterministic historical derivation such as `ever`,
`count`, `latest`, or an ordered-pattern predicate. The derived historical fact may describe a later
Entry; the raw occurrence does not silently move to that later `Moment`.

These reducers and derivations are not a closed vocabulary baked into an application SDK.
Responsibility is split deliberately:

- a source-specific ingress contract supplies stable base event identity, value shape, occurrence
  versus state classification, and—for state eligible for reduction—one base update operation in
  normalized `SdkSchema`; unresolved legacy identities remain evidence but not state;
- the framework supplies deterministic, bounded execution for reducers and derived observations;
- the versioned `CampaignConfig` selects the derivations and cell projection in force;
- a person, an LLM, or a future feature-discovery algorithm may propose new derivations from
  retained evidence.

### Two SDK ingress formats, one persisted observation contract

`docs/LAYERS.md` R-L3 remains authoritative about the app-facing SDK: normal applications consume
the Antithesis SDK surface unmodified and send Antithesis JSON through `/dev/harmony`. The existing
binary `guest/sdk` wire remains an internal surface for bare-metal payloads and guest-resident
agents. The strategy must not mistake that internal wire for the product API.

`sdk-events` owns both source-specific decoders and normalizes their output into persisted
`SdkSchema` plus ordered `SdkEvent` records:

- ordinary Antithesis assertion evaluations normalize to occurrence/property evidence. Numeric-
  guidance verbs carry an explicit extremal contract and may be SDK-filtered to new watermarks, so
  they may normalize to the declared monotone maximum/minimum—never to arbitrary current `set`
  state. A versioned workload instrumentation declaration may supply another state contract only
  when its emission path actually reports every required update; it cannot recover values filtered
  out by the SDK;
- the binary-v1 decoder preserves the declared point identity and the operation carried by each
  fired event. Mixed operations or incompatible value shapes for one identity are malformed
  evidence. A declared-but-never-fired state point remains valid schema coverage with unresolved
  base semantics; it is not reducible state and is not a malformed stream;
- the cooperative production path must therefore add a versioned binary declaration (wire v2) or
  an equally explicit workload instrumentation declaration before a binary state point can be used
  by `CellFn`. The first Differential vertical cannot silently bless inference from v1 events;
- `accumulate` requires either a source verb/version that declares it or a campaign derivation over
  occurrences. Binary v1 must not claim a base operation it cannot encode;
- adding a new app-facing verb remains deferred until the normalized contract requires semantics
  that cannot be expressed by the adopted Antithesis surface.

Thus `SdkSchema` is a host-persisted normalized data contract, not necessarily a new application
API object. The original source declaration and raw bytes remain recoverable so a later decoder can
audit or migrate the normalization.

Numeric JSON is not permitted to enter state-affecting code through a host `f64`. `sdk-events`
preserves the original number token. Before numeric guidance may drive reduction or cells, its
schema must select a bounded exact representation and deterministic total order—for example a
canonical sign/coefficient/base-10-scale tuple with explicit digit and exponent limits. Non-finite,
out-of-range, or inexactly representable input remains report-only evidence or fails validation;
it is never approximately compared. The initial cooperative vertical uses bounded integers.

The word **fold** is avoided for this responsibility: `CellFnV1` already uses `fold_k` for numeric
modulo reduction. State reduction, historical derivation, and cell quantization are different
operations and should not share one name.

### Discovery must not be limited to anticipated state

The run evidence and the archive projection are different assets. `RunTrace` preserves decoded SDK
events and raw records; retained reproducers can regenerate that evidence by deterministic replay.
An Entry archive is only one versioned projection of that evidence into restorable search states.
It must not become the sole evidence store.

The campaign therefore has two conceptual loops:

1. Within a campaign configuration, observation derivations and cell semantics are fixed. Search,
   replay, archive admission, and comparison all use that stable meaning.
2. Outside that fixed loop, generic novelty can retain a bounded evidence sample for previously
   unseen event species, attribute shapes, value changes, transitions, short sequences, log
   templates, coverage, or causal deltas. Analysis may propose a new deterministic derivation. A
   new configuration is then issued and cells are recomputed from retained traces or reproducers.

This lets Dissonance discover that an event or transition matters before it knows what the event
means. Promotion into search state remains explicit, replayable, and attributable. A learned
representation may eventually propose derivations, but it must be frozen and identified within a
configuration; an online model must not silently change the meaning of existing cells.

Evidence retention needs an explicit bounded policy separate from archive admission. Otherwise an
unmodeled but interesting discarded run cannot be studied later merely because some retained Entry
can be replayed. Three records must remain distinct:

1. the immutable evidence ledger while its raw records are physically retained;
2. versioned membership in bounded analysis/novelty working sets, where admission and expiration
   are ordinary positive and negative Differential updates;
3. committed Entry cell assignments, archive decisions, and finalized campaign summaries.

Working-set expiration must not retract the cell of a live Entry or make finalized campaign counts
move backward. Entry eviction is its own deterministic archive-policy update. Every retention or
eviction choice that can affect search or Resolution is declared in `CampaignConfig`, uses stable
tie-breaks, and is independent of disk pressure and wall time; resource exhaustion aborts rather
than silently changing policy.

Physical garbage collection is allowed only behind either (a) a durable base-state checkpoint that
is sufficient to rebuild every still-supported view, or (b) a finalized artifact that explicitly
ends future raw-evidence reinterpretation. A summary preserves only the derivations it actually
materialized; completeness metadata does not make it answer a new query. Evidence required to
reproduce a retained Entry—its genesis-complete reproducer and lineage—cannot be collected while
that Entry is live. The existing campaign-trace-retention work is therefore part of the cooperative
product path, not optional observability polish.

### Differential Dataflow is the observation and materialization plane

Dissonance uses the `differential-dataflow` runtime rather than reimplementing differential
collections. Persisted serde records form the evidence ledger, versioned update log, committed
Entry assignments, and finalized artifacts; Differential arrangements are rebuildable indexes and
working views. Runtime traces and arrangements are never portable artifacts.

The generic Explorer integration owns the crash-safe append of each completed, normalized evidence
batch before it can be submitted at a revision. The Revision coordinator atomically associates that
durable batch identity with its proposal and revision; restart replays committed ledger inputs rather
than treating a live arrangement as authority. The existing `TraceStore` may remain payload backing
for immutable reproducers or journals referenced by digest and format version, but it is not the
evidence ledger: its legacy `EnvOnly` policy and whole-`RunTrace` key do not carry campaign,
configuration, rollout, lineage, cut, or revision identity. A referenced payload cannot be removed
except through the ledger-aware retention and garbage-collection rules below.

The boundary between control and data is strict:

- the imperative, seeded search loop selects an Entry, materializes it, mints a reproducer, and
  executes one open-loop rollout;
- immutable `SdkEvent`s and other finished-run evidence enter Differential only after or outside
  that live control path;
- Differential materializes observations, cells, archive occupancy, causal comparisons, and
  reports; it never schedules VM actions or feeds live feedback into a `Tactic`.

A round-trip SDK operation illustrates the split: a `buggify` request and answer are control because
they affect execution; the separately recorded outcome is an `SdkEvent` on the data plane.

Initially, Differential's logical timestamp is a monotonically increasing campaign revision. A
V-time coordinate remains domain data as `(Moment, explicit source ordinal)`. Search branches
routinely return to an earlier `Moment` at a later campaign revision, so `Moment` cannot be the one
global Timely timestamp. A nested V-time scope may be added only if measured prefix-evaluation cost
justifies it and its partial-order/frontier semantics are pinned by tests.

A revision numbers a committed input update, not a rollout. One search step may submit a completed
rollout at one revision and its later materialized seal at another.

Revision assignment is a control-plane decision. Under parallel rollouts, revisions are allocated
from deterministic seeded proposal/dispatch order, never completion order, wall time, or worker
arrival order. Results may complete out of order, but the campaign buffers them and advances the
search-visible frontier in deterministic issued order (or as an explicitly configured fixed cohort
with a canonical commit order). One Timely worker does not by itself make input ordering
deterministic.

Every issued revision slot has a persisted proposal identity before dispatch and must end in a
deterministic terminal record under V-time/work limits. A crashed worker replays the same proposal;
an unrecoverable host/control failure aborts the campaign instead of creating a completion-order
skip. A fixed cohort freezes the selector/archive view at cohort start, mints proposals in canonical
order, and exposes no partial cohort result to another proposal. These rules close both frontier
holes and crash recovery.

The first production relations should cover, in plain terms:

- persisted SDK schemas, SDK events, log records, coverage, interventions, and outcomes;
- Entry lineage and candidate `sealed_at` coordinates;
- provisional observation/cell transitions at configured unsealed evidence cuts, usable only to
  nominate materialization replay;
- current and historically derived observations at each candidate seal;
- `CellFn` assignment under a versioned campaign configuration;
- committed Entry cell assignments and explicit Entry-retention updates;
- deterministic best-Entry selection per cell, with a stable quality tie-break;
- bounded evidence-membership views and finalized campaign metrics.

Ordered evidence is scoped by execution lineage. Its stable key contains the campaign/configuration,
a deterministic rollout identity, source identity, `Moment`, and explicit ordinal. For evidence
captured on one deterministic machine-event stream, the ordinal is rollout-global across sources;
for the initial SDK capture, persisted vector position is the rollout-local source ordinal and must
be contractual. A batched source with only source-local order declares that limitation and cannot
participate in cross-source sequence predicates. Differential retains a multiset, not iteration
order: every ordered query reconstructs sequence by canonical sorting these explicit coordinates.

The current live serial-console adapter is specifically **source-local and stop-granular**: it pages
a raw byte buffer separately from SDK events and stamps decoded lines at the terminal stop. Those
records are full-run evidence for source-local reporting, novelty, and terminal judgment. They are
not eligible for exact same-Moment cuts, cross-source sequences, or log-derived `CellFn` dimensions
evaluated at `sealed_at`. Promotion to seal-relative evidence requires capture-time serial stamps and
a cursor captured with the snapshot; cross-source sequence predicates additionally require one
shared machine-event ordinal. Sorting the two terminal vectors afterward cannot recover that order.

A VM seal still occurs at a `Moment`, but its initial cut also records the SDK stream prefix length.
The cut `(Moment, included SDK-event count)` is half-open: persisted SDK vector positions less than
the count are included, including the exact subset emitted at the seal's `Moment`. Ancestor segments
use the half-open interval from the parent cut to the child cut, so a boundary event is neither
duplicated nor dropped. If another source later becomes seal-relative, the seal carries a distinct
declared cursor for that source; independent cursors do not imply cross-source order.

The branch implementation restores an ancestor SDK prefix into the child machine. Ingestion must
therefore append only the child positions after the parent cut under the child rollout identity; the
restored prefix is inherited through lineage and is never inserted again as child evidence.

The cut is captured with the seal, not reconstructed from a terminal trace. The production
`Machine`/control-protocol snapshot response must bind the snapshot handle, synchronized seal
`Moment`, taint, and included SDK-event count from the same stopped server state. The generic
Explorer carries that cut through its pending-fork and lineage records. Today a client can derive the
same SDK count with a second pure read while the VM remains stopped, but that unbound two-call result
is not the production authority. A failed snapshot yields neither a usable seal nor a cut.

This cut contract is backend-independent. A Differential NO-GO may replace the incremental
materialization backend, but direct recomputation still needs the same authoritative boundary for
actual-seal admission. Cut capture may be retired only by an explicit ruling that abandons
seal-relative admission and defines the replacement boundary semantics.

A child rollout normally contributes only the suffix observed after its branch point. The complete
prefix for a candidate seal is therefore the canonical merge of ancestor evidence segments through
their cuts plus the child's suffix through the candidate cut. The Entry lineage relation is the
authority for that composition. Replaying the genesis-complete reproducer is the semantic oracle;
cached ancestor materializations are acceleration and must produce the same consolidated multiset
and the same canonically sorted projection.

Standard Differential operators carry their standard meanings. `reduce` defines temporal or archive
semantics; `distinct` turns multiplicity into presence; `count` preserves occurrence cardinality;
`consolidate` only canonicalizes equal `(data, revision)` updates and cancels net-zero differences.
Every search-visible relation is read only after its probe frontier has passed the submitted
revision, then consolidated and canonically ordered before it can affect selection or serialized
bytes.

No public “semantic-state” wrapper is required. The target pipeline is:

1. `sdk-events` decodes the applicable SDK ingress format and produces normalized `SdkSchema` plus
   typed, timestamped `SdkEvent`s. The normalized schema, source/instrumentation declaration,
   events, ordering scope, and unknown raw bytes are persisted with the run.
2. The decoder validates that every event conforms to the normalized identity, value shape, and
   base update operation when one is declared. Malformed conflicts fail as typed evidence errors;
   a valid unresolved legacy declaration remains non-reducible evidence.
3. The completed normalized evidence batch and Entry lineage are durably appended, associated with
   one campaign revision, and then enter Differential. Differential evaluates configured evidence
   cuts and materializes provisional observation/cell transitions. Those transitions may nominate
   replay but can never enter archive occupancy.
4. `Explorer` selects a bounded canonical subset of provisional transitions and materializes them.
   The resulting candidate seals enter Differential at later revisions.
5. Differential derives the complete evidence prefix through each candidate seal. Base observations
   preserve their declared `set`, `max`, `min`, or `accumulate` behavior; configured historical
   derivations are evaluated; unpromoted occurrences remain timestamped evidence.
6. `CellFn` is evaluated on the complete projected observations at the actual `sealed_at`, producing
   one committed composite `CellKey`.
7. Archive occupancy is a deterministic Differential reduction by `(configuration, CellKey)` over
   Entry quality and a stable Entry-id tie-break.

Observation and materialization are deliberately two passes. The first rollout may discover an
interesting cell transition at `observed_at`, even when that instant is not safely snapshotable.
After Differential's probe has passed that completed rollout's revision, the imperative campaign
controller—the generic `Explorer`—deduplicates transitions by configured projection, orders them by
their explicit evidence coordinates, applies a configured candidate/materialization cap, and
schedules replay. Materialization replay consumes the campaign budget. It advances to the first
valid `sealed_at >= observed_at` and temporarily holds that physical seal.

The replay result and candidate seal enter Differential at a later revision. After that second
probe barrier, Differential computes the complete projected observations and cell actually true at
`sealed_at`, then the occupancy reduction decides whether to retain the Entry. `Explorer` keeps the
temporarily held seal only for an admitted Entry and drops it otherwise. An `Entry` must occupy its
claimed cell at its real `sealed_at` moment.

That materialized-view-to-controller edge exists only between completed rollouts at a revision
barrier. It is not live feedback into `Tactic`, and Differential still does not execute or schedule
VM actions.

If the interesting state disappears before a valid seal can be taken, it is not returnable through
that route and must not be admitted under the earlier cell. A cooperative workload can make such a
transient state returnable by adding an explicit SDK checkpoint boundary in a later build. The
archive never pretends that an observation is a restorable state.

For a replicated database, a cell might be derived from observations representing leader identity,
term bucket, commit-index bucket, quorum visibility, and durable-log length. Those are not “waypoints.”
They are versioned projections derived from SDK evidence and discretized by `CellFn`. Some may have
been anticipated by the SDK author; others may have been proposed only after generic novelty or
causal analysis made them interesting.

### Separate provenance, observation identity, value, and cell dimension

The present `ChannelId`/`FeatureId` representation is not the target contract for cooperative
state. `ChannelId` is documented as source or plugin provenance, while `CellFnV1` treats each
configured channel as one independently reduced cell dimension. `LinkSensor` then packs many SDK
register identities and values into a single state channel. Those three choices cannot represent
simultaneous current values for independent registers, and numeric `FeatureId` ordering is not a
valid implementation of `set`.

The target model must represent these roles separately:

- **source provenance** — SDK events, logs, coverage, or another producer;
- **observation identity** — the particular register or derived fact being tracked;
- **value** — the observation's value at a `Moment`;
- **cell projection** — whether and how that observation is reduced, quantized, and included in a
  campaign's cell.

Each independent observation is reduced independently before the complete point-in-time state is
passed to `CellFn`. Provenance alone never implies a cell dimension. The implementation may reach
that contract by assigning dimensions per observation or by replacing the packed feature shape,
but it must not hash an entire current-state map into one opaque value merely to preserve the old
API.

The data contract settles the `link` rename: **`dissonance/link` becomes `sdk-events`**. It owns the
Antithesis-JSON and internal binary-wire decoders, normalization into `SdkSchema` and typed
`SdkEvent`, validation, original-declaration and unknown-byte preservation, and serde. It does not
own temporal reduction, derived observations, Differential arrangements, cells, archive policy, or
oracle judgment. `LinkSensor`, `LINK_ASSERT_CHANNEL`, `LINK_STATE_CHANNEL`, and packed
`(register, value) → FeatureId` are compatibility machinery to delete during the Differential
integration, not APIs to rename. The physical crate rename rides that substantive work rather than
landing as a churn-only patch.

The two other current `link` residents do not move blindly with the decoder. Normalized assertion
evidence preserves both **site identity** and **property identity**. For the adopted Antithesis
surface, the assertion message identifies the property and multiple sites may contribute to it;
site identity remains provenance and coverage, not a separate property verdict.

The binary path may surface an assertion as `StopReason::Assertion`, which `TerminalOracle` can
judge; Antithesis JSON assertions are completed-trace evidence and need not stop the rollout.
Occurrence counterexamples—such as `always` evaluating false or an `unreachable` point firing—are
judged by an SDK-assertion Oracle in the Explorer layer. Its input is a borrowed, immutable
completed-run evidence view supplied only after durable append: terminal and reproducer identity,
normalized schemas/events/records, and their coordinates. It does not query mutable ledger state or
Differential directly, and it deduplicates any equivalent terminal verdict. `AlwaysViolation` as a
decoder-crate type is compatibility code, not proof that the Oracle role is redundant.

Absence-based expectations—such as `sometimes` or `reachable` with no satisfying evaluation—are a
separate finalized Differential view over explicit **property** expectations (including the source
protocol's `must_hit` semantics) minus aggregated
property results, scoped by source-schema instance, configuration, and campaign. It is not a
per-site subtraction and it does not run as a per-trace Oracle. State declarations with unresolved
reducers remain reportable as site/schema coverage but are not automatically failed expectations.
Finalized property counts survive working-set retention and raw-evidence GC, so expiration cannot
resurrect a false never-fired finding. `sdk-events` preserves declarations, sites, properties, and
events; reporting owns the derived absence claim.

### Fate of the current spine interfaces

The generic `Explorer` remains the production campaign engine, but adopting Differential changes
which current traits are its durable seams:

- `Tactic` and `Selector` remain control interfaces with their existing single-pass and entry-choice
  boundaries;
- `Oracle` remains a pure completed-run judgment boundary. The current
  `Oracle::judge(&RunTrace)` carrier is compatibility unless `RunTrace` is versioned into the
  immutable ledger-backed completed-run evidence view above; the target does not add a second
  ledger-shaped Oracle or persist a duplicate normalized event authority;
- `CellFn` remains the pure, versioned projection from a complete materialized observation map to
  one `CellKey`; the current `CellFnV1` feature/channel algorithm is not ratified by that decision;
- archive remains the one conceptual corpus and a selector-facing materialized read model, but the
  current imperative `Archive::admit(trace, forks, sensors)` trait is not the target admission
  interface. Differential reductions own occupancy and deterministic quality domination;
- `Sensor`, `Feature`, `FeatureSet`, and `ChannelId` are compatibility interfaces, not production
  currencies. Source decoders persist evidence and Differential owns state reduction and historical
  derivation;
- `LogSensor` and similar extractors may contribute pure, versioned parsing logic to evidence
  ingestion or Differential derivations, but their legacy feature/channel output has no authority.

`docs/SCORING.md` now fences its historical design. Its genesis-rooted recomputation, explicit
retention, and deterministic best-per-cell ideas survive; its `CellFnV1` knob ratification, mutable
`Archive::admit` framing, selector-bandit coupling, and per-subtree STADS assignment do not govern
this target.

This is the most important cooperative-product gap in the current generic Explorer. Its
[trace construction](../dissonance/explorer/src/engine.rs#L452) still leaves the legacy
`RunTrace.events` empty, and its [default archive](../dissonance/explorer/src/defaults.rs#L306) calls `CellFn` on
singleton features at exactly the fork's `Moment`. That is a valid compatibility baseline for
AFL-style edge novelty, but it cannot implement the composite, persistent state semantics
described above.

Black-box testing naturally degrades along the same pipeline:

- scrape-tier records and log templates provide evidence without SDK changes;
- coverage provides terminal novelty;
- externally visible failures provide oracles;
- host-plane faults remain available without guest cooperation.

The black-box mode will generally expose weaker cells and later oracles, but it needs no separate
search architecture.

## `CampaignConfig` is the integration primitive

The repository already has concrete
[`CampaignConfig`](../dissonance/conductor/src/campaign.rs#L132) and
[`CampaignReport`](../dissonance/conductor/src/campaign.rs#L268) types. They currently describe a
narrow planted-bug campaign, not the full target search composition. The target should generalize
those campaign artifacts rather than introduce a new `SearchProposal` or `CampaignPlan`.

A future general `CampaignConfig` needs to resolve, directly or by reference:

- workload and campaign budget;
- the selected evidence sources, source/instrumentation schemas, versioned parsers, and observation
  derivations;
- `CellFn` and archive admission/quality policy;
- `Selector` configuration;
- the `Portfolio` and its tactic/mutation arms, if present;
- rollout stop conditions, deterministic work limits, cohort width/order, and materialization
  replay budget;
- seal/Entry-retention policy;
- bounded evidence working-set policy, distinct from Entry archive admission and finalized facts;
- trace and probe oracles;
- permanent random and nominal control allocation.

The exact serialized configuration and all referenced artifacts must be versioned, canonical, and
hashed. `CampaignReport` should identify the configuration and campaign seed that produced it.

The LLM may emit a candidate configuration directly if the format is fully typed and bounded. In
that case Harmony needs parsing and validation, not a component called “the compiler.” If semantic
predicates and tactic programs require symbolic name resolution and lowering, then compilation is
the correct operation:

```text
campaign source + SDK schemas and signal declarations
        ── compile ──▶ CampaignConfig
        ── failure ──▶ diagnostics
```

The source artifact should remain deliberately unnamed until its language and semantics are fixed.
If compilation is introduced, it must at minimum:

- resolve declared signal, SDK observation, decision, and fault names;
- type-check predicates and tactic parameters;
- enforce capability and cooperation requirements;
- prove every range, loop, allocation, and rollout expansion is bounded;
- reject host-dependent or floating-point state-affecting behavior;
- lower symbolic predicates into deterministic integer operations;
- produce a canonical `CampaignConfig` or explicit diagnostics.

Free text may explain a candidate configuration but never carries executable authority.

## Recomputing cells from retained traces

A campaign may later change its `CellFn` or observation projection after learning that the original
cell abstraction was too coarse or too fine. It can then recompute cells for already-recorded
timelines:

```text
same retained RunTrace observations + a different CellFn
        → different derived CellKeys and archive view
```

This does not change or rerun the executions. It answers the diagnostic question: “Would this new
cell definition have distinguished the states we already observed?” It can rebuild a diagnostic
archive view when ordered evidence, source schemas, parser versions, and required deterministic
derivation state were retained.

Recomputing cells cannot manufacture a restorable `Entry` for a moment that was never sealed. If a newly
interesting recorded moment needs a captured state, Resolution must ask consonance to replay the
`Reproducer` and materialize it through the two-pass seal procedure. Keep the operations distinct:

- **recompute cells** — reinterpret retained observations under a different `CellFn`; no VM
  execution;
- **materialize** — replay a `Reproducer` to create or recover captured state at a valid seal.

Campaign retention must support whichever claim is made. A campaign that discards ordered evidence,
source declarations, parser versions, or required derivation state cannot later have its cells
recomputed without replay and must say so explicitly.

The historical `SCORING.md` design called this operation “re-keying,” while `EnvCodec::compose`
already used “re-key” for shifting `Moment` keys during reproducer composition. `GLOSSARY.md` now
resolves the overload in favor of the plain phrase **recompute cells**.

## Resolution's two interfaces

Resolution has two related but distinct interfaces.

### 1. Between-campaign judgment

This is the primary product loop. Resolution reads existing artifacts rather than requiring a new
omnibus input type:

- the exact `CampaignConfig`, including its campaign seed;
- `CampaignReport`;
- retained `RunTrace`s and archive/frontier summaries;
- signal-declaration and SDK-schema coverage reports;
- findings stamped with `MomentRef`s;
- previous investigation transcripts;
- source, specifications, manuals, and human intent made available for this campaign review.

It may propose changes through existing or deliberately scoped surfaces:

- the next `CampaignConfig`;
- signal declarations, `CellFn` configuration, tactics, mutators, and oracles;
- a request for new SDK assertions, state registers, or trace fields in a future guest build;
- explicit `MomentRef` counterfactuals to investigate before choosing the next campaign.

The boundary remains artifact-shaped. Resolution never reaches into an active Explorer and the
Explorer never calls Resolution.

### 2. Moment-addressed investigation

The existing [`dissonance/resolution`](../dissonance/resolution/src/lib.rs) crate implements this
interface: materialize a `MomentRef`, read memory or registers, hash state, run forward, execute a
tainting improvisation, derive a clean counterfactual with `vary`, and record a stamped transcript.

This is necessary infrastructure, but it is not yet the full between-campaign loop. The current
crate does not consume `CampaignReport`, choose a new `CampaignConfig`, invoke a model, or supervise
campaigns. It is the investigation instrument Resolution will use.

Direct access to the control-transport socket is intentional for investigation; it prevents the
Explorer from becoming a gatekeeper. That plumbing peer relationship does not change Resolution's
logical placement inside dissonance.

## Causal testing is an investigation workflow

Deterministic branching gives Resolution an unusually strong primitive: replay an identical prefix
and vary one recorded intervention. `MomentRef::vary` already represents the simplest form of that
operation.

A causal investigation should state:

- the hypothesis;
- one or more base `MomentRef`s;
- the control and treatment edits;
- the expected distinguishing observations or oracle verdicts;
- the fixed campaign budget, if the investigation spans multiple bases or parameter values.

The mechanical executor then runs the matched branches and records ordinary `RunTrace` and finding
artifacts. Resolution interprets the comparison after the batch completes.

Determinism removes rerun noise; it does not make every intervention well-designed or every causal
claim valid. Hidden variables, an incorrectly chosen intervention point, or changing more than one
effective cause can still confound an explanation. The initial causal product should therefore be
matched, same-prefix, explicit-intervention testing—not general causal-graph discovery.

Causal investigations optimize attribution, not broad novelty. They should be reported separately
from archive-search yield. An untainted useful branch may later be admitted as an `Entry`, but that
is an explicit deposit into the normal archive, not an implicit merger of objectives.

## Evaluation

The first question is not whether an LLM can produce an impressive trajectory. It is whether
between-campaign semantic judgment finds more useful behavior under a fixed execution budget.

Every evaluation should include equal-branch-budget comparisons among:

1. seeded random exploration;
2. the simple archive-guided baseline;
3. the same campaign-authoring surface populated randomly;
4. human-authored campaign configuration;
5. LLM-authored configuration, initially ratified by a human.

Permanent random and nominal controls are part of the product, not temporary baselines. They detect
model bias, campaign regressions, and search collapse.

Primary measures for software workloads are:

- bugs found and branches or time to first bug;
- replay success and reproducer size;
- minimization and causal-isolation quality;
- distinct held-out bugs, not merely rediscovery of one signature;
- branches per second and total model cost;
- candidate-configuration yield: how often a proposed change beats its controls.

Cells, depth, and discovery curves are diagnostic measures. They are not substitutes for finding
bugs.

Games remain useful exploration benchmarks because progress is deep, sparse, and visually
inspectable. They are not the target product. Use two tracks:

- **knowledge-allowed** — source, RAM maps, manuals, and domain knowledge are available, matching
  the cooperative software-testing product;
- **blind generalization** — withhold semantic documentation or use an unfamiliar/procedurally
  modified workload, testing whether the method improves search rather than recalling a solution.

Keep the development game separate from the held-out evaluation game. A positive game result must
then transfer to planted distributed-system bugs using the same campaign and signal mechanisms.

## Current implementation reality

The target architecture must not be confused with what is already integrated.

Working foundations include:

- consonance's deterministic execution, branch, replay, snapshot, and hash mechanisms;
- the current Explorer spine and generic `Explorer` control loop, with the target interface
  dispositions described above;
- exact entry materialization with genesis-complete reproducers;
- versioned `RunTrace` storage and scrape/legacy-link decoding;
- the current SDK declaration, assertion, and state-register wire events;
- deterministic sequence mutators;
- a narrow real `CampaignConfig` / `CampaignReport` path for the planted host-fault campaign;
- the Resolution session, `MomentRef::vary`, taint discipline, REPL, and transcript.

The important gaps are:

1. **One production campaign path.** The generic Explorer and the hand-written planted-bug campaign
   are not yet one composition. The generic abstractions need a real end-to-end instantiation.
2. **SDK events in the generic trace.** The generic Explorer currently constructs traces with an
   empty event stream even though the socket machine can retrieve SDK events.
3. **Composite point-in-time cells.** The default archive keys singleton features at an exact
   moment; cooperative state-register values need persistence and aggregation before one `CellFn`
   call.
4. **Differential observation plane.** No production Differential relations, arrangements,
   frontier barrier, or SDK-event evidence schema described by this strategy exists yet. The
   current `Sensor`/`FeatureSet`/`Archive::admit` path is compatibility code, not a partial
   implementation of that target.
5. **Quality domination.** The default archive is first-wins. Versioned per-Entry `quality` data,
   its deterministic configured order, and the replacement rule are not implemented.
6. **Integrated mutation arms.** Sequence mutators exist but are not wired into the production
   Explorer as a measured portfolio.
7. **Resolution's campaign loop.** The investigation client exists; artifact review, model/human
   judgment, configuration emission, and campaign supervision do not.
8. **Campaign authoring.** The general configuration format—and therefore whether a compiler is
   necessary—remains unspecified.

These gaps should be closed before advanced selector or bandit work becomes load-bearing. A simple
selector over correctly materialized cooperative observations is more informative than a
sophisticated selector over empty or incorrectly keyed SDK events.

## Staged direction

1. **Vocabulary and document convergence.** Make this document, `LAYERS.md`, `SCORING.md`,
   `RESOLUTION.md`, `EXPLORATION.md`, `DISSONANCE.md`, and the task-84/task-86 search specifications
   tell one story. Do not introduce new public types for concepts already owned by the surviving
   control or campaign vocabulary.
2. **SDK boundary and Differential GO/NO-GO.** Persist normalized `SdkSchema` and ordered
   `SdkEvent`s. Spike lineage prefixes, evidence cuts, provisional transitions, and retention views
   on the real DD runtime; measure arrangement sharing; explicitly ratify GO before production
   relations, Revision coordination, or Explorer integration proceed. After GO, capture SDK seal
   cuts atomically across the VM control seam before the generic Explorer integration consumes them.
3. **Cooperative mechanism vertical without an LLM.** On the deterministic development maze,
   derive persistent composite observations and cells, retain and restore Entries, and demonstrate
   archive-guided progress through the generic Explorer using a simple selector and fixed tactic.
   This stage includes building the deterministic Linux guest workload, init/image wiring, and
   wire-v2 X/Y instrumentation; no maze implementation exists to preserve today. It begins only
   after the full-retention evaluation profile is available, so evidence is retained from rollout
   one. An explicit mechanism GO decision precedes either held-out or software-system transfer.
4. **Software-system transfer without an LLM.** Apply the identical SDK/Differential/Explorer path
   to a planted database or distributed-system bug and beat permanent controls on a predeclared
   bug/progress measure. A game-only success does not unlock selector cleverness; an explicit
   software-transfer GO decision is required.
5. **Human-authored general `CampaignConfig`.** Make the complete search composition explicit,
   validated, canonical, and reportable. Establish random and nominal controls.
6. **Artifact-only Resolution advisor.** Give a human-ratified model frozen campaign artifacts and
   let it propose changes using exactly the same configuration surface available to a human.
7. **Capped automated campaigns.** Allow validated model-authored configurations to receive a
   minority of the execution budget while permanent controls remain active. Promote only measured
   improvements.
8. **Causal investigation.** Let Resolution drive matched `MomentRef` counterfactuals and promote
   confirmed distinctions into signals, cells, or SDK instrumentation.
9. **Portfolio and distillation, if earned.** Allocate budget among tactic/mutation arms only after
   their yields are measurable. Move recurring successful patterns into deterministic templates or
   cheaper policies.

At every stage, black-box operation remains the same system with fewer cooperative signals. The
SDK-cooperative path is built first because it makes the hard abstraction—the cell—explicit and
gives Resolution the strongest leverage.

## Explicit non-goals

- No LLM callback inside a rollout or search step.
- No learned policy, value function, or reward model in the first product.
- No second archive for QD, causal testing, or model-authored candidates.
- No model score, embedding, wall clock, or host entropy in state-affecting campaign logic.
- No free-form executable model output.
- No new named unit between step and campaign.
- No claim of quality-diversity until quality domination exists.
- No claim that determinism alone proves causality.
- No game-playing product masquerading as the software-testing objective.

## Selected references

- McSherry et al., [“Differential Dataflow”](https://www.cidrdb.org/cidr2013/Papers/CIDR13_Paper111.pdf),
  CIDR 2013.
- Murray et al., [“Naiad: A Timely Dataflow System”](https://www.microsoft.com/en-us/research/publication/naiad-a-timely-dataflow-system/),
  SOSP 2013.
- Ecoffet et al., [“First return, then explore”](https://www.nature.com/articles/s41586-020-03157-9),
  *Nature*, 2021.
- Mouret and Clune, [“Illuminating search spaces by mapping elites”](https://arxiv.org/abs/1504.04909),
  2015.
- Johnson, Brun, and Meliou, [“Causal Testing: Finding Defects' Root Causes”](https://arxiv.org/abs/1809.06991),
  ICSE 2020.
- Meng et al., [“Large Language Model Guided Protocol Fuzzing”](https://www.ndss-symposium.org/ndss-paper/large-language-model-guided-protocol-fuzzing/),
  NDSS 2024.
- Xia et al., [“Fuzz4All: Universal Fuzzing with Large Language Models”](https://arxiv.org/abs/2308.04748),
  ICSE 2024.
- Wang et al., [“Voyager: An Open-Ended Embodied Agent with Large Language Models”](https://arxiv.org/abs/2305.16291),
  2023.
- Ma et al., [“Eureka: Human-Level Reward Design via Coding Large Language Models”](https://arxiv.org/abs/2310.12931),
  2023.
