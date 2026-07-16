# Exploration — the dissonance search & scoring architecture

> **Status: PARTIALLY SUPERSEDED (production path); design record otherwise.** Reconciled 2026-07-16
> against `docs/DISSONANCE-STRATEGY.md` (the ruled strategy, PR #103) and `docs/GLOSSARY.md` (the
> naming authority). What survives: the literature, the live-plane/replay-plane split, the
> two-hard-problems discipline, parent-rooted materialization + lazy retention, the
> `Tactic`/`Selector` decomposition of the old `Strategy` god-object, triage-by-determinism, and the
> phased plan as a record of what was built. What no longer governs the **target**: the
> `Sensor → CellFnV1 → Archive::admit` seam, the `link`-tier `(Moment, GuestEvent)` stream, and the
> SDK "catalog" are compatibility framing. The strategy rules the production shapes instead —
> normalized `SdkSchema` + ordered `SdkEvent` decoded by `dissonance/sdk-events`; temporal
> reduction, historical derivation, and cells materialized on the Differential observation plane
> (`differential-dataflow`); archive occupancy as a deterministic Differential reduction rather than
> a mutable `Archive::admit`; and a simple selector before any advanced selector work. That
> migration is the `hm-bbx` epic and is **not implemented yet**; the current `Sensor`/`FeatureSet`/
> `Archive::admit` code is compatibility, not a partial build of the target. See
> `docs/DISSONANCE-STRATEGY.md` for the boundaries (evidence cuts, lineage-complete prefixes,
> retention/finalization, deterministic `Revision` assignment) and `docs/GLOSSARY.md` for the names
> (`GuestEvent` → `SdkEvent`; the `link` crate → `sdk-events`; the SDK catalog → `SdkSchema`).
>
> **Loop names.** `docs/GLOSSARY.md` (2026-07-06) retired the loop names this doc uses:
> **Modulation → rollout**, **Progression → the search loop / `step`**. They are kept below as the
> historical design vocabulary; read *Progression* as the generic search loop and *Modulation* as
> one rollout.

