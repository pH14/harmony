# Dissonance from scratch — a fuzzing-vocabulary design sketch

**Status: exploration note, not a ruling.** This document records a design
conversation (Paul + agent, 2026-08-08) that rebuilt dissonance from first
principles around two questions: what if the search vocabulary were
industry-standard fuzzing terms instead of harmony's own, and where exactly do
cheap/fast LLMs fit in the loop? It deliberately uses names that differ from
`docs/GLOSSARY.md` rulings (e.g. *run* where the glossary says *rollout*,
*corpus* where the code says `Archive`). Nothing here retires a glossary
ruling; if any of this is adopted, it goes through the normal glossary process.

## Premise

Antithesis validates its searcher by having it play video games. LLMs are
excellent at the *strategy* layer of games (goals, quest logic) and terrible at
the *twitch* layer (per-frame control); a mechanical searcher is the inverse.
LLM calls are ~10^6 slower than a branching decision, so LLMs must never sit on
the critical path of a run. Two facts make the marriage work anyway:

1. **Determinism makes LLM latency invisible to the SUT.** The VM is frozen
   between decisions; V-time doesn't advance while a model thinks. Slowness
   costs wall-clock only, and wall-clock parallelizes with cheap models.
2. **Amortization.** LLM tokens are only well spent on outputs that steer many
   runs: labels, priorities, instrumentation — never single actions.

The governing pattern: **LLMs judge until they can write down the rule, then
the rule replaces them.** Cheap models make per-entry judgments (extensional);
a bigger model occasionally distills those judgments into installed code
(intensional), after which the mechanical loop applies the rule for free.

## Architecture

A **coverage-guided, snapshot-based fuzzer** with an **LLM
triage-and-instrumentation loop** alongside. Five components:

1. **Executor** — the deterministic VM. `run(snapshot, input) →
   (observations, snapshots)`. Thousands of runs/sec. No LLM, ever.
2. **Corpus** — retained snapshots worth continuing from. A run's endpoint
   joins the corpus iff it's novel: it lights up a part of the coverage/feature
   map nothing else has. Entries carry lineage (the fork tree).
3. **Scheduler** — seed selection + power schedule (energy), AFL-style:
   energy flows to entries whose recent extensions claimed fresh map cells,
   decays where nothing new comes back.
4. **Triage** — a pool of cheap LLMs streaming behind the search, labeling
   each new corpus entry. Never blocks the fast loop.
5. **Instrumentor** — a bigger LLM, invoked occasionally with a digest of
   triage labels and corpus stats. Two outputs, named for what they touch:
   it **writes instrumentation** (finer-grained novelty detectors, *scoped* to
   descendants of a corpus entry) and it **sets energy** (priority caps on a
   subtree). Artifacts install between runs and auto-retire when unproductive.

One sentence of glue: the fast loop looks where the map is detailed, and the
LLMs' only power is to change where the map is detailed.

```
        FAST LOOP (machine speed) ──────────────────────────────┐
        │                                                       │
        │   Scheduler ──picks──► Executor ──coverage──► novel?  │
        │       ▲            (deterministic VM)           │yes  │
        │       │                                         ▼     │
        │       └──────────priorities──────────────── Corpus    │
        │                                          (snapshots)  │
        └───────────────────────▲───────────────────────┬───────┘
                                │                       │ new entries
              scoped instrumentation                    ▼
              + energy caps     │                 Triage — cheap LLMs,
                                │                 label entries, stream
                                │                       │ digest
                                └── Instrumentor ◄──────┘
                                    (LLM, occasional)

        SLOW LOOP (LLM speed, async — fast loop never waits)
```

### Design rules

- **Scope by lineage, record the artifact.** An instrumentation artifact
  applies to runs launched from descendants of a named corpus entry, is
  installed at a run boundary, and is recorded — so campaigns replay without
  any LLM present (see Testing). Scoping bounds the blast radius of a wrong
  artifact to one subtree.
- **Refine, never replace.** Scoped instrumentation only *subdivides* the
  global map (quadtree-style); regions never get incomparable feature spaces,
  so global dedup survives.
- **Mechanical expiry.** An artifact that yields no fresh cells for N steps is
  retired with no LLM in the decision. Bad codifications die of natural causes.
- **Machine-consumed label fields are enum-shaped.** Cheap models can't break
  the scheduler with free text.

## Vocabulary

| this sketch          | replaces (current harmony)        | provenance            |
|----------------------|-----------------------------------|-----------------------|
| run                  | rollout                           | fuzzing (execs/sec)   |
| corpus / corpus entry| `Archive` / `Entry`               | AFL                   |
| coverage/feature map | cells (`IdentityCells`)           | AFL / MAP-Elites      |
| novelty check        | admission                         | greybox fuzzing       |
| scheduler / energy   | bandit / selection weights        | AFL / AFLFast         |
| triage               | assess loop (Resolution, partly)  | fuzzing (crash triage)|
| instrumentation      | codify loop (Resolution, partly)  | IJON, laf-intel       |
| snapshot             | MomentRef                         | snapshot fuzzing      |
| campaign             | campaign                          | already standard      |