This is the design ruling for **how dissonance searches**. `docs/DISSONANCE.md` rules the
*permutation surface* (two control planes, one `Moment`-keyed `Environment`, the Modulation/Progression
loops, the Progression's three agnostic seams). This doc rules what lives **behind those seams**: how a
run becomes a **signal**, how signals become a **cell**, how cells become a **frontier**, and how
the frontier is **searched** — the machinery `DISSONANCE.md` deliberately leaves as "an opaque
coverage vector + oracle events."

> **Scope.** Nothing here changes the Progression. Every component below sits behind one of the three
> seams `DISSONANCE.md` already names (Navigation / Scoring / Proposal). The load-bearing invariant
> is unchanged and is the acceptance test for this whole wave: *adding a fault type, a signal
> channel, a tactic, or a `harmony-<env>` layer grows the seams and touches **Progression** never.* If a
> task here forces a change to Progression select/score/GC policy, the abstraction has leaked and the task
> is wrong.

> **Naming.** The loop names below — **Progression** (outer) and **Modulation** (inner) — are this
> doc's historical vocabulary, retired again by `docs/GLOSSARY.md` to **the search loop / `step`**
> and **rollout** respectively (see the status banner above). `docs/DISSONANCE.md` predates even the
> Progression/Modulation rename and still says *Theme*/*Variation* — read those as
> Progression/Modulation.

## Where this sits

This is the wave **after** the loop closes. It assumes the frontier glue is landing or landed:

| Prereq task | Supplies | This doc's dependence |
|---|---|---|
| **58** — socket server + socket-backed `Machine` (the R2 adapter — **seed-driven**; reactive-suspension `run(resolve)` is a task-58 non-goal and arrives with the first guest-plane service, task 61) | the live `Machine`: `branch`/`replay`/`run`/`snapshot`/`hash`/`coverage` over `control-proto` | every phase — the Progression finally drives a real guest |
| **59** — host-plane `perturb` enforcement (`CorruptMemory`, `InjectInterrupt` @ `Moment`) | the first zero-cooperation fault vocabulary; **`InjectInterrupt` is the PCT lever** | Phase G (schedule entropy) |
| **61** — the net vertical (`net_decide` service + `flow` on the CNI) | per-flow network faults enforced in-guest | Phase G (fault content), G-partitions |
| **41** — non-quiescent snapshot | seal at any V-time, mid-workload | Phase A (validate), Phase C (materialization) |
| **12** — the explorer (Modulation/Progression, corpus, scoring, strategy) | the loop this wave **refactors and enriches** | Phases C, F |

The intelligence layer is **Wave 5**. Its spine is one idea from `DISSONANCE.md` taken seriously:
the Progression is *agnostic by interface*. Wave 5 builds out the interfaces — richly — while keeping the
Progression as dumb as it is today.

## The organizing split: live plane vs. replay plane

Partition every component by **what it touches**, because that partition is what makes the wave
composable and cheap to iterate:

- **Live plane** — touches the guest, runs at branch speed: the **`Machine`** (task 58) and the
  **Tactic** (the Modulation's decision-answering policy). Only these cost VM time.
- **Replay plane** — pure or folded functions of a **serialized run**, never touches the guest:
  **Sensor**, **CellFn**, **Oracle** (pure per run) and **Archive**, **Selector**, feature
  **codebooks** (stateful folds over the run sequence).

Two invariants fall out, and both are load-bearing:

1. **The inner loop is open-loop.** A Tactic draws from a *stateful distribution* using only its own
   state and the recorded `Environment` — it **never reads Sensor output mid-run**. All
   feedback-driven adaptation happens *between* runs, in the Progression. Intra-run steering ("inject the
   partition the instant the leader is elected") is recovered not by live feedback but by
   **checkpointing**: seal at the leader-elected state, then fuzz the partition timing from that
   snapshot in the next branch. This is exactly `DISSONANCE.md`'s "the loops interlock at a
   snapshot," and it is what lets Sensors be run-end and still sufficient.
2. **Replay-plane iteration is cheap, with a precise limit.** Because a run is a pure function of its
   `Environment`, you can change a Sensor/CellFn/Oracle and **re-derive over recorded runs with no
   VM**. But the three roles differ in what that buys:
   - **Oracle** re-run over recorded runs **finds real bugs** — a recorded run that violates a new
     oracle is a genuine finding. (Strong.)
   - **CellFn / Sensor** re-run only **measures signal discrimination over the runs you already
     have** — it is *not* campaign-predictive, because a different cell function would have branched
     differently and produced runs you never recorded. (Diagnostic, not predictive.)
   - **Tactic / EnvCodec** cannot be evaluated offline at all — different inputs, must re-run.

   The store need not be a data lake: always persist the tiny `Environment` (seed +
   `Moment→Action`); a run's full serialized form is *regenerable* by replay on demand. "VM-free
   iteration" holds over whatever subset you chose to serialize; beyond it, regeneration is cheap
   parallel replay.

## The Scoring seam, elaborated: Sensor → Cell → Archive

`DISSONANCE.md`'s Scoring seam is today "an opaque coverage vector + oracle events; the Progression
maximizes novelty over bits whose *meaning* is guest-defined." Wave 5 refines the vector into a
**cell key**, produced by a configured, campaign-defined pipeline — while keeping the Progression blind to
what a cell *means*, exactly as it is blind to what a coverage bit means today.

```
   run (serialized)                  campaign-defined, behind the Scoring seam        Progression-internal (generic)
   ────────────────                  ─────────────────────────────────────────       ────────────────────────
   RunTrace  ──►  Sensor(pure)  ──►  [ (V-time, RawFeature) ]  ──►  codebook(fold)*  ──►  [FeatureId]
                                                                                             │
                                                                     CellFn(pure) ──► CellKey ──► Archive(fold) ──► novelty
                                                                                                                     │
                                                                                                      Selector ◄─────┘
   * codebook only for OPEN-vocabulary signals (log templates, LSH); fixed-vocab sensors emit stable IDs directly.
```

The boundary that preserves Progression-blindness: **CellFn is the last campaign-defined stage; Archive
and Selector are generic and never learn what a cell means.** A cell is opaque to the Progression in the
same sense a coverage bit is opaque today. This is why the whole signal architecture is additive: it
lives entirely behind Scoring.

### RunTrace — the serializable, decoded bundle

The run stops being opaque and becomes a versioned, serializable bundle so the replay plane can work
offline. (The sketches below are illustrative; **the task-64 spine is authoritative for exact
field/method names and time units** — it keys these on `Moment`/`moment()`. The unit
question, escalated per task 65, is RULED in `docs/GLOSSARY.md`: one axis, `Moment` for
a point on it, `Span` for a duration.)

```rust
struct RunTrace {
    terminal: StopReason,             // Crash / Quiescent / Deadline / Decision / Assertion / SnapshotPoint
    env:      Environment,            // the genesis-complete reproducer (DISSONANCE.md)
    coverage: Option<CoverageView>,   // instrument tier — the negotiated shmem geometry, snapshotted at run end
    events:   Vec<(Moment, GuestEvent)>, // link tier — decoded SDK assertions / registers / buggify results
    records:  Vec<(Moment, Record)>,  // scrape tier — decoded log lines, OTel spans, k8s events
}
```

Features are a **timestamped stream**, not a terminal set. One run passes through many interesting
states (every `assert_sometimes` hit, every new cell entered mid-run), so the Archive admits a
**virtual exemplar at each novel `(cell, V-time)` the run passed through** — Go-Explore's
cells-along-a-trajectory. CellFn therefore keys a *point-in-time* feature slice. Coverage is the
exception: it is an accumulated bitmap available only at run end, so it is a **terminal** signal —
it feeds terminal admission; do not blend it into along-timeline cell keys.

### The three signal tiers (all produce Features)

Acquisition is layered cheapest-first; all three converge into the same Feature stream, which is why
the seams below them never move:

- **scrape** (zero recompile, fully offline-tunable) — log-template clustering, OTel spans, k8s
  events. Works on off-the-shelf software. The primary channel.
- **link** (the guest SDK, Tier-2 of `DISSONANCE.md`) — `assert_always`/`sometimes`/`reachable`,
  state registers, buggify. For code you own. Tunable in *interpretation* only; what's emitted is
  fixed at guest build.
- **instrument** (SGFuzz-style state-variable / basic-block coverage) — last, maybe never; changing
  granularity requires re-execution.

### The matcher DSL — authoring signals without Rust

Most signals should be declarative. A generic `MatchSensor`/`MatchOracle` operates over any record
implementing `Matchable`; each channel plugin adapts its record type:

```rust
trait Matchable { fn kind(&self) -> &str; fn attr(&self, k: &str) -> Option<Value>; fn moment(&self) -> Moment; }
```

```yaml
signals:
  - match: { span: "raft.leader_election", attr: { outcome: won } }
    role: sometimes        # objective + checkpoint candidate → Feature + catalog entry
  - match: { span: "wal.replay", attr.max: lsn }
    role: state_max        # IJON register, no recompile → Feature
  - match: { log: "database system is ready*" }
    role: cell             # descriptor channel → CellFn input
  - match: { span: "txn.commit", attr: { error: true }, during: no_faults }
    role: never            # declarative always-assertion → Oracle
```

`role` routes a matched event to the right consumer; the **config's declared signal set is the
catalog**, so a declared `sometimes` that never matched is your never-fired detection — unified
across link (SDK-registered) and scrape (config-declared). Writing a Rust `Sensor` is the escape
hatch for logic the DSL can't express. Open-vocabulary state (log templates) is clustered by a
codebook **internal to that plugin**; it never leaks into core.

## The Navigation seam: virtual exemplars and lazy materialization

The Archive stores **virtual** exemplars — `(parent SnapId, seed', suffix of Moment→Action after the
parent's V-time)`, kilobytes each. **Exemplars are parent-rooted, not genesis-rooted:** a
genesis-rooted exemplar would make materialization a replay-from-genesis, reintroducing exactly the
cost snapshots exist to avoid. Materialize = branch from the (already-sealed) parent, replay only the
suffix, seal. This composes with `EnvCodec::compose` (task 93): the genesis-complete `Bug.env` an
external reproducer needs is the concatenation of suffixes down the ancestor chain, and the
tail-completeness / `at`-provenance contract in `DISSONANCE.md` §"keep compose" is precisely what
makes that concatenation collision-free.

Two consequences:

- **Retention bounds materialization cost.** Keep a spanning set of ancestor snapshots so every live
  virtual exemplar is cheaply reachable; cost = replay depth from the nearest *retained* ancestor.
  This is the Agamotto checkpoint-pool economics — retain by expected re-execution time saved.
- **Eviction is always safe.** Determinism re-materializes any evicted state from genesis, identical.
  So retention is a **pure performance knob, never a correctness concern** — the Archive's GC never
  has to reason about reachability, only cost.

Materialization is an **engine mechanism** between `Selector.pick` and `Machine.branch`, not a
trait. Sealing depends on task 41 (non-quiescent snapshot) holding at arbitrary V-time under
adversarial timing — see Phase A.

## The Proposal seam: Tactic + EnvCodec

`DISSONANCE.md` already splits proposal into `EnvCodec::seeded/mutate/compose` (vocabulary-aware) and
the Modulation's answering. Wave 5 names the answering policy the **Tactic** and decomposes the
explorer's current `Strategy` god-object into **Tactic** (inner, open-loop, live) + **Selector**
(outer, replay-plane) — the two were conflated in `strategy.rs`.

- **EnvCodec** pre-populates the `Moment→Action` entries being fuzzed (the mutation axis, outer).
- **Tactic** answers the residual decisions online from a stateful distribution (the entropy axis,
  inner). The recorded union is the reproducer.

Tactics are a **portfolio** (Coyote's lesson: no single strategy dominates): `quiet` (nominal — the
determinism canary + baseline histories), `fault-regime` (Markov on/off bursts, not IID coins),
`pct(d)`, `value-fuzz`, `swizzle`. Portfolio membership later becomes **bandit arms** (Phase F/G3).

**PCT via determinism.** Probabilistic Concurrency Testing assigns priorities and `d−1` change
points among `k` scheduling steps; a depth-`d` bug is found with probability ≥ `1/(n·k^(d−1))`. On a
nondeterministic system PCT must place change points online (reservoir approximation); here you do it
**exactly in two passes** — pass 1 counts the `k` scheduling `Moment`s, pass 2 places exact change
points and replays them as `InjectInterrupt @ Moment` (task 59). This is a capability Antithesis
structurally lacks (it is single-core-pinned; see `docs/REVIEW-2026-07.md` gap 5) and it is
consistent with the single-online-vCPU v1 contract (task 62): PCT perturbs the *guest scheduler's*
interleaving on the one online vCPU, not true SMP.

## Oracles: trace vs. probe

Not every oracle is pure over a RunTrace:

- **Trace oracles** (replay-plane, pure): `Crash`, `assert_always` violation, and **Elle** over an
  already-recorded operation history. The strong offline-bug-finding property applies to these.
- **Probe oracles** (live-plane): liveness / `eventually` ("does the cluster converge once faults
  stop?") require *running forward* from a state — a directed probe on a **throwaway terminal
  branch**, discarded so it never contaminates the timeline. This is really a specialized
  Tactic+`Machine` interaction, not a `judge(&RunTrace)` call.

**Elle lives at the evaluator layer, not in `harmony-linux`.** The guest/SDK provides only transport
and determinism (an operation history over the `Event` service, or derived from OTel spans);
isolation/linearizability checking is an `Oracle` plugin. Prefer trace oracles by arranging the
workload to emit what the checker needs (e.g. Elle final-reads) so the oracle stays pure.

## Triage: determinism turns statistics into algorithms

Every failing run is `(parent chain, Environment)`. Triage is therefore algorithmic, not
probabilistic:

- **Minimize** — ddmin over the `Moment`-keyed schedule (delete-one, delete-range, time-shrink);
  every probe is one deterministic, *conclusive* replay.
- **Localize** — trunk bisection with **inevitability probing**: binary-search the parent chain for
  the earliest snapshot from which the failure still occurs across N random continuations. Output:
  "bug inevitable between snapshot 412 and 413." (This is Antithesis's causality analysis, free from
  the primitives.)
- **Explain** — LDFI counterfactuals: replay the minimized schedule without each fault; the
  individually-necessary set *is* the bug explanation.
- **Dedup** on **stable coordinates** — `(necessary-fault set, earliest-divergence V-time bracket,
  terminal signature)`. **Never** on learned cells (they drift as codebooks evolve) or coverage/stack
  hashes (Klees et al.: they actively miscount).

## The two hard problems (and the discipline they impose)

The literature is unanimous on where systems like this fail, and it is not the search algorithm:

1. **The cell abstraction** is the whole game — too fine and the archive explodes on a single run's
   trajectory; too coarse and progress is invisible. It is isolated to *one* pure trait, `CellFn`,
   deliberately, so it can be iterated hardest without disturbing search or injection. Best-per-cell
   domination is **mandatory from day one**, or a long k3s run OOMs the archive.
2. **The feedback signal must correlate with bugs**, or a better search optimizes the wrong thing
   faster. This is why **Phase E gates Phase F**: validate signal→bug correlation on a seeded-bug
   benchmark *before* investing in bandit/MCTS search.

## The roadmap (Wave 5)

Sequenced by risk and dependency. `[box]` gates run on the determinism box (hand to the foreman);
`[Mac]` gates are pure-logic (closeable locally). Two **GO/NO-GO** gates guard the wave. Concrete
task numbers were assigned at handoff (2026-07-01): **tasks 63–76**, mapped per row below.

| Phase | Delivers | Key gate | Prereqs | Lead papers |
|---|---|---|---|---|
| **A** de-risk — **task 63** | validate task-41 seal at arbitrary mid-workload V-time under injected timing jitter; define `sealable(V-time)` if partial | **GO/NO-GO**: seal succeeds at ≥target% of arbitrary V-times, branch-from-mid-seal deterministic-twice `[box]` | 41, 58 | Agamotto (Sec'20) |
| **B** trace — **task 65** | `RunTrace` as a serializable decoded bundle; the run loop populates it | reload + re-derive is byte-stable `[Mac/box]` | 58, 64 (spine) | Nyx / Nyx-Net |
| **C** spine — **tasks 64 + 68** (spine + materialization) | **in `explorer`** (rule 2 — interfaces live in the consumer): add the search-plane trait spine (`Sensor`/`CellFn`/`Archive`/`Selector`/`Tactic`/`Oracle`) + `RunTrace`/`Feature`/`Cell` vocab; decompose `Strategy`→`Tactic`+`Selector`; `Corpus`→cell `Archive` (timeline admission, best-per-cell, parent-rooted virtual exemplars) | behavior-equivalent on the toy machine; eviction never changes reproducibility `[Mac]`; materialize replays only the suffix `[box]` | 12, A | Go-Explore, MAP-Elites, Agamotto, Legion, Nyx-Net |
| **D** signals — **tasks 66 + 67** (matcher DSL + logtmpl/CellFn v1) | Sensor pipeline; log-template scrape sensor; the matcher DSL + `Matchable`; CellFn v1 (multi-channel) | distinct templates form a stable species set; DSL routes roles; never-fired declared signals detected `[Mac]` | C | Mallory, SGFuzz, ModelFuzz, IJON |
| **E** validate — **task 69** | seeded-bug toy distributed workload; signal→bug correlation harness; baseline time-to-bug | **GO/NO-GO**: bug reproduces 25/25; correlation report greenlights F `[box]` | 60, B, D, 68 | STADS, Klees et al. |
| **F** search — **task 70** | Selector v2 (Go-Explore count-based); v3 (non-stationary bandit + STADS stop) | time-to-seeded-bug beats baseline `[box]` | E | EcoFuzz, Legion, AFLFast, Entropic, AFLGo |
| **G** entropy — **tasks 71 + 72** (regimes + exact PCT) | regime-based faults; exact two-pass PCT (`InjectInterrupt`); tactic portfolio as bandit arms | finds a partition-duration bug the IID version misses; finds a depth-2 concurrency bug `[box]` | E, 59, 61 | PCT, PCTCP, Coyote, RFF, Krace, FDB, AFLNet |
| **H** SDK — **task 73** | `harmony-linux` guest SDK (assert_*, catalog-at-init, random, lifecycle); buggify as a `DecisionClass` on the fault stream; state registers | always-violation → Bug; never-fired sometimes flagged; deterministic `[box]` | C, D | IJON, FDB buggify, AFLGo |
| **I** otel — **task 74** | in-guest OTLP bridge over the `Event` service (AlwaysOn); `dissonance/otel` decoder + `Matchable for Span` + HB-summary sensor | same-seed runs produce byte-identical span forests; HB summaries distinguish interleavings `[box]` | D, H | Mallory, Elle |
| **J** oracle/triage — **tasks 75 + 76** (oracles + triage) | trace oracles + genesis-complete `Bug` + fingerprint; probe oracles on throwaway branches; Elle plugin; triage suite (ddmin / bisection / LDFI / stable-coord dedup) | crash reproduces 25/25; liveness caught on a discarded branch; bug minimized + localized + explained + deduped `[box]` | E, plus J3 needs op-histories | Elle, LDFI/Molly, ddmin, Igor, Klees, rr |

**Critical path:** A → B → C → D → E → F/G (63 → 64 → 65 → 68 → 66/67 → 69 → 70/71–72; task 64's
spine lands **before** 65, which serializes its vocabulary — the A→B→C phase lettering predates
that inversion). H, I, J hang
off a validated loop and are prioritized by which bugs matter most. **Parallelizable off-path:** the
seeded-bug workload (task 69's benchmark half) from Phase B onward; task 71 (pure-logic regime
faults) any time after 64; the matcher DSL scaffolding (task 66 — crate `matcher`; `match` is a Rust
keyword) once C lands.

**Don't build past a GO/NO-GO without passing it.** If **A** fails, the archive model changes
(cells restricted to sealable boundaries). If **E** fails, fix the *cell function* (D), not the
search (F).

## The five papers that keep this from being an Antithesis clone

Antithesis supplies the *architecture* (deterministic hypervisor, snapshot-as-prefix, sometimes
assertions) but keeps its two hardest parts secret (branch scheduling, the coverage "middle-ground"
scoring) and structurally cannot do a third (true concurrency). These go where it doesn't:

1. **Go-Explore** (Ecoffet et al., Nature 2021) — the outer loop done general, not a hand-tuned grid.
2. **LDFI / Molly** (Alvaro et al., SIGMOD 2015) — fault injection by *backward* reasoning; uniquely
   enabled by deterministic lineage.
3. **Mallory** (Meng et al., CCS 2023) — the only greybox fuzzer of real distributed systems;
   happens-before novelty is the D/I signal design.
4. **ModelFuzz** (Nagendra et al., OOPSLA 2025) — a small formal model supplies the cell abstraction.
5. **PCT** (Burckhardt et al., ASPLOS 2010) — the concurrency capability Antithesis can't offer, made
   *exact* by determinism.