The old rollout/step/campaign/Resolution ladder collapses to **run →
campaign**; the loops in between need no nouns ("between runs" covers it).
The one term of art we add is the adjective **scoped** (per-subtree
instrumentation — AFL has no analog; its map is global).

## Schemas (sketch)

```
CorpusEntry {
  id, parent_id                  // lineage — the fork tree
  snapshot                       // how to resume (or reproducer: parent + seed)
  reached_by: { seed, run_len }
  novelty:   { map_keys, features }
  evidence:  { log_excerpt, sensor_readings, counters }   // what triage reads
  sched:     { energy, runs_launched, novelty_yield, last_novel_at }
  triage:    TriageLabels?
}

TriageLabels {
  interest:     Boost | Neutral | Suppress    // machine-consumed → energy
  duplicate_of: id?                           // machine-consumed → dedup
  flags:        [BugSuspect, InvariantNearMiss, DeadEnd]
  tags:         ["leader-election", ...]      // digest-bound free text
  summary:      one line
  hypotheses:   [free text]                   // digest-bound
}
```

## Testing without LLMs

LLMs never act; they emit data through two typed seams (`evidence → labels`,
`digest → artifacts`). Consequences:

- The fast loop tests as a pure fuzzer: determinism properties, novelty
  monotonicity, scoping (artifact on entry E affects only E's descendants),
  expiry.
- Seam plumbing tests with scripted fakes (a regex triager, a fixed-artifact
  instrumentor).
- **Replay is the master abstraction:** a campaign is a pure function of
  (config, seeds, recorded artifact stream). Record every LLM output as it
  arrives and the whole campaign replays with no LLM present.
- Model quality is evals, not unit tests: benchmark SUTs with known deep
  states, time-to-discovery vs. a null triager.

## Validation before fault injection

The executor contract is input-vocabulary-generic (a harness, in fuzzing
terms). Validate "can it reach novel states?" on targets where fault injection
is irrelevant:

1. Toy state machines / maze programs with known deep states (exact
   time-to-state metrics).
2. An NES/SDL game via key presses (Antithesis's own Super Mario validation;
   tests the LLM-as-game-player thesis directly).
3. An SDK-instrumented program where the input is the decision sequence the
   SDK requests.

Fault injection arrives later as input vocabulary #4, not an architectural
event.

## Build on LibAFL, not from scratch

- **AFL/AFL++ the binary: no.** Welded to byte-buffer inputs, fork-server
  processes, one global bitmap. The components it would save are the easy 10%;
  the hard 90% is the deterministic executor, which is consonance.
- **LibAFL the library: the real candidate.** AFL decomposed into Rust traits
  (Corpus, Scheduler, Feedback, Observer, Mutator, Executor) precisely for
  custom executors and non-byte inputs. kAFL/Nyx proves the
  hypervisor-snapshot + AFL-machinery marriage.
- **Key reframe:** determinism means snapshot ≡ (genesis + input prefix). Let
  the corpus be AFL-shaped — entries are decision sequences, extension is
  mutating a suffix — and make snapshots a *prefix cache inside the executor*,
  a pure performance layer invisible to the fuzzing loop.
- What stays ours regardless: the executor (consonance), scoped
  instrumentation (a custom Feedback), and the entire LLM layer (triage +
  instrumentor exist nowhere).
- Plan: prototype the validation targets on LibAFL (custom Input = key-press
  sequence, custom Executor = emulator wrapper); decide embed-vs-port after
  the trait system has had a chance to chafe. Priced-in caveat: LibAFL is
  generics-heavy with a steep learning curve.

## Follow-up

`docs/LIBAFL-PLAN.md` verifies LibAFL's API surface against this sketch and
lays out the phased build. It supersedes the first three open questions below
(installable code → recompile-and-restart; digest → dropped, the instrumentor
reads fuzzer stats and the labeled corpus directly).

## Open questions

- How installable code ships: scoped detectors as data (feature configs), an
  expression layer, WASM, or restart-per-install. Gates how fast the
  instrumentor loop can turn.
- Digest design: what summary of triage labels + corpus stats fits an
  instrumentor context and still localizes starvation.
- Whether interactive LLM trajectory seeding (play once slowly at the
  macro-action layer, record as input, mutate mechanically) earns its plumbing
  over open-loop script writing. Severable either way.
- Relationship to the existing `dissonance/explorer` Archive/step machinery if
  any of this is adopted: port ideas, or converge vocabularies.
